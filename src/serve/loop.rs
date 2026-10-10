//! `lyra serve`'s loop: devices' messages in, each conversation's updates out,
//! and the timed work (briefings, plans, proactive help, recaps, watches …).


use super::*;

/// Run lyra for the devices until the process is stopped: one lyra per open
/// conversation (each device shows one; several can answer at once), all
/// sharing memory, skills, agents and tools. The first one also runs lyra's
/// background work.
pub fn run(primary: App, hub: &Hub, inbound: std::sync::mpsc::Receiver<Inbound>, notify: bool) {
    // The lyra-node this server hands out (compared with machines' builds).
    let node_build = hub.node_build();
    let mut machines: Vec<String> = hub.machines().into_iter().map(|m| m.name).collect();
    let mut extra = hub_status(hub, node_build.as_deref());
    let mut last_extra = Instant::now();
    let mut last_open = Value::Null;
    // Machine health: what's been reported, and the server's own checkups.
    let mut alerts = crate::health::Alerts::load();
    // Status: everything lyra depends on, checked off this loop every minute.
    let serving_since = Instant::now();
    let (status_tx, status_rx) = crate::status::worker(crate::config::home().unwrap_or_default().join("status"));
    let mut status_alerts = crate::status::Alerts::load();
    let mut status_view = Value::Null;
    let mut status_busy = false;
    let mut last_status: Option<Instant> = None;
    // The coding agents installed here (the server's card shows them like a machine's).
    let mut server_harnesses = lyra_node::coding::available();
    let (health_tx, health_rx) = std::sync::mpsc::channel::<Value>();
    let mut server_health = Value::Null;
    let mut last_checkup: Option<Instant> = None;
    // Routines: when they were last looked at, and finished runs coming back.
    let mut last_routines = Instant::now() - Duration::from_secs(60);
    let (routine_tx, routine_rx) = std::sync::mpsc::channel::<(String, crate::routines::Routine, crate::routines::Run)>();
    // Each person's routines, for their devices.
    let mut routines_views: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    // A routine's run started, ended or did something: its page looks again.
    let mut running_seen = 0u64;
    // Diagnoses: when they were last looked at, and what the panels show.
    crate::diagnose::requeue_running();
    let mut last_diag = Instant::now() - Duration::from_secs(60);
    let mut diag_view = Value::Null;
    // The daily briefing: the last one (it survives restarts), when the next is due.
    let (brief_tx, brief_rx) = std::sync::mpsc::channel::<(String, crate::briefing::Briefing)>();
    let mut last_brief = crate::briefing::last();
    // Each person's latest briefing, for their devices (members get theirs: their tasks).
    let mut brief_views: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    if let Some(b) = &last_brief {
        brief_views.insert(lyra_web::users::OWNER.to_string(), json!(b));
    }
    // A message whose attachments were read off the loop, back to be sent.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Ready>();
    // A model switch from a page, checked with the model server off the loop,
    // then applied here to every conversation.
    let (switch_tx, switch_rx) = std::sync::mpsc::channel::<(String, String, tokio::sync::oneshot::Sender<Value>)>();
    // Briefings still being made (the batch is done at 0).
    let mut brief_left = 0usize;
    let mut last_brief_check = Instant::now() - Duration::from_secs(60);
    // PMI: its live events (a thread), and the view read after each change.
    let (pmi_events_tx, pmi_events) = std::sync::mpsc::channel::<(String, crate::pmi::Event)>();
    let (pmi_tx, pmi_rx) = std::sync::mpsc::channel::<(String, Result<crate::pmi::State, String>)>();
    // Each person with a PMI token: their live view, followed while lyra runs.
    let mut pmi: std::collections::HashMap<String, PmiUser> = std::collections::HashMap::new();
    let mut last_pmi_scan = Instant::now() - Duration::from_secs(120);
    // Each person's assistant work: nags, plan my day, proactive help, the
    // recap, "tell me when …", and telling what was done outside quiet time.
    let mut assistant = assistant::Assistant::new();
    let started = Instant::now();
    let mut convs = vec![Conv::new(primary)];
    // A stop waits for what's running; runs a restart cut off start again now.
    catch_stop();
    let mut draining: Option<Instant> = None;
    // What grows by itself, tidied once a day (off the loop), the first time a little after starting.
    let mut last_tidy = Instant::now() - Duration::from_secs(86_400 - 600);
    let (tidy_tx, tidy_rx) = std::sync::mpsc::channel::<Vec<String>>();
    // Routine runs done but not recorded yet (their check and email): a stop waits for them too.
    let mut finishing = 0usize;
    for (user, name) in crate::routines::take_resume() {
        crate::routines::request_run_for(&user, &name);
        convs[0].app.log(Level::Plan, format!("routine {name}: running again (a restart cut it off)"));
    }
    loop {
        if stopping() {
            drain_step(&mut convs, &mut draining, finishing);
        }
        let stopping_now = stopping();
        if last_tidy.elapsed() >= Duration::from_secs(86_400) && !stopping_now {
            last_tidy = Instant::now();
            let tx = tidy_tx.clone();
            crate::acting::spawn(move || {
                let home = crate::config::home().unwrap_or_default();
                let devices = lyra_web::Devices::open(&home.join("web")).ok();
                let _ = tx.send(crate::retention::tidy(&home, devices.as_ref()));
            });
        }
        while let Ok(said) = tidy_rx.try_recv() {
            for s in said {
                convs[0].app.log(Level::Info, format!("tidied: {s}"));
            }
        }
        // Problems with nobody watching: into the owner's Activity.
        for t in crate::trouble::take_new() {
            convs[0].app.log(Level::Error, t);
        }
        let mut everyone = false;
        // What the devices ask for (waiting a little when there's nothing).
        let mut next = inbound.recv_timeout(Duration::from_millis(40)).ok();
        while let Some(msg) = next.take() {
            match msg {
                Inbound::Send { text, device, who, session, conn, files } => {
                    sync_role(&mut convs, &who);
                    if files.is_empty() {
                        deliver(&mut convs, hub, Ready { text, device, who, session, conn, images: Vec::new(), looks: Vec::new() });
                    } else {
                        // Attached files are read (PDF text, pictures) off the loop (I-7);
                        // the message comes back here to be sent, in the order it was written.
                        let i = conv_for(&mut convs, &session, &who);
                        let (hub, vision, tx) = (hub.clone(), convs[i].app.vision, ready_tx.clone());
                        crate::acting::spawn(move || {
                            let (about, images, looks) = attachments(&hub, &files, vision, &who);
                            let text = format!("{text}{about}").trim().to_string();
                            let _ = tx.send(Ready { text, device, who, session, conn, images, looks });
                        });
                    }
                }
                Inbound::Stop { device, who, session } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    convs[i].app.log(Level::Info, format!("{device} pressed stop"));
                    let _ = convs[i].app.stop();
                    convs[i].changed = true;
                }
                Inbound::Approve { id, answer, device, who } => {
                    sync_role(&mut convs, &who);
                    // Only in the user's own conversations.
                    if let Some(c) = convs.iter_mut().find(|c| c.app.owner == who.user && c.app.approvals.iter().any(|r| r.id == id)) {
                        c.app.log(Level::Agent, format!("{device} answered approval {id}: {answer}"));
                        c.app.answer_approval_id(id, &answer);
                        c.changed = true;
                    }
                }
                // A reset link, by email to its own account (off the loop).
                Inbound::ResetLink { user, link } => {
                    convs[0].app.log(Level::Agent, format!("a password reset link was asked for ({user})"));
                    crate::acting::spawn(move || {
                        let body = format!(
                            "Someone (hopefully you) asked to choose a new password for lyra.\n\n**[Choose a new password]({link})**\n\nThe link works once, for 30 minutes. If you didn't ask, ignore this email: your password stays as it is."
                        );
                        if let Err(e) = crate::mailout::send_to_me(&user, "Choose a new lyra password", &body, "reset") {
                            crate::trouble::report(format!("the reset link for {user} wasn't emailed: {e}"));
                        }
                    });
                }
                Inbound::Answer { id, value, device, who } => {
                    sync_role(&mut convs, &who);
                    // Only in the user's own conversations.
                    if let Some(c) = convs.iter_mut().find(|c| c.app.owner == who.user && c.app.asks.iter().any(|r| r.id == id)) {
                        c.app.log(Level::Agent, format!("{device} answered in the chat ({id})"));
                        c.app.answer_ask(id, &value);
                        c.changed = true;
                    }
                }
                Inbound::Action { action, reference, device, who } => {
                    if who.user == convs[0].app.owner {
                        convs[0].app.log(Level::Info, format!("{device} pressed {action} on a reminder"));
                    }
                    let tx = assistant.action_tx.clone();
                    crate::acting::spawn(move || {
                        // In their own PMI account.
                        let r = crate::pmi::as_user(&who.user, || crate::pmi::push_action(&action, &reference));
                        let _ = tx.send((who.user, action, reference, r));
                    });
                }
                Inbound::Note(text) => {
                    convs[0].app.log(Level::Info, text);
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::SignIn { name, email } => {
                    convs[0].app.log(Level::Agent, format!("👤 {name} <{email}> signed in with Microsoft and waits to be let in: /users approve {email}"));
                    if notify {
                        hub.notify(Notification {
                            title: format!("{name} wants to use lyra"),
                            body: format!("{email} signed in with Microsoft. Let them in on the Users page, or /users approve {email}."),
                            tag: format!("signin-{email}"),
                            approval: None,
                            url: None, actions: vec![], reference: None,
                        to: To::Admins,
                        });
                    }
                    everyone = true;
                }
                Inbound::Connected { user, service, token, scope } => {
                    crate::graph::forget_access(&user);
                    match crate::secrets::set_token_for(&service, &token, &user).and_then(|_| crate::graph::keep_scope(&user, &scope)) {
                        Ok(()) if user == convs[0].app.owner => convs[0].app.log(Level::Info, format!("your Outlook is connected ({})", crate::graph::granted_text(&user))),
                        Ok(()) => {}
                        Err(e) => convs[0].app.log(Level::Error, format!("couldn't keep a calendar connection: {e}")),
                    }
                    everyone = true;
                }
                Inbound::DevicesChanged => {
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::PairRequested(p) => {
                    let what = if p.kind == "node" { "machine" } else { "device" };
                    convs[0].app.log(Level::Agent, format!("🔗 {} ({}) asks to pair as a {what}, code {}: /devices approve {}", p.name, p.hostname, p.code, p.code));
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                    if notify {
                        hub.notify(Notification {
                            title: format!("{} wants to pair", p.name),
                            body: format!("A {what} ({}) asks to pair. Check its code is {} and approve it in lyra.", p.hostname, p.code),
                            tag: format!("pair-{}", p.code),
                            approval: None,
                            url: None, actions: vec![], reference: None,
                        to: To::Admins,
                        });
                    }
                }
                Inbound::Get { what, arg, session, who, reply } => {
                    if get::get_page(&mut convs, hub, node_build.as_deref(), &switch_tx, what, arg, session, who, reply) {
                        everyone = true;
                    }
                }
                Inbound::MachineHealth { name, health } => {
                    health_report(&mut convs[0].app, hub, &mut alerts, &name, &health);
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                }
                Inbound::MachinesChanged => {
                    let now: Vec<String> = hub.machines().into_iter().map(|m| m.name).collect();
                    for m in now.iter().filter(|m| !machines.contains(m)) {
                        convs[0].app.log(Level::Agent, format!("machine {m} connected: the Operator can work on it"));
                    }
                    for m in machines.iter().filter(|m| !now.contains(m)) {
                        convs[0].app.log(Level::Agent, format!("machine {m} disconnected"));
                    }
                    machines = now;
                    extra = hub_status(hub, node_build.as_deref());
                    everyone = true;
                    // The system tools' `machine` choices follow.
                    if let Some(caps) = convs[0].app.caps.clone() {
                        crate::acting::spawn(move || {
                            caps.refresh();
                        });
                    }
                }
                Inbound::Health(reply) => {
                    let _ = reply.send(json!({
                        "version": crate::changelog::version(),
                        "uptime_seconds": started.elapsed().as_secs(),
                        "busy": convs.iter().any(|c| c.app.waiting),
                        "conversations": convs.len(),
                        "approvals_waiting": convs.iter().map(|c| c.app.approvals.len()).sum::<usize>(),
                    }));
                }
                Inbound::Snapshot { session, who, reply } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    let c = &mut convs[i];
                    let mut all = extra.clone();
                    all["conversations"] = last_open.clone();
                    all["server_health"] = server_health.clone();
                    all["server_harnesses"] = server_harnesses.clone();
                    all["status"] = status_view.clone();
                    all["diagnoses"] = diag_view.clone();
                    c.mirror.machines = if c.app.admin { machines.clone() } else { Vec::new() };
                    c.mirror.extra = for_viewer(&all, &c.app);
                    c.mirror.extra["pmi"] = pmi.get(&c.app.owner).map_or(Value::Null, |p| p.view.clone());
                c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                c.mirror.extra["routines"] = routines_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    c.mirror.extra["routines"] = routines_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                    for u in c.mirror.updates(&mut c.app) {
                        hub.publish(Some(&c.app.session_id), u);
                    }
                    c.changed = false;
                    let _ = reply.send(c.mirror.snapshot(hub.seq(), hub.app_version()));
                }
            }
            next = inbound.try_recv().ok();
        }
        // Decisions made for any conversation are logged with the primary.
        convs[0].app.decide_notes();
        // Each conversation's own work: replies streaming, agents, plans…
        let many = convs.len() > 1;
        let mut diagnosed: Vec<String> = Vec::new();
        for c in convs.iter_mut() {
            conversation_events(c, hub, notify, many, &routine_tx, &mut finishing, &mut diagnosed);
        }
        // Finished write-ups: logged and told.
        for key in diagnosed {
            if let Some(d) = crate::diagnose::all().into_iter().find(|d| d.key == key) {
                let head = crate::diagnose::headline(&d.summary);
                convs[0].app.log(if d.state == "done" { Level::Agent } else { Level::Error }, format!("🔎 {}: {} — {head}", d.machine, d.problem));
                if crate::health::settings().notify && d.state == "done" {
                    hub.notify(Notification { title: format!("🔎 {}: {}", d.machine, d.problem), body: head, tag: format!("diag-{}", d.key), approval: None, url: None, actions: vec![], reference: None, to: To::Admins });
                }
            }
            last_diag = Instant::now() - Duration::from_secs(60);
        }
        // lyra's background work, on the primary.
        {
            let p = &mut convs[0];
            if !p.app.waiting && p.app.last_schedule_check.elapsed() > Duration::from_secs(10 * 60) {
                p.app.scheduled();
                p.changed = true;
            }
            if !p.app.waiting && !p.app.plan_busy && p.app.goals_checked.elapsed() > p.app.goals_every {
                p.app.goals_tick();
                p.changed = true;
            }
        }
        // Routines: due ones and ones asked for, each in its own conversation.
        if !stopping_now && (last_routines.elapsed() >= Duration::from_secs(20) || routines_wanted()) {
            last_routines = Instant::now();
            // Everyone's: each runs in its person's own conversation, with their rights.
            start_routines(&mut convs, hub);
        }
        while let Ok((user, r, run)) = routine_rx.try_recv() {
            routine_done(&mut convs, hub, &user, &r, run);
            finishing = finishing.saturating_sub(1);
            everyone = true;
        }
        // PMI, per person with a token: follow their live events (a thread each,
        // started as tokens appear), read again shortly after a change and every 5 minutes.
        if pmi_tick(&mut pmi, &mut last_pmi_scan, &pmi_events_tx, &pmi_events, &pmi_tx, &pmi_rx, &mut convs, hub) {
            everyone = true;
        }
        everyone = assistant.tick(&mut convs, hub, &mut pmi, everyone);
        // The daily briefing: on schedule or asked for, gathered and written off the loop.
        // Asked for while a batch is still being made: kept for the next look.
        let wanted = if brief_left == 0 { crate::briefing::take_requests() } else { Vec::new() };
        if brief_left == 0 && (!wanted.is_empty() || last_brief_check.elapsed() >= Duration::from_secs(20)) {
            last_brief_check = Instant::now();
            let s = crate::briefing::settings();
            let now = chrono::Local::now();
            let due = s.enabled && crate::briefing::next(&s.schedule, last_brief.as_ref().map(|b| b.at), now).is_some_and(|t| t <= now);
            if !wanted.is_empty() || due {
                brief_left = start_briefings(&convs, hub, node_build.as_deref(), &server_health, &pmi, last_brief.as_ref(), &wanted, due, &s, &brief_tx);
            }
        }
        while let Ok(ready) = ready_rx.try_recv() {
            deliver(&mut convs, hub, ready);
        }
        while let Ok((name, _url, reply)) = switch_rx.try_recv() {
            let answer = convs[0].app.switch_model(&name);
            for c in convs.iter_mut() {
                c.app.model = name.clone();
                c.changed = true;
            }
            let _ = reply.send(match answer {
                Ok(text) => json!({ "ok": true, "text": text }),
                Err(e) => json!({ "ok": false, "text": e }),
            });
        }
        while let Ok((user, b)) = brief_rx.try_recv() {
            let owner = user == convs[0].app.owner;
            crate::briefing::save_for(&user, &b);
            // By email too, when they asked for it (off the loop).
            if crate::mailout::prefs(&user).briefing && !b.sections.is_empty() {
                let (u, subject, body) = (user.clone(), format!("☀ Briefing: {}", b.headline), crate::briefing::describe(&b));
                crate::acting::spawn(move || {
                    if let Err(e) = crate::mailout::send_to_me(&u, &subject, &body, "briefing") {
                        crate::trouble::report(format!("the briefing wasn't emailed to {u}: {e}"));
                    }
                });
            }
            brief_left = brief_left.saturating_sub(1);
            if owner {
                convs[0].app.log(if b.attention > 0 { Level::Error } else { Level::Plan }, format!("briefing: {}{}", b.headline, b.takeaway.as_ref().map(|t| format!(" — {t}")).unwrap_or_default()));
            }
            if crate::briefing::settings().notify {
                hub.notify(Notification {
                    title: format!("☀ Briefing: {}", b.headline),
                    body: crate::briefing::push_body(&b),
                    tag: "briefing".into(),
                    approval: None,
                    url: Some("/?page=status".into()),
                    actions: vec![],
                    reference: None,
                    to: To::User(user.clone()),
                });
            }
            brief_views.insert(user, json!(b));
            if owner {
                last_brief = Some(b);
            }
            everyone = true;
        }
        // Diagnoses: one at a time, each in its own conversation; their write-ups for the panels.
        if last_diag.elapsed() >= Duration::from_secs(5) {
            last_diag = Instant::now();
            let d = convs[0].app.diagnose.clone();
            if d.enabled && !stopping_now && !convs.iter().any(|c| c.diagnosis.is_some()) && crate::diagnose::has_queued() {
                let mut app = convs[0].app.fork();
                let session = app.session_id.clone();
                if let Some(next) = crate::diagnose::start_next(|_| session.clone()) {
                    app.input = crate::diagnose::message(&next);
                    app.unattended = true;
                    app.send();
                    convs[0].app.log(Level::Agent, format!("🔎 looking into {}: {}", next.machine, next.problem));
                    let mut c = Conv::new(app);
                    c.diagnosis = Some(next.key);
                    convs.push(c);
                }
            }
            let now = crate::diagnose::view();
            if now != diag_view {
                diag_view = now;
                everyone = true;
            }
        }
        // What the panels show: each routine's next run, last result, running now.
        if last_routines.elapsed() < Duration::from_millis(50) || everyone || crate::routines::running_rev() != running_seen {
            running_seen = crate::routines::running_rev();
            for user in crate::routines::people() {
                let now = crate::acting::run(&user, || crate::routines::view(1));
                if routines_views.get(&user) != Some(&now) {
                    routines_views.insert(user, now);
                    everyone = true;
                }
            }
        }
        let ss = convs[0].app.status.clone();
        if ss.enabled && !status_busy && (last_status.is_none_or(|t| t.elapsed() >= Duration::from_secs(ss.every_seconds.max(15))) || crate::status::take_request()) {
            last_status = Some(Instant::now());
            let mut i = convs[0].app.status_inputs();
            let (sent, failed, error) = hub.take_push_counts();
            i.push = Some((hub.push_devices(), sent, failed, error));
            let up = serving_since.elapsed().as_secs();
            i.serving = Some(format!(
                "v{} · up {}d {}h {}m · {} conversation{} open · {} device{} connected",
                crate::changelog::version(),
                up / 86400,
                up % 86400 / 3600,
                up % 3600 / 60,
                convs.len(),
                if convs.len() == 1 { "" } else { "s" },
                hub.connections(),
                if hub.connections() == 1 { "" } else { "s" }
            ));
            i.machines = machines_detail(hub, node_build.as_deref());
            i.server_health = server_health.clone();
            status_busy = status_tx.send(i).is_ok();
            server_harnesses = lyra_node::coding::available();
        }
        while let Ok(board) = status_rx.try_recv() {
            status_busy = false;
            match board {
                Ok(board) => {
                    status_view = status_board(&mut convs, hub, &mut status_alerts, &board, &ss);
                    everyone = true;
                }
                Err(e) => convs[0].app.log(Level::Error, format!("status: {e}")),
            }
        }
        // The server's own health, like a machine's.
        if last_checkup.is_none_or(|t| t.elapsed() >= lyra_node::health::EVERY) {
            last_checkup = Some(Instant::now());
            let tx = health_tx.clone();
            crate::acting::spawn(move || {
                let _ = tx.send(lyra_node::health::report());
            });
        }
        while let Ok(h) = health_rx.try_recv() {
            health_report(&mut convs[0].app, hub, &mut alerts, "server", &h);
            server_health = crate::health::view(&h);
            everyone = true;
        }
        // Pairing requests expire and devices come and go: refresh now and then.
        if last_extra.elapsed() > Duration::from_secs(10) {
            last_extra = Instant::now();
            let s = crate::health::settings();
            if s.enabled {
                let paired: Vec<String> = hub.devices().list().into_iter().filter(|d| d.kind == "node").map(|d| d.name).collect();
                let (gone, back) = alerts.connected(&paired, &machines, Duration::from_secs(s.offline_minutes * 60));
                for m in gone {
                    alert(&mut convs[0].app, hub, &m, &format!("{m} has been offline for {} minute{}", s.offline_minutes, if s.offline_minutes == 1 { "" } else { "s" }), true);
                }
                for m in back {
                    alert(&mut convs[0].app, hub, &m, &format!("{m} is back online"), false);
                }
            }
            let now = hub_status(hub, node_build.as_deref());
            if now != extra {
                extra = now;
                everyone = true;
            }
        }
        // The conversations loaded now and which are answering, for every device.
        let open = json!(convs.iter().map(|c| json!({ "session": c.app.session_id, "title": title(&c.app), "answering": c.app.waiting, "owner": c.app.owner })).collect::<Vec<_>>());
        if open != last_open {
            last_open = open.clone();
            everyone = true;
        }
        let mut extra_now = extra.clone();
        extra_now["conversations"] = open;
        extra_now["server_health"] = server_health.clone();
        extra_now["server_harnesses"] = server_harnesses.clone();
        extra_now["status"] = status_view.clone();
        extra_now["diagnoses"] = diag_view.clone();
        let attached = hub.attached_sessions();
        for c in convs.iter_mut() {
            // The phase timer ("thinking 4s") ticks while something is happening.
            if everyone || c.changed || (c.app.waiting && c.last_status.elapsed() > Duration::from_secs(1)) {
                c.last_status = Instant::now();
                c.changed = false;
                c.mirror.machines = if c.app.admin { machines.clone() } else { Vec::new() };
                c.mirror.extra = for_viewer(&extra_now, &c.app);
                c.mirror.extra["pmi"] = pmi.get(&c.app.owner).map_or(Value::Null, |p| p.view.clone());
                c.mirror.extra["briefing"] = brief_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                c.mirror.extra["routines"] = routines_views.get(&c.app.owner).cloned().unwrap_or(Value::Null);
                for u in c.mirror.updates(&mut c.app) {
                    hub.publish(Some(&c.app.session_id), u);
                }
            }
            // The activity log goes to stdout (the systemd journal).
            if c.app.logged > c.printed {
                let new = ((c.app.logged - c.printed) as usize).min(c.app.activity.len());
                for a in &c.app.activity[c.app.activity.len() - new..] {
                    println!("{} {}", a.time, a.text);
                }
                c.printed = c.app.logged;
            }
            if c.app.waiting || attached.contains(&c.app.session_id) {
                c.quiet_since = Instant::now();
            }
        }
        // Put away conversations nobody has looked at for a while (saved first).
        let mut i = 1;
        while i < convs.len() {
            if convs[i].quiet_since.elapsed() > IDLE {
                let mut c = convs.remove(i);
                c.app.save_session();
                println!("put away conversation {} (idle)", c.app.session_id);
            } else {
                i += 1;
            }
        }
    }
}

