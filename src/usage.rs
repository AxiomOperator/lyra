//! AI usage: every model call lyra makes, for whom (the acting person), what
//! kind (a chat turn, an agent or plan step, lyra's own background work),
//! which model, tokens and time. Kept as monthly JSON lines
//! (`~/.lyra/usage/<YYYY-MM>.jsonl`); admins see everyone's totals and each
//! person's, members their own.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use chrono::{DateTime, Datelike, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One model call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub at: DateTime<Utc>,
    pub user: String,
    /// "chat", "agent" (agents and plans), "background" (capture, triage, briefings, reviews…),
    /// "decision" (the `[decide]` model), "embedding", "reranker", "vision" (`[vision_model]`).
    pub kind: String,
    pub model: String,
    pub input: u64,
    #[serde(default)]
    pub cached: u64,
    pub output: u64,
    #[serde(default)]
    pub ms: u64,
}

/// Prices per million tokens (`input_cost_per_mtok` …), for an estimate.
#[derive(Debug, Clone, Default)]
pub struct Prices {
    pub input: f64,
    pub cached: f64,
    pub output: f64,
    pub currency: String,
}

static PRICES: RwLock<Option<Prices>> = RwLock::new(None);
static WRITE: Mutex<()> = Mutex::new(());

pub fn configure(p: Prices) {
    *PRICES.write().unwrap_or_else(|e| e.into_inner()) = Some(p);
}

