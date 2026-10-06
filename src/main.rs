mod agents;
mod caps;
mod commands;
mod config;
mod connect;
mod context;
mod evolve;
mod goals;
mod markdown;
mod learn;
mod lock;
mod mem;
mod plan;
mod migrate;
mod retrieval;
mod serve;
mod sessions;
mod stats;
mod tools;
mod websearch;
mod ui;

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use config::Config;
use context::Context;
use learn::{Learning, Review, SkillsSnapshot};
use lyra_learning::{Applied, Mode, SkillOutcome, Uuid, evaluator};
use retrieval::Endpoint;
use stats::{Pricing, Stats, Totals, Usage, secs};
use lyra_memory::CaptureMode;
use lyra_memory::capture;
use lyra_execution::{Engine, ExecutionEvent, Goal, GoalStatus, Plan, PlanStatus, RunOutcome};
use evolve::{Evolution, EvolutionSnapshot};
use mem::{Mem, MemorySnapshot};
use caps::Caps;
use goals::{Goals, GoalsSnapshot};
use plan::LyraRuntime;
use tools::{CallContext, Tools};

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
    /// Tools an assistant message asked to run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCall>,
    /// For `role: "tool"`: the call this is the result of.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// The model's thinking; shown in the UI but never sent back to the model.
    #[serde(skip)]
    reasoning: String,
    /// Timing and token counts for an assistant reply.
    #[serde(skip)]
    stats: Option<Stats>,
    /// Memories that were in the prompt for this reply (short ids).
    #[serde(skip)]
    memories: Vec<String>,
    /// Learned skills that were in the prompt for this reply.
    #[serde(skip)]
    skills: Vec<String>,
    /// Specialist agents that worked on this reply.
    #[serde(skip)]
    agents: Vec<String>,
    /// Images sent with it (data URLs), for a model that can see.
    #[serde(skip)]
    images: Vec<String>,
    /// The content rendered as Markdown, and the content length it was
    /// rendered from (re-rendered when a streaming reply grows).
    #[serde(skip)]
    rendered: Option<(usize, Vec<ratatui::text::Line<'static>>)>,
}

impl Message {
    fn new(role: &str, content: String) -> Self {
        Self {
            role: role.into(),
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: String::new(),
            stats: None,
            memories: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            images: Vec::new(),
            rendered: None,
        }
    }

    /// Whether the model sees this message (info and error lines are UI-only).
    fn is_history(&self) -> bool {
        matches!(self.role.as_str(), "user" | "assistant" | "tool")
    }
}

/// A complete tool call, as sent back to the model in the conversation history.
#[derive(Clone, Default, Serialize, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCall,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct FunctionCall {
    name: String,
    /// JSON text, streamed in pieces.
    arguments: String,
}

/// A streamed fragment of a tool call; fragments with the same `index` are joined.
#[derive(Deserialize)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// One SSE chunk from a streaming `/chat/completions` response.
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    /// Sent in the final chunk when `stream_options.include_usage` is set.
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    /// llama.cpp/DeepSeek use `reasoning_content`; some servers use `reasoning`.
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

/// What one streamed response produced.
struct Round {
    stats: Stats,
    content: String,
    tool_calls: Vec<ToolCall>,
    /// The user stopped it partway.
    stopped: bool,
}

/// Set to stop the run in progress (`/stop`, the stop button, Ctrl-X).
type Cancel = Arc<std::sync::atomic::AtomicBool>;

fn stopped(cancel: &Cancel) -> bool {
    cancel.load(std::sync::atomic::Ordering::SeqCst)
}

enum StreamEvent {
    Token(String),
    /// The main agent is working with a subagent (routing, progress, result).
    Agent(agents::AgentEvent),
    /// An agent waits for the user's approval before changing something.
    Approval(agents::ApprovalRequest),
    Reasoning(String),
    /// The model asked to run these tools.
    ToolCalls(Vec<ToolCall>),
    /// A tool finished; `content` is what the model will see.
    ToolResult { id: String, name: String, content: String },
    Done(Stats),
    Error(String),
    /// Progress note from the worker for the activity log.
    Log(String),
    /// Something to say in the chat (a remote update finished, …).
    Notice(String),
    /// Health check results for the embedding/reranker models.
    Models(Vec<Result<String, String>>),
    /// Fresh numbers for the memory panel.
    Memory(Result<MemorySnapshot, String>),
    /// Memories the context compiler put in the prompt (short ids) and their size.
    MemoriesApplied { ids: Vec<String>, tokens: u64 },
    /// A finished memory capture or curation.
    MemoryReview { curation: bool, review: Review<Vec<String>> },
    /// Notes from background memory work (upkeep, feedback).
    MemoryNotes(Vec<String>),
    /// A new memory was related to similar ones (supersedes, contradicts, …).
    MemoryRelated(Review<Vec<String>>),
    /// Something happened in a running plan.
    PlanEvent(ExecutionEvent),
    /// A request was turned into a goal and plan (or couldn't be).
    PlanCreated(Result<(Goal, Plan), String>),
    /// A plan run ended: finished, paused or failed.
    PlanFinished(Result<RunOutcome, String>),
    /// Notes from planning upkeep (recovery after a restart).
    PlanNotes(Vec<String>),
    /// A finished learning review.
    Reviewed(Review<Applied>),
    /// A finished curation of the skill collection.
    Curated(Review<Vec<String>>),
    /// Notes from background learning work (outcomes, lifecycle changes, sync).
    LearnNotes(Vec<String>),
    /// Skills added to the system prompt for the message being answered,
    /// with their approximate size in tokens.
    SkillsApplied { names: Vec<String>, tokens: u64 },
    /// Fresh contents for the skills panel.
    Skills(Result<SkillsSnapshot, String>),
    /// Background evolution work finished (`/evolve …`, a review, the monitor);
    /// `show` puts its notes in the chat as well as the activity log.
    Evolved { done: evolve::Done, show: bool },
    /// Fresh numbers for the evolution panel.
    Evolution(Result<EvolutionSnapshot, String>),
    /// Notes from capability work (registry refresh, health checks).
    CapNotes(Vec<String>),
    /// Notes from goal work; `show` puts them in the chat too.
    GoalNotes { notes: Vec<String>, show: bool },
    /// A plan was made for a goal (and is now running).
    GoalPlan { result: Result<(Goal, Plan), String>, autonomous: bool },
    /// Fresh contents for the goals panel.
    Goals(Result<GoalsSnapshot, String>),
    /// The agent wizard built and tested a draft.
    AgentBuilt(agents::Built),
}

/// What `main` opened before the UI starts (shared by every conversation).
#[derive(Clone)]
struct Services {
    tools: Option<Arc<Tools>>,
    /// Where memory lives, or why it's off.
    memory_status: Result<String, String>,
    learning: Option<Arc<Learning>>,
    /// Where skills live, or why learning is off.
    learning_status: Result<String, String>,
    engine: Option<Arc<Engine>>,
    /// Where plans live, or why planning is off.
    planning_status: Result<String, String>,
    evolution: Option<Arc<Evolution>>,
    /// Where evolution keeps its records, or why it's off.
    evolution_status: Result<String, String>,
    caps: Option<Arc<Caps>>,
    goals: Option<Arc<Goals>>,
    agents: Option<Arc<agents::Agents>>,
    /// Where agents live, or why they're off.
    agents_status: Result<String, String>,
}

/// What the assistant is doing right now, for the session panel.
#[derive(Clone, PartialEq)]
enum Phase {
    Idle,
    /// Request sent, nothing back yet.
    Waiting,
    Thinking,
    Streaming,
    /// Running these tools.
    Tools(String),
    /// A specialist agent is working (its name, and its tool).
    Delegating(String),
    /// An agent waits for the user's approval.
    Approval(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Tool,
    Learn,
    Memory,
    Plan,
    Evolve,
    Agent,
    Error,
}

/// A finished run, awaiting the user's reaction.
#[derive(Clone)]
struct LastRun {
    id: Uuid,
    skills: Vec<String>,
    memories: usize,
    /// Its evolution telemetry record.
    evo: Option<Uuid>,
}

/// One line in the activity log.
struct Activity {
    time: String,
    level: Level,
    text: String,
}

const MAX_ACTIVITY: usize = 500;

struct App {
    base_url: String,
    model: String,
    /// Built from SOUL.md / AGENT.md / USER.md; sent first with every request.
    system_prompt: Option<String>,
    pricing: Pricing,
    /// Not used by the chat yet; checked at startup and on reload.
    embedding: Option<Endpoint>,
    reranker: Option<Endpoint>,
    /// Memory tools; `None` when disabled or the database couldn't be opened.
    tools: Option<Arc<Tools>>,
    totals: Totals,
    /// When the in-flight request was sent.
    started: Option<Instant>,
    /// When the current phase began, for live timers.
    phase_since: Instant,
    phase: Phase,
    messages: Vec<Message>,
    input: String,
    waiting: bool,
    show_reasoning: bool,
    /// Side panels (Ctrl-B); hidden automatically on narrow terminals.
    show_panels: bool,
    activity: Vec<Activity>,
    /// Context files loaded, as `(name, path)`.
    context_files: Vec<(&'static str, String)>,
    /// `None` while a health check is running.
    model_status: Option<Vec<Result<String, String>>>,
    /// Where memory lives, or why it's off.
    memory_status: Result<String, String>,
    memory: Option<Result<MemorySnapshot, String>>,
    learning: Option<Arc<Learning>>,
    learning_status: Result<String, String>,
    skills: Option<Result<SkillsSnapshot, String>>,
    /// A learning review is running in the background.
    reviewing: bool,
    /// A curation is running in the background.
    curating: bool,
    /// Days between automatic curations (`[learning] curate`), if scheduled.
    curate_every: Option<i64>,
    /// The run (user turn) being answered.
    run: Option<Uuid>,
    /// The last finished run, awaiting the user's reaction.
    last_run: Option<LastRun>,
    /// Memories the context compiler chose for the current (or last) reply.
    applied_memories: Vec<String>,
    applied_memories_tokens: u64,
    /// A memory capture is running in the background.
    capturing: bool,
    /// A memory curation is running in the background.
    memory_curating: bool,
    /// Days between automatic memory curations, if scheduled.
    memory_curate_every: Option<i64>,
    engine: Option<Arc<Engine>>,
    planning_status: Result<String, String>,
    evolution: Option<Arc<Evolution>>,
    evolution_status: Result<String, String>,
    evolution_panel: Option<Result<EvolutionSnapshot, String>>,
    /// Every capability, behind policy (`[capabilities]`).
    caps: Option<Arc<Caps>>,
    /// Long-lived goals and autonomy (`[goals]`).
    goals: Option<Arc<Goals>>,
    goals_panel: Option<Result<GoalsSnapshot, String>>,
    /// Specialist subagents (`~/.lyra/agents`).
    agents: Option<Arc<agents::Agents>>,
    agents_status: Result<String, String>,
    agents_panel: Vec<agents::PanelRow>,
    /// Agents that worked on the reply being answered.
    handled_by: Vec<String>,
    /// Say which agents worked on a reply (`[agents] show_handled_by`).
    show_handled_by: bool,
    /// The agent wizard is building a draft.
    wizard_busy: bool,
    /// A message to send once the current command is handled (`/agent ask`).
    pending_input: Option<String>,
    /// Agents working now and the chat card each one's calls go on.
    agent_cards: Vec<(String, usize)>,
    /// The earliest message changed in place since devices were last updated.
    touched: Option<usize>,
    /// This conversation's id (`~/.lyra/sessions/<id>.json`).
    session_id: String,
    /// Activity lines logged so far (`lyra serve` prints the new ones).
    logged: u64,
    /// The chat model can see images (`vision` in the config).
    vision: bool,
    /// Images for the next message sent (from a device's attachments).
    attach_images: Vec<String>,
    /// The web server, when this lyra is `lyra serve` (machines, devices).
    hub: Option<lyra_web::Hub>,
    /// Everything opened at startup, for starting another conversation.
    shared: Services,
    /// Runs lyra's background work (schedules, goals); a conversation forked
    /// for another device doesn't.
    primary: bool,
    /// Stops the run in progress (a fresh one per run).
    cancel: Cancel,
    /// Agents' actions waiting for the user's y / n / a, oldest first.
    approvals: Vec<agents::ApprovalRequest>,
    /// The highlighted entry in the command palette, and whether Esc closed it.
    palette: usize,
    palette_hidden: bool,
    /// When goals were last checked, and how often to.
    goals_checked: Instant,
    goals_every: Duration,
    /// Plans started by autonomy (their spending counts against the session).
    autonomous_plans: std::collections::HashSet<Uuid>,
    /// The last thing said about autonomy (not repeated every tick).
    autonomy_note: String,
    /// A goal decomposition or review is running.
    goals_busy: bool,
    /// The evolution work running in the background, if any.
    evolving: Option<&'static str>,
    /// Days between automatic evolution reviews, if scheduled.
    evolution_review_every: Option<i64>,
    /// When the schedules were last checked.
    last_schedule_check: Instant,
    /// When this session began (episodes start here).
    session_started: chrono::DateTime<chrono::Utc>,
    /// Tools plan steps may never use (`[planning] forbidden_tools`).
    forbidden_tools: Vec<String>,
    /// A plan is being created or run in the background.
    plan_busy: bool,
    /// The plan shown in the Plan panel (the latest one), and its goal.
    current_plan: Option<Plan>,
    current_goal: Option<Goal>,
    /// Skills used by a reply the user just corrected (feeds the review trigger).
    corrected_skills: Vec<String>,
    /// Skills applied to the message being answered (or last answered).
    applied_skills: Vec<String>,
    /// Their approximate size in the prompt.
    applied_skills_tokens: u64,
    /// Approximate size of the tool definitions sent with every request.
    tools_tokens: u64,
    /// How many tools the model is offered.
    tool_count: usize,
    /// Top line of the chat view when scrolled up; `None` follows the bottom.
    scroll: Option<u16>,
    /// Chat view size from the last frame, used for scroll bounds.
    max_scroll: u16,
    page: u16,
    tx: Sender<StreamEvent>,
    rx: Receiver<StreamEvent>,
}

impl App {
    fn new(config: Config, context: Context, services: Services) -> Self {
        let shared = services.clone();
        let Services {
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
        } = services;
        let show_handled_by = config.agents.show_handled_by;
        let definitions = tools.as_ref().map(|t| t.definitions());
        let tools_tokens = definitions.as_ref().map_or(0, |d| learn::approx_tokens(&d.to_string()));
        let tool_count = definitions.as_ref().and_then(|d| d.as_array().map(Vec::len)).unwrap_or(0);
        let (tx, rx) = mpsc::channel();
        Self {
            base_url: config.url.clone(),
            model: config.model.clone(),
            system_prompt: system_prompt(&context, tools.is_some()),
            pricing: pricing(&config),
            embedding: config.embedding,
            reranker: config.reranker,
            tools,
            totals: Totals::default(),
            started: None,
            phase_since: Instant::now(),
            phase: Phase::Idle,
            messages: Vec::new(),
            input: String::new(),
            waiting: false,
            show_reasoning: true,
            show_panels: true,
            activity: Vec::new(),
            context_files: context.files(),
            model_status: None,
            memory_status,
            memory: None,
            learning,
            learning_status,
            skills: None,
            reviewing: false,
            curating: false,
            curate_every: config.learning.curate.every_days(),
            run: None,
            last_run: None,
            applied_memories: Vec::new(),
            applied_memories_tokens: 0,
            capturing: false,
            memory_curating: false,
            memory_curate_every: config.memory.curate.every_days(),
            engine,
            planning_status,
            evolution,
            evolution_status,
            evolution_panel: None,
            caps,
            goals,
            goals_panel: None,
            agents,
            agents_status,
            agents_panel: Vec::new(),
            handled_by: Vec::new(),
            show_handled_by,
            wizard_busy: false,
            pending_input: None,
            agent_cards: Vec::new(),
            touched: None,
            session_id: sessions::new_id(),
            logged: 0,
            hub: None,
            vision: config.vision,
            attach_images: Vec::new(),
            shared,
            primary: true,
            cancel: Cancel::default(),
            approvals: Vec::new(),
            palette: 0,
            palette_hidden: false,
            goals_checked: Instant::now(),
            goals_every: Duration::from_secs(config.goals.tick_seconds.max(10)),
            autonomous_plans: Default::default(),
            autonomy_note: String::new(),
            goals_busy: false,
            evolving: None,
            evolution_review_every: config.evolution.review.every_days(),
            session_started: chrono::Utc::now(),
            last_schedule_check: Instant::now(),
            forbidden_tools: config.planning.forbidden_tools.clone(),
            plan_busy: false,
            current_plan: None,
            current_goal: None,
            corrected_skills: Vec::new(),
            applied_skills: Vec::new(),
            applied_skills_tokens: 0,
            tools_tokens,
            tool_count,
            scroll: None,
            max_scroll: 0,
            page: 1,
            tx,
            rx,
        }
    }

