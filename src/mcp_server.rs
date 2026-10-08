//! `lyra mcp`: lyra as an MCP server (stdio) for coding harnesses — Claude
//! Code, OpenCode — on a machine paired with `lyra connect`. They can ask
//! lyra (in a conversation of its own), recall its memories, and check its
//! status, machines and routines. `lyra mcp --install` registers it with both.

use std::io::{BufRead, Write};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::connect::RemoteConfig;

const PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    let tool = |name: &str, description: &str, props: Value, required: &[&str]| {
        json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": props, "required": required } })
    };
    json!([
        tool("lyra_ask", "Ask lyra, the user's always-on assistant (it knows their machines, servers, projects and preferences, and can work on their machines). Answers in its own conversation.", json!({ "message": { "type": "string" } }), &["message"]),
        tool("lyra_memory_recall", "What lyra remembers about something (the user's setup, decisions, preferences, past fixes).", json!({ "query": { "type": "string" } }), &["query"]),
        tool("lyra_status", "lyra's status: its models, web search, APIs, public address, storage, backups and machines — what's up, degraded or down.", json!({}), &[]),
        tool("lyra_machines", "The user's machines lyra knows: online, health (disk, memory, load, failed units, updates) and coding agents.", json!({}), &[]),
        tool("lyra_routines", "Routines lyra runs on a schedule, with their last results.", json!({}), &[]),
        tool("lyra_run_routine", "Run one of lyra's routines now.", json!({ "name": { "type": "string" } }), &["name"]),
    ])
}

fn config() -> Result<RemoteConfig, String> {
    let path = crate::connect::config_path().ok_or("no config folder")?;
    let text = std::fs::read_to_string(&path).map_err(|_| "this machine isn't paired with lyra: run `lyra connect --pair <code> --url <lyra url>` first".to_string())?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(c: &RemoteConfig, session: &str) -> Result<(Ws, Value), String> {
    let url = format!("{}&session={session}", lyra_node::socket_url(&c.url, "ws", &c.token));
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.map_err(|e| format!("can't reach lyra: {e}"))?;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), ws.next()).await.map_err(|_| "lyra didn't answer")?.ok_or("lyra closed the connection")?.map_err(|e| e.to_string())?;
        if let Message::Text(t) = msg
            && let Ok(v) = serde_json::from_str::<Value>(t.as_str())
            && v["type"] == "snapshot"
        {
            return Ok((ws, v));
        }
    }
}

/// One `get` (page data) from lyra.
async fn get(c: &RemoteConfig, what: &str, arg: Value) -> Result<Value, String> {
    let (mut ws, _) = open(c, "").await?;
    ws.send(Message::Text(json!({ "type": "get", "what": what, "arg": arg, "id": 1 }).to_string().into())).await.map_err(|e| e.to_string())?;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(60), ws.next()).await.map_err(|_| "lyra didn't answer")?.ok_or("lyra closed the connection")?.map_err(|e| e.to_string())?;
        if let Message::Text(t) = msg
            && let Ok(v) = serde_json::from_str::<Value>(t.as_str())
            && v["type"] == "data"
            && v["id"] == 1
        {
            let _ = ws.close(None).await;
            return Ok(v["data"].clone());
        }
    }
}

fn session_file() -> Option<std::path::PathBuf> {
    Some(lyra_node::config_dir()?.join("mcp-session"))
}

/// The conversation `lyra mcp` uses: its own, made once.
async fn own_session(c: &RemoteConfig) -> Result<String, String> {
    if let Some(s) = session_file().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(s);
    }
    let (mut ws, first) = open(c, "").await?;
    let before = first["session_id"].as_str().unwrap_or("").to_string();
    ws.send(Message::Text(json!({ "type": "send", "text": "/new" }).to_string().into())).await.map_err(|e| e.to_string())?;
    // The server moves this connection to the new conversation and sends its snapshot.
    let id = loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), ws.next()).await.map_err(|_| "lyra didn't start a conversation")?.ok_or("lyra closed the connection")?.map_err(|e| e.to_string())?;
        if let Message::Text(t) = msg
            && let Ok(v) = serde_json::from_str::<Value>(t.as_str())
            && v["type"] == "snapshot"
            && let Some(id) = v["session_id"].as_str().filter(|id| *id != before)
        {
            break id.to_string();
        }
    };
    let _ = ws.close(None).await;
    if let Some(p) = session_file() {
        let _ = crate::store::write_text(&p, &id);
    }
    Ok(id)
}