/// A fresh status board: what went down or came back told (and looked into,
/// when diagnoses do that by themselves), and the board as the Status page
/// shows it, with the models marked known down.
fn status_board(convs: &mut [Conv], hub: &Hub, status_alerts: &mut crate::status::Alerts, board: &crate::status::Board, ss: &crate::status::Settings) -> Value {
    for (id, problem, text) in crate::status::alerts(status_alerts, board, ss) {
        let d = &convs[0].app.diagnose;
        if problem && d.enabled && d.auto {
            crate::diagnose::queue(&format!("status:{id}"), crate::caps::HERE, &text, false);
        } else if !problem {
            crate::diagnose::resolve(&format!("status:{id}"));
        }
        convs[0].app.log(if problem { Level::Error } else { Level::Agent }, format!("{} {text}", if problem { "⚠" } else { "✓" }));
        if ss.notify {
            hub.notify(Notification {
                title: if problem { "⚠ lyra needs a look".into() } else { "✓ lyra".into() },
                body: text,
                tag: "status".into(),
                approval: None,
                url: None,
                actions: vec![],
                reference: None,
                to: To::Admins,
            });
        }
    }
    let mut status_view = json!(board);
    // The models marked known down, for the page's buttons.
    status_view["known_down"] = crate::known_down::view();
    status_view
}

