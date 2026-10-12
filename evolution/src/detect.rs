//! Improvement detection (E2): deterministic detectors over run telemetry.
//! They find *problems* with evidence; proposing fixes is the evolver's job.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::model::{Category, Opportunity, RunKind, RunOutcome, RunRecord};

/// How a learned skill is doing, from the skills system.
/// How a long-lived goal has been going, for spotting goals that keep
/// getting stuck the same way.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalRecord {
    pub title: String,
    /// Times it was blocked, and the most common kind of blocker.
    pub blocks: u32,
    pub common_blocker: Option<String>,
    pub failed_plans: u32,
}

/// Goals blocked again and again (for the same reason) or whose plans keep
/// failing: better workflows, tools or guidance could get them moving.
pub fn detect_goals(goals: &[GoalRecord], t: &Thresholds) -> Vec<Opportunity> {
    goals
        .iter()
        .filter(|g| g.blocks >= t.goal_blocks || g.failed_plans >= t.goal_blocks)
        .map(|g| Opportunity {
            kind: "stuck_goal".into(),
            problem: format!(
                "goal \"{}\" keeps getting stuck: blocked {} times{}, {} failed plans",
                g.title,
                g.blocks,
                g.common_blocker.as_ref().map_or(String::new(), |b| format!(" (mostly {b})")),
                g.failed_plans
            ),
            categories: vec![Category::Workflow, Category::Prompt, Category::Tool, Category::Skill],
            evidence: Vec::new(),
            details: json!({ "goal": g.title, "blocks": g.blocks, "common_blocker": g.common_blocker, "failed_plans": g.failed_plans }),
        })
        .collect()
}

/// How a specialist subagent has been doing (A15).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRecord {
    pub name: String,
    pub delegations: u32,
    /// Delegations that didn't complete.
    pub failed: u32,
    /// Delegations whose result the user corrected.
    pub corrected: u32,
    /// Recent tasks it got wrong, with the user's reaction if any.
    pub examples: Vec<String>,
}

/// Agents whose work keeps failing or getting corrected: their instructions
/// (or the routing that sends them work) should change.
pub fn detect_agents(agents: &[AgentRecord], t: &Thresholds) -> Vec<Opportunity> {
    agents
        .iter()
        .filter(|a| a.delegations >= t.agent_min_delegations)
        .filter(|a| (a.failed + a.corrected) as f32 / a.delegations.max(1) as f32 >= t.agent_trouble_share)
        .map(|a| Opportunity {
            kind: "struggling_agent".into(),
            problem: format!("agent {} is struggling: {} failed and {} corrected of {} delegations", a.name, a.failed, a.corrected, a.delegations),
            categories: vec![Category::Agent, Category::Prompt],
            evidence: Vec::new(),
            details: json!({ "agent": a.name, "failed": a.failed, "corrected": a.corrected, "delegations": a.delegations, "examples": a.examples }),
        })
        .collect()
}