/// Ask lyra in its conversation and wait for the reply.
async fn ask(c: &RemoteConfig, text: &str) -> Result<String, String> {
    let session = own_session(c).await?;
    let (mut ws, snapshot) = open(c, &session).await?;
    let mut messages: Vec<Value> = snapshot["messages"].as_array().cloned().unwrap_or_default();
    let mut seq = snapshot["seq"].as_u64().unwrap_or(0);
    let start = messages.len();
    ws.send(Message::Text(json!({ "type": "send", "text": text }).to_string().into())).await.map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(900);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next()).await.map_err(|_| "lyra is still working on it after 15 minutes; check the app")?.ok_or("lyra closed the connection")?.map_err(|e| e.to_string())?;
        let Message::Text(t) = msg else { continue };
        let Ok(v) = serde_json::from_str::<Value>(t.as_str()) else { continue };
        if v["seq"].as_u64().is_some_and(|s| s <= seq) {
            continue;
        }
        seq = v["seq"].as_u64().unwrap_or(seq);
        let i = v["index"].as_u64().unwrap_or(0) as usize;
        match v["type"].as_str().unwrap_or("") {
            "add" | "replace" => {
                if i >= messages.len() {
                    messages.resize(i + 1, Value::Null);
                }
                messages[i] = v["message"].clone();
                let m = &messages[i];
                if i >= start && m["role"] == "assistant" && !m["stats"].is_null() {
                    let _ = ws.close(None).await;
                    return Ok(m["content"].as_str().unwrap_or("").to_string());
                }
                if i >= start && m["role"] == "error" {
                    return Err(m["content"].as_str().unwrap_or("lyra failed").to_string());
                }
            }
            "status" if v["status"]["approvals"].as_array().is_some_and(|a| !a.is_empty()) => {
                let a = &v["status"]["approvals"][0];
                return Ok(format!("lyra is waiting for the user's approval in its app ({} {}): {}", a["agent"].as_str().unwrap_or(""), a["what"].as_str().unwrap_or(""), a["detail"].as_str().unwrap_or("")));
            }
            _ => {}
        }
    }
}

/// A short text for a tool result.
async fn call(c: &RemoteConfig, name: &str, args: &Value) -> Result<String, String> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    match name {
        "lyra_ask" => ask(c, args["message"].as_str().ok_or("message is required")?).await,
        "lyra_memory_recall" => {
            let v = get(c, "memory", json!({ "query": args["query"] })).await?;
            let rows: Vec<String> = v["memories"].as_array().into_iter().flatten().map(|m| format!("[{}] {} ({}, {})", s(&m["id"]), s(&m["content"]), s(&m["kind"]), s(&m["scope"]))).collect();
            Ok(if rows.is_empty() { "lyra remembers nothing about that".into() } else { rows.join("\n") })
        }
        "lyra_status" => {
            let v = get(c, "status", json!(null)).await?;
            let rows: Vec<String> = v["rows"].as_array().into_iter().flatten().map(|r| format!("{} {} — {} {}", s(&r["state"]), s(&r["name"]), s(&r["detail"]), r["latency_ms"].as_u64().map_or(String::new(), |m| format!("({m} ms)")))).collect();
            Ok(format!("overall: {}\n{}", s(&v["overall"]), rows.join("\n")))
        }
        "lyra_machines" => {
            let v = get(c, "machines", json!(null)).await?;
            let rows: Vec<String> = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| {
                    format!(
                        "{} — {}{}{}",
                        s(&m["name"]),
                        if m["online"] == true { "online" } else { "offline" },
                        m["health"]["summary"].as_str().map_or(String::new(), |h| format!(" · {h}")),
                        m["health"]["problems"].as_array().filter(|p| !p.is_empty()).map_or(String::new(), |p| format!(" · problems: {}", p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")))
                    )
                })
                .collect();
            Ok(rows.join("\n"))
        }
        "lyra_routines" => {
            let v = get(c, "routines", json!(null)).await?;
            let rows: Vec<String> = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| format!("{} — {} · {} · last: {}", s(&r["name"]), s(&r["schedule"]), s(&r["prompt"]), r["runs"][0]["summary"].as_str().map_or("never run".into(), |x| x.lines().next().unwrap_or("").to_string())))
                .collect();
            Ok(if rows.is_empty() { "no routines".into() } else { rows.join("\n") })
        }
        "lyra_run_routine" => {
            let v = get(c, "do", json!({ "command": format!("/routine run {}", s(&args["name"])) })).await?;
            if v["ok"] == true { Ok(s(&v["text"])) } else { Err(s(&v["text"])) }
        }
        other => Err(format!("no tool {other}")),
    }
}

