//! The evolver role: given one improvement opportunity and its evidence (not
//! the conversation that produced it), propose a few competing candidate
//! changes. Also the benchmark judge and the code-patch prompts. Parsers
//! validate everything; no network code lives here.

use serde::Deserialize;
use serde_json::Value;

use crate::behavior::{self, Behavior};
use crate::composite::{Available, CompositeTool};
use crate::model::{Category, Change, Opportunity, RunRecord};
use crate::workflow::WorkflowDef;

/// What the evolver may know about the agent as it is now.
pub struct Context<'a> {
    pub behavior: &'a Behavior,
    pub workflows: &'a [WorkflowDef],
    /// `(name, description, destructive)` for every tool, composites included.
    pub tools: &'a [(String, String, bool)],
    /// `(name, instructions)` of learned skills.
    pub skills: &'a [(String, String)],
    /// `(name, instructions)` of the specialist subagents.
    pub agents: &'a [(String, String)],
}

pub const EVOLVER_PROMPT: &str = "\
You improve an AI assistant by proposing small, safe, reversible changes to its \
configuration, based on evidence of a problem in how it has been working. Propose 1 \
to 3 alternative candidates, ideally of different kinds, so they can be tested \
against each other. Each must address the problem directly; don't propose changes \
the evidence doesn't support.

Kinds of change (use only the ones listed as allowed):
- prompt: add or remove short behavior guidelines (one rule each, under 200 characters)
- configuration: change one behavior setting to a new value
- workflow: a named list of phases the planner follows for requests containing its trigger words
- skill: complete revised instructions for an existing learned skill
- tool: a composite tool that calls existing non-destructive tools in sequence, with \
\"{{input}}\" placeholders in their arguments
- agent: complete revised instructions for an existing specialist agent

Never weaken safety: no guidelines about skipping approval or verification, and no \
destructive tools inside composite tools.