/// A capability's track record, for spotting tools that fail or crawl (C12).
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityRecord {
    pub name: String,
    pub uses: u64,
    pub success_rate: f32,
    pub average_latency_ms: u64,
    /// The most common error kind.
    pub common_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillHealth {
    pub name: String,
    pub uses: u64,
    pub reliability: f32,
}

/// When something counts as a problem. `[evolution.thresholds]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// A run with at least this many tool calls is heavy...
    pub heavy_tool_calls: u32,
    /// ...or this many model calls.
    pub heavy_model_calls: u32,
    /// Heavy runs needed before it's a pattern.
    pub heavy_runs: usize,
    /// The same error this many times.
    pub repeated_error: usize,
    /// Corrections in the window (and at least a fifth of the runs).
    pub corrections: usize,
    /// Runs sharing the same tool sequence.
    pub sequence_runs: usize,
    /// Plan runs with this many replans...
    pub replans: u32,
    /// ...or this many retries...
    pub plan_retries: u32,
    /// ...or this many failed verifications count as troubled.
    pub plan_verification_failures: u32,
    /// Troubled plan runs needed before it's a pattern.
    pub plan_runs: usize,
    /// A skill used at least this often...
    pub skill_min_uses: u64,
    /// ...with reliability below this is failing.
    pub skill_reliability: f32,
    /// A capability used at least this often...
    pub capability_min_uses: u64,
    /// ...that succeeds less than this share of the time is failing...
    pub capability_success: f32,
    /// ...and one this slow on average (milliseconds) is slow.
    pub capability_slow_ms: u64,
    /// A goal blocked (or with failed plans) this often is stuck.
    pub goal_blocks: u32,
    /// An agent with at least this many delegations...
    pub agent_min_delegations: u32,
    /// ...that failed or was corrected this share of the time is struggling.
    pub agent_trouble_share: f32,
    /// Replies stopped at the tool-call limit this often in the window.
    pub limit_hits: usize,
    /// A reply this slow (seconds) is slow; this many of them (and a fifth of the runs) is a pattern.
    pub slow_seconds: u64,
    pub slow_runs: usize,
    /// A reply this long (characters) is long; this many of them corrected is a pattern.
    pub long_chars: usize,
    pub long_corrected: usize,
    /// A workflow corrected this many times, at least half the times it ran, mis-triggers.
    pub workflow_corrections: usize,
    /// This many guidelines is too many to keep in every prompt.
    pub max_guidelines: usize,
    /// A guideline in play for this many judged replies, and as many without it...
    pub guideline_min_runs: usize,
    /// ...that does this much worse with it doesn't help.
    pub guideline_drop: f32,
    /// "I told you that already" this often in the window.
    pub memory_misses: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            heavy_tool_calls: 8,
            heavy_model_calls: 6,
            heavy_runs: 2,
            repeated_error: 3,
            corrections: 3,
            sequence_runs: 3,
            replans: 2,
            plan_retries: 3,
            plan_verification_failures: 2,
            plan_runs: 2,
            skill_min_uses: 3,
            skill_reliability: 0.4,
            capability_min_uses: 5,
            capability_success: 0.6,
            capability_slow_ms: 15_000,
            goal_blocks: 3,
            agent_min_delegations: 4,
            agent_trouble_share: 0.4,
            limit_hits: 3,
            slow_seconds: 90,
            slow_runs: 3,
            long_chars: 4000,
            long_corrected: 2,
            workflow_corrections: 2,
            max_guidelines: 10,
            guideline_min_runs: 10,
            guideline_drop: 0.15,
            memory_misses: 2,
        }
    }
}

