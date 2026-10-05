//! Lyra's side of long-lived goals (docs/done/goal_manager.md): the
//! `/goals` and `/goal` commands, turning a goal into a plan and a plan's
//! outcome back into progress, the conditions event triggers watch, and the
//! autonomy session: what may run without the user, and when it must stop.

use std::sync::Mutex;
use std::time::Instant;

use chrono::Utc;
use lyra_execution::{GoalStatus as PlanGoalStatus, PlanStatus, RunOutcome};
use lyra_goals::prompts;
use lyra_goals::{
    AutonomyMode, BlockerType, Goal, GoalManager, GoalStatus, PlanResult, PriorityScore, Trigger, parse_duration, parse_when,
};

/// One autonomous working session's spending (G11).
#[derive(Debug, Default, Clone)]
pub struct Session {
    pub started: Option<Instant>,
    pub plans: u32,
    pub tool_calls: u32,
    pub model_calls: u32,
    pub replans: u32,
    pub cost: f64,
    /// Why the last session stopped, and when.
    pub stopped: Option<(Instant, String)>,
}

pub struct Goals {
    pub manager: GoalManager,
    /// The mode in effect (config, or `/goals autonomy` for this run).
    mode: Mutex<AutonomyMode>,
    pub session: Mutex<Session>,
    /// The last review's suggestions, waiting for `/goals review apply`.
    pub review: Mutex<Option<prompts::Review>>,
}

/// What the Goals panel shows.
pub struct GoalsSnapshot {
    pub ranked: Vec<(Goal, PriorityScore, Option<String>)>,
    pub open: usize,
    pub mode: AutonomyMode,
    pub session: Session,
}

