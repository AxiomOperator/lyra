//! Declarative tools (E5): new capabilities built from existing tools, as
//! data. No generated code runs; a composite just calls its steps in order
//! with arguments filled from its inputs, and returns their combined output.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `~/.lyra/tools/<name>.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositeTool {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<Input>,
    pub steps: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub name: String,
    pub description: String,
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    /// Arguments; `"{{input}}"` is replaced by that input's value.
    #[serde(default = "empty")]
    pub arguments: Value,
}

fn empty() -> Value {
    Value::Object(Map::new())
}

/// What a composite may be built from, as the runtime knows its tools.
pub struct Available<'a> {
    /// `(name, destructive)` for every tool.
    pub tools: &'a [(String, bool)],
}

impl CompositeTool {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("tool definition: {e}"))
    }

    pub fn render(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Composites call existing, non-destructive, non-composite tools only,
    /// and every placeholder must be a declared input.
    pub fn validate(&self, available: &Available) -> Result<(), String> {
        if !crate::workflow::valid_name(&self.name.replace('_', "-")) || self.name.contains('-') {
            return Err(format!("bad tool name {:?}: use lowercase letters, digits and underscores", self.name));
        }
        if available.tools.iter().any(|(n, _)| n == &self.name) {
            return Err(format!("a tool named {} already exists", self.name));
        }
        if self.steps.len() < 2 || self.steps.len() > 8 {
            return Err("a composite tool combines 2–8 steps".into());
        }
        if self.description.trim().is_empty() {
            return Err("a composite tool needs a description".into());
        }
        let inputs: Vec<&str> = self.inputs.iter().map(|i| i.name.as_str()).collect();
        for s in &self.steps {
            match available.tools.iter().find(|(n, _)| n == &s.tool) {
                None => return Err(format!("step tool {} doesn't exist", s.tool)),
                Some((_, true)) => return Err(format!("{} is destructive; composites may not use it", s.tool)),
                Some(_) => {}
            }
            if !s.arguments.is_object() {
                return Err(format!("{}'s arguments must be an object", s.tool));
            }
            for p in placeholders(&s.arguments) {
                if !inputs.contains(&p.as_str()) {
                    return Err(format!("{} uses {{{{{p}}}}}, which isn't an input", s.tool));
                }
            }
        }
        Ok(())
    }

    /// A tool definition for the model (OpenAI function format).
    pub fn definition(&self) -> Value {
        let mut properties = Map::new();
        for i in &self.inputs {
            properties.insert(i.name.clone(), serde_json::json!({ "type": "string", "description": i.description }));
        }
        let required: Vec<&str> = self.inputs.iter().filter(|i| i.required).map(|i| i.name.as_str()).collect();
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": format!("{} (combines {})", self.description, self.steps.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>().join(", ")),
                "parameters": { "type": "object", "properties": properties, "required": required },
            },
        })
    }

    /// The calls to make for these inputs, with placeholders filled in.
    pub fn calls(&self, inputs: &Value) -> Result<Vec<(String, Value)>, String> {
        for i in self.inputs.iter().filter(|i| i.required) {
            if inputs.get(&i.name).is_none_or(Value::is_null) {
                return Err(format!("missing input {}", i.name));
            }
        }
        Ok(self.steps.iter().map(|s| (s.tool.clone(), fill(&s.arguments, inputs))).collect())
    }
}

/// The input names used as `{{name}}` in a value.
fn placeholders(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let text = v.to_string();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        out.push(after[..end].trim().to_string());
        rest = &after[end + 2..];
    }
    out
}

/// Replace `"{{name}}"` strings (whole or embedded) with input values.
fn fill(v: &Value, inputs: &Value) -> Value {
    match v {
        Value::String(s) => {
            let trimmed = s.trim();
            if let Some(name) = trimmed.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
                && !name.contains("{{")
            {
                return inputs.get(name.trim()).cloned().unwrap_or(Value::Null);
            }
            let mut out = s.clone();
            for name in placeholders(v) {
                let value = match inputs.get(&name) {
                    Some(Value::String(x)) => x.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                out = out.replace(&format!("{{{{{name}}}}}"), &value);
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|i| fill(i, inputs)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, x)| (k.clone(), fill(x, inputs))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DIAGNOSE: &str = r#"
name = "project_overview"
description = "Everything remembered about a project"

[[inputs]]
name = "project"
description = "project name"

[[steps]]
tool = "memory_recall"
arguments = { query = "{{project}} decisions", scope = "project:{{project}}" }

[[steps]]
tool = "memory_list"
arguments = { scope = "project:{{project}}" }
"#;

    fn available() -> Vec<(String, bool)> {
        vec![("memory_recall".into(), false), ("memory_list".into(), false), ("memory_forget".into(), true)]
    }

    #[test]
    fn validates_and_fills_in_inputs() {
        let t = CompositeTool::parse(DIAGNOSE).unwrap();
        let tools = available();
        t.validate(&Available { tools: &tools }).unwrap();
        let calls = t.calls(&json!({"project": "arcella"})).unwrap();
        assert_eq!(calls[0], ("memory_recall".to_string(), json!({"query": "arcella decisions", "scope": "project:arcella"})));
        assert!(t.calls(&json!({})).unwrap_err().contains("missing input"));
        assert_eq!(t.definition()["function"]["parameters"]["required"], json!(["project"]));
        assert_eq!(CompositeTool::parse(&t.render()).unwrap(), t);
    }

    #[test]
    fn rejects_unsafe_or_broken_composites() {
        let tools = available();
        let check = |text: &str| CompositeTool::parse(text).unwrap().validate(&Available { tools: &tools });
        assert!(check(&DIAGNOSE.replace("memory_list", "memory_forget")).unwrap_err().contains("destructive"));
        assert!(check(&DIAGNOSE.replace("memory_list", "shell")).unwrap_err().contains("doesn't exist"));
        assert!(check(&DIAGNOSE.replace("{{project}} decisions", "{{nope}}")).unwrap_err().contains("isn't an input"));
        assert!(check(&DIAGNOSE.replace("project_overview", "memory_recall")).unwrap_err().contains("already exists"));
    }
}
