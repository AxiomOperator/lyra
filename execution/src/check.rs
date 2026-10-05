//! Deterministic verification helpers (P5, C9): fill a follow-up call's
//! arguments from the step being verified, and test its output.

use serde_json::Value;

/// A value at a dotted path (`id`, `data.task.id`, `items.0.name`).
pub fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').filter(|p| !p.is_empty()).try_fold(v, |node, key| match node {
        Value::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => node.get(key),
    })
}

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// Replace `{{result.path}}` and `{{args.path}}` in strings with values from
/// the step's output and arguments. A string that is only a placeholder takes
/// the value as is (keeping numbers and objects).
pub fn fill(template: &Value, result: &Value, args: &Value) -> Value {
    let lookup = |name: &str| -> Option<Value> {
        let name = name.trim();
        if let Some(p) = name.strip_prefix("result.") {
            at(result, p).cloned()
        } else if name == "result" {
            Some(result.clone())
        } else {
            name.strip_prefix("args.").and_then(|p| at(args, p).cloned())
        }
    };
    match template {
        Value::String(s) => {
            let t = s.trim();
            if let Some(inner) = t.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
                && !inner.contains("{{")
                && let Some(v) = lookup(inner)
            {
                return v;
            }
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(start) = rest.find("{{") {
                out += &rest[..start];
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else {
                    out += &rest[start..];
                    rest = "";
                    break;
                };
                out += &lookup(&after[..end]).map_or_else(|| format!("{{{{{}}}}}", &after[..end]), |v| text(&v));
                rest = &after[end + 2..];
            }
            out += rest;
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| fill(v, result, args)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), fill(v, result, args))).collect()),
        other => other.clone(),
    }
}

/// Test `output` against an expression: `exists:path`, `equals:path=value`,
/// `contains:text`, or plain text (contains, case-insensitive). Empty passes.
/// Returns whether it holds and a reason.
pub fn check(output: &Value, expression: &str) -> (bool, String) {
    let e = expression.trim();
    if e.is_empty() {
        return (true, "the check call succeeded".into());
    }
    let contains = |needle: &str| text(output).to_lowercase().contains(&needle.to_lowercase());
    if let Some(path) = e.strip_prefix("exists:") {
        let ok = at(output, path.trim()).is_some_and(|v| !v.is_null());
        return (ok, format!("{} {}", path.trim(), if ok { "is present" } else { "is missing" }));
    }
    if let Some(rest) = e.strip_prefix("equals:") {
        let (path, want) = rest.split_once('=').unwrap_or((rest, ""));
        let got = at(output, path.trim()).map(text);
        let ok = got.as_deref() == Some(want.trim());
        return (ok, format!("{} is {:?}, expected {:?}", path.trim(), got.unwrap_or_default(), want.trim()));
    }
    let needle = e.strip_prefix("contains:").unwrap_or(e).trim();
    let ok = contains(needle);
    (ok, format!("output {} {needle:?}", if ok { "contains" } else { "doesn't contain" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn templates_and_checks() {
        let result = json!({ "id": "t-1", "n": 3 });
        let args = json!({ "project": "p9", "content": "Fix it" });
        let t = json!({ "id": "{{result.id}}", "n": "{{result.n}}", "path": "/p/{{args.project}}/t/{{result.id}}" });
        assert_eq!(fill(&t, &result, &args), json!({ "id": "t-1", "n": 3, "path": "/p/p9/t/t-1" }));
        let out = json!({ "id": "t-1", "status": "open", "title": "Fix it" });
        assert!(check(&out, "exists:id").0 && !check(&out, "exists:owner").0);
        assert!(check(&out, "equals:status=open").0 && !check(&out, "equals:status=done").0);
        assert!(check(&out, "contains:fix IT").0 && check(&out, "").0);
        assert_eq!(at(&json!({"a": [{"b": 1}]}), "a.0.b"), Some(&json!(1)));
    }
}
