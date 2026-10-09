//! What's new: `CHANGELOG.json` (newest release first), built into lyra.
//! Versions are major.minor.fix.build: build counts every change and goes back
//! to 0 only with a new major; a new minor resets fix. The app's What's new
//! page and the version shown everywhere come from here, and so does
//! `lyra_changelog`, the tool lyra answers "when did X come in?" with.

use std::sync::LazyLock;

use lyra_capabilities::model::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const RAW: &str = include_str!("../CHANGELOG.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub improved: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<String>,
}

impl Release {
    /// Every word of `query` is somewhere in it (its version, date, title or lines), ignoring case.
    pub fn matches(&self, query: &str) -> bool {
        let text = [&self.version, &self.date, &self.title].into_iter().chain(&self.new).chain(&self.improved).chain(&self.fixed).map(|s| s.to_lowercase()).collect::<Vec<_>>().join("\n");
        query.to_lowercase().split_whitespace().all(|w| text.contains(w))
    }
}

static ALL: LazyLock<Vec<Release>> = LazyLock::new(|| serde_json::from_str(RAW).unwrap_or_default());

/// Every release, newest first.
pub fn all() -> &'static [Release] {
    &ALL
}

/// Words that say nothing about which feature is meant.
const FILLER: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "for", "with", "is", "was", "were", "be", "been", "it", "its", "did", "do", "does", "when", "what", "which", "how",
    "lyra", "lyra's", "feature", "features", "added", "add", "included", "include", "introduced", "come", "came", "get", "got", "new", "version", "release", "released", "change",
    "changed", "able", "can", "could", "we", "i", "my", "you", "your", "there", "that", "this", "ability", "support", "first",
];

/// A word without its common endings ("passwords" finds "password").
fn stem(w: &str) -> String {
    let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
    for end in ["ing", "ed", "es", "s"] {
        if let Some(s) = w.strip_suffix(end).filter(|s| s.chars().count() >= 4) {
            return s.to_string();
        }
    }
    w.to_string()
}

/// The releases about `query`, best first, with the lines about it and a
/// score. A line with most of the words counts most, then the title, then
/// the words side by side as in the question ("chat only"); ties go to the
/// earliest release (where it began).
pub fn search(query: &str, limit: usize) -> Vec<(&'static Release, Vec<String>, usize)> {
    let raw: Vec<String> = query.to_lowercase().split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '\'').to_string()).filter(|w| !w.is_empty()).collect();
    let words: Vec<String> = raw.iter().filter(|w| !FILLER.contains(&w.as_str())).map(|w| stem(w)).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return all().iter().take(limit).map(|r| (r, Vec::new(), 0)).collect();
    }
    // Words next to each other in the question, at least one of them meaningful.
    const SMALL: &[&str] = &["a", "an", "the", "to", "of", "in", "on", "for", "with", "and", "or", "is", "was", "we", "i", "it", "did", "get"];
    let pairs: Vec<String> = raw
        .windows(2)
        .filter(|p| !SMALL.contains(&p[0].as_str()) && !SMALL.contains(&p[1].as_str()) && !(FILLER.contains(&p[0].as_str()) && FILLER.contains(&p[1].as_str())))
        .map(|p| format!("{} {}", p[0], stem(&p[1])))
        .collect();
    let covered = |text: &str| words.iter().filter(|w| text.contains(w.as_str())).count();
    let mut found: Vec<_> = all()
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let title = r.title.to_lowercase();
            let lines: Vec<(String, &String)> = r.new.iter().chain(&r.improved).chain(&r.fixed).map(|l| (l.to_lowercase(), l)).collect();
            let best_line = lines.iter().map(|(l, _)| covered(l)).max().unwrap_or(0);
            let head = covered(&format!("{} {} {title}", r.version, r.date));
            let phrases = pairs.iter().filter(|p| title.contains(p.as_str()) || lines.iter().any(|(l, _)| l.contains(p.as_str()))).count();
            let score = best_line * 3 + head * 3 + phrases * 4;
            // The lines with the most of the words.
            let about: Vec<String> = lines.iter().filter(|(l, _)| best_line > 0 && covered(l) == best_line).map(|(_, l)| (*l).clone()).collect();
            (best_line + head > 0).then_some((i, r, about, score))
        })
        .collect();
    found.sort_by_key(|(i, _, _, score)| (std::cmp::Reverse(*score), std::cmp::Reverse(*i)));
    let best = found.first().map_or(0, |f| f.3);
    found.into_iter().filter(|f| f.3 * 2 >= best).take(limit).map(|(_, r, lines, score)| (r, lines, score)).collect()
}

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "lyra_changelog",
        CapabilityKind::NativeTool,
        "lyra's own release history (What's new): when a feature or fix came in, which version, and what changed. Use it for \"when was X added?\", \"what changed lately?\" or \"what version is this?\". Answer with the version and date of the release that brought it.",
        RiskLevel::ReadOnly,
    );
    c.input_schema = json!({ "type": "object", "properties": {
        "query": { "type": "string", "description": "The feature or change, in a few words (e.g. \"password sign in\", \"fallback model\"). Empty: the latest releases." },
    } });
    c.source = "changelog".into();
    c.tags = ["changelog", "release", "version", "when", "added", "feature", "update", "history", "new", "lyra"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    if name != "lyra_changelog" {
        return Err(format!("{name} isn't a changelog tool"));
    }
    let query = args["query"].as_str().unwrap_or("");
    let found = search(query, 6);
    let releases: Vec<Value> = found
        .iter()
        .map(|(r, lines, _)| {
            // The lines about it, or the whole release when nothing narrower matched.
            let shown = if lines.is_empty() { r.new.iter().chain(&r.improved).chain(&r.fixed).cloned().collect() } else { lines.clone() };
            json!({ "version": r.version, "date": r.date, "title": r.title, "lines": shown })
        })
        .collect();
    // The best match (ties already go to the earliest).
    let earliest = found.first().filter(|f| f.2 > 0);
    Ok(json!({
        "current_version": version(),
        "releases": releases,
        "first_mentioned": earliest.map(|(r, _, _)| json!({ "version": r.version, "date": r.date, "title": r.title })),
        "note": if releases.is_empty() { "nothing in the changelog mentions that: say so, and don't guess a version" } else { "best match first; first_mentioned is the release that brought it (later ones changed it)" },
    }))
}

