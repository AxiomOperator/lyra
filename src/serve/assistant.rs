//! Each person's assistant work, off a turn: reminders (nags) for PMI
//! tasks, plan my day, proactive help (meeting prep, mail triage), the
//! end-of-day recap, "tell me when …" watches, and what the planner did, told
//! once they're out of quiet time. Its own state, its own tick.

use super::*;

pub(crate) struct Assistant {
    last_nag: Instant,
    plan_tx: std::sync::mpsc::Sender<(String, Result<Vec<String>, String>)>,
    plan_rx: std::sync::mpsc::Receiver<(String, Result<Vec<String>, String>)>,
    last_plan: Instant,
    planning: std::collections::HashSet<String>,
    held: std::collections::HashMap<String, Vec<String>>,
    quiet: QuietTimes,
    last_held: Instant,
    pro_tx: std::sync::mpsc::Sender<(String, crate::proactive::Pass)>,
    pro_rx: std::sync::mpsc::Receiver<(String, crate::proactive::Pass)>,
    last_pro: Instant,
    pro_round: u64,
    pro_busy: std::collections::HashSet<String>,
    /// A notification's button (Done / Later on a reminder) comes back here.
    pub(crate) action_tx: std::sync::mpsc::Sender<(String, String, String, Result<String, String>)>,
    action_rx: std::sync::mpsc::Receiver<(String, String, String, Result<String, String>)>,
    recap_tx: std::sync::mpsc::Sender<(String, crate::recap::Recap)>,
    recap_rx: std::sync::mpsc::Receiver<(String, crate::recap::Recap)>,
    last_recap_look: Instant,
    recapping: std::collections::HashSet<String>,
    watch_tx: std::sync::mpsc::Sender<(String, Vec<(String, String)>)>,
    watch_rx: std::sync::mpsc::Receiver<(String, Vec<(String, String)>)>,
    last_watch: Instant,
    watching: std::collections::HashSet<String>,
}

impl Assistant {
    pub(crate) fn new() -> Self {
        let (plan_tx, plan_rx) = std::sync::mpsc::channel();
        let (pro_tx, pro_rx) = std::sync::mpsc::channel();
        let (action_tx, action_rx) = std::sync::mpsc::channel();
        let (recap_tx, recap_rx) = std::sync::mpsc::channel();
        let (watch_tx, watch_rx) = std::sync::mpsc::channel();
        Self {
            last_nag: Instant::now(),
            plan_tx,
            plan_rx,
            // Plan my day: re-planned every 15 minutes in each person's working day.
            last_plan: Instant::now() - Duration::from_secs(3600),
            planning: Default::default(),
            held: Default::default(),
            quiet: QuietTimes::new(),
            last_held: Instant::now(),
            pro_tx,
            pro_rx,
            // Proactive help: a pass every 5 minutes in the working day (mail every other one).
            last_pro: Instant::now() - Duration::from_secs(3600),
            pro_round: 0,
            pro_busy: Default::default(),
            action_tx,
            action_rx,
            recap_tx,
            recap_rx,
            // The recap: looked for once a minute, made once a day per person.
            last_recap_look: Instant::now() - Duration::from_secs(120),
            recapping: Default::default(),
            watch_tx,
            watch_rx,
            // "Tell me when …": looked at every two minutes.
            last_watch: Instant::now(),
            watching: Default::default(),
        }
    }

