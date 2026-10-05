//! Tools the model can call. V1: the four memory operations, backed by
//! `MemoryManager` (the agent never sees what store is underneath).
//!
//! Tool names use `_` (`memory_remember`) because OpenAI-style function names
//! can't contain dots.

use std::sync::Arc;

use lyra_memory::{ALL_SCOPES, Memory, MemoryManager, Uuid};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Handle;

/// Appended to the system prompt when memory is enabled.
pub const MEMORY_PROMPT: &str = "\
# Memory

You have a persistent memory that survives between conversations, through the \
memory_remember, memory_recall, memory_forget and memory_list tools.

- Before answering anything that may depend on earlier conversations, decisions or \
facts about the user, call memory_recall with a few keywords.
- When the user shares a lasting fact, preference or decision, or asks you to remember \
something, call memory_remember. Store one self-contained fact per memory, written so \
it makes sense on its own later.
- If a memory is wrong or outdated, memory_forget it (and remember the correction).
- Don't store secrets or passing small talk.
- Scopes: \"user\" for facts about the user, \"agent\" for notes on how to work, \
\"project:<name>\" for a specific project.";

/// What the memory panel shows.
pub struct MemorySnapshot {
    pub total: u64,
    /// Memories per scope, largest first.
    pub scopes: Vec<(String, u64)>,
    /// Most recent first.
    pub recent: Vec<Memory>,
}

pub struct Tools {
    memory: Arc<MemoryManager>,
    runtime: Handle,
    default_scope: String,
}

impl Tools {
    pub fn new(memory: Arc<MemoryManager>, runtime: Handle, default_scope: String) -> Self {
        Self { memory, runtime, default_scope }
    }

    /// Counts and recent memories for the UI.
    pub fn snapshot(&self, recent: usize) -> Result<MemorySnapshot, String> {
        self.runtime.block_on(async {
            let scopes = self.memory.scopes().await.map_err(|e| e.to_string())?;
            let recent = self.memory.list(ALL_SCOPES, recent).await.map_err(|e| e.to_string())?;
            let total = scopes.iter().map(|(_, n)| n).sum();
            Ok(MemorySnapshot { total, scopes, recent })
        })
    }

    /// The `tools` array for a chat completions request.
    pub fn definitions(&self) -> Value {
        let scope = |default: &str| {
            json!({
                "type": "string",
                "description": format!(
                    "\"user\", \"agent\", or \"project:<name>\". Default: {default}."
                ),
            })
        };
        json!([
            function(
                "memory_remember",
                "Save one fact to persistent memory.",
                json!({
                    "content": { "type": "string", "description": "The fact, self-contained." },
                    "scope": scope(&self.default_scope),
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": {
                        "type": "string",
                        "description": "conversation, user, tool, document, agent or system. Default: conversation.",
                    },
                }),
                &["content"],
            ),
            function(
                "memory_recall",
                "Keyword search of persistent memory, best matches first.",
                json!({
                    "query": { "type": "string", "description": "Keywords to search for." },
                    "scope": scope("all scopes"),
                    "limit": { "type": "integer", "description": "Max results (default 5, max 20)." },
                }),
                &["query"],
            ),
            function(
                "memory_forget",
                "Delete a memory by id.",
                json!({ "id": { "type": "string", "description": "Memory id from recall or list." } }),
                &["id"],
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
        ])
    }

    /// Run a tool call and return its result as JSON text for the model.
    /// Failures are reported to the model as `{"error": ...}` rather than ending the turn.
    pub fn run(&self, name: &str, arguments: &str) -> String {
        let result = match name {
            "memory_remember" => self.remember(arguments),
            "memory_recall" => self.recall(arguments),
            "memory_forget" => self.forget(arguments),
            "memory_list" => self.list(arguments),
            _ => Err(format!("unknown tool {name}")),
        };
        result.unwrap_or_else(|e| json!({ "error": e })).to_string()
    }

