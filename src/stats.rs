use std::time::Duration;

use serde::Deserialize;

/// Token counts from the server's final `usage` chunk.
#[derive(Clone, Copy, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Clone, Copy, Deserialize)]
pub struct PromptDetails {
    /// Prompt tokens served from the cache; included in `prompt_tokens`.
    #[serde(default)]
    pub cached_tokens: u64,
}

impl Usage {
    pub fn cached(&self) -> u64 {
        self.prompt_tokens_details.map_or(0, |d| d.cached_tokens)
    }
}

/// Measurements for one reply.
/// The chat model's context window in tokens (0: not known yet), asked of
/// its server once at startup.
pub static CONTEXT_WINDOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Ask the chat model's server for its context window (llama.cpp's
/// `/models` meta or `/props`; vLLM's `max_model_len`), in the background.
pub fn learn_context_window(url: &str) {
    let base = url.trim_end_matches('/').to_string();
    std::thread::spawn(move || {
        let get = |u: &str| -> Option<serde_json::Value> {
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(10)).build().ok()?.get(u).send().ok()?.json().ok()
        };
        let models = get(&format!("{base}/models"));
        let n = models
            .as_ref()
            .and_then(|m| m["data"][0]["meta"]["n_ctx"].as_u64().or_else(|| m["data"][0]["max_model_len"].as_u64()))
            .or_else(|| {
                let root = base.strip_suffix("/v1").unwrap_or(&base);
                let p = get(&format!("{root}/props"))?;
                p["default_generation_settings"]["n_ctx"].as_u64().or_else(|| p["n_ctx"].as_u64())
            });
        if let Some(n) = n {
            CONTEXT_WINDOW.store(n, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

pub struct Stats {
    /// Time from sending the request to the first token (reasoning or content).
    pub ttft: Option<Duration>,
    /// Time from sending the request to the end of the stream.
    pub elapsed: Duration,
    /// Prompt tokens, including cached ones.
    pub input: u64,
    /// The part of `input` served from the prompt cache.
    pub cached: u64,
    pub output: u64,
    /// The server sent no usage: `input` is unknown and `output` is a chunk count.
    pub estimated: bool,
}

impl Stats {
    /// Fold in another request from the same turn (e.g. after a tool call).
    pub fn absorb(&mut self, other: Stats) {
        self.ttft = self.ttft.or(other.ttft);
        self.elapsed += other.elapsed;
        self.input += other.input;
        self.cached += other.cached;
        self.output += other.output;
        self.estimated |= other.estimated;
    }

    /// Generation speed: tokens after the first, over the time after the first.
    pub fn tokens_per_sec(&self) -> Option<f64> {
        let generating = self.elapsed.checked_sub(self.ttft?)?.as_secs_f64();
        (generating > 0.0 && self.output > 1).then(|| (self.output - 1) as f64 / generating)
    }
}

/// Prices per million tokens, from the config file.
pub struct Pricing {
    pub input_per_mtok: f64,
    pub cached_per_mtok: f64,
    pub output_per_mtok: f64,
    pub currency: String,
}

impl Pricing {
    /// `input` includes `cached`, which is billed at the cache rate instead.
    pub fn cost(&self, input: u64, cached: u64, output: u64) -> f64 {
        let uncached = input.saturating_sub(cached) as f64;
        (uncached * self.input_per_mtok
            + cached as f64 * self.cached_per_mtok
            + output as f64 * self.output_per_mtok)
            / 1_000_000.0
    }

    /// What the cached tokens would have cost extra at the full input rate.
    pub fn savings(&self, cached: u64) -> f64 {
        cached as f64 * (self.input_per_mtok - self.cached_per_mtok) / 1_000_000.0
    }

    /// Four decimals, or six for tiny non-zero amounts so they don't read as zero.
    pub fn format(&self, cost: f64) -> String {
        let decimals = if cost > 0.0 && cost < 0.0001 { 6 } else { 4 };
        format!("{}{cost:.decimals$}", self.currency)
    }
}

/// Totals across every reply in the session.
#[derive(Default)]
pub struct Totals {
    pub replies: u64,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    /// At least one reply had no server-reported usage.
    pub estimated: bool,
    /// Learning reviews: their tokens are included in the totals above, and
    /// tracked here too so their share can be shown.
    pub reviews: u64,
    pub review_input: u64,
    pub review_cached: u64,
    pub review_output: u64,
    ttft_sum: Duration,
    ttft_count: u32,
}

impl Totals {
    pub fn add(&mut self, stats: &Stats) {
        self.replies += 1;
        self.input += stats.input;
        self.cached += stats.cached;
        self.output += stats.output;
        self.estimated |= stats.estimated;
        if let Some(ttft) = stats.ttft {
            self.ttft_sum += ttft;
            self.ttft_count += 1;
        }
    }

    /// Count a learning review's request (not a reply, so no TTFT or reply count).
    pub fn add_review(&mut self, usage: Option<&Usage>) {
        self.reviews += 1;
        if let Some(u) = usage {
            self.input += u.prompt_tokens;
            self.cached += u.cached();
            self.output += u.completion_tokens;
            self.review_input += u.prompt_tokens;
            self.review_cached += u.cached();
            self.review_output += u.completion_tokens;
        }
    }

    pub fn avg_ttft(&self) -> Option<Duration> {
        (self.ttft_count > 0).then(|| self.ttft_sum / self.ttft_count)
    }
}

/// `1234567` -> `"1,234,567"`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Share of `part` in `whole` as a percentage, or `None` if `whole` is 0.
pub fn percent(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| part as f64 * 100.0 / whole as f64)
}

pub fn secs(d: Duration) -> String {
    format!("{:.2}s", d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> Pricing {
        Pricing {
            input_per_mtok: 0.50,
            cached_per_mtok: 0.05,
            output_per_mtok: 1.50,
            currency: "$".into(),
        }
    }

    #[test]
    fn cached_tokens_billed_at_cache_rate() {
        // 34 uncached + 82 cached input, 322 output (from a live run).
        let cost = pricing().cost(116, 82, 322);
        let expected = (34.0 * 0.50 + 82.0 * 0.05 + 322.0 * 1.50) / 1e6;
        assert!((cost - expected).abs() < 1e-12);
    }

    #[test]
    fn savings_are_the_rate_difference() {
        assert!((pricing().savings(1_000_000) - 0.45).abs() < 1e-12);
    }

    #[test]
    fn tiny_costs_do_not_round_to_zero() {
        let p = pricing();
        assert_eq!(p.format(0.0), "$0.0000");
        assert_eq!(p.format(0.0000041), "$0.000004");
        assert_eq!(p.format(0.000504), "$0.0005");
    }

    #[test]
    fn usage_parses_cached_tokens() {
        let with: Usage = serde_json::from_str(
            r#"{"prompt_tokens":74,"completion_tokens":35,"prompt_tokens_details":{"cached_tokens":70}}"#,
        )
        .unwrap();
        assert_eq!(with.cached(), 70);
        let without: Usage =
            serde_json::from_str(r#"{"prompt_tokens":5,"completion_tokens":1}"#).unwrap();
        assert_eq!(without.cached(), 0);
    }

    #[test]
    fn reviews_count_toward_totals_but_not_replies() {
        let mut totals = Totals::default();
        let usage: Usage = serde_json::from_str(
            r#"{"prompt_tokens":900,"completion_tokens":100,"prompt_tokens_details":{"cached_tokens":600}}"#,
        )
        .unwrap();
        totals.add_review(Some(&usage));
        totals.add_review(None);
        assert_eq!((totals.replies, totals.reviews), (0, 2));
        assert_eq!((totals.input, totals.cached, totals.output), (900, 600, 100));
        assert_eq!((totals.review_input, totals.review_output), (900, 100));
        assert!(totals.avg_ttft().is_none());
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