fn prices() -> Prices {
    PRICES.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn dir() -> Option<PathBuf> {
    Some(crate::config::home()?.join("usage"))
}

/// Keep a call, for the person this thread works for.
pub fn record(kind: &str, model: &str, input: u64, cached: u64, output: u64, ms: u64) {
    if input == 0 && output == 0 || cfg!(test) {
        return;
    }
    let c = Call { at: Utc::now(), user: crate::acting::current(), kind: kind.into(), model: model.into(), input, cached, output, ms };
    let Some(d) = dir() else { return };
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::create_dir_all(&d);
    if let (Ok(mut f), Ok(line)) = (std::fs::OpenOptions::new().create(true).append(true).open(d.join(format!("{}.jsonl", c.at.format("%Y-%m")))), serde_json::to_string(&c)) {
        let _ = writeln!(f, "{line}");
    }
}

/// From an OpenAI-style `usage` object.
pub fn record_usage(kind: &str, model: &str, usage: &Value, ms: u64) {
    let input = usage["prompt_tokens"].as_u64().or_else(|| usage["input_tokens"].as_u64()).unwrap_or(0);
    let cached = usage["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
    let output = usage["completion_tokens"].as_u64().or_else(|| usage["output_tokens"].as_u64()).unwrap_or(0);
    record(kind, model, input, cached, output, ms);
}

/// Calls since `since` (reading the months it spans).
pub fn since(since: DateTime<Utc>) -> Vec<Call> {
    let Some(d) = dir() else { return vec![] };
    let mut months = Vec::new();
    let start = since.with_timezone(&Local).date_naive();
    let mut m = start.with_day0(0).unwrap_or(start);
    let end = Local::now().date_naive();
    while m <= end {
        months.push(m.format("%Y-%m").to_string());
        m = m.checked_add_months(chrono::Months::new(1)).unwrap_or(end + Duration::days(1));
    }
    months
        .iter()
        .filter_map(|mo| std::fs::read_to_string(d.join(format!("{mo}.jsonl"))).ok())
        .flat_map(|t| t.lines().filter_map(|l| serde_json::from_str::<Call>(l).ok()).collect::<Vec<_>>())
        .filter(|c| c.at >= since)
        .collect()
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Total {
    pub calls: u64,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub ms: u64,
    /// The estimate in the configured currency (0 when no prices are set).
    pub cost: f64,
}

impl Total {
    fn add(&mut self, c: &Call, p: &Prices) {
        self.calls += 1;
        self.input += c.input;
        self.cached += c.cached;
        self.output += c.output;
        self.ms += c.ms;
        // The prices are the chat model's; the small models' calls count tokens only.
        if !matches!(c.kind.as_str(), "chat" | "agent" | "background") {
            return;
        }
        let fresh = c.input.saturating_sub(c.cached) as f64;
        self.cost += (fresh * p.input + c.cached as f64 * if p.cached > 0.0 { p.cached } else { p.input } + c.output as f64 * p.output) / 1_000_000.0;
    }
}

/// Totals over the last `days`: everyone, each person, each kind, each model, each day.
/// `only`: one person's (a member sees their own).
pub fn summary(days: i64, only: Option<&str>) -> Value {
    let p = prices();
    let from = Utc::now() - Duration::days(days.clamp(1, 366));
    let calls: Vec<Call> = since(from).into_iter().filter(|c| only.is_none_or(|u| c.user == u)).collect();
    let mut all = Total::default();
    let mut by_user: BTreeMap<String, (Total, BTreeMap<String, Total>)> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, Total> = BTreeMap::new();
    let mut by_model: BTreeMap<String, Total> = BTreeMap::new();
    let mut by_day: BTreeMap<String, Total> = BTreeMap::new();
    for c in &calls {
        all.add(c, &p);
        let u = by_user.entry(c.user.clone()).or_default();
        u.0.add(c, &p);
        u.1.entry(c.kind.clone()).or_default().add(c, &p);
        by_kind.entry(c.kind.clone()).or_default().add(c, &p);
        by_model.entry(c.model.clone()).or_default().add(c, &p);
        by_day.entry(c.at.with_timezone(&Local).format("%Y-%m-%d").to_string()).or_default().add(c, &p);
    }
    let mut users: Vec<Value> = by_user.into_iter().map(|(u, (t, kinds))| json!({ "user": u, "total": t, "kinds": kinds })).collect();
    users.sort_by(|a, b| b["total"]["input"].as_u64().unwrap_or(0).saturating_add(b["total"]["output"].as_u64().unwrap_or(0)).cmp(&a["total"]["input"].as_u64().unwrap_or(0).saturating_add(a["total"]["output"].as_u64().unwrap_or(0))));
    json!({
        "days": days,
        "currency": p.currency,
        "priced": p.input > 0.0 || p.output > 0.0,
        "total": all,
        "users": users,
        "kinds": by_kind,
        "models": by_model,
        "daily": by_day.into_iter().map(|(d, t)| json!({ "day": d, "tokens": t.input + t.output, "calls": t.calls })).collect::<Vec<_>>(),
    })
}

fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// `/usage [days]`, with names for ids.
pub fn describe(days: i64, only: Option<&str>, name: &dyn Fn(&str) -> String) -> String {
    let s = summary(days, only);
    let line = |t: &Value| {
        let cost = t["cost"].as_f64().unwrap_or(0.0);
        format!(
            "{} calls · {} in ({} cached) · {} out{}",
            t["calls"].as_u64().unwrap_or(0),
            tokens(t["input"].as_u64().unwrap_or(0)),
            tokens(t["cached"].as_u64().unwrap_or(0)),
            tokens(t["output"].as_u64().unwrap_or(0)),
            if s["priced"] == true { format!(" · ≈{cost:.2} {}", s["currency"].as_str().unwrap_or("")) } else { String::new() }
        )
    };
    let mut out = vec![format!("AI usage, last {days} day{}: {}", if days == 1 { "" } else { "s" }, line(&s["total"]))];
    if only.is_none() {
        for u in s["users"].as_array().into_iter().flatten() {
            out.push(format!("  {}: {}", name(u["user"].as_str().unwrap_or("?")), line(&u["total"])));
        }
    }
    for (k, t) in s["kinds"].as_object().into_iter().flatten() {
        out.push(format!("  · {k}: {}", line(t)));
    }
    for (m, t) in s["models"].as_object().into_iter().flatten() {
        out.push(format!("  · model {m}: {}", line(t)));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_add_up_and_cost_uses_the_cache_price() {
        let p = Prices { input: 1.0, cached: 0.1, output: 2.0, currency: "USD".into() };
        let mut t = Total::default();
        let c = Call { at: Utc::now(), user: "u".into(), kind: "chat".into(), model: "m".into(), input: 1_000_000, cached: 500_000, output: 1_000_000, ms: 10 };
        t.add(&c, &p);
        t.add(&c, &p);
        assert_eq!((t.calls, t.input, t.cached, t.output), (2, 2_000_000, 1_000_000, 2_000_000));
        // Each: 0.5M fresh × 1 + 0.5M cached × 0.1 + 1M out × 2 = 2.55
        assert!((t.cost - 5.1).abs() < 1e-9, "{}", t.cost);
        assert_eq!(tokens(1_234), "1.2k");
        assert_eq!(tokens(2_500_000), "2.5M");
    }
}
