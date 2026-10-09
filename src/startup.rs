//! Starting lyra: the command line (`lyra`, `serve`, `pair`, `backup` …), opening
//! each subsystem from the config, and re-reading the config (`/reload`).


use crate::*;

impl App {
    /// Re-read the context files and config.toml; takes effect on the next request.
    pub(crate) fn reload(&mut self) {
        let context = Context::load();
        self.system_prompt = system_prompt(&context, self.tools.is_some());
        self.context_files = context.files();
        self.log(Level::Info, "reloaded config and context files".into());
        self.log_context();
        match Config::load() {
            Ok(config) => {
                self.base_url = config.url.clone();
                self.model = config.model.clone();
                configure(&config);
                self.pricing = pricing(&config);
                self.status = config.status.clone();
                if let Some(mem) = self.mem() {
                    for note in mem.reconfigure(config.memory.settings.clone(), config.embedding.clone()) {
                        self.log(Level::Memory, note);
                    }
                    mem.set_project(config.memory.project());
                    mem.set_vector_index_threshold(config.memory.vector_index_threshold);
                }
                self.memory_curate_every = config.memory.curate.every_days();
                if let Some(caps) = &self.caps {
                    caps.manager.set_settings(config.capabilities.settings.clone());
                }
                if let Some(agents) = &self.agents {
                    *agents.settings.lock().unwrap_or_else(|e| e.into_inner()) = config.agents.clone();
                }
                self.show_handled_by = config.agents.show_handled_by;
                self.curate_every = config.learning.curate.every_days();
                self.evolution_review_every = config.evolution.review.every_days();
                self.embedding = config.embedding;
                self.reranker = config.reranker;
            }
            // Keep the current settings rather than dropping to defaults.
            Err(e) => {
                let e = format!("config not reloaded: {e}");
                self.log(Level::Error, e.clone());
                self.messages.push(Message::new("error", e));
            }
        }
        self.scroll = None;
        self.reload_evolution();
        self.sync_agents();
        self.refresh_agents();
        self.refresh_caps(true);
        self.check_models();
        self.refresh_memory();
        self.refresh_skills();
    }
}

/// Hand each module its part of the config. The one place both starting lyra
/// and `/reload` go through, so a setting can't apply on one and not the other.
pub(crate) fn configure(config: &Config) {
    learn::configure(learn::Structured {
        max_tokens: config.structured_max_tokens,
        thinking: config.structured_thinking,
    });
    decide::configure(config.decide.clone());
    health::configure(config.health.clone());
    coding::configure(config.coding.clone());
    briefing::configure(config.briefing.clone());
    pmi::configure(config.pmi.clone());
    planner::configure(config.planner.clone());
    usage::configure(prices(config));
    vision::configure(config.vision_model.clone());
    proactive::configure(config.proactive.clone());
    recap::configure(config.recap.clone());
    feedback::configure(&config.url, &config.model);
}

pub(crate) fn prices(config: &Config) -> usage::Prices {
    usage::Prices {
        input: config.input_cost_per_mtok,
        cached: config.cached_input_cost_per_mtok.unwrap_or(config.input_cost_per_mtok),
        output: config.output_cost_per_mtok,
        currency: config.currency.clone(),
        // Every other model at its own section's prices.
        kinds: [
            ("embedding", config.embedding.as_ref().map(|e| &e.price)),
            ("reranker", config.reranker.as_ref().map(|e| &e.price)),
            ("decision", config.decide.as_ref().map(|d| &d.price)),
            ("vision", config.vision_model.as_ref().map(|v| &v.price)),
        ]
        .into_iter()
        .filter_map(|(k, p)| Some((k.to_string(), p?.clone())))
        .collect(),
    }
}

pub(crate) fn pricing(config: &Config) -> Pricing {
    Pricing {
        input_per_mtok: config.input_cost_per_mtok,
        cached_per_mtok: config.cached_input_cost_per_mtok.unwrap_or(config.input_cost_per_mtok),
        output_per_mtok: config.output_cost_per_mtok,
        currency: config.currency.clone(),
    }
}

pub(crate) const USAGE: &str = "\
lyra — a terminal chat client for a local LLM