fn bar(progress: f32) -> String {
    let filled = (progress.clamp(0.0, 1.0) * 10.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

fn icon(s: GoalStatus) -> &'static str {
    match s {
        GoalStatus::Proposed => "?",
        GoalStatus::Active => "▸",
        GoalStatus::Blocked => "⏸",
        GoalStatus::Paused => "‖",
        GoalStatus::Completed => "✓",
        GoalStatus::Failed => "✗",
        GoalStatus::Cancelled => "⊘",
    }
}

impl Goals {
    pub fn new(manager: GoalManager) -> Self {
        let mode = manager.settings.autonomy.mode;
        Self { manager, mode: Mutex::new(mode), session: Mutex::new(Session::default()), review: Mutex::new(None) }
    }

    pub fn mode(&self) -> AutonomyMode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_mode(&self, mode: AutonomyMode) {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
        // A new mode starts a fresh session.
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Session::default();
    }

    pub fn snapshot(&self) -> Result<GoalsSnapshot, String> {
        let ranked = self.manager.ranked()?;
        let open = ranked.len();
        let ranked = ranked
            .into_iter()
            .take(8)
            .map(|(g, s)| {
                let blocker = self.manager.blockers(Some(g.id), true).ok().and_then(|b| b.first().map(|b| b.reason.clone()));
                (g, s, blocker)
            })
            .collect();
        Ok(GoalsSnapshot { ranked, open, mode: self.mode(), session: self.session.lock().unwrap_or_else(|e| e.into_inner()).clone() })
    }

    /// Short lines for the system prompt, so the model knows what's being
    /// worked toward and can answer "how far are we on …".
    pub fn prompt_section(&self) -> Option<String> {
        let ranked = self.manager.ranked().ok()?;
        let lines: Vec<String> = ranked
            .iter()
            .filter(|(g, _)| g.status != GoalStatus::Proposed)
            .take(6)
            .map(|(g, _)| {
                let summary = if g.progress_detail.summary.is_empty() { String::new() } else { format!(" — {}", g.progress_detail.summary) };
                format!("- [{}] {} ({}, {:.0}%){summary}", g.short(), g.title, g.status, g.progress * 100.0)
            })
            .collect();
        (!lines.is_empty()).then(|| {
            format!(
                "# Goals\n\nLong-term goals you're working toward (goal_get has details; goal_note records progress):\n{}",
                lines.join("\n")
            )
        })
    }

    // ---- G11: autonomy

    /// Whether an autonomous session may start or continue now; starts one
    /// if needed. `Err` says why not.
    pub fn may_work(&self, cooldown_minutes: u64) -> Result<(), String> {
        if self.mode() == AutonomyMode::Reactive {
            return Err("autonomy is reactive".into());
        }
        let mut s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, why)) = &s.stopped {
            if at.elapsed().as_secs() < cooldown_minutes * 60 {
                return Err(format!("stopped ({why}); cooling down"));
            }
            *s = Session::default();
        }
        if s.started.is_none() {
            s.started = Some(Instant::now());
        }
        let p = &self.manager.settings.autonomy;
        let limit = if s.started.is_some_and(|t| t.elapsed().as_secs() >= p.max_runtime_minutes * 60) {
            Some(format!("{} minutes of work", p.max_runtime_minutes))
        } else if s.plans >= p.max_plans {
            Some(format!("{} plans", p.max_plans))
        } else if s.tool_calls >= p.max_tool_calls {
            Some(format!("{} tool calls", p.max_tool_calls))
        } else if s.model_calls >= p.max_model_calls {
            Some(format!("{} model calls", p.max_model_calls))
        } else if s.replans >= p.max_replans {
            Some(format!("{} replans", p.max_replans))
        } else if p.max_cost > 0.0 && s.cost >= p.max_cost {
            Some(format!("a cost of {}", p.max_cost))
        } else {
            None
        };
        if let Some(why) = limit {
            s.stopped = Some((Instant::now(), format!("reached {why}")));
            return Err(format!("autonomy stopped: reached {why}"));
        }
        Ok(())
    }

    /// What's left of the session, as a plan budget.
    pub fn remaining_budget(&self, base: lyra_execution::Budget) -> lyra_execution::Budget {
        let p = &self.manager.settings.autonomy;
        let s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let left = |max: u32, used: u32| max.saturating_sub(used).max(1);
        let minutes = s.started.map_or(p.max_runtime_minutes, |t| p.max_runtime_minutes.saturating_sub(t.elapsed().as_secs() / 60)).max(1);
        lyra_execution::Budget {
            max_model_calls: Some(base.max_model_calls.unwrap_or(u32::MAX).min(left(p.max_model_calls, s.model_calls))),
            max_tool_calls: Some(base.max_tool_calls.unwrap_or(u32::MAX).min(left(p.max_tool_calls, s.tool_calls))),
            max_replans: Some(base.max_replans.unwrap_or(u32::MAX).min(p.max_replans.saturating_sub(s.replans))),
            max_minutes: Some(base.max_minutes.unwrap_or(u32::MAX).min(minutes as u32)),
            max_tokens: base.max_tokens,
        }
    }

    /// Add an autonomous plan's spending to the session.
    pub fn spent(&self, model_calls: u32, tool_calls: u32, replans: u32, cost: f64) {
        let mut s = self.session.lock().unwrap_or_else(|e| e.into_inner());
        s.plans += 1;
        s.model_calls += model_calls;
        s.tool_calls += tool_calls;
        s.replans += replans;
        s.cost += cost;
    }

    /// The riskiest capability autonomous plans may use, and from which
    /// level on a person approves (assisted: every meaningful write).
    pub fn guard(&self) -> (lyra_capabilities::RiskLevel, Option<lyra_capabilities::RiskLevel>) {
        use lyra_capabilities::RiskLevel;
        let max = serde_json::from_value(serde_json::json!(self.manager.settings.autonomy.max_risk)).unwrap_or(RiskLevel::Write);
        let approval = (self.mode() == AutonomyMode::Assisted).then_some(RiskLevel::Write);
        (max, approval)
    }

    // ---- G4, G5: plans and progress

    /// The request for a plan toward a goal.
    pub fn request(&self, g: &Goal) -> Result<String, String> {
        let plans = self.manager.plans(g.id)?;
        let done: Vec<String> = self.manager.children(g.id)?.into_iter().filter(|c| c.status == GoalStatus::Completed).map(|c| c.title).collect();
        let parent = match g.parent_goal_id {
            Some(p) => self.manager.get(p)?,
            None => None,
        };
        Ok(prompts::plan_request(g, &plans, &done, parent.as_ref()))
    }

    /// A plan ended or paused: if it was for a goal, update the goal (G5, G6).
    pub fn plan_finished(&self, outcome: &RunOutcome, tokens: u64) -> Result<Vec<String>, String> {
        let plan = &outcome.plan;
        let r = PlanResult {
            outcome: if plan.status == PlanStatus::Paused {
                "paused".into()
            } else {
                match outcome.goal_status {
                    Some(PlanGoalStatus::Completed) => "completed",
                    Some(PlanGoalStatus::Partial) => "partial",
                    Some(PlanGoalStatus::Cancelled) => "cancelled",
                    _ => "failed",
                }
                .into()
            },
            summary: outcome.evaluation.as_ref().map(|e| e.summary.clone()).or(plan.note.clone()).unwrap_or_default(),
            criteria: outcome.evaluation.as_ref().map(|e| (e.criteria.iter().filter(|c| c.met).count() as u32, e.criteria.len() as u32)),
            tokens,
            blocker: (plan.status == PlanStatus::Paused).then(|| blocker_for(plan.note.as_deref().unwrap_or(""))),
        };
        self.manager.plan_finished(plan.id, &r)
    }
}

