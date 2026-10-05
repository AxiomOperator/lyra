//! The goal manager end to end (G1–G12).

use chrono::{Duration, Utc};
use lyra_goals::prompts;
use lyra_goals::*;

fn manager() -> (tokio::runtime::Runtime, GoalManager) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(GoalStore::in_memory()).unwrap();
    let m = GoalManager::with_store(store, rt.handle().clone(), Settings::default());
    (rt, m)
}

fn done(outcome: &str, summary: &str, criteria: Option<(u32, u32)>) -> PlanResult {
    PlanResult { outcome: outcome.into(), summary: summary.into(), criteria, tokens: 1000, blocker: None }
}

#[test]
fn goals_live_through_plans_and_report_progress() {
    let (_rt, m) = manager();
    let g = m.create(Goal::new("Integrate PMI", "tasks and projects")).unwrap();
    assert_eq!(m.find("pmi").unwrap().id, g.id, "found by title");
    let p1 = Uuid::new_v4();
    assert_eq!(m.link_plan(g.id, p1, false).unwrap(), 1);
    let notes = m.plan_finished(p1, &done("partial", "spec parsed; auth missing", Some((1, 3)))).unwrap();
    assert!(notes[0].contains("33%"), "{notes:?}");
    let g1 = m.get(g.id).unwrap().unwrap();
    assert_eq!((g1.status, g1.progress_detail.completed_items, g1.progress_detail.total_items), (GoalStatus::Active, 1, Some(3)));
    assert_eq!(g1.progress_detail.summary, "spec parsed; auth missing");
    let p2 = Uuid::new_v4();
    assert_eq!(m.link_plan(g.id, p2, true).unwrap(), 2, "a second attempt");
    m.plan_finished(p2, &done("completed", "all done", Some((3, 3)))).unwrap();
    let g2 = m.get(g.id).unwrap().unwrap();
    assert_eq!((g2.status, g2.progress), (GoalStatus::Completed, 1.0));
    assert!(g2.completed_at.is_some());
    let kinds: Vec<String> = m.events(Some(g.id), 50).unwrap().into_iter().map(|e| e.kind).collect();
    for k in ["created", "plan_started", "plan_finished", "progress_updated", "completed"] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing from {kinds:?}");
    }
    assert_eq!(m.plans(g.id).unwrap().len(), 2);
}

#[test]
fn decomposition_dependencies_and_rollup() {
    let (_rt, m) = manager();
    let parent = m.create(Goal::new("Integrate PMI", "")).unwrap();
    let reply = r#"{"subgoals":[
        {"title":"Parse OpenAPI","success_criteria":["operations listed"]},
        {"title":"Add authentication","depends_on":[1]},
        {"title":"Test end-to-end","depends_on":[1,2,9]}]}"#;
    let subs = prompts::parse_decomposition(reply, &parent).unwrap();
    assert_eq!(subs[2].1, vec![0, 1], "unknown and later numbers are dropped");
    let kids = m.decompose(parent.id, subs).unwrap();
    assert_eq!(kids.len(), 3);
    assert_eq!(kids[1].dependencies, vec![kids[0].id]);
    // Only the first can start; the parent is worked through its subgoals.
    let (next, _) = m.next(false).unwrap().unwrap();
    assert_eq!(next.id, kids[0].id);
    assert!(m.can_progress(&kids[1]).is_err());
    assert!(m.add_dependency(kids[0].id, kids[2].id).unwrap_err().contains("each other"), "no cycles");
    for k in &kids {
        m.set_status(k.id, GoalStatus::Completed, "done").unwrap();
        let p = m.get(parent.id).unwrap().unwrap();
        assert!(p.progress_detail.total_items == Some(3));
    }
    assert_eq!(m.get(parent.id).unwrap().unwrap().status, GoalStatus::Completed, "every subgoal done");
}

#[test]
fn blocked_goals_wait_for_a_state_change() {
    let (_rt, m) = manager();
    let g = m.create(Goal::new("Finish PMI integration", "")).unwrap();
    m.block(g.id, BlockerType::MissingPermission, "API token unavailable").unwrap();
    assert!(m.next(false).unwrap().is_none(), "blocked goals aren't picked");
    assert_eq!(m.blockers(Some(g.id), true).unwrap()[0].blocker_type, BlockerType::MissingPermission);
    m.add_trigger(g.id, Trigger::When { condition: "env:PMI_TOKEN".into() }).unwrap();
    let woke = m.fire_triggers(Utc::now(), &|c| c == "env:PMI_TOKEN" && false).unwrap();
    assert!(woke.is_empty());
    let woke = m.fire_triggers(Utc::now(), &|c| c == "env:PMI_TOKEN").unwrap();
    assert!(woke[0].contains("woke up"), "{woke:?}");
    assert_eq!(m.get(g.id).unwrap().unwrap().status, GoalStatus::Active);
    assert!(m.blockers(Some(g.id), true).unwrap().is_empty(), "blockers resolved");

    // Plans that keep failing block the goal instead of retrying forever.
    let (_rt2, mut m2) = manager();
    m2.settings.autonomy.max_failures = 2;
    let h = m2.create(Goal::new("Flaky", "")).unwrap();
    for _ in 0..2 {
        let p = Uuid::new_v4();
        m2.link_plan(h.id, p, true).unwrap();
        m2.plan_finished(p, &done("failed", "server down", None)).unwrap();
    }
    assert_eq!(m2.get(h.id).unwrap().unwrap().status, GoalStatus::Blocked);
    // A paused plan (approval) blocks with its reason.
    let k = m2.create(Goal::new("Delete old tasks", "")).unwrap();
    let p = Uuid::new_v4();
    m2.link_plan(k.id, p, true).unwrap();
    let pause = PlanResult { blocker: Some((BlockerType::ApprovalRequired, "s2 needs approval".into())), ..done("paused", "", None) };
    m2.plan_finished(p, &pause).unwrap();
    assert_eq!(m2.blockers(Some(k.id), true).unwrap()[0].blocker_type, BlockerType::ApprovalRequired);
}