usage: lyra [command] [options]

  serve                   run for phones and browsers (no terminal UI; see [web] in the config)
  pair                    a code to pair a phone or browser with `lyra serve`
  devices [remove <name>] paired devices
  service                 install a systemd user service that runs `lyra serve`
  node [pair|service]     let a lyra server work on this machine (see lyra node --help)
  pmi [token]             the PMI connection; `lyra pmi token` saves the owner's access token (read from stdin; everyone else connects their own on the Tasks page)
  secret <service>        save a secret (read from stdin, not echoed): `lyra secret entra` for Microsoft sign-in
  connect [--pair <code>] the terminal UI for a lyra server (see lyra connect --help);
                          plain `lyra` opens it on a machine that has no lyra of its own

  -c, --continue          continue the latest conversation (started in this folder, else any)
  -r, --resume [id]       resume a saved conversation; without an id, list them
  --restore-memory <dir>  put a memory backup in place, then exit
  --force                 start even if another lyra (TUI or serve) is using the same home
  -h, --help              this help

Conversations are saved in ~/.lyra/sessions/ ($LYRA_HOME/sessions).";

/// A secret typed (not shown) or piped in.
pub(crate) fn read_secret(prompt: &str) -> String {
    use std::io::{BufRead, IsTerminal};
    let tty = std::io::stdin().is_terminal();
    let echo = |on: bool| {
        let _ = std::process::Command::new("stty").arg(if on { "echo" } else { "-echo" }).stdin(std::process::Stdio::inherit()).status();
    };
    if tty {
        eprint!("{prompt}, then Enter: ");
        echo(false);
    }
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    if tty {
        echo(true);
        eprintln!();
    }
    line.trim().to_string()
}

/// `lyra secret <service>`: a token or client secret into secrets.toml.
pub(crate) fn secret_cli(args: &[String]) {
    let Some(service) = args.first().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')) else {
        eprintln!("usage: lyra secret <service>   (e.g. entra: the Microsoft sign-in app's client secret)");
        std::process::exit(2);
    };
    let value = read_secret(&format!("The secret for {service}"));
    match secrets::set_token(service, &value) {
        Ok(()) => println!("{} for {service} (readable by this user only, not backed up){}", if value.is_empty() { "removed the secret" } else { "saved the secret" }, if service == "entra" { "; restart lyra serve to use it" } else { "" }),
        Err(e) => {
            eprintln!("lyra secret: {e}");
            std::process::exit(1);
        }
    }
}

/// `lyra pmi [token]`: the token is read from stdin, so it's never in the shell history.
pub(crate) fn pmi_cli(args: &[String]) {
    if let Ok(config) = Config::load() {
        pmi::configure(config.pmi);
    }
    let result = match args.first().map(String::as_str) {
        None | Some("status") => Ok(pmi::describe()),
        Some("token") => {
            let line = read_secret("PMI access token (Your account → Security in PMI)");
            pmi::command(&format!("token {}", line.trim()))
        }
        Some(other) => Err(format!("unknown: lyra pmi {other} (try lyra pmi or lyra pmi token)")),
    };
    match result {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("lyra pmi: {e}");
            std::process::exit(1);
        }
    }
}

