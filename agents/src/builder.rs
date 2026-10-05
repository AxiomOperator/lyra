//! The agent creation wizard (A3, A4, A23, A24): structured questions fill
//! an [`AgentCreationDraft`] (saved, so an interrupted interview resumes),
//! the model turns it into instructions and routing examples, the new agent
//! is tried on a test task, and only then activated.

use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::templates;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationMode {
    /// From a template: only the customization questions.
    Template,
    /// Every question.
    #[default]
    Guided,
    /// The user wrote the profile.
    Expert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Interview,
    /// The profile is built; waiting for activate / modify / cancel.
    Review,
}

/// What the interview has collected so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentCreationDraft {
    pub mode: CreationMode,
    pub template: Option<String>,
    pub name: Option<String>,
    pub purpose: Option<String>,
    pub domains: Vec<String>,
    pub tone: Vec<String>,
    pub behavior: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub allowed_capabilities: Vec<String>,
    pub memory_mode: Option<MemoryMode>,
    pub memory_label: Option<String>,
    pub auto_delegate: Option<bool>,
    pub model_preference: Option<String>,
    /// Questions answered (by key).
    pub answered: Vec<String>,
    pub stage: Stage,
    /// The built profile, once the interview is done.
    pub profile: Option<AgentProfile>,
    pub test_output: Option<String>,
}

/// One question: free text, or pick from options (by number or words).
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub key: &'static str,
    pub text: String,
    pub options: Vec<String>,
    pub multi: bool,
}

impl Question {
    pub fn render(&self) -> String {
        let mut out = self.text.clone();
        for (i, o) in self.options.iter().enumerate() {
            out += &format!("\n  {}. {o}", i + 1);
        }
        if self.multi {
            out += "\n(pick one or more, e.g. \"1, 3\", or write your own)";
        } else if !self.options.is_empty() {
            out += "\n(a number or your own words)";
        }
        out
    }
}

fn opts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

const MEMORY_OPTIONS: &[&str] = &["Nothing", "Your preferences only", "Shared memory, read-only", "Shared memory, read and write"];

impl AgentCreationDraft {
    /// Start from what the user asked: "create a writer agent" uses the
    /// writer template; "create a new subagent" is the full interview.
    pub fn start(request: &str) -> Self {
        let lower = request.to_lowercase();
        let template = templates::NAMES
            .iter()
            .filter(|t| **t != "custom")
            .find(|t| lower.contains(&t.replace('-', " ")) || lower.contains(*t))
            .map(|t| t.to_string());
        let mut d = Self { mode: if template.is_some() { CreationMode::Template } else { CreationMode::Guided }, ..Default::default() };
        if let Some(t) = &template {
            let p = templates::template(t).expect("listed template");
            d.name = Some(p.title.clone());
            d.purpose = Some(p.description.clone());
            d.answered.extend(["name".to_string(), "purpose".into()]);
        }
        d.template = template;
        d
    }

    fn base(&self) -> AgentProfile {
        self.template.as_deref().and_then(templates::template).unwrap_or_else(|| templates::template("custom").expect("custom"))
    }

    fn done(&self, key: &str) -> bool {
        self.answered.iter().any(|k| k == key)
    }

