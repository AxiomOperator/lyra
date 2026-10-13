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
    // The browser sandbox's containers (admins: this isn't a member's page), asked of its machine off the loop.
    if what == "sandbox_containers" {
        let users = hub.users();
        let named = |owner: &str| users.get(owner).map_or_else(|| owner.to_string(), |u| u.name);
        let open: Vec<(String, String, String)> = convs.iter().map(|c| (c.app.session_id.clone(), super::title(&c.app).unwrap_or_default(), named(&c.app.owner))).collect();
        let names: Vec<(String, String)> = users.list().into_iter().map(|u| (u.id, u.name)).collect();
        let hub = hub.clone();
        crate::acting::spawn(move || {
            let remote = HubRemote(hub);
            let mut v = crate::sandbox::live(Some(&remote)).unwrap_or_else(|e| json!({ "error": e }));
            let dir = crate::sessions::dir();
            for field in ["containers", "queued"] {
                let Some(list) = v[field].as_array_mut() else { continue };
                for c in list.iter_mut() {
                    let Some(key) = c["session"].as_str().map(str::to_string) else { continue };
                    // Its conversation: open now, else its saved file.
                    let found = open.iter().find(|(id, ..)| *id == key).map(|(_, t, w)| (t.clone(), w.clone())).or_else(|| {
                        let s = dir.as_ref().and_then(|d| crate::sessions::list(d).into_iter().find(|s| s.id == key))?;
                        Some((s.label(), names.iter().find(|(id, _)| *id == s.owner).map_or_else(|| s.owner.clone(), |(_, n)| n.clone())))
                    });
                    if let Some((title, who)) = found {
                        c["conversation"] = json!(title);
                        c["who"] = json!(who);
                    }
                }
            }
            let _ = reply.send(v);
        });
        return everyone;
    }
    if what == "sandbox_kill" {
        let (hub, name, by) = (hub.clone(), arg["container"].as_str().unwrap_or("").to_string(), who.name.clone());
        convs[0].app.log(Level::Agent, format!("{by} stops browser container {name} (Running now)"));
        crate::acting::spawn(move || {
            let r = crate::sandbox::kill(Some(&HubRemote(hub)), &name);
            let _ = reply.send(match r {
                Ok(_) => json!({ "ok": true }),
                Err(e) => json!({ "error": e }),
            });
        });
        return true;
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
            let confirmed: Vec<String> = arg["confirmed"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
            let answer = match crate::settings::set_confirmed(&arg["changes"], &confirmed) {
                Ok(changed) if changed.is_empty() => json!({ "ok": true, "changed": changed, "page": crate::settings::page() }),
                Ok(changed) => {
                    reload_all(convs, hub);
                    convs[0].app.log(Level::Agent, format!("settings changed from the app by {}: {}", who.user, changed.join(", ")));
                    let keys: Vec<String> = arg["changes"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
                    convs[0].app.note_settings_edit(&who.user, &keys);
                    json!({ "ok": true, "changed": changed, "page": crate::settings::page() })
                }
                Err(e) => json!({ "ok": false, "error": e }),
            };
            let _ = reply.send(answer);
        }
        // Settings → Everything: every setting there is, with its value now (keys hidden).
        "settings_all" => {
            let path = crate::config::path();
            let text = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            let mut v = crate::config_schema::page(&text);
            v["path"] = json!(path.map(|p| crate::context::show(&p)));
            let _ = reply.send(v);
        }
        // Settings → History: config.toml's versions (an edit by hand since the last is kept first).
        "config_history" => {
            let answer = match crate::config::path() {
                Some(p) => {
                    crate::config_history::notice_hand_edit_at(&p);
                    json!({ "versions": crate::config_history::page_at(&p) })
                }
                None => json!({ "error": "no home directory" }),
            };
            let _ = reply.send(answer);
        }
        "config_diff" => {
            let answer = crate::config::path().ok_or("no home directory".to_string()).and_then(|p| crate::config_history::diff_at(&p, arg["id"].as_str().unwrap_or(""), arg["against_now"] == true));
            let _ = reply.send(answer.unwrap_or_else(|e| json!({ "error": e })));
        }
        // Put config.toml back as it was before a change (kept as a version too), and reload.
        "config_restore" => {
            let answer = match crate::config::path().ok_or("no home directory".to_string()).and_then(|p| crate::acting::run(&who.user, || crate::config_history::restore_at(&p, arg["id"].as_str().unwrap_or(""), &who.user))) {
                Ok(e) => {
                    reload_all(convs, hub);
                    convs[0].app.log(Level::Agent, format!("config.toml {} by {} (from the app)", e.how, who.name));
                    everyone = true;
                    json!({ "ok": true, "text": format!("config.toml {}, and reloaded", e.how) })
                }
                Err(e) => json!({ "ok": false, "error": e }),
            };
            let _ = reply.send(answer);
        }
        // Settings → Check config: does config.toml read, and does each model
        // server answer with its key (off this loop: it asks each server).
        "config_check" => {
            crate::acting::spawn(move || {
                let _ = reply.send(crate::startup::config_check());
            });
        }
        // Settings → Reload config: config.toml read again by every
        // conversation (a file that doesn't read changes nothing).
        "config_reload" => {
            let answer = match crate::config::Config::load() {
                Err(e) => json!({ "ok": false, "error": format!("config.toml doesn't read, so nothing changed: {e}") }),
                Ok(_) => {
                    // Edited by hand since lyra last wrote it: that's a version in History.
                    if let Some(p) = crate::config::path() {
                        crate::config_history::notice_hand_edit_at(&p);
                    }
                    reload_all(convs, hub);
                    convs[0].app.log(Level::Agent, format!("config reloaded from the app by {}", who.name));
                    crate::status::request();
                    everyone = true;
                    json!({ "ok": true, "text": format!("config reloaded: chat model {} at {}; the Status page checks everything again now", convs[0].app.model, convs[0].app.base_url) })
                }
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
            // Its browser in the sandbox, if it has one open (Stop ends the reply, and the browser with it).
            "browser": crate::sandbox::sessions().into_iter().find(|s| s["session"].as_str() == Some(c.app.session_id.as_str())),
        }));
    }
    json!(out)
}

/// config.toml read again: lyra's settings, then every conversation's model,
/// prices and Status settings, and the speech servers devices use.
fn reload_all(convs: &mut [Conv], hub: &Hub) {
    convs[0].app.reload();
    let (url, model, pricing, status) = (convs[0].app.base_url.clone(), convs[0].app.model.clone(), convs[0].app.pricing.clone(), convs[0].app.status.clone());
    for c in convs.iter_mut() {
        c.app.base_url = url.clone();
        c.app.model = model.clone();
        c.app.pricing = pricing.clone();
        c.app.status = status.clone();
        c.changed = true;
    }
    if let Ok(config) = crate::config::Config::load() {
        hub.set_speech(config.speech.web());
    }
}