pub fn detect(runs: &[RunRecord], skills: &[SkillHealth], t: &Thresholds) -> Vec<Opportunity> {
    let mut out = Vec::new();
    let ids = |rs: &[&RunRecord]| rs.iter().map(|r| r.id).collect::<Vec<Uuid>>();
    let tasks = |rs: &[&RunRecord]| rs.iter().filter(|r| !r.task.is_empty()).take(6).map(|r| r.task.clone()).collect::<Vec<_>>();
    out.extend(detect_more(runs, t));

    // Heavy runs: many tool or model calls for one task.
    let heavy: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| r.kind == RunKind::Chat && (r.tool_calls >= t.heavy_tool_calls || r.model_calls >= t.heavy_model_calls))
        .collect();
    if heavy.len() >= t.heavy_runs {
        let avg_tools = heavy.iter().map(|r| r.tool_calls).sum::<u32>() as f32 / heavy.len() as f32;
        let avg_models = heavy.iter().map(|r| r.model_calls).sum::<u32>() as f32 / heavy.len() as f32;
        out.push(Opportunity {
            kind: "inefficient_runs".into(),
            problem: format!(
                "{} runs needed many steps (avg {avg_tools:.1} tool calls, {avg_models:.1} model calls)",
                heavy.len()
            ),
            categories: vec![Category::Workflow, Category::Tool, Category::Prompt],
            evidence: ids(&heavy),
            details: json!({ "tasks": tasks(&heavy), "avg_tool_calls": avg_tools, "avg_model_calls": avg_models }),
        });
    }

    // The same error again and again: a missing recovery behavior.
    let mut errors: HashMap<String, Vec<&RunRecord>> = HashMap::new();
    for r in runs {
        let mut seen = Vec::new();
        for e in &r.errors {
            let key = normalize_error(e);
            if !seen.contains(&key) {
                errors.entry(key.clone()).or_default().push(r);
                seen.push(key);
            }
        }
    }
    let mut repeated: Vec<(String, Vec<&RunRecord>)> = errors.into_iter().filter(|(_, rs)| rs.len() >= t.repeated_error).collect();
    repeated.sort_by_key(|(_, rs)| std::cmp::Reverse(rs.len()));
    for (error, rs) in repeated.into_iter().take(3) {
        out.push(Opportunity {
            kind: "repeated_error".into(),
            problem: format!("the same error happened in {} runs: {error}", rs.len()),
            // A recurring error may also be a bug in the agent itself.
            categories: vec![Category::Skill, Category::Workflow, Category::Prompt, Category::Code],
            evidence: ids(&rs),
            details: json!({ "error": error, "tasks": tasks(&rs) }),
        });
    }

    // The user keeps correcting answers: behavior or prompt should change.
    let corrected: Vec<&RunRecord> = runs.iter().filter(|r| r.corrected).collect();
    if corrected.len() >= t.corrections && corrected.len() * 5 >= runs.len() {
        out.push(Opportunity {
            kind: "frequent_corrections".into(),
            problem: format!("{} of {} recent answers were corrected by the user", corrected.len(), runs.len()),
            categories: vec![Category::Prompt, Category::Configuration],
            evidence: ids(&corrected),
            details: json!({ "corrected_tasks": tasks(&corrected) }),
        });
    }

    // The same chain of tools in many runs: a composite tool could do it in one call.
    if let Some((sequence, rs)) = repeated_sequence(runs, t.sequence_runs) {
        out.push(Opportunity {
            kind: "repeated_tool_sequence".into(),
            problem: format!("{} runs called the same tools in sequence: {}", rs.len(), sequence.join(" → ")),
            categories: vec![Category::Tool],
            evidence: ids(&rs),
            details: json!({ "sequence": sequence, "tasks": tasks(&rs) }),
        });
    }

    // A learned skill keeps failing: the skill, its retrieval or the workflow is wrong.
    for s in skills.iter().filter(|s| s.uses >= t.skill_min_uses && s.reliability < t.skill_reliability) {
        let rs: Vec<&RunRecord> = runs.iter().filter(|r| r.skills_used.iter().any(|u| u.trim_end_matches(" (trial)") == s.name)).collect();
        out.push(Opportunity {
            kind: "failing_skill".into(),
            problem: format!("skill {} keeps failing (reliability {:.2} over {} uses)", s.name, s.reliability, s.uses),
            categories: vec![Category::Skill, Category::Prompt, Category::Workflow],
            evidence: ids(&rs),
            details: json!({ "skill": s.name }),
        });
    }

    // Plans that needed replanning or many retries: the planning workflow could be better.
    let troubled: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| {
            r.kind == RunKind::Plan
                && (r.replans >= t.replans || r.retries >= t.plan_retries || r.verification_failures >= t.plan_verification_failures)
        })
        .collect();
    if troubled.len() >= t.plan_runs {
        out.push(Opportunity {
            kind: "plans_need_rework".into(),
            problem: format!("{} plans needed replanning or many retries", troubled.len()),
            categories: vec![Category::Workflow, Category::Configuration],
            evidence: ids(&troubled),
            details: json!({ "goals": tasks(&troubled) }),
        });
    }
    out
}