    /// Push the user's input and start streaming a reply on a background thread.
    fn send(&mut self) {
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
        // The agent wizard takes plain lines as its answers.
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
        let goals_section = self.goals.as_ref().and_then(|g| g.prompt_section());
        let mut agent_env = self.agent_env();
        // `@desktop`: that machine is where system work goes.
        if let (Some(env), Some(caps)) = (agent_env.as_mut(), &self.caps) {
            let known: Vec<String> = caps.machines().into_iter().map(|m| m.0).collect();
            env.machine = agents::machine_mention(&content, &known);
        }
        thread::spawn(move || {
            let mut history = history;
            // Evolved behavior: guidelines, the matching workflow, the round limit.
            let mut max_rounds = 8;
            if let Some(evolution) = &evolution {
                max_rounds = evolution.behavior().max_tool_rounds as usize;
                if let Some(section) = evolution.chat_section(&content) {
                    add_to_system(&mut history, &section);
                }
            }
            if let Some(section) = &goals_section {
                add_to_system(&mut history, section);
            }
            if let Some(tools) = &tools {
                apply_memories(&tools.mem, &content, run, &mut history, &tx);
            }
            if let Some(learning) = learning {
                apply_skills(&learning, &content, run, &mut history, &tx);
            }
            // A specialist may take it first; the main agent checks and presents
            // its result (A6, A11).
            if let Some(env) = &agent_env {
                if let Some(names) = env.agents.registry.enabled().iter().map(|a| format!("{} ({})", a.name, a.description)).reduce(|a, b| format!("{a}; {b}")) {
                    add_to_system(&mut history, &format!("{}\nSpecialist agents you can hand work to with the delegate tool: {names}. Keep simple requests yourself.", agents::MAIN_ROLE));
                }
                if agents::auto_delegate(env, &content, run, &mut history).is_some() {
                    add_to_system(&mut history, agents::MAIN_NOTE);
                }
            }
            let event = match converse(&url, &model, history, caps.as_deref(), agent_env.as_ref(), &content, max_rounds, run, &tx, &cancel) {
                Ok(stats) => StreamEvent::Done(stats),
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }

    fn handle(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Agent(e) => self.agent_event(e),
            StreamEvent::Approval(r) => self.approval_requested(r),
            StreamEvent::AgentBuilt(built) => self.agent_built(built),
            StreamEvent::Token(t) => {
                self.set_phase(Phase::Streaming);
                self.reply().content.push_str(&t);
            }
            StreamEvent::Reasoning(t) => {
                self.set_phase(Phase::Thinking);
                self.reply().reasoning.push_str(&t);
            }
            StreamEvent::ToolCalls(calls) => {
                for call in &calls {
                    let text = format!("{} {}", call.function.name, call.function.arguments);
                    self.log(Level::Tool, text);
                }
                let names: Vec<_> = calls.iter().map(|c| c.function.name.as_str()).collect();
                self.set_phase(Phase::Tools(names.join(", ")));
                self.reply().tool_calls = calls;
            }
            StreamEvent::ToolResult { id, name, content } => {
                self.log(Level::Tool, format!("↳ {content}"));
                self.set_phase(Phase::Waiting);
                if name.starts_with("memory_") || name == "working_memory" {
                    self.refresh_memory();
                }
                if name.starts_with("goal_") {
                    self.refresh_goals();
                }
                // A new fact may update, contradict or support what's known (M3, M4).
                if name == "memory_remember"
                    && let Ok(v) = serde_json::from_str::<Value>(&content)
                    && v["result"] == "remembered"
                    && let Some(id) = v["id"].as_str()
                {
                    self.relate_memory(id.to_string());
                }
                let mut result = Message::new("tool", content);
                result.tool_call_id = Some(id);
                self.messages.push(result);
            }
            StreamEvent::Done(stats) => {
                if stopped(&self.cancel) {
                    let reply = self.reply();
                    reply.content.push_str(if reply.content.is_empty() { "_(stopped)_" } else { "\n\n_(stopped)_" });
                }
                self.waiting = false;
                self.started = None;
                self.set_phase(Phase::Idle);
                self.log(
                    Level::Info,
                    format!("done · {} out tokens · {}", stats.output, secs(stats.elapsed)),
                );
                self.totals.add(&stats);
                let skills = self.applied_skills.clone();
                let memories = self.applied_memories.clone();
                let evo = self.record_chat_run(&stats, None);
                // Working memory tracks what both sides mention (M7).
                if let Some(mem) = self.mem() {
                    let text = self.reply().content.clone();
                    mem.working().note_entities(&text);
                }
                let reply = self.reply();
                reply.stats = Some(stats);
                reply.skills = skills.clone();
                reply.memories = memories.clone();
                if self.show_handled_by {
                    let handled = self.handled_by.clone();
                    self.reply().agents = handled;
                }
                self.last_run = self.run.take().map(|id| LastRun { id, skills, memories: memories.len(), evo });
                self.save_session();
                self.review(false);
                self.capture(false);
            }
            StreamEvent::Error(e) => {
                let stats = Stats {
                    ttft: None,
                    elapsed: self.started.map(|s| s.elapsed()).unwrap_or_default(),
                    input: 0,
                    cached: 0,
                    output: 0,
                    estimated: true,
                };
                self.record_chat_run(&stats, Some(&e));
                self.waiting = false;
                self.started = None;
                self.run = None;
                self.last_run = None;
                self.set_phase(Phase::Idle);
                self.log(Level::Error, e.clone());
                self.messages.push(Message::new("error", e));
                self.save_session();
            }
            StreamEvent::Log(text) => self.log(Level::Info, text),
            StreamEvent::Notice(text) => {
                self.log(Level::Agent, text.clone());
                self.messages.push(Message::new("info", text));
            }
            StreamEvent::Models(results) => {
                for result in &results {
                    match result {
                        Ok(line) => self.log(Level::Info, line.clone()),
                        Err(line) => self.log(Level::Error, line.clone()),
                    }
                }
                self.model_status = Some(results);
            }
            StreamEvent::SkillsApplied { names, tokens } => {
                self.log(Level::Learn, format!("applying skills: {}", names.join(", ")));
                self.applied_skills = names;
                self.applied_skills_tokens = tokens;
            }
            StreamEvent::Reviewed(Review { outcome, usage }) => {
                self.reviewing = false;
                self.totals.add_review(usage.as_ref());
                let tokens = usage_text(usage.as_ref());
                match outcome {
                    Ok(Applied::Created(skill)) => {
                        let id = learn::short_id(skill.id);
                        self.log(Level::Learn, format!("learned {} (proposed) · {tokens}", skill.name));
                        let text = format!(
                            "💡 learned a new skill (proposed): {} — {}\n{}\n/approve {id} to start using it · /reject {id} to discard it",
                            skill.name, skill.description, skill.instructions
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Updated { skill, version }) => {
                        self.log(Level::Learn, format!("refined {} to v{version} · {tokens}", skill.name));
                        let text = format!(
                            "🔧 refined skill {} (v{version}):\n{}\n/history {} · /rollback {}",
                            skill.name, skill.instructions, skill.name, skill.name
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Proposed(p)) => {
                        let id = learn::short_id(p.id);
                        self.log(Level::Learn, format!("proposal: {} · {tokens}", p.change.kind()));
                        let detail = match &p.change {
                            lyra_learning::proposal::Change::Update { instructions, .. } => instructions.clone(),
                            _ => String::new(),
                        };
                        let text = format!(
                            "🔧 proposed a refinement: {}\n{detail}\n/approve {id} to apply it · /reject {id} to discard it",
                            p.reason
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Ignored(why)) => self.log(Level::Learn, format!("no lesson: {why} · {tokens}")),
                    Err(e) => self.log(Level::Error, format!("learning review failed: {e} · {tokens}")),
                }
                self.refresh_skills();
            }
            StreamEvent::Curated(Review { outcome, usage }) => {
                self.curating = false;
                self.totals.add_review(usage.as_ref());
                match outcome {
                    Ok(notes) => {
                        for note in &notes {
                            self.log(Level::Learn, format!("curator: {note}"));
                        }
                        let summary = if notes.is_empty() {
                            "🧹 curated the skills: nothing to change".to_string()
                        } else {
                            format!("🧹 curated the skills:\n{}\n/skills to review", notes.join("\n"))
                        };
                        self.messages.push(Message::new("info", summary));
                    }
                    Err(e) => self.log(Level::Error, format!("curation failed: {e} · {}", usage_text(usage.as_ref()))),
                }
                self.refresh_skills();
            }
            StreamEvent::LearnNotes(notes) => {
                for note in notes {
                    self.log(Level::Learn, note);
                }
                self.refresh_skills();
            }
            StreamEvent::Skills(snapshot) => {
                // Skills are capabilities too.
                if let (Ok(s), Some(caps)) = (&snapshot, &self.caps)
                    && s.active.len() != caps.manager.all().iter().filter(|c| c.kind == lyra_capabilities::CapabilityKind::Skill).count()
                {
                    self.refresh_caps(false);
                }
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("skills: {e}"));
                }
                self.skills = Some(snapshot);
            }
            StreamEvent::MemoriesApplied { ids, tokens } => {
                self.log(Level::Memory, format!("recalled {} memor{}: {}", ids.len(), if ids.len() == 1 { "y" } else { "ies" }, ids.join(" ")));
                self.applied_memories = ids;
                self.applied_memories_tokens = tokens;
            }
            StreamEvent::MemoryReview { curation, review: Review { outcome, usage } } => {
                if curation {
                    self.memory_curating = false;
                } else {
                    self.capturing = false;
                }
                self.totals.add_review(usage.as_ref());
                let what = if curation { "memory curation" } else { "memory capture" };
                match outcome {
                    Ok(notes) => {
                        if notes.is_empty() {
                            self.log(Level::Memory, format!("{what}: nothing to change · {}", usage_text(usage.as_ref())));
                        }
                        for note in &notes {
                            self.log(Level::Memory, note.clone());
                        }
                        if curation {
                            let text = if notes.is_empty() {
                                "🧠 curated memories: nothing to change".to_string()
                            } else {
                                format!("🧠 curated memories:\n{}\n/memory to review", notes.join("\n"))
                            };
                            self.messages.push(Message::new("info", text));
                        }
                    }
                    Err(e) => self.log(Level::Error, format!("{what} failed: {e} · {}", usage_text(usage.as_ref()))),
                }
                self.refresh_memory();
            }
            StreamEvent::PlanEvent(e) => {
                self.log(Level::Plan, format!("{}: {}", e.kind, e.message));
                self.reload_plan(e.plan_id);
            }
            StreamEvent::PlanCreated(result) => {
                self.plan_busy = false;
                match result {
                    Ok((goal, plan)) => {
                        let mut text = plan::describe(Some(&goal), &plan);
                        text += "\n/plan run to start · /plan cancel to drop it";
                        self.log(Level::Plan, format!("planned {} steps for: {}", plan.steps.len(), goal.description));
                        self.messages.push(Message::new("info", text));
                        self.current_goal = Some(goal);
                        self.current_plan = Some(plan);
                    }
                    Err(e) => {
                        self.log(Level::Error, format!("planning failed: {e}"));
                        self.messages.push(Message::new("error", format!("couldn't plan that: {e}")));
                    }
                }
            }
            StreamEvent::PlanFinished(result) => {
                self.plan_busy = false;
                self.set_phase(Phase::Idle);
                match result {
                    Ok(outcome) => self.plan_finished(outcome),
                    Err(e) => {
                        self.log(Level::Error, format!("plan run failed: {e}"));
                        self.messages.push(Message::new("error", format!("plan run failed: {e}")));
                    }
                }
            }
            StreamEvent::PlanNotes(notes) => {
                for note in notes {
                    self.log(Level::Plan, note);
                }
            }
            StreamEvent::MemoryRelated(Review { outcome, usage }) => {
                if usage.is_some() {
                    self.totals.add_review(usage.as_ref());
                }
                match outcome {
                    Ok(notes) => {
                        for note in notes {
                            self.log(Level::Memory, note);
                        }
                    }
                    Err(e) => self.log(Level::Error, format!("relating a memory failed: {e}")),
                }
                self.refresh_memory();
            }
            StreamEvent::MemoryNotes(notes) => {
                for note in notes {
                    self.log(Level::Memory, note);
                }
                self.refresh_memory();
            }
            StreamEvent::Evolved { done, show } => {
                self.evolving = None;
                for usage in &done.usage {
                    self.totals.add_review(Some(usage));
                }
                for note in &done.notes {
                    let level = if note.starts_with('✗') || note.contains("failed") { Level::Error } else { Level::Evolve };
                    self.log(level, note.clone());
                }
                if show && !done.notes.is_empty() {
                    self.messages.push(Message::new("info", format!("🧬 {}", done.notes.join("\n"))));
                }
                self.tools_changed();
                self.refresh_evolution();
                self.refresh_skills();
            }
            StreamEvent::GoalNotes { notes, show } => {
                self.goals_busy = false;
                for note in &notes {
                    self.log(Level::Plan, note.clone());
                }
                if show && !notes.is_empty() {
                    self.messages.push(Message::new("info", format!("🎯 {}", notes.join("\n"))));
                }
                self.refresh_goals();
            }
            StreamEvent::GoalPlan { result, autonomous } => match result {
                Ok((goal, plan)) => {
                    if autonomous {
                        self.autonomous_plans.insert(plan.id);
                    }
                    let by = if autonomous { "autonomously " } else { "" };
                    self.log(Level::Plan, format!("working {by}on goal with plan {}", lyra_execution::short(plan.id)));
                    let mut text = plan::describe(Some(&goal), &plan);
                    text += "\nrunning it now · /plan cancel stops it";
                    self.messages.push(Message::new("info", text));
                    self.current_goal = Some(goal);
                    self.current_plan = Some(plan);
                    self.refresh_goals();
                }
                Err(e) => {
                    self.plan_busy = false;
                    self.set_phase(Phase::Idle);
                    self.log(Level::Error, e.clone());
                    self.messages.push(Message::new("error", e));
                    self.refresh_goals();
                }
            },
            StreamEvent::Goals(snapshot) => self.goals_panel = Some(snapshot),
            StreamEvent::CapNotes(notes) => {
                for note in notes {
                    self.log(Level::Tool, note);
                }
                self.tools_changed();
            }
            StreamEvent::Evolution(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("evolution: {e}"));
                }
                self.evolution_panel = Some(snapshot);
            }
            StreamEvent::Memory(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("memory: {e}"));
                }
                self.memory = Some(snapshot);
            }
        }
    }