/// PMI, per person with a token: follow their live events (a thread each,
/// started as tokens appear), read again shortly after a change and every 5
/// minutes. True when what devices show changed.
#[allow(clippy::too_many_arguments)]
fn pmi_tick(
    pmi: &mut std::collections::HashMap<String, PmiUser>,
    last_pmi_scan: &mut Instant,
    pmi_events_tx: &std::sync::mpsc::Sender<(String, crate::pmi::Event)>,
    pmi_events: &std::sync::mpsc::Receiver<(String, crate::pmi::Event)>,
    pmi_tx: &std::sync::mpsc::Sender<(String, Result<crate::pmi::State, String>)>,
    pmi_rx: &std::sync::mpsc::Receiver<(String, Result<crate::pmi::State, String>)>,
    convs: &mut [Conv],
    hub: &Hub,
) -> bool {
    let mut everyone = false;
    if last_pmi_scan.elapsed() >= Duration::from_secs(60) {
        *last_pmi_scan = Instant::now();
        let people: Vec<String> = hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active && !pmi.contains_key(&u.id) && crate::pmi::configured_for(&u.id)).map(|u| u.id).collect();
        for user in people {
            let tx = pmi_events_tx.clone();
            let who = user.clone();
            crate::acting::spawn(move || crate::pmi::follow(who, tx));
            pmi.insert(user.clone(), PmiUser { due: Some(Instant::now()), last: Instant::now(), nags: crate::pmi::Nags::load_for(&user), ..Default::default() });
        }
    }
    while let Ok((user, e)) = pmi_events.try_recv() {
        let Some(p) = pmi.get_mut(&user) else { continue };
        match e {
            crate::pmi::Event::Live(live) => {
                if live != p.state.live {
                    p.state.live = live;
                    if user == convs[0].app.owner {
                        convs[0].app.log(Level::Info, if live { "PMI: following its live updates".to_string() } else { "PMI: can't follow its live updates, retrying".to_string() });
                    }
                    p.view = json!(p.state);
                    everyone = true;
                    // Back after an outage: what changed meanwhile.
                    if live {
                        p.due = Some(Instant::now());
                    }
                }
                // Their token is gone: forget them until one is set again.
                if !live && !crate::pmi::configured_for(&user) {
                    pmi.remove(&user);
                    everyone = true;
                }
            }
            crate::pmi::Event::Changed(areas) => {
                if areas.iter().any(|a| matches!(a.as_str(), "tasks" | "projects" | "resync" | "structure")) {
                    let soon = Instant::now() + Duration::from_secs(5);
                    p.due = Some(p.due.map_or(soon, |d| d.min(soon)));
                }
            }
        }
    }
    for user in crate::pmi::take_stale() {
        if let Some(p) = pmi.get_mut(&user) {
            p.due = Some(Instant::now() + Duration::from_secs(2));
        } else {
            // A token was just set: start following them now.
            *last_pmi_scan = Instant::now() - Duration::from_secs(120);
        }
    }
    for (user, p) in pmi.iter_mut() {
        if !p.busy && (p.due.is_some_and(|d| Instant::now() >= d) || p.last.elapsed() >= Duration::from_secs(300)) {
            p.busy = true;
            p.due = None;
            p.last = Instant::now();
            let (tx, user) = (pmi_tx.clone(), user.clone());
            crate::acting::spawn(move || {
                let r = crate::pmi::as_user(&user, crate::pmi::snapshot);
                let _ = tx.send((user, r));
            });
        }
    }
    while let Ok((user, r)) = pmi_rx.try_recv() {
        let Some(p) = pmi.get_mut(&user) else { continue };
        p.busy = false;
        let mine = user == convs[0].app.owner;
        match r {
            Ok(mut s) => {
                s.live = p.state.live;
                if mine && p.state.error.is_some() {
                    convs[0].app.log(Level::Info, format!("PMI: back ({})", s.line(chrono::Local::now().date_naive())));
                }
                p.state = s;
            }
            Err(e) => {
                if mine && p.state.error.as_deref() != Some(e.as_str()) {
                    convs[0].app.log(Level::Error, format!("PMI: {e}"));
                }
                p.state.error = Some(e);
            }
        }
        let view = json!(p.state);
        if view != p.view {
            p.view = view;
            everyone = true;
        }
    }
    everyone
}