pub(crate) fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_none_or(|a| a != "node" && a != "connect") && args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let sub = args.get(1).map(String::as_str);
    match sub {
        Some("--version" | "-V" | "version") => {
            println!("lyra {}", changelog::version());
            return;
        }
        Some("pair") => return pair_command(&args[2..]),
        Some("backup") => return backup_cli(&args[2..]),
        Some("restore") => return restore_cli(&args[2..]),
        Some("devices") => return devices_command(&args[2..]),
        Some("service") => return service_command(),
        Some("node") => return lyra_node::main(&args[2..]),
        Some("connect") => return connect::main(&args[2..]),
        Some("mcp") => return mcp_server::main(&args[2..]),
        Some("pmi") => return pmi_cli(&args[2..]),
        Some("secret") => return secret_cli(&args[2..]),
        // No lyra of its own here, but a paired terminal: open that.
        None if connect::configured() && !config::home().is_some_and(|h| h.join("config").join("config.toml").exists()) => return connect::main(&[]),
        _ => {}
    }
    let serving = sub == Some("serve");
    // -c / --continue: the latest conversation (here); -r / --resume <id>: that one.
    let resume = match args.iter().position(|a| a == "-r" || a == "--resume") {
        Some(i) => {
            let dir = config::home().map(|h| h.join("sessions"));
            match (args.get(i + 1).filter(|a| !a.starts_with('-')), dir) {
                (Some(key), Some(dir)) => match sessions::find_for(&dir, key, lyra_web::users::OWNER) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!("lyra: {e}");
                        std::process::exit(1);
                    }
                },
                (None, dir) => {
                    let all = dir.map(|d| sessions::list_for(&d, lyra_web::users::OWNER)).unwrap_or_default();
                    println!("{}\n\nlyra -r <id> resumes one · lyra -c continues the latest", sessions::describe(&all, 20));
                    return;
                }
                (_, None) => {
                    eprintln!("lyra: no home directory");
                    std::process::exit(1);
                }
            }
        }
        None if args.iter().any(|a| a == "-c" || a == "--continue") => {
            match config::home().map(|h| h.join("sessions")).and_then(|d| sessions::latest(&d)) {
                Some(s) => Some(s),
                None => {
                    eprintln!("lyra: no saved conversation to continue yet");
                    std::process::exit(1);
                }
            }
        }
        None => None,
    };
    if let Some(i) = args.iter().position(|a| a == "--restore-memory") {
        let Some(backup) = args.get(i + 1) else {
            eprintln!("usage: lyra --restore-memory <backup directory>");
            std::process::exit(2);
        };
        match restore_memory(backup) {
            Ok(note) => println!("{note}"),
            Err(e) => {
                eprintln!("lyra: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // Before loading config: an old install's files may need moving into ~/.lyra.
    let migrated = match migrate::run() {
        Ok(notes) => notes,
        Err(e) => {
            eprintln!("lyra: couldn't move files into the lyra home: {e}");
            std::process::exit(1);
        }
    };
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("lyra: bad config: {e}");
            std::process::exit(1);
        }
    };
    // `lyra serve` already runs on this home: talk to it rather than start a second lyra.
    if !serving
        && !args.iter().any(|a| a == "--force")
        && let Some(home) = config::home()
        && let Some((_, mode)) = lock::holder(&home)
        && mode == "lyra serve"
    {
        return connect::local(&home, &config.web.listen);
    }
    // One lyra per home: two would each keep their own copy of the conversation.
    let _lock = match (config::home(), args.iter().any(|a| a == "--force")) {
        (Some(home), false) => match lock::acquire(&home, if serving { "lyra serve" } else { "the terminal UI" }) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("lyra: {e}.");
                if !serving {
                    eprintln!("Use the web app instead, or stop it first (systemctl --user stop lyra).");
                }
                eprintln!("Two at once overwrite each other's conversation; to run anyway: lyra {}--force", if serving { "serve " } else { "" });
                std::process::exit(1);
            }
        },
        _ => None,
    };
    // Memory is async (sqlx); a small runtime lets lyra's threads call into it.
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (tools, memory_status, memory_notes) = open_memory(&config, runtime.handle());
    let (learning, learning_status) = open_learning(&config, runtime.handle());
    let (engine, planning_status) = open_planning(&config, runtime.handle());
    let (evolution, evolution_status) = open_evolution(&config, runtime.handle());
    let (caps, caps_notes) = open_capabilities(&config, runtime.handle(), &tools, &learning, &evolution);
    let goals = open_goals(&config, runtime.handle());
    if let (Some(caps), Some(goals)) = (&caps, &goals) {
        caps.set_goals(goals.clone());
    }
    // Everyone else's goals open from here when first needed.
    if let Some(g) = &goals {
        goals::register(g.clone(), runtime.handle().clone(), config.goals.settings.clone());
    }
    let (agents, agents_status) = open_agents(&config, runtime.handle());
    if let (Some(caps), Some(agents)) = (&caps, &agents) {
        caps.set_agents(agents.clone());
    }
    let services = Services {
        tools,
        memory_status,
        learning,
        learning_status,
        engine,
        planning_status,
        evolution,
        evolution_status,
        caps,
        goals,
        agents,
        agents_status,
    };
    configure(&config);
    stats::learn_context_window(&config.url);
    let web = config.web.clone();
    let mut app = App::new(config, Context::load(), services);
    for note in migrated {
        app.log(Level::Info, note);
    }
    for note in memory_notes {
        app.log(Level::Memory, note);
    }
    for note in caps_notes {
        app.log(Level::Tool, note);
    }
    // The server carries on the latest conversation, so a phone finds it after a restart.
    let resume = resume.or_else(|| serving.then(|| sessions::dir().and_then(|d| sessions::list_for(&d, lyra_web::users::OWNER).into_iter().next())).flatten());
    if let Some(s) = resume {
        app.resume_session(s);
    }
    app.start();
    if serving {
        return serve_main(app, &web, runtime.handle());
    }
    ratatui::run(|terminal| run(terminal, &mut app)).expect("terminal error");
    app.save_session();
}