    fn log(&mut self, level: Level, text: String) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.activity.push(Activity { time, level, text });
        self.logged += 1;
        if self.activity.len() > MAX_ACTIVITY {
            self.activity.remove(0);
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase;
            self.phase_since = Instant::now();
        }
    }

    /// Log what was loaded at startup and kick off background checks.
    fn start(&mut self) {
        self.log_context();
        match self.memory_status.clone() {
            Ok(path) if self.tools.is_some() => self.log(Level::Info, format!("memory · {path}")),
            Ok(off) => self.log(Level::Info, off),
            Err(line) => self.log(Level::Error, line),
        }
        match self.learning_status.clone() {
            Ok(line) => self.log(Level::Info, line),
            Err(line) => self.log(Level::Error, line),
        }
        match self.planning_status.clone() {
            Ok(line) => self.log(Level::Info, line),
            Err(line) => self.log(Level::Error, line),
        }
        match self.evolution_status.clone() {
            Ok(line) => self.log(Level::Info, line),
            Err(line) => self.log(Level::Error, line),
        }
        match self.agents_status.clone() {
            Ok(line) => self.log(Level::Agent, line),
            Err(line) => self.log(Level::Error, line),
        }
        self.refresh_agents();
        self.sync_agents();
        self.reload_evolution();
        self.check_models();
        self.refresh_caps(true);
        self.refresh_goals();
        self.memory_upkeep();
        self.recover_plans();
        self.sync_skills();
        self.scheduled();
    }

    /// Run whatever scheduled maintenance is due: at startup, and every few
    /// minutes while lyra is open (a long session still gets its daily runs).
    fn scheduled(&mut self) {
        self.last_schedule_check = Instant::now();
        if self.evolving.is_none()
            && let Some(evolution) = &self.evolution
            && evolution.review_due(self.evolution_review_every)
        {
            self.log(Level::Evolve, "scheduled evolution review is due".into());
            self.evolve_in_background("reviewing", false, evolve::review);
        }
        if !self.memory_curating
            && let Some(mem) = self.mem()
            && mem.manager.settings().maintenance != lyra_memory::MaintenanceMode::Off
            && mem.curation_due(self.memory_curate_every)
        {
            self.log(Level::Memory, "scheduled memory curation is due".into());
            self.memory_curate();
        }
        if let Some(learning) = &self.learning
            && learning.mode() != Mode::Off
            && learning.curation_due(self.curate_every)
        {
            self.log(Level::Learn, "scheduled curation is due".into());
            self.curate();
        }
    }