    /// The next question to ask, or `None` when the interview is complete.
    pub fn next_question(&self) -> Option<Question> {
        if self.stage != Stage::Interview || self.mode == CreationMode::Expert {
            return None;
        }
        let writer = self.template.as_deref() == Some("writer");
        let q = |key, text: &str, options: Vec<String>, multi| Question { key, text: text.to_string(), options, multi };
        let all = [
            q("name", "What should this agent be called? (e.g. Writer, Researcher, Support)", vec![], false),
            q(
                "purpose",
                "What should this agent primarily handle?",
                if writer || self.template.is_none() {
                    opts(&["Emails", "Reports", "Social posts", "Technical writing", "General writing", "Research", "Code", "Other"])
                } else {
                    vec![]
                },
                true,
            ),
            q("tone", "How should it normally work and write?", opts(&["Professional", "Concise", "Friendly", "Formal", "Technical", "Adaptive to context"]), true),
            q(
                "behavior",
                if writer { "Should it rewrite only, or also create new content?" } else { "Anything it should always or never do? (or \"skip\")" },
                if writer { opts(&["Rewrite and edit only", "Also create new content"]) } else { vec![] },
                false,
            ),
            q("tools", "Should it have access to any tools?", opts(&["None", "Read memory", "Read and write memory", "Specific capabilities (name them)"]), false),
            q("memory", "Should it remember things?", opts(MEMORY_OPTIONS), false),
            q("delegation", "Should the main agent automatically hand matching tasks to it?", opts(&["Yes, automatically", "No, only when asked"]), false),
            q("model", "Which model should it use?", opts(&["The same as the main agent", "Another model (name it)"]), false),
        ];
        // A template only asks how to customize it.
        let template_keys = ["tone", "behavior", "memory", "delegation"];
        all.into_iter().find(|q| !self.done(q.key) && (self.mode == CreationMode::Guided || template_keys.contains(&q.key)))
    }

    /// Record the answer to the current question.
    pub fn answer(&mut self, text: &str) -> Result<(), String> {
        let q = self.next_question().ok_or("the interview is complete")?;
        let text = text.trim();
        if text.is_empty() {
            return Err(q.render());
        }
        // "2" or "1, 3" pick options; anything else is taken as written.
        let picked: Vec<String> = text
            .split([',', ' '])
            .filter_map(|t| t.trim().parse::<usize>().ok())
            .filter_map(|n| q.options.get(n.wrapping_sub(1)).cloned())
            .collect();
        let chosen: Vec<String> = if !picked.is_empty() { picked } else { vec![text.to_string()] };
        let first = chosen[0].to_lowercase();
        let skip = first == "skip";
        match q.key {
            "name" => {
                if AgentProfile::slug(text).is_empty() || AgentProfile::slug(text) == MAIN {
                    return Err("that name doesn't work; try another".into());
                }
                self.name = Some(text.trim_end_matches(" agent").trim_end_matches(" Agent").to_string());
            }
            "purpose" => {
                self.domains = chosen.iter().filter(|c| c.as_str() != "Other").cloned().collect();
                self.purpose = Some(chosen.join(", "));
            }
            "tone" if !skip => self.tone = chosen,
            "behavior" if !skip => self.behavior = chosen,
            "tools" => {
                self.allowed_tools = match first.as_str() {
                    f if f.starts_with("none") => vec![],
                    f if f.starts_with("read memory") => opts(&["memory_recall", "memory_list", "memory_inspect"]),
                    f if f.starts_with("read and write") => {
                        opts(&["memory_recall", "memory_list", "memory_inspect", "memory_remember", "memory_correct", "memory_supersede"])
                    }
                    f if f.starts_with("specific") => return Err("name the capabilities, separated by commas (see /caps)".into()),
                    _ => text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                }
            }
            "memory" => {
                let mode = match MEMORY_OPTIONS.iter().position(|o| o.to_lowercase() == first) {
                    Some(0) => MemoryMode::None,
                    Some(1) => MemoryMode::Scoped,
                    Some(2) => MemoryMode::SharedReadOnly,
                    Some(3) => MemoryMode::SharedReadWrite,
                    _ if first.contains("nothing") || first == "no" => MemoryMode::None,
                    _ if first.contains("prefer") => MemoryMode::Scoped,
                    _ if first.contains("write") => MemoryMode::SharedReadWrite,
                    _ => MemoryMode::SharedReadOnly,
                };
                self.memory_mode = Some(mode);
                self.memory_label = Some(chosen[0].clone());
            }
            "delegation" => self.auto_delegate = Some(first.starts_with("yes") || first.starts_with('y')),
            "model" => {
                self.model_preference = (!first.starts_with("the same") && !skip && !first.starts_with("another")).then(|| text.to_string());
                if first.starts_with("another") {
                    return Err("which model? (its name on the server)".into());
                }
            }
            _ => {}
        }
        self.answered.push(q.key.to_string());
        Ok(())
    }