/// The daily briefing, for those it's for (everyone with PMI or a calendar on
/// schedule; only those who asked otherwise): what's at hand gathered here,
/// their calendar, Teams and mail read and the takeaway written off the loop.
/// How many are coming.
#[allow(clippy::too_many_arguments)]
fn start_briefings(
    convs: &[Conv],
    hub: &Hub,
    node_build: Option<&str>,
    server_health: &Value,
    pmi: &std::collections::HashMap<String, PmiUser>,
    last_brief: Option<&crate::briefing::Briefing>,
    wanted: &[String],
    due: bool,
    s: &crate::briefing::Settings,
    brief_tx: &std::sync::mpsc::Sender<(String, crate::briefing::Briefing)>,
) -> usize {
    let at = chrono::Utc::now();
    let since = crate::briefing::window_start(last_brief.map(|b| b.at), at);
    let mut inputs = crate::briefing::local_inputs(convs[0].app.goals.as_deref(), at, since);
    inputs.machines = machines_detail(hub, node_build);
    inputs.server_health = (!server_health.is_null()).then(|| server_health.clone());
    inputs.pmi = pmi.get(&convs[0].app.owner).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
    let (url, model, tx) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone(), brief_tx.clone());
    let owner = convs[0].app.owner.clone();
    // On schedule: everyone with PMI or a calendar connected gets their own.
    // Asked for: only those who asked (whatever they have connected).
    let asked = |u: &String| wanted.iter().any(|w| w == u);
    let with_owner = due || asked(&owner);
    let others: Vec<(String, crate::briefing::Inputs)> = hub
        .users()
        .list()
        .into_iter()
        .filter(|u| u.status == lyra_web::Status::Active && u.id != owner)
        .map(|u| u.id)
        .filter(|u| if due { pmi.get(u).is_some_and(|p| p.state.at.is_some()) || crate::graph::connected_for(u) } else { asked(u) })
        .map(|u| {
            let since = crate::briefing::window_start(crate::briefing::last_for(&u).map(|b| b.at), at);
            // Their tasks, routines and goals.
            let (goals, goal_events) = crate::goals::for_user(&u).map_or_else(Default::default, |g| (g.manager.all().unwrap_or_default(), g.manager.events(None, 300).unwrap_or_default()));
            let runs = crate::acting::run(&u, crate::routines::runs);
            let pmi = pmi.get(&u).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
            (u, crate::briefing::Inputs { now: at, since, pmi, runs, goals, goal_events, ..Default::default() })
        })
        .collect();
    let batch: Vec<(String, crate::briefing::Inputs)> = with_owner.then_some((owner, inputs)).into_iter().chain(others).collect();
    let n = batch.len();
    let summary = s.summary;
    crate::acting::spawn(move || {
        for (user, mut inputs) in batch {
            // Their calendar today, when they've connected it.
            if crate::graph::connected_for(&user) {
                inputs.calendar = crate::acting::run(&user, crate::calendar::today).ok();
            }
            if crate::teams::connected_for(&user) {
                inputs.teams = crate::acting::run(&user, || crate::teams::chats(25)).ok().map(|c| c.into_iter().filter(|x| x["unread"] == true).collect());
            }
            if crate::mail::connected_for(&user) {
                let since = inputs.since;
                inputs.mail = crate::acting::run(&user, || crate::mail::glance(since)).ok();
            }
            let mut b = crate::briefing::gather(&inputs);
            if summary {
                b.takeaway = crate::acting::run(&user, || crate::briefing::takeaway(&url, &model, &b));
            }
            let _ = tx.send((user, b));
        }
    });
    n
}

