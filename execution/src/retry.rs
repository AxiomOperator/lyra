//! Failure classification and retry decisions (P6). Retrying and replanning
//! are separate: retries handle transient trouble with the same step;
//! replanning changes the plan.

use std::time::Duration;

use crate::model::{FailureClass, RetryPolicy};

/// Guess the class of a failure from its error text.
pub fn classify(error: &str) -> FailureClass {
    let e = error.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if any(&["timed out", "timeout", "deadline exceeded"]) {
        FailureClass::Timeout
    } else if any(&["rate limit", "429", "too many requests", "quota"]) {
        FailureClass::RateLimit
    } else if any(&["permission", "denied", "forbidden", "unauthorized", "401", "403", "not allowed", "authentication"]) {
        FailureClass::Permission
    } else if any(&["bad arguments", "invalid", "missing field", "unknown tool", "unknown kind", "usage:", "parse"]) {
        FailureClass::InvalidInput
    } else if any(&["not found", "no such", "doesn't exist", "does not exist", "no memory", "no skill", "dependency"]) {
        FailureClass::Dependency
    } else if any(&["connection", "refused", "reset", "unavailable", "503", "502", "500", "temporar", "try again", "broken pipe"]) {
        FailureClass::Transient
    } else if any(&["not verified", "verification"]) {
        FailureClass::Verification
    } else {
        FailureClass::Unknown
    }
}

/// Whether another attempt makes sense after `attempts` tries.
pub fn should_retry(policy: &RetryPolicy, class: FailureClass, attempts: u32) -> bool {
    attempts < policy.max_attempts && policy.retry_on.contains(&class)
}

/// Exponential backoff: `backoff_ms`, doubled for each attempt after the first, capped at a minute.
pub fn backoff(policy: &RetryPolicy, attempts: u32) -> Duration {
    let factor = 1u64 << attempts.saturating_sub(1).min(6);
    Duration::from_millis((policy.backoff_ms * factor).min(60_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_errors() {
        assert_eq!(classify("operation timed out after 30s"), FailureClass::Timeout);
        assert_eq!(classify("HTTP 429 Too Many Requests"), FailureClass::RateLimit);
        assert_eq!(classify("permission denied: /etc/shadow"), FailureClass::Permission);
        assert_eq!(classify("bad arguments: missing field `id`"), FailureClass::InvalidInput);
        assert_eq!(classify("no memory matches \"abcd\""), FailureClass::Dependency);
        assert_eq!(classify("error sending request: connection refused"), FailureClass::Transient);
        assert_eq!(classify("something odd"), FailureClass::Unknown);
    }

    #[test]
    fn retries_transient_trouble_but_not_permission_errors() {
        let p = RetryPolicy::default();
        assert!(should_retry(&p, FailureClass::Timeout, 1));
        assert!(!should_retry(&p, FailureClass::Timeout, 3), "out of attempts");
        assert!(!should_retry(&p, FailureClass::Permission, 1), "authentication failures aren't retried blindly");
        assert_eq!(backoff(&p, 1).as_millis(), 1000);
        assert_eq!(backoff(&p, 3).as_millis(), 4000);
        assert_eq!(backoff(&RetryPolicy { backoff_ms: 50_000, ..p }, 5).as_millis(), 60_000);
    }
}