    /// Version skill files that are new or were edited by hand, then refresh the panel.
    fn sync_skills(&self) {
        let Some(learning) = self.learning.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let notes = learning.sync().unwrap_or_else(|e| vec![format!("skill sync failed: {e}")]);
            let _ = tx.send(StreamEvent::LearnNotes(notes));
        });
    }

    /// Read the user's new message as feedback on the skills the last reply
    /// used: a correction is a failure, thanks a success.
    fn judge_last_run(&mut self, message: &str) {
        self.corrected_skills.clear();
        let Some(last) = self.last_run.take() else { return };
        let Some(outcome) = evaluator::outcome_signal(message) else { return };
        self.judge_delegations(last.id, outcome, message);
        if let Some(evo) = last.evo {
            self.record_evolution_outcome(evo, outcome, Some(message));
        }
        // Were the memories in that reply's prompt helpful? (M13)
        if last.memories > 0
            && let Some(mem) = self.mem()
        {
            let (tx, helpful) = (self.tx.clone(), outcome == SkillOutcome::Success);
            thread::spawn(move || {
                let note = match mem.feedback(last.id, helpful) {
                    Ok(n) => format!("{n} memor{} marked {}", if n == 1 { "y" } else { "ies" }, if helpful { "helpful" } else { "not helpful" }),
                    Err(e) => format!("memory feedback failed: {e}"),
                };
                let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
            });
        }
        let (run, skills) = (last.id, last.skills);
        let Some(learning) = self.learning.clone() else { return };
        if skills.is_empty() {
            return;
        }
        if outcome == SkillOutcome::Failure {
            self.corrected_skills = skills;
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            let notes = learning
                .record_outcome(run, outcome, false)
                .unwrap_or_else(|e| vec![format!("recording outcome failed: {e}")]);
            let _ = tx.send(StreamEvent::LearnNotes(notes));
        });
    }

    /// Curate the skill collection in the background.
    fn curate(&mut self) {
        let Some(learning) = self.learning.clone() else { return };
        if self.curating {
            return;
        }
        self.curating = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Curated(learning.curate(&url, &model, None)));
        });
    }

    /// Re-read the skills panel's contents in the background.
    fn refresh_skills(&self) {
        let Some(learning) = self.learning.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Skills(learning.snapshot()));
        });
    }

    /// The model-visible conversation as (role, text) pairs, with tool calls spelled out.
    fn history_text(&self) -> Vec<(&str, String)> {
        self.messages
            .iter()
            .filter(|m| m.is_history())
            .map(|m| {
                let mut text = m.content.clone();
                for call in &m.tool_calls {
                    text += &format!("\n→ {} {}", call.function.name, call.function.arguments);
                }
                (m.role.as_str(), text)
            })
            .collect()
    }

    /// Review the conversation for a reusable lesson in the background: after
    /// every turn when a trigger fires, or on /learn (`forced`).
    fn review(&mut self, forced: bool) {
        let Some(learning) = self.learning.clone() else { return };
        if learning.mode() == Mode::Off || self.reviewing {
            return;
        }
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let trigger = if forced {
            Some("the user asked to review this conversation for a lesson")
        } else {
            learn::last_turn(&history, self.corrected_skills.len()).and_then(|turn| evaluator::trigger(&turn))
        };
        let Some(trigger) = trigger else { return };
        let mut transcript = learn::transcript(&history, 16);
        if !self.corrected_skills.is_empty() {
            transcript += &format!(
                "\n\n[skills that were in the prompt for the corrected answer] {}",
                self.corrected_skills.join(", ")
            );
        }
        self.log(Level::Learn, format!("reviewing for a lesson: {trigger}"));
        self.reviewing = true;

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, run) = (self.model.clone(), self.tx.clone(), self.last_run.as_ref().map(|r| r.id));
        thread::spawn(move || {
            let review = learning.review(&url, &model, trigger, &transcript, run);
            let _ = tx.send(StreamEvent::Reviewed(review));
        });
    }

    /// Handle a `/command` typed in the input box.
    fn command(&mut self, line: &str) {
        let (name, arg) = line.split_once(' ').unwrap_or((line, ""));
        let learning = self.learning.clone();
        let need = || -> Result<Arc<Learning>, String> {
            learning.clone().ok_or_else(|| match &self.learning_status {
                Err(why) => why.clone(),
                Ok(_) => "learning is off".into(),
            })
        };
        let last_run = self.last_run.clone();
        let result = match name {
            "/help" => Ok(format!("{COMMANDS}\n{}\n{}\n{}\n{}\n{HELP_END}", goals::COMMANDS, agents::COMMANDS, evolve::COMMANDS, caps::COMMANDS)),
            "/skills" => need().and_then(|l| l.describe()),
            "/approve" => need().and_then(|l| l.approve(arg)),
            "/reject" => need().and_then(|l| l.reject(arg)),
            "/deprecate" => need().and_then(|l| l.deprecate(arg)),
            "/forget-skill" => need().and_then(|l| l.forget(arg)),
            "/history" => need().and_then(|l| l.history(arg)),
            "/rollback" => need().and_then(|l| l.rollback(arg)),
            "/outcome" => (|| {
                let outcome = match arg.trim() {
                    "good" | "success" => SkillOutcome::Success,
                    "bad" | "failure" => SkillOutcome::Failure,
                    "partial" => SkillOutcome::Partial,
                    _ => return Err("usage: /outcome good|bad|partial".into()),
                };
                let last = last_run.ok_or("no reply to rate yet")?;
                let mut out = Vec::new();
                if let Some(evo) = last.evo {
                    self.record_evolution_outcome(evo, outcome, None);
                    out.push(format!("recorded {outcome} for the last run"));
                }
                if let (Some(l), false) = (&learning, last.skills.is_empty()) {
                    let notes = l.record_outcome(last.id, outcome, true)?;
                    out.push(format!("recorded {outcome} for {}", last.skills.join(", ")));
                    out.extend(notes);
                }
                if out.is_empty() {
                    return Err("nothing to record it for: the last reply used no skills and evolution is off".into());
                }
                Ok(out.join("\n"))
            })(),
            "/learn" | "/curate" => need().and_then(|l| {
                if l.mode() == Mode::Off {
                    Err("learning mode is off ([learning] mode in config.toml)".into())
                } else if name == "/learn" && self.reviewing {
                    Err("a review is already running".into())
                } else if name == "/curate" && self.curating {
                    Err("a curation is already running".into())
                } else if name == "/learn" {
                    Ok("reviewing the conversation for a lesson…".into())
                } else {
                    Ok("curating the skill collection…".into())
                }
            }),
            "/plan" | "/plans" => self.plan_command(name, arg),
            "/evolve" => self.evolve_command(arg),
            "/caps" => self.caps_command(arg),
            "/goals" | "/goal" => self.goals_command(name, arg),
            "/agents" | "/agent" => self.agents_command(name, arg),
            "/sessions" => sessions::dir().ok_or("no home directory".to_string()).map(|d| {
                format!("this one: {}\n{}\n\nlyra -c continues the latest here · /resume <id> or lyra -r <id> resumes one", self.session_id, sessions::describe(&sessions::list(&d), 20))
            }),
            "/resume" => self.resume(arg),
            "/new" => self.new_session(),
            "/machines" => self.machines_command(arg),
            "/devices" => self.devices_command(arg),
            "/memory" => {
                let mem = self.mem().ok_or_else(|| match &self.memory_status {
                    Err(why) => why.clone(),
                    Ok(_) => "memory is off".to_string(),
                });
                mem.and_then(|m| match arg.split_whitespace().next().unwrap_or("") {
                    "curate" if self.memory_curating => Err("a memory curation is already running".into()),
                    "curate" => Ok("curating memories…".into()),
                    "episode" if self.capturing => Err("a memory capture is already running".into()),
                    "episode" => Ok("recording this conversation as an episode…".into()),
                    "reembed" => Ok("re-embedding memories in the background…".into()),
                    "project" => {
                        let name = arg.split_whitespace().nth(1);
                        match name {
                            None => Ok(m.project().map_or("no current project ([memory] project)".into(), |p| format!("current project: {p} (scope project:{p})"))),
                            Some("none") => {
                                m.set_project(None);
                                Ok("no current project: every project's memories stay out of prompts".into())
                            }
                            Some(p) => {
                                m.set_project(Some(p.to_string()));
                                Ok(format!("current project: {p} — its memories (project:{p}) are recalled, other projects' aren't"))
                            }
                        }
                    }
                    _ => m.command(arg),
                })
            }
            _ => Err(format!("unknown command {name} — try /help")),
        };
        let ok = result.is_ok();
        let (role, text) = match result {
            Ok(text) => ("info", text),
            Err(e) => ("error", e),
        };
        // A resumed conversation already says so.
        if !(ok && matches!(name, "/resume" | "/new") && text.is_empty()) {
            self.messages.push(Message::new(role, format!("> {line}\n{text}")));
        }
        let memory_sub = arg.split_whitespace().next().unwrap_or("");
        match name {
            "/memory" if ok && memory_sub == "curate" => self.memory_curate(),
            "/memory" if ok && memory_sub == "episode" => self.capture(true),
            "/memory" if ok && memory_sub == "reembed" => {
                if let Some(mem) = self.mem() {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let mut notes = mem.backfill();
                        if notes.is_empty() {
                            notes.push("every memory already has a current vector".into());
                        }
                        let _ = tx.send(StreamEvent::MemoryNotes(notes));
                    });
                }
            }
            "/memory" if ok && matches!(memory_sub, "forget" | "archive" | "restore" | "purge" | "correct" | "approve" | "reject" | "working" | "project" | "backup") => {
                self.log(Level::Memory, line.to_string());
                self.refresh_memory();
            }
            "/learn" if ok => self.review(true),
            "/curate" if ok => self.curate(),
            "/approve" | "/reject" | "/deprecate" | "/forget-skill" | "/rollback" | "/outcome" if ok => {
                self.refresh_evolution();
                self.log(Level::Learn, line.to_string());
                self.refresh_skills();
            }
            _ => {}
        }
    }

    fn log_context(&mut self) {
        let text = match self.context_files.len() {
            0 => "no SOUL.md / USER.md / AGENT.md found".to_string(),
            n => format!("loaded {n} context file{}", if n == 1 { "" } else { "s" }),
        };
        self.log(Level::Info, text);
    }

    /// Re-read the memory panel's numbers in the background.
    fn refresh_memory(&self) {
        let Some(mem) = self.mem() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Memory(mem.snapshot()));
        });
    }

    fn mem(&self) -> Option<Arc<Mem>> {
        self.tools.as_ref().map(|t| t.mem.clone())
    }

    /// Relate a memory the model just saved to similar ones, in the background.
    fn relate_memory(&self, short_id: String) {
        let Some(mem) = self.mem() else { return };
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        thread::spawn(move || {
            let review = match mem.run(mem.manager.find(&short_id)) {
                Ok(m) => mem.relate(&url, &model, m.id),
                Err(e) => Review { outcome: Err(e), usage: None },
            };
            let _ = tx.send(StreamEvent::MemoryRelated(review));
        });
    }

    /// Archive expired memories and add missing vectors, in the background.
    fn memory_upkeep(&self) {
        let Some(mem) = self.mem() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::MemoryNotes(mem.upkeep()));
        });
    }

    /// Review the turn just finished for things worth remembering (M2, M8):
    /// after every turn when the capture trigger fires, or on `/memory episode`.
    fn capture(&mut self, episode: bool) {
        let Some(mem) = self.mem() else { return };
        if self.capturing || (!episode && mem.manager.settings().capture == CaptureMode::Off) {
            return;
        }
        let Some(start) = self.messages.iter().rposition(|m| m.role == "user") else { return };
        let calls: Vec<&str> =
            self.messages[start..].iter().flat_map(|m| m.tool_calls.iter().map(|c| c.function.name.as_str())).collect();
        let user = self.messages[start].content.clone();
        let turn = capture::Turn {
            user: &user,
            task_tool_calls: calls.iter().filter(|n| !n.starts_with("memory_") && **n != "working_memory").count(),
            model_saved: calls.iter().any(|n| matches!(*n, "memory_remember" | "memory_correct" | "memory_supersede")),
        };
        let reason = if episode {
            Some("the user asked to record this conversation as an episode; summarize it in \"episode\"")
        } else {
            capture::trigger(&turn)
        };
        let Some(reason) = reason else { return };
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let transcript = learn::transcript(&history, if episode { 30 } else { 8 });
        let reply = self.messages.iter().rev().find(|m| m.role == "assistant").map_or("", |m| m.content.as_str());
        let query = format!("{user}\n{reply}");
        self.log(Level::Memory, format!("capturing: {reason}"));
        self.capturing = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, run) = (self.model.clone(), self.tx.clone(), self.last_run.as_ref().map(|r| r.id));
        let started = self.session_started;
        thread::spawn(move || {
            let review = mem.capture(&url, &model, reason, &transcript, &query, run, Some(started));
            let _ = tx.send(StreamEvent::MemoryReview { curation: false, review });
        });
    }

    /// The plan runtime for autonomous work: the autonomy guard applies (G11).
    fn autonomous_runtime(&self) -> LyraRuntime {
        let mut rt = self.runtime();
        rt.guard = self.goals.as_ref().map(|g| g.guard());
        rt
    }

    fn runtime(&self) -> LyraRuntime {
        LyraRuntime {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            forbidden_tools: self.forbidden_tools.clone(),
            evolution: self.evolution.clone(),
            caps: self.caps.clone(),
            guard: None,
            tx: self.tx.clone(),
        }
    }

    fn reload_plan(&mut self, id: Uuid) {
        let Some(engine) = &self.engine else { return };
        if let Ok(Some(plan)) = engine.plan(id) {
            // The goal's status changes as the plan runs, so read it each time.
            self.current_goal = engine.goal(plan.goal_id).ok().flatten().map(|(g, _)| g);
            self.current_plan = Some(plan);
        }
    }

    /// Find plans interrupted by a restart (P8) and show the latest plan.
    fn recover_plans(&mut self) {
        let Some(engine) = self.engine.clone() else { return };
        if let Ok(Some(latest)) = engine.plans(1).map(|p| p.into_iter().next()) {
            self.reload_plan(latest.id);
        }
        let (rt, tx) = (self.runtime(), self.tx.clone());
        thread::spawn(move || {
            let notes = engine.recover(&rt).unwrap_or_else(|e| vec![format!("plan recovery failed: {e}")]);
            let _ = tx.send(StreamEvent::PlanNotes(notes));
        });
    }

    /// `/plan …` and `/plans`.
    fn plan_command(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let engine = self.engine.clone().ok_or_else(|| match &self.planning_status {
            Err(why) => why.clone(),
            Ok(_) => "planning is off".to_string(),
        })?;
        if name == "/plans" {
            return plan::list(&engine);
        }
        let anyway = arg.split_whitespace().any(|w| w == "anyway");
        let words: Vec<&str> = arg.split_whitespace().filter(|w| *w != "anyway").collect();
        let is_id = |w: &str| w.len() >= 4 && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        // `/plan run` etc. are commands; anything else is a new request.
        let target = |rest: &[&str]| -> Result<Plan, String> {
            match rest.iter().find(|w| is_id(w)) {
                Some(id) => engine.find(id),
                None => self.current_plan.clone().ok_or_else(|| "no plan yet — /plan <what you want done>".to_string()),
            }
        };
        let command = match words.first().copied() {
            None => Some("show"),
            Some(w @ ("show" | "run" | "resume" | "cancel" | "events" | "checkpoints" | "revisions")) if words.len() <= 2 && words[1..].iter().all(|x| is_id(x)) => Some(w),
            Some("budget") if words.len() <= 3 => Some("budget"),
            Some(w @ ("approve" | "retry" | "skip")) if (2..=3).contains(&words.len()) => Some(w),
            _ => None,
        };
        match command {
            Some("show") => {
                let plan = target(&words)?;
                let goal = engine.goal(plan.goal_id)?.map(|(g, _)| g);
                Ok(plan::describe(goal.as_ref(), &plan))
            }
            Some("events") => plan::events(&engine, &target(&words[1..])?),
            Some("checkpoints") => plan::checkpoints(&engine, &target(&words[1..])?),
            Some("revisions") => plan::revisions(&engine, &target(&words[1..])?),
            Some("budget") => {
                let plan = target(&words[1..])?;
                if words.get(1) == Some(&"raise") {
                    // Add the configured budget again on top of the current one.
                    let note = engine.raise_budget(plan.id, &engine.settings.budget)?;
                    self.reload_plan(plan.id);
                    Ok(format!("{note} — /plan resume to continue"))
                } else {
                    Ok(format!("budget: {} — /plan budget raise adds the configured budget again", lyra_execution::budget::describe(&plan.budget, &plan.usage)))
                }
            }
            Some("run" | "resume") => {
                if self.plan_busy {
                    return Err("a plan is already being created or run".into());
                }
                let plan = target(&words[1..])?;
                if plan.status.is_finished() {
                    return Err(format!("plan {} is already {}", lyra_execution::short(plan.id), plan.status));
                }
                // P1: open questions are asked before anything runs.
                if plan.status == PlanStatus::Draft
                    && !anyway
                    && let Some((goal, _)) = engine.goal(plan.goal_id)?
                    && !goal.ambiguities.is_empty()
                {
                    return Err(format!(
                        "the goal has open questions:\n- {}\nanswer them with a clearer /plan <request>, or /plan run anyway",
                        goal.ambiguities.join("\n- ")
                    ));
                }
                self.plan_busy = true;
                self.set_phase(Phase::Tools(format!("plan {}", lyra_execution::short(plan.id))));
                let (rt, tx, id) = (self.runtime(), self.tx.clone(), plan.id);
                thread::spawn(move || {
                    let _ = tx.send(StreamEvent::PlanFinished(engine.run(id, &rt)));
                });
                Ok(format!("running plan {}…", lyra_execution::short(id)))
            }
            Some("cancel") => {
                let plan = target(&words[1..])?;
                let note = engine.cancel(plan.id, &self.runtime())?;
                self.reload_plan(plan.id);
                Ok(note)
            }
            Some(w @ ("approve" | "retry" | "skip")) => {
                let plan = target(&words[2..])?;
                let note = match w {
                    "approve" => engine.approve(plan.id, words[1], &self.runtime())?,
                    "retry" => engine.retry_step(plan.id, words[1])?,
                    _ => engine.skip_step(plan.id, words[1])?,
                };
                self.reload_plan(plan.id);
                Ok(format!("{note} — /plan resume to continue"))
            }
            _ => {
                if self.plan_busy {
                    return Err("a plan is already being created or run".into());
                }
                self.plan_busy = true;
                let (rt, tx, request) = (self.runtime(), self.tx.clone(), arg.trim().to_string());
                thread::spawn(move || {
                    let _ = tx.send(StreamEvent::PlanCreated(engine.create(&request, &rt)));
                });
                Ok("working out the goal and a plan…".into())
            }
        }
    }

    /// A plan run ended. Report it, and when it finished, hand the evidence to
    /// memory (P18) and skills (P19): the planner never writes either directly.
    fn plan_finished(&mut self, outcome: RunOutcome) {
        self.goal_plan_finished(&outcome);
        let plan = outcome.plan;
        self.reload_plan(plan.id);
        let goal = self.current_goal.clone();
        let summary = outcome.evaluation.as_ref().map(|e| e.summary.clone()).unwrap_or_default();
        let answer = outcome.evaluation.as_ref().map(|e| e.answer.clone()).filter(|a| !a.trim().is_empty());
        let mut text = match (plan.status, outcome.goal_status) {
            (PlanStatus::Paused, _) => format!("⏸ plan paused: {}", plan.note.clone().unwrap_or_default()),
            (_, Some(status)) => {
                let icon = match status {
                    GoalStatus::Completed => "🎯",
                    GoalStatus::Partial => "◐",
                    _ => "✗",
                };
                let mut t = format!("{icon} goal {status}");
                if !summary.is_empty() {
                    t += &format!(": {summary}");
                }
                if let Some(e) = &outcome.evaluation {
                    for c in &e.criteria {
                        t += &format!("\n  {} {}", if c.met { "✓" } else { "✗" }, c.criterion);
                    }
                }
                if let Some(a) = &answer {
                    t += &format!("\n\n{a}");
                }
                t
            }
            _ => format!("plan {}", plan.status),
        };
        text += &format!("\n\n{}", plan::describe(None, &plan));
        self.messages.push(Message::new("info", text));
        self.log(Level::Plan, format!("plan {} {}", lyra_execution::short(plan.id), plan.status));
        if !plan.status.is_finished() {
            return;
        }
        let Some(engine) = self.engine.clone() else { return };
        let goal_text = goal.as_ref().map_or(String::new(), |g| g.description.clone());
        let evo = self.record_plan_run(&engine, &plan, &goal_text, outcome.goal_status);
        // The user's next message is feedback on the plan, for evolution.
        self.last_run = Some(LastRun { id: plan.id, skills: Vec::new(), memories: 0, evo });
        // P18: the outcome becomes an episode (linked to the plan, with its
        // real start), and durable discoveries are offered to memory, all
        // through the memory manager.
        if let Some(mem) = self.mem() {
            let tx = self.tx.clone();
            let (scope, status) = (mem.manager.settings().default_scope.clone(), outcome.goal_status);
            let episode_summary = format!("Plan: {goal_text}. {summary}");
            let episode_outcome = format!("goal {}{}", status.map_or("unknown".into(), |s| s.to_string()), answer.as_ref().map_or(String::new(), |a| format!(": {}", a.chars().take(300).collect::<String>())));
            let entities: Vec<String> = mem.working().entities.iter().cloned().collect();
            let (plan_id, started) = (plan.id, plan.created_at);
            let transcript = format!("Goal: {goal_text}\n\n{}", plan::results(&plan));
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let model = self.model.clone();
            thread::spawn(move || {
                let note = match mem.run(mem.manager.add_episode(&scope, &episode_summary, &episode_outcome, entities, Some(plan_id), Some(started))) {
                    Ok(m) => format!("recorded plan episode [{}]", m.short_id()),
                    Err(e) => format!("plan episode not recorded: {e}"),
                };
                let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
                let reason = "a plan finished; keep the durable facts it discovered or changed (state, configuration, decisions), not the steps themselves";
                let review = mem.capture(&url, &model, reason, &transcript, &goal_text_for(&transcript), Some(plan_id), Some(started));
                let _ = tx.send(StreamEvent::MemoryReview { curation: false, review });
            });
            self.capturing = true;
        }
        // P19: recoveries are lessons; let the skill reviewer look at them.
        let metrics = engine.metrics(&plan).unwrap_or_default();
        if (metrics.recoveries > 0 || metrics.replans > 0)
            && let Some(learning) = self.learning.clone()
            && learning.mode() != Mode::Off
            && !self.reviewing
        {
            self.reviewing = true;
            let transcript = format!("Goal: {goal_text}\n\n{}", plan::events(&engine, &plan).unwrap_or_default());
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let (model, tx) = (self.model.clone(), self.tx.clone());
            self.log(Level::Learn, "reviewing the plan's recoveries for a lesson".into());
            thread::spawn(move || {
                let review = learning.review(&url, &model, "a failed step was replaced by a working one (plan execution)", &transcript, None);
                let _ = tx.send(StreamEvent::Reviewed(review));
            });
        }
    }

    // ---- evolution

    fn evolution_env(&self) -> Option<evolve::Env> {
        Some(evolve::Env {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            evolution: self.evolution.clone()?,
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            caps: self.caps.clone(),
            goals: self.goals.clone(),
            system_prompt: self.system_prompt.clone(),
        })
    }

    /// Run evolution work on a background thread; one piece at a time.
    fn evolve_in_background(&mut self, what: &'static str, show: bool, work: impl FnOnce(&evolve::Env) -> evolve::Done + Send + 'static) {
        let Some(env) = self.evolution_env() else { return };
        self.evolving = Some(what);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Evolved { done: work(&env), show });
        });
    }

    /// Re-read the evolved state (behavior, workflows, composite tools).
    fn reload_evolution(&mut self) {
        let Some(evolution) = self.evolution.clone() else { return };
        for note in evolution.reload(self.tools.as_deref()) {
            self.log(Level::Error, format!("evolution: {note}"));
        }
        // Evolved workflows and composite tools are capabilities (C11).
        self.refresh_caps(false);
        self.tools_changed();
        self.refresh_evolution();
    }

    /// The tool list may have changed (composite tools): recount it.
    fn tools_changed(&mut self) {
        // What the model is offered for an empty query: everything when few,
        // otherwise memory_recall and the search tool (discovery adds the rest).
        let definitions = match (&self.caps, &self.tools) {
            (Some(caps), _) => Some(Value::Array(caps.definitions("", &Default::default()))),
            (None, Some(tools)) => Some(tools.definitions()),
            _ => None,
        };
        self.tools_tokens = definitions.as_ref().map_or(0, |d| learn::approx_tokens(&d.to_string()));
        self.tool_count = definitions.as_ref().and_then(|d| d.as_array().map(Vec::len)).unwrap_or(0);
    }

    /// Rebuild the capability registry (skills, workflows or tools changed)
    /// and, with `health`, check every provider. In the background.
    fn refresh_caps(&self, health: bool) {
        let Some(caps) = self.caps.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut notes = caps.refresh();
            if health {
                notes.extend(caps.check_health());
            }
            let _ = tx.send(StreamEvent::CapNotes(notes));
        });
    }

    // ---- goals (docs/done/goal_manager.md)

    fn refresh_goals(&self) {
        let Some(goals) = self.goals.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Goals(goals.snapshot()));
        });
    }

    /// A plan ended or paused: if it was for a goal, the goal's progress,
    /// status and (for autonomous work) the session's spending follow (G5).
    fn goal_plan_finished(&mut self, outcome: &RunOutcome) {
        let (Some(goals), Some(engine)) = (self.goals.clone(), self.engine.clone()) else { return };
        let plan = &outcome.plan;
        if self.autonomous_plans.contains(&plan.id) {
            let m = engine.metrics(plan).unwrap_or_default();
            let cost = self.pricing.cost(m.tokens, 0, 0);
            goals.spent(m.model_calls, m.tool_calls, m.replans, cost);
            if plan.status.is_finished() {
                self.autonomous_plans.remove(&plan.id);
            }
        }
        match goals.plan_finished(outcome, plan.usage.tokens) {
            Ok(notes) if !notes.is_empty() => {
                for note in &notes {
                    self.log(Level::Plan, note.clone());
                }
                self.messages.push(Message::new("info", notes.join("\n")));
            }
            Ok(_) => {}
            Err(e) => self.log(Level::Error, format!("goal update failed: {e}")),
        }
        if let Ok(Some(gp)) = goals.manager.plan_goal(plan.id) {
            self.goal_episode(gp.goal_id);
        }
        self.refresh_goals();
    }

    /// A goal that ended becomes a memory episode (goal events feed Memory).
    fn goal_episode(&self, id: Uuid) {
        let (Some(goals), Some(mem)) = (self.goals.clone(), self.mem()) else { return };
        let Ok(Some(g)) = goals.manager.get(id) else { return };
        if !matches!(g.status, lyra_goals::GoalStatus::Completed | lyra_goals::GoalStatus::Failed) {
            return;
        }
        let scope = mem.manager.settings().default_scope.clone();
        let summary = format!("Goal: {}. {}", g.title, g.progress_detail.summary);
        let outcome = format!("goal {} after {} plan(s)", g.status, goals.manager.plans(g.id).map(|p| p.len()).unwrap_or(0));
        let tx = self.tx.clone();
        thread::spawn(move || {
            let note = match mem.run(mem.manager.add_episode(&scope, &summary, &outcome, Vec::new(), None, Some(g.created_at))) {
                Ok(m) => format!("recorded goal episode [{}]", m.short_id()),
                Err(e) => format!("goal episode not recorded: {e}"),
            };
            let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
        });
    }

    /// Plan and work on a goal now (G4): a new plan toward it, run right away.
    fn work_goal(&mut self, key: &str, autonomous: bool) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        let engine = self.engine.clone().ok_or("planning is off, and goals are worked through plans")?;
        if self.plan_busy {
            return Err("a plan is already being created or run".into());
        }
        let g = goals.manager.find(key)?;
        if matches!(g.status, lyra_goals::GoalStatus::Proposed | lyra_goals::GoalStatus::Paused) && !autonomous {
            goals.manager.set_status(g.id, lyra_goals::GoalStatus::Active, "the user started work on it")?;
        }
        let g = goals.manager.get(g.id)?.ok_or("goal vanished")?;
        if let Err((kind, why)) = goals.manager.can_progress(&g) {
            return Err(format!("{} can't progress: {} ({})", g.title, why, kind.as_str()));
        }
        let request = goals.request(&g)?;
        self.plan_busy = true;
        self.set_phase(Phase::Tools(format!("goal {}", g.short())));
        let rt = if autonomous { self.autonomous_runtime() } else { self.runtime() };
        let (tx, base) = (self.tx.clone(), engine.settings.budget);
        let title = g.title.clone();
        thread::spawn(move || match engine.create(&request, &rt) {
            Ok((plan_goal, plan)) => {
                let _ = goals.manager.link_plan(g.id, plan.id, autonomous);
                if autonomous {
                    let _ = engine.set_budget(plan.id, goals.remaining_budget(base));
                }
                let plan = engine.plan(plan.id).ok().flatten().unwrap_or(plan);
                let _ = tx.send(StreamEvent::GoalPlan { result: Ok((plan_goal, plan.clone())), autonomous });
                let _ = tx.send(StreamEvent::PlanFinished(engine.run(plan.id, &rt)));
            }
            Err(e) => {
                // A goal that can't even be planned needs information first.
                if autonomous {
                    let _ = goals.manager.block(g.id, lyra_goals::BlockerType::MissingInformation, &format!("couldn't plan it: {e}"));
                }
                let _ = tx.send(StreamEvent::GoalPlan { result: Err(format!("couldn't plan {title}: {e}")), autonomous });
            }
        });
        Ok(format!("working on {} — planning…", g.title))
    }

    /// Run in the background: break a goal into subgoals (G3).
    fn decompose_goal(&mut self, key: &str) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        let g = goals.manager.find(key)?;
        if self.goals_busy {
            return Err("a goal decomposition or review is already running".into());
        }
        self.goals_busy = true;
        // What lyra can do, so subgoals are things it can actually work on.
        let mut context: Vec<String> = Vec::new();
        if let Some(caps) = &self.caps {
            let (found, _) = caps.search(&serde_json::json!({ "query": format!("{} {}", g.title, g.description) }).to_string());
            context.push(format!("capabilities: {found}"));
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, title) = (self.model.clone(), self.tx.clone(), g.title.clone());
        thread::spawn(move || {
            let prompt = lyra_goals::prompts::decompose_prompt(&g, &context.join("\n"));
            let notes = learn::complete(&url, &model, lyra_goals::prompts::DECOMPOSE_PROMPT, &prompt)
                .and_then(|(reply, _)| lyra_goals::prompts::parse_decomposition(&reply, &g))
                .and_then(|subs| goals.manager.decompose(g.id, subs))
                .map(|kids| {
                    let mut notes = vec![format!("{title} → {} subgoals:", kids.len())];
                    notes.extend(kids.iter().map(|k| {
                        let waits = if k.dependencies.is_empty() { String::new() } else { format!(" (after {})", k.dependencies.len()) };
                        format!("  {} {}{waits}", k.short(), k.title)
                    }));
                    notes
                })
                .unwrap_or_else(|e| vec![format!("decomposing {title} failed: {e}")]);
            let _ = tx.send(StreamEvent::GoalNotes { notes, show: true });
        });
        Ok("breaking the goal into subgoals…".into())
    }

    /// Run in the background: review the goal list (G12). In autonomous mode
    /// the suggestions are applied; otherwise they wait for `/goals review apply`.
    fn review_goals(&mut self) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        if self.goals_busy {
            return Err("a goal decomposition or review is already running".into());
        }
        let all = goals.manager.all()?;
        if all.iter().filter(|g| g.status.is_open()).count() < 2 {
            return Ok("fewer than two open goals: nothing to tidy".into());
        }
        self.goals_busy = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        thread::spawn(move || {
            let stale: Vec<lyra_goals::Uuid> = goals.manager.stale().unwrap_or_default().iter().map(|g| g.id).collect();
            let prompt = lyra_goals::prompts::review_prompt(&all, &stale);
            let notes = match learn::complete(&url, &model, lyra_goals::prompts::REVIEW_PROMPT, &prompt).and_then(|(r, _)| lyra_goals::prompts::parse_review(&r)) {
                Ok(review) => {
                    for g in all.iter().filter(|g| g.status.is_open()) {
                        let _ = goals.manager.note_review(g.id, "reviewed");
                    }
                    let text = goals::describe_review(&goals, &review);
                    if goals.mode() == lyra_goals::AutonomyMode::Autonomous && !review.is_empty() {
                        let mut notes = vec!["goal review (applied):".to_string()];
                        notes.extend(goals::apply_review(&goals, &review));
                        notes
                    } else {
                        let empty = review.is_empty();
                        *goals.review.lock().unwrap_or_else(|e| e.into_inner()) = Some(review);
                        let mut notes = vec![format!("goal review:\n{text}")];
                        if !empty {
                            notes.push("/goals review apply to make these changes".into());
                        }
                        notes
                    }
                }
                Err(e) => vec![format!("goal review failed: {e}")],
            };
            let _ = tx.send(StreamEvent::GoalNotes { notes, show: true });
        });
        Ok("reviewing the goals…".into())
    }

    /// `/goals …` and `/goal …`
    fn goals_command(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off ([goals] enabled)")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        let result = match (name, sub) {
            ("/goals", "") => goals::list(&goals, false),
            ("/goals", "all") => goals::list(&goals, true),
            ("/goals", "next") => goals::next(&goals),
            ("/goals", "review") if rest == "apply" => {
                let review = goals.review.lock().unwrap_or_else(|e| e.into_inner()).take();
                match review {
                    Some(r) => Ok(goals::apply_review(&goals, &r).join("\n")),
                    None => Err("no review waiting: /goals review".into()),
                }
            }
            ("/goals", "review") => self.review_goals(),
            ("/goals", "autonomy") => {
                if !rest.is_empty() {
                    let mode: lyra_goals::AutonomyMode = serde_json::from_value(serde_json::json!(rest))
                        .map_err(|_| "usage: /goals autonomy reactive|assisted|autonomous")?;
                    goals.set_mode(mode);
                    self.autonomy_note.clear();
                    self.log(Level::Plan, format!("autonomy: {}", mode.as_str()));
                }
                let p = &goals.manager.settings.autonomy;
                let s = goals.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                Ok(format!(
                    "autonomy {} · per session: {} min, {} plans, {} tool / {} model calls, {} replans, up to {} risk\nthis session: {} plans, {} tool / {} model calls{}",
                    goals.mode().as_str(),
                    p.max_runtime_minutes,
                    p.max_plans,
                    p.max_tool_calls,
                    p.max_model_calls,
                    p.max_replans,
                    p.max_risk,
                    s.plans,
                    s.tool_calls,
                    s.model_calls,
                    s.stopped.as_ref().map_or(String::new(), |(_, why)| format!(" · stopped: {why}"))
                ))
            }
            ("/goal", "new") => goals::create(&goals, rest),
            ("/goal", "work") => self.work_goal(rest, false),
            ("/goal", "decompose") => self.decompose_goal(rest),
            ("/goal", "") => goals::list(&goals, false),
            ("/goal", s)
                if matches!(
                    s,
                    "activate" | "resume" | "pause" | "cancel" | "complete" | "fail" | "unblock" | "priority" | "importance" | "due"
                        | "criteria" | "depends" | "block" | "when"
                ) =>
            {
                let out = goals::edit(&goals, s, rest);
                if out.is_ok() && matches!(s, "complete" | "fail")
                    && let Ok(g) = goals.manager.find(rest.split_whitespace().next().unwrap_or(""))
                {
                    self.goal_episode(g.id);
                }
                out
            }
            ("/goal", _) => goals::show(&goals, arg.trim()),
            _ => Err(goals::COMMANDS.into()),
        };
        self.refresh_goals();
        result
    }

    /// The goal loop (G8–G11), every few seconds while idle: fire triggers,
    /// lift blockers whose cause went away, resume interrupted plans, and,
    /// within the autonomy policy, work on the next goal.
    fn goals_tick(&mut self) {
        self.goals_checked = Instant::now();
        let Some(goals) = self.goals.clone() else { return };
        let caps = self.caps.clone();
        let mut notes = goals.manager.fire_triggers(chrono::Utc::now(), &|c| goals::condition(c, caps.as_deref())).unwrap_or_default();
        let mode = goals.mode();
        // Blockers whose cause went away.
        if let Some(engine) = self.engine.clone() {
            for b in goals.manager.blockers(None, true).unwrap_or_default() {
                let lift = match b.blocker_type {
                    lyra_goals::BlockerType::ApprovalRequired => goals
                        .manager
                        .plans(b.goal_id)
                        .ok()
                        .and_then(|p| p.last().cloned())
                        .and_then(|p| engine.plan(p.plan_id).ok().flatten())
                        .is_some_and(|plan| !plan.steps.iter().any(|s| s.needs_approval())),
                    lyra_goals::BlockerType::CapabilityUnavailable => caps.as_ref().is_some_and(|c| {
                        c.manager.all().iter().all(|x| c.manager.health(x) != lyra_capabilities::CapabilityHealth::Unavailable)
                    }),
                    _ => false,
                };
                if lift && let Ok(g) = goals.manager.unblock(b.goal_id, &format!("{} resolved", b.blocker_type.as_str())) {
                    notes.push(format!("goal {} unblocked: {} resolved", g.title, b.blocker_type.as_str()));
                }
            }
            // G8: plans for goals that were interrupted or approved resume (not in reactive mode).
            if mode != lyra_goals::AutonomyMode::Reactive && !self.plan_busy {
                for gp in goals.manager.open_plans().unwrap_or_default() {
                    let Ok(Some(plan)) = engine.plan(gp.plan_id) else { continue };
                    let waiting = plan.steps.iter().any(|s| s.needs_approval() || s.status == lyra_execution::StepStatus::Blocked);
                    let goal_ok = goals.manager.get(gp.goal_id).ok().flatten().is_some_and(|g| g.status == lyra_goals::GoalStatus::Active);
                    if plan.status == PlanStatus::Paused && !waiting && goal_ok {
                        notes.push(format!("resuming plan {} for its goal", lyra_execution::short(plan.id)));
                        self.plan_busy = true;
                        self.autonomous_plans.insert(plan.id);
                        let (rt, tx) = (self.autonomous_runtime(), self.tx.clone());
                        thread::spawn(move || {
                            let _ = tx.send(StreamEvent::PlanFinished(engine.run(plan.id, &rt)));
                        });
                        break;
                    }
                }
            }
        }
        for note in &notes {
            self.log(Level::Plan, note.clone());
        }
        if !notes.is_empty() {
            self.refresh_goals();
        }
        // G11: autonomous work, within the policy.
        if mode == lyra_goals::AutonomyMode::Reactive || self.plan_busy || self.engine.is_none() {
            return;
        }
        let note = match goals.may_work(goals.manager.settings.autonomy.cooldown_minutes) {
            Err(why) => why,
            Ok(()) => match goals.manager.next(mode == lyra_goals::AutonomyMode::Assisted) {
                Ok(Some((g, score))) => {
                    self.log(Level::Plan, format!("autonomy picked {} (score {:.2})", g.title, score.total));
                    match self.work_goal(&g.id.to_string(), true) {
                        Ok(text) => text,
                        Err(e) => e,
                    }
                }
                Ok(None) => "nothing to work on".into(),
                Err(e) => e,
            },
        };
        if note != self.autonomy_note {
            self.log(Level::Plan, format!("autonomy: {note}"));
            self.autonomy_note = note;
        }
    }

    /// `/caps …`
    fn caps_command(&mut self, arg: &str) -> Result<String, String> {
        let caps = self.caps.clone().ok_or("capabilities are off")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        match sub {
            "" | "list" => Ok(caps::list(&caps)),
            "search" => caps::search(&caps, rest),
            "show" => caps::show(&caps, rest),
            "allow" => {
                let c = caps.manager.allow(rest.trim())?;
                self.log(Level::Tool, format!("allowed {} for this session", c.id));
                Ok(format!("{} may run without approval for the rest of this session", c.id))
            }
            "health" => {
                self.refresh_caps(true);
                Ok("checking every provider…".into())
            }
            _ => Err(format!("unknown /caps command {sub:?}\n{}", caps::COMMANDS)),
        }
    }

    fn refresh_evolution(&self) {
        let Some(evolution) = self.evolution.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Evolution(evolution.snapshot()));
        });
    }

    /// E1: record the turn just answered (or failed) for evolution. Returns its id.
    fn record_chat_run(&mut self, stats: &Stats, error: Option<&str>) -> Option<Uuid> {
        let evolution = self.evolution.clone()?;
        let start = self.messages.iter().rposition(|m| m.role == "user")?;
        let turn = &self.messages[start..];
        let mut run = lyra_evolution::RunRecord::new(lyra_evolution::RunKind::Chat, &turn[0].content, 0);
        run.model_calls = turn.iter().filter(|m| m.role == "assistant").count().max(1) as u32;
        run.tools_used = turn.iter().flat_map(|m| m.tool_calls.iter().map(|c| c.function.name.clone())).collect();
        run.tool_calls = run.tools_used.len() as u32;
        run.errors = turn
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| serde_json::from_str::<Value>(&m.content).ok()?.get("error")?.as_str().map(str::to_string))
            .collect();
        run.retries = run.errors.len() as u32;
        run.tokens = stats.input + stats.output;
        run.duration_ms = stats.elapsed.as_millis() as u64;
        run.skills_used = self.applied_skills.iter().map(|s| s.trim_end_matches(" (trial)").to_string()).collect();
        if let Some(e) = error {
            run.errors.push(e.chars().take(300).collect());
            run.outcome = lyra_evolution::RunOutcome::Failure;
        }
        let id = run.id;
        let tx = self.tx.clone();
        thread::spawn(move || {
            if let Err(e) = evolution.record(run) {
                let _ = tx.send(StreamEvent::Log(format!("recording the run failed: {e}")));
            }
            let _ = tx.send(StreamEvent::Evolution(evolution.snapshot()));
        });
        Some(id)
    }

    /// The user's reaction tells how a run went; then check for a regression.
    fn record_evolution_outcome(&self, id: Uuid, outcome: SkillOutcome, feedback: Option<&str>) {
        let Some(env) = self.evolution_env() else { return };
        let (outcome, corrected) = match outcome {
            SkillOutcome::Success => (lyra_evolution::RunOutcome::Success, false),
            SkillOutcome::Partial => (lyra_evolution::RunOutcome::Partial, false),
            SkillOutcome::Failure => (lyra_evolution::RunOutcome::Failure, true),
            SkillOutcome::Unknown => return,
        };
        let (tx, feedback) = (self.tx.clone(), feedback.map(str::to_string));
        thread::spawn(move || {
            let mut notes = Vec::new();
            if let Err(e) = env.evolution.manager.set_outcome(id, outcome, corrected, feedback.as_deref()) {
                notes.push(format!("recording the outcome failed: {e}"));
            }
            notes.extend(evolve::monitor(&env));
            let show = notes.iter().any(|n| n.starts_with('⚠') || n.starts_with('↩'));
            let _ = tx.send(StreamEvent::Evolved { done: evolve::Done { notes, usage: Vec::new() }, show });
        });
    }

    /// E1: record a finished plan run, its outcome being the goal's status.
    /// Returns its telemetry id.
    fn record_plan_run(&self, engine: &Engine, plan: &Plan, goal: &str, status: Option<GoalStatus>) -> Option<Uuid> {
        let evolution = self.evolution.clone()?;
        let m = engine.metrics(plan).unwrap_or_default();
        let mut run = lyra_evolution::RunRecord::new(lyra_evolution::RunKind::Plan, goal, 0);
        run.model_calls = m.model_calls;
        run.tool_calls = m.tool_calls;
        run.tokens = m.tokens;
        run.retries = m.retries;
        run.replans = m.replans;
        run.failed_attempts = m.failed_attempts as u32;
        run.recoveries = m.recoveries as u32;
        run.verification_failures = m.verification_failures as u32;
        run.duration_ms = m.seconds * 1000;
        // Every tool called, inside reasoning and subagent steps too.
        run.tools_used = Engine::tools_used(plan);
        // Learned procedures the plan followed.
        run.skills_used = plan
            .steps
            .iter()
            .filter_map(|s| match &s.action {
                lyra_execution::StepAction::Workflow { workflow, .. } => Some(workflow.clone()),
                _ => None,
            })
            .collect();
        // Every error along the way, including ones later recovered from.
        run.errors = engine.errors(plan).unwrap_or_default();
        run.outcome = match status {
            Some(GoalStatus::Completed) => lyra_evolution::RunOutcome::Success,
            Some(GoalStatus::Partial) => lyra_evolution::RunOutcome::Partial,
            Some(_) => lyra_evolution::RunOutcome::Failure,
            None => lyra_evolution::RunOutcome::Unknown,
        };
        let env = self.evolution_env();
        let (tx, id) = (self.tx.clone(), run.id);
        thread::spawn(move || {
            let mut notes = Vec::new();
            if let Err(e) = evolution.record(run) {
                notes.push(format!("recording the plan run failed: {e}"));
            }
            if let Some(env) = env {
                notes.extend(evolve::monitor(&env));
            }
            let show = notes.iter().any(|n| n.starts_with('⚠') || n.starts_with('↩'));
            let _ = tx.send(StreamEvent::Evolved { done: evolve::Done { notes, usage: Vec::new() }, show });
        });
        Some(id)
    }

    /// `/evolve …`
    fn evolve_command(&mut self, arg: &str) -> Result<String, String> {
        let evolution = self.evolution.clone().ok_or_else(|| match &self.evolution_status {
            Err(why) => why.clone(),
            Ok(_) => "evolution is off".to_string(),
        })?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim().to_string();
        let background = matches!(sub, "review" | "test" | "compare" | "approve" | "rollback" | "code");
        if background && let Some(what) = self.evolving {
            return Err(format!("evolution is busy ({what}); try again when it's done"));
        }
        match sub {
            "" | "status" => evolve::status(&evolution),
            "list" | "candidates" => evolve::list(&evolution),
            "show" => evolve::show(&evolution, &rest),
            "reject" => {
                let note = evolve::reject(&evolution, &rest)?;
                self.refresh_evolution();
                Ok(note)
            }
            "generations" => evolve::generations(&evolution),
            "history" => evolve::history(&evolution),
            "runs" => evolve::runs(&evolution),
            "review" => {
                if evolution.mode() == lyra_evolution::Mode::Off {
                    return Err("evolution mode is off ([evolution] mode in config.toml); runs are still recorded".into());
                }
                self.evolve_in_background("reviewing", true, evolve::review);
                Ok("looking for problems in recent runs…".into())
            }
            "test" => {
                let c = evolution.manager.find(&rest)?;
                self.evolve_in_background("testing", true, move |env| evolve::test(env, c).into());
                Ok("testing the candidate (static checks, then a sandboxed benchmark)…".into())
            }
            "compare" => {
                evolution.manager.find(&rest)?;
                self.evolve_in_background("comparing", true, move |env| evolve::compare(env, &rest));
                Ok("testing every open candidate for that problem and ranking them…".into())
            }
            "approve" => {
                evolution.manager.find(rest.split_whitespace().next().unwrap_or(""))?;
                self.evolve_in_background("deploying", true, move |env| evolve::approve(env, &rest));
                Ok("applying the change…".into())
            }
            "rollback" => {
                self.evolve_in_background("rolling back", true, move |env| evolve::rollback(env, &rest));
                Ok("rolling back…".into())
            }
            "code" => {
                if rest.is_empty() {
                    return Err("usage: /evolve code <the problem to fix>".into());
                }
                self.evolve_in_background("writing a patch", true, move |env| evolve::propose_code(env, &rest));
                Ok("asking the model for a source patch (it only becomes a candidate)…".into())
            }
            _ => Err(format!("unknown /evolve command {sub:?}\n{}", evolve::COMMANDS)),
        }
    }

    /// Curate the memory collection in the background (M9, M18).
    fn memory_curate(&mut self) {
        let Some(mem) = self.mem() else { return };
        if self.memory_curating {
            return;
        }
        self.memory_curating = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        thread::spawn(move || {
            let review = mem.curate(&url, &model);
            let _ = tx.send(StreamEvent::MemoryReview { curation: true, review });
        });
    }

    /// Re-read the context files and config.toml; takes effect on the next request.
    fn reload(&mut self) {
        let context = Context::load();
        self.system_prompt = system_prompt(&context, self.tools.is_some());
        self.context_files = context.files();
        self.log(Level::Info, "reloaded config and context files".into());
        self.log_context();
        match Config::load() {
            Ok(config) => {
                self.base_url = config.url.clone();
                self.model = config.model.clone();
                learn::configure(learn::Structured {
                    max_tokens: config.structured_max_tokens,
                    thinking: config.structured_thinking,
                });
                self.pricing = pricing(&config);
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

    /// Ping the embedding and reranker models in the background.
    fn check_models(&mut self) {
        self.model_status = None;
        let (embedding, reranker) = (self.embedding.clone(), self.reranker.clone());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let results = retrieval::check(embedding.as_ref(), reranker.as_ref());
            let _ = tx.send(StreamEvent::Models(results));
        });
    }

    fn scroll_up(&mut self, n: u16) {
        let top = self.scroll.unwrap_or(self.max_scroll);
        self.scroll = Some(top.saturating_sub(n));
    }

    fn scroll_down(&mut self, n: u16) {
        if let Some(top) = self.scroll {
            let top = top.saturating_add(n);
            self.scroll = (top < self.max_scroll).then_some(top);
        }
    }

    /// The assistant message being streamed into, created on the first token.
    /// Save this conversation (after every reply and on the way out).
    fn save_session(&mut self) {
        if !self.messages.iter().any(|m| m.role == "user") {
            return;
        }
        let Some(dir) = sessions::dir() else { return };
        let s = sessions::Session::from_messages(&self.session_id, self.session_started, &self.messages);
        if let Err(e) = sessions::save(&dir, &s) {
            self.log(Level::Error, format!("couldn't save the session: {e}"));
        }
    }

    /// Carry on a saved conversation (`lyra -c`, `lyra -r <id>`, `/resume <id>`).
    fn resume_session(&mut self, s: sessions::Session) {
        let (id, turns, when) = (s.id.clone(), s.user_turns(), s.updated.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
        self.session_id = id.clone();
        self.session_started = s.started;
        self.messages = s.into_messages();
        self.agent_cards.clear();
        self.messages.push(Message::new("info", format!("resumed session {id} · {turns} turns · last active {when}")));
        self.scroll = None;
        self.last_run = None;
        self.log(Level::Info, format!("resumed session {id} ({turns} turns)"));
    }

    /// `/new`: start a fresh conversation (this one is saved first).
    fn new_session(&mut self) -> Result<String, String> {
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        self.save_session();
        self.session_id = sessions::new_id();
        self.session_started = chrono::Utc::now();
        self.messages.clear();
        self.agent_cards.clear();
        self.last_run = None;
        self.scroll = None;
        self.log(Level::Info, format!("new session {}", self.session_id));
        Ok(String::new())
    }

    /// `/resume <id>`: switch to a saved conversation (this one is saved first).
    fn resume(&mut self, arg: &str) -> Result<String, String> {
        if self.waiting {
            return Err("wait for the reply to finish".into());
        }
        let dir = sessions::dir().ok_or("no home directory")?;
        if arg.trim().is_empty() {
            return Ok(format!("{}\n\n/resume <id> to switch", sessions::describe(&sessions::list(&dir), 20)));
        }
        let s = sessions::find(&dir, arg)?;
        if s.id == self.session_id {
            return Err("that's this session".into());
        }
        self.save_session();
        self.resume_session(s);
        Ok(String::new())
    }

    /// Another conversation next to this one (`lyra serve`: one per device or
    /// as asked), sharing memory, skills, agents, tools and the server.
    fn fork(&self) -> App {
        let config = Config::load().unwrap_or_default();
        let mut app = App::new(config, Context::load(), self.shared.clone());
        app.hub = self.hub.clone();
        app.primary = false;
        app.refresh_agents();
        app
    }

    /// Stop the reply being written (and any agent working for it).
    pub(crate) fn stop(&mut self) -> Result<String, String> {
        if !self.waiting {
            return Err("nothing is running".into());
        }
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        // An agent waiting for a yes gets a no, so it can stop too.
        let pending: Vec<u64> = self.approvals.iter().map(|r| r.id).collect();
        for id in pending {
            self.answer_approval_id(id, "n");
        }
        self.log(Level::Info, "stopping…".into());
        Ok("stopping…".into())
    }

    /// A message further back changed (an agent's card): devices must hear.
    fn touch(&mut self, i: usize) {
        self.touched = Some(self.touched.map_or(i, |t| t.min(i)));
    }

    /// What the command palette shows for the input (nothing when closed).
    fn palette_entries(&self) -> Vec<&'static commands::Entry> {
        if self.palette_hidden { Vec::new() } else { commands::matching(&self.input) }
    }

    /// Fill in a palette entry; one that takes no arguments is sent right away.
    fn complete_command(&mut self, e: &commands::Entry) {
        let text = commands::completion(e);
        let send = !text.ends_with(' ') && text == self.input.trim();
        self.input = text;
        self.palette = 0;
        if send {
            self.send();
        }
    }

    fn reply(&mut self) -> &mut Message {
        if self.messages.last().is_none_or(|m| m.role != "assistant") {
            self.messages.push(Message::new("assistant", String::new()));
        }
        self.messages.last_mut().unwrap()
    }
}

