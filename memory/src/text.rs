//! Word handling shared by search and ranking.

/// Words too common to say anything about which memory is relevant.
pub const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "did", "do",
    "does", "for", "from", "had", "has", "have", "how", "i", "if", "in", "is", "it", "its", "just",
    "me", "my", "of", "on", "or", "our", "please", "should", "so", "that", "the", "then", "there",
    "this", "to", "us", "was", "we", "were", "what", "when", "where", "which", "who", "will",
    "with", "would", "you", "your",
];

/// Lowercase words worth matching on: no stopwords, no single letters.
pub fn content_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Same word, allowing for endings: "deploy" matches "deploys" and "deployment".
fn same_word(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (short.len() >= 4 && long.starts_with(short))
}

/// Share of the query's content words that `text` contains (0–1). An absolute
/// measure: a weak match stays weak even when it's the best one there is.
pub fn coverage(query: &str, text: &str) -> f32 {
    let mut terms = content_words(query);
    terms.sort();
    terms.dedup();
    if terms.is_empty() {
        return 0.0;
    }
    let words = content_words(text);
    let found = terms.iter().filter(|t| words.iter().any(|w| same_word(t, w))).count();
    found as f32 / terms.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_absolute() {
        assert_eq!(coverage("billing api port", "The billing API listens on port 8080"), 1.0);
        assert!((coverage("agent runtime language", "The agent runtime will be written in Rust.") - 2.0 / 3.0).abs() < 1e-6);
        // Shares only filler with the memory: no match at all.
        assert_eq!(coverage("When do we usually ship releases? One sentence, don't use tools.", "The user's billing API listens on port 8080"), 0.0);
        assert_eq!(coverage("deploy", "Deploys happen on Fridays"), 1.0, "word endings");
        assert_eq!(coverage("the of and", "anything"), 0.0);
    }
}
