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
    let mut brief_busy = false;
    let mut last_brief_check = Instant::now() - Duration::from_secs(60);
    // PMI: its live events (a thread), and the view read after each change.
    let (pmi_events_tx, pmi_events) = std::sync::mpsc::channel::<(String, crate::pmi::Event)>();
    let (pmi_tx, pmi_rx) = std::sync::mpsc::channel::<(String, Result<crate::pmi::State, String>)>();
    // Each person with a PMI token: their live view, followed while lyra runs.
    let mut pmi: std::collections::HashMap<String, PmiUser> = std::collections::HashMap::new();
    let mut last_pmi_scan = Instant::now() - Duration::from_secs(120);
    let mut last_nag = Instant::now();
    // Plan my day: re-planned every 15 minutes in each person's working day;
    // what changed is told when they aren't in quiet time (held until then).
    let (plan_tx, plan_rx) = std::sync::mpsc::channel::<(String, Result<Vec<String>, String>)>();
    let mut last_plan = Instant::now() - Duration::from_secs(3600);
    let mut planning: std::collections::HashSet<String> = Default::default();
    let mut held: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut quiet_seen: std::collections::HashMap<String, (bool, Instant)> = Default::default();
    let mut last_held = Instant::now();
    // Proactive help: a pass every 5 minutes in the working day (mail every other one).
    let (pro_tx, pro_rx) = std::sync::mpsc::channel::<(String, crate::proactive::Pass)>();
    let mut last_pro = Instant::now() - Duration::from_secs(3600);
    let mut pro_round: u64 = 0;
    let mut pro_busy: std::collections::HashSet<String> = Default::default();
    let (action_tx, action_rx) = std::sync::mpsc::channel::<(String, String, String, Result<String, String>)>();
    // The end-of-day recap: looked for once a minute, made once a day per person.
    let (recap_tx, recap_rx) = std::sync::mpsc::channel::<(String, crate::recap::Recap)>();
    let mut last_recap_look = Instant::now() - Duration::from_secs(120);
    let mut recapping: std::collections::HashSet<String> = Default::default();
    // "Tell me when …": each person's watches, looked at every two minutes.
    let (watch_tx, watch_rx) = std::sync::mpsc::channel::<(String, Vec<(String, String)>)>();
    let mut last_watch = Instant::now();
    let mut watching: std::collections::HashSet<String> = Default::default();
    let started = Instant::now();
    let mut convs = vec![Conv::new(primary)];
    loop {
        let mut everyone = false;
        // What the devices ask for (waiting a little when there's nothing).
        let mut next = inbound.recv_timeout(Duration::from_millis(40)).ok();
        while let Some(msg) = next.take() {
            match msg {
                Inbound::Send { text, device, who, session, conn, files } => {
                    sync_role(&mut convs, &who);
                    let i = conv_for(&mut convs, &session, &who);
                    // Attached files: described (or read) in the message itself.
                    let (about, images, looks) = attachments(hub, &files, convs[i].app.vision, &who);
                    let text = format!("{text}{about}").trim().to_string();
                    convs[i].app.attach_images = images;
                    convs[i].app.attach_looks = looks;
                    // A new conversation, or another one, for this device only.
                    if text == "/new" {
                        let app = convs[0].app.fork_for(&who);
                        let id = app.session_id.clone();
                        convs.push(Conv::new(app));
                        hub.attach(conn, &id);
                        convs[0].app.log(Level::Info, format!("{device} ({}) started a new conversation", who.name));
                    } else if let Some(key) = text.strip_prefix("/resume ").map(str::trim).filter(|k| !k.is_empty()) {
                        let open = convs.iter().position(|c| c.app.owner == who.user && c.app.session_id.starts_with(key));
                        match open.or_else(|| find_conv(&mut convs, key, &who)) {
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
                        if c.app.waiting && !text.starts_with('/') && c.app.approvals.is_empty() {
                            c.app.messages.push(Message::new("info", "lyra is still answering here — send it again when the reply is done (or start a new conversation)".into()));
                        } else {
                            c.app.log(Level::Info, format!("from {device}: {}", crate::shown(&text).chars().take(80).collect::<String>()));
                            c.app.input = text;
                            c.app.send();
                        }
                        c.changed = true;
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
                Inbound::Action { action, reference, device, who } => {
                    if who.user == convs[0].app.owner {
                        convs[0].app.log(Level::Info, format!("{device} pressed {action} on a reminder"));
                    }
                    let tx = action_tx.clone();
                    std::thread::spawn(move || {
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
                    sync_role(&mut convs, &who);
                    // Pages a member may open; the rest are admins' (or still the owner's data).
                    if !who.admin && !matches!(what.as_str(), "sessions" | "search" | "status" | "agents" | "skills" | "activity" | "about" | "models" | "do" | "pmi" | "routines" | "goals" | "memory" | "calendar" | "mail" | "notes" | "usage" | "recap" | "watches" | "everything" | "changelog" | "feedback" | "feedback_submit" | "feedback_comment" | "feedback_update" | "feedback_seen" | "feedback_analyze" | "feedback_enhance" | "qa" | "qa_put" | "qa_remove" | "qa_promote") {
                        let _ = reply.send(json!({ "error": "that's for admins" }));
                        continue;
                    }
                    let loaded: Loaded = convs.iter().filter(|c| c.app.owner == who.user).map(|c| (c.app.session_id.clone(), c.app.waiting)).collect();
                    let i = conv_for(&mut convs, &session, &who);
                    let machine = arg["machine"].as_str().unwrap_or("server").to_string();
                    match what.as_str() {
                        // A machine's rules: it answers (and checks changes) itself, off this loop.
                        "rules" | "set_rules" if machine != "server" => {
                            let hub = hub.clone();
                            let mut request = json!({ "type": "rules" });
                            if what == "set_rules" {
                                request["set"] = arg["system"].clone();
                                convs[0].app.log(Level::Agent, format!("rules for {machine} changed from the app"));
                            }
                            std::thread::spawn(move || {
                                let answer = hub.call_machine(&machine, request, Duration::from_secs(20));
                                let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
                            });
                        }
                        // A coding job's changes: `git diff` in its folder, read-only, on its machine.
                        "coding_diff" => {
                            let (dir, caps) = (arg["dir"].as_str().unwrap_or("").to_string(), convs[0].app.caps.clone());
                            std::thread::spawn(move || {
                                let shell = json!({ "command": "git diff --stat HEAD~0 && git diff && git status --short", "cwd": dir });
                                let answer = if machine == crate::caps::HERE {
                                    caps.as_ref().and_then(|c| c.system.as_ref()).ok_or("system access is off".to_string()).and_then(|s| s.call("shell_run", &shell))
                                } else {
                                    caps.as_ref().and_then(|c| c.remote()).ok_or("no machines".to_string()).and_then(|r| r.call(&machine, json!({ "type": "call", "tool": "shell_run", "args": shell, "approved": false }), Duration::from_secs(30)))
                                };
                                let _ = reply.send(answer.map(|v| json!({ "diff": v["stdout"], "error": v["error"] })).unwrap_or_else(|e| json!({ "error": e })));
                            });
                        }
                        // The Settings page's Save: written to config.toml, then every
                        // conversation picks it up (the model and prices are each one's own).
                        "settings_set" => {
                            let answer = match crate::settings::set(&arg["changes"]) {
                                Ok(changed) if changed.is_empty() => json!({ "ok": true, "changed": changed, "page": crate::settings::page() }),
                                Ok(changed) => {
                                    convs[0].app.reload();
                                    let (url, model, pricing, status) = (convs[0].app.base_url.clone(), convs[0].app.model.clone(), convs[0].app.pricing.clone(), convs[0].app.status.clone());
                                    for c in convs.iter_mut() {
                                        c.app.base_url = url.clone();
                                        c.app.model = model.clone();
                                        c.app.pricing = pricing.clone();
                                        c.app.status = status.clone();
                                        c.changed = true;
                                    }
                                    convs[0].app.log(Level::Agent, format!("settings changed from the app by {}: {}", who.user, changed.join(", ")));
                                    json!({ "ok": true, "changed": changed, "page": crate::settings::page() })
                                }
                                Err(e) => json!({ "ok": false, "error": e }),
                            };
                            let _ = reply.send(answer);
                        }
                        "set_rules" => {
                            let answer = set_server_rules(&mut convs[0].app, &arg["system"]);
                            let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
                        }
                        // A clearer draft for the feedback form: off this loop (the model takes a moment).
                        "feedback_enhance" => {
                            let (kind, title, details, user) = (arg["kind"].as_str().unwrap_or("bug").to_string(), arg["title"].as_str().unwrap_or("").to_string(), arg["details"].as_str().unwrap_or("").to_string(), convs[i].app.owner.clone());
                            std::thread::spawn(move || {
                                let answer = crate::acting::run(&user, || crate::feedback::enhance(&kind, &title, &details));
                                let _ = reply.send(answer.map_or_else(|e| json!({ "error": e }), |text| json!({ "text": text })));
                            });
                        }
                        // One search for everything (Ctrl-K): off this loop, their own things only.
                        "everything" => {
                            let (query, user) = (arg["query"].as_str().unwrap_or("").to_string(), convs[i].app.owner.clone());
                            let mem = convs[i].app.tools.as_ref().map(|t| t.mem.clone());
                            std::thread::spawn(move || {
                                let _ = reply.send(crate::search::everything(&query, &user, mem));
                            });
                        }
                        "models" => {
                            let (url, current) = (convs[i].app.base_url.clone(), convs[i].app.model.clone());
                            std::thread::spawn(move || {
                                let answer = match crate::models(&url) {
                                    Ok(list) => json!({ "current": current, "models": list }),
                                    Err(e) => json!({ "current": current, "models": [], "error": e }),
                                };
                                let _ = reply.send(answer);
                            });
                        }
                        // A page's button: memory, skills, goals, model commands, answered to the page.
                        "do" => {
                            let line = arg["command"].as_str().unwrap_or("").trim().to_string();
                            let result = convs[i].app.quiet_command(&line);
                            if result.is_ok() && line.starts_with("/model ") {
                                let model = convs[i].app.model.clone();
                                for c in convs.iter_mut() {
                                    c.app.model = model.clone();
                                }
                            }
                            for c in convs.iter_mut() {
                                c.changed = true;
                            }
                            let _ = reply.send(match result {
                                Ok(text) => json!({ "ok": true, "text": text }),
                                Err(e) => json!({ "ok": false, "text": e }),
                            });
                        }
                        _ => {
                            let _ = reply.send(data(&mut convs[i].app, hub, &what, &arg, node_build.as_deref(), &loaded));
                        }
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
                        std::thread::spawn(move || {
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
                        let reply = c.app.messages.iter().rev().find(|m| matches!(m.role.as_str(), "assistant" | "error")).map(|m| (m.role.clone(), m.content.clone()));
                        let outcome = match &reply {
                            Some((role, _)) if role == "error" => "error",
                            Some((_, content)) if content.ends_with("_(stopped)_") => "stopped",
                            _ => "ok",
                        };
                        let text = reply.map(|m| m.1).unwrap_or_default();
                        let (url, model, session, tx) = (format!("{}/chat/completions", c.app.base_url.trim_end_matches('/')), c.app.model.clone(), c.app.session_id.clone(), routine_tx.clone());
                        let whose = c.app.owner.clone();
                        std::thread::spawn(move || {
                            let (needs_user, decided_by) = if outcome == "ok" { crate::routines::needs_user(&url, &model, &r, &text) } else { (true, "it didn't finish".into()) };
                            let summary: String = text.trim().chars().take(400).collect();
                            let run = crate::routines::Run { at: chrono::Utc::now(), seconds: started.elapsed().as_secs(), needs_user, outcome: outcome.into(), summary, session, decided_by };
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
        if last_routines.elapsed() >= Duration::from_secs(20) || routines_wanted() {
            last_routines = Instant::now();
            // Everyone's: each runs in its person's own conversation, with their rights.
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
                app.input = crate::routines::message(&r);
                app.send();
                if mine {
                    convs[0].app.log(Level::Plan, format!("routine {} started", r.name));
                }
                let mut c = Conv::new(app);
                c.routine = Some((r, Instant::now()));
                convs.push(c);
            }
        }
        while let Ok((user, r, run)) = routine_rx.try_recv() {
            let verdict = if run.outcome != "ok" { format!("{} ({})", run.outcome, run.decided_by) } else if run.needs_user { "needs you".into() } else { "all clear".into() };
            let first = run.summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(140).collect::<String>();
            // The owner's Activity tells the owner's routines only.
            if user == convs[0].app.owner {
                convs[0].app.log(if run.needs_user { Level::Error } else { Level::Plan }, format!("routine {}: {verdict} ({}s, by {}) — {first}", r.name, run.seconds, run.decided_by));
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
                    to: To::User(user.clone()),
                });
            }
            crate::acting::run(&user, || crate::routines::record(&r.name, run));
            everyone = true;
        }
        // PMI, per person with a token: follow their live events (a thread each,
        // started as tokens appear), read again shortly after a change and every 5 minutes.
        if last_pmi_scan.elapsed() >= Duration::from_secs(60) {
            last_pmi_scan = Instant::now();
            let people: Vec<String> = hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active && !pmi.contains_key(&u.id) && crate::pmi::configured_for(&u.id)).map(|u| u.id).collect();
            for user in people {
                let tx = pmi_events_tx.clone();
                let who = user.clone();
                std::thread::spawn(move || crate::pmi::follow(who, tx));
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
                last_pmi_scan = Instant::now() - Duration::from_secs(120);
            }
        }
        for (user, p) in pmi.iter_mut() {
            if !p.busy && (p.due.is_some_and(|d| Instant::now() >= d) || p.last.elapsed() >= Duration::from_secs(300)) {
                p.busy = true;
                p.due = None;
                p.last = Instant::now();
                let (tx, user) = (pmi_tx.clone(), user.clone());
                std::thread::spawn(move || {
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
        // Nags: each person's, checked with each new view and every minute.
        if last_nag.elapsed() >= Duration::from_secs(60) || everyone {
            last_nag = Instant::now();
            let s = crate::pmi::settings();
            let owner = convs[0].app.owner.clone();
            for (user, p) in pmi.iter_mut().filter(|(_, p)| p.state.at.is_some() && p.state.error.is_none()) {
                // Not in their quiet time (outside work hours, in a meeting): nags wait.
                let quiet = match quiet_seen.get(user) {
                    Some((q, at)) if at.elapsed() < Duration::from_secs(300) => *q,
                    _ => {
                        let q = crate::planner::quiet(user);
                        quiet_seen.insert(user.clone(), (q, Instant::now()));
                        q
                    }
                };
                if quiet {
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
        while let Ok((user, action, task, r)) = action_rx.try_recv() {
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
        if last_plan.elapsed() >= Duration::from_secs(15 * 60) && crate::planner::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            last_plan = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if planning.contains(&u) || !crate::graph::connected_for(&u) || !crate::pmi::configured_for(&u) {
                    continue;
                }
                planning.insert(u.clone());
                let tx = plan_tx.clone();
                std::thread::spawn(move || {
                    let r = crate::acting::run(&u, crate::planner::run);
                    let _ = tx.send((u, r));
                });
            }
        }
        while let Ok((user, r)) = plan_rx.try_recv() {
            planning.remove(&user);
            let mine = user == convs[0].app.owner;
            match r {
                Ok(did) if !did.is_empty() => {
                    if mine {
                        for d in &did {
                            convs[0].app.log(Level::Plan, format!("🗓 {d}"));
                        }
                    }
                    held.entry(user).or_default().extend(did);
                }
                Ok(_) => {}
                Err(e) if mine => convs[0].app.log(Level::Error, format!("plan my day: {e}")),
                Err(_) => {}
            }
        }
        // Proactive help, for everyone with Outlook connected.
        if last_pro.elapsed() >= Duration::from_secs(5 * 60) && crate::proactive::settings().enabled && crate::planner::settings().working_now(chrono::Local::now()) {
            last_pro = Instant::now();
            pro_round += 1;
            let mail_due = pro_round % 2 == 1;
            let (url, model) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone());
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if pro_busy.contains(&u) || !crate::graph::connected_for(&u) {
                    continue;
                }
                pro_busy.insert(u.clone());
                let (tx, url, model) = (pro_tx.clone(), url.clone(), model.clone());
                std::thread::spawn(move || {
                    let p = crate::acting::run(&u, || crate::proactive::pass(&url, &model, mail_due));
                    let _ = tx.send((u, p));
                });
            }
        }
        if last_recap_look.elapsed() >= Duration::from_secs(60) && crate::recap::settings().enabled {
            last_recap_look = Instant::now();
            let now = chrono::Local::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if recapping.contains(&u) || !(crate::graph::connected_for(&u) || crate::pmi::configured_for(&u)) || !crate::recap::due(&u, now) {
                    continue;
                }
                recapping.insert(u.clone());
                let tx = recap_tx.clone();
                std::thread::spawn(move || {
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
        if last_watch.elapsed() >= Duration::from_secs(120) {
            last_watch = Instant::now();
            for u in hub.users().list().into_iter().filter(|u| u.status == lyra_web::Status::Active).map(|u| u.id) {
                if watching.contains(&u) || !crate::watches::any(&u) {
                    continue;
                }
                watching.insert(u.clone());
                let tx = watch_tx.clone();
                std::thread::spawn(move || {
                    let fired = crate::acting::run(&u, crate::watches::pass);
                    let _ = tx.send((u, fired));
                });
            }
        }
        while let Ok((user, fired)) = watch_rx.try_recv() {
            watching.remove(&user);
            for (title, body) in fired {
                if user == convs[0].app.owner {
                    convs[0].app.log(Level::Plan, format!("👀 {title}: {body}"));
                }
                hub.notify(Notification { title, body, tag: "watch".into(), approval: None, url: None, actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, r)) = recap_rx.try_recv() {
            recapping.remove(&user);
            crate::recap::save_for(&user, &r);
            if user == convs[0].app.owner {
                convs[0].app.log(Level::Plan, format!("end of day: {}", crate::recap::push_body(&r)));
            }
            if crate::recap::settings().notify && !r.parts.is_empty() {
                hub.notify(Notification { title: "End of day".into(), body: crate::recap::push_body(&r), tag: "recap".into(), approval: None, url: Some("/?page=status".into()), actions: vec![], reference: None, to: To::User(user.clone()) });
            }
            everyone = true;
        }
        while let Ok((user, p)) = pro_rx.try_recv() {
            pro_busy.remove(&user);
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
                held.entry(user).or_default().extend(p.done);
            }
        }
        // What the planner did, told once they're out of quiet time.
        if !held.is_empty() && last_held.elapsed() >= Duration::from_secs(60) {
            last_held = Instant::now();
            let ready: Vec<String> = held.keys().filter(|u| !quiet_seen.get(*u).is_some_and(|(q, at)| *q && at.elapsed() < Duration::from_secs(300))).cloned().collect();
            for user in ready {
                if crate::planner::quiet(&user) {
                    quiet_seen.insert(user.clone(), (true, Instant::now()));
                    continue;
                }
                let did = held.remove(&user).unwrap_or_default();
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
        // The daily briefing: on schedule or asked for, gathered and written off the loop.
        let wanted = crate::briefing::take_request();
        if !brief_busy && (wanted || last_brief_check.elapsed() >= Duration::from_secs(20)) {
            last_brief_check = Instant::now();
            let s = crate::briefing::settings();
            let now = chrono::Local::now();
            let due = s.enabled && crate::briefing::next(&s.schedule, last_brief.as_ref().map(|b| b.at), now).is_some_and(|t| t <= now);
            if wanted || due {
                brief_busy = true;
                let at = chrono::Utc::now();
                let since = crate::briefing::window_start(last_brief.as_ref().map(|b| b.at), at);
                let mut inputs = crate::briefing::local_inputs(convs[0].app.goals.as_deref(), at, since);
                inputs.machines = machines_detail(hub, node_build.as_deref());
                inputs.server_health = (!server_health.is_null()).then(|| server_health.clone());
                inputs.pmi = pmi.get(&convs[0].app.owner).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
                let (url, model, tx) = (format!("{}/chat/completions", convs[0].app.base_url.trim_end_matches('/')), convs[0].app.model.clone(), brief_tx.clone());
                let owner = convs[0].app.owner.clone();
                // Everyone else with PMI: their own, with just their tasks.
                // Everyone else with PMI or a calendar connected: their own.
                let others: Vec<(String, crate::briefing::Inputs)> = hub
                    .users()
                    .list()
                    .into_iter()
                    .filter(|u| u.status == lyra_web::Status::Active && u.id != owner)
                    .map(|u| u.id)
                    .filter(|u| pmi.get(u).is_some_and(|p| p.state.at.is_some()) || crate::graph::connected_for(u))
                    .map(|u| {
                        let since = crate::briefing::window_start(crate::briefing::last_for(&u).map(|b| b.at), at);
                        // Their tasks, routines and goals.
                        let (goals, goal_events) = crate::goals::for_user(&u).map_or_else(Default::default, |g| (g.manager.all().unwrap_or_default(), g.manager.events(None, 300).unwrap_or_default()));
                        let runs = crate::acting::run(&u, crate::routines::runs);
                        let pmi = pmi.get(&u).filter(|p| p.state.at.is_some()).map(|p| p.state.clone());
                        (u, crate::briefing::Inputs { now: at, since, pmi, runs, goals, goal_events, ..Default::default() })
                    })
                    .collect();
                std::thread::spawn(move || {
                    for (user, mut inputs) in std::iter::once((owner, inputs)).chain(others) {
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
                        if s.summary {
                            b.takeaway = crate::acting::run(&user, || crate::briefing::takeaway(&url, &model, &b));
                        }
                        let _ = tx.send((user, b));
                    }
                });
            }
        }
        while let Ok((user, b)) = brief_rx.try_recv() {
            let owner = user == convs[0].app.owner;
            crate::briefing::save_for(&user, &b);
            if owner {
                brief_busy = false;
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
            if d.enabled && !convs.iter().any(|c| c.diagnosis.is_some()) && crate::diagnose::has_queued() {
                let mut app = convs[0].app.fork();
                let session = app.session_id.clone();
                if let Some(next) = crate::diagnose::start_next(|_| session.clone()) {
                    app.input = crate::diagnose::message(&next);
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
        if last_routines.elapsed() < Duration::from_millis(50) || everyone {
            for user in crate::routines::people() {
                let running: Vec<String> = convs.iter().filter(|c| c.app.owner == user).filter_map(|c| c.routine.as_ref().map(|(r, _)| r.name.clone())).collect();
                let now = crate::acting::run(&user, || crate::routines::view(&running, 1));
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
                    for (id, problem, text) in crate::status::alerts(&mut status_alerts, &board, &ss) {
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
                                url: None, actions: vec![], reference: None,
                            to: To::Admins,
                            });
                        }
                    }
                    status_view = json!(board);
                    everyone = true;
                }
                Err(e) => convs[0].app.log(Level::Error, format!("status: {e}")),
            }
        }
        // The server's own health, like a machine's.
        if last_checkup.is_none_or(|t| t.elapsed() >= lyra_node::health::EVERY) {
            last_checkup = Some(Instant::now());
            let tx = health_tx.clone();
            std::thread::spawn(move || {
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