/// One conversation's new events: handled, and what follows from them, by
/// what the conversation is (a problem write-up only looks; a routine's run
/// is judged and told by its own rules, then kept; anyone else's reply is
/// pushed to them when they aren't looking).
#[allow(clippy::too_many_arguments)]
fn conversation_events(
    c: &mut Conv,
    hub: &Hub,
    notify: bool,
    many: bool,
    routine_tx: &std::sync::mpsc::Sender<(String, crate::routines::Routine, crate::routines::Run)>,
    finishing: &mut usize,
    diagnosed: &mut Vec<String>,
) {
    while let Ok(event) = c.app.rx.try_recv() {
        let note = notification(&event);
        let done = matches!(event, StreamEvent::Done(_) | StreamEvent::Error(_));
        c.app.handle(event);
        c.changed = true;
        // A diagnosis only looks: changes it asks for are declined; its write-up is kept.
        if let Some(key) = c.diagnosis.clone() {
            let asked: Vec<u64> = c.app.approvals.iter().map(|a| a.id).collect();
            for id in asked {
                c.app.answer_approval_id(id, "n");
            }
            if done && !c.app.waiting {
                c.diagnosis = None;
                let reply = c.app.messages.iter().rev().find(|m| matches!(m.role.as_str(), "assistant" | "error")).map(|m| (m.role == "assistant", m.content.clone()));
                let (ok, text) = reply.unwrap_or((false, "no answer".into()));
                crate::diagnose::finish(&key, &text, ok);
                diagnosed.push(key);
            }
            continue;
        }
        // A routine's run: judged and told by its own rules, not "lyra replied".
        if let Some((r, started)) = c.routine.clone() {
            crate::routines::running_doing(&c.app.owner, &r.name, &crate::ui::doing_text(&c.app));
            let may_change = r.changes;
            // One that only looks: anything asking to change something is refused at once.
            if !r.changes {
                let asked: Vec<u64> = c.app.approvals.iter().map(|a| a.id).collect();
                for id in asked {
                    c.app.answer_approval_id(id, "n");
                    c.app.log(Level::Plan, format!("routine {}: declined a change (it only looks; /routine edit {} changes on lets it ask)", r.name, r.name));
                }
            }
            if done && !c.app.waiting {
                c.routine = None;
                crate::routines::running_end(&c.app.owner, &r.name);
                let reply = c.app.messages.iter().rev().find(|m| matches!(m.role.as_str(), "assistant" | "error")).map(|m| (m.role.clone(), m.content.clone()));
                let outcome = match &reply {
                    Some((role, _)) if role == "error" => "error",
                    Some((_, content)) if content.ends_with("_(stopped)_") => "stopped",
                    _ => "ok",
                };
                let text = reply.map(|m| m.1).unwrap_or_default();
                let (url, model, session, tx) = (format!("{}/chat/completions", c.app.base_url.trim_end_matches('/')), c.app.model.clone(), c.app.session_id.clone(), routine_tx.clone());
                let whose = c.app.owner.clone();
                let began = chrono::Utc::now() - chrono::Duration::from_std(started.elapsed()).unwrap_or_default();
                *finishing += 1;
                crate::acting::spawn(move || {
                    // Its checks count for the run too.
                    crate::usage::set_job(Some(session.clone()));
                    let summary: String = text.trim().chars().take(400).collect();
                    // The whole result, for its page and the next run.
                    if outcome == "ok"
                        && let Err(e) = crate::acting::run(&whose, || crate::routines::save_result(&r.name, chrono::Utc::now(), &text))
                    {
                        crate::trouble::report(format!("routine {}: its result wasn't kept: {e}", r.name));
                    }
                    // Kept as done at once (a stop during the check or the email
                    // leaves this, not "interrupted"); the full record replaces it.
                    let early = crate::routines::Run {
                        at: chrono::Utc::now(), seconds: started.elapsed().as_secs(), needs_user: outcome != "ok", outcome: outcome.into(), summary: summary.clone(), session: session.clone(),
                        decided_by: if outcome == "ok" { "not checked: lyra stopped first".into() } else { "it didn't finish".into() },
                        emailed: if r.email && outcome == "ok" { "not emailed: lyra stopped first".into() } else { String::new() },
                        calls: 0, tokens_in: 0, tokens_out: 0, cost: 0.0,
                    };
                    crate::acting::run(&whose, || crate::routines::record(&r.name, early));
                    let (needs_user, decided_by) = if outcome == "ok" { crate::routines::needs_user(&url, &model, &r, &text) } else { (true, "it didn't finish".into()) };
                    // Its result by email, to its person only.
                    let emailed = if !r.email {
                        String::new()
                    } else if outcome != "ok" {
                        format!("not emailed: the run {outcome}")
                    } else {
                        let subject = format!("{} · {}", r.name.replace('-', " "), chrono::Local::now().format("%a %b %-d"));
                        crate::mailout::send_to_me(&whose, &subject, &text, "routine").unwrap_or_else(|e| format!("not emailed: {e}"))
                    };
                    if emailed.starts_with("not emailed:") && outcome == "ok" {
                        crate::trouble::report(format!("routine {}: {emailed}", r.name));
                    }
                    let spent = crate::usage::spent(&session, began);
                    let run = crate::routines::Run {
                        at: chrono::Utc::now(), seconds: started.elapsed().as_secs(), needs_user, outcome: outcome.into(), summary, session, decided_by, emailed,
                        calls: spent.calls, tokens_in: spent.input, tokens_out: spent.output, cost: spent.cost,
                    };
                    let _ = tx.send((whose, r, run));
                });
            }
            // Only a routine allowed to change things asks the user.
            if let Some(mut n) = note.filter(|_| may_change) {
                n.to = To::User(c.app.owner.clone());
                hub.notify(n);
            }
            continue;
        }
        // Their own conversation, and only when they aren't looking at lyra.
        if !notify || hub.watching(&c.app.owner) {
            continue;
        }
        // With several conversations, say which one.
        let about = |title: String| match c.app.messages.iter().find(|m| m.role == "user") {
            Some(m) if many => format!("{title} · {}", m.content.lines().next().unwrap_or("").chars().take(40).collect::<String>()),
            _ => title,
        };
        if done {
            let body = c.app.messages.iter().rev().find(|m| m.role == "assistant").map(|m| preview(&m.content)).unwrap_or_default();
            hub.notify(Notification { title: about("lyra replied".into()), body, tag: format!("reply-{}", c.app.session_id), approval: None, url: None, actions: vec![], reference: None, to: To::User(c.app.owner.clone()) });
        }
        if let Some(mut n) = note {
            n.title = about(n.title);
            n.to = To::User(c.app.owner.clone());
            hub.notify(n);
        }
    }

}

