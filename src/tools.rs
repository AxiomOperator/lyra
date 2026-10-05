//! Tools the model can call: the memory operations and working memory, all
//! going through the `MemoryManager` (the agent never touches the store).
//!
//! Tool names use `_` (`memory_remember`) because OpenAI-style function names
//! can't contain dots.

use std::sync::{Arc, RwLock};

use chrono::{Duration, Utc};
use lyra_memory::{MemorySource, NewMemory, Provenance, Remembered, Uuid};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mem::{Mem, parse_kind};
use lyra_evolution::CompositeTool;
use lyra_evolution::composite::Available;

/// Appended to the system prompt when memory is enabled.
pub const MEMORY_PROMPT: &str = "\
# Memory

You have a persistent memory that survives between conversations. Memories relevant to \
the current message are added below automatically; use memory_recall to look for more.

- When the user shares a lasting fact, preference or decision, or asks you to remember \
something, call memory_remember: one self-contained fact per memory. Say source \"user\" \
when the user stated it outright and \"agent\" when you're inferring it.
- If a memory has a mistake (a typo, a missing detail), memory_correct it.
- If something has changed over time (we used X, now we use Y), memory_supersede the old \
memory so its history is kept.
- memory_archive what's no longer relevant; memory_forget what shouldn't be remembered.
- Never store passwords, tokens, keys or other secrets; they will be refused.
- Scopes: \"user\" (about the user), \"agent\" (how you should work), \"project:<name>\".
- Keep short-term notes for the current task (goal, plan, values) with working_memory; \
they aren't saved.";

/// The run and tool call a tool is being called for (provenance).
#[derive(Clone, Copy)]
pub struct CallContext<'a> {
    pub run: Option<Uuid>,
    pub call_id: &'a str,
    /// Scopes the caller may write (`prefix:*` patterns); `None` means any
    /// allowed scope. Subagents are limited by their memory policy (M16, A10).
    pub write_scopes: Option<&'a [&'a str]>,
    /// Scopes the caller may read; `None` means every allowed scope.
    pub read_scopes: Option<&'a [&'a str]>,
    /// The subagent making the call; `None` is the main agent (or a plan's
    /// own step). System access is only for agents.
    pub agent: Option<&'a str>,
}

/// `scope` matches one of the patterns (`user`, `project:*`, `*`).
fn in_scopes(patterns: &[&str], scope: &str) -> bool {
    patterns.iter().any(|p| p.strip_suffix('*').map_or(*p == scope, |prefix| scope.starts_with(prefix)))
}

impl CallContext<'_> {
    pub fn new(run: Option<Uuid>, call_id: &str) -> CallContext<'_> {
        CallContext { run, call_id, write_scopes: None, read_scopes: None, agent: None }
    }

    fn check_write(&self, scope: &str) -> Result<(), String> {
        match self.write_scopes {
            Some(allowed) if !in_scopes(allowed, scope) => {
                Err(format!("this agent may not write to scope {scope:?} (only {})", allowed.join(", ")))
            }
            _ => Ok(()),
        }
    }

    fn can_read(&self, scope: &str) -> bool {
        self.read_scopes.is_none_or(|allowed| in_scopes(allowed, scope))
    }
}

pub struct Tools {
    pub mem: Arc<Mem>,
    /// Evolved tools built from the ones below (`~/.lyra/tools`).
    composites: RwLock<Vec<CompositeTool>>,
}

/// Tools that destroy something: only run as approved plan steps, never
/// inside a composite.
pub fn is_destructive(name: &str) -> bool {
    name == "memory_forget"
}

/// Tools that only read.
pub fn is_read_only(name: &str) -> bool {
    matches!(name, "memory_recall" | "memory_list" | "memory_inspect")
}

impl Tools {
    pub fn new(mem: Arc<Mem>) -> Self {
        Self { mem, composites: RwLock::new(Vec::new()) }
    }

