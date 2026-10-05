//! The goal manager: the durable layer that answers what the agent is trying
//! to accomplish over time, what to work on next, and when to resume.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::model::*;
use crate::store::GoalStore;

/// `[goals]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub autonomy: AutonomyPolicy,
    pub priority: PriorityWeights,
    /// Open goals untouched this long are flagged by reviews (G12).
    pub stale_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { autonomy: AutonomyPolicy::default(), priority: PriorityWeights::default(), stale_days: 14 }
    }
}

/// How a plan for a goal ended, as the goal manager needs it.
#[derive(Debug, Clone)]
pub struct PlanResult {
    /// `completed`, `partial`, `failed`, `cancelled`, or `paused` (waiting).
    pub outcome: String,
    pub summary: String,
    /// Success criteria met and total, from the plan's evaluation.
    pub criteria: Option<(u32, u32)>,
    pub tokens: u64,
    /// Why it's waiting, when paused.
    pub blocker: Option<(BlockerType, String)>,
}

pub struct GoalManager {
    store: GoalStore,
    rt: Handle,
    pub settings: Settings,
}

fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

impl GoalManager {
    pub fn open(path: &Path, rt: Handle, settings: Settings) -> Result<Self> {
        let store = rt.block_on(GoalStore::open(path))?;
        Ok(Self { store, rt, settings })
    }

    pub fn with_store(store: GoalStore, rt: Handle, settings: Settings) -> Self {
        Self { store, rt, settings }
    }

    fn db<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T, String> {
        self.rt.block_on(f).map_err(|e| format!("{e:#}"))
    }

    fn event(&self, goal: Uuid, kind: GoalEventKind, message: impl Into<String>) -> Result<(), String> {
        let e = GoalEvent { id: Uuid::new_v4(), goal_id: goal, kind: kind.as_str().into(), message: message.into(), created_at: Utc::now() };
        self.db(self.store.record(&e))
    }

    // ---- reading

