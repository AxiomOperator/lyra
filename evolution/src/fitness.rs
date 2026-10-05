//! Fitness (E7): how good a version of the agent is on a benchmark, so that
//! changes are selected by evidence instead of model preference.

use crate::model::{BenchResult, Fitness, FitnessWeights};

pub fn fitness(results: &[BenchResult], w: &FitnessWeights) -> Fitness {
    if results.is_empty() {
        return Fitness::default();
    }
    let n = results.len() as f32;
    let avg = |f: &dyn Fn(&BenchResult) -> f32| results.iter().map(f).sum::<f32>() / n;
    let tool_calls = avg(&|r| r.tool_calls as f32);
    let model_calls = avg(&|r| r.model_calls as f32);
    let seconds = avg(&|r| r.seconds);
    let mut f = Fitness {
        success_rate: avg(&|r| if r.success { 1.0 } else { 0.0 }),
        accuracy: avg(&|r| r.accuracy.clamp(0.0, 1.0)),
        // Fewer calls, tokens and seconds is better; 1.0 means almost free.
        efficiency: 1.0 / (1.0 + (tool_calls + model_calls) / 8.0 + avg(&|r| r.tokens as f32) / 20_000.0 + seconds / 120.0),
        reliability: avg(&|r| if r.errors == 0 { 1.0 } else { 0.0 }),
        safety: avg(&|r| if r.safety_violations == 0 { 1.0 } else { 0.0 }),
        total: 0.0,
        tool_calls,
        model_calls,
        tokens: avg(&|r| r.tokens as f32),
        seconds,
    };
    f.total = f.success_rate * w.success + f.accuracy * w.accuracy + f.efficiency * w.efficiency + f.reliability * w.reliability + f.safety * w.safety;
    f
}

/// Whether `after` is an improvement worth deploying over `before`: better
/// overall, no less safe, and not noticeably less successful.
pub fn improves(before: &Fitness, after: &Fitness) -> bool {
    after.total > before.total + 0.01 && after.safety >= before.safety && after.success_rate + 0.05 >= before.success_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(success: bool, tool_calls: u32) -> BenchResult {
        BenchResult { success, accuracy: if success { 0.9 } else { 0.2 }, model_calls: 2, tool_calls, seconds: 10.0, ..Default::default() }
    }

    #[test]
    fn success_dominates_and_efficiency_breaks_ties() {
        let w = FitnessWeights::default();
        let baseline = fitness(&[result(true, 6), result(false, 6)], &w);
        let better = fitness(&[result(true, 6), result(true, 6)], &w);
        let leaner = fitness(&[result(true, 1), result(false, 1)], &w);
        assert!(improves(&baseline, &better));
        assert!(improves(&baseline, &leaner), "same success, fewer calls");
        assert!(!improves(&better, &leaner), "efficiency doesn't excuse failures");
        assert_eq!(fitness(&[], &w), Fitness::default());
    }

    #[test]
    fn unsafe_candidates_never_improve() {
        let w = FitnessWeights::default();
        let baseline = fitness(&[result(true, 6)], &w);
        let mut fast_but_unsafe = result(true, 0);
        fast_but_unsafe.safety_violations = 1;
        assert!(!improves(&baseline, &fitness(&[fast_but_unsafe], &w)));
    }
}