pub(crate) const COMMANDS: &str = "\
/skills                      skills and changes waiting for review
/approve <id>                apply a proposal, or (re)activate a skill
/reject <id>                 discard a proposal or proposed skill for good
/deprecate <id>              stop using a skill without deleting it
/forget-skill <id>           delete a skill's file (its history is kept)
/stop                        stop the reply being written (also Ctrl-X)
/new                         start a new conversation (this one is saved)
/machines [update|remove <name>]  machines lyra works on (lyra-node): online, version, update, remove
/devices [approve|deny <code>]    paired phones, browsers, terminals and machines; pairing requests
/devices remove <name>       unpair a device or machine
/sessions                    saved conversations (lyra -c continues the latest)
/resume <id>                 switch to a saved conversation
/history <id>                a skill's versions and audit trail
/rollback <id> [version]     restore an earlier version (the previous one by default)
/outcome good|bad|partial    how the last reply's skills worked out
/learn                       review the conversation for a lesson now
/curate                      look for duplicates, conflicts and stale skills now
/memory                      memory stats and what's waiting for approval
/memory search <query>       recall with scores · /memory list [scope]
/memory inspect <id>         a memory's details, versions, links and history
/memory correct <id> <text>  fix a memory (keeps the old version)
/memory forget|archive|restore|purge <id>
/memory approve|reject <id>  act on a proposed consolidation or archive
/memory working [clear]      show or clear working memory
/memory events               what happened to memories lately (created, superseded, linked, …)
/memory reembed              give every memory a vector from the current embedding model
/memory backup               copy the memory store to ~/.lyra/backup (restore: lyra --restore-memory <dir>)
/memory curate               consolidate duplicates, flag contradictions now
/memory episode              record this conversation as an episode
/memory project [name|none]  the current project (its memories are recalled, others' aren't)
/plan <request>              turn a request into a goal and a structured plan
/plan [id] · /plans          show the current (or a) plan · list plans
/plan run|resume [id]        execute it (pauses for approvals and budgets)
/plan approve <step>         approve a step's action (needed again if it changes)
/plan retry|skip <step>      after a failure or an interrupted step
/plan cancel · /plan events  stop it · what happened, with metrics
/plan run anyway             run despite the goal's open questions
/plan budget [raise]         the plan's budget · add the configured budget again
/plan checkpoints|revisions  recovery points · how the plan changed
/caps [search|show|allow|health]  what lyra can do, which capability fits, what's allowed
/outcome good|bad|partial    (also) how the last reply went, for evolution";

