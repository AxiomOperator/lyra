//! Lyra's side of capability intelligence (docs/done/capabilities.md): builds the
//! registry from every provider — the native memory tools, evolved
//! composite tools, OpenAPI operations, MCP servers, learned skills,
//! workflows and helper agents — and is the one way the chat and plans call
//! anything: policy first, then the call, usage recorded, writes verified.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use lyra_capabilities::mcp::{McpClient, McpConfig};
use lyra_capabilities::openapi::{OpenApiClient, OpenApiConfig};
use lyra_capabilities::{
    Capability, CapabilityHealth, CapabilityKind, CapabilityManager, CapabilityRequirement, CapabilityUsage, RiskLevel, Rule,
    Scored, VerificationRule, error_code,
};
use lyra_execution::{Risk, ToolInfo, Verification, VerificationStrategy};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::evolve::Evolution;
use crate::learn::Learning;
use crate::tools::{CallContext, Tools};

/// The tool the model uses to find capabilities it wasn't offered.
pub const SEARCH_TOOL: &str = "capability_search";

pub struct Caps {
    pub manager: CapabilityManager,
    rt: Handle,
    pub tools: Option<Arc<Tools>>,
    pub learning: Option<Arc<Learning>>,
    pub evolution: Option<Arc<Evolution>>,
    openapi: Vec<OpenApiClient>,
    mcp: Vec<McpClient>,
    /// Long-lived goals: the model can read them and note progress.
    goals: std::sync::OnceLock<Arc<crate::goals::Goals>>,
    /// Specialist agents (plans can give them steps).
    agents: std::sync::OnceLock<Arc<crate::agents::Agents>>,
    /// Shell, files, network, servers (`[system]`): for agents only.
    pub system: Option<lyra_system::System>,
    /// Web search and page reading (`[search]`).
    pub search: Option<crate::websearch::Settings>,
    /// Named sets of machines (`[groups]`, e.g. web = ["web1", "web2"]) for `fleet_run` and `@group`.
    pub groups: std::collections::HashMap<String, Vec<String>>,
    /// Other machines' system tools (`lyra node`), when serving.
    remote: std::sync::OnceLock<Arc<dyn Remote>>,
}

/// What a call needs the user's approval for.
#[derive(Debug, Clone)]
pub struct Ask {
    /// What kind of thing would happen ("run a command on this machine").
    pub what: String,
    /// Exactly what: the command, the path, the URL.
    pub detail: String,
    pub why: String,
    pub dangerous: bool,
}

/// Other machines lending lyra their tools (`lyra node`, through `lyra serve`).
pub trait Remote: Send + Sync {
    /// Every paired machine by name, and whether it's connected now. Offline
    /// ones are listed too, so "on my desktop" never quietly runs elsewhere.
    fn machines(&self) -> Vec<(String, bool)>;
    /// Send a request to a machine and wait for its answer.
    fn call(&self, machine: &str, request: Value, timeout: std::time::Duration) -> Result<Value, String>;
    /// The project folders `user`'s open pages lend: (the device, the folder).
    fn folders(&self, user: &str) -> Vec<(String, lyra_web::Folder)> {
        let _ = user;
        Vec::new()
    }
    /// Ask `user`'s own page with `folder` to do something in it.
    fn call_folder(&self, user: &str, folder: &str, request: Value, timeout: std::time::Duration) -> Result<Value, String> {
        let _ = (user, folder, request, timeout);
        Err("project folders come through lyra's app (lyra serve)".into())
    }
    /// Where a file a device sent is on this server.
    fn upload_path(&self, id: &str) -> Option<std::path::PathBuf> {
        let _ = id;
        None
    }
    /// A long job on a machine (a coding harness): progress as it comes, stoppable.
    fn call_streaming(&self, machine: &str, request: Value, timeout: std::time::Duration, cancel: &std::sync::atomic::AtomicBool, progress: &dyn Fn(Value)) -> Result<Value, String> {
        let _ = (cancel, progress);
        self.call(machine, request, timeout)
    }
    /// The coding harnesses each connected machine has ({"claude": "2.1.291", …}).
    fn harnesses(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
    /// Connected machines running Windows (their commands are PowerShell).
    fn windows_machines(&self) -> Vec<String> {
        Vec::new()
    }
}

/// What the machine lyra itself runs on is called in the `machine` argument.
pub const HERE: &str = "server";

/// The machine a system call is for, when it's another one than lyra's own.
fn remote_machine(args: &Value) -> Option<String> {
    let m = args["machine"].as_str()?.trim();
    (!m.is_empty() && !matches!(m.to_lowercase().as_str(), HERE | "local" | "localhost" | "this")).then(|| m.to_string())
}

/// The system tools as capabilities; with machines connected, each (but
/// `ssh_run`) can be pointed at one of them.
fn system_tools(machines: &[(String, bool)], windows: &[String]) -> Vec<Capability> {
    lyra_system::specs()
        .into_iter()
        .map(|s| {
            let risk = match s.risk {
                lyra_system::Risk::ReadOnly => RiskLevel::ReadOnly,
                lyra_system::Risk::Write => RiskLevel::Write,
                lyra_system::Risk::Destructive => RiskLevel::Destructive,
            };
            let mut c = Capability::new(s.name, CapabilityKind::NativeTool, s.description, risk);
            c.input_schema = s.parameters;
            if !machines.is_empty() && s.name != "ssh_run" {
                let names: Vec<String> = std::iter::once(HERE.to_string()).chain(machines.iter().map(|m| m.0.clone())).collect();
                let listed: Vec<String> = machines
                    .iter()
                    .map(|(m, on)| {
                        let win = if windows.iter().any(|w| w.eq_ignore_ascii_case(m)) { ", Windows: commands already run in PowerShell (don't wrap them in powershell/pwsh), use Windows paths" } else { "" };
                        format!("{m} ({}{win})", if *on { "online" } else { "offline now" })
                    })
                    .collect();
                c.input_schema["properties"]["machine"] = json!({
                    "type": "string",
                    "enum": names,
                    "description": format!(
                        "Where to do it: \"{HERE}\" (where lyra runs; the default) or another machine: {}. \"my desktop\", \"my PC\" and the like mean {}. Never use the server for a request about another machine.",
                        listed.join(", "),
                        machines[0].0
                    ),
                });
            }
            c.source = "system".into();
            c.tags = vec!["system".into(), s.name.split('_').next().unwrap_or("").into()];
            c.permissions = vec![format!("system.{}", s.name)];
            c
        })
        .collect()
}

/// `fleet_run`: one command on several machines, one approval, a result each.
fn fleet_capability(machines: &[(String, bool)], groups: &std::collections::HashMap<String, Vec<String>>) -> Capability {
    let names: Vec<&str> = std::iter::once(HERE).chain(machines.iter().map(|m| m.0.as_str())).collect();
    let mut groups: Vec<String> = groups.iter().map(|(g, m)| format!("{g} ({})", m.join(", "))).collect();
    groups.sort();
    let mut c = Capability::new(
        "fleet_run",
        CapabilityKind::NativeTool,
        "Run one shell command on several machines at once (\"@all\", a group, or a list): one approval covers them all, \
         each machine still checks the command against its own rules, and you get one result per machine. Use it for \
         fleet-wide chores (\"update packages on @all\", \"disk usage on @web\"); use shell_run for one machine.",
        RiskLevel::Write,
    );
    c.input_schema = json!({ "type": "object", "properties": {
        "command": { "type": "string", "description": "The shell command, the same on every machine." },
        "machines": { "type": "string", "description": format!(
            "\"all\" (the server and every online machine){}, or names separated by commas from: {}.",
            if groups.is_empty() { String::new() } else { format!(", a group: {}", groups.join("; ")) },
            names.join(", ")
        ) },
        "cwd": { "type": "string", "description": "Folder to run in (default: home)." },
        "timeout_seconds": { "type": "integer", "description": "Longest it may run on each machine." },
    }, "required": ["command", "machines"] });
    c.source = "system".into();
    c.tags = vec!["system".into(), "fleet".into(), "all".into(), "machines".into(), "shell".into()];
    c.permissions = vec!["system.fleet_run".into()];
    c
}

/// Native tools: risk, permissions, prerequisites and how to check them.
fn native(name: &str, description: &str, parameters: &Value) -> Capability {
    let risk = match name {
        "memory_recall" | "memory_list" | "memory_inspect" => RiskLevel::ReadOnly,
        "memory_forget" => RiskLevel::Destructive,
        "memory_archive" => RiskLevel::Write,
        _ => RiskLevel::LowWrite,
    };
    let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
    c.input_schema = parameters.clone();
    c.source = "native".into();
    c.permissions = vec![match risk {
        RiskLevel::ReadOnly => "memory.read",
        RiskLevel::Destructive => "memory.delete",
        _ if name == "working_memory" => "working_memory",
        _ => "memory.write",
    }
    .into()];
    c.tags = vec!["memory".into()];
    let needs_id = matches!(name, "memory_correct" | "memory_supersede" | "memory_archive" | "memory_forget" | "memory_inspect");
    if needs_id {
        c.metadata.requirements =
            vec![CapabilityRequirement { entity: "memory id".into(), resolution_capability: Some("memory_recall".into()) }];
    }
    let check = |args: Value, expression: &str| {
        Some(VerificationRule { capability: "memory_inspect".into(), arguments: args, success_expression: expression.into() })
    };
    c.metadata.verification = match name {
        "memory_remember" => check(json!({ "id": "{{result.id}}" }), "contains:{{result.id}}"),
        "memory_correct" => check(json!({ "id": "{{result.id}}" }), "contains:{{args.content}}"),
        "memory_supersede" => check(json!({ "id": "{{result.new}}" }), "contains:{{args.content}}"),
        "memory_archive" => check(json!({ "id": "{{args.id}}" }), "contains:archived"),
        "memory_forget" => check(json!({ "id": "{{args.id}}" }), "contains:deleted"),
        _ => None,
    };
    c
}

/// The model's view of long-lived goals.
fn goal_tools() -> Vec<Capability> {
    let id = json!({ "type": "string", "description": "Goal id (the 8 characters shown is enough) or part of its title." });
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "goals".into();
        c.tags = vec!["goals".into(), "progress".into()];
        c.permissions = vec![if risk == RiskLevel::ReadOnly { "goals.read" } else { "goals.write" }.into()];
        c
    };
    vec![
        tool("goal_list", "List the long-term goals by priority, with status and progress.", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "goal_get",
            "A goal's details: progress and where it stands, subgoals, plans so far, blockers.",
            RiskLevel::ReadOnly,
            json!({ "id": id }),
            &["id"],
        ),
        tool(
            "goal_create",
            "Propose a new long-term goal (the user accepts it with /goal activate). Only for lasting objectives, not one-off requests.",
            RiskLevel::LowWrite,
            json!({
                "title": { "type": "string" },
                "description": { "type": "string" },
                "success_criteria": { "type": "array", "items": { "type": "string" } },
                "priority": { "type": "integer", "description": "0-10" },
            }),
            &["title"],
        ),
        tool(
            "goal_note",
            "Record progress on a goal: where it stands now, and optionally how many of how many pieces are done.",
            RiskLevel::LowWrite,
            json!({
                "id": id,
                "summary": { "type": "string" },
                "completed_items": { "type": "integer" },
                "total_items": { "type": "integer" },
            }),
            &["id", "summary"],
        ),
    ]
}

