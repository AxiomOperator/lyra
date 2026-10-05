//! MCP servers as capability providers: lyra starts each configured server
//! (stdio transport, JSON-RPC 2.0), lists its tools and calls them. Risk comes
//! from the tool's annotations (`readOnlyHint`, `destructiveHint`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{Capability, CapabilityHealth, CapabilityKind, RiskLevel};

/// `[[capabilities.mcp]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpConfig {
    /// Prefix for its capabilities, e.g. `fs` → `fs.read_file`.
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the server (values may name variables: `$TOKEN`).
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
}

struct Io {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

pub struct McpClient {
    pub config: McpConfig,
    io: Mutex<Io>,
    next: AtomicU64,
    tools: Vec<Value>,
    pub server: String,
}

const TIMEOUT: Duration = Duration::from_secs(120);

impl McpClient {
    /// Start the server, initialize the session and list its tools.
    pub fn start(config: McpConfig) -> Result<Self, String> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in &config.env {
            let value = match v.strip_prefix('$') {
                Some(var) => std::env::var(var).map_err(|_| format!("{}: environment variable {var} isn't set", config.name))?,
                None => v.clone(),
            };
            cmd.env(k, value);
        }
        if let Some(dir) = &config.cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: couldn't start {}: {e}", config.name, config.command))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self { config, io: Mutex::new(Io { child, stdin, lines }), next: AtomicU64::new(1), tools: Vec::new(), server: String::new() };
        let init = client.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "lyra", "version": env!("CARGO_PKG_VERSION") } }),
        )?;
        client.server = format!("{} {}", init["serverInfo"]["name"].as_str().unwrap_or("?"), init["serverInfo"]["version"].as_str().unwrap_or(""));
        client.notify("notifications/initialized")?;
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map_or(json!({}), |c| json!({ "cursor": c }));
            let page = client.request("tools/list", params)?;
            client.tools.extend(page["tools"].as_array().cloned().unwrap_or_default());
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(client)
    }

    fn send(&self, io: &mut Io, message: &Value) -> Result<(), String> {
        writeln!(io.stdin, "{message}").and_then(|_| io.stdin.flush()).map_err(|e| format!("{}: {e}", self.config.name))
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.send(&mut io, &json!({ "jsonrpc": "2.0", "method": method }))
    }

    /// One request; waits for its response, answering server requests in between.
    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.send(&mut io, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = io.lines.recv_timeout(TIMEOUT).map_err(|_| format!("{}: no answer to {method}", self.config.name))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("method").is_some() {
                // A request from the server (e.g. roots/list): not supported here.
                if let Some(req_id) = msg.get("id") {
                    let reply = json!({ "jsonrpc": "2.0", "id": req_id, "error": { "code": -32601, "message": "not supported by lyra" } });
                    self.send(&mut io, &reply)?;
                }
                continue;
            }
            if msg["id"] != json!(id) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(format!("{}: {}", self.config.name, e["message"].as_str().unwrap_or("error")));
            }
            return Ok(msg["result"].clone());
        }
    }

    fn id(&self, tool: &str) -> String {
        format!("{}.{tool}", self.config.name)
    }

    pub fn capabilities(&self) -> Vec<Capability> {
        self.tools
            .iter()
            .filter_map(|t| {
                let name = t["name"].as_str()?;
                let a = &t["annotations"];
                let risk = if a["readOnlyHint"].as_bool() == Some(true) {
                    RiskLevel::ReadOnly
                } else if a["destructiveHint"].as_bool() == Some(true) {
                    RiskLevel::Destructive
                } else {
                    RiskLevel::Write
                };
                let description = t["description"].as_str().or(a["title"].as_str()).unwrap_or(name);
                let mut c = Capability::new(&self.id(name), CapabilityKind::Mcp, description, risk);
                if t["inputSchema"].is_object() {
                    c.input_schema = t["inputSchema"].clone();
                }
                c.output_schema = t.get("outputSchema").cloned();
                c.source = self.config.name.clone();
                c.permissions = vec![format!("{}.{}", self.config.name, if risk == RiskLevel::ReadOnly { "read" } else { "write" })];
                c.metadata.idempotent = a["idempotentHint"].as_bool().unwrap_or(risk == RiskLevel::ReadOnly);
                Some(c)
            })
            .collect()
    }

    pub fn has(&self, id: &str) -> bool {
        self.tools.iter().any(|t| t["name"].as_str().is_some_and(|n| self.id(n) == id))
    }

    /// Call a tool by capability id. Blocking.
    pub fn call(&self, id: &str, args: &Value) -> Result<Value, String> {
        let tool = id.strip_prefix(&format!("{}.", self.config.name)).ok_or_else(|| format!("{id} isn't from {}", self.config.name))?;
        let result = self.request("tools/call", json!({ "name": tool, "arguments": args }))?;
        let text: Vec<String> =
            result["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str().map(str::to_string)).collect();
        if result["isError"].as_bool() == Some(true) {
            return Err(text.join("\n"));
        }
        Ok(match result.get("structuredContent") {
            Some(v) if !v.is_null() => v.clone(),
            _ => match serde_json::from_str::<Value>(&text.join("\n")) {
                Ok(v) if text.len() == 1 => v,
                _ => json!({ "content": text.join("\n") }),
            },
        })
    }

    /// Alive and answering?
    pub fn health(&self) -> CapabilityHealth {
        let exited = self.io.lock().unwrap_or_else(|e| e.into_inner()).child.try_wait().map(|s| s.is_some()).unwrap_or(true);
        if exited {
            return CapabilityHealth::Unavailable;
        }
        match self.request("ping", json!({})) {
            Ok(_) => CapabilityHealth::Healthy,
            Err(_) => CapabilityHealth::Degraded,
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let mut io = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let _ = io.child.kill();
        let _ = io.child.wait();
    }
}
