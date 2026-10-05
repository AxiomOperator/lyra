//! Talking to the model about plans: parsing a request into a goal (P1),
//! generating a structured plan (P2, P10), revising the broken part (P7),
//! judging a step (P5) and judging the goal (P17). The model proposes; the
//! parsers here enforce policy (known tools, approval for risky ones, no
//! touching completed work). No network code: the engine makes the calls.

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::graph;
use crate::model::*;

/// How dangerous a tool is, as declared by the runtime (not the model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    ReadOnly,
    Mutating,
    Destructive,
}

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub risk: Risk,
    /// The tool's JSON-schema parameters.
    pub parameters: Value,
}

impl ToolInfo {
    /// `content*, scope, tags` (required ones starred).
    fn parameter_list(&self) -> String {
        let required: Vec<&str> =
            self.parameters["required"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let names: Vec<String> = self.parameters["properties"]
            .as_object()
            .map(|p| p.keys().map(|k| if required.contains(&k.as_str()) { format!("{k}*") } else { k.clone() }).collect())
            .unwrap_or_default();
        if names.is_empty() { "none".into() } else { names.join(", ") }
    }
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    /// It can use tools that change things.
    pub changes_things: bool,
}

/// What the planner gets to see (P10): only what's relevant.
#[derive(Debug, Clone, Default)]
pub struct PlanningContext {
    pub memories: Vec<String>,
    /// `(name, description)` of relevant skills; also the workflows a step may use.
    pub skills: Vec<(String, String)>,
    /// Every workflow a step may name, shown or not (the planner may know a
    /// procedure's name from guidance or memory).
    pub workflows: Vec<String>,
    pub tools: Vec<ToolInfo>,
    pub agents: Vec<AgentInfo>,
    /// Tools steps may never use.
    pub forbidden_tools: Vec<String>,
    /// Set when the budget is running low, so plans stay short.
    pub budget_note: Option<String>,
    /// How to plan this kind of request (a workflow), if one applies.
    pub guidance: Option<String>,
}

impl PlanningContext {
    fn tool(&self, name: &str) -> Option<&ToolInfo> {
        self.tools.iter().find(|t| t.name == name)
    }

    fn render(&self) -> String {
        let mut out = String::from("Tools a step can call (parameters; * = required):\n");
        for t in &self.tools {
            let risk = match t.risk {
                Risk::ReadOnly => "read-only",
                Risk::Mutating => "changes things",
                Risk::Destructive => "destructive: only as its own tool step, which needs approval",
            };
            out += &format!("- {} ({risk}): {} Parameters: {}\n", t.name, t.description, t.parameter_list());
        }
        out += "\nHelper agents for subagent steps:\n";
        for a in &self.agents {
            out += &format!("- {}: {}\n", a.name, a.description);
        }
        if !self.skills.is_empty() {
            out += "\nLearned procedures (usable as workflow steps by name):\n";
            for (name, description) in &self.skills {
                out += &format!("- {name}: {description}\n");
            }
        }
        if !self.memories.is_empty() {
            out += "\nRelevant things remembered:\n";
            for m in &self.memories {
                out += &format!("- {m}\n");
            }
        }
        if let Some(guidance) = &self.guidance {
            out += &format!("\n{guidance}\n");
        }
        if let Some(note) = &self.budget_note {
            out += &format!("\nNote: {note}\n");
        }
        out
    }
}

/// Read the JSON object out of a model reply, tolerating thinking tags and text around it.
pub fn json_object<T: for<'de> Deserialize<'de>>(reply: &str, what: &str) -> Result<T, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in the {what} reply: {}", reply.trim().chars().take(200).collect::<String>()));
    };
    if end < start {
        return Err(format!("malformed JSON in the {what} reply"));
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad {what} JSON: {e}"))
}

// ---- P1: goals

pub const GOAL_PROMPT: &str = "\
You turn a user's request into a precise goal for an AI agent that will plan and \
carry it out. Define what success means: concrete, checkable criteria, not a \
restatement of the request. List explicit constraints, the outputs the user \
expects, whether it involves destructive or external changes, and any ambiguity \
that would change how it should be done (leave it empty if there's none).