pub(crate) const HELP_END: &str = "/help                        this list";

/// The first line of a transcript (the goal), as the query for similar memories.
fn goal_text_for(transcript: &str) -> String {
    transcript.lines().next().unwrap_or("").trim_start_matches("Goal: ").to_string()
}

/// `1,234 tokens`, or that the server didn't say.
fn usage_text(usage: Option<&stats::Usage>) -> String {
    usage.map_or("usage unknown".into(), |u| format!("{} tokens", u.prompt_tokens + u.completion_tokens))
}

/// Add the skills that match the user's message to the system prompt, and
/// record their use for this run.
fn apply_skills(
    learning: &Learning,
    message: &str,
    run: Uuid,
    history: &mut Vec<Value>,
    tx: &Sender<StreamEvent>,
) {
    match learning.relevant(message) {
        Ok(skills) if !skills.is_empty() => {
            let section = learn::prompt_section(&skills);
            let ids: Vec<Uuid> = skills.iter().map(|r| r.skill.id).collect();
            if let Err(e) = learning.record_usage(run, &ids) {
                let _ = tx.send(StreamEvent::Log(format!("recording skill use failed: {e}")));
            }
            let names = skills
                .into_iter()
                .map(|r| if r.trial { format!("{} (trial)", r.skill.name) } else { r.skill.name })
                .collect();
            let tokens = learn::approx_tokens(&section);
            let _ = tx.send(StreamEvent::SkillsApplied { names, tokens });
            add_to_system(history, &section);
        }
        Ok(_) => {}
        Err(e) => {
            let _ = tx.send(StreamEvent::Log(format!("skill search failed: {e}")));
        }
    }
}