    fn remember(&self, arguments: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            content: String,
            scope: Option<String>,
            #[serde(default)]
            tags: Vec<String>,
            source: Option<String>,
        }
        let args: Args = parse(arguments)?;
        let scope = args.scope.unwrap_or_else(|| self.default_scope.clone());
        let source = args.source.unwrap_or_else(|| "conversation".into());
        let memory = self
            .runtime
            .block_on(self.memory.remember(&scope, &args.content, &args.tags, Some(&source)))
            .map_err(|e| e.to_string())?;
        Ok(json!({ "remembered": { "id": memory.id, "scope": memory.scope } }))
    }

    fn recall(&self, arguments: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            scope: Option<String>,
            limit: Option<usize>,
        }
        let args: Args = parse(arguments)?;
        let scope = args.scope.unwrap_or_else(|| ALL_SCOPES.into());
        let limit = args.limit.unwrap_or(5).clamp(1, 20);
        let found = self
            .runtime
            .block_on(self.memory.recall(&scope, &args.query, limit))
            .map_err(|e| e.to_string())?;
        Ok(memories(&found))
    }

    fn forget(&self, arguments: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            id: String,
        }
        let args: Args = parse(arguments)?;
        let id = Uuid::parse_str(args.id.trim()).map_err(|e| format!("bad id: {e}"))?;
        self.runtime.block_on(self.memory.forget(id)).map_err(|e| e.to_string())?;
        Ok(json!({ "forgotten": id }))
    }

    fn list(&self, arguments: &str) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Args {
            scope: Option<String>,
            limit: Option<usize>,
        }
        let args: Args = parse(arguments)?;
        let scope = args.scope.unwrap_or_else(|| ALL_SCOPES.into());
        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let listed = self
            .runtime
            .block_on(self.memory.list(&scope, limit))
            .map_err(|e| e.to_string())?;
        Ok(memories(&listed))
    }
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

/// What the model sees for each memory (timestamps trimmed to the date).
fn memories(list: &[Memory]) -> Value {
    Value::Array(
        list.iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "scope": m.scope,
                    "content": m.content,
                    "tags": m.tags,
                    "source": m.source,
                    "created": m.created_at.format("%Y-%m-%d").to_string(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_memory::SqliteStore;

    fn tools() -> (tokio::runtime::Runtime, Tools) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let memory = rt.block_on(SqliteStore::in_memory()).map(MemoryManager::new).unwrap();
        let tools = Tools::new(Arc::new(memory), rt.handle().clone(), "user".into());
        (rt, tools)
    }

    #[test]
    fn remember_recall_list_forget_round_trip() {
        let (_rt, tools) = tools();
        let saved: Value = serde_json::from_str(&tools.run(
            "memory_remember",
            r#"{"content":"The agent runtime will be written in Rust.","scope":"project:arcella","tags":["architecture","rust"]}"#,
        ))
        .unwrap();
        let id = saved["remembered"]["id"].as_str().unwrap().to_string();

        let found: Value = serde_json::from_str(&tools.run(
            "memory_recall",
            r#"{"query":"agent runtime language","scope":"project:arcella","limit":5}"#,
        ))
        .unwrap();
        assert_eq!(found[0]["content"], "The agent runtime will be written in Rust.");
        assert_eq!(found[0]["source"], "conversation");

        let listed: Value = serde_json::from_str(&tools.run("memory_list", "")).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);

        let forgotten: Value =
            serde_json::from_str(&tools.run("memory_forget", &format!(r#"{{"id":"{id}"}}"#))).unwrap();
        assert_eq!(forgotten["forgotten"], id.as_str());
        let listed: Value = serde_json::from_str(&tools.run("memory_list", "{}")).unwrap();
        assert!(listed.as_array().unwrap().is_empty());
    }

    #[test]
    fn remember_defaults_scope() {
        let (_rt, tools) = tools();
        let saved: Value =
            serde_json::from_str(&tools.run("memory_remember", r#"{"content":"Likes tea."}"#)).unwrap();
        assert_eq!(saved["remembered"]["scope"], "user");
    }

    #[test]
    fn errors_are_returned_to_the_model() {
        let (_rt, tools) = tools();
        for (name, args) in [
            ("memory_remember", "not json"),
            ("memory_forget", r#"{"id":"nope"}"#),
            ("memory_forget", &format!(r#"{{"id":"{}"}}"#, Uuid::new_v4())),
            ("memory_fly", "{}"),
        ] {
            let result: Value = serde_json::from_str(&tools.run(name, args)).unwrap();
            assert!(result["error"].is_string(), "{name} {args}: {result}");
        }
    }

    #[test]
    fn definitions_name_all_four_tools() {
        let (_rt, tools) = tools();
        let names: Vec<_> = tools
            .definitions()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["memory_remember", "memory_recall", "memory_forget", "memory_list"]);
    }
}