/// 4.6: the tool-call limit, slow or long replies, workflows that fire on
/// the wrong requests, too many guidelines (or ones that don't help), and
/// memory missing what the person had said before.
pub fn detect_more(runs: &[RunRecord], t: &Thresholds) -> Vec<Opportunity> {
    let mut out = Vec::new();
    let chats: Vec<&RunRecord> = runs.iter().filter(|r| r.kind == RunKind::Chat).collect();
    let ids = |rs: &[&RunRecord]| rs.iter().map(|r| r.id).collect::<Vec<Uuid>>();
    let share = |n: usize| chats.is_empty() || n * 5 >= chats.len();
    let limited: Vec<&RunRecord> = chats.iter().copied().filter(|r| r.limited).collect();
    if limited.len() >= t.limit_hits {
        out.push(Opportunity {
            kind: "tool_limit_hits".into(),
            problem: format!("{} recent replies stopped at the tool-call limit, part done", limited.len()),
            categories: vec![Category::Configuration, Category::Workflow, Category::Tool, Category::Prompt],
            evidence: ids(&limited),
            details: json!({ "tools": limited.iter().take(6).map(|r| r.tools_used.join(" → ")).collect::<Vec<_>>() }),
        });
    }
    let slow: Vec<&RunRecord> = chats.iter().copied().filter(|r| r.duration_ms >= t.slow_seconds * 1000).collect();
    if slow.len() >= t.slow_runs && share(slow.len()) {
        let avg = slow.iter().map(|r| r.duration_ms).sum::<u64>() as f32 / slow.len() as f32 / 1000.0;
        out.push(Opportunity {
            kind: "slow_replies".into(),
            problem: format!("{} recent replies took over {}s (avg {avg:.0}s)", slow.len(), t.slow_seconds),
            categories: vec![Category::Prompt, Category::Configuration, Category::Tool],
            evidence: ids(&slow),
            details: json!({ "avg_seconds": avg, "avg_tool_calls": slow.iter().map(|r| r.tool_calls).sum::<u32>() as f32 / slow.len() as f32 }),
        });
    }
    let long: Vec<&RunRecord> = chats.iter().copied().filter(|r| r.reply_chars >= t.long_chars && r.corrected).collect();
    if long.len() >= t.long_corrected {
        out.push(Opportunity {
            kind: "long_replies".into(),
            problem: format!("{} long replies (over {} characters) were corrected", long.len(), t.long_chars),
            categories: vec![Category::Prompt],
            evidence: ids(&long),
            details: json!({ "feedback": long.iter().filter_map(|r| r.feedback.clone()).take(5).collect::<Vec<_>>() }),
        });
    }
    // A workflow that keeps getting corrected fires on the wrong requests (or misleads).
    let mut workflows: Vec<&str> = chats.iter().filter_map(|r| r.workflow.as_deref()).collect();
    workflows.sort();
    workflows.dedup();
    for w in workflows {
        let with: Vec<&RunRecord> = chats.iter().copied().filter(|r| r.workflow.as_deref() == Some(w)).collect();
        let corrected: Vec<&RunRecord> = with.iter().copied().filter(|r| r.corrected).collect();
        if corrected.len() >= t.workflow_corrections && corrected.len() * 2 >= with.len() {
            out.push(Opportunity {
                kind: "workflow_mistrigger".into(),
                problem: format!("workflow {w} was corrected {} of the {} times it ran", corrected.len(), with.len()),
                categories: vec![Category::Workflow],
                evidence: ids(&corrected),
                details: json!({ "workflow": w, "tasks": corrected.iter().filter(|r| !r.task.is_empty()).take(5).map(|r| r.task.clone()).collect::<Vec<_>>() }),
            });
        }
    }
    // Guidelines: too many, or one that doesn't help (replies with it vs without).
    if let Some(latest) = chats.first().filter(|r| r.guidelines.len() >= t.max_guidelines) {
        out.push(Opportunity {
            kind: "guideline_bloat".into(),
            problem: format!("{} guidelines go into every prompt: too many to all matter", latest.guidelines.len()),
            categories: vec![Category::Prompt],
            evidence: Vec::new(),
            details: json!({ "guidelines": guideline_effects(&chats) }),
        });
    }
    for g in guideline_effects(&chats) {
        if g.with_judged >= t.guideline_min_runs && g.without_judged >= t.guideline_min_runs && g.without_rate - g.with_rate > t.guideline_drop {
            out.push(Opportunity {
                kind: "guideline_unhelpful".into(),
                problem: format!("replies with the guideline \"{}\" go well {:.0}% of the time vs {:.0}% without it", g.guideline, g.with_rate * 100.0, g.without_rate * 100.0),
                categories: vec![Category::Prompt],
                evidence: Vec::new(),
                details: json!({ "guideline": g.guideline }),
            });
        }
    }
    let missed: Vec<&RunRecord> = chats.iter().copied().filter(|r| r.memory_miss).collect();
    if missed.len() >= t.memory_misses {
        out.push(Opportunity {
            kind: "memory_misses".into(),
            problem: format!("{} times recently the person said they'd told lyra before", missed.len()),
            categories: vec![Category::Configuration, Category::Prompt],
            evidence: ids(&missed),
            details: json!({ "hint": "recall_before_answering makes lyra look in its memory first" }),
        });
    }
    out
}

/// How replies went with each guideline in play, and without it.
#[derive(Debug, Clone, Serialize)]
pub struct GuidelineEffect {
    pub guideline: String,
    pub with_judged: usize,
    pub with_rate: f32,
    pub without_judged: usize,
    pub without_rate: f32,
}