/// Append a section to the system message, adding one if there isn't one.
fn add_to_system(history: &mut Vec<Value>, section: &str) {
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
fn apply_memories(mem: &Mem, message: &str, run: Uuid, history: &mut Vec<Value>, tx: &Sender<StreamEvent>) {
    mem.working().note_entities(message);
    let mut sections = Vec::new();
    match mem.compile(message, run) {
        Ok(compiled) => {
            if let Some(section) = compiled.section {
                let ids = compiled.used.iter().map(|r| r.memory.short_id()).collect();
                let _ = tx.send(StreamEvent::MemoriesApplied { ids, tokens: compiled.tokens as u64 });
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
fn system_prompt(context: &Context, memory: bool) -> Option<String> {
    let parts: Vec<String> = context
        .system_prompt()
        .into_iter()
        .chain(memory.then(|| tools::MEMORY_PROMPT.to_string()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn pricing(config: &Config) -> Pricing {
    Pricing {
        input_per_mtok: config.input_cost_per_mtok,
        cached_per_mtok: config.cached_input_cost_per_mtok.unwrap_or(config.input_cost_per_mtok),
        output_per_mtok: config.output_cost_per_mtok,
        currency: config.currency.clone(),
    }
}

/// One user turn: stream a reply; if the model calls tools, run them, add the
/// results to the history and stream again. Stats cover the whole turn.
#[allow(clippy::too_many_arguments)]
fn converse(
    url: &str,
    model: &str,
    mut history: Vec<Value>,
    caps: Option<&Caps>,
    agents: Option<&agents::Env>,
    request: &str,
    max_rounds: usize,
    run: Uuid,
    tx: &Sender<StreamEvent>,
    cancel: &Cancel,
) -> Result<Stats, String> {
    let start = Instant::now();
    let mut total: Option<Stats> = None;
    let finish = |total: Option<Stats>| {
        let mut stats = total.unwrap_or(Stats { ttft: None, elapsed: Duration::ZERO, input: 0, cached: 0, output: 0, estimated: true });
        stats.elapsed = start.elapsed();
        stats
    };
    // Capabilities the model found with the search tool, offered from then on.
    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            let mut definitions = caps.definitions(request, &found);
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
            return Ok(finish(total));
        }
        let round = stream(url, &body, tx, cancel)?;
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        let Some(caps) = caps.filter(|_| !round.tool_calls.is_empty() && !round.stopped) else {
            return Ok(finish(total));
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
            let ctx = CallContext::new(Some(run), &call.id);
            // Policy, usage tracking and verification happen in there.
            let content = if let (Some(env), "delegate") = (agents, call.function.name.as_str()) {
                agents::delegate_call(env, &call.function.arguments, run)
            } else if call.function.name == caps::SEARCH_TOOL {
                let (text, names) = caps.search(&call.function.arguments);
                found.extend(names);
                text
            } else {
                caps.invoke(&call.function.name, &call.function.arguments, ctx, false, true)
            };
            history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            let (id, name) = (call.id.clone(), call.function.name.clone());
            tx.send(StreamEvent::ToolResult { id, name, content }).map_err(|e| e.to_string())?;
        }
    }
    Err(format!("stopped after {max_rounds} rounds of tool calls"))
}

/// POST the request, forward each delta as it arrives, and measure the reply.
fn stream(url: &str, body: &Value, tx: &Sender<StreamEvent>, cancel: &Cancel) -> Result<Round, String> {
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
    let resp = client.post(url).json(body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
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

const USAGE: &str = "\
lyra — a terminal chat client for a local LLM

usage: lyra [command] [options]

  serve                   run for phones and browsers (no terminal UI; see [web] in the config)
  pair                    a code to pair a phone or browser with `lyra serve`
  devices [remove <name>] paired devices
  service                 install a systemd user service that runs `lyra serve`
  node [pair|service]     let a lyra server work on this machine (see lyra node --help)
  connect [--pair <code>] the terminal UI for a lyra server (see lyra connect --help);
                          plain `lyra` opens it on a machine that has no lyra of its own

  -c, --continue          continue the latest conversation (started in this folder, else any)
  -r, --resume [id]       resume a saved conversation; without an id, list them
  --restore-memory <dir>  put a memory backup in place, then exit
  --force                 start even if another lyra (TUI or serve) is using the same home
  -h, --help              this help

Conversations are saved in ~/.lyra/sessions/ ($LYRA_HOME/sessions).";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_none_or(|a| a != "node" && a != "connect") && args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let sub = args.get(1).map(String::as_str);
    match sub {
        Some("pair") => return pair_command(),
        Some("devices") => return devices_command(&args[2..]),
        Some("service") => return service_command(),
        Some("node") => return lyra_node::main(&args[2..]),
        Some("connect") => return connect::main(&args[2..]),
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
                (Some(key), Some(dir)) => match sessions::find(&dir, key) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!("lyra: {e}");
                        std::process::exit(1);
                    }
                },
                (None, dir) => {
                    let all = dir.map(|d| sessions::list(&d)).unwrap_or_default();
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
    learn::configure(learn::Structured {
        max_tokens: config.structured_max_tokens,
        thinking: config.structured_thinking,
    });
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
    let resume = resume.or_else(|| serving.then(|| sessions::dir().and_then(|d| sessions::list(&d).into_iter().next())).flatten());
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
fn serve_main(mut app: App, web: &lyra_web::Settings, rt: &tokio::runtime::Handle) {
    let Some(dir) = config::home().map(|h| h.join("web")) else {
        eprintln!("lyra: no home directory");
        std::process::exit(1);
    };
    let (tx, rx) = mpsc::channel();
    let hub = match lyra_web::Hub::start(rt, web, &dir, tx) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    };
    app.hub = Some(hub.clone());
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
fn pair_command() {
    let dir = config::home().map(|h| h.join("web")).expect("a home directory");
    match lyra_web::Devices::open(&dir).and_then(|d| d.new_code(10)) {
        Ok(code) => {
            let url = Config::load().map(|c| c.web.public_url).unwrap_or_default();
            println!("Pairing code: {}-{}", &code[..4], &code[4..]);
            println!("Valid for 10 minutes, once. Open {} on the device and enter it.", if url.is_empty() { "lyra's address" } else { &url });
        }
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    }
}

/// `lyra devices [remove <name|id>]`.
fn devices_command(args: &[String]) {
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
fn service_command() {
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
fn open_evolution(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<Evolution>>, Result<String, String>) {
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
fn open_planning(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<Engine>>, Result<String, String>) {
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
fn open_learning(
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
fn open_memory(
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
fn open_agents(config: &Config, runtime: &tokio::runtime::Handle) -> (Option<Arc<agents::Agents>>, Result<String, String>) {
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
            // System tools added since the Operator was installed from its template.
            if let Some(mut op) = a.registry.get("operator").filter(|p| p.template.as_deref() == Some("operator")) {
                let missing: Vec<String> = ["upload_place"].iter().filter(|t| !op.tools.iter().any(|x| x == *t)).map(|t| t.to_string()).collect();
                if !missing.is_empty() {
                    op.tools.extend(missing.iter().cloned());
                    let _ = a.registry.update(op, &format!("new system tools: {}", missing.join(", ")));
                }
            }
            (Some(Arc::new(a)), Ok(format!("agents · {}", context::show(&dir))))
        }
        Err(e) => (None, Err(format!("agents off: {e:#}"))),
    }
}

/// Open the goal store (`~/.lyra/goals/goals.db`), unless goals are off.
fn open_goals(config: &Config, runtime: &tokio::runtime::Handle) -> Option<Arc<Goals>> {
    if !config.goals.enabled {
        return None;
    }
    let path = config::home()?.join("goals").join("goals.db");
    lyra_goals::GoalManager::open(&path, runtime.clone(), config.goals.settings.clone()).ok().map(|m| Arc::new(Goals::new(m)))
}

/// Open the capability registry: usage history and discovery index in
/// `~/.lyra/capabilities`, providers from `[capabilities]`, plus the memory
/// tools, skills, workflows and helper agents. Returns notes for the log.
fn open_capabilities(
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
    if config.system.enabled {
        caps.system = Some(lyra_system::System::new(config.system.clone(), config::expand_path));
    }
    if config.search.enabled {
        caps.search = Some(config.search.clone());
    }
    notes.extend(caps.refresh());
    notes.push(format!("capabilities · {} ({} callable)", caps.manager.all().len(), caps.manager.all().iter().filter(|c| c.kind.callable()).count()));
    (Some(Arc::new(caps)), notes)
}

/// `lyra --restore-memory <backup>`: put a memory backup in place before
/// anything opens the store. The replaced store is kept next to it.
fn restore_memory(backup: &str) -> Result<String, String> {
    let config = Config::load()?;
    if config.memory.backend != config::MemoryBackend::Lance {
        return Err("restore works for the lance backend; for sqlite, copy the file back".into());
    }
    let path = config.memory.path().ok_or("no memory path")?;
    let kept = lyra_memory::restore(std::path::Path::new(backup), &path).map_err(|e| format!("{e:#}"))?;
    Ok(format!("restored memory from {backup}; the previous store is kept at {}", kept.display()))
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        // Drain everything the stream has produced since the last frame.
        while let Ok(event) = app.rx.try_recv() {
            app.handle(event);
        }
        if !app.waiting && app.last_schedule_check.elapsed() > Duration::from_secs(10 * 60) {
            app.scheduled();
        }
        if !app.waiting && !app.plan_busy && app.goals_checked.elapsed() > app.goals_every {
            app.goals_tick();
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // An agent waits for approval: y / n / a answer it straight away.
        if !app.approvals.is_empty()
            && app.input.is_empty()
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N' | 'a' | 'A')) = key.code
        {
            app.answer_approval(&c.to_string());
            continue;
        }
        // The command palette, while `/…` is being typed (↑↓ pick, Tab/Enter complete, Esc closes).
        let palette = app.palette_entries();
        if !palette.is_empty() {
            let n = palette.len();
            match key.code {
                KeyCode::Esc => {
                    app.palette_hidden = true;
                    continue;
                }
                KeyCode::Up => {
                    app.palette = (app.palette + n - 1) % n;
                    continue;
                }
                KeyCode::Down => {
                    app.palette = (app.palette + 1) % n;
                    continue;
                }
                KeyCode::Tab => {
                    app.complete_command(palette[app.palette.min(n - 1)]);
                    continue;
                }
                KeyCode::Enter if !app.input.contains(' ') && !commands::all().iter().any(|e| e.usage.split_whitespace().next() == Some(app.input.trim())) => {
                    app.complete_command(palette[app.palette.min(n - 1)]);
                    continue;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.show_reasoning = !app.show_reasoning;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => app.reload(),
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = app.stop();
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.show_panels = !app.show_panels;
            }
            KeyCode::Up => app.scroll_up(1),
            KeyCode::Down => app.scroll_down(1),
            KeyCode::PageUp => app.scroll_up(app.page),
            KeyCode::PageDown => app.scroll_down(app.page),
            KeyCode::Enter => app.send(),
            KeyCode::Backspace => {
                app.input.pop();
                app.palette = 0;
                app.palette_hidden = false;
            }
            KeyCode::Char(c) => {
                app.input.push(c);
                app.palette = 0;
                app.palette_hidden = false;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;
    use std::io::Write;

    /// A chat server that streams a token every 30 ms for ~3 s.
    fn slow_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let mut buf = [0u8; 65536];
            let _ = std::io::Read::read(&mut s, &mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for i in 0..100 {
                let chunk = json!({ "choices": [{ "delta": { "content": format!("w{i} ") } }] });
                if s.write_all(format!("data: {chunk}\n\n").as_bytes()).is_err() {
                    return; // the client hung up: stop generating
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            let _ = s.write_all(b"data: [DONE]\n\n");
        });
        url
    }

    #[test]
    fn stopping_ends_a_reply_partway() {
        let url = slow_server();
        let (tx, _rx) = mpsc::channel();
        let cancel = Cancel::default();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let start = Instant::now();
        let round = stream(&url, &json!({ "model": "m", "messages": [] }), &tx, &cancel).unwrap();
        assert!(round.stopped);
        assert!(round.content.starts_with("w0 "), "what arrived is kept: {:?}", round.content);
        assert!(!round.content.contains("w99"), "it didn't run to the end");
        assert!(start.elapsed() < Duration::from_secs(2), "it stopped promptly");
        assert!(round.tool_calls.is_empty());
    }
}