/// This lyra's version.
pub fn version() -> &'static str {
    ALL.first().map_or("0.0.0.0", |r| r.version.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_finds_when_something_came_in() {
        // Real entries: the first release that mentions it, and its version.
        let v = call("lyra_changelog", &json!({ "query": "When was the fallback model added?" })).unwrap();
        assert!(!v["releases"].as_array().unwrap().is_empty());
        let first = v["first_mentioned"]["version"].as_str().unwrap();
        assert!(all().iter().any(|r| r.version == first && (r.title.to_lowercase().contains("fallback") || r.new.iter().any(|l| l.to_lowercase().contains("fallback")))));
        let latest = call("lyra_changelog", &json!({})).unwrap();
        assert_eq!(latest["releases"][0]["version"], version(), "no query: the latest");
        assert!(call("lyra_changelog", &json!({ "query": "zebra unicorn" })).unwrap()["releases"].as_array().unwrap().is_empty());
        assert_eq!(stem("passwords"), "password");
    }

    #[test]
    fn search_finds_every_word_anywhere() {
        let r = Release { version: "0.42.0.176".into(), date: "2026-10-09".into(), title: "What's new in pages".into(), new: vec![], improved: vec!["Ten releases at a time, with Newer and Older".into()], fixed: vec![] };
        assert!(r.matches("older TEN"));
        assert!(r.matches("0.42"));
        assert!(r.matches(""));
        assert!(!r.matches("older calendar"), "every word must be there");
    }

    /// A version's four numbers.
    fn parts(v: &str) -> Option<[u64; 4]> {
        let n: Vec<u64> = v.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
        n.try_into().ok()
    }

    #[test]
    fn the_changelog_reads_and_its_versions_follow_the_rules() {
        let all: Vec<Release> = serde_json::from_str(RAW).expect("CHANGELOG.json reads");
        assert!(!all.is_empty());
        for r in &all {
            assert!(parts(&r.version).is_some(), "{} isn't major.minor.fix.build", r.version);
            assert!(chrono::NaiveDate::parse_from_str(&r.date, "%Y-%m-%d").is_ok(), "{}: bad date {}", r.version, r.date);
            assert!(!r.title.trim().is_empty() && !(r.new.is_empty() && r.improved.is_empty() && r.fixed.is_empty()), "{} says nothing", r.version);
        }
        // Newest first: each one up from the one before it.
        for w in all.windows(2) {
            let (new, old) = (parts(&w[0].version).unwrap(), parts(&w[1].version).unwrap());
            assert!(w[0].date >= w[1].date, "{} is dated before {}", w[0].version, w[1].version);
            assert!(new[..3] > old[..3] || (new[..3] == old[..3] && new[3] > old[3]), "{} doesn't come after {}", w[0].version, w[1].version);
            if new[0] == old[0] {
                assert!(new[3] > old[3], "{}: the build only goes back with a new major", w[0].version);
                if new[1] > old[1] {
                    assert_eq!(new[2], 0, "{}: a new minor starts its fixes at 0", w[0].version);
                }
            }
        }
        assert_eq!(version(), all[0].version);
    }
}