    pub fn all(&self) -> Result<Vec<Goal>, String> {
        self.db(self.store.goals())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<Goal>, String> {
        self.db(self.store.goal(id))
    }

    /// By id prefix, or a unique case-insensitive match in the title.
    pub fn find(&self, key: &str) -> Result<Goal, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("give a goal id (or part of its title)".into());
        }
        let all = self.all()?;
        let by_id: Vec<&Goal> = all.iter().filter(|g| key.len() >= 4 && g.id.to_string().starts_with(key)).collect();
        if by_id.len() == 1 {
            return Ok(by_id[0].clone());
        }
        let lower = key.to_lowercase();
        let by_title: Vec<&Goal> = all.iter().filter(|g| g.title.to_lowercase().contains(&lower)).collect();
        match by_title.len() {
            1 => Ok(by_title[0].clone()),
            0 => Err(format!("no goal matches {key:?}")),
            n => Err(format!("{n} goals match {key:?}; use the id")),
        }
    }

    pub fn children(&self, id: Uuid) -> Result<Vec<Goal>, String> {
        Ok(self.all()?.into_iter().filter(|g| g.parent_goal_id == Some(id)).collect())
    }

    pub fn plans(&self, id: Uuid) -> Result<Vec<GoalPlan>, String> {
        self.db(self.store.plans(id))
    }

    pub fn plan_goal(&self, plan: Uuid) -> Result<Option<GoalPlan>, String> {
        self.db(self.store.plan_goal(plan))
    }

    pub fn open_plans(&self) -> Result<Vec<GoalPlan>, String> {
        self.db(self.store.open_plans())
    }

    pub fn blockers(&self, id: Option<Uuid>, open_only: bool) -> Result<Vec<GoalBlocker>, String> {
        self.db(self.store.blockers(id, open_only))
    }

    pub fn triggers(&self, id: Option<Uuid>) -> Result<Vec<GoalTrigger>, String> {
        self.db(self.store.triggers(id))
    }

    pub fn events(&self, id: Option<Uuid>, limit: usize) -> Result<Vec<GoalEvent>, String> {
        self.db(self.store.events(id, limit))
    }

    // ---- writing (G1, G2)

    /// Store a new goal. Agent-proposed goals start as `proposed`.
    pub fn create(&self, mut goal: Goal) -> Result<Goal, String> {
        if goal.title.is_empty() {
            return Err("a goal needs a title".into());
        }
        if goal.origin == Origin::Agent && goal.status == GoalStatus::Active {
            goal.status = GoalStatus::Proposed;
        }
        goal.priority = goal.priority.min(10);
        self.db(self.store.save(&goal))?;
        let by = if goal.origin == Origin::Agent { "proposed by the agent" } else { "set by the user" };
        self.event(goal.id, GoalEventKind::Created, format!("{} ({by})", goal.title))?;
        Ok(goal)
    }

    /// Save changes to a goal's fields, with a note of what changed.
    pub fn update(&self, goal: &Goal, what: &str) -> Result<(), String> {
        self.db(self.store.save(goal))?;
        self.event(goal.id, GoalEventKind::Changed, what)
    }

    /// Move a goal to a new status, recording why.
    pub fn set_status(&self, id: Uuid, status: GoalStatus, why: &str) -> Result<Goal, String> {
        let mut g = self.get(id)?.ok_or("no such goal")?;
        if g.status == status {
            return Ok(g);
        }
        let from = g.status;
        g.status = status;
        match status {
            GoalStatus::Completed => {
                g.completed_at = Some(Utc::now());
                g.progress = 1.0;
            }
            GoalStatus::Active if !from.is_open() => g.completed_at = None,
            _ => {}
        }
        self.db(self.store.save(&g))?;
        if matches!(status, GoalStatus::Active | GoalStatus::Completed | GoalStatus::Cancelled) {
            self.db(self.store.resolve_blockers(id))?;
        }
        let kind = match status {
            GoalStatus::Active if from == GoalStatus::Blocked => GoalEventKind::Unblocked,
            GoalStatus::Active => GoalEventKind::Activated,
            GoalStatus::Blocked => GoalEventKind::Blocked,
            GoalStatus::Paused => GoalEventKind::Paused,
            GoalStatus::Completed => GoalEventKind::Completed,
            GoalStatus::Failed => GoalEventKind::Failed,
            GoalStatus::Cancelled => GoalEventKind::Cancelled,
            GoalStatus::Proposed => GoalEventKind::Changed,
        };
        self.event(id, kind, format!("{from} → {status}: {why}"))?;
        if let Some(parent) = g.parent_goal_id {
            self.rollup(parent)?;
        }
        Ok(g)
    }

    // ---- decomposition and dependencies (G3, G6)

    /// Add subgoals (already built) under `parent`; `depends_on` holds, per
    /// subgoal, the indexes of earlier subgoals it waits for.
    pub fn decompose(&self, parent: Uuid, subgoals: Vec<(Goal, Vec<usize>)>) -> Result<Vec<Goal>, String> {
        let p = self.get(parent)?.ok_or("no such goal")?;
        let mut created: Vec<Goal> = Vec::new();
        for (mut g, deps) in subgoals {
            g.parent_goal_id = Some(parent);
            g.dependencies = deps.iter().filter_map(|i| created.get(*i).map(|c| c.id)).collect();
            if g.due_at.is_none() {
                g.due_at = p.due_at;
            }
            g.importance = p.importance;
            created.push(self.create(g)?);
        }
        let names: Vec<String> = created.iter().map(|g| g.title.clone()).collect();
        self.event(parent, GoalEventKind::Decomposed, format!("into {}: {}", created.len(), names.join("; ")))?;
        self.rollup(parent)?;
        Ok(created)
    }

    /// `id` waits for `on`. Refuses cycles.
    pub fn add_dependency(&self, id: Uuid, on: Uuid) -> Result<(), String> {
        if id == on || self.depends_on(on, id)? {
            return Err("that would make goals wait for each other".into());
        }
        let mut g = self.get(id)?.ok_or("no such goal")?;
        self.get(on)?.ok_or("no such goal to depend on")?;
        if !g.dependencies.contains(&on) {
            g.dependencies.push(on);
            self.update(&g, &format!("now waits for {}", short(on)))?;
        }
        Ok(())
    }

    /// Whether `id` waits for `on`, directly or not.
    fn depends_on(&self, id: Uuid, on: Uuid) -> Result<bool, String> {
        let all = self.all()?;
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            if let Some(g) = all.iter().find(|g| g.id == x) {
                if g.dependencies.contains(&on) {
                    return Ok(true);
                }
                stack.extend(&g.dependencies);
            }
        }
        Ok(false)
    }

    /// A parent's progress follows its subgoals; when all are completed, so is it.
    fn rollup(&self, parent: Uuid) -> Result<(), String> {
        let Some(mut p) = self.get(parent)? else { return Ok(()) };
        let kids: Vec<Goal> = self.children(parent)?.into_iter().filter(|c| c.status != GoalStatus::Cancelled).collect();
        if kids.is_empty() {
            return Ok(());
        }
        let done = kids.iter().filter(|c| c.status == GoalStatus::Completed).count() as u32;
        p.progress_detail.completed_items = done;
        p.progress_detail.total_items = Some(kids.len() as u32);
        p.progress_detail.updated_at = Some(Utc::now());
        p.progress = kids.iter().map(|c| c.progress).sum::<f32>() / kids.len() as f32;
        self.db(self.store.save(&p))?;
        if done as usize == kids.len() && p.status.is_open() {
            self.set_status(parent, GoalStatus::Completed, "every subgoal is completed")?;
        }
        Ok(())
    }

    // ---- blockers (G6)

    /// Record why a goal can't progress; it won't be retried until the state changes.
    pub fn block(&self, id: Uuid, kind: BlockerType, reason: &str) -> Result<(), String> {
        let b = GoalBlocker { id: Uuid::new_v4(), goal_id: id, reason: reason.into(), blocker_type: kind, created_at: Utc::now(), resolved_at: None };
        self.db(self.store.add_blocker(&b))?;
        self.set_status(id, GoalStatus::Blocked, &format!("{}: {reason}", kind.as_str()))?;
        Ok(())
    }

    pub fn unblock(&self, id: Uuid, why: &str) -> Result<Goal, String> {
        self.set_status(id, GoalStatus::Active, why)
    }

    /// Whether work can start on a goal now, or why not.
    pub fn can_progress(&self, g: &Goal) -> Result<(), (BlockerType, String)> {
        if g.status != GoalStatus::Active {
            return Err((BlockerType::ExternalDependency, format!("it's {}", g.status)));
        }
        for dep in &g.dependencies {
            match self.get(*dep).ok().flatten() {
                Some(d) if d.status == GoalStatus::Completed => {}
                Some(d) if matches!(d.status, GoalStatus::Failed | GoalStatus::Cancelled) => {
                    return Err((BlockerType::FailedDependency, format!("{} {}", d.title, d.status)));
                }
                Some(d) => return Err((BlockerType::ExternalDependency, format!("waiting for {}", d.title))),
                None => {}
            }
        }
        Ok(())
    }

    // ---- plans and progress (G4, G5)

    /// A plan was started for a goal.
    pub fn link_plan(&self, goal: Uuid, plan: Uuid, autonomous: bool) -> Result<u32, String> {
        let attempt = self.plans(goal)?.len() as u32 + 1;
        let gp = GoalPlan { goal_id: goal, plan_id: plan, attempt, outcome: None, summary: None, tokens: 0, autonomous, started_at: Utc::now(), finished_at: None };
        self.db(self.store.add_plan(&gp))?;
        let by = if autonomous { " (autonomous)" } else { "" };
        self.event(goal, GoalEventKind::PlanStarted, format!("plan {} attempt {attempt}{by}", short(plan)))?;
        Ok(attempt)
    }

    /// A plan for a goal ended (or paused): update progress, and complete,
    /// block or keep the goal going. Returns notes. Plans for no goal are ignored.
    pub fn plan_finished(&self, plan: Uuid, r: &PlanResult) -> Result<Vec<String>, String> {
        let Some(gp) = self.plan_goal(plan)? else { return Ok(Vec::new()) };
        let mut g = self.get(gp.goal_id)?.ok_or("goal missing")?;
        let mut notes = Vec::new();
        if r.outcome == "paused" {
            let (kind, reason) = r.blocker.clone().unwrap_or((BlockerType::ApprovalRequired, "the plan is waiting".into()));
            self.block(g.id, kind, &reason)?;
            notes.push(format!("goal {} blocked: {reason}", g.title));
            return Ok(notes);
        }
        self.db(self.store.finish_plan(plan, &r.outcome, &r.summary, r.tokens))?;
        // The plan went on (approved, resumed): the goal is no longer stuck.
        if g.status == GoalStatus::Blocked {
            g = self.unblock(g.id, "its plan continued")?;
        }
        self.event(g.id, GoalEventKind::PlanFinished, format!("plan {} {}: {}", short(plan), r.outcome, r.summary))?;
        // G5: progress from the plan's criteria, kept with a summary.
        if let Some((met, total)) = r.criteria.filter(|(_, t)| *t > 0) {
            g.progress_detail.completed_items = met;
            g.progress_detail.total_items = Some(total);
            g.progress = g.progress.max(met as f32 / total as f32);
        }
        if !r.summary.is_empty() {
            g.progress_detail.summary = r.summary.clone();
        }
        g.progress_detail.updated_at = Some(Utc::now());
        self.db(self.store.save(&g))?;
        self.event(g.id, GoalEventKind::ProgressUpdated, format!("{:.0}% · {}", g.progress * 100.0, g.progress_detail.summary))?;
        let open_kids = self.children(g.id)?.iter().filter(|c| c.status.is_open()).count();
        match r.outcome.as_str() {
            "completed" if open_kids == 0 => {
                self.set_status(g.id, GoalStatus::Completed, "a plan met its success criteria")?;
                notes.push(format!("🎯 goal completed: {}", g.title));
            }
            "failed" | "cancelled" => {
                // Don't retry a goal that keeps failing (G6).
                let failures = self.plans(g.id)?.iter().rev().take_while(|p| matches!(p.outcome.as_deref(), Some("failed" | "cancelled"))).count() as u32;
                if failures >= self.settings.autonomy.max_failures {
                    self.block(g.id, BlockerType::FailedDependency, &format!("{failures} plans in a row failed: {}", r.summary))?;
                    notes.push(format!("goal {} blocked after {failures} failed plans", g.title));
                } else {
                    notes.push(format!("goal {}: plan {} ({failures} failure{} in a row)", g.title, r.outcome, if failures == 1 { "" } else { "s" }));
                }
            }
            _ => notes.push(format!("goal {}: {:.0}% — {}", g.title, g.progress * 100.0, g.progress_detail.summary)),
        }
        if let Some(parent) = g.parent_goal_id {
            self.rollup(parent)?;
        }
        Ok(notes)
    }

    // ---- priority (G7)

    /// Why a goal ranks where it does among `all`.
    pub fn score(&self, g: &Goal, all: &[Goal], now: DateTime<Utc>) -> PriorityScore {
        let w = &self.settings.priority;
        let explicit = g.priority as f32 / 10.0;
        let deadline = match g.due_at {
            None => 0.0,
            Some(due) if due <= now => 1.0,
            Some(due) => {
                let days = (due - now).num_minutes() as f32 / 1440.0;
                1.0 / (1.0 + days / 3.0)
            }
        };
        // Finishing it unblocks other goals.
        let waiting = all.iter().filter(|o| o.status.is_open() && o.dependencies.contains(&g.id)).count();
        let dependency = (waiting as f32 / 3.0).min(1.0);
        let importance = g.importance.clamp(0.0, 1.0);
        let progress = g.progress.clamp(0.0, 1.0);
        let spent: u64 = self.plans(g.id).unwrap_or_default().iter().map(|p| p.tokens).sum();
        let cost = (spent as f32 / 500_000.0).min(1.0);
        let total = explicit * w.explicit + deadline * w.deadline + dependency * w.dependency + importance * w.importance + progress * w.progress
            - cost * w.cost;
        PriorityScore { explicit, deadline, dependency, importance, progress, cost, total }
    }

    /// Open goals, best first, with their scores.
    pub fn ranked(&self) -> Result<Vec<(Goal, PriorityScore)>, String> {
        let all = self.all()?;
        let now = Utc::now();
        let mut out: Vec<(Goal, PriorityScore)> =
            all.iter().filter(|g| g.status.is_open()).map(|g| (g.clone(), self.score(g, &all, now))).collect();
        out.sort_by(|a, b| b.1.total.total_cmp(&a.1.total));
        Ok(out)
    }

    /// The goal to work on next (G7, G8): active, able to progress, no plan
    /// running, and a leaf (a goal with open subgoals is worked through them).
    /// `existing_only` (assisted autonomy) limits it to goals already worked on.
    pub fn next(&self, existing_only: bool) -> Result<Option<(Goal, PriorityScore)>, String> {
        let running: HashSet<Uuid> = self.open_plans()?.into_iter().map(|p| p.goal_id).collect();
        let all = self.all()?;
        for (g, score) in self.ranked()? {
            let has_open_children = all.iter().any(|c| c.parent_goal_id == Some(g.id) && c.status.is_open());
            if has_open_children || running.contains(&g.id) || self.can_progress(&g).is_err() {
                continue;
            }
            if existing_only && self.plans(g.id)?.is_empty() {
                // Assisted: a subgoal of a goal already worked on counts.
                let parent_worked = g.parent_goal_id.is_some_and(|p| !self.plans(p).unwrap_or_default().is_empty());
                if !parent_worked {
                    continue;
                }
            }
            return Ok(Some((g, score)));
        }
        Ok(None)
    }

    // ---- triggers (G9, G10)

    pub fn add_trigger(&self, goal: Uuid, trigger: Trigger) -> Result<GoalTrigger, String> {
        self.get(goal)?.ok_or("no such goal")?;
        let t = GoalTrigger { id: Uuid::new_v4(), goal_id: goal, trigger, last_fired: None, active: true };
        self.db(self.store.add_trigger(&t))?;
        self.event(goal, GoalEventKind::Changed, format!("wakes {}", t.trigger.describe()))?;
        Ok(t)
    }

    /// Fire the triggers that are due: wake blocked, paused or proposed goals,
    /// and reopen completed recurring ones. `condition` answers `When`
    /// conditions (`env:…`, `file:…`, `capability:…`). Returns notes.
    pub fn fire_triggers(&self, now: DateTime<Utc>, condition: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
        let mut notes = Vec::new();
        for t in self.triggers(None)?.into_iter().filter(|t| t.active) {
            let Some(g) = self.get(t.goal_id)? else { continue };
            if matches!(g.status, GoalStatus::Cancelled | GoalStatus::Failed) {
                continue;
            }
            let due = match &t.trigger {
                Trigger::At { at } => *at <= now,
                Trigger::Every { minutes } => t.last_fired.is_none_or(|last| now - last >= Duration::minutes(*minutes as i64)),
                Trigger::After { goal } => self.get(*goal)?.is_some_and(|o| o.status == GoalStatus::Completed),
                Trigger::When { condition: c } => match c.strip_prefix("goal:") {
                    Some(key) => self.find(key).is_ok_and(|o| o.status == GoalStatus::Completed),
                    None => condition(c),
                },
            };
            if !due {
                continue;
            }
            let recurring = matches!(t.trigger, Trigger::Every { .. });
            // A recurring trigger fires each period; the others once.
            self.db(self.store.set_trigger(t.id, Some(now), recurring))?;
            let woke = match g.status {
                GoalStatus::Completed if recurring => {
                    let mut g2 = g.clone();
                    g2.progress = 0.0;
                    g2.progress_detail = GoalProgress::default();
                    self.db(self.store.save(&g2))?;
                    self.set_status(g.id, GoalStatus::Active, &format!("recurring: {}", t.trigger.describe()))?;
                    true
                }
                GoalStatus::Blocked | GoalStatus::Paused | GoalStatus::Proposed => {
                    self.set_status(g.id, GoalStatus::Active, &format!("triggered {}", t.trigger.describe()))?;
                    true
                }
                _ => false,
            };
            self.event(g.id, GoalEventKind::Triggered, t.trigger.describe())?;
            if woke {
                notes.push(format!("⏰ goal {} woke up ({})", g.title, t.trigger.describe()));
            }
        }
        Ok(notes)
    }

    // ---- review (G12)

    /// Open goals with no change for `stale_days`.
    pub fn stale(&self) -> Result<Vec<Goal>, String> {
        let cutoff = Utc::now() - Duration::days(self.settings.stale_days);
        let mut out = Vec::new();
        for g in self.all()?.into_iter().filter(|g| g.status.is_open()) {
            let last = self.events(Some(g.id), 1)?.first().map_or(g.created_at, |e| e.created_at);
            if last < cutoff {
                out.push(g);
            }
        }
        Ok(out)
    }

    /// Record that a review looked at a goal.
    pub fn note_review(&self, id: Uuid, what: &str) -> Result<(), String> {
        self.event(id, GoalEventKind::Reviewed, what)
    }
}

