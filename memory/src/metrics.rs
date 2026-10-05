//! Memory observability (L23): how often each storage operation runs, how
//! often it fails, and its latency (P50/P95 over recent calls). Every
//! operation also runs in a `tracing` span (`memory.write`,
//! `memory.vector_search`, …) for whatever subscriber the app installs.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

use tracing::Instrument;

/// Recent latencies kept per operation.
const SAMPLES: usize = 200;

#[derive(Default)]
struct Op {
    count: u64,
    failures: u64,
    millis: VecDeque<f32>,
}

/// One operation's numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct OpStats {
    pub name: &'static str,
    pub count: u64,
    pub failures: u64,
    pub p50_ms: f32,
    pub p95_ms: f32,
}

#[derive(Default)]
pub struct Metrics {
    ops: Mutex<HashMap<&'static str, Op>>,
}

impl Metrics {
    pub fn record(&self, name: &'static str, millis: f32, ok: bool) {
        let mut ops = self.ops.lock().unwrap_or_else(|e| e.into_inner());
        let op = ops.entry(name).or_default();
        op.count += 1;
        if !ok {
            op.failures += 1;
        }
        op.millis.push_back(millis);
        if op.millis.len() > SAMPLES {
            op.millis.pop_front();
        }
    }

    /// Time a fallible operation, in its own span.
    pub async fn timed<T, E>(&self, name: &'static str, f: impl Future<Output = Result<T, E>>) -> Result<T, E> {
        let start = Instant::now();
        let result = f.instrument(tracing::info_span!("memory", op = name)).await;
        self.record(name, start.elapsed().as_secs_f32() * 1000.0, result.is_ok());
        result
    }

    /// Every operation seen so far, by name.
    pub fn snapshot(&self) -> Vec<OpStats> {
        let ops = self.ops.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<OpStats> = ops
            .iter()
            .map(|(name, op)| {
                let mut sorted: Vec<f32> = op.millis.iter().copied().collect();
                sorted.sort_by(f32::total_cmp);
                let pct = |p: f32| sorted.get(((sorted.len() as f32 - 1.0) * p).round() as usize).copied().unwrap_or(0.0);
                OpStats { name, count: op.count, failures: op.failures, p50_ms: pct(0.5), p95_ms: pct(0.95) }
            })
            .collect();
        out.sort_by_key(|o| o.name);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_failures() {
        let m = Metrics::default();
        for i in 1..=100 {
            m.record("memory.write", i as f32, i != 50);
        }
        let s = &m.snapshot()[0];
        assert_eq!((s.name, s.count, s.failures), ("memory.write", 100, 1));
        assert!((s.p50_ms - 50.0).abs() <= 1.0 && (s.p95_ms - 95.0).abs() <= 1.0, "{s:?}");
    }
}
