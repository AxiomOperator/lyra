//! What's new: `CHANGELOG.json` (newest release first), built into lyra.
//! Versions are major.minor.fix.build: build counts every change and goes back
//! to 0 only with a new major; a new minor resets fix. The app's What's new
//! page and the version shown everywhere come from here.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

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

static ALL: LazyLock<Vec<Release>> = LazyLock::new(|| serde_json::from_str(RAW).unwrap_or_default());

/// Every release, newest first.
pub fn all() -> &'static [Release] {
    &ALL
}

/// This lyra's version.
pub fn version() -> &'static str {
    ALL.first().map_or("0.0.0.0", |r| r.version.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

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
