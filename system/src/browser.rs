//! What a sandboxed browser may do on its own (`lyra_node::sandbox`): the
//! same rule on the server (before a call is sent) and on the machine (before
//! the browser does it). Reading and moving about run by themselves; sending
//! a form waits for the person's yes; payment details and file uploads never
//! happen. `element` is the page element the action is on, as the browser's
//! last read described it (`kind`, `submits`, `payment`, `file`).

use serde_json::Value;

use crate::Check;

/// Whether `action` on `element` may run (`approved`: the person said yes).
pub fn check(action: &str, element: Option<&Value>, approved: bool) -> Check {
    let flag = |k: &str| element.is_some_and(|e| e[k] == true);
    match action {
        "open" | "read" | "screenshot" | "close" | "back" => Check::Auto,
        "fill" | "login" if flag("payment") => Check::Forbidden("that's a payment field: lyra never enters card or payment details".into()),
        "click" | "fill" | "submit" if flag("file") => Check::Forbidden("that's a file upload: lyra doesn't send files from the sandbox".into()),
        "fill" => Check::Auto,
        // Signing in uses a saved password: always the person's yes.
        "login" if approved => Check::Auto,
        "login" => Check::Ask { why: "signs in with your saved password for this site".into(), dangerous: false },
        "click" if !flag("submits") => Check::Auto,
        "click" | "submit" if approved => Check::Auto,
        "click" | "submit" => Check::Ask { why: "sends a form on the page (others may see what it sends)".into(), dangerous: false },
        other => Check::Forbidden(format!("the browser can't {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reading_runs_sending_waits_and_payments_never() {
        let link = json!({ "kind": "link" });
        let send = json!({ "kind": "button", "submits": true });
        let card = json!({ "kind": "field", "payment": true });
        let upload = json!({ "kind": "field", "file": true });
        assert!(matches!(check("open", None, false), Check::Auto));
        assert!(matches!(check("click", Some(&link), false), Check::Auto), "following a link");
        assert!(matches!(check("fill", Some(&json!({ "kind": "field" })), false), Check::Auto));
        assert!(matches!(check("click", Some(&send), false), Check::Ask { .. }), "a button that sends the form");
        assert!(matches!(check("submit", None, false), Check::Ask { .. }));
        assert!(matches!(check("submit", None, true), Check::Auto), "once the person said yes");
        assert!(matches!(check("fill", Some(&card), true), Check::Forbidden(_)), "never, even approved");
        assert!(matches!(check("click", Some(&upload), true), Check::Forbidden(_)));
        assert!(matches!(check("login", None, false), Check::Ask { .. }));
        assert!(matches!(check("download", None, true), Check::Forbidden(_)));
    }
}