/// What a paused plan's note says is in the way.
fn blocker_for(note: &str) -> (BlockerType, String) {
    let n = note.to_lowercase();
    let kind = if n.contains("approval") || n.contains("approve") {
        BlockerType::ApprovalRequired
    } else if n.contains("open question") || n.contains("ambigu") {
        BlockerType::MissingInformation
    } else if n.contains("permission") || n.contains("not allowed") || n.contains("denied") {
        BlockerType::MissingPermission
    } else if n.contains("unavailable") {
        BlockerType::CapabilityUnavailable
    } else {
        BlockerType::ExternalDependency
    };
    (kind, if note.is_empty() { "the plan is waiting".into() } else { note.to_string() })
}

/// Is a trigger condition true now? `env:VAR`, `file:/path`, `capability:<id or source.*>`.
pub fn condition(c: &str, caps: Option<&crate::caps::Caps>) -> bool {
    if let Some(var) = c.strip_prefix("env:") {
        return std::env::var(var.trim()).is_ok_and(|v| !v.is_empty());
    }
    if let Some(path) = c.strip_prefix("file:") {
        return crate::config::expand_path(path.trim()).exists();
    }
    if let Some(key) = c.strip_prefix("capability:") {
        let Some(caps) = caps else { return false };
        let key = key.trim();
        let all = caps.manager.all();
        let matching: Vec<_> = all.iter().filter(|x| x.id == key || x.name == key || key.strip_suffix(".*").is_some_and(|s| x.source == s)).collect();
        return !matching.is_empty()
            && matching.iter().all(|x| caps.manager.health(x) == lyra_capabilities::CapabilityHealth::Healthy);
    }
    false
}

// ---- commands

pub const COMMANDS: &str = "\
/goals [all]                 long-term goals by priority (score, progress, blockers)
/goals next                  what to work on next, and why
/goals review [apply]        tidy the list: merges, cancellations, completions, priorities
/goals autonomy [reactive|assisted|autonomous]   what may run without you, and its session
/goal new <title> [-- what]  a new goal · /goal <id> its details, plans, blockers, events
/goal decompose <id>         break it into subgoals · /goal work <id> plan and work on it now
/goal activate|pause|cancel|complete|fail <id> [why]
/goal priority <id> <0-10> · importance <id> <0-1> · due <id> <in 3d|date|none>
/goal criteria <id> <text> · depends <id> <other> · block <id> <type> <why> · unblock <id>
/goal when <id> <at <time>|every 1d|after <goal>|env:VAR|file:path|capability:id>";

