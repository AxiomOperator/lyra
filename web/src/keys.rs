//! Keys for model servers (a gateway like Syntra, or a server that wants
//! one): each configured base URL and its key, set when lyra's config is
//! read. A request to a model server gets `Authorization: Bearer <key>` when
//! its URL is under one with a key; without a key nothing is added, so
//! servers that don't ask for one work as before. Keys are never logged.

use std::sync::RwLock;

/// (base URL without its trailing slash, key).
static KEYS: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

/// Every model server's key, replacing what was there (empty keys are left out).
pub fn set_all(list: Vec<(String, String)>) {
    let mut keys: Vec<(String, String)> = list.into_iter().map(|(u, k)| (u.trim().trim_end_matches('/').to_string(), k.trim().to_string())).filter(|(u, k)| !u.is_empty() && !k.is_empty()).collect();
    // The most specific address first.
    keys.sort_by_key(|(u, _)| std::cmp::Reverse(u.len()));
    *KEYS.write().unwrap_or_else(|e| e.into_inner()) = keys;
}

/// The key for a request to `url`, if its server has one.
pub fn for_url(url: &str) -> Option<String> {
    let url = url.trim();
    KEYS.read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(base, _)| url == base || url.strip_prefix(base.as_str()).is_some_and(|rest| rest.starts_with('/') || rest.starts_with('?')))
        .map(|(_, k)| k.clone())
}

/// `req` (to `url`) with its server's key, when it has one.
pub fn auth(req: reqwest::blocking::RequestBuilder, url: &str) -> reqwest::blocking::RequestBuilder {
    match for_url(url) {
        Some(k) => req.bearer_auth(k),
        None => req,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_only_to_its_own_server() {
        set_all(vec![("http://gw:4000/v1/".into(), "k1".into()), ("http://gw:4000/v1/native/decide".into(), "k2".into()), ("http://plain:8080/v1".into(), "".into())]);
        assert_eq!(for_url("http://gw:4000/v1/chat/completions").as_deref(), Some("k1"));
        assert_eq!(for_url("http://gw:4000/v1/native/decide/systemone").as_deref(), Some("k2"), "the most specific");
        assert_eq!(for_url("http://gw:4000/v1"), Some("k1".into()));
        assert_eq!(for_url("http://gw:4000/v10/x"), None, "not a lookalike");
        assert_eq!(for_url("http://plain:8080/v1/models"), None, "no key: none sent");
        set_all(vec![]);
        assert_eq!(for_url("http://gw:4000/v1/models"), None);
    }
}