pub fn guideline_effects(chats: &[&RunRecord]) -> Vec<GuidelineEffect> {
    let mut all: Vec<&str> = chats.iter().flat_map(|r| r.guidelines.iter().map(String::as_str)).collect();
    all.sort();
    all.dedup();
    let judged = |r: &&&RunRecord| r.outcome != RunOutcome::Unknown;
    let rate = |rs: &[&&RunRecord]| if rs.is_empty() { 0.0 } else { rs.iter().filter(|r| r.outcome == RunOutcome::Success).count() as f32 / rs.len() as f32 };
    all.into_iter()
        .map(|g| {
            let (with, without): (Vec<&&RunRecord>, Vec<&&RunRecord>) = chats.iter().filter(judged).partition(|r| r.guidelines.iter().any(|x| x == g));
            GuidelineEffect { guideline: g.to_string(), with_judged: with.len(), with_rate: rate(&with), without_judged: without.len(), without_rate: rate(&without) }
        })
        .collect()
}

/// C12: capabilities that keep failing or crawl, with the runs that used
/// them as evidence. Better tools, workflows around them, or guidance on
/// when (not) to use them can fix that.
pub fn detect_capabilities(runs: &[RunRecord], caps: &[CapabilityRecord], t: &Thresholds) -> Vec<Opportunity> {
    let mut out = Vec::new();
    for c in caps.iter().filter(|c| c.uses >= t.capability_min_uses) {
        let used: Vec<Uuid> = runs.iter().filter(|r| r.tools_used.contains(&c.name)).map(|r| r.id).collect();
        if c.success_rate < t.capability_success {
            out.push(Opportunity {
                kind: "failing_capability".into(),
                problem: format!(
                    "capability {} succeeds only {:.0}% of the time over {} uses{}",
                    c.name,
                    c.success_rate * 100.0,
                    c.uses,
                    c.common_error.as_ref().map_or(String::new(), |e| format!(" (mostly {e} errors)"))
                ),
                categories: vec![Category::Prompt, Category::Workflow, Category::Tool, Category::Code],
                evidence: used.clone(),
                details: json!({ "capability": c.name, "success_rate": c.success_rate, "common_error": c.common_error }),
            });
        } else if c.average_latency_ms >= t.capability_slow_ms {
            out.push(Opportunity {
                kind: "slow_capability".into(),
                problem: format!("capability {} takes {:.1}s on average over {} uses", c.name, c.average_latency_ms as f32 / 1000.0, c.uses),
                categories: vec![Category::Prompt, Category::Workflow, Category::Tool],
                evidence: used,
                details: json!({ "capability": c.name, "average_latency_ms": c.average_latency_ms }),
            });
        }
    }
    out
}

/// Errors that differ only in numbers or ids count as the same error.
pub fn normalize_error(e: &str) -> String {
    let lowered: String = e
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(80).collect()
}

/// The longest, most common chain of 2–4 consecutive tool calls shared by at
/// least `min_runs` runs.
fn repeated_sequence(runs: &[RunRecord], min_runs: usize) -> Option<(Vec<String>, Vec<&RunRecord>)> {
    let mut by_seq: HashMap<Vec<String>, Vec<&RunRecord>> = HashMap::new();
    for r in runs {
        let mut seen: Vec<Vec<String>> = Vec::new();
        for n in 2..=4 {
            for w in r.tools_used.windows(n) {
                let seq = w.to_vec();
                if w.iter().all(|t| t == &w[0]) || seen.contains(&seq) {
                    continue;
                }
                seen.push(seq.clone());
                by_seq.entry(seq).or_default().push(r);
            }
        }
    }
    by_seq
        .into_iter()
        .filter(|(_, rs)| rs.len() >= min_runs)
        .max_by(|a, b| (a.0.len() * a.1.len()).cmp(&(b.0.len() * b.1.len())).then(a.0.len().cmp(&b.0.len())))
}

#[cfg(test)]
mod tests {

