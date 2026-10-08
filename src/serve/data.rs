//! The app's pages: what each asks for (`get`), answered from the conversation's lyra.


use super::*;

/// The lists the web app's pages show.
/// Conversations loaded now: (session, answering).
pub(crate) type Loaded = Vec<(String, bool)>;

pub(crate) fn data(app: &mut App, hub: &Hub, what: &str, arg: &Value, node_build: Option<&str>, loaded: &Loaded) -> Value {
    let text = |r: Result<String, String>| json!({ "text": r.unwrap_or_else(|e| e) });
    let page = |r: Result<Value, String>| r.unwrap_or_else(|e| json!({ "error": e }));
    match what {
        "sessions" => {
            let Some(dir) = crate::sessions::dir() else { return json!([]) };
            let all = crate::sessions::list_for(&dir, &app.owner);
            let metas = crate::sessions::metas(&dir);
            let folders = crate::sessions::folders(&dir, &app.owner);
            // A folder for a few new ones (the decision model, once each, in the background).
            if !folders.is_empty() && crate::decide::model().is_some() {
                let todo: Vec<String> = all.iter().filter(|s| s.id != app.session_id && s.updated > chrono::Utc::now() - chrono::Duration::days(14) && metas.get(&s.id).is_none_or(|m| !m.looked && m.folder.is_none() && !m.archived)).take(3).map(|s| s.id.clone()).collect();
                if !todo.is_empty() {
                    let (dir, owner) = (dir.clone(), app.owner.clone());
                    std::thread::spawn(move || {
                        crate::acting::set(&owner);
                        for id in todo {
                            let Ok(s) = crate::sessions::find_for(&dir, &id, &owner) else { continue };
                            let pick = crate::sessions::suggest_folder(&s, &folders);
                            let _ = crate::sessions::set_meta(&dir, &id, &owner, |m| {
                                m.looked = true;
                                m.suggested = pick;
                            });
                        }
                    });
                }
            }
            // The latest 300, and every pinned or filed one however old.
            let keep = |s: &crate::sessions::Session| metas.get(&s.id).is_some_and(|m| m.pinned || m.folder.is_some());
            json!(all.iter().enumerate().filter(|(i, s)| *i < 300 || keep(s)).map(|(_, s)| {
                let m = metas.get(&s.id).cloned().unwrap_or_default();
                json!({
                    "id": s.id, "title": s.title, "turns": s.user_turns(), "updated": s.updated, "current": s.id == app.session_id,
                    "open": loaded.iter().any(|(id, _)| *id == s.id), "answering": loaded.iter().any(|(id, w)| *id == s.id && *w),
                    "pinned": m.pinned, "archived": m.archived, "folder": m.folder, "suggested": m.suggested,
                })
            }).collect::<Vec<_>>())
        }
        "search" => {
            let query = arg["query"].as_str().unwrap_or("").trim();
            let all = crate::sessions::dir().map(|d| crate::sessions::list_for(&d, &app.owner)).unwrap_or_default();
            json!(crate::sessions::search(&all, query, 30).iter().map(|h| json!({
                "id": h.id, "title": h.title, "updated": h.updated, "role": h.role, "snippet": h.snippet, "score": h.score,
                "current": h.id == app.session_id,
            })).collect::<Vec<_>>())
        }
        "routines" => crate::acting::run(&app.owner, || crate::routines::view(&[], 10)),
        "briefing" => crate::briefing::last_for(&app.owner).map_or(Value::Null, |b| json!(b)),
        "recap" => crate::recap::last_for(&app.owner).map_or(Value::Null, |r| json!(r)),
        "changelog" => json!({ "version": crate::changelog::version(), "releases": crate::changelog::all() }),
        // Feedback: everyone sends and follows their own; admins see and move everyone's.
        // Q&A: everyone reads it; admins add, change, remove and promote questions into it.
        "qa" | "qa_put" | "qa_remove" | "qa_promote" => {
            let who = crate::feedback::Who { user: app.owner.clone(), name: hub.users().get(&app.owner).map_or_else(|| "the owner".to_string(), |u| u.name), admin: app.admin };
            let id = arg["id"].as_u64().unwrap_or(0);
            let s = |k: &str| arg[k].as_str().unwrap_or("").to_string();
            let done = match what {
                "qa_put" => crate::qa::put(&who, id, &s("question"), &s("answer"), None, None).map(|e| json!({ "ok": true, "id": e.id })),
                "qa_remove" => crate::qa::remove(&who, id).map(|_| json!({ "ok": true })),
                "qa_promote" => {
                    // A Feedback question, with the admin's approved answer.
                    let asked = crate::feedback::list(&who).into_iter().find(|i| i.id == id && i.kind == "question");
                    match asked {
                        None => Err(format!("no question #{id}")),
                        Some(q) => crate::qa::put(&who, 0, &s("question"), &s("answer"), Some(id), Some(q.name.clone()))
                            .and_then(|e| crate::feedback::answered(&who, id, &s("answer"), e.id).map(|_| json!({ "ok": true, "qa": e.id }))),
                    }
                }
                _ => Ok(json!({ "admin": who.admin, "entries": crate::qa::all() })),
            };
            done.unwrap_or_else(|e| json!({ "error": e }))
        }
        "feedback" | "feedback_submit" | "feedback_comment" | "feedback_update" | "feedback_seen" | "feedback_analyze" => {
            let who = crate::feedback::Who { user: app.owner.clone(), name: hub.users().get(&app.owner).map_or_else(|| "the owner".to_string(), |u| u.name), admin: app.admin };
            let id = arg["id"].as_u64().unwrap_or(0);
            let s = |k: &str| arg[k].as_str().map(str::to_string);
            let done = match what {
                "feedback_submit" => {
                    // Their own uploads only.
                    let ups: Vec<_> = arg["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).filter_map(|id| hub.upload(id)).filter(|(up, _)| up.user.as_deref().unwrap_or(lyra_web::users::OWNER) == who.user).collect();
                    let files = ups.iter().map(|(up, _)| crate::feedback::File { id: up.id.clone(), name: up.name.clone(), mime: up.mime.clone() }).collect();
                    let paths = ups.into_iter().map(|(up, path)| (up.name, path)).collect();
                    crate::feedback::submit(&who, &s("kind").unwrap_or_default(), &s("title").unwrap_or_default(), &s("details").unwrap_or_default(), s("severity").as_deref(), files, &s("page").unwrap_or_default()).map(|i| {
                        // lyra's read of it, in the background.
                        crate::feedback::analyze(i.id, &who.user, paths);
                        json!({ "ok": true, "id": i.id })
                    })
                }
                // Admins: read it again (after replies added detail).
                "feedback_analyze" if who.admin => {
                    let found = crate::feedback::list(&who).into_iter().find(|i| i.id == id);
                    match found {
                        Some(i) => {
                            let paths = i.files.iter().filter_map(|f| hub.upload(&f.id).map(|(up, path)| (up.name, path))).collect();
                            crate::feedback::analyze(id, &i.user, paths);
                            Ok(json!({ "ok": true }))
                        }
                        None => Err(format!("no feedback #{id}")),
                    }
                }
                "feedback_comment" => crate::feedback::comment(&who, id, &s("text").unwrap_or_default()).map(|_| json!({ "ok": true })),
                "feedback_update" => crate::feedback::update(&who, id, s("status").as_deref(), s("priority").as_deref(), s("shipped_in").as_deref()).map(|_| json!({ "ok": true })),
                "feedback_seen" => crate::feedback::seen(&who, id).map(|_| json!({ "ok": true })),
                _ => Ok(json!({
                    "admin": who.admin, "me": who.user, "version": crate::changelog::version(),
                    "items": crate::feedback::list(&who).iter().map(|i| crate::feedback::view(i, &who)).collect::<Vec<_>>(),
                })),
            };
            done.unwrap_or_else(|e| json!({ "error": e }))
        }
        "watches" => json!(crate::watches::list_for(&app.owner)),
        // AI usage: everyone's and each person's for admins, their own for members.
        // The current plan's recovery points (admins; members have no plans).
        "plan_checkpoints" if app.admin => match (app.current_plan.as_ref(), app.engine.as_ref()) {
            (Some(p), Some(engine)) => match engine.checkpoints(p.id) {
                Ok(list) => json!(list.iter().rev().map(|c| json!({
                    "at": c.created_at, "version": c.plan_version, "reason": c.reason,
                    "done": c.completed_steps.iter().filter_map(|id| p.step(*id)).map(|s| s.key.clone()).collect::<Vec<_>>(),
                })).collect::<Vec<_>>()),
                Err(e) => json!({ "error": e }),
            },
            _ => json!([]),
        },
        "usage" => {
            let days = arg["days"].as_i64().unwrap_or(7);
            let mut v = crate::usage::summary(days, (!app.admin).then_some(app.owner.as_str()));
            let users = hub.users().list();
            if let Some(list) = v["users"].as_array_mut() {
                for u in list {
                    let id = u["user"].as_str().unwrap_or("").to_string();
                    u["name"] = json!(users.iter().find(|x| x.id == id).map_or(id, |x| x.name.clone()));
                }
            }
            v
        }
        // Their notes and lists.
        "notes" => crate::acting::run(&app.owner, crate::notes::page),
        // Their inbox at a glance (the last day).
        "mail" => match crate::mail::connected_for(&app.owner) {
            false => json!({ "connected": false }),
            true => crate::acting::run(&app.owner, || crate::mail::glance(chrono::Utc::now() - chrono::Duration::days(1))).map_or_else(|e| json!({ "connected": true, "error": e }), |mut v| {
                v["connected"] = json!(true);
                v
            }),
        },
        // Today's calendar (theirs), or how to connect it.
        "calendar" => {
            if !crate::calendar::available() {
                json!({ "available": false })
            } else if !crate::calendar::connected_for(&app.owner) {
                json!({ "available": true, "connected": false })
            } else {
                match crate::acting::run(&app.owner, crate::calendar::today) {
                    Ok(mut v) => {
                        v["available"] = json!(true);
                        v["connected"] = json!(true);
                        v["mail"] = json!(crate::mail::connected_for(&app.owner));
                        v["teams"] = json!(crate::teams::connected_for(&app.owner));
                        v["meetings"] = json!(crate::meetings::ready(&app.owner));
                        v["meetings_available"] = json!(crate::calendar::meetings_enabled());
                        v
                    }
                    Err(e) => json!({ "available": true, "connected": true, "error": e }),
                }
            }
        }
        "pmi" => crate::pmi::as_user(&app.owner, crate::pmi::snapshot).map_or_else(|e| json!({ "error": e }), |s| json!(s)),
        "coding" => json!(crate::coding::jobs()),
        // The people who use lyra (admins: the gate is in the loop).
        "users" => {
            let devices = hub.devices().list();
            json!(hub.users().list().iter().map(|u| json!({
                "id": u.id, "name": u.name, "email": u.email, "role": u.role, "status": u.status,
                "created": u.created, "last_seen": u.last_seen, "microsoft": !u.oid.is_empty(),
                "devices": devices.iter().filter(|d| d.user.as_deref() == Some(u.id.as_str())).map(|d| json!({ "name": d.name, "last_seen": d.last_seen })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>())
        }
        "status" => {
            let mut v = crate::status::latest().map_or(Value::Null, |b| json!(b));
            // A member's view leaves the machines out.
            if !app.admin
                && let Some(rows) = v["rows"].as_array()
            {
                v["rows"] = json!(rows.iter().filter(|r| r["group"] != "Machines").cloned().collect::<Vec<_>>());
            }
            v
        }
        "devices" => {
            let online: Vec<String> = hub.online_devices().into_iter().map(|(id, _)| id).collect();
            let machines: Vec<String> = hub.machines().into_iter().map(|m| m.name.to_lowercase()).collect();
            json!(hub.devices().list().into_iter().map(|d| json!({
                "id": d.id, "name": d.name, "kind": d.kind,
                "online": if d.kind == "node" { machines.contains(&d.name.to_lowercase()) } else { online.contains(&d.id) },
                "created": d.created, "last_seen": d.last_seen, "push": d.push.is_some(),
            })).collect::<Vec<_>>())
        }
        "machines" => json!(machines_detail(hub, node_build)),
        "activity" => json!(app.activity.iter().rev().take(200).map(|a| json!({ "time": a.time, "level": level_name(a.level), "text": a.text })).collect::<Vec<_>>()),
        "agents" => text(app.agents.as_deref().map(crate::agents::list).ok_or("agents are off".into())),
        "goals" => page(app.goals.clone().ok_or("goals are off ([goals] enabled)".to_string()).and_then(|g| crate::goals::page(&g))),
        "skills" => page(app.learning.clone().ok_or("learning is off".to_string()).and_then(|l| l.page_for(app.personal().as_deref(), app.admin))),
        "memory" => {
            let mine = app.personal().map(|u| format!("user:{u}"));
            page(app.mem().ok_or("memory is off".to_string()).and_then(|m| m.page(arg["query"].as_str().unwrap_or(""), arg["scope"].as_str().unwrap_or(""), mine.as_deref())))
        }
        "rules" => json!({
            "system": app.caps.as_ref().and_then(|c| c.system.as_ref()).map(|s| s.settings()).unwrap_or_default(),
            "path": crate::config::path().map(|p| crate::context::show(&p)),
        }),
        "about" => json!({
            "backups": crate::config::home().map(|home| {
                let dir = app.backup.dir(&home);
                let all = crate::backup::list(&dir);
                json!({
                    "dir": crate::context::show(&dir), "enabled": app.backup.enabled, "at": app.backup.at, "keep": app.backup.keep,
                    "count": all.len(), "last": all.first().map(|b| json!({ "name": b.name, "made": b.made.to_rfc3339(), "size": b.size })),
                })
            }),
            "lyra": env!("CARGO_PKG_VERSION"),
            "app": hub.app_version(),
            "node_build": node_build.map(|b| b.chars().take(12).collect::<String>()),
            "model": app.model,
            "decide": crate::decide::model(),
            "session": app.session_id,
            "devices": hub.devices().list().len(),
        })
        .as_object()
        .map(|m| {
            // A member sees what lyra is, not the server's backups or devices.
            let keep = |k: &str| app.admin || !matches!(k, "backups" | "devices" | "node_build");
            json!(m.iter().filter(|(k, _)| keep(k)).map(|(k, v)| (k.clone(), v.clone())).collect::<serde_json::Map<_, _>>())
        })
        .unwrap_or_default(),
        other => json!({ "error": format!("nothing called {other:?}") }),
    }
}
