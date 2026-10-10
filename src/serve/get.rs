//! A page's request (`get`): who may ask, then the answer, from the
//! conversation it's for, off the loop when it calls out or reads a lot.

use super::*;

/// Answer one page request; true when every device should hear about it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn get_page(
    convs: &mut Vec<Conv>,
    hub: &Hub,
    node_build: Option<&str>,
    switch_tx: &std::sync::mpsc::Sender<(String, String, tokio::sync::oneshot::Sender<Value>)>,
    what: String,
    arg: Value,
    session: String,
    who: Who,
    reply: tokio::sync::oneshot::Sender<Value>,
) -> bool {
    let mut everyone = false;
    sync_role(convs, &who);
    // Pages a member may open; the rest are admins' (or still the owner's data).
    if !who.admin && !member_may_get(&what) {
        let _ = reply.send(json!({ "error": "that's for admins" }));
        return everyone;
    }
    // Running now (admins): every conversation at work, anyone's; and Stop.
    if what == "running" {
        let _ = reply.send(running_view(convs, hub));
        return everyone;
    }
    if what == "running_stop" {
        let id = arg["session"].as_str().unwrap_or("");
        let answer = match convs.iter_mut().find(|c| c.app.session_id == id) {
            Some(c) => {
                let r = if c.app.waiting { c.app.stop() } else if c.app.plan_busy { c.app.command_result("/plan cancel") } else { Err("it isn't running any more".into()) };
                c.changed = true;
                if r.is_ok() {
                    convs[0].app.log(Level::Agent, format!("{} stopped {id} (Running now)", who.name));
                }
                r
            }
            None => Err("it isn't running any more".into()),
        };
        let _ = reply.send(match answer {
            Ok(t) => json!({ "ok": true, "text": t }),
            Err(e) => json!({ "error": e }),
        });
        everyone = true;
        return everyone;
    }
    let loaded: Loaded = convs.iter().filter(|c| c.app.owner == who.user).map(|c| (c.app.session_id.clone(), c.app.waiting)).collect();
    let i = conv_for(convs, &session, &who);
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
            crate::acting::spawn(move || {
                let answer = hub.call_machine(&machine, request, Duration::from_secs(20));
                let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
            });
        }
        // A coding job's changes: `git diff` in its folder, read-only, on its machine.
        "coding_diff" => {
            let (dir, caps) = (arg["dir"].as_str().unwrap_or("").to_string(), convs[0].app.caps.clone());
            crate::acting::spawn(move || {
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
            crate::acting::spawn(move || {
                let answer = crate::acting::run(&user, || crate::feedback::enhance(&kind, &title, &details));
                let _ = reply.send(answer.map_or_else(|e| json!({ "error": e }), |text| json!({ "text": text })));
            });
        }
        // One search for everything (Ctrl-K): off this loop, their own things only.
        "everything" => {
            let (query, user) = (arg["query"].as_str().unwrap_or("").to_string(), convs[i].app.owner.clone());
            let mem = convs[i].app.tools.as_ref().map(|t| t.mem.clone());
            crate::acting::spawn(move || {
                let _ = reply.send(crate::search::everything(&query, &user, mem));
            });
        }
        "models" => {
            let (url, current) = (convs[i].app.base_url.clone(), convs[i].app.model.clone());
            crate::acting::spawn(move || {
                let answer = match crate::models(&url) {
                    Ok(list) => json!({ "current": current, "models": list }),
                    Err(e) => json!({ "current": current, "models": [], "error": e }),
                };
                let _ = reply.send(answer);
            });
        }
        // A page's button: memory, skills, goals, model commands, answered to the page.
        "do" if convs[i].app.off_loop(arg["command"].as_str().unwrap_or("").trim()).is_some() => {
            let line = arg["command"].as_str().unwrap_or("").trim().to_string();
            let job = convs[i].app.off_loop(&line).expect("checked above");
            convs[i].app.log(Level::Info, format!("from the app: {}", crate::shown(&line)));
            crate::acting::spawn(move || {
                let _ = reply.send(match job() {
                    Ok(text) => json!({ "ok": true, "text": text }),
                    Err(e) => json!({ "ok": false, "text": e }),
                });
            });
        }
        // Switching the model: the server is asked off the loop; the switch comes back here.
        "do" if convs[i].app.admin && arg["command"].as_str().unwrap_or("").trim().strip_prefix("/model ").is_some_and(|n| !n.trim().is_empty()) => {
            let name = arg["command"].as_str().unwrap_or("").trim().trim_start_matches("/model ").trim().to_string();
            let (url, tx) = (convs[i].app.base_url.clone(), switch_tx.clone());
            crate::acting::spawn(move || match crate::commands::check_model(&url, &name) {
                Ok(()) => {
                    let _ = tx.send((name, url, reply));
                }
                Err(e) => {
                    let _ = reply.send(json!({ "ok": false, "text": e }));
                }
            });
        }
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
        // Pages that read every conversation or call out: on a thread (I-6).
        _ if slow(&convs[i].app, &what, &arg, &loaded).is_some() => {
            let job = slow(&convs[i].app, &what, &arg, &loaded).expect("checked above");
            crate::acting::spawn(move || {
                let _ = reply.send(job());
            });
        }
        _ => {
            let _ = reply.send(data(&mut convs[i].app, hub, &what, &arg, node_build, &loaded));
        }
    }

    everyone
}

/// Running now (admins): each conversation at work, whoever's: what it is,
/// since when, what it's doing, what its model calls have cost so far.
fn running_view(convs: &[Conv], hub: &Hub) -> Value {
    let users = hub.users();
    let mut out = Vec::new();
    for c in convs.iter().filter(|c| c.app.waiting || c.app.plan_busy) {
        let who = users.get(&c.app.owner).map_or_else(|| c.app.owner.clone(), |u| u.name);
        let (kind, what, since) = if let Some((r, started)) = &c.routine {
            ("routine", r.name.clone(), Some(chrono::Utc::now() - chrono::Duration::from_std(started.elapsed()).unwrap_or_default()))
        } else if c.diagnosis.is_some() {
            ("diagnosis", c.app.messages.iter().find(|m| m.role == "user").map(|m| m.content.lines().next().unwrap_or("").chars().take(80).collect()).unwrap_or_default(), None)
        } else if c.app.plan_busy && !c.app.waiting {
            ("plan", c.app.current_goal.as_ref().map(|g| g.description.clone()).or_else(|| c.app.current_plan.as_ref().map(|p| lyra_execution::short(p.id))).unwrap_or_else(|| "a plan".into()), None)
        } else {
            ("reply", title(&c.app).unwrap_or_default(), None)
        };
        let since = since.or_else(|| c.app.started.map(|s| chrono::Utc::now() - chrono::Duration::from_std(s.elapsed()).unwrap_or_default()));
        let spent = crate::usage::spent_now(&c.app.session_id, since.unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::hours(24)));
        out.push(json!({
            "session": c.app.session_id, "who": who, "kind": kind, "what": what, "since": since,
            "doing": crate::ui::doing_text(&c.app), "calls": spent.calls, "tokens_in": spent.input, "tokens_out": spent.output, "cost": spent.cost,
            "stoppable": c.app.waiting || c.app.plan_busy,
        }));
    }
    json!(out)
}
