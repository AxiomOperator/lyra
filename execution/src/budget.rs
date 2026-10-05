//! Budgets (P11): limits the runtime enforces while a plan runs.

use crate::model::{Budget, BudgetUsage};

/// The first limit that's been reached, if any.
pub fn exhausted(b: &Budget, u: &BudgetUsage) -> Option<String> {
    let over = |used: u64, max: Option<u64>| max.is_some_and(|m| used >= m);
    if over(u.model_calls as u64, b.max_model_calls.map(u64::from)) {
        return Some(format!("model call budget used up ({})", u.model_calls));
    }
    if over(u.tool_calls as u64, b.max_tool_calls.map(u64::from)) {
        return Some(format!("tool call budget used up ({})", u.tool_calls));
    }
    if over(u.tokens, b.max_tokens) {
        return Some(format!("token budget used up ({})", u.tokens));
    }
    if over(u.seconds, b.max_minutes.map(|m| m as u64 * 60)) {
        return Some(format!("time budget used up ({} min)", u.seconds / 60));
    }
    None
}

/// Whether another replan is allowed.
pub fn can_replan(b: &Budget, u: &BudgetUsage) -> bool {
    b.max_replans.is_none_or(|m| u.replans < m)
}

/// Whether any limit is at 80% or more (the planner is asked to keep it short).
pub fn running_low(b: &Budget, u: &BudgetUsage) -> bool {
    let near = |used: u64, max: Option<u64>| max.is_some_and(|m| m > 0 && used * 5 >= m * 4);
    near(u.model_calls as u64, b.max_model_calls.map(u64::from))
        || near(u.tool_calls as u64, b.max_tool_calls.map(u64::from))
        || near(u.tokens, b.max_tokens)
        || near(u.seconds, b.max_minutes.map(|m| m as u64 * 60))
}

/// `model 8/40 · tools 14/100 · replans 1/3 · tokens 31k/300k`.
pub fn describe(b: &Budget, u: &BudgetUsage) -> String {
    let of = |used: u64, max: Option<u64>| match max {
        Some(m) => format!("{used}/{m}"),
        None => used.to_string(),
    };
    let k = |n: u64| if n >= 1000 { format!("{}k", n / 1000) } else { n.to_string() };
    let tokens = match b.max_tokens {
        Some(m) => format!("{}/{}", k(u.tokens), k(m)),
        None => k(u.tokens),
    };
    format!(
        "model {} · tools {} · replans {} · tokens {tokens}",
        of(u.model_calls as u64, b.max_model_calls.map(u64::from)),
        of(u.tool_calls as u64, b.max_tool_calls.map(u64::from)),
        of(u.replans as u64, b.max_replans.map(u64::from)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_enforced() {
        let b = Budget { max_model_calls: Some(10), max_tool_calls: None, max_replans: Some(1), max_minutes: Some(1), max_tokens: Some(1000) };
        let mut u = BudgetUsage::default();
        assert_eq!(exhausted(&b, &u), None);
        assert!(!running_low(&b, &u));
        u.model_calls = 8;
        assert!(running_low(&b, &u));
        u.model_calls = 10;
        assert!(exhausted(&b, &u).unwrap().contains("model call"));
        u.model_calls = 0;
        u.seconds = 61;
        assert!(exhausted(&b, &u).unwrap().contains("time"));
        assert!(can_replan(&b, &u));
        u.replans = 1;
        assert!(!can_replan(&b, &u));
        assert_eq!(describe(&b, &BudgetUsage { tokens: 31_000, ..u }), "model 0/10 · tools 0 · replans 1/1 · tokens 31k/1k");
    }
}
