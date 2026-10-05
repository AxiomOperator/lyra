//! Lyra's side of capability intelligence (docs/capabilities.md): builds the
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

/// The planner's risk scale.
pub fn plan_risk(r: RiskLevel) -> Risk {
    match r {
        RiskLevel::ReadOnly => Risk::ReadOnly,
        RiskLevel::LowWrite | RiskLevel::Write => Risk::Mutating,
        RiskLevel::Destructive | RiskLevel::Privileged => Risk::Destructive,
    }
}

impl Caps {
    /// Start the configured MCP servers and load the OpenAPI specs. Returns
    /// the providers and notes about the ones that failed.
    pub fn connect(openapi: &[OpenApiConfig], mcp: &[McpConfig], expand: impl Fn(&str) -> std::path::PathBuf) -> (Vec<OpenApiClient>, Vec<McpClient>, Vec<String>) {
        let mut notes = Vec::new();
        let mut specs = Vec::new();
        for c in openapi {
            let path = expand(&c.spec);
            match std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display())).and_then(|t| OpenApiClient::load(c.clone(), &t)) {
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
        Self { manager, rt, tools: None, learning: None, evolution: None, openapi, mcp }
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
        for (name, description, tools) in crate::plan::AGENTS {
            let risk = if tools.iter().all(|t| crate::plan::risk(t) == Risk::ReadOnly) { RiskLevel::ReadOnly } else { RiskLevel::LowWrite };
            let mut c = Capability::new(&format!("agent.{name}"), CapabilityKind::Subagent, description, risk);
            c.source = "agents".into();
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

    /// Callable capabilities usable now.
    fn callable(&self) -> Vec<Capability> {
        self.manager.usable().into_iter().filter(|c| c.kind.callable()).collect()
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
        let names = found.iter().filter(|f| f.capability.kind.callable()).map(|f| f.capability.name.clone()).collect();
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
        if self.manager.health(&c) == CapabilityHealth::Unavailable {
            return json!({ "error": format!("{} is unavailable right now", c.id) }).to_string();
        }
        let args: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).unwrap_or(json!({}));
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
        caps.refresh();
        (rt, caps)
    }

    fn call(caps: &Caps, name: &str, args: Value) -> Value {
        serde_json::from_str(&caps.invoke(name, &args.to_string(), CallContext::new(None, ""), false, true)).unwrap()
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