/// Register `lyra mcp` with Claude Code and OpenCode.
fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string();
    let mut out = Vec::new();
    match lyra_node::coding::Harness::Claude.program() {
        Some(claude) => {
            let _ = std::process::Command::new(&claude).args(["mcp", "remove", "--scope", "user", "lyra"]).output();
            let o = std::process::Command::new(&claude).args(["mcp", "add", "--scope", "user", "lyra", "--", &exe, "mcp"]).output().map_err(|e| e.to_string())?;
            out.push(if o.status.success() { "Claude Code: added (user scope)".to_string() } else { format!("Claude Code: {}", String::from_utf8_lossy(&o.stderr).trim()) });
        }
        None => out.push("Claude Code: not installed".into()),
    }
    if lyra_node::coding::Harness::OpenCode.program().is_some() {
        let path = lyra_node::home().join(".config/opencode/opencode.json");
        let mut cfg: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({ "$schema": "https://opencode.ai/config.json" }));
        cfg["mcp"]["lyra"] = json!({ "type": "local", "command": [exe, "mcp"], "enabled": true });
        crate::store::write_json(&path, &cfg)?;
        out.push(format!("OpenCode: added to {}", path.display()));
    } else {
        out.push("OpenCode: not installed".into());
    }
    Ok(out.join("\n"))
}

/// Answer one JSON-RPC request (None for notifications).
pub fn answer(rt: &tokio::runtime::Runtime, c: &Result<RemoteConfig, String>, req: &Value) -> Option<Value> {
    let id = req.get("id")?.clone();
    let result = match req["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lyra", "version": crate::changelog::version() },
            "instructions": "lyra is the user's always-on assistant: ask it about their machines, servers, projects, routines and what it remembers.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = req["params"]["arguments"].clone();
            let out = match c {
                Ok(c) => rt.block_on(call(c, name, &args)),
                Err(e) => Err(e.clone()),
            };
            Ok(match out {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            })
        }
        m => Err(json!({ "code": -32601, "message": format!("no method {m}") })),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

pub fn main(args: &[String]) {
    if args.first().map(String::as_str) == Some("--install") {
        match install() {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("lyra mcp: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    lyra_node::tls_provider();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let c = config();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(reply) = answer(&rt, &c, &req) {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_mcp() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let none: Result<RemoteConfig, String> = Err("not paired".into());
        let init = answer(&rt, &none, &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "lyra");
        let list = answer(&rt, &none, &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "lyra_ask"));
        assert!(answer(&rt, &none, &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none(), "notifications get no answer");
        let call = answer(&rt, &none, &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "lyra_status", "arguments": {} } })).unwrap();
        assert_eq!(call["result"]["isError"], true);
        assert!(answer(&rt, &none, &json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" })).unwrap()["error"]["code"] == -32601);
    }
}