    /// The built-in tools as `(name, destructive)`.
    pub fn base_tools(&self) -> Vec<(String, bool)> {
        self.base_definitions()
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["function"]["name"].as_str())
            .map(|n| (n.to_string(), is_destructive(n)))
            .collect()
    }

    /// Replace the composite tools, keeping only valid ones; returns why the rest were dropped.
    pub fn set_composites(&self, composites: Vec<CompositeTool>) -> Vec<String> {
        let base = self.base_tools();
        let available = Available { tools: &base };
        let mut notes = Vec::new();
        let valid = composites
            .into_iter()
            .filter(|c| match c.validate(&available) {
                Ok(()) => true,
                Err(e) => {
                    notes.push(format!("tool {} not loaded: {e}", c.name));
                    false
                }
            })
            .collect();
        *self.composites.write().unwrap_or_else(|e| e.into_inner()) = valid;
        notes
    }

    pub fn composite_names(&self) -> Vec<String> {
        self.composites.read().unwrap_or_else(|e| e.into_inner()).iter().map(|c| c.name.clone()).collect()
    }

    fn composite(&self, name: &str) -> Option<CompositeTool> {
        self.composites.read().unwrap_or_else(|e| e.into_inner()).iter().find(|c| c.name == name).cloned()
    }

    /// The `tools` array for a chat completions request: the built-in tools
    /// and the composite ones.
    pub fn definitions(&self) -> Value {
        let mut all = self.base_definitions();
        if let Some(list) = all.as_array_mut() {
            list.extend(self.composites.read().unwrap_or_else(|e| e.into_inner()).iter().map(CompositeTool::definition));
        }
        all
    }

    fn base_definitions(&self) -> Value {
        let default_scope = &self.mem.manager.settings().default_scope;
        let scope = |default: &str| {
            json!({ "type": "string", "description": format!("\"user\", \"agent\" or \"project:<name>\". Default: {default}.") })
        };
        let id = json!({ "type": "string", "description": "Memory id (the 8 characters shown in brackets is enough)." });
        let reason = json!({ "type": "string", "description": "Why, in a few words." });
        json!([
            function(
                "memory_remember",
                "Save one fact to persistent memory.",
                json!({
                    "content": { "type": "string", "description": "The fact, self-contained." },
                    "scope": scope(default_scope),
                    "kind": { "type": "string", "enum": ["semantic", "episodic", "working"], "description": "semantic: a fact (default); episodic: something that happened; working: a short-lived note." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": { "type": "string", "enum": ["user", "conversation", "tool", "document", "agent"], "description": "user: stated by the user; agent: your inference. Default: conversation." },
                    "importance": { "type": "number", "description": "0.1 minor detail … 0.9 critical. Default 0.5." },
                    "expires_in_days": { "type": "number", "description": "For facts that stop being true (optional)." },
                }),
                &["content"],
            ),
            function(
                "memory_recall",
                "Search persistent memory by meaning and keywords, best matches first.",
                json!({
                    "query": { "type": "string" },
                    "scope": scope("all scopes"),
                    "limit": { "type": "integer", "description": "Max results (default 5, max 20)." },
                    "include_archived": { "type": "boolean" },
                }),
                &["query"],
            ),
            function(
                "memory_list",
                "List the most recent memories.",
                json!({
                    "scope": scope("all scopes"),
                    "limit": { "type": "integer", "description": "Max results (default 10, max 50)." },
                }),
                &[],
            ),
            function(
                "memory_correct",
                "Fix a mistake in a memory (same fact, better wording or detail). Keeps a version history.",
                json!({ "id": id, "content": { "type": "string", "description": "The corrected memory." }, "reason": reason }),
                &["id", "content"],
            ),
            function(
                "memory_supersede",
                "Replace a memory that is no longer true with a new one; the old one is kept as history.",
                json!({
                    "id": id,
                    "content": { "type": "string", "description": "What is true now." },
                    "reason": reason,
                    "source": { "type": "string", "enum": ["user", "conversation", "tool", "document", "agent"], "description": "Where the new fact comes from. Default: conversation." },
                }),
                &["id", "content"],
            ),
            function(
                "memory_inspect",
                "A memory's full record: versions, relationships (supersedes, contradicts, …), usage and history.",
                json!({ "id": id }),
                &["id"],
            ),
            function(
                "memory_archive",
                "Keep a memory for reference but stop using it.",
                json!({ "id": id, "reason": reason }),
                &["id"],
            ),
            function(
                "memory_forget",
                "Stop remembering something (it can be restored by the user).",
                json!({ "id": id, "reason": reason }),
                &["id"],
            ),
            function(
                "working_memory",
                "Short-term notes for the current task: the goal, a plan, named values. Not saved.",
                json!({
                    "goal": { "type": "string" },
                    "plan": { "type": "array", "items": { "type": "string" } },
                    "set": { "type": "object", "description": "Values to keep, e.g. {\"server\": \"db1\"}.", "additionalProperties": { "type": "string" } },
                    "remove": { "type": "array", "items": { "type": "string" } },
                    "clear": { "type": "boolean" },
                }),
                &[],
            ),
        ])
    }

    /// Run a tool call and return its result as JSON text for the model.
    /// Failures are reported to the model as `{"error": ...}` rather than ending the turn.
    pub fn run(&self, name: &str, arguments: &str, ctx: CallContext) -> String {
        let result = match name {
            "memory_remember" => self.remember(arguments, ctx),
            "memory_recall" => self.recall(arguments, ctx),
            "memory_list" => self.list(arguments, ctx),
            "memory_inspect" => self.inspect(arguments, ctx),
            "memory_correct" => self.correct(arguments, ctx),
            "memory_supersede" => self.supersede(arguments, ctx),
            "memory_archive" => self.set_status(arguments, ctx, true),
            "memory_forget" => self.set_status(arguments, ctx, false),
            "working_memory" => self.working(arguments),
            _ => match self.composite(name) {
                Some(c) => self.run_composite(&c, arguments, ctx),
                None => Err(format!("unknown tool {name}")),
            },
        };
        let result = result.unwrap_or_else(|e| json!({ "error": e }));
        let text = result.to_string();
        self.mem.working().note_tool(name, &text);
        text
    }

    /// A composite runs its steps in order and stops at the first error.
    fn run_composite(&self, c: &CompositeTool, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        let inputs: Value = serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }).map_err(|e| format!("bad arguments: {e}"))?;
        let mut steps = Vec::new();
        for (tool, args) in c.calls(&inputs)? {
            // Composites only use built-in, non-destructive tools (checked when loaded).
            if is_destructive(&tool) || self.composite(&tool).is_some() {
                return Err(format!("{tool} can't be used inside a composite"));
            }
            let text = self.run(&tool, &args.to_string(), ctx);
            let result: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            let failed = result.get("error").is_some();
            steps.push(json!({ "tool": tool, "result": result }));
            if failed {
                return Ok(json!({ "error": format!("step {tool} failed"), "steps": steps }));
            }
        }
        Ok(json!({ "steps": steps }))
    }

    fn remember(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            content: String,
            scope: Option<String>,
            kind: Option<String>,
            #[serde(default)]
            tags: Vec<String>,
            source: Option<String>,
            importance: Option<f32>,
            expires_in_days: Option<f32>,
        }
        let a: Args = parse(arguments)?;
        let source = a.source.as_deref().unwrap_or("conversation").parse().unwrap_or(MemorySource::Conversation);
        let new = NewMemory {
            scope: a.scope.unwrap_or_else(|| self.mem.manager.settings().default_scope.clone()),
            kind: parse_kind(a.kind.as_deref())?,
            tags: a.tags,
            provenance: provenance(ctx),
            importance: a.importance,
            expires_at: a.expires_in_days.filter(|d| *d > 0.0).map(|d| Utc::now() + Duration::minutes((d * 1440.0) as i64)),
            ..NewMemory::fact("", &a.content, source)
        };
        ctx.check_write(&new.scope)?;
        let r = self.mem.run(self.mem.manager.remember(new))?;
        let (what, m) = match &r {
            Remembered::Created(m) => ("remembered", m),
            Remembered::Reconfirmed(m) => ("already known; reconfirmed", m),
        };
        Ok(json!({ "result": what, "id": m.short_id(), "scope": m.scope, "confidence": round2(m.confidence) }))
    }

    fn recall(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            scope: Option<String>,
            limit: Option<usize>,
            #[serde(default)]
            include_archived: bool,
        }
        let a: Args = parse(arguments)?;
        let scope = a.scope.filter(|s| s != "*" && !s.is_empty());
        if let Some(s) = &scope
            && !ctx.can_read(s)
        {
            return Err(format!("this agent may not read scope {s:?}"));
        }
        let mut found = self.mem.recall(scope.as_deref(), &a.query, a.limit.unwrap_or(5).clamp(1, 20), a.include_archived)?;
        found.retain(|r| ctx.can_read(&r.memory.scope));
        if let Some(run) = ctx.run {
            let ids: Vec<Uuid> = found.iter().map(|r| r.memory.id).collect();
            let _ = self.mem.run(self.mem.manager.record_recall(run, &ids));
        }
        Ok(Value::Array(found.iter().map(|r| memory_json(&r.memory)).collect()))
    }

    fn inspect(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
        }
        let a: Args = parse(arguments)?;
        let m = self.mem.run(self.mem.manager.find(&a.id))?;
        if !ctx.can_read(&m.scope) {
            return Err(format!("this agent may not read scope {:?}", m.scope));
        }
        Ok(json!({ "memory": self.mem.inspect_text(&m)? }))
    }

    fn list(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            scope: Option<String>,
            limit: Option<usize>,
        }
        let a: Args = parse(arguments)?;
        let filter = lyra_memory::Filter {
            scope: a.scope.filter(|s| s != "*" && !s.is_empty()),
            ..lyra_memory::Filter::active()
        };
        let mut listed = self.mem.run(self.mem.manager.list(&filter, a.limit.unwrap_or(10).clamp(1, 50)))?;
        listed.retain(|m| ctx.can_read(&m.scope));
        Ok(Value::Array(listed.iter().map(memory_json).collect()))
    }

    fn correct(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            content: String,
            reason: Option<String>,
        }
        let a: Args = parse(arguments)?;
        let m = self.mem.run(self.mem.manager.find(&a.id))?;
        ctx.check_write(&m.scope)?;
        let reason = a.reason.unwrap_or_else(|| "corrected".into());
        let v = self.mem.run(self.mem.manager.correct(m.id, &a.content, &reason, ctx.run))?;
        Ok(json!({ "result": "corrected", "id": m.short_id(), "version": v }))
    }

    fn supersede(&self, arguments: &str, ctx: CallContext) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            content: String,
            reason: Option<String>,
            source: Option<String>,
        }
        let a: Args = parse(arguments)?;
        let old = self.mem.run(self.mem.manager.find(&a.id))?;
        ctx.check_write(&old.scope)?;
        let source = a.source.as_deref().unwrap_or("conversation").parse().unwrap_or(MemorySource::Conversation);
        let new = NewMemory {
            kind: old.kind,
            tags: old.tags.clone(),
            provenance: provenance(ctx),
            importance: Some(old.importance),
            ..NewMemory::fact(&old.scope, &a.content, source)
        };
        let reason = a.reason.unwrap_or_else(|| "no longer true".into());
        let m = self.mem.run(self.mem.manager.supersede(old.id, new, &reason))?;
        Ok(json!({ "result": "superseded", "old": old.short_id(), "new": m.short_id() }))
    }

    fn set_status(&self, arguments: &str, ctx: CallContext, archive: bool) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
            reason: Option<String>,
        }
        let a: Args = parse(arguments)?;
        let m = self.mem.run(self.mem.manager.find(&a.id))?;
        ctx.check_write(&m.scope)?;
        let reason = a.reason.unwrap_or_else(|| "asked by the assistant".into());
        let m = if archive {
            self.mem.run(self.mem.manager.archive(m.id, &reason, ctx.run))?
        } else {
            self.mem.run(self.mem.manager.forget(m.id, &reason, ctx.run))?
        };
        Ok(json!({ "result": m.status.as_str(), "id": m.short_id() }))
    }

    fn working(&self, arguments: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            goal: Option<String>,
            plan: Option<Vec<String>>,
            #[serde(default)]
            set: Map<String, Value>,
            #[serde(default)]
            remove: Vec<String>,
            #[serde(default)]
            clear: bool,
        }
        let a: Args = parse(arguments)?;
        let mut w = self.mem.working();
        if a.clear {
            w.clear();
        }
        if let Some(goal) = a.goal {
            w.goal = (!goal.trim().is_empty()).then(|| goal.trim().to_string());
        }
        if let Some(plan) = a.plan {
            w.plan = plan;
        }
        for (k, v) in a.set {
            let v = match v {
                Value::String(s) => s,
                other => other.to_string(),
            };
            if lyra_memory::safety::scan(&v).is_some() {
                return Err(format!("not keeping {k}: it looks like a secret"));
            }
            w.values.insert(k, v);
        }
        for k in a.remove {
            w.values.remove(&k);
        }
        Ok(json!({ "goal": w.goal, "plan": w.plan, "values": w.values }))
    }
}