/// The routines due now (everyone's) and the ones asked for: each started in
/// its person's own conversation, with their rights (one run of each at a time).
fn start_routines(convs: &mut Vec<Conv>, hub: &Hub) {
    let mut start: Vec<(String, crate::routines::Routine)> = crate::routines::due_all(chrono::Local::now());
    for (user, name) in crate::routines::take_requests() {
        if let Ok(r) = crate::acting::run(&user, || crate::routines::find(&name))
            && !start.iter().any(|(u, x)| *u == user && x.name == r.name)
        {
            start.push((user, r));
        }
    }
    for (user, r) in start {
        if convs.iter().any(|c| c.app.owner == user && c.routine.as_ref().is_some_and(|(x, _)| x.name == r.name)) {
            continue;
        }
        let mine = user == convs[0].app.owner;
        let mut app = if mine {
            convs[0].app.fork()
        } else {
            // Someone else's: only while they may use lyra.
            let Some(who) = hub.users().who(Some(&user)) else { continue };
            convs[0].app.fork_for(&who)
        };
        // Its own person's past results go with it.
        app.input = crate::acting::run(&user, || crate::routines::message(&r));
        app.unattended = true;
        app.send();
        crate::routines::running_start(&user, &r.name, &app.session_id);
        if mine {
            convs[0].app.log(Level::Plan, format!("routine {} started", r.name));
        }
        let mut c = Conv::new(app);
        c.routine = Some((r, Instant::now()));
        convs.push(c);
    }
}