pub fn list(goals: &Goals, all: bool) -> Result<String, String> {
    let m = &goals.manager;
    let ranked = m.ranked()?;
    let mut out = Vec::new();
    if ranked.is_empty() && !all {
        out.push("no open goals — /goal new <title>".to_string());
    }
    for (g, s) in &ranked {
        let blocker = m.blockers(Some(g.id), true)?.first().map(|b| format!(" — blocked: {}", b.reason)).unwrap_or_default();
        let due = g.due_at.map_or(String::new(), |d| format!(" · due {}", d.with_timezone(&chrono::Local).format("%m-%d")));
        let indent = if g.parent_goal_id.is_some() { "  " } else { "" };
        out.push(format!(
            "{indent}{} {} {} {} {:.0}% · priority {} · score {:.2}{due}{blocker}",
            icon(g.status),
            g.short(),
            g.title,
            bar(g.progress),
            g.progress * 100.0,
            g.priority,
            s.total
        ));
    }
    if all {
        for g in m.all()?.iter().filter(|g| !g.status.is_open()) {
            out.push(format!("{} {} {} ({})", icon(g.status), g.short(), g.title, g.status));
        }
    }
    out.push(format!("autonomy: {}", goals.mode().as_str()));
    Ok(out.join("\n"))
}

pub fn next(goals: &Goals) -> Result<String, String> {
    let existing = goals.mode() == AutonomyMode::Assisted;
    match goals.manager.next(existing)? {
        Some((g, s)) => Ok(format!(
            "next: {} {} — score {:.2} (priority {:.2} · deadline {:.2} · unblocks others {:.2} · importance {:.2} · progress {:.2} · cost −{:.2})",
            g.short(),
            g.title,
            s.total,
            s.explicit,
            s.deadline,
            s.dependency,
            s.importance,
            s.progress,
            s.cost
        )),
        None => Ok("nothing can be worked on now (blocked, waiting on others, or already running)".into()),
    }
}