    /// The profile from the answers, before the model adds its instructions.
    pub fn profile(&self) -> AgentProfile {
        let mut p = self.base();
        if let Some(name) = &self.name {
            p.title = name.clone();
            p.name = AgentProfile::slug(name);
            p.id = AgentProfile::new(&p.name, name, "").id;
        }
        if let (Some(purpose), true) = (&self.purpose, self.template.is_none()) {
            p.description = format!("Specialist for {}.", purpose.to_lowercase());
        }
        if self.done("tools") {
            p.tools = self.allowed_tools.clone();
            p.capabilities = self.allowed_capabilities.clone();
        }
        if let Some(mode) = self.memory_mode {
            p.memory_policy.mode = mode;
            if mode == MemoryMode::Scoped && p.memory_policy.read.is_empty() {
                p.memory_policy.read = vec!["user".into()];
            }
            // An agent that may not read memory gets no memory tools.
            if !p.memory_policy.reads() {
                p.tools.retain(|t| !t.starts_with("memory_"));
            }
            if !p.memory_policy.writes() {
                p.tools.retain(|t| !matches!(t.as_str(), "memory_remember" | "memory_correct" | "memory_supersede"));
            }
        }
        if p.tools.iter().any(|t| matches!(t.as_str(), "memory_remember" | "memory_correct" | "memory_supersede")) && p.permission_policy.max_risk == "read_only" {
            p.permission_policy.max_risk = "low_write".into();
        }
        if let Some(auto) = self.auto_delegate {
            p.delegation.auto_delegate = auto;
        }
        p.model_policy.model = self.model_preference.clone();
        let mut extra = Vec::new();
        if !self.tone.is_empty() {
            extra.push(format!("Default style: {}.", self.tone.join(", ").to_lowercase()));
        }
        if !self.behavior.is_empty() {
            extra.push(self.behavior.join(". "));
        }
        if !extra.is_empty() {
            p.instructions = format!("{}\n{}", p.instructions, extra.join("\n")).trim().to_string();
        }
        p
    }

    /// A summary for the user to approve.
    pub fn summary(&self) -> String {
        let p = self.profile.clone().unwrap_or_else(|| self.profile());
        let tools = if p.tools.is_empty() && p.capabilities.is_empty() {
            "none".into()
        } else {
            [p.tools.clone(), p.capabilities.clone()].concat().join(", ")
        };
        format!(
            "{} Agent\n\nPurpose:\n{}\n\nRole:\n{}\n\nInstructions:\n{}\n\nHandles:\n{}\n\nAuto delegation:\n{}\n\nTools:\n{tools}\n\nMemory:\n{}\n\nModel:\n{}",
            p.title,
            p.description,
            p.role,
            p.instructions,
            if p.delegation.examples.is_empty() { p.delegation.intents.join(", ") } else { p.delegation.examples.join("\n") },
            if p.delegation.auto_delegate { "enabled" } else { "only when asked" },
            self.memory_label.clone().unwrap_or_else(|| p.memory_policy.mode.as_str().replace('_', " ")),
            p.model_policy.model.clone().unwrap_or_else(|| "same as the main agent".into()),
        )
    }
}

pub const GENERATE_PROMPT: &str = "\
You write the configuration for a specialist agent inside an AI assistant from a \
short interview. Write its role (one line), its instructions (3-6 sentences in the \
second person: how it works, its style, what to keep and what to avoid), the intents \
it handles (snake_case), routing keywords, 4-6 example requests it should get, a few \
requests it should NOT get, and one realistic test task.