/// A routine's run is done and judged: told (by its own rules) and kept.
fn routine_done(convs: &mut [Conv], hub: &Hub, user: &str, r: &crate::routines::Routine, run: crate::routines::Run) {
    let verdict = if run.outcome != "ok" { format!("{} ({})", run.outcome, run.decided_by) } else if run.needs_user { "needs you".into() } else { "all clear".into() };
    let first = run.summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(140).collect::<String>();
    // The owner's Activity tells the owner's routines only.
    if user == convs[0].app.owner {
        convs[0].app.log(if run.needs_user { Level::Error } else { Level::Plan }, format!("routine {}: {verdict} ({}s, by {}) — {first}", r.name, run.seconds, run.decided_by));
        if !run.emailed.is_empty() {
            convs[0].app.log(if run.emailed.starts_with("not ") { Level::Error } else { Level::Plan }, format!("routine {}: {}", r.name, run.emailed));
        }
    }
    let tell = match r.notify {
        crate::routines::Notify::Always => true,
        crate::routines::Notify::Problems => run.needs_user,
        crate::routines::Notify::Never => false,
    };
    if tell {
        hub.notify(Notification {
            title: format!("{} {}", if run.needs_user { "⚠" } else { "✓" }, r.name),
            body: preview(&run.summary),
            tag: format!("routine-{}", r.name),
            approval: None,
            url: None, actions: vec![], reference: None,
            to: To::User(user.to_string()),
        });
    }
    crate::acting::run(user, || crate::routines::record(&r.name, run));

}