/// `lyra serve`: no terminal UI; phones and browsers connect over the web.
pub(crate) fn serve_main(mut app: App, web: &lyra_web::Settings, rt: &tokio::runtime::Handle) {
    let Some(dir) = config::home().map(|h| h.join("web")) else {
        eprintln!("lyra: no home directory");
        std::process::exit(1);
    };
    let (tx, rx) = mpsc::channel();
    // Microsoft sign-in's client secret, from the secrets file.
    let mut web = web.clone();
    web.entra.secret = secrets::token("entra");
    // Calendars sign in with the same Microsoft app.
    graph::configure(web.entra.clone());
    let hub = match lyra_web::Hub::start(rt, &web, &dir, tx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    };
    app.hub = Some(hub.clone());
    if let Some(home) = config::home() {
        hub.set_backups(app.backup.dir(&home));
    }
    if let Some(caps) = &app.caps {
        caps.set_remote(Arc::new(serve::HubRemote(hub.clone())));
        // Paired machines show in the tools' choices even before they connect.
        caps.refresh();
    }
    println!("lyra serve · listening on http://{} ({} paired devices)", hub.address, hub.devices().list().len());
    match web.public_url.as_str() {
        "" => println!("set [web] public_url to the https:// address your reverse proxy serves (needed for install and notifications)"),
        url => println!("open {url} on your phone · pair it with `lyra pair`"),
    }
    if !hub.address.ip().is_loopback() && hub.address.ip().is_unspecified() {
        println!("note: listening on every interface; prefer the proxy's address or 127.0.0.1");
    }
    serve::run(app, &hub, rx, web.notify);
}

/// `lyra pair`: a code for pairing a phone or browser (valid 10 minutes).
pub(crate) fn pair_command(args: &[String]) {
    let usage = || -> ! {
        eprintln!("usage: lyra pair [--minutes N] [--user <name or email>]   (without --user the device is the owner's)");
        std::process::exit(2);
    };
    let (mut minutes, mut user) = (10, None::<String>);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" => match it.next().and_then(|n| n.parse::<i64>().ok()) {
                Some(n) if (1..=60).contains(&n) => minutes = n,
                _ => {
                    eprintln!("lyra: --minutes takes 1 to 60");
                    std::process::exit(2);
                }
            },
            "--user" => user = Some(it.next().cloned().unwrap_or_else(|| usage())),
            _ => usage(),
        }
    }
    let dir = config::home().map(|h| h.join("web")).expect("a home directory");
    // Whose device it will be: a coworker's phone is theirs, not the owner's.
    let whose = match &user {
        None => None,
        Some(key) => match lyra_web::Users::open(&dir).find(key).filter(|u| u.status == lyra_web::Status::Active) {
            Some(u) => Some(u.id),
            None => {
                eprintln!("lyra: no active user {key:?} (lyra serve's /users lists them)");
                std::process::exit(1);
            }
        },
    };
    if whose.is_none() {
        eprintln!("This device will be the owner's (an admin). For a coworker, use Microsoft sign-in or lyra pair --user <their email>.");
    }
    match lyra_web::Devices::open(&dir).and_then(|d| d.new_code_for(minutes, whose.as_deref())) {
        Ok(code) => {
            let url = Config::load().map(|c| c.web.public_url).unwrap_or_default();
            let shown = format!("{}-{}", &code[..4], &code[4..]);
            println!("Pairing code: {shown}");
            println!("Valid for {minutes} minute{}, once.", if minutes == 1 { "" } else { "s" });
            if url.is_empty() {
                println!("Open lyra's address on the device and enter it (set [web] public_url for a link and QR code).");
            } else {
                let link = pair_link(&url, &shown);
                println!("Open {link} on the device (or scan this), then tap Pair:\n");
                print!("{}", qr(&link));
            }
        }
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    }
}

/// The app's address with the code filled in.
pub(crate) fn pair_link(public_url: &str, code: &str) -> String {
    format!("{}/?pair={code}", public_url.trim_end_matches('/'))
}