Respond with only a JSON object:
{\"description\": \"...\", \"success_criteria\": [\"...\"], \"constraints\": [\"...\"], \
\"outputs\": [\"...\"], \"destructive\": false, \"ambiguities\": [\"...\"]}";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GoalDraft {
    pub description: String,
    pub success_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub outputs: Vec<String>,
    pub destructive: bool,
    pub ambiguities: Vec<String>,
}

pub fn parse_goal(request: &str, reply: &str) -> Result<Goal, String> {
    let d: GoalDraft = json_object(reply, "goal")?;
    if d.description.trim().is_empty() {
        return Err("the goal has no description".into());
    }
    if d.success_criteria.is_empty() {
        return Err("the goal has no success criteria".into());
    }
    Ok(Goal {
        id: Uuid::new_v4(),
        request: request.to_string(),
        description: d.description.trim().to_string(),
        success_criteria: d.success_criteria,
        constraints: d.constraints,
        outputs: d.outputs,
        destructive: d.destructive,
        ambiguities: d.ambiguities,
        status: GoalStatus::Pending,
        created_at: Utc::now(),
    })
}

fn render_goal(goal: &Goal) -> String {
    let list = |v: &[String]| if v.is_empty() { "(none)".into() } else { v.iter().map(|x| format!("\n- {x}")).collect::<String>() };
    format!(
        "Goal: {}\nSuccess criteria:{}\nConstraints:{}\nExpected outputs:{}",
        goal.description,
        list(&goal.success_criteria),
        list(&goal.constraints),
        list(&goal.outputs)
    )
}

// ---- P2: plans

const STEP_FORMAT: &str = "\
Each step is an object:
{\"key\": \"s1\", \"title\": \"short title\", \"description\": \"what and why\", \
\"depends_on\": [\"keys of steps that must finish first\"], \
\"action\": one of
  {\"type\": \"tool\", \"tool\": \"tool_name\", \"arguments\": {...}}
  {\"type\": \"reasoning\", \"instruction\": \"what to work out or produce (may use tools)\"}
  {\"type\": \"workflow\", \"workflow\": \"learned-procedure-name\", \"input\": {...}}
  {\"type\": \"subagent\", \"agent\": \"agent-name\", \"task\": \"self-contained task\"},
\"expected_outcome\": \"what success looks like for this step\", \
\"verification\": {\"strategy\": \"tool_result | follow_up_tool | state_check | model_evaluation\", \
\"tool\": \"read-only tool for follow_up_tool\", \"arguments\": {...}, \"check\": \"what must hold\"}, \
\"idempotency\": \"safe | conditional | unsafe\", \"requires_approval\": false, \
\"resources\": [\"exclusive resources, e.g. host:db1\"], \"max_attempts\": 3}";

pub fn plan_system_prompt() -> String {
    format!(
        "You plan how an AI agent will reach a goal. Produce a small structured plan: each \
step small enough to verify on its own but not a single command; independent steps \
shouldn't depend on each other so they can run in parallel. Prefer a tool step when \
one tool call does the job, reasoning for thinking or writing, workflow for a learned \
procedure, subagent for a self-contained branch. Use deterministic verification \
(tool_result, follow_up_tool, state_check) where you can; model_evaluation for \
judgment calls. Mark anything that deletes, overwrites or acts externally as unsafe \
and requires_approval. Destructive tools can only be used as tool steps, never from \
reasoning, workflow or subagent steps. A reasoning or workflow step only gets tools \
that change things (e.g. recording in memory) when it is marked conditional; safe \
steps must be safe to repeat, so they only get read-only tools.

When a tool argument depends on an earlier step's result (an id it found, a text it \
wrote), write the placeholder \"{{{{key}}}}\" with that step's key, e.g. {{\"id\": \"{{{{s3}}}}\"}}, and \
make the step depend on it; it is filled in from that result just before the step runs.

{STEP_FORMAT}

Respond with only a JSON object: {{\"steps\": [ ... ]}}"
    )
}

pub fn plan_prompt(goal: &Goal, ctx: &PlanningContext) -> String {
    format!("{}\n\n{}", render_goal(goal), ctx.render())
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct StepDraft {
    pub key: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub action: Option<Value>,
    pub expected_outcome: Option<String>,
    pub verification: Option<Verification>,
    pub idempotency: Option<String>,
    pub requires_approval: bool,
    pub resources: Vec<String>,
    pub max_attempts: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PlanDraft {
    steps: Vec<StepDraft>,
}

/// Turn a draft step into a step, enforcing policy: known tools, workflows
/// and agents; approval for destructive tools (the model can ask for more
/// approval, never less); no automatic retries of unsafe steps.
fn build_step(d: &StepDraft, id: Uuid, deps: Vec<Uuid>, ctx: &PlanningContext) -> Result<PlanStep, String> {
    let key = d.key.clone();
    let action: StepAction = serde_json::from_value(d.action.clone().ok_or(format!("{key} has no action"))?)
        .map_err(|e| format!("{key} has a bad action: {e}"))?;
    let mut approval = if d.requires_approval { ApprovalPolicy::RequireApproval } else { ApprovalPolicy::Automatic };
    let mut idempotency = match d.idempotency.as_deref() {
        Some("unsafe") => Idempotency::Unsafe,
        Some("conditional") => Idempotency::Conditional,
        _ => Idempotency::Safe,
    };
    match &action {
        StepAction::Tool { tool, .. } => {
            if ctx.forbidden_tools.contains(tool) {
                return Err(format!("{key} uses {tool}, which plans may not use"));
            }
            let info = ctx.tool(tool).ok_or(format!("{key} uses unknown tool {tool}"))?;
            match info.risk {
                Risk::Destructive => {
                    approval = ApprovalPolicy::RequireApproval;
                    idempotency = Idempotency::Unsafe;
                }
                Risk::Mutating if idempotency == Idempotency::Safe => idempotency = Idempotency::Conditional,
                _ => {}
            }
        }
        StepAction::Workflow { workflow, .. } => {
            if !ctx.skills.iter().any(|(n, _)| n == workflow) && !ctx.workflows.contains(workflow) {
                return Err(format!("{key} uses unknown workflow {workflow}"));
            }
        }
        StepAction::Subagent { agent, .. } => {
            let info = ctx.agents.iter().find(|a| &a.name == agent).ok_or(format!("{key} uses unknown agent {agent}"))?;
            // An agent that changes things can't be repeated blindly.
            if info.changes_things && idempotency == Idempotency::Safe {
                idempotency = Idempotency::Conditional;
            }
        }
        StepAction::Reasoning { instruction } if instruction.trim().is_empty() => {
            return Err(format!("{key} has an empty instruction"));
        }
        StepAction::Reasoning { .. } => {}
    }
    let verification = d.verification.clone().unwrap_or_default();
    if verification.strategy == VerificationStrategy::FollowUpTool {
        let tool = verification.tool.as_deref().ok_or(format!("{key}: follow_up_tool needs a tool"))?;
        let info = ctx.tool(tool).ok_or(format!("{key} verifies with unknown tool {tool}"))?;
        if info.risk != Risk::ReadOnly {
            return Err(format!("{key} verifies with {tool}, which isn't read-only"));
        }
    }
    let mut retry_policy = RetryPolicy { max_attempts: d.max_attempts.unwrap_or(3).clamp(1, 5), ..RetryPolicy::default() };
    if idempotency == Idempotency::Unsafe {
        // Repeating it repeats its effect: never automatically.
        retry_policy.max_attempts = 1;
    }
    let title = if d.title.trim().is_empty() { key.clone() } else { d.title.trim().to_string() };
    Ok(PlanStep {
        id,
        key,
        title,
        description: d.description.trim().to_string(),
        status: StepStatus::Pending,
        dependencies: deps,
        action,
        expected_outcome: d.expected_outcome.clone().filter(|s| !s.trim().is_empty()),
        verification,
        retry_policy,
        approval,
        approved: None,
        idempotency,
        resources: d.resources.clone(),
        result: None,
        verification_result: None,
        attempts: 0,
        failure_class: None,
        last_error: None,
        operation_id: (idempotency != Idempotency::Safe).then(Uuid::new_v4),
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
    })
}

/// Build steps from drafts. `known` maps keys of existing steps (for
/// revisions) so new steps can depend on them.
fn build_steps(drafts: &[StepDraft], known: &HashMap<String, Uuid>, ctx: &PlanningContext) -> Result<Vec<PlanStep>, String> {
    let mut ids: HashMap<String, Uuid> = known.clone();
    let mut drafts = drafts.to_vec();
    let mut n = known.len();
    for d in &mut drafts {
        if d.key.trim().is_empty() || ids.contains_key(d.key.trim()) && !known.contains_key(d.key.trim()) {
            n += 1;
            d.key = format!("s{n}");
            while ids.contains_key(&d.key) {
                n += 1;
                d.key = format!("s{n}");
            }
        }
        d.key = d.key.trim().to_string();
        if known.contains_key(&d.key) {
            return Err(format!("new step {} reuses an existing key", d.key));
        }
        ids.insert(d.key.clone(), Uuid::new_v4());
    }
    drafts
        .iter()
        .map(|d| {
            let deps = d
                .depends_on
                .iter()
                .map(|k| ids.get(k.trim()).copied().ok_or(format!("{} depends on unknown step {k}", d.key)))
                .collect::<Result<Vec<_>, _>>()?;
            build_step(d, ids[&d.key], deps, ctx)
        })
        .collect()
}

pub fn parse_plan(reply: &str, ctx: &PlanningContext) -> Result<Vec<PlanStep>, String> {
    let draft: PlanDraft = json_object(reply, "plan")?;
    if draft.steps.is_empty() {
        return Err("the plan has no steps".into());
    }
    let steps = build_steps(&draft.steps, &HashMap::new(), ctx)?;
    graph::validate(&steps)?;
    Ok(steps)
}

// ---- binding placeholders in tool arguments

/// Whether a tool step's arguments still refer to earlier results.
pub fn has_placeholders(action: &StepAction) -> bool {
    matches!(action, StepAction::Tool { arguments, .. } if arguments.to_string().contains("{{"))
}

pub const BIND_PROMPT: &str = "\
A tool call in an AI agent's plan has placeholders like {{s3}} that stand for values \
from earlier steps' results. Fill them in from those results and return the complete \
arguments, using the tool's parameter names. If a value can't be determined from the \
results, don't guess.

Respond with only a JSON object: {\"arguments\": {...}} or {\"error\": \"why it can't be filled in\"}";

pub fn bind_prompt(plan: &Plan, step: &PlanStep, tool: Option<&ToolInfo>) -> String {
    let StepAction::Tool { tool: name, arguments } = &step.action else { return String::new() };
    let mut out = format!("Step {}: {}\n{}\nTool: {name}\n", step.key, step.title, step.description);
    if let Some(t) = tool {
        out += &format!("Tool parameters (JSON schema): {}\n", t.parameters);
    }
    out += &format!("Arguments with placeholders: {arguments}\n\nEarlier results:\n");
    for dep in step.dependencies.iter().filter_map(|d| plan.step(*d)) {
        let result = dep.result.as_ref().map_or("(none)".into(), |r| truncate(&output_text(&r.output), 3000));
        out += &format!("\n{} ({}):\n{result}\n", dep.key, dep.title);
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Bound {
    arguments: Option<Value>,
    error: Option<String>,
}

/// The filled-in arguments, or the model's reason they can't be (inner
/// `Err`). The outer `Err` means the reply itself couldn't be used.
pub fn parse_bound(reply: &str) -> Result<Result<Value, String>, String> {
    let b: Bound = json_object(reply, "arguments")?;
    match (b.arguments, b.error) {
        (Some(args), _) if args.is_object() && !args.to_string().contains("{{") => Ok(Ok(args)),
        (Some(_), _) => Err("the arguments still have placeholders".into()),
        (None, Some(e)) => Ok(Err(e)),
        (None, None) => Err("no arguments in the reply".into()),
    }
}

// ---- P7: revisions

pub const REPLAN_PROMPT: &str = "\
A step in an AI agent's plan failed. Revise only the broken part of the plan: keep \
completed steps as they are (you may not remove or change them), replace or change \
the failed step, and add steps if needed. Don't redo work that already succeeded.";

pub fn replan_system_prompt() -> String {
    format!(
        "{REPLAN_PROMPT}\n\n{STEP_FORMAT}\n\nRespond with only a JSON object: \
{{\"remove\": [\"keys of steps to drop\"], \"modify\": [steps with an existing key and new content], \
\"add\": [new steps with new keys; depends_on may name existing steps], \"reason\": \"what changed and why\"}}"
    )
}

pub fn replan_prompt(goal: &Goal, plan: &Plan, failed: &PlanStep, ctx: &PlanningContext) -> String {
    let mut steps = String::new();
    for s in &plan.steps {
        let deps: Vec<&str> =
            s.dependencies.iter().filter_map(|d| plan.step(*d).map(|x| x.key.as_str())).collect();
        steps += &format!(
            "- {} [{}] {} (depends on: {}; action: {})\n",
            s.key,
            s.status,
            s.title,
            if deps.is_empty() { "-".into() } else { deps.join(", ") },
            serde_json::to_string(&s.action).unwrap_or_default()
        );
        if let Some(r) = &s.result
            && s.status == StepStatus::Completed
        {
            steps += &format!("    result: {}\n", truncate(&r.output.to_string(), 300));
        }
    }
    let evidence = failed
        .verification_result
        .as_ref()
        .map(|v| format!("verification: {} {}", v.reason.clone().unwrap_or_default(), v.evidence.join("; ")))
        .unwrap_or_default();
    format!(
        "{}\n\nPlan (version {}):\n{steps}\nFailed step: {} — {}\nError: {}\nFailure class: {}\n{evidence}\n\n{}",
        render_goal(goal),
        plan.version,
        failed.key,
        failed.title,
        failed.last_error.clone().unwrap_or_default(),
        failed.failure_class.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()),
        ctx.render()
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RevisionDraft {
    remove: Vec<String>,
    modify: Vec<StepDraft>,
    add: Vec<StepDraft>,
    reason: String,
}

/// Apply a revision to the plan in place (bumping its version) and return
/// what changed. Completed work is protected; the failed step must be
/// removed or changed; dependents of removed steps move onto the new steps.
pub fn revise(plan: &mut Plan, reply: &str, failed: Uuid, ctx: &PlanningContext) -> Result<PlanRevision, String> {
    let draft: RevisionDraft = json_object(reply, "revision")?;
    let known: HashMap<String, Uuid> = plan.steps.iter().map(|s| (s.key.clone(), s.id)).collect();
    let key_of = |k: &str| known.get(k.trim()).copied().ok_or(format!("no step {k} to change"));

    let removed: Vec<Uuid> = draft.remove.iter().map(|k| key_of(k)).collect::<Result<_, _>>()?;
    let mut modified = Vec::new();
    for d in &draft.modify {
        let id = key_of(&d.key)?;
        let deps = d.depends_on.iter().map(|k| key_of(k)).collect::<Result<Vec<_>, _>>()?;
        modified.push(build_step(d, id, deps, ctx)?);
    }
    let added = build_steps(&draft.add, &known, ctx)?;
    for id in removed.iter().chain(modified.iter().map(|s| &s.id)) {
        if plan.step(*id).is_some_and(|s| s.status == StepStatus::Completed) {
            return Err(format!("the revision changes completed step {}", plan.step(*id).unwrap().key));
        }
    }
    if !removed.contains(&failed) && !modified.iter().any(|m| m.id == failed) {
        return Err("the revision must replace or change the failed step".into());
    }

    let mut next = plan.clone();
    next.steps.retain(|s| !removed.contains(&s.id));
    for m in &modified {
        if let Some(s) = next.step_mut(m.id) {
            let created_at = s.created_at;
            *s = m.clone();
            s.created_at = created_at;
        }
    }
    let new_ids: Vec<Uuid> = added.iter().map(|s| s.id).collect();
    next.steps.extend(added.clone());
    // Whatever depended on a removed step now waits for the new steps instead.
    for s in &mut next.steps {
        if s.dependencies.iter().any(|d| removed.contains(d)) {
            s.dependencies.retain(|d| !removed.contains(d));
            for id in &new_ids {
                if *id != s.id && !s.dependencies.contains(id) {
                    s.dependencies.push(*id);
                }
            }
        }
    }
    graph::validate(&next.steps)?;
    next.version += 1;
    next.updated_at = Utc::now();
    *plan = next;
    Ok(PlanRevision { removed_steps: removed, added_steps: added, modified_steps: modified, reason: draft.reason })
}

// ---- P5: model verification

pub const VERIFY_PROMPT: &str = "\
You check whether a step of an AI agent's plan actually achieved what it was meant \
to, from the evidence alone. Don't assume success: if the evidence doesn't show it, \
it isn't verified.

Respond with only a JSON object: {\"verified\": true, \"evidence\": [\"what in the output \
shows it\"], \"reason\": \"why\"}";

pub fn verify_prompt(step: &PlanStep, check: Option<&str>, output: &str) -> String {
    format!(
        "Step: {}\n{}\nExpected outcome: {}\nWhat must hold: {}\n\nEvidence (step output):\n{}",
        step.title,
        step.description,
        step.expected_outcome.clone().unwrap_or_else(|| "(not stated)".into()),
        check.unwrap_or("(see expected outcome)"),
        truncate(output, 4000)
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct VerdictDraft {
    verified: bool,
    evidence: Vec<String>,
    reason: String,
}

pub fn parse_verification(reply: &str) -> Result<VerificationResult, String> {
    let v: VerdictDraft = json_object(reply, "verification")?;
    Ok(VerificationResult { verified: v.verified, evidence: v.evidence, reason: Some(v.reason).filter(|r| !r.is_empty()) })
}

// ---- P17: goal evaluation

pub const EVALUATE_PROMPT: &str = "\
A plan has finished. Judge the goal, not the plan: for each success criterion, decide \
from the evidence whether it was met. A plan whose steps all completed can still have \
missed the goal.

Respond with only a JSON object: {\"criteria\": [{\"criterion\": \"...\", \"met\": true, \
\"evidence\": \"...\"}], \"summary\": \"what was achieved, in two or three sentences\", \
\"answer\": \"the final result for the user, if the goal asked for one\"}";

pub fn evaluate_prompt(goal: &Goal, plan: &Plan) -> String {
    let mut out = format!("{}\n\nWhat happened:\n", render_goal(goal));
    for s in &plan.steps {
        out += &format!("- {} [{}] {}\n", s.key, s.status, s.title);
        if let Some(r) = &s.result {
            out += &format!("    output: {}\n", truncate(&output_text(&r.output), 1200));
        }
        if let Some(v) = &s.verification_result {
            out += &format!("    verified: {} {}\n", v.verified, v.reason.clone().unwrap_or_default());
        }
    }
    out
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct CriterionResult {
    pub criterion: String,
    pub met: bool,
    pub evidence: String,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default)]
pub struct Evaluation {
    pub criteria: Vec<CriterionResult>,
    pub summary: String,
    pub answer: String,
}

impl Evaluation {
    pub fn status(&self) -> GoalStatus {
        let met = self.criteria.iter().filter(|c| c.met).count();
        match (met, self.criteria.len()) {
            (_, 0) => GoalStatus::Partial,
            (m, n) if m == n => GoalStatus::Completed,
            (0, _) => GoalStatus::Failed,
            _ => GoalStatus::Partial,
        }
    }
}

pub fn parse_evaluation(reply: &str) -> Result<Evaluation, String> {
    json_object(reply, "evaluation")
}

/// A step output as plain text (strings unquoted).
pub fn output_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn tool(name: &str, risk: Risk) -> ToolInfo {
        let parameters = serde_json::json!({"type": "object", "properties": {"id": {"type": "string"}, "query": {"type": "string"}}, "required": ["id"]});
        ToolInfo { name: name.into(), description: format!("{name}."), risk, parameters }
    }

    #[test]
    fn placeholders_are_detected_and_bound() {
        let a = StepAction::Tool { tool: "memory_forget".into(), arguments: serde_json::json!({"id": "{{s3}}"}) };
        assert!(has_placeholders(&a));
        assert!(!has_placeholders(&StepAction::Tool { tool: "x".into(), arguments: serde_json::json!({"id": "abcd1234"}) }));
        assert_eq!(parse_bound(r#"{"arguments":{"id":"84bd0657"}}"#).unwrap().unwrap()["id"], "84bd0657");
        assert!(parse_bound(r#"{"arguments":{"id":"{{s3}}"}}"#).is_err(), "unusable: asked again");
        assert_eq!(parse_bound(r#"{"error":"s3 found nothing"}"#).unwrap().unwrap_err(), "s3 found nothing", "a real answer");
        let rendered = context().render();
        assert!(rendered.contains("Parameters: id*, query"), "{rendered}");
    }

    pub(crate) fn context() -> PlanningContext {
        PlanningContext {
            tools: vec![
                tool("memory_recall", Risk::ReadOnly),
                tool("memory_remember", Risk::Mutating),
                tool("memory_forget", Risk::Destructive),
                tool("shell", Risk::Destructive),
            ],
            agents: vec![AgentInfo { name: "researcher".into(), description: "reads".into(), changes_things: false }],
            skills: vec![("rust-precommit-checks".into(), "before commits".into())],
            forbidden_tools: vec!["shell".into()],
            ..Default::default()
        }
    }

    #[test]
    fn parses_goals_with_success_criteria() {
        let g = parse_goal(
            "Deploy PostgreSQL and verify it",
            r#"{"description":"Deploy PostgreSQL and verify it works","success_criteria":["PostgreSQL is running","accepts connections"],"constraints":[],"destructive":true}"#,
        )
        .unwrap();
        assert_eq!(g.success_criteria.len(), 2);
        assert!(g.destructive);
        assert!(parse_goal("x", r#"{"description":"x","success_criteria":[]}"#).is_err(), "success must be defined");
    }

    const PLAN: &str = r#"{"steps":[
        {"key":"s1","title":"Look up deploy notes","action":{"type":"tool","tool":"memory_recall","arguments":{"query":"deploy"}}},
        {"key":"s2","title":"Look up ports","action":{"type":"subagent","agent":"researcher","task":"find ports"}},
        {"key":"s3","title":"Write summary","depends_on":["s1","s2"],"action":{"type":"reasoning","instruction":"summarize"},"verification":{"strategy":"model_evaluation"}},
        {"key":"s4","title":"Forget old note","depends_on":["s3"],"action":{"type":"tool","tool":"memory_forget","arguments":{"id":"abcd1234"}},"requires_approval":false,"idempotency":"safe"}
    ]}"#;

    #[test]
    fn parses_plans_and_enforces_policy() {
        let steps = parse_plan(PLAN, &context()).unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[2].dependencies, [steps[0].id, steps[1].id]);
        let forget = &steps[3];
        assert_eq!(forget.approval, ApprovalPolicy::RequireApproval, "destructive tools always need approval");
        assert_eq!(forget.idempotency, Idempotency::Unsafe);
        assert_eq!(forget.retry_policy.max_attempts, 1, "unsafe steps aren't retried automatically");
        assert!(forget.operation_id.is_some());
    }

    #[test]
    fn rejects_unknown_forbidden_and_circular_plans() {
        let ctx = context();
        let bad = |json: &str| parse_plan(json, &ctx).unwrap_err();
        assert!(bad(r#"{"steps":[{"key":"s1","action":{"type":"tool","tool":"nope","arguments":{}}}]}"#).contains("unknown tool"));
        assert!(bad(r#"{"steps":[{"key":"s1","action":{"type":"tool","tool":"shell","arguments":{}}}]}"#).contains("may not use"));
        assert!(bad(r#"{"steps":[{"key":"s1","action":{"type":"workflow","workflow":"nope","input":{}}}]}"#).contains("unknown workflow"));
        assert!(bad(r#"{"steps":[{"key":"s1","depends_on":["s2"],"action":{"type":"reasoning","instruction":"a"}},{"key":"s2","depends_on":["s1"],"action":{"type":"reasoning","instruction":"b"}}]}"#).contains("circular"));
        assert!(bad(r#"{"steps":[{"key":"s1","depends_on":["s9"],"action":{"type":"reasoning","instruction":"a"}}]}"#).contains("unknown step"));
        assert!(bad(r#"{"steps":[{"key":"s1","action":{"type":"reasoning","instruction":"a"},"verification":{"strategy":"follow_up_tool","tool":"memory_remember"}}]}"#).contains("read-only"));
        assert!(bad(r#"{"steps":[]}"#).contains("no steps"));
    }

    pub(crate) fn plan_from(steps: Vec<PlanStep>) -> Plan {
        let now = Utc::now();
        Plan {
            id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            version: 1,
            status: PlanStatus::Running,
            steps,
            budget: Budget::default(),
            usage: BudgetUsage::default(),
            note: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn revisions_replace_only_the_broken_part() {
        // A → B → C → D, C failed.
        let reply = r#"{"steps":[
            {"key":"s1","title":"A","action":{"type":"reasoning","instruction":"a"}},
            {"key":"s2","title":"B","depends_on":["s1"],"action":{"type":"reasoning","instruction":"b"}},
            {"key":"s3","title":"C","depends_on":["s2"],"action":{"type":"reasoning","instruction":"c"}},
            {"key":"s4","title":"D","depends_on":["s3"],"action":{"type":"reasoning","instruction":"d"}}]}"#;
        let ctx = context();
        let mut plan = plan_from(parse_plan(reply, &ctx).unwrap());
        plan.steps[0].status = StepStatus::Completed;
        plan.steps[1].status = StepStatus::Completed;
        plan.steps[2].status = StepStatus::Failed;
        let (b, c, d) = (plan.steps[1].id, plan.steps[2].id, plan.steps[3].id);

        let revision = revise(
            &mut plan,
            r#"{"remove":["s3"],"add":[
                {"key":"c2","title":"C2","depends_on":["s2"],"action":{"type":"reasoning","instruction":"c2"}},
                {"key":"c3","title":"C3","depends_on":["c2"],"action":{"type":"reasoning","instruction":"c3"}}],
               "reason":"C needed two smaller steps"}"#,
            c,
            &ctx,
        )
        .unwrap();
        assert_eq!(plan.version, 2);
        assert_eq!(revision.removed_steps, [c]);
        assert_eq!(revision.added_steps.len(), 2);
        assert_eq!(plan.steps.iter().filter(|s| s.status == StepStatus::Completed).count(), 2, "completed work kept");
        let d_step = plan.step(d).unwrap();
        assert!(d_step.dependencies.iter().all(|x| revision.added_steps.iter().any(|a| a.id == *x)), "D now waits for C2 and C3");
        assert_eq!(plan.step(revision.added_steps[0].id).unwrap().dependencies, [b]);
    }

    #[test]
    fn revisions_may_not_touch_completed_steps_or_ignore_the_failure() {
        let ctx = context();
        let reply = r#"{"steps":[
            {"key":"s1","action":{"type":"reasoning","instruction":"a"}},
            {"key":"s2","depends_on":["s1"],"action":{"type":"reasoning","instruction":"b"}}]}"#;
        let mut plan = plan_from(parse_plan(reply, &ctx).unwrap());
        plan.steps[0].status = StepStatus::Completed;
        plan.steps[1].status = StepStatus::Failed;
        let failed = plan.steps[1].id;
        let before = plan.clone();
        assert!(revise(&mut plan, r#"{"remove":["s1","s2"]}"#, failed, &ctx).unwrap_err().contains("completed"));
        assert!(revise(&mut plan, r#"{"add":[{"key":"x","action":{"type":"reasoning","instruction":"x"}}]}"#, failed, &ctx).unwrap_err().contains("failed step"));
        assert_eq!(plan.version, before.version, "a rejected revision changes nothing");

        let ok = revise(&mut plan, r#"{"modify":[{"key":"s2","depends_on":["s1"],"action":{"type":"reasoning","instruction":"b, differently"}}]}"#, failed, &ctx).unwrap();
        assert_eq!(ok.modified_steps.len(), 1);
        assert_eq!(plan.step(failed).unwrap().status, StepStatus::Pending, "the changed step runs again");
    }

    #[test]
    fn evaluations_judge_the_goal() {
        let e = parse_evaluation(r#"{"criteria":[{"criterion":"a","met":true},{"criterion":"b","met":false}],"summary":"half"}"#).unwrap();
        assert_eq!(e.status(), GoalStatus::Partial, "a finished plan can still miss the goal");
        let all = parse_evaluation(r#"{"criteria":[{"criterion":"a","met":true}],"summary":"done"}"#).unwrap();
        assert_eq!(all.status(), GoalStatus::Completed);
        let v = parse_verification(r#"{"verified":false,"evidence":[],"reason":"no output"}"#).unwrap();
        assert!(!v.verified);
    }
}