    #[test]
    fn struggling_agents_are_found() {
        let t = Thresholds::default();
        let rec = |name: &str, delegations, failed, corrected| AgentRecord { name: name.into(), delegations, failed, corrected, examples: vec![] };
        let ops = detect_agents(&[rec("writer", 6, 1, 2), rec("researcher", 10, 1, 0), rec("new", 2, 2, 0)], &t);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, "struggling_agent");
        assert!(ops[0].categories.contains(&Category::Agent));
    }

    use super::*;
    use crate::model::RunOutcome;

    #[test]
    fn new_detectors_find_limits_slowness_long_corrections_bad_workflows_unhelpful_guidelines_and_memory_misses() {
        let t = Thresholds::default();
        let mut runs = Vec::new();
        for i in 0..12 {
            let mut r = run(&format!("task {i}"));
            r.limited = i < 3;
            r.duration_ms = if i < 4 { 120_000 } else { 5_000 };
            r.reply_chars = if i < 2 { 6000 } else { 500 };
            r.corrected = i < 2 || (4..6).contains(&i);
            r.workflow = (4..6).contains(&i).then(|| "release".to_string());
            r.memory_miss = i == 7 || i == 8;
            runs.push(r);
        }
        let kinds = |rs: &[RunRecord]| detect_more(rs, &t).into_iter().map(|o| o.kind).collect::<Vec<_>>();
        let found = kinds(&runs);
        for k in ["tool_limit_hits", "slow_replies", "long_replies", "workflow_mistrigger", "memory_misses"] {
            assert!(found.contains(&k.to_string()), "{k}: {found:?}");
        }
        assert!(!found.contains(&"guideline_bloat".to_string()));
        // A guideline whose replies go worse than those without it.
        let mut judged = Vec::new();
        for i in 0..24 {
            let mut r = run(&format!("judged {i}"));
            let with = i < 12;
            r.guidelines = if with { vec!["Answer in one word.".into()] } else { vec![] };
            r.outcome = if (with && i % 3 == 0) || (!with && i % 4 != 0) { RunOutcome::Success } else { RunOutcome::Failure };
            judged.push(r);
        }
        let found = detect_more(&judged, &t);
        let g = found.iter().find(|o| o.kind == "guideline_unhelpful").expect("the guideline doesn't help");
        assert!(g.problem.contains("Answer in one word."), "{}", g.problem);
        // Too many guidelines in play.
        let mut bloated = run("x");
        bloated.guidelines = (0..12).map(|i| format!("rule {i}")).collect();
        assert!(kinds(&[bloated]).contains(&"guideline_bloat".to_string()));
    }

    fn run(task: &str) -> RunRecord {
        RunRecord::new(RunKind::Chat, task, 1)
    }

    #[test]
    fn quiet_telemetry_finds_nothing() {
        let runs: Vec<RunRecord> = (0..10).map(|i| run(&format!("question {i}"))).collect();
        assert!(detect(&runs, &[], &Thresholds::default()).is_empty());
    }

    #[test]
    fn finds_heavy_runs_errors_corrections_and_sequences() {
        let mut runs = Vec::new();
        for i in 0..3 {
            let mut r = run(&format!("diagnose container {i}"));
            r.tool_calls = 9;
            r.tools_used = vec!["memory_recall".into(), "memory_list".into(), "memory_recall".into(), "memory_list".into()];
            r.errors = vec![format!("connection refused on port 80{i}0")];
            r.corrected = true;
            r.outcome = RunOutcome::Failure;
            runs.push(r);
        }
        runs.extend((0..5).map(|i| run(&format!("other {i}"))));
        let found = detect(&runs, &[], &Thresholds::default());
        let kinds: Vec<&str> = found.iter().map(|o| o.kind.as_str()).collect();
        assert_eq!(kinds, ["inefficient_runs", "repeated_error", "frequent_corrections", "repeated_tool_sequence"]);
        assert_eq!(found[1].evidence.len(), 3, "port numbers don't make errors different");
        assert_eq!(found[3].details["sequence"].as_array().unwrap().len(), 4, "the longest shared chain");
    }

    #[test]
    fn finds_failing_skills_and_troubled_plans() {
        let mut a = RunRecord::new(RunKind::Plan, "deploy", 1);
        a.replans = 2;
        let mut b = a.clone();
        b.id = Uuid::new_v4();
        let skills = [SkillHealth { name: "deploy-steps".into(), uses: 5, reliability: 0.2 }, SkillHealth { name: "fine".into(), uses: 5, reliability: 0.9 }];
        let found = detect(&[a, b], &skills, &Thresholds::default());
        let kinds: Vec<&str> = found.iter().map(|o| o.kind.as_str()).collect();
        assert_eq!(kinds, ["failing_skill", "plans_need_rework"]);
    }
}