/// A QR code for a terminal: two rows per character, light quiet zone.
pub(crate) fn qr(text: &str) -> String {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => code.render::<qrcode::render::unicode::Dense1x2>().dark_color(qrcode::render::unicode::Dense1x2::Light).light_color(qrcode::render::unicode::Dense1x2::Dark).build() + "\n",
        Err(_) => String::new(),
    }
}

/// `lyra backup [list]`: back up now (lyra stopped; while it runs, `/backup now`).
pub(crate) fn backup_cli(args: &[String]) {
    let (Some(home), Ok(config)) = (config::home(), Config::load()) else {
        eprintln!("lyra: no lyra home or config");
        std::process::exit(1);
    };
    let dir = config.backup.dir(&home);
    if args.first().map(String::as_str) == Some("list") {
        println!("{}", backup::describe(&dir, &config.backup));
        return;
    }
    if let Some((pid, mode)) = lock::holder(&home) {
        eprintln!("lyra: {mode} is running (pid {pid}); back up from it so the copy is consistent: /backup now (or the app's About → Back up now)");
        std::process::exit(1);
    }
    match backup::run(&home, &config.backup, None) {
        Ok((b, notes)) => {
            for n in notes {
                println!("{n}");
            }
            println!("backed up to {} ({})", b.path.display(), backup::size_text(b.size));
        }
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    }
}

/// `lyra restore <file|latest>`: put a backup in place of the lyra home.
pub(crate) fn restore_cli(args: &[String]) {
    let (Some(home), Some(which)) = (config::home(), args.first()) else {
        eprintln!("usage: lyra restore <backup file|latest>   (lyra backup list shows them)");
        std::process::exit(2);
    };
    let settings = Config::load().map(|c| c.backup).unwrap_or_default();
    let archive = if which == "latest" {
        match backup::list(&settings.dir(&home)).into_iter().next() {
            Some(b) => b.path,
            None => {
                eprintln!("lyra: no backups in {}", settings.dir(&home).display());
                std::process::exit(1);
            }
        }
    } else {
        std::path::PathBuf::from(which)
    };
    match backup::restore(&home, &archive) {
        Ok(note) => println!("{note}\nstart lyra again (systemctl start lyra)"),
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    }
}

/// `lyra devices [remove <name|id>]`.
pub(crate) fn devices_command(args: &[String]) {
    let dir = config::home().map(|h| h.join("web")).expect("a home directory");
    let devices = match lyra_web::Devices::open(&dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    };
    match args.first().map(String::as_str) {
        Some("remove") => match args.get(1).map(|k| devices.remove(k)) {
            Some(Ok(d)) => println!("removed {} ({}); it has to pair again", d.name, d.id),
            Some(Err(e)) => {
                eprintln!("lyra: {e}");
                std::process::exit(1);
            }
            None => eprintln!("usage: lyra devices remove <name|id>"),
        },
        _ => {
            let all = devices.list();
            if all.is_empty() {
                println!("no paired devices — `lyra pair` makes a code");
            }
            for d in all {
                println!(
                    "{}  {:<16} paired {} · last seen {} · notifications {}",
                    d.id,
                    d.name,
                    d.created.with_timezone(&chrono::Local).format("%Y-%m-%d"),
                    d.last_seen.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
                    if d.push.is_some() { "on" } else { "off" }
                );
            }
        }
    }
}

/// `lyra service`: install a systemd service that runs `lyra serve` (a user
/// service; a system service when run as root).
pub(crate) fn service_command() {
    let binary = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "lyra".into());
    let root = std::fs::read_to_string("/proc/self/status").is_ok_and(|s| s.lines().any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0")));
    let dir = if root {
        std::path::PathBuf::from("/etc/systemd/system")
    } else {
        match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h).join(".config/systemd/user"),
            None => {
                eprintln!("lyra: no home directory");
                std::process::exit(1);
            }
        }
    };
    let path = dir.join("lyra.service");
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, serve::service_unit(&binary, root))) {
        eprintln!("lyra: couldn't write {}: {e}", path.display());
        std::process::exit(1);
    }
    println!("wrote {}", path.display());
    if root {
        println!("start it now and at every boot:");
        println!("  systemctl daemon-reload");
        println!("  systemctl enable --now lyra");
        println!("logs: journalctl -u lyra -f");
        if binary.starts_with("/root/") {
            println!("note: SELinux won't let systemd run {binary}; install with --root /usr/local");
        }
        return;
    }
    println!("start it now and at every boot:");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now lyra");
    println!("  loginctl enable-linger $USER     # keep it running when you're logged out");
    println!("logs: journalctl --user -u lyra -f");
}

