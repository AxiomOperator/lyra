//! The plan as a dependency graph (P3), and picking what can run together (P12).

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use crate::model::{Idempotency, PlanStep, StepStatus};

/// Reject plans the scheduler can't run: dependencies on steps that don't
/// exist, on themselves, or in a cycle.
pub fn validate(steps: &[PlanStep]) -> Result<(), String> {
    let ids: HashSet<Uuid> = steps.iter().map(|s| s.id).collect();
    if ids.len() != steps.len() {
        return Err("two steps share an id".into());
    }
    for s in steps {
        for d in &s.dependencies {
            if *d == s.id {
                return Err(format!("{} depends on itself", s.key));
            }
            if !ids.contains(d) {
                return Err(format!("{} depends on a step that doesn't exist", s.key));
            }
        }
    }
    if topo_order(steps).len() != steps.len() {
        let stuck: Vec<&str> = {
            let ordered: HashSet<Uuid> = topo_order(steps).into_iter().collect();
            steps.iter().filter(|s| !ordered.contains(&s.id)).map(|s| s.key.as_str()).collect()
        };
        return Err(format!("circular dependency among {}", stuck.join(", ")));
    }
    Ok(())
}

/// Steps in an order that respects dependencies (Kahn's algorithm). Steps in
/// a cycle are left out.
pub fn topo_order(steps: &[PlanStep]) -> Vec<Uuid> {
    let mut indegree: HashMap<Uuid, usize> = steps.iter().map(|s| (s.id, s.dependencies.len())).collect();
    let mut dependents: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for s in steps {
        for d in &s.dependencies {
            dependents.entry(*d).or_default().push(s.id);
        }
    }
    let mut queue: VecDeque<Uuid> = steps.iter().filter(|s| s.dependencies.is_empty()).map(|s| s.id).collect();
    let mut order = Vec::new();
    while let Some(id) = queue.pop_front() {
        order.push(id);
        for next in dependents.get(&id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(next) {
                *n -= 1;
                if *n == 0 {
                    queue.push_back(*next);
                }
            }
        }
    }
    order
}

/// Steps that can run now: not started, and every dependency done.
pub fn ready_steps(steps: &[PlanStep]) -> Vec<&PlanStep> {
    let by_id: HashMap<Uuid, &PlanStep> = steps.iter().map(|s| (s.id, s)).collect();
    let order = topo_order(steps);
    order
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter(|s| s.status == StepStatus::Pending)
        .filter(|s| s.dependencies.iter().all(|d| by_id.get(d).is_some_and(|dep| dep.is_settled())))
        .collect()
}

/// Steps that can't run because something they depend on failed, was
/// cancelled or is blocked.
pub fn blocked_steps(steps: &[PlanStep]) -> Vec<&PlanStep> {
    let by_id: HashMap<Uuid, &PlanStep> = steps.iter().map(|s| (s.id, s)).collect();
    steps
        .iter()
        .filter(|s| s.status == StepStatus::Pending)
        .filter(|s| {
            s.dependencies.iter().any(|d| {
                by_id.get(d).is_some_and(|dep| matches!(dep.status, StepStatus::Failed | StepStatus::Cancelled | StepStatus::Blocked))
            })
        })
        .collect()
}

/// Pick ready steps to run together: at most `max_parallel`, no two needing
/// the same exclusive resource, and anything unsafe to repeat runs alone.
pub fn schedule(ready: &[&PlanStep], max_parallel: usize) -> Vec<Uuid> {
    let mut batch: Vec<Uuid> = Vec::new();
    let mut locked: HashSet<&str> = HashSet::new();
    for s in ready {
        if batch.len() >= max_parallel.max(1) {
            break;
        }
        // Steps that may change things run on their own.
        let alone = s.idempotency != Idempotency::Safe;
        if alone && !batch.is_empty() {
            continue;
        }
        if s.resources.iter().any(|r| locked.contains(r.as_str())) {
            continue;
        }
        locked.extend(s.resources.iter().map(String::as_str));
        batch.push(s.id);
        if alone {
            break;
        }
    }
    batch
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;

    pub(crate) fn step(key: &str, deps: &[&PlanStep]) -> PlanStep {
        PlanStep {
            id: Uuid::new_v4(),
            key: key.into(),
            title: format!("step {key}"),
            description: String::new(),
            status: StepStatus::Pending,
            dependencies: deps.iter().map(|d| d.id).collect(),
            action: StepAction::Reasoning { instruction: format!("do {key}") },
            expected_outcome: None,
            verification: Verification::default(),
            retry_policy: RetryPolicy::default(),
            approval: ApprovalPolicy::Automatic,
            approved: None,
            idempotency: Idempotency::Safe,
            resources: vec![],
            result: None,
            verification_result: None,
            attempts: 0,
            failure_class: None,
            last_error: None,
            operation_id: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    fn keys(steps: &[&PlanStep]) -> Vec<String> {
        steps.iter().map(|s| s.key.clone()).collect()
    }

    #[test]
    fn diamond_runs_in_dependency_order() {
        let inspect = step("inspect", &[]);
        let storage = step("storage", &[&inspect]);
        let network = step("network", &[&inspect]);
        let install = step("install", &[&storage, &network]);
        let mut steps = vec![install, network, storage, inspect];
        assert!(validate(&steps).is_ok());
        assert_eq!(keys(&ready_steps(&steps)), ["inspect"]);

        steps[3].status = StepStatus::Completed;
        assert_eq!(keys(&ready_steps(&steps)).len(), 2, "storage and network can run in parallel");
        steps[1].status = StepStatus::Completed;
        assert_eq!(keys(&ready_steps(&steps)), ["storage"], "install still waits for storage");
        steps[2].status = StepStatus::Skipped;
        assert_eq!(keys(&ready_steps(&steps)), ["install"], "skipped counts as settled");
    }

    #[test]
    fn rejects_missing_self_and_circular_dependencies() {
        let a = step("a", &[]);
        let mut b = step("b", &[&a]);
        b.dependencies.push(Uuid::new_v4());
        assert!(validate(&[a.clone(), b]).unwrap_err().contains("doesn't exist"));

        let mut selfish = step("c", &[]);
        selfish.dependencies.push(selfish.id);
        assert!(validate(&[selfish]).unwrap_err().contains("itself"));

        let mut x = step("x", &[]);
        let y = step("y", &[&x]);
        x.dependencies.push(y.id);
        assert!(validate(&[x, y]).unwrap_err().contains("circular"));
    }

    #[test]
    fn failed_dependencies_block_dependents() {
        let mut a = step("a", &[]);
        let b = step("b", &[&a]);
        a.status = StepStatus::Failed;
        let steps = vec![a, b];
        assert!(ready_steps(&steps).is_empty());
        assert_eq!(keys(&blocked_steps(&steps)), ["b"]);
    }

    #[test]
    fn scheduling_respects_locks_limits_and_unsafe_steps() {
        let mut a = step("a", &[]);
        a.resources = vec!["host:db1".into()];
        let mut b = step("b", &[]);
        b.resources = vec!["host:db1".into()];
        let c = step("c", &[]);
        let mut d = step("d", &[]);
        d.idempotency = Idempotency::Unsafe;
        let ready = [&a, &b, &c, &d];
        let batch = schedule(&ready, 4);
        assert_eq!(batch, [a.id, c.id], "b shares a's lock; d must run alone");
        assert_eq!(schedule(&ready, 1), [a.id]);
        assert_eq!(schedule(&[&d, &c], 4), [d.id]);
    }
}
