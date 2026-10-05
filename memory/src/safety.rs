//! The sensitive-data scan every memory passes before it's stored (M16):
//! passwords, tokens, keys, cookies and other credentials are rejected.
//! Mentioning a secret is fine ("rotate passwords monthly"); stating one isn't.

/// Why `text` must not be stored, or `None` if it looks safe.
pub fn scan(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("it contains a private key");
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    for w in &words {
        let w = w.trim_matches(|c: char| "\"'`,;()[]{}<>".contains(c));
        if looks_like_api_key(w) {
            return Some("it contains what looks like an API key or token");
        }
        if looks_random(w) {
            return Some("it contains a long random-looking string (possibly a secret)");
        }
    }
    let lower = text.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "password", "passwd", "passphrase", "pwd", "secret", "api key", "api_key", "apikey",
        "access key", "private key", "client secret", "token", "session cookie", "cookie",
        "credentials", "credential", "pin code", "bearer",
    ];
    for keyword in KEYWORDS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(keyword) {
            let after = &lower[from + pos + keyword.len()..];
            if states_a_value(after, *keyword == "bearer") {
                return Some("it states a password, token or other credential");
            }
            from += pos + keyword.len();
        }
    }
    None
}

/// After a keyword: `: value`, `= value`, ` is value` (or, for `bearer`, just
/// ` value`), where the value looks like a secret rather than a word like
/// "required" or "rotated".
fn states_a_value(after: &str, bare: bool) -> bool {
    let after = after.trim_start_matches(['s', ' ']);
    let rest = if let Some(r) = after.strip_prefix(':').or_else(|| after.strip_prefix('=')) {
        r
    } else if let Some(r) = after.strip_prefix("is ").or_else(|| after.strip_prefix("was ")) {
        r
    } else if bare {
        after
    } else {
        return false;
    };
    let Some(value) = rest.split_whitespace().next() else { return false };
    let value = value.trim_matches(|c: char| "\"'`,.;()".contains(c));
    const ORDINARY: &[&str] = &[
        "required", "needed", "set", "stored", "rotated", "changed", "expired", "missing", "wrong",
        "invalid", "valid", "correct", "incorrect", "empty", "unknown", "secret", "the", "a", "an",
        "in", "on", "not", "never", "always", "handled", "managed", "configured", "used",
    ];
    value.chars().count() >= 4 && !ORDINARY.contains(&value)
}

/// Well-known token shapes: `sk-…`, `ghp_…`, `xoxb-…`, `AKIA…`, JWTs.
fn looks_like_api_key(w: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-", "sk_live_", "sk_test_", "pk_live_", "ghp_", "gho_", "ghs_", "github_pat_", "glpat-",
        "xoxb-", "xoxp-", "xoxa-", "AIza", "AKIA", "ASIA",
    ];
    let tail_len = |p: &str| w.len() - p.len();
    PREFIXES.iter().any(|p| w.starts_with(p) && tail_len(p) >= 16)
        || (w.starts_with("eyJ") && w.len() >= 40 && w.matches('.').count() == 2)
}

/// 32+ characters mixing letters and digits, not a path, URL or UUID.
fn looks_random(w: &str) -> bool {
    w.len() >= 32
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
        && !w.contains('/')
        && !is_uuid(w)
}

fn is_uuid(w: &str) -> bool {
    w.len() == 36 && w.chars().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_stated_secrets() {
        for text in [
            "The database password is hunter2",
            "db password: s3cr3t!",
            "API_KEY=abcd1234efgh",
            "Use token ghp_aBcDeFgHiJkLmNoPqRsTuVwX for GitHub",
            "OpenAI key sk-proj1234567890abcdefgh",
            "AWS AKIAIOSFODNN7EXAMPLE works",
            "Authorization: Bearer abcdef1234567890",
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "-----BEGIN OPENSSH PRIVATE KEY----- abc",
            "recovery code a8f3k29dk3l2k4j5h6g7f8d9s0a1q2w3e4r5",
        ] {
            assert!(scan(text).is_some(), "should reject: {text}");
        }
    }

    #[test]
    fn allows_talking_about_secrets() {
        for text in [
            "The user rotates passwords monthly.",
            "A password is required for the admin panel.",
            "Tokens are stored in the system keyring.",
            "API keys live in Vault, never in the repo.",
            "Memory id 2c9ca857-1f0e-4d0e-9a51-6f4c0f3e8a11 was superseded.",
            "Config lives at /home/user/.config/some-really-long-directory-name-2024/file",
            "The project uses Rust and SQLite.",
        ] {
            assert_eq!(scan(text), None, "should allow: {text}");
        }
    }
}