/// Open the evolution records and make sure there's a first generation.
pub(crate) fn open_evolution(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<Evolution>>, Result<String, String>) {
    let c = &config.evolution;
    if !c.enabled {
        return (None, Ok("evolution off".into()));
    }
    let Some(home) = config::home() else {
        return (None, Err("evolution off: no home directory".into()));
    };
    match lyra_evolution::EvolutionManager::open(&home, runtime.clone(), c.settings.clone()) {
        Ok(manager) => {
            let code = if c.source_repo().is_some() { " · code lab on" } else { "" };
            let status = format!("evolution · {} · mode {:?}{code}", context::show(&home.join("evolution")), c.settings.mode);
            let evolution = Evolution::new(manager, runtime.clone(), c.source_repo(), c.benchmark_tasks.max(1));
            (Some(Arc::new(evolution)), Ok(status))
        }
        Err(e) => (None, Err(format!("evolution off: {e:#}"))),
    }
}

/// Open the plan store and engine, unless planning is off.
pub(crate) fn open_planning(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<Engine>>, Result<String, String>) {
    let c = &config.planning;
    if !c.enabled {
        return (None, Ok("planning off".into()));
    }
    let Some(path) = c.path() else {
        return (None, Err("planning off: no home directory (set [planning] path)".into()));
    };
    let settings = lyra_execution::Settings { budget: c.budget, max_parallel: c.max_parallel };
    match Engine::open(&path, runtime.clone(), settings) {
        Ok(engine) => (Some(Arc::new(engine)), Ok(format!("plans · {}", context::show(&path)))),
        Err(e) => (None, Err(format!("planning off: {e:#}"))),
    }
}

