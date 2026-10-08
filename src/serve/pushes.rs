//! Push notifications: what an event or a check is worth telling, and to whom.


use super::*;

/// A notification an event deserves (approvals, failures), if any.
pub(crate) fn notification(event: &StreamEvent) -> Option<Notification> {
    match event {
        StreamEvent::Approval(r) => Some(Notification {
            title: format!("{} needs your OK", r.agent),
            body: format!("{}: {}{}", r.what, r.detail.lines().next().unwrap_or(""), if r.dangerous { format!(" — {}", r.why) } else { String::new() }),
            tag: format!("approval-{}", r.id),
            approval: Some(r.id),
            url: None, actions: vec![], reference: None,
        to: To::Admins,
        }),
        StreamEvent::Error(e) => Some(Notification { title: "lyra hit an error".into(), body: preview(e), tag: "error".into(), approval: None, url: None, actions: vec![], reference: None, to: To::Admins }),
        StreamEvent::PlanFinished(result) => Some(Notification {
            title: "Plan update".into(),
            body: match result {
                Ok(_) => "A plan stopped running — open lyra to see how it went.".into(),
                Err(e) => preview(e),
            },
            tag: "plan".into(),
            approval: None,
            url: None, actions: vec![], reference: None,
        to: To::Admins,
        }),
        _ => None,
    }
}

/// Log a health alert, and push it when `[health] notify` is on.
pub(crate) fn alert(app: &mut App, hub: &Hub, machine: &str, text: &str, problem: bool) {
    app.log(if problem { Level::Error } else { Level::Agent }, format!("{} {text}", if problem { "⚠" } else { "✓" }));
    if crate::health::settings().notify {
        hub.notify(Notification {
            title: if problem { format!("⚠ {machine} needs a look") } else { format!("✓ {machine}") },
            body: text.to_string(),
            tag: format!("health-{}", machine.to_lowercase()),
            approval: None,
            url: None, actions: vec![], reference: None,
        to: To::Admins,
        });
    }
}

/// A machine's (or the server's) health report: new problems are logged and
/// pushed, cleared ones logged and pushed as fixed.
pub(crate) fn health_report(app: &mut App, hub: &Hub, alerts: &mut crate::health::Alerts, name: &str, h: &Value) {
    let change = alerts.report(name, crate::health::problems(h, &crate::health::settings()));
    if !change.new.is_empty() {
        alert(app, hub, name, &format!("{name}: {}", change.new.join("; ")), true);
    }
    if !change.cleared.is_empty() {
        alert(app, hub, name, &format!("{name} is fine again: {}", change.cleared.join("; ")), false);
    }
    // New problems are researched by themselves; cleared ones are marked so.
    let d = app.diagnose.clone();
    for (key, text) in &change.new_keys {
        // Nothing runs on another machine unless the user asks: theirs wait for "look into it".
        if d.enabled && d.auto && name.eq_ignore_ascii_case(crate::caps::HERE) {
            crate::diagnose::queue(&format!("{}:{key}", name.to_lowercase()), name, text, false);
        }
    }
    for key in &change.cleared_keys {
        crate::diagnose::resolve(&format!("{}:{key}", name.to_lowercase()));
    }
}