/// Run a goal tool.
fn goal_tool(goals: &crate::goals::Goals, name: &str, args: &Value) -> Result<Value, String> {
    let m = &goals.manager;
    let brief = |g: &lyra_goals::Goal| {
        json!({
            "id": g.short(), "title": g.title, "status": g.status.as_str(), "priority": g.priority,
            "progress": (g.progress * 100.0).round() / 100.0, "where_it_stands": g.progress_detail.summary,
        })
    };
    match name {
        "goal_list" => Ok(json!({ "goals": m.ranked()?.iter().map(|(g, _)| brief(g)).collect::<Vec<_>>() })),
        "goal_get" => {
            let g = m.find(args["id"].as_str().unwrap_or(""))?;
            let mut v = brief(&g);
            v["description"] = json!(g.description);
            v["success_criteria"] = json!(g.success_criteria);
            v["items"] = json!({ "completed": g.progress_detail.completed_items, "total": g.progress_detail.total_items });
            v["subgoals"] = json!(m.children(g.id)?.iter().map(brief).collect::<Vec<_>>());
            v["plans"] = json!(m.plans(g.id)?.iter().map(|p| json!({ "attempt": p.attempt, "outcome": p.outcome, "summary": p.summary })).collect::<Vec<_>>());
            v["blockers"] = json!(m.blockers(Some(g.id), true)?.iter().map(|b| format!("{}: {}", b.blocker_type.as_str(), b.reason)).collect::<Vec<_>>());
            Ok(v)
        }
        "goal_create" => {
            let g = m.create(lyra_goals::prompts::agent_goal(args)?)?;
            Ok(json!({ "result": "proposed", "id": g.short(), "note": format!("the user can accept it with /goal activate {}", g.short()) }))
        }
        "goal_note" => {
            let mut g = m.find(args["id"].as_str().unwrap_or(""))?;
            let summary = args["summary"].as_str().filter(|s| !s.trim().is_empty()).ok_or("a progress note needs a summary")?;
            let d = &mut g.progress_detail;
            d.summary = summary.trim().to_string();
            d.updated_at = Some(chrono::Utc::now());
            if let Some(n) = args["completed_items"].as_u64() {
                d.completed_items = n as u32;
            }
            if let Some(n) = args["total_items"].as_u64() {
                d.total_items = Some(n as u32);
            }
            if let Some(t) = d.total_items.filter(|t| *t > 0) {
                g.progress = (d.completed_items as f32 / t as f32).clamp(0.0, 1.0);
            }
            m.update(&g, &format!("progress: {summary}"))?;
            Ok(json!({ "result": "noted", "id": g.short(), "progress": (g.progress * 100.0).round() / 100.0 }))
        }
        _ => Err(format!("unknown goal tool {name}")),
    }
}

/// The planner's risk scale.
pub fn plan_risk(r: RiskLevel) -> Risk {
    match r {
        RiskLevel::ReadOnly => Risk::ReadOnly,
        RiskLevel::LowWrite | RiskLevel::Write => Risk::Mutating,
        RiskLevel::Destructive | RiskLevel::Privileged => Risk::Destructive,
    }
}

/// A spec served by the API itself: fetched once a day into
/// `~/.lyra/capabilities/specs/<name>.json`, the copy used when it can't be.
fn spec_from_url(name: &str, url: &str) -> Result<String, String> {
    let cache = crate::config::home().map(|h| h.join("capabilities").join("specs").join(format!("{name}.json")));
    let fresh = cache.as_ref().and_then(|p| p.metadata().ok()).and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < std::time::Duration::from_secs(86_400));
    let cached = || cache.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    if fresh && let Some(text) = cached() {
        return Ok(text);
    }
    let fetched = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .and_then(|c| c.get(url).send())
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| format!("{url}: {e}"));
    match fetched {
        Ok(text) => {
            if let Some(p) = &cache {
                let _ = p.parent().map(std::fs::create_dir_all);
                let _ = std::fs::write(p, &text);
            }
            Ok(text)
        }
        Err(e) => cached().ok_or(e),
    }
}