/// lyra was asked to stop: wait for what's running (up to `drain()`), then
/// note the routine runs still going (they start again after the restart),
/// tell the replies cut off, save every conversation and exit.
fn drain_step(convs: &mut [Conv], draining: &mut Option<Instant>, finishing: usize) {
    let busy: Vec<String> = convs
        .iter()
        .filter(|c| c.app.waiting || c.app.plan_busy)
        .map(|c| c.routine.as_ref().map_or_else(|| if c.diagnosis.is_some() { "a problem write-up".to_string() } else { format!("a reply for {}", c.app.owner) }, |(r, _)| format!("routine {}", r.name)))
        .chain((finishing > 0).then(|| format!("{finishing} routine result(s) being recorded")))
        .chain(crate::backup::running().then(|| "a backup".to_string()))
        .collect();
    let since = *draining.get_or_insert_with(|| {
        println!("stopping: {}", if busy.is_empty() { "nothing running".to_string() } else { format!("waiting up to {}s for {}", drain().as_secs(), busy.join(", ")) });
        Instant::now()
    });
    if busy.is_empty() || since.elapsed() >= drain() {
        // What's still going: routines start again after the restart; replies say so.
        let mut again = Vec::new();
        for c in convs.iter_mut() {
            if let Some((r, _)) = &c.routine {
                again.push((c.app.owner.clone(), r.name.clone()));
            } else if c.app.waiting && c.diagnosis.is_none() {
                c.app.messages.push(Message::new("error", "lyra restarted while answering this. Try again to send it once more.".into()));
            }
            c.app.save_session();
        }
        if !again.is_empty() {
            println!("stopping: {} routine run(s) will start again: {}", again.len(), again.iter().map(|a| a.1.as_str()).collect::<Vec<_>>().join(", "));
            crate::routines::save_resume(&again);
        }
        println!("stopped");
        std::process::exit(0);
    }
}

/// A message from a device, ready to go (its attachments read).
pub(crate) struct Ready {
    text: String,
    device: String,
    who: Who,
    session: String,
    conn: u64,
    images: Vec<String>,
    looks: Vec<(String, String, std::path::PathBuf)>,
}

/// Send a device's message: `/new`, `/resume`, or into its conversation. The
/// attached pictures go with it only when it's sent as a message (never left
/// behind for the next one, after a command or while lyra is busy).
fn deliver(convs: &mut Vec<Conv>, hub: &Hub, m: Ready) {
    let Ready { text, device, who, session, conn, images, looks } = m;
    let i = conv_for(convs, &session, &who);
    // A new conversation, or another one, for this device only.
    if text == "/new" {
        let app = convs[0].app.fork_for(&who);
        let id = app.session_id.clone();
        convs.push(Conv::new(app));
        hub.attach(conn, &id);
        convs[0].app.log(Level::Info, format!("{device} ({}) started a new conversation", who.name));
    } else if let Some(key) = text.strip_prefix("/resume ").map(str::trim).filter(|k| !k.is_empty()) {
        let open = convs.iter().position(|c| c.app.owner == who.user && c.app.session_id.starts_with(key));
        match open.or_else(|| find_conv(convs, key, &who)) {
            Some(j) => {
                let id = convs[j].app.session_id.clone();
                hub.attach(conn, &id);
            }
            None => {
                convs[i].app.messages.push(Message::new("error", format!("> {text}\nno saved conversation {key:?}")));
                convs[i].changed = true;
            }
        }
    } else {
        let c = &mut convs[i];
        if crate::serve::stopping() && !text.starts_with('/') {
            c.app.messages.push(Message::new("info", "lyra is restarting in a moment: send it again shortly.".into()));
        } else if c.app.waiting && !text.starts_with('/') && c.app.approvals.is_empty() {
            c.app.messages.push(Message::new("info", "lyra is still answering here — send it again when the reply is done (or start a new conversation)".into()));
        } else {
            c.app.log(Level::Info, format!("from {device}: {}", crate::shown(&text).chars().take(80).collect::<String>()));
            c.app.input = text;
            c.app.attach_images = images;
            c.app.attach_looks = looks;
            c.app.send();
            // A command or an approval's answer doesn't take them: they don't wait for the next message.
            c.app.attach_images.clear();
            c.app.attach_looks.clear();
        }
        c.changed = true;
    }
}