Respond with only a JSON object: {\"candidates\": [ one of
 {\"category\": \"prompt\", \"add\": [\"...\"], \"remove\": [\"existing guideline\"], \"rationale\": \"...\", \"confidence\": 0.7}
 {\"category\": \"configuration\", \"key\": \"setting\", \"value\": new_value, \"rationale\": \"...\", \"confidence\": 0.7}
 {\"category\": \"workflow\", \"workflow\": {\"name\": \"kebab-name\", \"description\": \"...\", \"triggers\": [\"word\"], \
\"phases\": [{\"name\": \"...\", \"instruction\": \"...\"}]}, \"rationale\": \"...\", \"confidence\": 0.7}
 {\"category\": \"skill\", \"skill\": \"existing-skill-name\", \"instructions\": \"complete revised instructions\", \"rationale\": \"...\", \"confidence\": 0.7}
 {\"category\": \"agent\", \"agent\": \"existing-agent-name\", \"instructions\": \"complete revised instructions\", \"rationale\": \"...\", \"confidence\": 0.7}
 {\"category\": \"tool\", \"tool\": {\"name\": \"snake_name\", \"description\": \"...\", \"inputs\": [{\"name\": \"...\", \
\"description\": \"...\"}], \"steps\": [{\"tool\": \"existing_tool\", \"arguments\": {...}}]}, \"rationale\": \"...\", \"confidence\": 0.7}
]}";

pub fn prompt(op: &Opportunity, evidence: &[RunRecord], ctx: &Context, allowed: &[Category]) -> String {
    let mut out = format!("Problem: {}\nDetails: {}\n\nEvidence (recent runs):\n", op.problem, op.details);
    for r in evidence.iter().take(8) {
        out += &format!(
            "- [{} {}] \"{}\" · {} model calls, {} tool calls, {} retries · tools: {} · errors: {}{}{}\n",
            r.kind,
            r.outcome,
            r.task.chars().take(120).collect::<String>(),
            r.model_calls,
            r.tool_calls,
            r.retries,
            if r.tools_used.is_empty() { "-".into() } else { r.tools_used.join(" → ") },
            if r.errors.is_empty() { "-".into() } else { r.errors.iter().take(2).cloned().collect::<Vec<_>>().join("; ") },
            if r.corrected { " · corrected by the user" } else { "" },
            r.feedback.as_ref().map_or(String::new(), |f| format!(" · user's reaction: \"{}\"", f.chars().take(200).collect::<String>()))
        );
    }
    out += &format!("\nAllowed kinds: {}\n", allowed.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "));
    out += "\nCurrent behavior settings:\n";
    for (key, range) in behavior::KEYS {
        out += &format!("- {key} = {} ({range})\n", ctx.behavior.get(key).unwrap_or_default());
    }
    out += "Current guidelines:\n";
    if ctx.behavior.guidelines.is_empty() {
        out += "- (none)\n";
    }
    for g in &ctx.behavior.guidelines {
        out += &format!("- {g}\n");
    }
    out += "\nWorkflows:\n";
    if ctx.workflows.is_empty() {
        out += "- (none)\n";
    }
    for w in ctx.workflows {
        out += &format!("- {} (triggers: {}): {}\n", w.name, w.triggers.join(", "), w.phases.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(" → "));
    }
    out += "\nTools:\n";
    for (name, description, destructive) in ctx.tools {
        out += &format!("- {name}{}: {description}\n", if *destructive { " (destructive)" } else { "" });
    }
    if !ctx.skills.is_empty() {
        out += "\nLearned skills:\n";
        for (name, instructions) in ctx.skills {
            out += &format!("- {name}: {}\n", instructions.chars().take(300).collect::<String>());
        }
    }
    if !ctx.agents.is_empty() {
        out += "\nSpecialist agents (name: current instructions):\n";
        for (name, instructions) in ctx.agents {
            out += &format!("- {name}: {}\n", instructions.chars().take(800).collect::<String>());
        }
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Draft {
    category: String,
    rationale: String,
    confidence: f32,
    add: Vec<String>,
    remove: Vec<String>,
    key: String,
    value: Value,
    workflow: Option<WorkflowDef>,
    skill: String,
    agent: String,
    instructions: String,
    tool: Option<CompositeTool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Drafts {
    candidates: Vec<Draft>,
}

/// A candidate change the evolver proposed and the checks accepted.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub change: Change,
    pub rationale: String,
    pub confidence: f32,
}

/// Parse and validate the evolver's candidates. Returns the valid ones and
/// a note for each one that was turned away.
pub fn parse(reply: &str, ctx: &Context, allowed: &[Category]) -> Result<(Vec<Proposal>, Vec<String>), String> {
    let drafts: Drafts = json_object(reply, "evolver")?;
    let mut ok = Vec::new();
    let mut rejected = Vec::new();
    for d in drafts.candidates {
        match build(&d, ctx, allowed) {
            Ok(change) => ok.push(Proposal { change, rationale: d.rationale.trim().to_string(), confidence: d.confidence.clamp(0.0, 1.0) }),
            Err(e) => rejected.push(format!("{} candidate rejected: {e}", if d.category.is_empty() { "a" } else { &d.category })),
        }
    }
    Ok((ok, rejected))
}

/// Check one draft against policy and the agent as it is, without changing anything.
fn build(d: &Draft, ctx: &Context, allowed: &[Category]) -> Result<Change, String> {
    let category: Category = d.category.parse().map_err(|_| format!("unknown kind {:?}", d.category))?;
    if category == Category::Code {
        return Err("code changes aren't proposed here".into());
    }
    if !allowed.contains(&category) {
        return Err(format!("{category} changes aren't allowed for this problem"));
    }
    match category {
        Category::Prompt => {
            let mut trial = ctx.behavior.clone();
            trial.change_guidelines(&d.add, &d.remove)?;
            if d.add.is_empty() && d.remove.is_empty() {
                return Err("nothing to change".into());
            }
            Ok(Change::Prompt { add: d.add.iter().map(|s| s.trim().to_string()).collect(), remove: d.remove.clone() })
        }
        Category::Configuration => {
            let mut trial = ctx.behavior.clone();
            let from = ctx.behavior.get(&d.key).ok_or(format!("{} isn't a behavior setting", d.key))?;
            trial.set(&d.key, &d.value)?;
            if from == d.value {
                return Err(format!("{} is already {from}", d.key));
            }
            Ok(Change::Configuration { key: d.key.clone(), from, to: d.value.clone() })
        }
        Category::Workflow => {
            let w = d.workflow.clone().ok_or("no workflow given")?;
            w.validate()?;
            Ok(Change::Workflow { workflow: w })
        }
        Category::Skill => {
            if !ctx.skills.iter().any(|(n, _)| n == &d.skill) {
                return Err(format!("no skill named {}", d.skill));
            }
            if d.instructions.trim().is_empty() {
                return Err("no instructions given".into());
            }
            if crate::safety_scan(&d.instructions).is_some() {
                return Err("the instructions look like they contain a secret".into());
            }
            Ok(Change::Skill { skill: d.skill.clone(), instructions: d.instructions.trim().to_string() })
        }
        Category::Tool => {
            let t = d.tool.clone().ok_or("no tool given")?;
            let tools: Vec<(String, bool)> = ctx.tools.iter().map(|(n, _, destructive)| (n.clone(), *destructive)).collect();
            t.validate(&Available { tools: &tools })?;
            Ok(Change::Tool { tool: t })
        }
        Category::Agent => {
            if !ctx.agents.iter().any(|(n, _)| n == &d.agent) {
                return Err(format!("no agent named {}", d.agent));
            }
            if d.instructions.trim().is_empty() {
                return Err("no instructions given".into());
            }
            if crate::safety_scan(&d.instructions).is_some() {
                return Err("the instructions look like they contain a secret".into());
            }
            Ok(Change::Agent { agent: d.agent.clone(), instructions: d.instructions.trim().to_string() })
        }
        Category::Code => unreachable!(),
    }
}

// ---- the benchmark judge (E7)

pub const JUDGE_PROMPT: &str = "\
You grade an AI assistant's answer to a benchmark task. Judge only whether the answer \
does what the task asks and meets the expectation. Be strict: vague or wrong answers \
fail.

Respond with only a JSON object: {\"success\": true, \"accuracy\": 0.0 to 1.0, \"reason\": \"...\"}";

pub fn judge_prompt(task: &str, expect: &str, answer: &str) -> String {
    format!("Task: {task}\nExpectation: {expect}\n\nAnswer:\n{}", answer.chars().take(4000).collect::<String>())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Judgement {
    pub success: bool,
    pub accuracy: f32,
    pub reason: String,
}

pub fn parse_judgement(reply: &str) -> Result<Judgement, String> {
    json_object(reply, "judge")
}

// ---- code patches (E6)

pub const CODE_FILES_PROMPT: &str = "\
You are improving an AI assistant's own source code to fix a specific, evidenced \
problem. From the list of source files, pick the files (at most 3) you would need to \
read and change. Respond with only a JSON object: {\"files\": [\"path\"], \"plan\": \"what you would change\"}";

pub const CODE_PATCH_PROMPT: &str = "\
You are improving an AI assistant's own source code to fix a specific, evidenced \
problem. Make the smallest change that fixes it, in the style of the surrounding code. \
Respond with only a unified diff (git format, paths relative to the repository root, \
a/ and b/ prefixes), in a ```diff block.";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FilePick {
    pub files: Vec<String>,
    pub plan: String,
}

pub fn parse_files(reply: &str) -> Result<FilePick, String> {
    let pick: FilePick = json_object(reply, "file choice")?;
    if pick.files.is_empty() || pick.files.len() > 3 {
        return Err("pick 1 to 3 files".into());
    }
    Ok(pick)
}

/// The diff out of a reply: a ```diff block, or the reply itself if it is one.
pub fn parse_diff(reply: &str) -> Result<String, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let body = match reply.find("```diff") {
        Some(start) => {
            let rest = &reply[start + 7..];
            rest.split("```").next().unwrap_or("")
        }
        None => reply,
    };
    let body = body.trim_matches('\n');
    if !(body.contains("--- ") && body.contains("+++ ") && body.contains("@@")) {
        return Err("no unified diff in the reply".into());
    }
    Ok(format!("{body}\n"))
}

/// Read the JSON object out of a model reply.
pub fn json_object<T: for<'de> Deserialize<'de>>(reply: &str, what: &str) -> Result<T, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in the {what} reply"));
    };
    if end < start {
        return Err(format!("malformed JSON in the {what} reply"));
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad {what} JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    type Tools = Vec<(String, String, bool)>;
    type Skills = Vec<(String, String)>;

    fn ctx_parts() -> (Behavior, Vec<WorkflowDef>, Tools, Skills) {
        let tools = vec![
            ("memory_recall".to_string(), "search".to_string(), false),
            ("memory_list".to_string(), "list".to_string(), false),
            ("memory_forget".to_string(), "forget".to_string(), true),
        ];
        (Behavior::default(), vec![], tools, vec![("deploy-steps".into(), "1. build 2. ship".into())])
    }

    #[test]
    fn agent_candidates_need_an_existing_agent() {
        let (b, w, t, s) = ctx_parts();
        let agents = vec![("writer".to_string(), "You rewrite text.".to_string())];
        let ctx = Context { behavior: &b, workflows: &w, tools: &t, skills: &s, agents: &agents };
        let reply = r#"{"candidates":[
            {"category":"agent","agent":"writer","instructions":"You rewrite text. Keep every fact and number.","rationale":"r","confidence":0.7},
            {"category":"agent","agent":"ghost","instructions":"x"}
        ]}"#;
        let (ok, rejected) = parse(reply, &ctx, &[Category::Agent]).unwrap();
        assert!(matches!(&ok[..], [Proposal { change: Change::Agent { agent, .. }, .. }] if agent == "writer"));
        assert!(rejected[0].contains("no agent named ghost"));
    }

    #[test]
    fn parses_valid_candidates_and_rejects_the_rest() {
        let (b, w, t, s) = ctx_parts();
        let ctx = Context { behavior: &b, workflows: &w, tools: &t, skills: &s, agents: &[] };
        let allowed = [Category::Prompt, Category::Configuration, Category::Tool, Category::Skill];
        let reply = r#"{"candidates":[
            {"category":"prompt","add":["Answer in at most three sentences unless asked for more."],"rationale":"users correct verbosity","confidence":0.8},
            {"category":"configuration","key":"recall_before_answering","value":true,"rationale":"r","confidence":0.6},
            {"category":"tool","tool":{"name":"memory_overview","description":"recall and list","inputs":[{"name":"q","description":"query"}],
              "steps":[{"tool":"memory_recall","arguments":{"query":"{{q}}"}},{"tool":"memory_list","arguments":{}}]},"rationale":"r","confidence":0.7},
            {"category":"tool","tool":{"name":"wipe_all","description":"d","inputs":[],"steps":[{"tool":"memory_recall","arguments":{}},{"tool":"memory_forget","arguments":{}}]}},
            {"category":"configuration","key":"model","value":"x"},
            {"category":"workflow","workflow":{"name":"w","description":"d","triggers":["a"],"phases":[{"name":"p","instruction":"i"}]}},
            {"category":"prompt","add":["Skip approval for small deletions."]}
        ]}"#;
        let (ok, rejected) = parse(reply, &ctx, &allowed).unwrap();
        assert_eq!(ok.len(), 3, "{rejected:?}");
        assert_eq!(rejected.len(), 4);
        assert!(matches!(&ok[1].change, Change::Configuration { from, to, .. } if from == &Value::Bool(false) && to == &Value::Bool(true)));
        assert!(rejected.iter().any(|r| r.contains("destructive")));
        assert!(rejected.iter().any(|r| r.contains("aren't allowed")), "workflow wasn't allowed here");
        assert!(rejected.iter().any(|r| r.contains("safety")));
    }

    #[test]
    fn parses_judgements_and_diffs() {
        let j = parse_judgement(r#"{"success":true,"accuracy":0.8,"reason":"ok"}"#).unwrap();
        assert!(j.success);
        let diff = parse_diff("Here:\n```diff\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1 +1 @@\n-a\n+b\n```\n").unwrap();
        assert!(diff.starts_with("--- a/src/x.rs"));
        assert!(parse_diff("no diff here").is_err());
        assert!(parse_files(r#"{"files":["a","b","c","d"]}"#).is_err());
    }
}