pub fn show(goals: &Goals, key: &str) -> Result<String, String> {
    let m = &goals.manager;
    let g = m.find(key)?;
    let mut out = vec![
        format!("{} {} [{}] — {}", icon(g.status), g.title, g.short(), g.status),
        format!(
            "priority {} · importance {:.1} · {}{}",
            g.priority,
            g.importance,
            g.due_at.map_or("no deadline".into(), |d| format!("due {}", d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"))),
            if g.origin == lyra_goals::Origin::Agent { " · proposed by lyra" } else { "" }
        ),
    ];
    if !g.description.is_empty() {
        out.push(g.description.clone());
    }
    if !g.success_criteria.is_empty() {
        out.push(format!("done when: {}", g.success_criteria.join("; ")));
    }
    let d = &g.progress_detail;
    out.push(format!(
        "progress {} {:.0}%{}{}",
        bar(g.progress),
        g.progress * 100.0,
        d.total_items.map_or(String::new(), |t| format!(" · {}/{t}", d.completed_items)),
        if d.summary.is_empty() { String::new() } else { format!(" — {}", d.summary) }
    ));
    if let Some(p) = g.parent_goal_id.and_then(|p| m.get(p).ok().flatten()) {
        out.push(format!("part of: {} [{}]", p.title, p.short()));
    }
    let kids = m.children(g.id)?;
    if !kids.is_empty() {
        out.push("subgoals:".into());
        out.extend(kids.iter().map(|k| format!("  {} {} {} {:.0}%", icon(k.status), k.short(), k.title, k.progress * 100.0)));
    }
    for dep in &g.dependencies {
        if let Some(d) = m.get(*dep)? {
            out.push(format!("waits for: {} [{}] ({})", d.title, d.short(), d.status));
        }
    }
    for b in m.blockers(Some(g.id), true)? {
        out.push(format!("blocked: {} — {}", b.blocker_type.as_str(), b.reason));
    }
    for t in m.triggers(Some(g.id))?.iter().filter(|t| t.active) {
        out.push(format!("wakes {}", t.trigger.describe()));
    }
    let plans = m.plans(g.id)?;
    if !plans.is_empty() {
        out.push("plans:".into());
        out.extend(plans.iter().map(|p| {
            format!(
                "  #{} {} {}{} — {}",
                p.attempt,
                &p.plan_id.to_string()[..8],
                p.outcome.as_deref().unwrap_or("running"),
                if p.autonomous { " (autonomous)" } else { "" },
                p.summary.as_deref().unwrap_or("")
            )
        }));
    }
    out.push("events:".into());
    out.extend(m.events(Some(g.id), 8)?.iter().rev().map(|e| {
        format!("  {} {}: {}", e.created_at.with_timezone(&chrono::Local).format("%m-%d %H:%M"), e.kind, e.message.chars().take(140).collect::<String>())
    }));
    Ok(out.join("\n"))
}

/// `/goal new <title> [-- description]`.
pub fn create(goals: &Goals, text: &str) -> Result<String, String> {
    let (title, description) = text.split_once(" -- ").unwrap_or((text, ""));
    if title.trim().is_empty() {
        return Err("usage: /goal new <title> [-- description]".into());
    }
    let g = goals.manager.create(Goal::new(title, description))?;
    Ok(format!("goal {} created: {} — /goal decompose {} to break it down, /goal work {} to start", g.short(), g.title, g.short(), g.short()))
}

/// The commands that change a goal's fields or status.
pub fn edit(goals: &Goals, sub: &str, rest: &str) -> Result<String, String> {
    let m = &goals.manager;
    let (key, arg) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    let arg = arg.trim();
    let mut g = m.find(key)?;
    let why = if arg.is_empty() { "by the user" } else { arg };
    match sub {
        "activate" | "resume" => m.set_status(g.id, GoalStatus::Active, why).map(|g| format!("{} is active", g.title)),
        "pause" => m.set_status(g.id, GoalStatus::Paused, why).map(|g| format!("{} is paused", g.title)),
        "cancel" => m.set_status(g.id, GoalStatus::Cancelled, why).map(|g| format!("{} is cancelled", g.title)),
        "complete" => m.set_status(g.id, GoalStatus::Completed, why).map(|g| format!("🎯 {} is completed", g.title)),
        "fail" => m.set_status(g.id, GoalStatus::Failed, why).map(|g| format!("{} failed", g.title)),
        "unblock" => m.unblock(g.id, why).map(|g| format!("{} is unblocked", g.title)),
        "priority" => {
            g.priority = arg.parse::<u8>().map_err(|_| "usage: /goal priority <id> <0-10>")?.min(10);
            m.update(&g, &format!("priority {}", g.priority)).map(|_| format!("{} now has priority {}", g.title, g.priority))
        }
        "importance" => {
            g.importance = arg.parse::<f32>().map_err(|_| "usage: /goal importance <id> <0-1>")?.clamp(0.0, 1.0);
            m.update(&g, &format!("importance {:.1}", g.importance)).map(|_| format!("{} now has importance {:.1}", g.title, g.importance))
        }
        "due" => {
            g.due_at = if arg == "none" { None } else { Some(parse_when(arg, Utc::now())?) };
            let shown = g.due_at.map_or("no deadline".into(), |d| format!("due {}", d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")));
            m.update(&g, &shown).map(|_| format!("{}: {shown}", g.title))
        }
        "criteria" => {
            if arg.is_empty() {
                return Err("usage: /goal criteria <id> <what must be true>".into());
            }
            g.success_criteria.push(arg.to_string());
            m.update(&g, &format!("criterion: {arg}")).map(|_| format!("{} is done when: {}", g.title, g.success_criteria.join("; ")))
        }
        "depends" => {
            let other = m.find(arg)?;
            m.add_dependency(g.id, other.id).map(|_| format!("{} now waits for {}", g.title, other.title))
        }
        "block" => {
            let (kind, reason) = arg.split_once(' ').ok_or("usage: /goal block <id> <type> <why>")?;
            let kind: BlockerType = kind.parse()?;
            m.block(g.id, kind, reason.trim()).map(|_| format!("{} is blocked: {}", g.title, reason.trim()))
        }
        "when" => {
            let trigger = parse_trigger(goals, arg)?;
            let t = m.add_trigger(g.id, trigger)?;
            Ok(format!("{} wakes {}", g.title, t.trigger.describe()))
        }
        _ => Err(format!("unknown /goal command {sub:?}\n{COMMANDS}")),
    }
}

fn parse_trigger(goals: &Goals, text: &str) -> Result<Trigger, String> {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("at ") {
        return Ok(Trigger::At { at: parse_when(rest, Utc::now())? });
    }
    if let Some(rest) = t.strip_prefix("every ") {
        return Ok(Trigger::Every { minutes: parse_duration(rest)?.num_minutes().max(1) as u64 });
    }
    if let Some(rest) = t.strip_prefix("after ") {
        return Ok(Trigger::After { goal: goals.manager.find(rest)?.id });
    }
    if ["env:", "file:", "capability:", "goal:"].iter().any(|p| t.starts_with(p)) {
        return Ok(Trigger::When { condition: t.to_string() });
    }
    Err("usage: /goal when <id> <at <time>|every 1d|after <goal>|env:VAR|file:path|capability:id>".into())
}

/// Apply a review's suggestions (G12). Merged goals are cancelled with a note
/// (their history stays); returns what was done.
pub fn apply_review(goals: &Goals, r: &prompts::Review) -> Vec<String> {
    let m = &goals.manager;
    let mut notes = Vec::new();
    for merge in &r.merge {
        let Ok(into) = m.find(&merge.into) else { continue };
        for other in merge.goals.iter().filter_map(|k| m.find(k).ok()).filter(|o| o.id != into.id) {
            let why = format!("merged into {} [{}]: {}", into.title, into.short(), merge.reason);
            if m.set_status(other.id, GoalStatus::Cancelled, &why).is_ok() {
                notes.push(format!("merged {} into {}", other.title, into.title));
            }
        }
    }
    for c in &r.cancel {
        if let Ok(g) = m.find(&c.goal)
            && m.set_status(g.id, GoalStatus::Cancelled, &format!("review: {}", c.reason)).is_ok()
        {
            notes.push(format!("cancelled {}: {}", g.title, c.reason));
        }
    }
    for c in &r.complete {
        if let Ok(g) = m.find(&c.goal)
            && m.set_status(g.id, GoalStatus::Completed, &format!("review: {}", c.reason)).is_ok()
        {
            notes.push(format!("completed {}: {}", g.title, c.reason));
        }
    }
    for p in &r.priority {
        if let Ok(mut g) = m.find(&p.goal) {
            g.priority = p.priority.min(10);
            if m.update(&g, &format!("review: priority {} — {}", g.priority, p.reason)).is_ok() {
                notes.push(format!("{} → priority {}", g.title, g.priority));
            }
        }
    }
    notes
}

/// The review's suggestions, as text.
pub fn describe_review(goals: &Goals, r: &prompts::Review) -> String {
    let name = |k: &str| goals.manager.find(k).map_or(k.to_string(), |g| g.title);
    let mut out = Vec::new();
    for m in &r.merge {
        out.push(format!("merge {} into {} — {}", m.goals.iter().map(|k| name(k)).collect::<Vec<_>>().join(", "), name(&m.into), m.reason));
    }
    out.extend(r.cancel.iter().map(|c| format!("cancel {} — {}", name(&c.goal), c.reason)));
    out.extend(r.complete.iter().map(|c| format!("complete {} — {}", name(&c.goal), c.reason)));
    out.extend(r.priority.iter().map(|p| format!("priority {} for {} — {}", p.priority, name(&p.goal), p.reason)));
    if out.is_empty() { "the goals look fine".into() } else { out.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_goals::{GoalStore, Settings};

    fn goals(settings: Settings) -> (tokio::runtime::Runtime, Goals) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let store = rt.block_on(GoalStore::in_memory()).unwrap();
        let m = GoalManager::with_store(store, rt.handle().clone(), settings);
        (rt, Goals::new(m))
    }

    #[test]
    fn autonomy_sessions_stop_at_their_limits_and_cool_down() {
        let mut s = Settings::default();
        s.autonomy.max_plans = 2;
        s.autonomy.max_tool_calls = 10;
        let (_rt, g) = goals(s);
        assert!(g.may_work(60).unwrap_err().contains("reactive"));
        g.set_mode(AutonomyMode::Autonomous);
        assert!(g.may_work(60).is_ok());
        g.spent(3, 4, 0, 0.0);
        let b = g.remaining_budget(lyra_execution::Budget::default());
        assert_eq!(b.max_tool_calls, Some(6), "what's left of the session");
        g.spent(3, 4, 0, 0.0);
        assert!(g.may_work(60).unwrap_err().contains("2 plans"));
        assert!(g.may_work(60).unwrap_err().contains("cooling down"), "and it stays stopped");
        assert!(g.may_work(0).is_ok(), "a new session after the cooldown");
        let (max, approval) = g.guard();
        assert_eq!((max, approval), (lyra_capabilities::RiskLevel::Write, None));
        g.set_mode(AutonomyMode::Assisted);
        assert_eq!(g.guard().1, Some(lyra_capabilities::RiskLevel::Write), "assisted: writes need approval");
    }

    #[test]
    fn commands_conditions_and_reviews() {
        let (_rt, g) = goals(Settings::default());
        assert!(create(&g, "Integrate PMI -- tasks and projects").unwrap().contains("created"));
        create(&g, "Write the docs").unwrap();
        assert!(edit(&g, "priority", "pmi 9").unwrap().contains("priority 9"));
        assert!(edit(&g, "due", "pmi in 2d").unwrap().contains("due"));
        assert!(edit(&g, "when", "docs after pmi").unwrap().contains("after goal"));
        assert!(edit(&g, "when", "pmi env:LYRA_TEST_GOAL_TOKEN").unwrap().contains("when env:"));
        assert!(edit(&g, "block", "pmi missing_permission no token").unwrap().contains("blocked"));
        assert!(list(&g, false).unwrap().contains("blocked: no token"));
        assert!(show(&g, "pmi").unwrap().contains("wakes when env:LYRA_TEST_GOAL_TOKEN"));
        assert!(!condition("env:LYRA_TEST_GOAL_TOKEN", None));
        assert!(condition("file:/", None) && !condition("file:/nope/never", None));
        // SAFETY: only this test reads the variable.
        unsafe { std::env::set_var("LYRA_TEST_GOAL_TOKEN", "x") };
        let woke = g.manager.fire_triggers(Utc::now(), &|c| condition(c, None)).unwrap();
        assert!(woke.iter().any(|n| n.contains("Integrate PMI")), "{woke:?}");
        let r = prompts::parse_review(r#"{"priority":[{"goal":"docs","priority":2,"reason":"later"}],"cancel":[{"goal":"nothing-like-this","reason":"x"}]}"#).unwrap();
        assert!(describe_review(&g, &r).contains("priority 2 for Write the docs"));
        assert_eq!(apply_review(&g, &r), ["Write the docs → priority 2"]);
        assert_eq!(blocker_for("s2 needs approval (/plan approve)").0, BlockerType::ApprovalRequired);
        assert_eq!(blocker_for("open questions: which server?").0, BlockerType::MissingInformation);
    }
}
