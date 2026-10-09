//! A chat turn: the user's message goes out with the prompt's parts (memories,
//! skills, goals, style), the tool loop runs, the reply streams back.


use crate::*;

impl App {
    /// Push the user's input and start streaming a reply on a background thread.
    pub(crate) fn send(&mut self) {
        let content = self.input.trim().to_string();
        // An agent is waiting for a yes or no: this line answers it.
        if !self.approvals.is_empty() && !content.is_empty() && !content.starts_with('/') {
            self.input.clear();
            self.scroll = None;
            self.answer_approval(&content);
            return;
        }
        // Stopping works while a reply is running (nothing else does).
        if content == "/stop" {
            self.input.clear();
            let text = match self.stop() {
                Ok(t) => t,
                Err(e) => e,
            };
            self.messages.push(Message::new("info", format!("> /stop\n{text}")));
            return;
        }
        if content.is_empty() || self.waiting {
            return;
        }
        self.input.clear();
        self.scroll = None;
        if content.starts_with('/') {
            self.command(&content);
            if let Some(next) = self.pending_input.take() {
                self.input = next;
                self.send();
            }
            return;
        }
        // The agent wizard takes plain lines as its answers (in the conversation that opened it).
        if self.wizard_active() {
            self.wizard_input(&content);
            return;
        }
        self.judge_last_run(&content);
        self.handled_by.clear();
        let mut user = Message::new("user", content.clone());
        user.images = std::mem::take(&mut self.attach_images);
        self.messages.push(user);
        self.applied_skills.clear();
        self.applied_skills_tokens = 0;
        self.applied_memories.clear();
        self.applied_memories_tokens = 0;
        let run = Uuid::new_v4();
        self.run = Some(run);
        self.waiting = true;
        self.started = Some(Instant::now());
        self.set_phase(Phase::Waiting);

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let system = self.system_prompt.clone().map(|p| Message::new("system", p));
        let history: Vec<Value> = system
            .iter()
            .chain(self.messages.iter().filter(|m| m.is_history()))
            .map(|m| {
                let mut v = serde_json::to_value(m).expect("message serializes");
                // A model that can see gets the images as parts of the message.
                if self.vision && !m.images.is_empty() {
                    let mut parts = vec![json!({ "type": "text", "text": m.content })];
                    parts.extend(m.images.iter().map(|url| json!({ "type": "image_url", "image_url": { "url": url } })));
                    v["content"] = Value::Array(parts);
                }
                v
            })
            .collect();
        let (model, tools, tx) = (self.model.clone(), self.tools.clone(), self.tx.clone());
        self.cancel = Cancel::default();
        let cancel = self.cancel.clone();
        let (learning, evolution, caps) = (self.learning.clone(), self.evolution.clone(), self.caps.clone());
        // Anyone but the owner sees only their own memories, and not the owner's
        // goals; a member (not an admin) also gets no admin tools.
        let member = !self.admin;
        let viewer = self.personal();
        let goals_section = self.goals.as_ref().and_then(|g| g.prompt_section());
        // How this person writes, for anything lyra writes as them.
        let style_section = style::section(&self.owner);
        let mut agent_env = self.agent_env();
        // `@desktop`: that machine is where system work goes.
        if let (Some(env), Some(caps)) = (agent_env.as_mut(), &self.caps) {
            let known: Vec<String> = caps.machines().into_iter().map(|m| m.0).collect();
            env.machine = agents::machine_mention(&content, &known);
            let groups: Vec<String> = caps.groups.keys().cloned().collect();
            env.fleet = agents::fleet_mention(&content, &groups);
        }
        let owner = self.owner.clone();
        // What lyra said last, so "yes, do it" reads as the go-ahead it is.
        let last_reply: String = self.messages.iter().rev().skip(1).find(|m| m.role == "assistant").map(|m| m.content.chars().take(400).collect()).unwrap_or_default();
        let looks = std::mem::take(&mut self.attach_looks);
        // Their own tool-call limit, else the shared one (evolved, or behavior.toml).
        let max_rounds = limits::tool_rounds(&owner, self.evolution.as_ref().map(|e| e.behavior().max_tool_rounds)) as usize;
        self.can_continue = false;
        thread::spawn(move || {
            // This turn works in its person's PMI account.
            pmi::set_user(&owner);
            let mut history = history;
            // Pictures and scans the chat model can't see: the vision model reads
            // them first, and what it saw goes with the message.
            if !looks.is_empty() {
                let mut seen = String::new();
                for (name, _mime, path) in &looks {
                    let _ = tx.send(StreamEvent::Log(format!("looking at {name}…")));
                    let text = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| vision::read(name, &b, Some(&content))) {
                        Ok(t) => t,
                        Err(e) => format!("(couldn't read it: {e})"),
                    };
                    seen += &format!("\n\n**What {name} shows** (read by the vision model):\n{}", text.chars().take(12_000).collect::<String>());
                }
                if let Some(last) = history.iter_mut().rev().find(|m| m["role"] == "user") {
                    let now = last["content"].as_str().unwrap_or("").to_string();
                    last["content"] = json!(format!("{now}{seen}"));
                }
                let _ = tx.send(StreamEvent::Seen(seen));
            }
            // Evolved behavior: guidelines and the matching workflow.
            if let Some(section) = evolution.as_ref().and_then(|e| e.chat_section(&content)) {
                add_to_system(&mut history, &section);
            }
            if let Some(section) = &goals_section {
                add_to_system(&mut history, section);
            }
            if let Some(section) = &style_section {
                add_to_system(&mut history, section);
            }
            if let Some(tools) = &tools {
                let mine = viewer.as_ref().map(|v| format!("user:{v}"));
                apply_memories(&tools.mem, &content, run, &mut history, &tx, mine.as_deref());
            }
            let skills = learning.map(|l| apply_skills(&l, &content, run, &mut history, &tx)).unwrap_or_default();
            // Just talking (a greeting, a thank-you, "test"): no tools offered but
            // the search for one, so nothing gets sent or changed by a guess.
            let chatting = looks.is_empty() && chatting(&content, &last_reply);
            if chatting {
                let _ = tx.send(StreamEvent::Log("just chatting: no tools offered".into()));
                add_to_system(&mut history, CHATTING);
            } else if caps.is_some() {
                add_to_system(&mut history, TOOL_RULES);
            }
            // What this turn's changes come from (for "What lyra knows about me" → Why?).
            crate::actions::because(crate::actions::Why { source: "chat".into(), detail: content.chars().take(300).collect(), skills });
            // A specialist may take it first; the main agent checks and presents
            // its result (A6, A11).
            if let Some(env) = agent_env.as_ref().filter(|_| !chatting) {
                if let Some(names) = env.agents.registry.enabled().iter().map(|a| format!("{} ({})", a.name, a.description)).reduce(|a, b| format!("{a}; {b}")) {
                    add_to_system(&mut history, &format!("{}\nSpecialist agents you can hand work to with the delegate tool: {names}. Keep simple requests yourself.", agents::MAIN_ROLE));
                }
                if agents::auto_delegate(env, &content, run, &mut history).is_some() {
                    add_to_system(&mut history, agents::MAIN_NOTE);
                }
            }
            let event = match converse(&url, &model, history, caps.as_deref(), agent_env.as_ref().filter(|_| !chatting), &content, chatting, max_rounds, run, &tx, &cancel, viewer.as_deref(), member) {
                Ok((stats, limited, answered)) => {
                    // The turn's own model calls (agents' and lyra's count separately), as the model that answered.
                    usage::record("chat", &answered, stats.input, stats.cached, stats.output, stats.elapsed.as_millis() as u64);
                    let _ = tx.send(StreamEvent::Done(stats));
                    if limited {
                        let _ = tx.send(StreamEvent::Limit(max_rounds));
                    }
                    return;
                }
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }
}

/// Add the skills that match the user's message to the system prompt, and
/// record their use for this run.
pub(crate) fn apply_skills(
    learning: &Learning,
    message: &str,
    run: Uuid,
    history: &mut Vec<Value>,
    tx: &Sender<StreamEvent>,
) -> Vec<String> {
    match learning.relevant(message) {
        Ok(skills) if !skills.is_empty() => {
            let section = learn::prompt_section(&skills);
            let ids: Vec<Uuid> = skills.iter().map(|r| r.skill.id).collect();
            if let Err(e) = learning.record_usage(run, &ids) {
                let _ = tx.send(StreamEvent::Log(format!("recording skill use failed: {e}")));
            }
            let names: Vec<String> = skills
                .into_iter()
                .map(|r| if r.trial { format!("{} (trial)", r.skill.name) } else { r.skill.name })
                .collect();
            let tokens = learn::approx_tokens(&section);
            let _ = tx.send(StreamEvent::SkillsApplied { names: names.clone(), tokens });
            add_to_system(history, &section);
            names
        }
        Ok(_) => Vec::new(),
        Err(e) => {
            let _ = tx.send(StreamEvent::Log(format!("skill search failed: {e}")));
            Vec::new()
        }
    }
}

/// Append a section to the system message, adding one if there isn't one.
pub(crate) fn add_to_system(history: &mut Vec<Value>, section: &str) {
    match history.first_mut() {
        Some(first) if first["role"] == "system" => {
            let prompt = format!("{}\n\n{section}", first["content"].as_str().unwrap_or(""));
            first["content"] = Value::String(prompt);
        }
        _ => history.insert(0, json!({ "role": "system", "content": section })),
    }
}

/// Add the memories worth this message's prompt space (the context compiler)
/// and the working memory to the system prompt.
/// `mine`: a member's own scope (`user:<id>`): only their memories, and none
/// of the owner's working memory or project.
pub(crate) fn apply_memories(mem: &Mem, message: &str, run: Uuid, history: &mut Vec<Value>, tx: &Sender<StreamEvent>, mine: Option<&str>) {
    if let Some(scope) = mine {
        match mem.compile_for(message, run, scope) {
            Ok(compiled) => {
                let mut section = compiled.section.unwrap_or_default();
                if !compiled.used.is_empty() {
                    let ids = compiled.used.iter().map(|r| r.memory.short_id()).collect();
                    let texts = compiled.used.iter().map(|r| r.memory.content.chars().take(240).collect()).collect();
                    let _ = tx.send(StreamEvent::MemoriesApplied { ids, texts, tokens: compiled.tokens as u64 });
                }
                section.push_str(&format!("\n\nThis person's memories are theirs alone: save and recall them in scope \"{scope}\"."));
                add_to_system(history, section.trim());
            }
            Err(e) => {
                let _ = tx.send(StreamEvent::Log(format!("memory recall failed: {e}")));
            }
        }
        return;
    }
    mem.working().note_entities(message);
    let mut sections = Vec::new();
    match mem.compile(message, run) {
        Ok(compiled) => {
            if let Some(section) = compiled.section {
                let ids = compiled.used.iter().map(|r| r.memory.short_id()).collect();
                let texts = compiled.used.iter().map(|r| r.memory.content.chars().take(240).collect()).collect();
                let _ = tx.send(StreamEvent::MemoriesApplied { ids, texts, tokens: compiled.tokens as u64 });
                sections.push(section);
            }
        }
        Err(e) => {
            let _ = tx.send(StreamEvent::Log(format!("memory recall failed: {e}")));
        }
    }
    sections.extend(mem.working().render());
    if let Some(project) = mem.project() {
        sections.push(format!(
            "Current project: {project}. Memories about it belong in scope \"project:{project}\"; other projects' memories are left out unless you recall them by scope."
        ));
    }
    if !sections.is_empty() {
        add_to_system(history, &sections.join("\n\n"));
    }
}

/// Context files plus, when memory is on, instructions for the memory tools.
pub(crate) fn system_prompt(context: &Context, memory: bool) -> Option<String> {
    let parts: Vec<String> = context
        .system_prompt()
        .into_iter()
        .chain(memory.then(|| tools::MEMORY_PROMPT.to_string()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// One user turn: stream a reply; if the model calls tools, run them, add the
/// results to the history and stream again. Stats cover the whole turn; the
/// flag says it stopped at `max_rounds` with the work unfinished.
/// Said to the model on a turn that's just conversation.
const CHATTING: &str = "This message is conversation, not a request to do something: answer it in words. Don't send, change or look up anything unless the user plainly asks; if they do, find the tool with capability_search.";

/// Said to the model whenever tools are offered.
const TOOL_RULES: &str = "Use a tool only when the request needs it. If it isn't clear what the user wants done (who an email goes to, what it says, which task), ask them instead of guessing. If the user says no to something, don't try it again: tell them what you would have done, or ask what they'd like instead.";

/// A message that's plainly just talk, without asking anyone: greetings,
/// thanks, "test", and "ok"/"great" unless lyra just asked something (then
/// it's a yes).
pub(crate) fn small_talk(message: &str, asked: bool) -> bool {
    let m: String = message.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '\'' { c } else { ' ' }).collect();
    let words: Vec<&str> = m.split_whitespace().collect();
    if words.is_empty() || words.len() > 4 {
        return false;
    }
    const ALONE: &[&str] = &[
        "hi", "hello", "hey", "hiya", "yo", "morning", "evening", "thanks", "thank", "thx", "ty", "cheers", "ok", "okay", "k", "cool", "nice", "great", "awesome", "test",
        "testing", "ping", "lol", "haha", "bye", "goodbye", "night", "goodnight", "sup",
    ];
    const WITH: &[&str] = &["you", "lyra", "there", "again", "message", "much", "so", "good", "1", "2", "3", "one", "two", "123", "a", "this", "is", "just", "only", "all", "very", "u"];
    const AGREE: &[&str] = &["ok", "okay", "k", "cool", "nice", "great", "awesome"];
    let alone = |w: &&str| ALONE.contains(w) && !(asked && AGREE.contains(w));
    words.iter().any(alone) && words.iter().all(|w| alone(w) || WITH.contains(w))
}

/// The decision model, or plain small talk, says this turn needs no tools.
/// When unsure, tools are offered as always.
fn chatting(message: &str, last_reply: &str) -> bool {
    if small_talk(message, last_reply.contains('?')) {
        return true;
    }
    if message.chars().count() > 300 {
        return false;
    }
    let state = if last_reply.is_empty() { format!("The user: {message}") } else { format!("lyra said: {last_reply}\n\nThe user: {message}") };
    crate::decide::yes("just chatting?", &state, CHAT_GATE).is_some_and(|(yes, _)| yes)
}

/// The decision model's question for [`chatting`].
const CHAT_GATE: &str = "Is the user's latest message only conversation (a greeting, thanks, small talk, a test, an opinion, or a question answered from general knowledge), and not asking lyra to do, find, check, write, send or change anything (mail, calendar, tasks, notes, files, machines, memory, the web), or agreeing to something lyra offered to do?";

#[allow(clippy::too_many_arguments)]
pub(crate) fn converse(
    url: &str,
    model: &str,
    mut history: Vec<Value>,
    caps: Option<&Caps>,
    agents: Option<&agents::Env>,
    request: &str,
    chatting: bool,
    max_rounds: usize,
    run: Uuid,
    tx: &Sender<StreamEvent>,
    cancel: &Cancel,
    viewer: Option<&str>,
    member: bool,
) -> Result<(Stats, bool, String), String> {
    let start = Instant::now();
    // The model answering: the main one, or the fallback while it's down.
    let fallback = crate::fallback::target();
    let mut current = (url.to_string(), model.to_string());
    let mut told = false;
    let switch = |current: &mut (String, String), told: &mut bool, why: &str| {
        if let Some(fb) = &fallback {
            *current = fb.clone();
            if !*told {
                *told = true;
                let _ = tx.send(StreamEvent::Notice(format!("The chat model isn't answering ({why}): {} answered instead.", fb.1)));
            }
        }
    };
    // A member's calls: their own memory scope only, and no admin tools.
    let mine = viewer.map(|v| format!("user:{v}"));
    let mine_scopes: Vec<&str> = mine.iter().map(String::as_str).collect();
    let scopes = mine.is_some().then_some(mine_scopes.as_slice());
    let mut total: Option<Stats> = None;
    let finish = |total: Option<Stats>| {
        let mut stats = total.unwrap_or(Stats { ttft: None, elapsed: Duration::ZERO, input: 0, cached: 0, output: 0, estimated: true });
        stats.elapsed = start.elapsed();
        stats
    };
    // Capabilities the model found with the search tool, offered from then on.
    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Tools the user said no to in this turn: not asked about again.
    let mut refused: std::collections::HashSet<String> = std::collections::HashSet::new();
    for round in 1..=max_rounds.max(1) {
        let n = history.len();
        let note = format!("round {round} · sending {n} message{}", if n == 1 { "" } else { "s" });
        tx.send(StreamEvent::Log(note)).map_err(|e| e.to_string())?;
        let mut body = json!({
            "model": model,
            "messages": history,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(caps) = caps {
            let mut definitions = if chatting { caps.chat_definitions(&found) } else { caps.definitions(request, &found) };
            // The main agent can hand work to a specialist itself.
            if let Some(env) = agents {
                let enabled = env.agents.registry.enabled();
                if !enabled.is_empty() {
                    definitions.push(agents::delegate_tool(&enabled));
                }
            }
            if !definitions.is_empty() {
                body["tools"] = Value::Array(definitions);
            }
        }
        if stopped(cancel) {
            return Ok((finish(total), false, current.1.clone()));
        }
        if let Some(why) = crate::fallback::blocked() {
            return Err(why);
        }
        if current.0 == url && crate::fallback::skip_main() {
            if crate::known_down::is_down("chat") {
                // Known down: no note in every reply, just where it went.
                if let Some(fb) = &fallback {
                    current = fb.clone();
                    told = true;
                }
            } else {
                switch(&mut current, &mut told, "it failed a moment ago");
            }
        }
        body["model"] = json!(current.1);
        let round = match stream(&current.0, &body, tx, cancel) {
            Ok(r) => {
                if current.0 == url {
                    crate::fallback::main_ok();
                }
                r
            }
            // Not there (before saying anything): the fallback takes this round and the rest.
            Err(e) if current.0 == url && fallback.is_some() && e.starts_with(UNREACHED) && crate::fallback::unreachable(&e) => {
                crate::fallback::main_failed();
                let _ = tx.send(StreamEvent::Log(format!("chat model failed: {e}")));
                switch(&mut current, &mut told, e.trim_start_matches(UNREACHED).chars().take(80).collect::<String>().as_str());
                body["model"] = json!(current.1);
                stream(&current.0, &body, tx, cancel)?
            }
            Err(e) => return Err(e),
        };
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        let Some(caps) = caps.filter(|_| !round.tool_calls.is_empty() && !round.stopped) else {
            return Ok((finish(total), false, current.1.clone()));
        };

        tx.send(StreamEvent::ToolCalls(round.tool_calls.clone())).map_err(|e| e.to_string())?;
        history.push(json!({
            "role": "assistant",
            "content": round.content,
            "tool_calls": round.tool_calls,
        }));
        for call in &round.tool_calls {
            // Stopped: the calls not made yet answer so (the history stays well-formed).
            if stopped(cancel) {
                history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": "{\"error\":\"stopped by the user\"}" }));
                continue;
            }
            let ctx = CallContext { member, read_scopes: scopes, write_scopes: scopes, ..CallContext::new(Some(run), &call.id) };
            // Policy, usage tracking and verification happen in there.
            let name = call.function.name.as_str();
            let content = if refused.contains(name) {
                json!({ "error": format!("the user already said no to {name} in this conversation turn: don't try it again; answer them") }).to_string()
            } else if let Some(problem) = caps.problem(name, &call.function.arguments) {
                // Nothing to ask the user about: the model is told what's missing.
                json!({ "error": problem, "hint": "don't guess: ask the user for what's missing" }).to_string()
            } else if let (Some(env), "delegate") = (agents, name) {
                agents::delegate_call(env, &call.function.arguments, run)
            } else if call.function.name == caps::SEARCH_TOOL {
                let (text, names) = caps.search(&call.function.arguments);
                found.extend(names);
                text
            } else if let Some(ask) = caps.manager.get(&call.function.name).filter(|c| matches!(c.source.as_str(), "calendar" | "mail" | "projects")).and_then(|_| caps.approval(&call.function.name, &call.function.arguments)) {
                // Changes others see, and files on their PC: the person approves them right here.
                match agents.map(|env| agents::approve(env, &agents::main_profile(), &call.function.name, ask)) {
                    Some(Ok(())) => caps.invoke(&call.function.name, &call.function.arguments, ctx, true, true),
                    Some(Err(why)) => {
                        refused.insert(call.function.name.clone());
                        json!({ "error": why, "hint": "the user said no: don't try this again; tell them what you would have done, or ask what they'd like" }).to_string()
                    }
                    None => json!({ "error": "that needs the user's approval, and approvals need agents on ([agents] enabled)" }).to_string(),
                }
            } else {
                caps.invoke(&call.function.name, &call.function.arguments, ctx, false, true)
            };
            history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            let (id, name) = (call.id.clone(), call.function.name.clone());
            tx.send(StreamEvent::ToolResult { id, name, content }).map_err(|e| e.to_string())?;
        }
    }
    // The limit: what was done so far stays in the conversation, to continue from.
    Ok((finish(total), true, current.1.clone()))
}

/// How a failure before the model said anything starts (the fallback may take over then).
pub(crate) const UNREACHED: &str = "couldn't reach the chat model: ";

/// POST the request, forward each delta as it arrives, and measure the reply.
pub(crate) fn stream(url: &str, body: &Value, tx: &Sender<StreamEvent>, cancel: &Cancel) -> Result<Round, String> {
    // No overall timeout: a long generation is fine as long as tokens keep coming.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut ttft = None;
    let mut usage = None;
    let mut chunks = 0;
    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let resp = client.post(url).json(body).send().map_err(|e| format!("{UNREACHED}{e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{UNREACHED}{status}: {}", resp.text().unwrap_or_default().chars().take(500).collect::<String>()));
    }
    let mut logged_first = false;
    let mut was_stopped = false;
    for line in BufReader::new(resp).lines() {
        // Dropping the response closes the connection, and the server stops generating.
        if stopped(cancel) {
            was_stopped = true;
            break;
        }
        if !logged_first && let Some(ttft) = ttft {
            logged_first = true;
            tx.send(StreamEvent::Log(format!("first token after {}", secs(ttft))))
                .map_err(|e| e.to_string())?;
        }
        let line = line.map_err(|e| e.to_string())?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let chunk: Chunk = serde_json::from_str(data).map_err(|e| format!("{e}: {data}"))?;
        usage = chunk.usage.or(usage);
        let Some(choice) = chunk.choices.into_iter().next() else { continue };
        let delta = choice.delta;
        for part in delta.tool_calls {
            ttft.get_or_insert_with(|| start.elapsed());
            if tool_calls.len() <= part.index {
                tool_calls.resize_with(part.index + 1, ToolCall::default);
            }
            let call = &mut tool_calls[part.index];
            call.kind = "function".into();
            if let Some(id) = part.id {
                call.id = id;
            }
            if let Some(f) = part.function {
                call.function.name += f.name.as_deref().unwrap_or("");
                call.function.arguments += f.arguments.as_deref().unwrap_or("");
            }
        }
        if let Some(t) = &delta.content {
            content.push_str(t);
        }
        let events = [
            delta.reasoning_content.map(StreamEvent::Reasoning),
            delta.content.map(StreamEvent::Token),
        ];
        for event in events.into_iter().flatten() {
            if matches!(&event, StreamEvent::Token(t) | StreamEvent::Reasoning(t) if t.is_empty()) {
                continue;
            }
            ttft.get_or_insert_with(|| start.elapsed());
            chunks += 1;
            tx.send(event).map_err(|e| e.to_string())?;
        }
    }
    let elapsed = start.elapsed();
    let stats = match usage {
        Some(u) => Stats {
            ttft,
            elapsed,
            input: u.prompt_tokens,
            cached: u.cached(),
            output: u.completion_tokens,
            estimated: false,
        },
        // Most servers send one token per chunk, so the chunk count is a fair guess.
        None => Stats { ttft, elapsed, input: 0, cached: 0, output: chunks, estimated: true },
    };
    if was_stopped {
        let _ = tx.send(StreamEvent::Log("stopped".into()));
        // Half-streamed tool calls aren't run.
        return Ok(Round { stats, content, tool_calls: Vec::new(), stopped: true });
    }
    Ok(Round { stats, content, tool_calls, stopped: false })
}

#[cfg(test)]
mod tests {
    use super::small_talk;

    #[test]
    fn small_talk_is_only_talk() {
        for m in ["test message", "Hi!", "hello lyra", "thanks", "Thank you so much", "testing 123", "ok"] {
            assert!(small_talk(m, false), "{m}");
        }
        for m in ["send a test message to Dana", "hey what's on my calendar", "check the disks", "ok send it", "remind me tomorrow", ""] {
            assert!(!small_talk(m, false), "{m}");
        }
        assert!(!small_talk("ok", true), "a yes to lyra's question");
        assert!(small_talk("thanks", true));
    }
}