/// `in 3d`, `in 2h`, `tomorrow`, `2026-10-20`, `2026-10-20 17:00` → a time.
pub fn parse_when(text: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let t = text.trim().to_lowercase();
    if let Some(rest) = t.strip_prefix("in ") {
        return Ok(now + parse_duration(rest)?);
    }
    if t == "tomorrow" {
        return Ok(now + Duration::days(1));
    }
    let local = |s: &str, fmt: &str| chrono::NaiveDateTime::parse_from_str(s, fmt).ok();
    let naive = local(&t, "%Y-%m-%d %H:%M")
        .or_else(|| chrono::NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(17, 0, 0)))
        .ok_or_else(|| format!("can't read {text:?}: try `in 3d`, `in 2h`, `tomorrow`, `2026-10-20` or `2026-10-20 17:00`"))?;
    naive.and_local_timezone(chrono::Local).single().map(|t| t.with_timezone(&Utc)).ok_or_else(|| format!("{text:?} isn't a valid local time"))
}

/// `30m`, `2h`, `3d`, `1w`.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let t = text.trim();
    let (n, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: i64 = n.parse().map_err(|_| format!("can't read {text:?} as a duration (30m, 2h, 3d, 1w)"))?;
    Ok(match unit.trim() {
        "m" | "min" | "mins" | "minutes" => Duration::minutes(n),
        "h" | "hour" | "hours" => Duration::hours(n),
        "d" | "day" | "days" => Duration::days(n),
        "w" | "week" | "weeks" => Duration::weeks(n),
        _ => return Err(format!("can't read {text:?} as a duration (30m, 2h, 3d, 1w)")),
    })
}