    /// One look at it all; `everyone` (devices are being told anyway) comes
    /// back true when what they show changed.
    pub(crate) fn tick(&mut self, convs: &mut [Conv], hub: &Hub, pmi: &mut std::collections::HashMap<String, PmiUser>, everyone: bool) -> bool {
        let mut everyone = everyone;
        // Nags: each person's, checked with each new view and every minute.
        if self.last_nag.elapsed() >= Duration::from_secs(60) || everyone {
            self.last_nag = Instant::now();
            let s = crate::pmi::settings();
            let owner = convs[0].app.owner.clone();
            for (user, p) in pmi.iter_mut().filter(|(_, p)| p.state.at.is_some() && p.state.error.is_none()) {
                // Not in their quiet time (outside work hours, in a meeting): nags wait.
                // Not known yet: it's being looked up, and they wait until it is.
                if self.quiet.now(user) != Some(false) {
                    continue;
                }
                let (due, changed) = p.nags.due(&p.state, &s, chrono::Utc::now());
                for n in due {
                    // Only the owner's own reminders in the owner's Activity.
                    if *user == owner {
                        convs[0].app.log(Level::Plan, format!("⏰ still to do: {} (reminder {} of {})", n.title, n.sent, s.nag_max));
                    }
                    hub.notify(Notification {
                        title: format!("⏰ Still to do: {}", n.title),
                        body: format!("The reminder went off {}. Done, or later?", n.fired.with_timezone(&chrono::Local).format("%a %H:%M")),
                        tag: format!("nag-{}", n.task),
                        approval: None,
                        url: Some("/?page=tasks".into()),
                        actions: vec![("done".into(), "Done".into()), ("snooze1h".into(), "In 1 hour".into()), ("tomorrow".into(), "Tomorrow".into())],
                        reference: Some(n.task.clone()),
                        to: To::User(user.clone()),
                    });
                }
                if changed {
                    p.nags.save_for(user);
                }
            }
        }
        while let Ok((user, action, task, r)) = self.action_rx.try_recv() {
            match r {
                Ok(what) => {
                    if user == convs[0].app.owner {
                        convs[0].app.log(Level::Plan, format!("reminder: {what}"));
                    }
                    if let Some(p) = pmi.get_mut(&user) {
                        p.nags.items.retain(|_, n| n.task != task);
                        p.nags.save_for(&user);
                    }
                }
                Err(e) => {
                    if user == convs[0].app.owner {
                        convs[0].app.log(Level::Error, format!("reminder {action} failed: {e}"));
                    }
                    hub.notify(Notification { title: "Couldn't update the task".into(), body: e, tag: format!("nag-{task}"), approval: None, url: Some("/?page=tasks".into()), actions: vec![], reference: None, to: To::User(user) });
                }
            }
        }
        // Plan my day, for everyone with Outlook and PMI, in their working day.
        if self.last_plan.elapsed() >= Duration::from_secs(15 * 60) && crate::planner::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            self.last_plan = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if self.planning.contains(&u) || !crate::graph::connected_for(&u) || !crate::pmi::configured_for(&u) {
                    continue;
                }
                self.planning.insert(u.clone());
                let tx = self.plan_tx.clone();
                crate::acting::spawn(move || {
                    let r = crate::acting::run(&u, crate::planner::run);
                    let _ = tx.send((u, r));
                });
            }
        }
        while let Ok((user, r)) = self.plan_rx.try_recv() {
            self.planning.remove(&user);
            let mine = user == convs[0].app.owner;
            match r {
                Ok(did) if !did.is_empty() => {
                    if mine {
                        for d in &did {
                            convs[0].app.log(Level::Plan, format!("🗓 {d}"));
                        }
                    }
                    self.held.entry(user).or_default().extend(did);
                }
                Ok(_) => {}
                Err(e) if mine => convs[0].app.log(Level::Error, format!("plan my day: {e}")),
                Err(_) => {}
            }
        }
        // Proactive help, for everyone with Outlook connected.
        if self.last_pro.elapsed() >= Duration::from_secs(5 * 60) && crate::proactive::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            self.last_pro = Instant::now();
            self.pro_round += 1;
            let mail_due = self.pro_round % 2 == 1;
            let (url, model) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone());
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if self.pro_busy.contains(&u) || !crate::graph::connected_for(&u) {
                    continue;
                }
                self.pro_busy.insert(u.clone());
                let (tx, url, model) = (self.pro_tx.clone(), url.clone(), model.clone());
                crate::acting::spawn(move || {
                    let p = crate::acting::run(&u, || crate::proactive::pass(&url, &model, mail_due));
                    let _ = tx.send((u, p));
                });
            }
        }
        if self.last_recap_look.elapsed() >= Duration::from_secs(60) && crate::recap::settings().enabled {
            self.last_recap_look = Instant::now();
            let now = chrono::Local::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if self.recapping.contains(&u) || !(crate::graph::connected_for(&u) || crate::pmi::configured_for(&u)) || !crate::recap::due(&u, now) {
                    continue;
                }
                self.recapping.insert(u.clone());
                let tx = self.recap_tx.clone();
                crate::acting::spawn(move || {
                    let r = crate::acting::run(&u, || crate::recap::gather(chrono::Utc::now()));
                    let _ = tx.send((u, r));
                });
            }
        }
        // Bug reports and feature requests: who should hear about what.
        for n in crate::feedback::take_notices() {
            let to = if n.to_admins { To::Admins } else { To::User(n.user.clone().unwrap_or_default()) };
            hub.notify(Notification { title: n.title, body: n.body, tag: "feedback".into(), approval: None, url: Some("/?page=feedback".into()), actions: vec![], reference: None, to });
            everyone = true;
        }
        if self.last_watch.elapsed() >= Duration::from_secs(120) {
            self.last_watch = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if self.watching.contains(&u) || !crate::watches::any(&u) {
                    continue;
                }
                self.watching.insert(u.clone());
                let tx = self.watch_tx.clone();
                crate::acting::spawn(move || {
                    let fired = crate::acting::run(&u, crate::watches::pass);
                    let _ = tx.send((u, fired));
                });
            }
        }
        while let Ok((user, fired)) = self.watch_rx.try_recv() {
            self.watching.remove(&user);
            for (title, body) in fired {
                if user == convs[0].app.owner {
                    convs[0].app.log(Level::Plan, format!("👀 {title}: {body}"));
                }
                hub.notify(Notification { title, body, tag: "watch".into(), approval: None, url: None, actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, r)) = self.recap_rx.try_recv() {
            self.recapping.remove(&user);
            crate::recap::save_for(&user, &r);
            if crate::mailout::prefs(&user).recap && !r.parts.is_empty() {
                let (u, body) = (user.clone(), crate::recap::describe(&r));
                crate::acting::spawn(move || {
                    if let Err(e) = crate::mailout::send_to_me(&u, &format!("End of day · {}", chrono::Local::now().format("%a %b %-d")), &body, "recap") {
                        crate::trouble::report(format!("the recap wasn't emailed to {u}: {e}"));
                    }
                });
            }
            if user == convs[0].app.owner {
                convs[0].app.log(Level::Plan, format!("end of day: {}", crate::recap::push_body(&r)));
            }
            if crate::recap::settings().notify && !r.parts.is_empty() {
                hub.notify(Notification { title: "End of day".into(), body: crate::recap::push_body(&r), tag: "recap".into(), approval: None, url: Some("/?page=status".into()), actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, p)) = self.pro_rx.try_recv() {
            self.pro_busy.remove(&user);
            let mine = user == convs[0].app.owner;
            // Meeting prep can't wait: it goes now.
            for (title, body) in p.prep {
                if mine {
                    convs[0].app.log(Level::Plan, format!("{title} — {}", body.replace('\n', " · ")));
                }
                hub.notify(Notification { title, body, tag: "prep".into(), approval: None, url: Some("/?page=tasks".into()), actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            if !p.done.is_empty() {
                if mine {
                    for d in &p.done {
                        convs[0].app.log(Level::Plan, format!("✨ {d}"));
                    }
                }
                self.held.entry(user).or_default().extend(p.done);
            }
        }
        // What the planner did, told once they're out of quiet time.
        if !self.held.is_empty() && self.last_held.elapsed() >= Duration::from_secs(60) {
            self.last_held = Instant::now();
            let ready: Vec<String> = self.held.keys().cloned().collect();
            for user in ready {
                if self.quiet.now(&user) != Some(false) {
                    continue;
                }
                let did = self.held.remove(&user).unwrap_or_default();
                hub.notify(Notification {
                    title: "✨ lyra took care of".into(),
                    body: did.join("\n").chars().take(400).collect(),
                    tag: "plan".into(),
                    approval: None,
                    url: Some("/?page=tasks".into()),
                    actions: vec![],
                    reference: None,
                    to: To::User(user),
                });
            }
        }
        everyone
    }
}
