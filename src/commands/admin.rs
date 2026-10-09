//! The server's own commands: machines, users, usage, who am I, devices.


use crate::*;
use crate::serve::*;

impl App {
    /// `/machines [update|remove <name|all>]` (only with `lyra serve`).
    pub(crate) fn machines_command(&mut self, arg: &str) -> Result<String, String> {
        let hub = self.hub.clone().ok_or("machines connect to `lyra serve`; this lyra isn't serving")?;
        let node_build = hub.node_build();
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        let all = machines_detail(&hub, node_build.as_deref());
        match sub {
            "" | "list" => {
                let here = coding_agents(&json!({ "harnesses": lyra_node::coding::available() }));
                let server = format!("● server — where lyra runs{}", if here.is_empty() { " · coding: none installed".to_string() } else { here });
                if all.is_empty() {
                    return Ok(format!("{server}\n\nno machines yet. On a machine: curl -fsSL <lyra url>/install.sh | sh  (or lyra-node pair <url>)"));
                }
                Ok(server + "\n" + &all
                    .iter()
                    .map(|m| {
                        format!(
                            "{} {} — {}{}{}{}",
                            if m["online"] == true { "●" } else { "○" },
                            m["name"].as_str().unwrap_or(""),
                            if m["online"] == true { "online" } else { "offline" },
                            m["hostname"].as_str().map_or(String::new(), |h| format!(" · {h}")),
                            m["version"].as_str().map_or(String::new(), |v| if m["self_update"] == true { format!(" · lyra-node {v} ({})", m["build"].as_str().unwrap_or("")) } else { format!(" · built into lyra {v}") }),
                            if m["update_available"] == true { " · update available (/machines update)" } else { "" },
                        ) + &coding_agents(m) + &match m["health"].as_object() {
                            Some(h) => {
                                let problems: Vec<&str> = h.get("problems").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                                format!("\n    {}{}", h.get("summary").and_then(Value::as_str).unwrap_or(""), if problems.is_empty() { String::new() } else { format!(" · ⚠ {}", problems.join("; ")) })
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n\n/machines health [name] · /machines update <name|all> · /machines remove <name> · add one: curl -fsSL <lyra url>/install.sh | sh (Linux) · irm <lyra url>/install.ps1 | iex (Windows, as administrator)")
            }
            "update" | "remove" => {
                if rest.is_empty() {
                    return Err(format!("usage: /machines {sub} <name{}>", if sub == "update" { "|all" } else { "" }));
                }
                let targets: Vec<String> = all
                    .iter()
                    .filter(|m| (rest == "all" && sub == "update" && m["online"] == true) || m["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(rest)))
                    .filter_map(|m| m["name"].as_str().map(str::to_string))
                    .collect();
                if targets.is_empty() {
                    return Err(format!("no machine {rest:?} (/machines lists them)"));
                }
                if sub == "update" && node_build.is_none() && hub.node_build_windows().is_none() {
                    return Err("this server has no lyra-node to hand out (build it next to lyra: see the README)".into());
                }
                let tx = self.tx.clone();
                for name in targets.clone() {
                    let (hub, tx) = (hub.clone(), tx.clone());
                    let sub = sub.to_string();
                    std::thread::spawn(move || {
                        let request = json!({ "type": if sub == "update" { "update" } else { "uninstall" } });
                        let result = hub.call_machine(&name, request, Duration::from_secs(180));
                        let note = match (sub.as_str(), result) {
                            ("update", Ok(v)) if v["updated"] == true => format!("✓ {name} updated ({} → {}); it restarted into the new version", v["from"].as_str().unwrap_or("?"), v["to"].as_str().unwrap_or("?")),
                            ("update", Ok(v)) => format!("{name}: {}", v["why"].as_str().unwrap_or("nothing to update")),
                            ("update", Err(e)) => format!("✗ {name} wasn't updated: {e}"),
                            (_, result) => {
                                let removed = hub.devices().remove(&name).is_ok();
                                match result {
                                    Ok(v) => {
                                        let files: Vec<String> = v["removed"]
                                            .as_array()
                                            .into_iter()
                                            .flatten()
                                            .filter_map(|x| x.as_str())
                                            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                                            .collect();
                                        format!(
                                            "✓ {name} removed: lyra-node uninstalled itself{}{}",
                                            if files.is_empty() { String::new() } else { format!(" ({})", files.join(", ")) },
                                            if removed { "" } else { "; its pairing was already gone" }
                                        )
                                    }
                                    Err(e) => format!("{name} unpaired{} — it couldn't uninstall itself ({e}); its files stay on that machine until removed there", if removed { "" } else { " (already)" }),
                                }
                            }
                        };
                        let _ = tx.send(crate::StreamEvent::Notice(note));
                    });
                }
                Ok(format!("{} {}…", if sub == "update" { "updating" } else { "removing" }, targets.join(", ")))
            }
            // Disks, memory, load, failed units, updates (from its last report).
            "health" => {
                if rest.is_empty() || rest.eq_ignore_ascii_case("server") {
                    let mut out = vec![crate::health::describe("server", &lyra_node::health::report())];
                    out.extend(crate::diagnose::lines_for("server"));
                    if rest.is_empty() {
                        for m in hub.machines() {
                            out.push(match &m.health {
                                Some(h) => crate::health::describe(&m.name, h),
                                None => format!("{}: no report yet", m.name),
                            });
                            out.extend(crate::diagnose::lines_for(&m.name));
                        }
                    }
                    return Ok(out.join("\n\n"));
                }
                let m = hub.machines().into_iter().find(|m| m.name.eq_ignore_ascii_case(rest)).ok_or_else(|| format!("{rest} isn't connected (/machines lists them)"))?;
                let mut text = m.health.as_ref().map_or(format!("{}: no report yet (it sends one every 5 minutes)", m.name), |h| crate::health::describe(&m.name, h));
                for line in crate::diagnose::lines_for(&m.name) {
                    text += &format!("\n{line}");
                }
                Ok(text)
            }
            // What runs without asking there; changed like the app's Rules dialog.
            "rules" => {
                let (name, change) = rest.split_once(' ').map_or((rest, ""), |(n, c)| (n, c.trim()));
                if name.is_empty() {
                    return Err(format!("usage: {RULES_USAGE}"));
                }
                if name.eq_ignore_ascii_case("server") {
                    let system = self.caps.as_ref().and_then(|c| c.system.as_ref()).ok_or("system access isn't set up")?;
                    let mut rules = system.settings();
                    if change.is_empty() {
                        return Ok(rules_text("server", &rules));
                    }
                    edit_rules(&mut rules, change)?;
                    let answer = set_server_rules(self, &json!(rules)).map_err(|e| format!("not changed: {e}"))?;
                    let rules: lyra_system::Settings = serde_json::from_value(answer["system"].clone()).map_err(|e| e.to_string())?;
                    return Ok(format!("saved to config.toml, in effect now\n{}", rules_text("server", &rules)));
                }
                let Some(name) = all.iter().filter_map(|m| m["name"].as_str()).find(|n| n.eq_ignore_ascii_case(name)).map(str::to_string) else {
                    return Err(format!("no machine {name:?} (/machines lists them)"));
                };
                // Check the change before asking the machine, so a typo fails here.
                if !change.is_empty() {
                    edit_rules(&mut lyra_system::Settings::default(), change)?;
                }
                let (tx, change) = (self.tx.clone(), change.to_string());
                std::thread::spawn(move || {
                    let ask = |set: Option<&lyra_system::Settings>| {
                        let mut request = json!({ "type": "rules" });
                        if let Some(set) = set {
                            request["set"] = json!(set);
                        }
                        hub.call_machine(&name, request, Duration::from_secs(20))
                            .and_then(|v| v["system"].is_object().then(|| v.clone()).ok_or_else(|| v["error"].as_str().unwrap_or("no rules in the answer").to_string()))
                            .and_then(|v| serde_json::from_value::<lyra_system::Settings>(v["system"].clone()).map(|r| (r, v["path"].as_str().unwrap_or("").to_string())).map_err(|e| e.to_string()))
                    };
                    let note = match ask(None) {
                        Err(e) => format!("✗ {name}'s rules: {e}"),
                        Ok((rules, _)) if change.is_empty() => rules_text(&name, &rules),
                        Ok((mut rules, _)) => match edit_rules(&mut rules, &change).and_then(|()| ask(Some(&rules))) {
                            Ok((rules, path)) => format!("✓ {name} saved its rules ({path}), in effect now\n{}", rules_text(&name, &rules)),
                            Err(e) => format!("✗ {name}'s rules weren't changed: {e}"),
                        },
                    };
                    let _ = tx.send(crate::StreamEvent::Notice(note));
                });
                Ok(format!("asking {}…", rest.split_whitespace().next().unwrap_or("")))
            }
            _ => Err(format!("usage: /machines [update <name|all> | remove <name> | health [name|server]] · {RULES_USAGE}")),
        }
    }

    /// `/users [approve|decline|admin|member|disable|enable <who>]`: the
    /// people who use lyra (only with `lyra serve`, admins only).
    pub(crate) fn users_command(&mut self, arg: &str) -> Result<String, String> {
        use lyra_web::{Role, Status};
        let hub = self.hub.clone().ok_or("users sign in to `lyra serve`; this lyra isn't serving")?;
        let users = hub.users();
        let (sub, rest) = arg.trim().split_once(' ').map_or((arg.trim(), ""), |(a, b)| (a, b.trim()));
        let change = |role: Option<Role>, status: Option<Status>, what: &str| -> Result<String, String> {
            if rest.is_empty() {
                return Err(format!("usage: /users {sub} <name or email>"));
            }
            let u = users.update(rest, role, status)?;
            Ok(format!("{} ({}) {what}", u.name, if u.email.is_empty() { u.id.clone() } else { u.email.clone() }))
        };
        match sub {
            "" | "list" => {
                let devices = hub.devices().list();
                let mut out: Vec<String> = users
                    .list()
                    .iter()
                    .map(|u| {
                        let n = devices.iter().filter(|d| d.user.as_deref() == Some(u.id.as_str())).count();
                        let status = match u.status {
                            Status::Active => "",
                            Status::Pending => " · ⏳ waiting to be let in (/users approve)",
                            Status::Disabled => " · disabled",
                        };
                        format!(
                            "{} {}{} · {} · {n} device{}{}{status}",
                            if u.role == Role::Admin { "★" } else { "·" },
                            u.name,
                            if u.email.is_empty() { if u.username.is_empty() { String::new() } else { format!(" (signs in as {})", u.username) } } else { format!(" <{}>", u.email) },
                            if u.role == Role::Admin { "admin" } else { "member" },
                            if n == 1 { "" } else { "s" },
                            u.tool_rounds.map_or(String::new(), |r| format!(" · {r} tool rounds"))
                        )
                    })
                    .collect();
                out.push("/users approve|decline|admin|member|disable|enable <name or email> · /users add <username> <name> [admin] · /users password <who> [username] · /users rounds <who> <1–64 | default>".into());
                Ok(out.join("\n"))
            }
            "approve" | "enable" => change(None, Some(Status::Active), "can use lyra"),
            "decline" | "disable" => change(None, Some(Status::Disabled), "can't use lyra (their devices stop working)"),
            "admin" => change(Some(Role::Admin), None, "is an admin"),
            "member" => change(Some(Role::Member), None, "is a member"),
            // An account without Microsoft: /users add <username> <name> [admin]; its one-time password, shown once.
            "add" => {
                let mut words: Vec<&str> = rest.split_whitespace().collect();
                let admin = words.last().is_some_and(|w| w.eq_ignore_ascii_case("admin"));
                if admin {
                    words.pop();
                }
                let (Some(username), name) = (words.first().copied(), words.get(1..).map(|w| w.join(" ")).unwrap_or_default()) else {
                    return Err("usage: /users add <username> <their name> [admin]".into());
                };
                let (u, temp) = users.create_local(username, &name, if admin { Role::Admin } else { Role::Member })?;
                self.log(Level::Agent, format!("account made for {} (signs in as {})", u.name, u.username));
                Ok(format!("{} can sign in as {} with the one-time password {temp}\n(shown only now: they choose their own at the first sign-in)", u.name, u.username))
            }
            // A new one-time password: /users password <who> [username] (a username for someone who has none).
            "password" => {
                let mut words = rest.split_whitespace();
                let who = words.next().ok_or("usage: /users password <name, email or username> [new username]")?;
                let (u, temp) = users.reset_password(who, words.next())?;
                self.log(Level::Agent, format!("password reset for {} ({})", u.name, u.username));
                Ok(format!("{} can sign in as {} with the one-time password {temp}\n(shown only now: they choose their own at the next sign-in)", u.name, u.username))
            }
            // Their own tool-call limit: a number, or "default" for the shared one.
            "rounds" => {
                let (who, n) = rest.rsplit_once(' ').map(|(a, b)| (a.trim(), b.trim())).ok_or("usage: /users rounds <name or email> <1–64 | default>")?;
                let rounds = match n {
                    "default" | "off" | "-" => None,
                    n => Some(n.parse::<u32>().ok().filter(|n| (1..=crate::limits::MAX_TOOL_ROUNDS).contains(n)).ok_or(format!("rounds: a number from 1 to {}, or default", crate::limits::MAX_TOOL_ROUNDS))?),
                };
                let u = users.set_tool_rounds(who, rounds)?;
                let shared = crate::limits::default_tool_rounds(self.evolution.as_ref().map(|e| e.behavior().max_tool_rounds));
                self.log(Level::Agent, format!("tool-call limit for {}: {}", u.name, rounds.map_or(format!("the default ({shared})"), |n| n.to_string())));
                Ok(match rounds {
                    Some(n) => format!("{}'s replies may take {n} rounds of tool calls (the default is {shared})", u.name),
                    None => format!("{} uses the default limit ({shared} rounds of tool calls)", u.name),
                })
            }
            _ => Err("usage: /users [approve|decline|admin|member|disable|enable <name or email>] · /users add <username> <name> [admin] · /users password <who> [username] · /users rounds <who> <1–64 | default>".into()),
        }
    }

    /// `/settings [<key> <value>]`: the common settings; a change reloads lyra.
    pub(crate) fn settings_command(&mut self, arg: &str) -> Result<String, String> {
        let text = crate::settings::command(arg)?;
        if !arg.trim().is_empty() && !text.contains(" is already ") {
            self.reload();
            self.log(Level::Agent, format!("settings changed: {}", arg.trim()));
        }
        Ok(text)
    }

    /// `/usage [days]`: everyone's and each person's for admins, one's own for members.
    pub(crate) fn usage_command(&mut self, arg: &str) -> Result<String, String> {
        let days = arg.trim().parse::<i64>().unwrap_or(7);
        let users = self.hub.as_ref().map(|h| h.users().list()).unwrap_or_default();
        let name = |id: &str| users.iter().find(|u| u.id == id).map_or_else(|| id.to_string(), |u| u.name.clone());
        let only = (!self.admin).then(|| self.owner.clone());
        Ok(crate::usage::describe(days, only.as_deref(), &name))
    }

    /// `/whoami`: who this conversation belongs to.
    pub(crate) fn whoami(&self) -> String {
        let name = self.hub.as_ref().and_then(|h| h.users().get(&self.owner)).map_or_else(|| self.owner.clone(), |u| if u.email.is_empty() { u.name } else { format!("{} <{}>", u.name, u.email) });
        format!("{name} · {}", if self.admin { "admin" } else { "member" })
    }

    /// `/devices [approve|deny <code> | remove <name|id>]` (only with `lyra serve`).
    pub(crate) fn devices_command(&mut self, arg: &str) -> Result<String, String> {
        let hub = self.hub.clone().ok_or("devices pair with `lyra serve`; this lyra isn't serving")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        match sub {
            "" | "list" => {
                let online: Vec<String> = hub.online_devices().into_iter().map(|(id, _)| id).collect();
                let machines: Vec<String> = hub.machines().into_iter().map(|m| m.name.to_lowercase()).collect();
                let mut out: Vec<String> = hub
                    .devices()
                    .list()
                    .into_iter()
                    .map(|d| {
                        let on = if d.kind == "node" { machines.contains(&d.name.to_lowercase()) } else { online.contains(&d.id) };
                        format!(
                            "{} {} ({}) — {} · last seen {}{}",
                            if on { "●" } else { "○" },
                            d.name,
                            if d.kind == "node" { "machine" } else { "device" },
                            if on { "online" } else { "offline" },
                            d.last_seen.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                            if d.push.is_some() { " · notifications on" } else { "" }
                        )
                    })
                    .collect();
                for p in hub.pair_requests() {
                    out.push(format!("? {} ({}, {}) asks to pair — code {}: /devices approve {}{} · /devices deny {}", p.name, p.hostname, p.kind, p.code, p.code, if p.kind == "node" { "" } else { " for <whose>" }, p.code));
                }
                if out.is_empty() {
                    out.push("no paired devices".into());
                }
                out.push(String::new());
                out.push("pair with a code: `lyra pair` on the server · headless: lyra-node pair <url> (then approve here)".into());
                Ok(out.join("\n"))
            }
            // "/devices approve CODE for dana@…": a terminal is someone's; a machine isn't.
            "approve" | "deny" => {
                let (code, user) = rest.split_once(" for ").map_or((rest, None), |(c, u)| (c.trim(), Some(u.trim())));
                hub.answer_pair(code, sub == "approve", user)
            }
            "remove" => {
                let d = hub.devices().remove(rest)?;
                Ok(format!("removed {} ({}); it has to pair again{}", d.name, d.id, if d.kind == "node" { " — to uninstall lyra-node too, use /machines remove while it's online" } else { "" }))
            }
            _ => Err("usage: /devices [approve <code> [for <name or email>] | deny <code> | remove <name|id>]".into()),
        }
    }
}