Respond with only a JSON object: {\"role\": \"...\", \"instructions\": \"...\", \"intents\": [\"...\"], \
\"keywords\": [\"...\"], \"examples\": [\"...\"], \"exclusions\": [\"...\"], \"test_task\": \"...\"}";

pub fn generate_prompt(d: &AgentCreationDraft) -> String {
    let p = d.profile();
    format!(
        "Agent: {}\nPurpose: {}\nHandles: {}\nStyle: {}\nBehavior: {}\nTools: {}\nStarting template: {}\nTemplate instructions: {}",
        p.title,
        d.purpose.clone().unwrap_or_else(|| p.description.clone()),
        d.domains.join(", "),
        d.tone.join(", "),
        d.behavior.join("; "),
        if p.tools.is_empty() { "none".into() } else { p.tools.join(", ") },
        d.template.clone().unwrap_or_else(|| "none".into()),
        p.instructions
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Generated {
    role: String,
    instructions: String,
    intents: Vec<String>,
    keywords: Vec<String>,
    examples: Vec<String>,
    exclusions: Vec<String>,
    test_task: String,
}

/// Merge the model's draft into the profile (keeping the template's where
/// the model gave nothing).
pub fn apply_generated(mut p: AgentProfile, reply: &str) -> Result<AgentProfile, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, a)| a);
    let (Some(s), Some(e)) = (reply.find('{'), reply.rfind('}')) else { return Err("no JSON in the reply".into()) };
    let g: Generated = serde_json::from_str(&reply[s..=e.max(s)]).map_err(|e| format!("bad JSON: {e}"))?;
    let keep = |new: String, old: String| if new.trim().is_empty() { old } else { new.trim().to_string() };
    p.role = keep(g.role, p.role);
    p.instructions = keep(g.instructions, p.instructions);
    let merge = |new: Vec<String>, old: &mut Vec<String>| {
        for n in new.into_iter().filter(|n| !n.trim().is_empty()) {
            if !old.contains(&n) {
                old.push(n);
            }
        }
    };
    merge(g.intents, &mut p.delegation.intents);
    merge(g.keywords, &mut p.delegation.keywords);
    merge(g.examples, &mut p.delegation.examples);
    merge(g.exclusions, &mut p.delegation.exclusions);
    if !g.test_task.trim().is_empty() {
        p.test_task = Some(g.test_task.trim().to_string());
    }
    Ok(p)
}

/// Expert mode: a whole profile written by the user, as TOML or YAML.
pub fn from_expert(text: &str) -> Result<AgentProfile, String> {
    #[derive(Deserialize)]
    struct Loose {
        name: String,
        title: Option<String>,
        description: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        instructions: String,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        capabilities: Vec<String>,
        #[serde(default)]
        memory: Option<MemoryPolicy>,
        #[serde(default)]
        permissions: Option<PermissionPolicy>,
        #[serde(default)]
        routing: Option<DelegationProfile>,
        #[serde(default)]
        model: Option<ModelPolicy>,
    }
    let loose: Loose = toml::from_str(text)
        .map_err(|e| e.to_string())
        .or_else(|t| serde_yaml_ng::from_str(text).map_err(|y| format!("not TOML ({t}) or YAML ({y})")))?;
    let title = loose.title.unwrap_or_else(|| loose.name.clone());
    let mut p = AgentProfile::new(&AgentProfile::slug(&loose.name), &title, &loose.description);
    p.role = loose.role;
    p.instructions = loose.instructions;
    p.tools = loose.tools;
    p.capabilities = loose.capabilities;
    p.memory_policy = loose.memory.unwrap_or_default();
    p.permission_policy = loose.permissions.unwrap_or_default();
    p.delegation = loose.routing.unwrap_or_default();
    p.model_policy = loose.model.unwrap_or_default();
    crate::registry::validate(&p)?;
    Ok(p)
}
