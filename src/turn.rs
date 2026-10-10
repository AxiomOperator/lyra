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
        // "Don't show steps" from a steps card, while the reply runs: no echo
        // (it would split the reply being written).
        if self.waiting && matches!(content.as_str(), "/steps off" | "/steps on") {
            self.input.clear();
            self.steps_wait = content == "/steps on";
            self.log(Level::Info, format!("steps first: {}", if self.steps_wait { "on" } else { "off" }));
            return;
        }
        if content.is_empty() || self.waiting {
            return;
        }
        self.input.clear();
        self.scroll = None;
        // The last message again (another model), or changed: its reply goes.
        if let Some(which) = content.strip_prefix("/retry").filter(|r| r.is_empty() || r.starts_with(' ')) {
            self.redo(None, which.trim());
            return;
        }
        if let Some(text) = content.strip_prefix("/edit ").map(str::trim).filter(|t| !t.is_empty()) {
            self.redo(Some(text.to_string()), "");
            return;
        }
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

        // `/retry other`: this reply from the other model.
        let (base, model) = self.next_model.take().unwrap_or_else(|| (self.base_url.clone(), self.model.clone()));
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let system = self.system_prompt.clone().map(|p| Message::new("system", p));
        // The oldest turns already summarized: the note goes instead of them
        // (dropped if the conversation was cut back before them: /retry, /edit).
        let kept = self.messages.iter().filter(|m| m.is_history()).count();
        if self.compacted.as_ref().is_some_and(|(upto, _)| *upto >= kept) {
            self.compacted = None;
        }
        let (already, prior) = self.compacted.clone().map_or((0, None), |(n, s)| (n, Some(s)));
        let history: Vec<Value> = system
            .iter()
            .chain(self.messages.iter().filter(|m| m.is_history()).skip(already))
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
        let (tools, tx) = (self.tools.clone(), self.tx.clone());
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
        // Cards in the chat (forms, steps) need someone looking at the app.
        let (chat_only, steps_wait, interactive, unattended) = (self.chat_only, self.steps_wait, self.hub.is_some() && !self.unattended, self.unattended);
        // Their own tool-call limit, else the shared one (evolved, or behavior.toml).
        let max_rounds = limits::tool_rounds(&owner, self.evolution.as_ref().map(|e| e.behavior().max_tool_rounds)) as usize;
        self.can_continue = false;
        let job = self.session_id.clone();
        crate::acting::spawn(move || {
            // This turn works in its person's PMI account; its calls count for this conversation.
            pmi::set_user(&owner);
            usage::set_job(Some(job));
            let mut history = history;
            // Nearing the model's context: the oldest turns become one note.
            let mut summary = prior;
            let summarize = |text: &str| crate::learn::complete_light(&url, &model, SUMMARIZE, text).map(|(s, _)| s);
            if let Some((n, note)) = compact(&mut history, summary.as_deref(), room(), summarize) {
                let _ = tx.send(StreamEvent::Compacted { upto: already + n, summary: note.clone() });
                summary = Some(note);
            }
            if let Some(note) = &summary {
                add_to_system(&mut history, &format!("{EARLIER}\n{note}"));
            }
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
            let offer = if chat_only {
                add_to_system(&mut history, CHAT_ONLY);
                Offer::Nothing
            } else if looks.is_empty() && chatting(&content, &last_reply) {
                let _ = tx.send(StreamEvent::Log("just chatting: no tools offered".into()));
                add_to_system(&mut history, CHATTING);
                Offer::Search
            } else {
                if caps.is_some() {
                    add_to_system(&mut history, TOOL_RULES);
                }
                Offer::All
            };
            let chatting = offer != Offer::All;
            let how = How { offer, asks: interactive, steps: interactive && steps_wait, finish: unattended };
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
            let event = match converse(&url, &model, history, caps.as_deref(), agent_env.as_ref().filter(|_| !chatting), &content, how, max_rounds, run, &tx, &cancel, viewer.as_deref(), member) {
                Ok((stats, limited, _answered)) => {
                    // The turn's model calls were recorded round by round (agents' and lyra's separately).
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

impl App {
    /// `/retry [other]` and `/edit <text>`: the last message is sent again (as
    /// it was, or changed), and everything after it goes; `other` has the other
    /// model answer (the fallback, or the main one when the fallback answered).
    pub(crate) fn redo(&mut self, edit: Option<String>, which: &str) {
        let Some(i) = self.messages.iter().rposition(|m| m.role == "user") else {
            self.messages.push(Message::new("info", "Nothing to send again yet.".into()));
            return;
        };
        let other = match which {
            "" | "main" => None,
            "other" | "fallback" => {
                let Some(other) = self.other_model() else {
                    self.messages.push(Message::new("error", "> /retry other\nthere's no other model: set up [fallback_model]".into()));
                    return;
                };
                Some(other)
            }
            _ => {
                self.messages.push(Message::new("error", format!("> /retry {which}\nuse /retry, or /retry other for the other model")));
                return;
            }
        };
        let original = std::mem::take(&mut self.messages[i].content);
        let images = std::mem::take(&mut self.messages[i].images);
        let text = match edit {
            // What was attached stays with it.
            Some(t) => match original.find("\n\n**Attached: ") {
                Some(at) => format!("{t}{}", &original[at..]),
                None => t,
            },
            None => original,
        };
        self.messages.truncate(i);
        self.can_continue = false;
        // Not a correction of the last run (it's gone).
        self.last_run = None;
        self.log(Level::Info, match (&other, which) {
            (Some((_, m)), _) => format!("sending the last message again, to {m}"),
            (None, _) => "sending the last message again".to_string(),
        });
        self.next_model = other;
        self.input = text;
        self.attach_images = images;
        self.send();
        self.attach_images.clear();
    }

    /// The other model the last reply can come from: the fallback, or the main
    /// one when the fallback answered it (base URL and name).
    pub(crate) fn other_model(&self) -> Option<(String, String)> {
        // Either one marked down: there's only one model to ask.
        let fb = crate::fallback::first_up().filter(|_| !crate::known_down::is_down("chat"))?;
        let start = self.messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
        let by_fallback = self.messages[start..].iter().any(|m| m.role == "info" && m.content.contains(&format!(": {} answered instead", fb.model)));
        Some(if by_fallback { (self.base_url.clone(), self.model.clone()) } else { (fb.url, fb.model) })
    }

    /// A device's answer to a question lyra asked in the chat (a form, steps).
    pub(crate) fn answer_ask(&mut self, id: u64, value: &Value) {
        let Some(i) = self.asks.iter().position(|r| r.id == id) else { return };
        let Some(answer) = crate::asks::read(&self.asks[i].kind, value) else { return };
        // Choosing what to skip: the steps wait, the card stays.
        if answer == crate::asks::Answer::Hold {
            let _ = self.asks[i].reply.send(answer);
            return;
        }
        let r = self.asks.remove(i);
        let said = match &answer {
            crate::asks::Answer::Filled(m) => format!("filled in {}", m.keys().cloned().collect::<Vec<_>>().join(", ")),
            crate::asks::Answer::Go { skip } if skip.is_empty() => "go".to_string(),
            crate::asks::Answer::Go { skip } => format!("skip {} step{}", skip.len(), if skip.len() == 1 { "" } else { "s" }),
            crate::asks::Answer::Cancel | crate::asks::Answer::Hold => "dismissed".to_string(),
        };
        self.log(Level::Agent, format!("answered in the chat: {said}"));
        let _ = r.reply.send(answer);
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
/// Which tools a turn is offered.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Offer {
    /// Those that fit the request (the usual).
    All,
    /// Just talk: only the search for one.
    Search,
    /// Chat only (the conversation's switch): none.
    Nothing,
}

/// How a turn goes about tools.
#[derive(Clone, Copy, Debug)]
pub(crate) struct How {
    pub offer: Offer,
    /// It may ask the user in the chat for a missing piece (someone's looking at the app).
    pub asks: bool,
    /// A round of several calls shows its steps first, to skip any.
    pub steps: bool,
    /// At the tool-call limit, one more reply without tools: the answer from
    /// what was found (nobody is there to press Continue).
    pub finish: bool,
}

/// Room for the conversation, in characters: most of the model's context
/// (about 3.5 characters a token), leaving space for the reply.
fn room() -> usize {
    let window = crate::stats::CONTEXT_WINDOW.load(std::sync::atomic::Ordering::Relaxed) as usize;
    let tokens = if window == 0 { 32_768 } else { window };
    tokens * 7 / 2 * 6 / 10
}

/// How many of the latest exchanges always stay whole when a conversation is compacted.
const KEEP_TURNS: usize = 4;

/// Asked of the background model when a conversation is compacted.
const SUMMARIZE: &str = "You condense the older part of a conversation between a person and their assistant, lyra, into a note lyra reads instead of it from now on. \
Keep what matters for carrying on: what the person asked for and decided, facts, names, numbers, dates, file names, commands, what was done and what came of it, \
open questions and anything promised. Drop greetings, repetition and raw tool output beyond what it showed. Write short bullet points, at most about 400 words, nothing else.";

/// Heads the note in the system prompt.
const EARLIER: &str = "Earlier in this conversation (its older turns, summarized to fit; the latest turns follow in full):";

/// When the conversation nears the model's context (80% of the room), its
/// oldest turns — all but the latest `KEEP_TURNS` exchanges, cut where a
/// person spoke so no tool call loses its result — become one note, with any
/// earlier note folded in. `history` loses those turns; the note and how many
/// messages it took come back (the caller puts the note in the system prompt
/// and keeps both with the conversation). Nothing happens when it fits, when
/// there's too little to summarize, or when the summary fails (then `fit`
/// still shortens old tool results).
fn compact(history: &mut Vec<Value>, prior: Option<&str>, room: usize, summarize: impl FnOnce(&str) -> Result<String, String>) -> Option<(usize, String)> {
    let size: usize = history.iter().map(|m| m["content"].as_str().map_or(0, str::len) + m["tool_calls"].to_string().len()).sum();
    if size <= room * 8 / 10 {
        return None;
    }
    let start = usize::from(history.first().is_some_and(|m| m["role"] == "system"));
    let people: Vec<usize> = history.iter().enumerate().skip(start).filter(|(_, m)| m["role"] == "user").map(|(i, _)| i).collect();
    if people.len() <= KEEP_TURNS {
        return None;
    }
    let cut = people[people.len() - KEEP_TURNS];
    let mut text = String::new();
    if let Some(p) = prior {
        text += &format!("The note so far:\n{p}\n\nWhat came after it:\n");
    }
    for m in &history[start..cut] {
        let body = m["content"].as_str().unwrap_or("");
        let line = match m["role"].as_str().unwrap_or("") {
            "user" => format!("Person: {body}"),
            "tool" => format!("(a tool returned: {})", body.chars().take(600).collect::<String>()),
            _ => {
                let calls: Vec<&str> = m["tool_calls"].as_array().into_iter().flatten().filter_map(|c| c["function"]["name"].as_str()).collect();
                let used = if calls.is_empty() { String::new() } else { format!(" [used {}]", calls.join(", ")) };
                format!("lyra{used}: {body}")
            }
        };
        text += &line.chars().take(4000).collect::<String>();
        text += "\n\n";
    }
    let text: String = text.chars().take(80_000).collect();
    let note = summarize(&text).ok()?;
    let note = note.rsplit("</think>").next().unwrap_or(&note).trim().to_string();
    if note.is_empty() {
        return None;
    }
    history.drain(start..cut);
    Some((cut - start, note))
}

/// Older tool results shortened when the conversation outgrows the model's
/// context (long research: dozens of fetched pages). The newest few stay whole.
fn fit(history: &mut [Value], room: usize) -> usize {
    let size = |h: &[Value]| h.iter().map(|m| m["content"].as_str().map_or(0, str::len) + m["tool_calls"].to_string().len()).sum::<usize>();
    if size(history) <= room {
        return 0;
    }
    let tools: Vec<usize> = history.iter().enumerate().filter(|(_, m)| m["role"] == "tool").map(|(i, _)| i).collect();
    let keep = tools.len().saturating_sub(6);
    let mut cut = 0;
    for &i in &tools[..keep] {
        if size(history) <= room {
            break;
        }
        let text = history[i]["content"].as_str().unwrap_or("").to_string();
        if text.len() > 1500 {
            let start: String = text.chars().take(1200).collect();
            history[i]["content"] = json!(format!("{start}\n… (shortened to fit the conversation: {} characters left out)", text.len() - start.len()));
            cut += 1;
        }
    }
    cut
}

/// Said to the model when the conversation is set to chat only.
const CHAT_ONLY: &str = "Tools are off in this conversation (the user set it to chat only): answer in words. If they ask for something that needs a tool (mail, calendar, tasks, files, machines), say you can do it once they turn Chat only off.";

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
const CHAT_GATE: &str = "Is the user's latest message only conversation (a greeting, thanks, small talk, a test, an opinion, or a question answered from general knowledge), and not asking lyra to do, find, check, write, send or change anything (mail, calendar, tasks, notes, files, machines, memory, the web), not asking about lyra itself (what it can do, when something changed, its version), and not agreeing to something lyra offered to do?";

#[allow(clippy::too_many_arguments)]
pub(crate) fn converse(
    url: &str,
    model: &str,
    mut history: Vec<Value>,
    caps: Option<&Caps>,
    agents: Option<&agents::Env>,
    request: &str,
    how: How,
    max_rounds: usize,
    run: Uuid,
    tx: &Sender<StreamEvent>,
    cancel: &Cancel,
    viewer: Option<&str>,
    member: bool,
) -> Result<(Stats, bool, String), String> {
    let start = Instant::now();
    // The model answering: the main one, or the next fallback in line while it's down.
    let mut current = (url.to_string(), model.to_string());
    // Moving down the line, said in the chat (unless `why` is None: known down).
    let switch = |current: &mut (String, String), next: (String, String), why: Option<&str>| {
        if let Some(why) = why {
            let who = if current.0 == url { "The chat model isn't answering".to_string() } else { format!("{} isn't answering either", current.1) };
            let _ = tx.send(StreamEvent::Notice(format!("{who} ({why}): {} answered instead.", next.1)));
        }
        *current = next;
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
        let shortened = fit(&mut history, room());
        if shortened > 0 {
            let _ = tx.send(StreamEvent::Log(format!("shortened {shortened} older tool result{} to fit the model's context", if shortened == 1 { "" } else { "s" })));
        }
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
            let mut definitions = match how.offer {
                Offer::All => caps.definitions(request, &found),
                Offer::Search => caps.chat_definitions(&found),
                Offer::Nothing => Vec::new(),
            };
            // The main agent can hand work to a specialist itself.
            if let Some(env) = agents {
                let enabled = env.agents.registry.enabled();
                if !enabled.is_empty() {
                    definitions.push(agents::delegate_tool(&enabled));
                    // The conversation's task board, shared with the agents.
                    if !matches!(how.offer, Offer::Nothing) {
                        definitions.extend(crate::board::definitions());
                    }
                    // A group discussion between agents.
                    let talkers: Vec<_> = enabled.iter().filter(|a| !(env.member && agents::admin_only(&a.name))).cloned().collect();
                    if talkers.len() >= 2 && !matches!(how.offer, Offer::Nothing) {
                        definitions.push(agents::huddle_tool(&talkers));
                        definitions.push(agents::swarm_tool(&talkers));
                    }
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
        if current.0 == url
            && crate::fallback::skip_main()
            && let Some(next) = crate::fallback::target()
        {
            // Known down: no note in every reply, just where it went.
            let why = (!crate::known_down::is_down("chat")).then_some("it failed a moment ago");
            switch(&mut current, next, why);
        }
        body["model"] = json!(current.1);
        let round = loop {
            match stream(&current.0, &body, tx, cancel) {
                Ok(r) => {
                    if current.0 == url {
                        crate::fallback::main_ok();
                    } else {
                        crate::fallback::ok(&current.0, &current.1);
                    }
                    break r;
                }
                // Not there (before saying anything): the next in line takes this round and the rest.
                Err(e) if e.starts_with(UNREACHED) && crate::fallback::unreachable(&e) => {
                    let Some(next) = crate::fallback::after(&current.0, &current.1) else { return Err(e) };
                    if current.0 == url {
                        crate::fallback::main_failed();
                    } else {
                        crate::fallback::failed(&current.0, &current.1);
                    }
                    let _ = tx.send(StreamEvent::Log(format!("{} failed: {e}", if current.0 == url { "chat model" } else { current.1.as_str() })));
                    switch(&mut current, next, Some(e.trim_start_matches(UNREACHED).chars().take(80).collect::<String>().as_str()));
                    body["model"] = json!(current.1);
                }
                Err(e) => return Err(e),
            }
        };
        // Each round as the model that answered it (the fallback may take over part way).
        usage::record("chat", &current.1, round.stats.input, round.stats.cached, round.stats.output, round.stats.elapsed.as_millis() as u64);
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        let Some(caps) = caps.filter(|_| !round.tool_calls.is_empty() && !round.stopped) else {
            return Ok((finish(total), false, current.1.clone()));
        };

        tx.send(StreamEvent::ToolCalls(round.tool_calls.clone())).map_err(|e| e.to_string())?;
        let at = history.len();
        history.push(json!({
            "role": "assistant",
            "content": round.content,
            "tool_calls": round.tool_calls,
        }));
        // Several steps at once: shown first, a few seconds to skip any.
        let skipped: Vec<String> = if how.steps && round.tool_calls.iter().filter(|c| c.function.name != caps::SEARCH_TOOL).count() >= 2 {
            let steps = round.tool_calls.iter().map(|c| crate::asks::Step { call_id: c.id.clone(), name: c.function.name.clone(), summary: crate::asks::summary(&c.function.arguments) }).collect();
            match crate::asks::ask(tx, &round.tool_calls[0].id, crate::asks::Kind::Steps { steps, seconds: crate::asks::STEPS_WAIT }) {
                crate::asks::Answer::Go { skip } => skip,
                _ => round.tool_calls.iter().map(|c| c.id.clone()).collect(),
            }
        } else {
            Vec::new()
        };
        // Searches and page reads in one round go side by side (each as this
        // thread's person, counted for this conversation).
        let mut ready: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        // How big this conversation is: the web reader condenses pages only once it's big.
        let size: usize = history.iter().map(|m| m["content"].as_str().map_or(0, str::len)).sum();
        crate::websearch::set_conversation_size(size);
        let side_by_side: Vec<&ToolCall> = round.tool_calls.iter().filter(|c| matches!(c.function.name.as_str(), "web_search" | "web_fetch") && !skipped.contains(&c.id)).collect();
        if side_by_side.len() > 1 && !stopped(cancel) {
            let (user, job) = (crate::acting::current(), crate::usage::job());
            std::thread::scope(|s| {
                let handles: Vec<_> = side_by_side
                    .iter()
                    .map(|c| {
                        let (user, job) = (user.clone(), job.clone());
                        s.spawn(move || {
                            crate::acting::run(&user, || {
                                crate::usage::set_job(job);
                                crate::websearch::set_conversation_size(size);
                                let ctx = CallContext { member, read_scopes: scopes, write_scopes: scopes, ..CallContext::new(Some(run), &c.id) };
                                (c.id.clone(), caps.invoke(&c.function.name, &c.function.arguments, ctx, false, true))
                            })
                        })
                    })
                    .collect();
                for h in handles {
                    if let Ok((id, out)) = h.join() {
                        ready.insert(id, out);
                    }
                }
            });
        }
        // Several delegations in one round: side by side (each with its own budget and approvals).
        if let Some(env) = agents {
            let many: Vec<(String, String)> = round
                .tool_calls
                .iter()
                .filter(|c| c.function.name == "delegate" && !skipped.contains(&c.id) && !ready.contains_key(&c.id))
                .map(|c| (c.id.clone(), c.function.arguments.clone()))
                .collect();
            if many.len() > 1 && !stopped(cancel) {
                let _ = tx.send(StreamEvent::Log(format!("{} delegations side by side", many.len())));
                ready.extend(agents::delegate_many(env, &many, Some(run), None));
            }
        }
        for (k, call) in round.tool_calls.iter().enumerate() {
            // Stopped: the calls not made yet answer so (the history stays well-formed).
            if stopped(cancel) {
                history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": "{\"error\":\"stopped by the user\"}" }));
                continue;
            }
            let ctx = CallContext { member, read_scopes: scopes, write_scopes: scopes, ..CallContext::new(Some(run), &call.id) };
            // Policy, usage tracking and verification happen in there.
            let name = call.function.name.as_str();
            let mut problem = caps.problem(name, &call.function.arguments);
            // Something only the user knows (who it goes to): a small form in the chat.
            let mut filled = None;
            if let Some(p) = problem.as_ref().filter(|p| how.asks && !p.fill.is_empty() && !refused.contains(name) && !skipped.contains(&call.id)) {
                let what = caps.manager.get(name).map(|c| c.description.split(['.', ':']).next().unwrap_or("").trim().to_string()).unwrap_or_default();
                match crate::asks::ask(tx, &call.id, crate::asks::Kind::Fill { tool: name.to_string(), what, fields: p.fill.clone() }) {
                    crate::asks::Answer::Filled(values) if !values.is_empty() => {
                        let arguments = crate::asks::merge(&call.function.arguments, &values);
                        history[at]["tool_calls"][k]["function"]["arguments"] = json!(arguments);
                        let _ = tx.send(StreamEvent::ToolArgs { id: call.id.clone(), arguments: arguments.clone() });
                        problem = caps.problem(name, &arguments);
                        filled = Some(arguments);
                    }
                    _ => problem = Some(crate::caps::Problem { text: "the user dismissed the form asking for it".into(), fill: Vec::new() }),
                }
            }
            let arguments = filled.as_deref().unwrap_or(&call.function.arguments);
            let content = if let Some(out) = ready.remove(&call.id) {
                out
            } else if skipped.contains(&call.id) {
                json!({ "error": "the user skipped this step", "hint": "carry on without it, and say what was skipped" }).to_string()
            } else if refused.contains(name) {
                json!({ "error": format!("the user already said no to {name} in this conversation turn: don't try it again; answer them") }).to_string()
            } else if let Some(problem) = problem {
                // Nothing (more) to ask the user here: the model is told what's missing.
                json!({ "error": problem.text, "hint": "don't guess: ask the user for what's missing" }).to_string()
            } else if let (Some(env), "delegate") = (agents, name) {
                agents::delegate_call(env, arguments, run)
            } else if let Some(env) = agents.filter(|_| crate::board::TOOLS.contains(&name)) {
                crate::board::call(&env.session, "lyra", name, arguments)
            } else if let (Some(env), "agent_huddle") = (agents, name) {
                agents::huddle(env, arguments, Some(run))
            } else if let (Some(env), "agent_swarm") = (agents, name) {
                agents::swarm(env, arguments, Some(run))
            } else if call.function.name == caps::SEARCH_TOOL {
                let (text, names) = caps.search(arguments);
                found.extend(names);
                text
            } else if let Some(ask) = caps.manager.get(&call.function.name).filter(|c| matches!(c.source.as_str(), "calendar" | "mail" | "projects")).and_then(|_| caps.approval(&call.function.name, arguments)) {
                // Changes others see, and files on their PC: the person approves them right here.
                match agents.map(|env| agents::approve(env, &agents::main_profile(), &call.function.name, ask)) {
                    Some(Ok(())) => caps.invoke(&call.function.name, arguments, ctx, true, true),
                    Some(Err(why)) => {
                        refused.insert(call.function.name.clone());
                        json!({ "error": why, "hint": "the user said no: don't try this again; tell them what you would have done, or ask what they'd like" }).to_string()
                    }
                    None => json!({ "error": "that needs the user's approval, and approvals need agents on ([agents] enabled)" }).to_string(),
                }
            } else {
                caps.invoke(&call.function.name, arguments, ctx, false, true)
            };
            history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            let (id, name) = (call.id.clone(), call.function.name.clone());
            tx.send(StreamEvent::ToolResult { id, name, content }).map_err(|e| e.to_string())?;
        }
    }
    // Nobody to press Continue (a routine): one last reply, no tools, from what was found.
    if how.finish && !stopped(cancel) {
        fit(&mut history, room());
        history.push(json!({ "role": "user", "content": "You've used all your tool calls for this run. Don't call any more tools: write the complete answer now from what you found, and say what you couldn't get to." }));
        let _ = tx.send(StreamEvent::Log("tool-call limit: writing the answer from what was found".into()));
        let body = json!({ "model": current.1, "messages": history, "stream": true, "stream_options": { "include_usage": true } });
        let round = stream(&current.0, &body, tx, cancel)?;
        usage::record("chat", &current.1, round.stats.input, round.stats.cached, round.stats.output, round.stats.elapsed.as_millis() as u64);
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        return Ok((finish(total), false, current.1.clone()));
    }
    // The limit: what was done so far stays in the conversation, to continue from.
    Ok((finish(total), true, current.1.clone()))
}

/// How long the model may say nothing (not a token, not a keep-alive) before
/// lyra gives up on it: 3 minutes (a long prompt takes a while to read), or
/// `LYRA_MODEL_SILENCE_SECONDS`.
pub(crate) fn silence() -> Duration {
    Duration::from_secs(std::env::var("LYRA_MODEL_SILENCE_SECONDS").ok().and_then(|s| s.parse().ok()).filter(|s| *s > 0).unwrap_or(180))
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
    // The request and its lines on their own thread, so Stop works even while
    // the model is silent (thinking, or stuck): this side waits in short steps.
    let (lines_tx, lines) = std::sync::mpsc::channel::<Result<String, String>>();
    let (url, body) = (url.to_string(), body.clone());
    crate::acting::spawn(move || {
        let resp = match client.post(&url).json(&body).send() {
            Ok(r) => r,
            Err(e) => return drop(lines_tx.send(Err(format!("{UNREACHED}{e}")))),
        };
        let status = resp.status();
        if !status.is_success() {
            let _ = lines_tx.send(Err(format!("{UNREACHED}{status}: {}", resp.text().unwrap_or_default().chars().take(500).collect::<String>())));
            return;
        }
        // Gone (stopped): the response drops, the connection closes, the server stops generating.
        for line in BufReader::new(resp).lines() {
            if lines_tx.send(line.map_err(|e| e.to_string())).is_err() {
                return;
            }
        }
    });
    let mut logged_first = false;
    let mut was_stopped = false;
    // A model that took the request and then says nothing: given up on after a
    // while, as unreachable when it said nothing at all (the fallback answers).
    let mut heard = Instant::now();
    let quiet_for = silence();
    loop {
        let line = match lines.recv_timeout(Duration::from_millis(150)) {
            Ok(line) => {
                heard = Instant::now();
                line
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if stopped(cancel) => {
                was_stopped = true;
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if heard.elapsed() >= quiet_for => {
                let secs = quiet_for.as_secs();
                return Err(if chunks == 0 && tool_calls.is_empty() && content.is_empty() {
                    format!("{UNREACHED}timed out: the model took the request but said nothing for {secs}s")
                } else {
                    format!("the model stopped part way through its answer (nothing for {secs}s)")
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if stopped(cancel) {
            was_stopped = true;
            break;
        }
        if !logged_first && let Some(ttft) = ttft {
            logged_first = true;
            tx.send(StreamEvent::Log(format!("first token after {}", secs(ttft))))
                .map_err(|e| e.to_string())?;
        }
        let line = line?;
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
    use super::{compact, fit, small_talk};
    use serde_json::json;

    #[test]
    fn the_oldest_turns_become_one_note_when_the_conversation_nears_the_context() {
        let mut history = vec![json!({ "role": "system", "content": "You are lyra." })];
        for n in 0..8 {
            history.push(json!({ "role": "user", "content": format!("question {n} {}", "x".repeat(400)) }));
            history.push(json!({ "role": "assistant", "content": "", "tool_calls": [{ "id": format!("c{n}"), "type": "function", "function": { "name": "web_search", "arguments": "{}" } }] }));
            history.push(json!({ "role": "tool", "tool_call_id": format!("c{n}"), "content": "found it" }));
            history.push(json!({ "role": "assistant", "content": format!("answer {n}") }));
        }
        // It fits: nothing happens, and the summary isn't asked for.
        let mut same = history.clone();
        assert!(compact(&mut same, None, 1_000_000, |_| panic!("not asked")).is_none());
        // Near the limit: the oldest four exchanges become the note; the last four stay whole.
        let mut seen = String::new();
        let (n, note) = compact(&mut history, Some("- they use Fedora"), 4_000, |text| {
            seen = text.to_string();
            Ok("- asked questions 0 to 3\n- they use Fedora".into())
        })
        .unwrap();
        assert_eq!(n, 16, "four exchanges of four messages");
        assert!(note.contains("they use Fedora"));
        assert!(seen.starts_with("The note so far:\n- they use Fedora") && seen.contains("Person: question 0") && seen.contains("[used web_search]") && !seen.contains("question 4"));
        assert_eq!(history.len(), 1 + 16);
        assert_eq!(history[0]["content"], "You are lyra.");
        assert!(history[1]["content"].as_str().unwrap().starts_with("question 4"), "cut where the person spoke");
        // A failed summary leaves it as it was (fit still shortens tool results).
        let mut kept = history.clone();
        assert!(compact(&mut kept, None, 100, |_| Err("down".into())).is_none());
        assert_eq!(kept.len(), history.len());
        // Too few turns to summarize: left alone.
        let mut short = history[..9].to_vec();
        assert!(compact(&mut short, None, 10, |_| Ok("x".into())).is_none());
    }

    #[test]
    fn long_research_is_shortened_to_fit_oldest_first() {
        let page = "x".repeat(20_000);
        let mut h = vec![json!({ "role": "user", "content": "brief me" })];
        for i in 0..10 {
            h.push(json!({ "role": "assistant", "content": "", "tool_calls": [{ "id": format!("c{i}") }] }));
            h.push(json!({ "role": "tool", "tool_call_id": format!("c{i}"), "content": page }));
        }
        assert_eq!(fit(&mut h, 1_000_000), 0, "it fits: nothing changes");
        let cut = fit(&mut h, 130_000);
        assert!(cut > 0 && cut <= 4, "{cut}");
        assert!(h[2]["content"].as_str().unwrap().contains("shortened to fit"), "the oldest first");
        assert_eq!(h[20]["content"].as_str().unwrap().len(), 20_000, "the newest stay whole");
        assert_eq!(h[0]["content"], "brief me");
    }

    #[test]
    fn a_silent_model_is_given_three_minutes_by_default() {
        if std::env::var_os("LYRA_MODEL_SILENCE_SECONDS").is_none() {
            assert_eq!(super::silence(), std::time::Duration::from_secs(180));
        }
    }

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