#[test]
fn deadlines_and_dependents_can_outrank_raw_priority() {
    let (_rt, m) = manager();
    // The doc's example: A priority 8, due tomorrow, blocks 3 goals; B priority 10.
    let mut a = Goal::new("Goal A", "");
    a.priority = 8;
    a.due_at = Some(Utc::now() + Duration::days(1));
    let a = m.create(a).unwrap();
    let mut b = Goal::new("Goal B", "");
    b.priority = 10;
    let b = m.create(b).unwrap();
    for i in 0..3 {
        let mut d = Goal::new(&format!("waits {i}"), "");
        d.priority = 1;
        d.dependencies = vec![a.id];
        m.create(d).unwrap();
    }
    let ranked = m.ranked().unwrap();
    assert_eq!(ranked[0].0.id, a.id, "{:?}", ranked.iter().map(|(g, s)| (&g.title, s.total)).collect::<Vec<_>>());
    assert!(ranked.iter().position(|(g, _)| g.id == b.id).unwrap() == 1);
    // Assisted autonomy only continues goals that were already worked on.
    assert!(m.next(true).unwrap().is_none());
    m.link_plan(b.id, Uuid::new_v4(), false).unwrap();
    assert!(m.next(true).unwrap().is_none(), "B has a plan still running");
}

#[test]
fn schedules_and_recurring_goals() {
    let (_rt, m) = manager();
    let now = Utc::now();
    let mut later = Goal::new("Check backups", "");
    later.status = GoalStatus::Paused;
    let later = m.create(later).unwrap();
    m.add_trigger(later.id, Trigger::At { at: now + Duration::hours(1) }).unwrap();
    assert!(m.fire_triggers(now, &|_| false).unwrap().is_empty(), "not yet");
    assert_eq!(m.fire_triggers(now + Duration::hours(2), &|_| false).unwrap().len(), 1);
    assert!(m.triggers(Some(later.id)).unwrap().iter().all(|t| !t.active), "one-shot");

    let daily = m.create(Goal::new("Daily report", "")).unwrap();
    m.add_trigger(daily.id, Trigger::Every { minutes: 1440 }).unwrap();
    m.fire_triggers(now, &|_| false).unwrap();
    m.set_status(daily.id, GoalStatus::Completed, "sent").unwrap();
    assert!(m.fire_triggers(now + Duration::hours(1), &|_| false).unwrap().is_empty(), "not a day yet");
    let woke = m.fire_triggers(now + Duration::hours(25), &|_| false).unwrap();
    assert!(woke[0].contains("Daily report"));
    let d = m.get(daily.id).unwrap().unwrap();
    assert_eq!((d.status, d.progress), (GoalStatus::Active, 0.0), "reopened for the next period");

    let first = m.create(Goal::new("Ship v1", "")).unwrap();
    let mut second = Goal::new("Announce v1", "");
    second.status = GoalStatus::Paused;
    let second = m.create(second).unwrap();
    m.add_trigger(second.id, Trigger::After { goal: first.id }).unwrap();
    m.set_status(first.id, GoalStatus::Completed, "shipped").unwrap();
    assert!(m.fire_triggers(now, &|_| false).unwrap().iter().any(|n| n.contains("Announce")));
}

#[test]
fn parsing_times_reviews_and_requests() {
    let now = Utc::now();
    assert_eq!(parse_when("in 3d", now).unwrap(), now + Duration::days(3));
    assert_eq!(parse_duration("90m").unwrap(), Duration::minutes(90));
    assert!(parse_when("2026-10-20", now).is_ok() && parse_when("someday", now).is_err());
    let r = prompts::parse_review(r#"{"cancel":[{"goal":"abcd1234","reason":"obsolete"}],"priority":[{"goal":"x","priority":9}]}"#).unwrap();
    assert_eq!((r.cancel.len(), r.priority[0].priority, r.merge.len()), (1, 9, 0));
    let mut g = Goal::new("Integrate PMI", "tasks");
    g.progress_detail.summary = "auth done".into();
    let req = prompts::plan_request(&g, &[], &["Parse OpenAPI".into()], None);
    assert!(req.contains("Integrate PMI") && req.contains("Already done: Parse OpenAPI") && req.contains("auth done"));
    let proposed = prompts::agent_goal(&serde_json::json!({ "title": "Tidy the wiki", "priority": 3 })).unwrap();
    assert_eq!((proposed.status, proposed.origin), (GoalStatus::Proposed, Origin::Agent));
}