impl Caps {
    /// Start the configured MCP servers and load the OpenAPI specs. Returns
    /// the providers and notes about the ones that failed.
    pub fn connect(openapi: &[OpenApiConfig], mcp: &[McpConfig], expand: impl Fn(&str) -> std::path::PathBuf) -> (Vec<OpenApiClient>, Vec<McpClient>, Vec<String>) {
        let mut notes = Vec::new();
        let mut specs = Vec::new();
        for c in openapi {
            let mut c = c.clone();
            // A token from secrets.toml, handed over (never in the config).
            if let Some(name) = &c.auth_secret {
                c.secret = crate::secrets::token(name);
            }
            let text = if c.spec.starts_with("http://") || c.spec.starts_with("https://") { spec_from_url(&c.name, &c.spec) } else {
                let path = expand(&c.spec);
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
            };
            match text.and_then(|t| OpenApiClient::load(c.clone(), &t)) {
                Ok(client) => {
                    notes.push(format!("openapi {}: {} operations", c.name, client.capabilities().len()));
                    specs.push(client);
                }
                Err(e) => notes.push(format!("openapi {} not loaded: {e}", c.name)),
            }
        }
        let mut servers = Vec::new();
        for c in mcp {
            match McpClient::start(c.clone()) {
                Ok(client) => {
                    notes.push(format!("mcp {} ({}): {} tools", c.name, client.server.trim(), client.capabilities().len()));
                    servers.push(client);
                }
                Err(e) => notes.push(format!("mcp {} not started: {e}", c.name)),
            }
        }
        (specs, servers, notes)
    }

    pub fn new(manager: CapabilityManager, rt: Handle, openapi: Vec<OpenApiClient>, mcp: Vec<McpClient>) -> Self {
        Self { manager, rt, groups: Default::default(), tools: None, learning: None, evolution: None, openapi, mcp, goals: std::sync::OnceLock::new(), agents: std::sync::OnceLock::new(), system: None, remote: std::sync::OnceLock::new(), search: None }
    }

    /// Add the goal tools (goal_list, goal_get, goal_create, goal_note).
    pub fn set_remote(&self, remote: Arc<dyn Remote>) {
        let _ = self.remote.set(remote);
    }

    /// Paired machines and whether they're connected (none without `lyra serve`).
    pub fn machines(&self) -> Vec<(String, bool)> {
        self.remote.get().map(|r| r.machines()).unwrap_or_default()
    }

