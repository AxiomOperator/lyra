//! What the goal manager asks the model: breaking a goal into subgoals (G3)
//! and reviewing the whole set (G12), plus the request a plan for a goal
//! starts from (G4). No network code: lyra makes the calls.

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::model::{Goal, GoalPlan, GoalStatus, Origin};

pub const DECOMPOSE_PROMPT: &str = "\
You break an AI assistant's long-term goal into subgoals: the distinct pieces of work \
that together achieve it, each something one or a few work sessions can finish and \
check. Use 2 to 8 subgoals, in a sensible order; say which earlier subgoals each one \
needs first (by number). Give each 1-3 concrete success criteria.

Respond with only a JSON object: {\"subgoals\": [{\"title\": \"...\", \"description\": \"...\", \
\"success_criteria\": [\"...\"], \"depends_on\": [1], \"priority\": 5}]}";

pub fn decompose_prompt(goal: &Goal, context: &str) -> String {
    let mut out = format!("Goal: {}\n", goal.title);
    if !goal.description.is_empty() {
        out += &format!("Details: {}\n", goal.description);
    }
    if !goal.success_criteria.is_empty() {
        out += &format!("Done when: {}\n", goal.success_criteria.join("; "));
    }
    if !context.is_empty() {
        out += &format!("\nWhat the assistant can do and knows:\n{context}\n");
    }
    out
}

#[derive(Deserialize)]
struct SubgoalDraft {
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    success_criteria: Vec<String>,
    #[serde(default)]
    depends_on: Vec<usize>,
    priority: Option<u8>,
}

#[derive(Deserialize)]
struct Decomposition {
    subgoals: Vec<SubgoalDraft>,
}

/// The JSON object in a model reply.
fn json_object<T: for<'de> Deserialize<'de>>(reply: &str, what: &str) -> Result<T, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, after)| after);
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        return Err(format!("no JSON in the {what} reply"));
    };
    if end < start {
        return Err(format!("malformed {what} reply"));
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("bad {what} JSON: {e}"))
}

/// Subgoals with, for each, the indexes (0-based) of earlier ones it waits for.
pub fn parse_decomposition(reply: &str, parent: &Goal) -> Result<Vec<(Goal, Vec<usize>)>, String> {
    let d: Decomposition = json_object(reply, "decomposition")?;
    if d.subgoals.is_empty() || d.subgoals.len() > 12 {
        return Err("expected 1 to 12 subgoals".into());
    }
    Ok(d
        .subgoals
        .into_iter()
        .enumerate()
        .filter(|(_, s)| !s.title.trim().is_empty())
        .map(|(i, s)| {
            let mut g = Goal::new(&s.title, &s.description);
            g.success_criteria = s.success_criteria.into_iter().filter(|c| !c.trim().is_empty()).collect();
            g.priority = s.priority.unwrap_or(parent.priority).min(10);
            // Subgoals of the user's goal are part of it, not proposals.
            g.origin = parent.origin;
            g.status = if parent.status == GoalStatus::Proposed { GoalStatus::Proposed } else { GoalStatus::Active };
            // 1-based numbers in the reply, earlier subgoals only.
            let deps = s.depends_on.into_iter().filter(|n| *n >= 1 && *n <= i).map(|n| n - 1).collect();
            (g, deps)
        })
        .collect())
}

pub const REVIEW_PROMPT: &str = "\
You review an AI assistant's list of long-term goals and suggest how to tidy it: \
merge goals that are really the same, cancel ones that no longer make sense, mark ones \
whose progress shows they're done as completed, and adjust priorities that look wrong \
(0-10). Only suggest changes the list clearly supports.

Respond with only a JSON object: {\"merge\": [{\"into\": \"id\", \"goals\": [\"id\"], \"reason\": \"...\"}], \
\"cancel\": [{\"goal\": \"id\", \"reason\": \"...\"}], \"complete\": [{\"goal\": \"id\", \"reason\": \"...\"}], \
\"priority\": [{\"goal\": \"id\", \"priority\": 7, \"reason\": \"...\"}]}";

pub fn review_prompt(goals: &[Goal], stale: &[Uuid]) -> String {
    let mut out = String::from("Goals:\n");
    for g in goals.iter().filter(|g| g.status.is_open()) {
        out += &format!(
            "- [{}] {} ({}, priority {}, {:.0}% done{}{}): {}{}\n",
            g.short(),
            g.title,
            g.status,
            g.priority,
            g.progress * 100.0,
            if stale.contains(&g.id) { ", untouched for a long time" } else { "" },
            g.parent_goal_id.map_or(String::new(), |p| format!(", part of {}", &p.to_string()[..8])),
            g.description,
            if g.progress_detail.summary.is_empty() { String::new() } else { format!(" — progress: {}", g.progress_detail.summary) }
        );
    }
    out
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Review {
    pub merge: Vec<Merge>,
    pub cancel: Vec<Reasoned>,
    pub complete: Vec<Reasoned>,
    pub priority: Vec<Reprioritize>,
}

#[derive(Debug, Deserialize)]
pub struct Merge {
    pub into: String,
    pub goals: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct Reasoned {
    pub goal: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct Reprioritize {
    pub goal: String,
    pub priority: u8,
    #[serde(default)]
    pub reason: String,
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.merge.is_empty() && self.cancel.is_empty() && self.complete.is_empty() && self.priority.is_empty()
    }
}

pub fn parse_review(reply: &str) -> Result<Review, String> {
    json_object(reply, "review")
}

/// The request a plan for this goal starts from (G4): the goal, what's done,
/// what earlier plans found, and what remains.
pub fn plan_request(goal: &Goal, plans: &[GoalPlan], done_subgoals: &[String], parent: Option<&Goal>) -> String {
    let mut out = format!("Work toward this long-term goal: {}", goal.title);
    if !goal.description.is_empty() {
        out += &format!("\n{}", goal.description);
    }
    if let Some(p) = parent {
        out += &format!("\nIt is part of the larger goal: {}", p.title);
    }
    if !goal.success_criteria.is_empty() {
        out += &format!("\nIt's done when: {}", goal.success_criteria.join("; "));
    }
    if !done_subgoals.is_empty() {
        out += &format!("\nAlready done: {}", done_subgoals.join("; "));
    }
    let earlier: Vec<String> = plans
        .iter()
        .filter_map(|p| Some(format!("attempt {} {}: {}", p.attempt, p.outcome.as_deref()?, p.summary.as_deref().unwrap_or(""))))
        .collect();
    if !earlier.is_empty() {
        out += &format!("\nEarlier work sessions:\n- {}", earlier.join("\n- "));
    }
    if !goal.progress_detail.summary.is_empty() {
        out += &format!("\nWhere it stands: {}", goal.progress_detail.summary);
    }
    out += "\nDo the next useful part of it in this session; it doesn't all have to be finished now.";
    out
}

/// A goal from the model's tool call (`goal_create`): proposed, not active.
pub fn agent_goal(args: &Value) -> Result<Goal, String> {
    let title = args["title"].as_str().filter(|t| !t.trim().is_empty()).ok_or("a goal needs a title")?;
    let mut g = Goal::new(title, args["description"].as_str().unwrap_or(""));
    g.success_criteria = args["success_criteria"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(str::to_string)).collect();
    if let Some(p) = args["priority"].as_u64() {
        g.priority = p.min(10) as u8;
    }
    g.origin = Origin::Agent;
    g.status = GoalStatus::Proposed;
    Ok(g)
}