/// f32s like 0.9 print as 0.8999999761581421 in JSON; the model doesn't need that.
fn round2(x: f32) -> f64 {
    (x as f64 * 100.0).round() / 100.0
}

fn provenance(ctx: CallContext) -> Provenance {
    Provenance { run_id: ctx.run, tool_call_id: (!ctx.call_id.is_empty()).then(|| ctx.call_id.to_string()), conversation_id: None }
}

fn function(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": { "type": "object", "properties": properties, "required": required },
        },
    })
}

/// Tool arguments; models occasionally send an empty string for "no arguments".
fn parse<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, String> {
    let arguments = if arguments.trim().is_empty() { "{}" } else { arguments };
    serde_json::from_str(arguments).map_err(|e| format!("bad arguments: {e}"))
}

/// What the model sees for each memory.
fn memory_json(m: &lyra_memory::Memory) -> Value {
    json!({
        "id": m.short_id(),
        "scope": m.scope,
        "kind": m.kind.as_str(),
        "content": m.content,
        "tags": m.tags,
        "source": m.source.as_str(),
        "confidence": round2(m.confidence),
        "status": m.status.as_str(),
        "updated": m.updated_at.format("%Y-%m-%d").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_memory::{MemoryManager, Settings};

    fn tools() -> (tokio::runtime::Runtime, Tools) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("lyra-tools-test-{}", Uuid::new_v4()));
        let manager = rt.block_on(MemoryManager::open_lance(&dir, "memories", Settings::default())).unwrap();
        let mem = Mem::new(manager, rt.handle().clone(), "test".into(), Some("api".into()));
        (rt, Tools::new(Arc::new(mem)))
    }

    #[test]
    fn helper_agents_only_write_their_scopes() {
        let (_rt, t) = tools();
        let scopes: &[&str] = &["agent", "project:*"];
        let ctx = CallContext { run: None, call_id: "", write_scopes: Some(scopes), read_scopes: None, agent: Some("archivist") };
        let denied: Value = serde_json::from_str(&t.run("memory_remember", r#"{"content":"The user likes tea.","scope":"user"}"#, ctx)).unwrap();
        assert!(denied["error"].as_str().unwrap().contains("may not write"));
        let ok: Value = serde_json::from_str(&t.run("memory_remember", r#"{"content":"Builds use cargo.","scope":"project:api"}"#, ctx)).unwrap();
        assert_eq!(ok["result"], "remembered");
        let id = ok["id"].as_str().unwrap();
        let inspected = call(&t, "memory_inspect", &format!(r#"{{"id":"{id}"}}"#));
        assert!(inspected["memory"].as_str().unwrap().contains("Builds use cargo."));
    }

    fn call(t: &Tools, name: &str, args: &str) -> Value {
        let ctx = CallContext::new(Some(Uuid::new_v4()), "call_1");
        serde_json::from_str(&t.run(name, args, ctx)).unwrap()
    }

    #[test]
    fn remember_recall_correct_supersede_forget() {
        let (_rt, t) = tools();
        let saved = call(&t, "memory_remember", r#"{"content":"The API uses port 8000.","scope":"project:api","source":"user"}"#);
        let id = saved["id"].as_str().unwrap().to_string();
        assert_eq!(saved["confidence"], 1.0);

        let again = call(&t, "memory_remember", r#"{"content":"The API uses port 8000","scope":"project:api"}"#);
        assert_eq!(again["result"], "already known; reconfirmed");

        let found = call(&t, "memory_recall", r#"{"query":"which port does the api use"}"#);
        assert_eq!(found[0]["content"], "The API uses port 8000.");

        let fixed = call(&t, "memory_correct", &format!(r#"{{"id":"{id}","content":"The API uses port 8001."}}"#));
        assert_eq!(fixed["version"], 2);

        let moved = call(&t, "memory_supersede", &format!(r#"{{"id":"{id}","content":"The API uses port 8080.","reason":"moved"}}"#));
        let new_id = moved["new"].as_str().unwrap().to_string();
        let found = call(&t, "memory_recall", r#"{"query":"api port"}"#);
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["content"], "The API uses port 8080.");

        let gone = call(&t, "memory_forget", &format!(r#"{{"id":"{new_id}"}}"#));
        assert_eq!(gone["result"], "deleted");
        assert!(call(&t, "memory_list", "").as_array().unwrap().is_empty());
    }

    #[test]
    fn errors_are_returned_to_the_model() {
        let (_rt, t) = tools();
        for (name, args) in [
            ("memory_remember", "not json"),
            ("memory_remember", r#"{"content":"my password is hunter2"}"#),
            ("memory_remember", r#"{"content":"x","kind":"dream"}"#),
            ("memory_forget", r#"{"id":"nope"}"#),
            ("memory_correct", r#"{"id":"12345678","content":"x"}"#),
            ("working_memory", r#"{"set":{"token":"ghp_aBcDeFgHiJkLmNoPqRsTuVwX"}}"#),
            ("memory_fly", "{}"),
        ] {
            let result = call(&t, name, args);
            assert!(result["error"].is_string(), "{name} {args}: {result}");
        }
    }

    #[test]
    fn working_memory_updates_and_never_persists() {
        let (_rt, t) = tools();
        let w = call(&t, "working_memory", r#"{"goal":"Fix PgBouncer","plan":["check perms","restart"],"set":{"server":"db1","port":6432}}"#);
        assert_eq!(w["goal"], "Fix PgBouncer");
        assert_eq!(w["values"]["port"], "6432");
        let w = call(&t, "working_memory", r#"{"remove":["port"]}"#);
        assert!(w["values"].get("port").is_none());
        assert!(t.mem.working().render().unwrap().contains("Goal: Fix PgBouncer"));
        assert!(call(&t, "memory_list", "").as_array().unwrap().is_empty(), "not persisted");
    }

    #[test]
    fn definitions_name_all_tools() {
        let (_rt, t) = tools();
        let names: Vec<String> = t
            .definitions()
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["memory_remember", "memory_recall", "memory_list", "memory_correct", "memory_supersede", "memory_inspect", "memory_archive", "memory_forget", "working_memory"]
        );
    }
}