    /// How long a call on another machine may take: its command, plus the trip.
    fn remote_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.system.as_ref().map_or(60, |s| s.settings().timeout_seconds).max(60) + 30)
    }

    /// The machines `spec` names: "all" (the server and every online
    /// machine), a `[groups]` name, or names separated by commas.
    pub fn fleet_targets(&self, spec: &str) -> Result<Vec<String>, String> {
        let known = self.machines();
        let spec = spec.trim().trim_start_matches('@').to_lowercase();
        let wanted: Vec<String> = if spec == "all" {
            std::iter::once(HERE.to_string()).chain(known.iter().filter(|m| m.1).map(|m| m.0.clone())).collect()
        } else if let Some(group) = self.groups.get(&spec) {
            group.clone()
        } else {
            spec.split([',', ' ']).map(|w| w.trim().trim_start_matches('@').to_string()).filter(|w| !w.is_empty()).collect()
        };
        let mut out: Vec<String> = Vec::new();
        for w in wanted {
            let name = if w.eq_ignore_ascii_case(HERE) { HERE.to_string() } else { known.iter().find(|m| m.0.eq_ignore_ascii_case(&w)).map(|m| m.0.clone()).ok_or_else(|| format!("no machine {w:?}"))? };
            if !out.contains(&name) {
                out.push(name);
            }
        }
        if out.is_empty() {
            return Err(format!("{spec:?} names no machines"));
        }
        Ok(out)
    }

    fn fleet_args(args: &Value) -> Value {
        let mut a = json!({ "command": args["command"] });
        for k in ["cwd", "timeout_seconds"] {
            if !args[k].is_null() {
                a[k] = args[k].clone();
            }
        }
        a
    }

    /// One approval for the machines that ask; machines that run it freely aren't in it.
    fn fleet_approval(&self, args: &Value) -> Option<Ask> {
        let targets = self.fleet_targets(args["machines"].as_str().unwrap_or("")).ok()?;
        let shell = Self::fleet_args(args);
        let checks: Vec<(String, Option<(String, bool)>)> = std::thread::scope(|s| {
            let handles: Vec<_> = targets
                .iter()
                .map(|m| {
                    let shell = &shell;
                    s.spawn(move || {
                        let ask = if m == HERE {
                            match self.system.as_ref().map(|sys| sys.check("shell_run", shell)) {
                                Some(lyra_system::Check::Ask { why, dangerous }) => Some((why, dangerous)),
                                _ => None,
                            }
                        } else {
                            self.remote.get().and_then(|r| r.call(m, json!({ "type": "check", "tool": "shell_run", "args": shell }), std::time::Duration::from_secs(30)).ok()).and_then(|v| {
                                (v["check"] == "ask").then(|| (v["why"].as_str().unwrap_or("").to_string(), v["dangerous"] == true))
                            })
                        };
                        (m.clone(), ask)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let asking: Vec<&(String, Option<(String, bool)>)> = checks.iter().filter(|c| c.1.is_some()).collect();
        if asking.is_empty() {
            return None;
        }
        let mut whys: Vec<String> = asking.iter().filter_map(|c| c.1.as_ref().map(|a| a.0.clone())).filter(|w| !w.is_empty()).collect();
        whys.dedup();
        Some(Ask {
            what: format!("run a command on {}", asking.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(", ")),
            detail: format!("{}{}", args["command"].as_str().unwrap_or(""), args["cwd"].as_str().map_or(String::new(), |c| format!(" (in {c})"))),
            why: whys.join("; "),
            dangerous: asking.iter().any(|c| c.1.as_ref().is_some_and(|a| a.1)),
        })
    }

    /// Run the command on every target at once; one result each.
    fn fleet_run(&self, args: &Value, approved: bool) -> Result<Value, String> {
        if args["command"].as_str().is_none_or(|c| c.trim().is_empty()) {
            return Err("command is required".into());
        }
        let targets = self.fleet_targets(args["machines"].as_str().unwrap_or(""))?;
        let shell = Self::fleet_args(args);
        let results: Vec<Value> = std::thread::scope(|s| {
            let handles: Vec<_> = targets
                .iter()
                .map(|m| {
                    let shell = &shell;
                    s.spawn(move || {
                        let out = if m == HERE {
                            match &self.system {
                                None => Err("system access is off".to_string()),
                                Some(sys) => match sys.check("shell_run", shell) {
                                    lyra_system::Check::Forbidden(why) => Err(format!("refused: {why}")),
                                    lyra_system::Check::Ask { why, .. } if !approved => Err(format!("needs the user's approval ({why})")),
                                    _ => sys.call("shell_run", shell),
                                },
                            }
                        } else {
                            match self.remote.get() {
                                Some(r) => r.call(m, json!({ "type": "call", "tool": "shell_run", "args": shell, "approved": approved }), self.remote_timeout()),
                                None => Err("other machines connect to `lyra serve`".to_string()),
                            }
                        };
                        let mut v = match out {
                            Ok(v) if v.is_object() => v,
                            Ok(v) => json!({ "output": v }),
                            Err(e) => json!({ "error": e }),
                        };
                        let ok = v["error"].is_null() && v["exit_code"].as_i64().is_none_or(|c| c == 0);
                        v["machine"] = json!(m);
                        v["ok"] = json!(ok);
                        v
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let ok = results.iter().filter(|r| r["ok"] == true).count();
        Ok(json!({ "machines": results.len(), "ok": ok, "failed": results.len() - ok, "results": results }))
    }

    /// Every OpenAPI and MCP provider, checked now: (kind, name, where, health,
    /// how many capabilities it gives). Blocking: call from a worker thread.
    pub fn providers(&self) -> Vec<(&'static str, String, String, lyra_capabilities::CapabilityHealth, usize)> {
        let mut out: Vec<_> = self.openapi.iter().map(|c| ("OpenAPI", c.config.name.clone(), c.base_url().to_string(), c.health(), c.capabilities().len())).collect();
        out.extend(self.mcp.iter().map(|c| ("MCP", c.config.name.clone(), format!("{} {}", c.config.command, c.config.args.join(" ")).trim().to_string(), c.health(), c.capabilities().len())));
        out
    }

    /// The machines connection (lyra serve), when there is one.
    pub fn remote(&self) -> Option<Arc<dyn Remote>> {
        self.remote.get().cloned()
    }

    pub fn set_agents(&self, agents: Arc<crate::agents::Agents>) {
        let _ = self.agents.set(agents);
    }

    pub fn agents(&self) -> Option<&Arc<crate::agents::Agents>> {
        self.agents.get()
    }

    pub fn set_goals(&self, goals: Arc<crate::goals::Goals>) {
        let _ = self.goals.set(goals);
        self.refresh();
    }

    fn run<T>(&self, f: impl Future<Output = anyhow::Result<T>>) -> Result<T, String> {
        self.rt.block_on(f).map_err(|e| format!("{e:#}"))
    }

    /// Every capability from every provider (C2, C11).
    pub fn collect(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        if let Some(tools) = &self.tools {
            let base: Vec<String> = tools.base_tools().into_iter().map(|(n, _)| n).collect();
            for d in tools.definitions().as_array().into_iter().flatten() {
                let name = d["function"]["name"].as_str().unwrap_or("");
                let description = d["function"]["description"].as_str().unwrap_or("");
                if base.iter().any(|b| b == name) {
                    caps.push(native(name, description, &d["function"]["parameters"]));
                } else {
                    // Generated by evolution from the tools above (C11).
                    let mut c = Capability::new(name, CapabilityKind::Composite, description, RiskLevel::LowWrite);
                    c.input_schema = d["function"]["parameters"].clone();
                    c.source = "evolution".into();
                    c.tags = vec!["composite".into()];
                    caps.push(c);
                }
            }
        }
        if self.goals.get().is_some() {
            caps.extend(goal_tools());
        }
        if self.search.as_ref().is_some_and(|s| s.enabled) {
            caps.extend(crate::websearch::capabilities());
        }
        caps.extend(crate::routines::capabilities());
        caps.extend(crate::notes::capabilities());
        caps.extend(crate::people::capabilities());
        // Folders lent by people's open pages (only through lyra serve).
        if self.remote.get().is_some() {
            caps.extend(crate::projects::capabilities());
        }
        if crate::coding::settings().enabled {
            caps.push(crate::coding::capability());
        }
        if crate::pmi::anyone() {
            caps.extend(crate::pmi::capabilities());
        }
        if crate::calendar::available() {
            caps.extend(crate::calendar::capabilities());
            caps.extend(crate::mail::capabilities());
            caps.extend(crate::style::capabilities());
            caps.extend(crate::teams::capabilities());
            caps.extend(crate::files::capabilities());
        }
        if self.system.as_ref().is_some_and(|s| s.settings().enabled) {
            let machines = self.machines();
            if !machines.is_empty() {
                caps.push(fleet_capability(&machines, &self.groups));
            }
            let windows = self.remote.get().map(|r| r.windows_machines()).unwrap_or_default();
            caps.extend(system_tools(&machines, &windows));
        }
        caps.extend(self.openapi.iter().flat_map(OpenApiClient::capabilities));
        caps.extend(self.mcp.iter().flat_map(McpClient::capabilities));
        // Learned procedures and workflows are capabilities too (C10): the
        // planner can use them as workflow steps.
        if let Some(learning) = &self.learning {
            for s in learning.active_skills().unwrap_or_default() {
                let mut c = Capability::new(&s.name, CapabilityKind::Skill, &s.description, RiskLevel::LowWrite);
                c.source = "skills".into();
                c.metadata.success_rate = s.usage.success_rate();
                caps.push(c);
            }
        }
        if let Some(evolution) = &self.evolution {
            for w in evolution.manager.workflows().0 {
                let mut c = Capability::new(&format!("workflow.{}", w.name), CapabilityKind::Workflow, &w.description, RiskLevel::LowWrite);
                c.source = "evolution".into();
                c.tags = w.triggers.clone();
                caps.push(c);
            }
        }
        for a in self.agents.get().map(|a| a.registry.enabled()).unwrap_or_default() {
            let mut c = Capability::new(&format!("agent.{}", a.name), CapabilityKind::Subagent, &a.description, lyra_agents::delegation::max_risk(&a));
            c.source = "agents".into();
            c.tags = a.delegation.keywords.clone();
            caps.push(c);
        }
        caps
    }

    /// Rebuild the registry from the providers. Returns notes.
    pub fn refresh(&self) -> Vec<String> {
        self.run(self.manager.set_capabilities(self.collect())).unwrap_or_else(|e| vec![format!("capability index: {e}")])
    }

    /// Check every provider's availability (C8). Blocking; returns notes.
    pub fn check_health(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for c in &self.openapi {
            let h = c.health();
            self.manager.set_health(&format!("{}.*", c.config.name), h);
            if h != CapabilityHealth::Healthy {
                notes.push(format!("openapi {} is {}", c.config.name, h.as_str()));
            }
        }
        for c in &self.mcp {
            let h = c.health();
            self.manager.set_health(&format!("{}.*", c.config.name), h);
            if h != CapabilityHealth::Healthy {
                notes.push(format!("mcp {} is {}", c.config.name, h.as_str()));
            }
        }
        if let Some(tools) = &self.tools {
            let h = match tools.mem.run(tools.mem.manager.list(&lyra_memory::Filter::default(), 1)) {
                Ok(_) => CapabilityHealth::Healthy,
                Err(_) => CapabilityHealth::Unavailable,
            };
            self.manager.set_health("native.*", h);
            if h != CapabilityHealth::Healthy {
                notes.push("memory tools are unavailable".into());
            }
        }
        notes
    }

    /// Callable capabilities usable now by the main agent and plans (system
    /// access is only for agents whose profile allows it).
    fn callable(&self) -> Vec<Capability> {
        self.manager.usable().into_iter().filter(|c| c.kind.callable() && c.source != "system").collect()
    }

    /// Whether this call needs the user's approval first, and for what: the
    /// policy's approval rule, or a system call that changes things.
    pub fn approval(&self, name: &str, arguments: &str) -> Option<Ask> {
        let c = self.manager.get(name)?;
        let args: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).unwrap_or(json!({}));
        if c.name == "fleet_run" {
            return self.fleet_approval(&args);
        }
        if c.source == "coding" {
            return Some(crate::coding::approval(&args));
        }
        if c.source == "pmi" {
            return crate::pmi::approval(&c.name, &args);
        }
        if c.source == "calendar" {
            return crate::calendar::approval(&c.name, &args);
        }
        if c.source == "mail" {
            return crate::mail::approval(&c.name, &args);
        }
        if c.source == "projects" {
            return crate::projects::approval(self.remote.get().map(|r| r.as_ref()), &c.name, &args);
        }
        // Another machine decides for itself (its own rules), and says what it would do.
        if c.source == "system"
            && let Some(machine) = remote_machine(&args)
        {
            let remote = self.remote.get()?;
            let v = remote.call(&machine, json!({ "type": "check", "tool": name, "args": args }), std::time::Duration::from_secs(30)).ok()?;
            return (v["check"] == "ask").then(|| Ask {
                what: v["what"].as_str().unwrap_or("change something").to_string(),
                detail: v["detail"].as_str().unwrap_or("").to_string(),
                why: v["why"].as_str().unwrap_or("").to_string(),
                dangerous: v["dangerous"] == true,
            });
        }
        if c.source == "system"
            && let Some(system) = &self.system
            && let lyra_system::Check::Ask { why, dangerous } = system.check(name, &args)
        {
            let (what, detail) = system.describe(name, &args);
            return Some(Ask { what, detail, why, dangerous });
        }
        (self.manager.rule(&c) == Rule::Approval).then(|| Ask {
            what: format!("use {name}"),
            detail: args.to_string().chars().take(300).collect(),
            why: format!("{} needs approval ({} risk)", c.name, c.risk.as_str()),
            dangerous: c.risk >= RiskLevel::Destructive,
        })
    }

    /// The tools to offer the model for `query` (C3): all of them when there
    /// are few; otherwise the ones discovery finds, plus `extra` (found with
    /// the search tool) and the search tool itself.
    pub fn definitions(&self, query: &str, extra: &HashSet<String>) -> Vec<Value> {
        let all = self.callable();
        let s = self.manager.settings();
        if all.len() <= s.max_tools {
            return all.iter().map(Capability::definition).collect();
        }
        let found = self.run(self.manager.discover(query, s.discovery_limit, &[])).unwrap_or_default();
        let mut chosen: Vec<&Capability> = all
            .iter()
            .filter(|c| found.iter().any(|f| f.capability.id == c.id) || extra.contains(&c.name) || c.name == "memory_recall")
            .collect();
        chosen.dedup_by(|a, b| a.id == b.id);
        let mut defs: Vec<Value> = chosen.into_iter().map(Capability::definition).collect();
        defs.push(json!({
            "type": "function",
            "function": {
                "name": SEARCH_TOOL,
                "description": "Find tools, workflows and skills for a task when the ones offered don't fit. Returns the best matches; the tools among them become available.",
                "parameters": { "type": "object", "properties": { "query": { "type": "string", "description": "What you need to do." } }, "required": ["query"] },
            },
        }));
        defs
    }

    /// `capability_search`: the best matches, as JSON for the model, and the
    /// names of the callable ones (to offer from now on).
    pub fn search(&self, arguments: &str) -> (String, Vec<String>) {
        let query = serde_json::from_str::<Value>(arguments).ok().and_then(|v| v["query"].as_str().map(str::to_string)).unwrap_or_default();
        let found = self.run(self.manager.discover(&query, 10, &[])).unwrap_or_default();
        let names = found.iter().filter(|f| f.capability.kind.callable() && f.capability.source != "system").map(|f| f.capability.name.clone()).collect();
        let list: Vec<Value> = found.iter().map(|f| self.describe(f)).collect();
        (json!({ "capabilities": list }).to_string(), names)
    }

    fn describe(&self, f: &Scored) -> Value {
        let c = &f.capability;
        json!({
            "name": c.name,
            "kind": c.kind.as_str(),
            "description": c.description,
            "risk": c.risk.as_str(),
            "policy": f.rule.as_str(),
            "success_rate": c.metadata.success_rate,
            "score": (f.score.total * 100.0).round() / 100.0,
        })
    }

    /// Call a capability: policy (C4) and health (C8) first, then the call,
    /// then usage (C5) and, for successful writes, verification (C9).
    /// `approved` means a person approved this call (a plan step's approval).
    /// `verify` is off when the caller verifies itself (the plan engine).
    pub fn invoke(&self, name: &str, arguments: &str, ctx: CallContext, approved: bool, verify: bool) -> String {
        let Some(c) = self.manager.get(name) else {
            return json!({ "error": format!("unknown tool {name}") }).to_string();
        };
        if !c.kind.callable() {
            return json!({ "error": format!("{} is a {}, not a tool: use it in a plan", c.id, c.kind.as_str()) }).to_string();
        }
        match self.manager.rule(&c) {
            Rule::Deny => return json!({ "error": format!("{} is not allowed by policy", c.id) }).to_string(),
            Rule::Approval if !approved => {
                return json!({
                    "error": format!("{} needs the user's approval", c.id),
                    "hint": format!("ask the user; they can allow it with /caps allow {}, or plan it with /plan", c.name),
                })
                .to_string();
            }
            _ => {}
        }
        // Members: no machines, system tools or coding; nothing that's still the owner's alone.
        if ctx.member {
            if matches!(c.source.as_str(), "system" | "coding") || c.name == "fleet_run" {
                return json!({ "error": "machines, system tools and coding agents are for admins" }).to_string();
            }
            if matches!(c.kind, CapabilityKind::OpenApi | CapabilityKind::Mcp) {
                return json!({ "error": format!("{} uses lyra's own credentials: that's for admins", c.name) }).to_string();
            }
            if c.name == "working_memory" {
                return json!({ "error": format!("{} isn't set up for your account yet", c.name) }).to_string();
            }
        }
        // System access: only agents whose profile allows it (checked by the
        // delegation), and every call checked again here.
        if c.source == "system" {
            if ctx.agent.is_none() {
                return json!({ "error": format!("{} is only for agents with system access: hand the task to the operator agent", c.name) }).to_string();
            }
            if c.name == "fleet_run" {
                let args: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).unwrap_or(json!({}));
                return self.fleet_run(&args, approved).unwrap_or_else(|e| json!({ "error": e })).to_string();
            }
            let Some(system) = &self.system else { return json!({ "error": "system access is off" }).to_string() };
            let args: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).unwrap_or(json!({}));
            // Another machine checks the call itself before running it.
            let local = remote_machine(&args).is_none();
            match if local { system.check(&c.name, &args) } else { lyra_system::Check::Auto } {
                lyra_system::Check::Forbidden(why) => return json!({ "error": format!("refused: {why}") }).to_string(),
                lyra_system::Check::Ask { why, .. } if !approved => {
                    return json!({ "error": format!("needs the user's approval ({why})") }).to_string();
                }
                _ => {}
            }
        }
        if self.manager.health(&c) == CapabilityHealth::Unavailable {
            return json!({ "error": format!("{} is unavailable right now", c.id) }).to_string();
        }
        let mut args: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).unwrap_or(json!({}));
        // Only lyra says where an upload is.
        if let Some(map) = args.as_object_mut() {
            map.remove("from");
        }
        let start = Instant::now();
        let outcome: Result<Value, String> = match c.kind {
            CapabilityKind::OpenApi => match self.openapi.iter().find(|o| o.has(&c.id)) {
                Some(client) => client.call(&c.id, &args),
                None => Err(format!("{} has no provider", c.id)),
            },
            CapabilityKind::Mcp => match self.mcp.iter().find(|m| m.has(&c.id)) {
                Some(client) => client.call(&c.id, &args),
                None => Err(format!("{} has no provider", c.id)),
            },
            _ if c.source == "coding" => {
                if ctx.agent.is_none() {
                    Err("coding work goes to the Coder agent: delegate it".into())
                } else if !approved {
                    Err("needs the user's approval".into())
                } else {
                    Ok(crate::coding::run(self, &args, &std::sync::atomic::AtomicBool::new(false), &|_| {}, None))
                }
            }
            // PMI: what others would see waits for the user's yes (asked in an agent's delegation).
            _ if c.source == "pmi" => match crate::pmi::approval(&c.name, &args) {
                Some(ask) if !approved && ctx.agent.is_none() => Err(format!("{} changes something others see ({}): delegate it to the Project Manager, which asks the user", c.name, ask.what)),
                Some(_) if !approved => Err("needs the user's approval".into()),
                _ => crate::pmi::call(&c.name, &args),
            },
            // The calendar of whoever this turn is for; what others see needs their yes.
            _ if c.source == "calendar" => match crate::calendar::approval(&c.name, &args) {
                Some(ask) if !approved => Err(format!("{} needs the user's approval ({})", c.name, ask.what)),
                _ => crate::calendar::call(&c.name, &args),
            },
            // The mail of whoever this turn is for; sending needs their yes.
            _ if c.source == "mail" => match crate::mail::approval(&c.name, &args) {
                Some(ask) if !approved => Err(format!("{} needs the user's approval ({})", c.name, ask.what)),
                _ => crate::mail::call(&c.name, &args),
            },
            // Folders on the person's own PC, through their own open page; a write needs their yes.
            _ if c.source == "projects" => match crate::projects::approval(self.remote.get().map(|r| r.as_ref()), &c.name, &args) {
                Some(ask) if !approved => Err(format!("{} needs the user's approval ({})", c.name, ask.what)),
                _ => crate::projects::call(self.remote.get().map(|r| r.as_ref()), &c.name, &args),
            },
            _ if c.source == "style" => crate::style::call(&c.name, &args),
            // Teams and files of whoever this turn is for (read-only; attaching only touches their draft).
            _ if c.source == "teams" => crate::teams::call(&c.name, &args),
            _ if c.source == "files" => crate::files::call(&c.name, &args),
            // The notes and people of whoever this turn is for.
            _ if c.source == "notes" => crate::notes::call(&c.name, &args),
            _ if c.source == "people" => crate::people::call(&c.name, &args, self.tools.as_ref().map(|t| t.mem.as_ref())),
            _ if c.source == "routines" => crate::routines::call(&c.name, &args),
            _ if c.source == "web" => match &self.search {
                Some(s) => crate::websearch::call(s, &c.name, &args),
                None => Err("web search is off".into()),
            },
            _ if c.source == "system" => match (remote_machine(&args), &self.system) {
                (Some(machine), _) => match self.remote.get() {
                    Some(remote) => remote.call(&machine, json!({ "type": "call", "tool": c.name, "args": args, "approved": approved }), self.remote_timeout()),
                    None => Err(format!("{machine} isn't reachable: other machines connect to `lyra serve`")),
                },
                (None, Some(system)) if c.name == "upload_place" => {
                    // The source is the upload the user sent, never a path the model picks.
                    let mut args = args.clone();
                    let from = args["upload"].as_str().and_then(|id| self.remote.get().and_then(|r| r.upload_path(id)));
                    match (from, args.as_object_mut()) {
                        (Some(from), Some(map)) => {
                            map.insert("from".into(), json!(from.display().to_string()));
                            system.call(&c.name, &args)
                        }
                        _ => Err("no such upload (the id is in the user's message)".into()),
                    }
                }
                (None, Some(system)) => system.call(&c.name, &args),
                (None, None) => Err("system access is off".into()),
            },
            // The goals of whoever this turn is for.
            _ if c.source == "goals" => match crate::goals::for_user(&crate::acting::current()) {
                Some(goals) => goal_tool(&goals, &c.name, &args),
                None => Err("goals are off".into()),
            },
            _ => match &self.tools {
                Some(tools) => {
                    let text = tools.run(&c.name, arguments, ctx);
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
                    match v.get("error").and_then(Value::as_str) {
                        Some(e) => Err(e.to_string()),
                        None => Ok(v),
                    }
                }
                None => Err("tools are off".into()),
            },
        };
        let duration_ms = start.elapsed().as_millis() as u64;
        let usage = CapabilityUsage {
            capability_id: c.id.clone(),
            run_id: ctx.run,
            success: outcome.is_ok(),
            duration_ms,
            retries: 0,
            error_code: outcome.as_ref().err().map(|e| error_code(e)),
            error: outcome.as_ref().err().map(|e| e.chars().take(300).collect()),
        };
        let _ = self.run(self.manager.record(usage));
        let mut result = match outcome {
            Ok(v) => v,
            Err(e) => return json!({ "error": e }).to_string(),
        };
        if verify
            && c.risk != RiskLevel::ReadOnly
            && let Some(rule) = &c.metadata.verification
        {
            let (verified, reason) = self.verify(rule, &result, &args, ctx);
            if let Value::Object(map) = &mut result {
                map.insert("verified".into(), json!(verified));
                map.insert("verification".into(), json!(reason));
            } else {
                result = json!({ "result": result, "verified": verified, "verification": reason });
            }
        }
        result.to_string()
    }

    /// Run a verification rule against a call's result (C9).
    fn verify(&self, rule: &VerificationRule, result: &Value, args: &Value, ctx: CallContext) -> (bool, String) {
        let arguments = lyra_execution::check::fill(&rule.arguments, result, args);
        let text = self.invoke(&rule.capability, &arguments.to_string(), ctx, true, false);
        let out: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if let Some(e) = out.get("error").and_then(Value::as_str) {
            return (false, format!("{} failed: {e}", rule.capability));
        }
        let expression = lyra_execution::check::fill(&json!(rule.success_expression), result, args);
        let (ok, reason) = lyra_execution::check::check(&out, expression.as_str().unwrap_or(""));
        (ok, format!("{}: {reason}", rule.capability))
    }

    /// The planner's view of a callable capability (C3, C4, C7, C9).
    pub fn tool_info(&self, c: &Capability) -> ToolInfo {
        let mut info = ToolInfo::new(&c.name, &c.description, plan_risk(c.risk), c.input_schema.clone());
        info.requires_approval = self.manager.rule(c) == Rule::Approval;
        info.verification = c.metadata.verification.as_ref().map(|r| Verification {
            strategy: VerificationStrategy::FollowUpTool,
            tool: self.manager.get(&r.capability).map(|v| v.name),
            arguments: Some(r.arguments.clone()),
            check: Some(r.success_expression.clone()),
        });
        let mut notes = Vec::new();
        if let Some(r) = c.metadata.success_rate {
            notes.push(format!("{:.0}% success", r * 100.0));
        }
        if let Some(ms) = c.metadata.average_latency_ms {
            notes.push(format!("~{ms}ms"));
        }
        if self.manager.health(c) == CapabilityHealth::Degraded {
            notes.push("degraded lately".into());
        }
        for r in &c.metadata.requirements {
            let from = r.resolution_capability.as_deref().and_then(|id| self.manager.get(id)).map(|x| format!(" (from {})", x.name)).unwrap_or_default();
            notes.push(format!("needs {}{from}", r.entity));
        }
        info.notes = notes.join(" · ");
        info
    }

    /// The tools a plan for `goal` should consider: all when few, else the
    /// discovered ones plus whatever resolves their requirements.
    pub fn plan_tools(&self, goal: &str) -> Vec<ToolInfo> {
        let all = self.callable();
        let s = self.manager.settings();
        if all.len() <= s.max_tools {
            return all.iter().map(|c| self.tool_info(c)).collect();
        }
        let found = self.run(self.manager.discover(goal, s.discovery_limit * 2, &[])).unwrap_or_default();
        let mut ids: Vec<String> = found.iter().filter(|f| f.capability.kind.callable()).map(|f| f.capability.id.clone()).collect();
        // C7: what resolves their prerequisites, and what verifies them.
        for f in &found {
            let m = &f.capability.metadata;
            ids.extend(m.requirements.iter().filter_map(|r| r.resolution_capability.clone()));
            ids.extend(m.verification.iter().map(|v| v.capability.clone()));
        }
        all.iter().filter(|c| ids.contains(&c.id)).map(|c| self.tool_info(c)).collect()
    }

    /// Function definitions of every usable callable capability.
    pub fn tool_definitions(&self) -> Vec<Value> {
        self.callable().iter().map(Capability::definition).collect()
    }

    /// A capability's risk level, by name.
    pub fn risk_level(&self, name: &str) -> Option<RiskLevel> {
        self.manager.get(name).map(|c| c.risk)
    }

    /// The planner's risk for a tool, by name.
    pub fn risk_of(&self, name: &str) -> Risk {
        self.manager.get(name).map_or_else(|| crate::plan::risk(name), |c| plan_risk(c.risk))
    }

    /// Every callable capability for the planner (argument filling, validation).
    pub fn all_tools(&self) -> Vec<ToolInfo> {
        self.callable().iter().map(|c| self.tool_info(c)).collect()
    }
}

// ---- commands

pub const COMMANDS: &str = "\
/caps                        capabilities by kind, with health, policy and track record
/caps search <what>          discovery: the best capabilities for a task, with their scores
/caps show <name>            one capability: risk, permissions, requirements, verification, stats
/caps allow <name>           allow a capability that needs approval, for this session
/caps health                 check every provider's availability now";

pub fn list(caps: &Caps) -> String {
    let all = caps.manager.all();
    if all.is_empty() {
        return "no capabilities".into();
    }
    let mut out = Vec::new();
    for kind in [
        CapabilityKind::NativeTool,
        CapabilityKind::Composite,
        CapabilityKind::OpenApi,
        CapabilityKind::Mcp,
        CapabilityKind::Workflow,
        CapabilityKind::Skill,
        CapabilityKind::Subagent,
    ] {
        let of: Vec<&Capability> = all.iter().filter(|c| c.kind == kind).collect();
        if of.is_empty() {
            continue;
        }
        out.push(format!("{} ({}):", kind.as_str(), of.len()));
        for c in of.iter().take(40) {
            let s = caps.manager.stats(&c.id);
            let record = match s.success_rate() {
                Some(r) => format!(" · {:.0}% of {} · ~{}ms", r * 100.0, s.uses, s.average_latency_ms.unwrap_or(0)),
                None => String::new(),
            };
            let health = caps.manager.health(c);
            let health = if health == CapabilityHealth::Healthy { String::new() } else { format!(" · {}", health.as_str()) };
            out.push(format!("  {} [{} · {}]{record}{health}", c.id, c.risk.as_str(), caps.manager.rule(c).as_str()));
        }
        if of.len() > 40 {
            out.push(format!("  … {} more (/caps search)", of.len() - 40));
        }
    }
    out.push(String::new());
    out.push(COMMANDS.into());
    out.join("\n")
}

pub fn search(caps: &Caps, query: &str) -> Result<String, String> {
    if query.trim().is_empty() {
        return Err("usage: /caps search <what you need to do>".into());
    }
    let found = caps.run(caps.manager.discover(query, 10, &[]))?;
    if found.is_empty() {
        return Ok("nothing matches".into());
    }
    Ok(found
        .iter()
        .map(|f| {
            let s = &f.score;
            format!(
                "{} ({}) {:.2} — relevance {:.2} · reliability {:.2} · permission {:.2} · efficiency {:.2} · history {:.2}\n    {}",
                f.capability.id,
                f.capability.kind.as_str(),
                s.total,
                s.relevance,
                s.reliability,
                s.permission,
                s.efficiency,
                s.history,
                f.capability.description.chars().take(120).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn show(caps: &Caps, key: &str) -> Result<String, String> {
    let c = caps.manager.get(key.trim()).ok_or_else(|| format!("no capability {key:?}"))?;
    let s = caps.manager.stats(&c.id);
    let mut out = vec![
        format!("{} ({}, from {}) — {}", c.id, c.kind.as_str(), c.source, c.description),
        format!(
            "risk {} · policy {} · health {} · permissions {}",
            c.risk.as_str(),
            caps.manager.rule(&c).as_str(),
            caps.manager.health(&c).as_str(),
            if c.permissions.is_empty() { "—".into() } else { c.permissions.join(", ") }
        ),
        format!(
            "used {} times · success {} · avg {} · reliability {:.2}{}",
            s.uses,
            s.success_rate().map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
            s.average_latency_ms.map_or("—".into(), |ms| format!("{ms}ms")),
            s.reliability(),
            s.common_error.as_ref().map_or(String::new(), |e| format!(" · most common error: {e}"))
        ),
    ];
    for r in &c.metadata.requirements {
        out.push(format!("needs {}{}", r.entity, r.resolution_capability.as_ref().map_or(String::new(), |c| format!(", found with {c}"))));
    }
    if let Some(v) = &c.metadata.verification {
        out.push(format!("verified with {} {} — {}", v.capability, v.arguments, if v.success_expression.is_empty() { "succeeds" } else { &v.success_expression }));
    }
    if c.kind.callable() {
        out.push(format!("parameters: {}", c.input_schema));
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Mem;
    use lyra_memory::{MemoryManager, Settings};

    fn caps() -> (tokio::runtime::Runtime, Caps) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("lyra-caps-test-{}", lyra_memory::Uuid::new_v4()));
        let memory = rt.block_on(MemoryManager::open_lance(&dir.join("memory"), "memories", Settings::default())).unwrap();
        let mem = Mem::new(memory, rt.handle().clone(), "test".into(), Some("api".into()));
        let manager = rt.block_on(CapabilityManager::open(&dir.join("caps"), Default::default())).unwrap();
        let mut caps = Caps::new(manager, rt.handle().clone(), Vec::new(), Vec::new());
        caps.tools = Some(Arc::new(Tools::new(Arc::new(mem))));
        let agents = crate::agents::Agents::open(&dir.join("agents"), rt.handle().clone(), Default::default()).unwrap();
        caps.set_agents(Arc::new(agents));
        caps.refresh();
        (rt, caps)
    }

    fn call(caps: &Caps, name: &str, args: Value) -> Value {
        serde_json::from_str(&caps.invoke(name, &args.to_string(), CallContext::new(None, ""), false, true)).unwrap()
    }

    /// A pretend `lyra node`: asks about `touch`, runs what it's told.
    struct Desktop(std::sync::Mutex<Vec<Value>>);

    impl Remote for Desktop {
        fn machines(&self) -> Vec<(String, bool)> {
            vec![("desktop".into(), true), ("laptop".into(), false)]
        }

        fn call(&self, machine: &str, req: Value, _: std::time::Duration) -> Result<Value, String> {
            if machine != "desktop" {
                return Err(format!("{machine} isn't connected"));
            }
            self.0.lock().unwrap().push(req.clone());
            Ok(match req["type"].as_str() {
                Some("check") => json!({ "check": "ask", "what": "run a command on desktop", "detail": req["args"]["command"], "why": "runs touch", "dangerous": false }),
                _ => json!({ "ran": req["args"]["command"], "approved": req["approved"] }),
            })
        }
    }

    #[test]
    fn system_calls_can_go_to_another_machine() {
        let (_rt, mut caps) = caps();
        caps.system = Some(lyra_system::System::new(Default::default(), crate::config::expand_path));
        let desktop = Arc::new(Desktop(Default::default()));
        caps.set_remote(desktop.clone());
        caps.refresh();
        let shell = caps.manager.get("shell_run").unwrap();
        assert_eq!(shell.input_schema["properties"]["machine"]["enum"], json!(["server", "desktop", "laptop"]), "offline machines too");
        assert!(shell.input_schema["properties"]["machine"]["description"].as_str().unwrap().contains("laptop (offline now)"));
        assert!(caps.manager.get("ssh_run").unwrap().input_schema["properties"].get("machine").is_none());

        let args = r#"{"command":"touch notes.txt","machine":"desktop"}"#;
        let ask = caps.approval("shell_run", args).unwrap();
        assert_eq!((ask.what.as_str(), ask.detail.as_str()), ("run a command on desktop", "touch notes.txt"), "the machine's own answer");
        let agent = CallContext { agent: Some("operator"), ..CallContext::new(None, "") };
        let ran: Value = serde_json::from_str(&caps.invoke("shell_run", args, agent, true, true)).unwrap();
        assert_eq!(ran, json!({ "ran": "touch notes.txt", "approved": true }));
        let sent = desktop.0.lock().unwrap().clone();
        assert_eq!((sent[0]["type"].as_str(), sent[1]["type"].as_str()), (Some("check"), Some("call")));

        let offline: Value = serde_json::from_str(&caps.invoke("shell_run", r#"{"command":"ls","machine":"laptop"}"#, agent, false, true)).unwrap();
        assert!(offline["error"].as_str().unwrap().contains("isn't connected"));
        // Without a machine (or "server") it stays here, under this machine's rules.
        assert!(caps.approval("shell_run", r#"{"command":"ls","machine":"server"}"#).is_none());
        let direct = caps.invoke("shell_run", args, CallContext::new(None, ""), true, true);
        assert!(direct.contains("only for agents"), "the main agent still can't, wherever it points");
        // A member's agent can't either, approved or not.
        let member = CallContext { agent: Some("operator"), member: true, ..CallContext::new(None, "") };
        assert!(caps.invoke("shell_run", args, member, true, true).contains("for admins"));
    }

    /// One person's open page lending a folder; it says whose request reached it.
    struct Pages;

    impl Remote for Pages {
        fn machines(&self) -> Vec<(String, bool)> {
            Vec::new()
        }
        fn call(&self, machine: &str, _: Value, _: std::time::Duration) -> Result<Value, String> {
            Err(format!("{machine} isn't connected"))
        }
        fn folders(&self, user: &str) -> Vec<(String, lyra_web::Folder)> {
            if user != "dana" {
                return Vec::new();
            }
            let f = |name: &str, trusted: bool| ("Dana's PC".to_string(), lyra_web::Folder { name: name.into(), writable: true, allowed: true, trusted });
            vec![f("Firewall", false), f("Scratch", true)]
        }
        fn call_folder(&self, user: &str, folder: &str, req: Value, _: std::time::Duration) -> Result<Value, String> {
            Ok(json!({ "user": user, "folder": folder, "op": req["op"], "path": req["path"] }))
        }
    }

    #[test]
    fn members_reach_their_own_project_folders_and_writes_wait() {
        let (_rt, mut caps) = caps();
        caps.system = Some(lyra_system::System::new(Default::default(), crate::config::expand_path));
        caps.set_remote(Arc::new(Pages));
        caps.refresh();
        let member = CallContext { member: true, ..CallContext::new(None, "") };
        let listed: Value = crate::acting::run("dana", || serde_json::from_str(&caps.invoke("project_folders", "{}", member, false, true)).unwrap());
        assert_eq!(listed["folders"][0]["folder"], "Firewall");
        // Asked as the person this turn is for, never anyone else.
        let read: Value = crate::acting::run("dana", || serde_json::from_str(&caps.invoke("project_read", r#"{"folder":"Firewall","path":"docs/../plan.md"}"#, member, false, true)).unwrap());
        assert!(read["error"].as_str().unwrap_or("").contains("outside the folder"), "{read}");
        let listed: Value = crate::acting::run("dana", || serde_json::from_str(&caps.invoke("project_list", r#"{"folder":"Firewall","path":"docs"}"#, member, false, true)).unwrap());
        assert_eq!((listed["user"].as_str(), listed["path"].as_str()), (Some("dana"), Some("docs")));
        // A write waits for their yes.
        let w = r#"{"folder":"Firewall","path":"notes.md","content":"hi"}"#;
        assert!(caps.approval("project_write", w).unwrap().what.contains("notes.md in Firewall"));
        assert!(crate::acting::run("dana", || caps.invoke("project_write", w, member, false, true)).contains("approval"));
        // A folder they trust: no asking (for them; anyone else is still asked).
        let t = r#"{"folder":"Scratch","path":"notes.md","content":"hi"}"#;
        assert!(crate::acting::run("dana", || caps.approval("project_write", t)).is_none());
        assert!(crate::acting::run("owner", || caps.approval("project_write", t)).is_some());
        let wrote: Value = crate::acting::run("dana", || serde_json::from_str(&caps.invoke("project_write", t, member, false, true)).unwrap());
        assert_eq!(wrote["op"], "write");
        // Still no shell for members.
        assert!(caps.invoke("shell_run", r#"{"command":"ls"}"#, CallContext { agent: Some("operator"), member: true, ..CallContext::new(None, "") }, true, true).contains("for admins"));
    }

    #[test]
    fn one_command_on_several_machines_asks_once_and_answers_each() {
        let (_rt, mut caps) = caps();
        caps.system = Some(lyra_system::System::new(Default::default(), crate::config::expand_path));
        caps.groups = [("web".to_string(), vec!["desktop".to_string(), "laptop".to_string()])].into();
        let desktop = Arc::new(Desktop(Default::default()));
        caps.set_remote(desktop.clone());
        caps.refresh();
        assert_eq!(caps.fleet_targets("@all").unwrap(), vec!["server", "desktop"], "the server and the online machines");
        assert_eq!(caps.fleet_targets("web").unwrap(), vec!["desktop", "laptop"]);
        assert_eq!(caps.fleet_targets("Desktop, server").unwrap(), vec!["desktop", "server"]);
        assert!(caps.fleet_targets("nas").is_err());
        assert!(caps.manager.get("fleet_run").unwrap().input_schema["properties"]["machines"]["description"].as_str().unwrap().contains("web (desktop, laptop)"));

        // Only the machine that asks is in the approval; the server runs `echo` freely.
        let args = r#"{"command":"echo hi","machines":"all"}"#;
        let ask = caps.approval("fleet_run", args).unwrap();
        assert_eq!((ask.what.as_str(), ask.detail.as_str()), ("run a command on desktop", "echo hi"));
        let agent = CallContext { agent: Some("operator"), ..CallContext::new(None, "") };
        let ran: Value = serde_json::from_str(&caps.invoke("fleet_run", args, agent, true, true)).unwrap();
        assert_eq!((ran["machines"].as_u64(), ran["ok"].as_u64()), (Some(2), Some(2)), "{ran}");
        let server = ran["results"].as_array().unwrap().iter().find(|r| r["machine"] == "server").unwrap();
        assert_eq!(server["stdout"].as_str().map(str::trim), Some("hi"));
        let lines = crate::ui::fleet_lines(&ran.to_string()).unwrap();
        assert_eq!(lines.len(), 3, "a summary and a line per machine");

        // An offline member is reported, not skipped silently.
        let web: Value = serde_json::from_str(&caps.invoke("fleet_run", r#"{"command":"echo hi","machines":"web"}"#, agent, true, true)).unwrap();
        assert_eq!(web["failed"], 1);
        assert!(caps.invoke("fleet_run", args, CallContext::new(None, ""), true, true).contains("only for agents"));
    }

    #[test]
    fn calls_go_through_policy_and_writes_are_verified() {
        let (_rt, caps) = caps();
        let saved = call(&caps, "memory_remember", json!({ "content": "The API uses port 8000.", "scope": "project:api" }));
        assert_eq!(saved["verified"], true, "{saved}");
        let id = saved["id"].as_str().unwrap().to_string();

        let refused = call(&caps, "memory_forget", json!({ "id": id }));
        assert!(refused["error"].as_str().unwrap().contains("approval"), "{refused}");
        caps.manager.allow("memory_forget").unwrap();
        let forgot = call(&caps, "memory_forget", json!({ "id": id }));
        assert_eq!(forgot["verified"], true, "{forgot}");

        let stats = caps.manager.stats("memory_remember");
        assert_eq!((stats.uses, stats.successes), (1, 1));
        assert!(caps.manager.stats("memory_inspect").uses >= 2, "verification calls are tracked too");
        let info = caps.tool_info(&caps.manager.get("memory_correct").unwrap());
        assert!(info.notes.contains("needs memory id (from memory_recall)"), "{}", info.notes);
        assert!(info.verification.is_some());
        assert!(call(&caps, "agent.researcher", json!({}))["error"].as_str().unwrap().contains("subagent"));
    }
}