/// Open the skill files and their ledger.
pub(crate) fn open_learning(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> (Option<Arc<Learning>>, Result<String, String>) {
    let c = &config.learning;
    let Some(dir) = c.dir() else {
        return (None, Err("learning off: no home directory (set [learning] dir)".into()));
    };
    match runtime.block_on(lyra_learning::SkillManager::open(&dir, c.settings.clone())) {
        Ok(manager) => {
            let shown = context::show(&dir);
            let status = format!("skills · {shown} · mode {}", c.settings.mode.as_str());
            let learning = Learning::new(Arc::new(manager), runtime.clone(), shown);
            (Some(Arc::new(learning)), Ok(status))
        }
        Err(e) => (None, Err(format!("learning off: {e:#}"))),
    }
}

/// Open the memory database and build the memory tools, if enabled.
/// Open the memory store (LanceDB by default), connect the embedding model
/// and build the memory tools, if enabled. Also returns notes for the log.
pub(crate) fn open_memory(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> (Option<Arc<Tools>>, Result<String, String>, Vec<String>) {
    let c = &config.memory;
    if !c.enabled {
        return (None, Ok("memory off".into()), Vec::new());
    }
    let Some(path) = c.path() else {
        return (None, Err("memory off: no data directory (set [memory] path)".into()), Vec::new());
    };
    let opened = match c.backend {
        config::MemoryBackend::Lance => runtime.block_on(lyra_memory::MemoryManager::open_lance(&path, &c.table, c.settings.clone())),
        config::MemoryBackend::Sqlite => runtime.block_on(lyra_memory::MemoryManager::open_sqlite(&path, c.settings.clone())),
    };
    match opened {
        Ok(manager) => {
            // Memories saved in this session are traced to it (provenance).
            manager.set_conversation(Some(Uuid::new_v4()));
            let shown = format!("{} · {}", manager.backend(), context::show(&path));
            let mut mem = Mem::new(manager, runtime.clone(), shown.clone(), c.project());
            mem.backups = config::home().map(|h| h.join("backup"));
            mem.set_vector_index_threshold(c.vector_index_threshold);
            let mut notes = Vec::new();
            if c.points_at_sqlite() {
                notes.push(format!(
                    "[memory] path {} is a SQLite file; memory now lives in LanceDB at {} (remove the path line, or set it to a directory)",
                    c.path.clone().unwrap_or_default(),
                    context::show(&path)
                ));
            }
            notes.extend(mem.set_embedding(config.embedding.clone()));
            (Some(Arc::new(Tools::new(Arc::new(mem)))), Ok(shown), notes)
        }
        Err(e) => (None, Err(format!("memory off: {e:#}")), Vec::new()),
    }
}

/// Open the agent profiles (`~/.lyra/agents/*.toml`), their ledger and the
/// routing index, unless agents are off.
pub(crate) fn open_agents(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<agents::Agents>>, Result<String, String>) {
    if !config.agents.enabled {
        return (None, Ok("agents off".into()));
    }
    let Some(dir) = config::home().map(|h| h.join("agents")) else {
        return (None, Err("agents off: no home directory".into()));
    };
    match agents::Agents::open(&dir, runtime.clone(), config.agents.clone()) {
        Ok(a) => {
            if let Some(endpoint) = config.embedding.clone()
                && let Ok(provider) = retrieval::EndpointEmbedder::connect(endpoint)
            {
                a.router.set_embedder(Some(Arc::new(provider.with_instruction(retrieval::ROUTING_INSTRUCTION))));
            }
            // System access comes with an operator to use it (once: deleting it sticks).
            if config.system.enabled && a.registry.get("operator").is_none() && a.registry.versions("operator").is_ok_and(|v| v.is_empty())
                && let Some(p) = lyra_agents::templates::template("operator")
            {
                let _ = a.registry.create(p, "installed with system access");
            }
            // Coding agents come with a Coder to hand work to them (once: deleting it sticks).
            if config.coding.enabled && a.registry.get("coder").is_none() && a.registry.versions("coder").is_ok_and(|v| v.is_empty())
                && let Some(p) = lyra_agents::templates::template("coder")
            {
                let _ = a.registry.create(p, "installed with coding agents (Claude Code, OpenCode)");
            }
            // PMI comes with a Project Manager to work in it (once: deleting it sticks).
            if pmi::anyone() && a.registry.get("project-manager").is_none() && a.registry.versions("project-manager").is_ok_and(|v| v.is_empty())
                && let Some(p) = lyra_agents::templates::template("project-manager")
            {
                let _ = a.registry.create(p, "installed with PMI");
            }
            // A Project Manager from before PMI: its tools and instructions.
            if let Some(mut pm) = a.registry.get("project-manager").filter(|p| p.template.as_deref() == Some("project-manager"))
                && !pm.tools.iter().any(|t| t == "pmi_tasks")
            {
                for t in lyra_agents::templates::PM_TOOLS {
                    if !pm.tools.iter().any(|x| x == t) {
                        pm.tools.push(t.to_string());
                    }
                }
                pm.instructions = lyra_agents::templates::PM_NOTE.into();
                let _ = a.registry.update(pm, "PMI tools");
            }
            // Agents that look things up know the user's people and notes too.
            for name in ["researcher", "assistant"] {
                if let Some(mut p) = a.registry.get(name).filter(|p| p.template.as_deref() == Some(name))
                    && !p.tools.iter().any(|t| t == "who_is")
                {
                    p.tools.extend(["who_is".to_string(), "note_find".to_string()]);
                    let _ = a.registry.update(p, "new tools: who_is, note_find");
                }
            }
            // …and can read the user's project folders.
            for name in ["researcher", "assistant"] {
                if let Some(mut p) = a.registry.get(name).filter(|p| p.template.as_deref() == Some(name))
                    && !p.tools.iter().any(|t| t == "project_read")
                {
                    p.tools.extend(["project_folders", "project_list", "project_read", "project_search"].map(String::from));
                    let _ = a.registry.update(p, "new tools: project folders (read)");
                }
            }
            // The Coder's instructions as its template has them now (work stays on the server).
            if let Some(mut coder) = a.registry.get("coder").filter(|p| p.template.as_deref() == Some("coder"))
                && let Some(t) = lyra_agents::templates::template("coder")
                && coder.instructions != t.instructions
            {
                coder.instructions = t.instructions;
                let _ = a.registry.update(coder, "instructions: work stays on the server unless a machine is named");
            }
            // System tools added since the Operator was installed from its template.
            if let Some(mut op) = a.registry.get("operator").filter(|p| p.template.as_deref() == Some("operator")) {
                let missing: Vec<String> = ["upload_place", "fleet_run", "routine_create", "routine_list"].iter().filter(|t| !op.tools.iter().any(|x| x == *t)).map(|t| t.to_string()).collect();
                // Routines: the Operator schedules with routine_create, not cron.
                let note = !op.instructions.contains(lyra_agents::templates::ROUTINE_NOTE);
                if note {
                    // An earlier version of the note is replaced, not repeated.
                    let base = ["Work on the server (machine", "For @all or a group", "Something to do on a schedule"]
                        .iter()
                        .filter_map(|start| op.instructions.find(start))
                        .min()
                        .map_or(op.instructions.clone(), |i| op.instructions[..i].to_string());
                    op.instructions = format!("{} {}", base.trim_end(), lyra_agents::templates::ROUTINE_NOTE);
                }
                if !missing.is_empty() || note {
                    op.tools.extend(missing.iter().cloned());
                    let _ = a.registry.update(op, &format!("new tools: {}{}", missing.join(", "), if note { " (and how to schedule)" } else { "" }));
                }
            }
            (Some(Arc::new(a)), Ok(format!("agents · {}", context::show(&dir))))
        }
        Err(e) => (None, Err(format!("agents off: {e:#}"))),
    }
}

/// Open the goal store (`~/.lyra/goals/goals.db`), unless goals are off.
pub(crate) fn open_goals(config: &Config, runtime: &tokio::runtime::Handle) -> Option<Arc<Goals>> {
    if !config.goals.enabled {
        return None;
    }
    let path = config::home()?.join("goals").join("goals.db");
    lyra_goals::GoalManager::open(&path, runtime.clone(), config.goals.settings.clone()).ok().map(|m| Arc::new(Goals::new(m)))
}

/// Open the capability registry: usage history and discovery index in
/// `~/.lyra/capabilities`, providers from `[capabilities]`, plus the memory
/// tools, skills, workflows and helper agents. Returns notes for the log.
pub(crate) fn open_capabilities(
    config: &Config,
    runtime: &tokio::runtime::Handle,
    tools: &Option<Arc<Tools>>,
    learning: &Option<Arc<Learning>>,
    evolution: &Option<Arc<Evolution>>,
) -> (Option<Arc<Caps>>, Vec<String>) {
    let c = &config.capabilities;
    let Some(dir) = config::home().map(|h| h.join("capabilities")) else {
        return (None, vec!["capabilities off: no home directory".into()]);
    };
    let manager = match runtime.block_on(lyra_capabilities::CapabilityManager::open(&dir, c.settings.clone())) {
        Ok(m) => m,
        Err(e) => return (None, vec![format!("capabilities off: {e:#}")]),
    };
    let (openapi, mcp, mut notes) = Caps::connect(&c.openapi, &c.mcp, config::expand_path);
    if let Some(endpoint) = config.embedding.clone()
        && let Ok(provider) = retrieval::EndpointEmbedder::connect(endpoint)
    {
        manager.set_embedder(Some(Arc::new(provider.with_instruction(retrieval::CAPABILITY_INSTRUCTION))));
    }
    let mut caps = Caps::new(manager, runtime.clone(), openapi, mcp);
    caps.tools = tools.clone();
    caps.learning = learning.clone();
    caps.evolution = evolution.clone();
    // Always there, so the rules can be switched on from the app; while
    // `enabled` is off every check says so and no system tools are offered.
    caps.system = Some(lyra_system::System::new(config.system.clone(), config::expand_path));
    if config.search.enabled {
        caps.search = Some(config.search.clone());
    }
    caps.groups = config.groups.iter().map(|(g, m)| (g.to_lowercase(), m.clone())).collect();
    notes.extend(caps.refresh());
    notes.push(format!("capabilities · {} ({} callable)", caps.manager.all().len(), caps.manager.all().iter().filter(|c| c.kind.callable()).count()));
    (Some(Arc::new(caps)), notes)
}

/// The models an OpenAI-compatible endpoint offers (`GET /models`).
pub fn models(base_url: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let body: Value = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("couldn't list models: {e}"))?;
    Ok(body["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
}

/// `lyra --restore-memory <backup>`: put a memory backup in place before
/// anything opens the store. The replaced store is kept next to it.
pub(crate) fn restore_memory(backup: &str) -> Result<String, String> {
    let config = Config::load()?;
    if config.memory.backend != config::MemoryBackend::Lance {
        return Err("restore works for the lance backend; for sqlite, copy the file back".into());
    }
    let path = config.memory.path().ok_or("no memory path")?;
    let kept = lyra_memory::restore(std::path::Path::new(backup), &path).map_err(|e| format!("{e:#}"))?;
    Ok(format!("restored memory from {backup}; the previous store is kept at {}", kept.display()))
}
