mod acting;
mod activity;
mod app;
mod actions;
mod alerts;
mod avoid;
mod asks;
mod trouble;
mod retention;
mod sandbox;
mod mailout;
mod agents;
mod board;
mod backup;
mod branches;
mod briefing;
mod calendar;
mod caps;
mod changelog;
mod coding;
mod commands;
mod startup;
mod store;
mod turn;
mod config;
mod config_history;
mod config_schema;
mod connect;
mod context;
mod decide;
mod diagnose;
mod docx;
mod documents;
mod evolve;
mod feedback;
mod fallback;
mod files;
mod projects;
mod goals;
mod graph;
mod health;
mod markdown;
mod mcp_server;
mod learn;
mod mail;
mod meetings;
mod limits;
mod known_down;
mod lock;
mod mem;
mod outcomes;
mod patterns;
mod notes;
mod people;
mod plan;
mod planner;
mod tasks;
mod proactive;
mod migrate;
mod retrieval;
mod routines;
mod serve;
mod search;
mod secrets;
mod sessions;
mod shared;
mod settings;
mod stats;
mod style;
mod status;
mod teams;
mod text;
mod templates;
mod tools;
mod triggers;
mod qa;
mod recap;
mod usage;
mod watches;
mod vision;
mod websearch;
mod when;
mod ui;

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
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
use commands::{COMMANDS, HELP_END, shown};
use startup::*;
use turn::*;
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
    /// A reply's run, to rate or judge (and how it went), kept with the saved conversation.
    #[serde(skip)]
    run: Option<LastRun>,
    /// The content rendered as Markdown, and the content length it was
    /// rendered from (re-rendered when a streaming reply grows).
    #[serde(skip)]
    rendered: Option<(usize, Vec<ratatui::text::Line<'static>>)>,
    /// In a shared conversation: who said it (a user message), by name.
    #[serde(skip)]
    by: String,
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
            run: None,
            rendered: None,
            by: String::new(),
        }
    }

    /// What the model reads: in a shared conversation, with who said it.
    fn said(&self) -> String {
        if self.by.is_empty() { self.content.clone() } else { format!("{}: {}", self.by, self.content) }
    }

    /// The same message in another copy of a shared conversation (its run stays where it was made).
    fn copy(&self) -> Self {
        let mut m = Message::new(&self.role, self.content.clone());
        m.tool_calls = self.tool_calls.clone();
        m.tool_call_id = self.tool_call_id.clone();
        m.reasoning = self.reasoning.clone();
        m.memories = self.memories.clone();
        m.skills = self.skills.clone();
        m.agents = self.agents.clone();
        m.images = self.images.clone();
        m.by = self.by.clone();
        m
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
    /// The decision model found nothing for a memory capture (`memory`) or a
    /// skill review to look at.
    GateClosed { memory: bool },
    /// How a reply went, read from the person's next message (off the loop).
    Judged { run: Uuid, verdict: outcomes::Verdict },
    /// A routine or goal to offer (4.5): what to say, and the command that does it.
    Suggest(patterns::Offer),
    /// A backup finished (`nightly` ones only log).
    BackedUp { nightly: bool, result: backup::Made },
    /// The main agent is working with a subagent (routing, progress, result).
    Agent(agents::AgentEvent),
    /// An agent waits for the user's approval before changing something.
    Approval(agents::ApprovalRequest),
    /// lyra asks in the chat: a missing piece as a form, or a round's steps.
    Ask(asks::Request),
    /// That question was answered, or timed out: its card goes.
    AskDone(u64),
    /// A call's arguments after the user filled in what was missing.
    ToolArgs { id: String, arguments: String },
    Reasoning(String),
    /// The model asked to run these tools.
    ToolCalls(Vec<ToolCall>),
    /// A tool finished; `content` is what the model will see.
    ToolResult { id: String, name: String, content: String },
    Done(Stats),
    /// The reply stopped at its tool-call limit (after `Done`): it can be continued.
    Limit(usize),
    Error(String),
    /// Progress note from the worker for the activity log.
    Log(String),
    /// What the vision model read off a file the user attached: kept with their message.
    Seen(String),
    /// The conversation's oldest turns were summarized to fit the model's
    /// context: how many history messages the note now stands for, and the note.
    Compacted { upto: usize, summary: String },
    /// The goal loop's look, worked out off the loop.
    GoalsLooked { notes: Vec<String>, resume: Option<Uuid> },
    /// The browser sandbox's page after a step: shown under that call, never sent to the model.
    Picture { id: String, data: String },
    /// Something to say in the chat (a remote update finished, …).
    Notice(String),
    /// Health check results for the embedding/reranker models.
    Models(Vec<Result<String, String>>),
    /// Fresh numbers for the memory panel.
    Memory(Result<MemorySnapshot, String>),
    /// Memories the context compiler put in the prompt (short ids) and their size.
    MemoriesApplied { ids: Vec<String>, texts: Vec<String>, tokens: u64 },
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
type LastRun = outcomes::RunRef;

/// A skill review waiting for the one running: its turn (`key`, so one per
/// turn), why, the transcript as it was, whose.
struct QueuedReview {
    key: Uuid,
    trigger: Option<&'static str>,
    transcript: String,
    run: Option<Uuid>,
    owner: Option<String>,
    /// The turn as both post-turn gates read it (one question for the two).
    gate: String,
}

/// Reviews that may wait at once; more are dropped (the next turn brings its own).
const MAX_QUEUED_REVIEWS: usize = 5;

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
    /// The same moment on the clock, for devices that count the timer up themselves.
    phase_at: chrono::DateTime<chrono::Utc>,
    phase: Phase,
    messages: Vec<Message>,
    input: String,
    waiting: bool,
    /// The last reply stopped at its tool-call limit, or on an error: Continue
    /// (or Try again, when nothing was done) picks it up.
    can_continue: bool,
    /// It stopped on an error, not at the limit.
    continue_after_error: bool,
    /// What lyra asks in the chat, waiting for an answer (forms, steps).
    asks: Vec<asks::Request>,
    /// This conversation is just talk: no tools at all (the app's switch, `/chat-only`).
    chat_only: bool,
    /// How hard the model thinks in this conversation (`/effort`; none: its default).
    effort: Option<turn::Effort>,
    /// A round of several calls shows its steps first (off: "don't wait next time").
    steps_wait: bool,
    /// The Talk page is open: replies are spoken, so they're written for the ear.
    voice: bool,
    /// The next reply comes from this model (`/retry other`): base URL and name.
    next_model: Option<(String, String)>,
    /// Nobody watches this conversation (a routine's run, a diagnosis): no
    /// questions in the chat, and it ends with an answer even at its limit.
    unattended: bool,
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
    /// Reviews waiting for the one running (3.4).
    review_queue: std::collections::VecDeque<QueuedReview>,
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
    /// What recalled memories say, by short id (the app lists them under a reply).
    memory_texts: std::collections::HashMap<String, String>,
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
    /// `[status]`.
    status: status::Settings,
    /// `[diagnose]`.
    diagnose: diagnose::Settings,
    /// `[backup]`, and where the memory store is (backed up through it).
    backup: backup::Settings,
    memory_store: Option<std::path::PathBuf>,
    /// The chat model can see images (`vision` in the config).
    vision: bool,
    /// Images for the next message sent (from a device's attachments).
    attach_images: Vec<String>,
    /// Attached files for the vision model to read in the turn (pictures for a
    /// chat model that can't see, scanned PDFs): name, type, path.
    attach_looks: Vec<(String, String, std::path::PathBuf)>,
    /// The web server, when this lyra is `lyra serve` (machines, devices).
    hub: Option<lyra_web::Hub>,
    /// Everything opened at startup, for starting another conversation.
    shared: Services,
    /// Runs lyra's background work (schedules, goals); a conversation forked
    /// for another device doesn't.
    primary: bool,
    /// Whose conversation this is (`users.json`), and whether they're an admin.
    owner: String,
    admin: bool,
    /// Stops the run in progress (a fresh one per run).
    cancel: Cancel,
    /// Agents' actions waiting for the user's y / n / a, oldest first.
    approvals: Vec<agents::ApprovalRequest>,
    /// The highlighted entry in the command palette, and whether Esc closed it.
    palette: usize,
    palette_hidden: bool,
    /// When goals were last checked, and how often to.
    goals_checked: Instant,
    /// The goal loop's look is being worked out (off the loop).
    goals_looking: bool,
    /// The person's tasks in a line ("2 overdue · 1 today"), for the Goals
    /// panel, and when it was read (the tasks' revision, the time).
    tasks_line: Option<String>,
    tasks_read: (u64, Instant),
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
    /// The oldest turns as one note (how many history messages it stands
    /// for, and the note), once the conversation neared the model's context.
    compacted: Option<(usize, String)>,
    /// A shared conversation: its thread (`shared.rs`), the others' copies kept the same as this one.
    thread: Option<String>,
    /// Who's sending the next message, by name (a shared conversation's messages say who).
    speaker: String,
    /// Earlier messages changed and sent again: the other versions from each (`branches.rs`).
    forks: Vec<branches::Fork>,
    /// What the person has open beside the chat (the app's page: its tasks, a note), for the next message.
    looking: String,
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
            phase_at: chrono::Utc::now(),
            phase: Phase::Idle,
            messages: Vec::new(),
            input: String::new(),
            waiting: false,
            can_continue: false,
            continue_after_error: false,
            asks: Vec::new(),
            chat_only: false,
            effort: None,
            steps_wait: true,
            voice: false,
            next_model: None,
            unattended: false,
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
            review_queue: Default::default(),
            curating: false,
            curate_every: config.learning.curate.every_days(),
            run: None,
            last_run: None,
            applied_memories: Vec::new(),
            applied_memories_tokens: 0,
            memory_texts: Default::default(),
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
            backup: config.backup.clone(),
            status: config.status.clone(),
            diagnose: config.diagnose.clone(),
            memory_store: (config.memory.backend == config::MemoryBackend::Lance).then(|| config.memory.path()).flatten(),
            hub: None,
            vision: config.vision,
            attach_images: Vec::new(),
            attach_looks: Vec::new(),
            shared,
            primary: true,
            owner: lyra_web::users::OWNER.to_string(),
            admin: true,
            cancel: Cancel::default(),
            approvals: Vec::new(),
            palette: 0,
            palette_hidden: false,
            goals_checked: Instant::now(),
            goals_looking: false,
            tasks_line: None,
            tasks_read: (0, Instant::now()),
            goals_every: Duration::from_secs(config.goals.tick_seconds.max(10)),
            autonomous_plans: Default::default(),
            autonomy_note: String::new(),
            goals_busy: false,
            evolving: None,
            evolution_review_every: config.evolution.review.every_days(),
            session_started: chrono::Utc::now(),
            compacted: None,
            thread: None,
            speaker: String::new(),
            forks: Vec::new(),
            looking: String::new(),
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

    fn log(&mut self, level: Level, text: String) {
        let (owner, session) = (self.owner.clone(), self.session_id.clone());
        self.log_for(&owner, &session, level, text);
    }

    /// A log line that's about someone else's work (kept as theirs).
    pub(crate) fn log_for(&mut self, user: &str, session: &str, level: Level, text: String) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        // Kept, with whose it is (the Activity pages).
        activity::record(user, session, serve::level_name(level), &text);
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
            self.phase_at = chrono::Utc::now();
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
        // A first look at everything lyra depends on (lyra serve keeps checking).
        if self.status.enabled {
            let inputs = self.status_inputs();
            crate::acting::spawn(move || status::set_latest(&status::Board::plain(status::pass(inputs))));
        }
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
        if let Some(home) = config::home() {
            backup::refresh(&self.backup.dir(&home));
            if !backup::running() && backup::due(&self.backup, backup::last().map(|b| b.made), chrono::Local::now()) {
                self.log(Level::Info, "the nightly backup is due".into());
                if let Err(e) = self.backup_now(true) {
                    self.log(Level::Error, format!("backup: {e}"));
                }
            }
        }
        if self.evolving.is_none()
            && let Some(evolution) = &self.evolution
            && evolution.review_due(self.evolution_review_every)
        {
            self.log(Level::Evolve, "scheduled evolution review is due".into());
            self.evolve_in_background("reviewing", false, evolve::scheduled_review);
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
        crate::acting::spawn(move || {
            let notes = learning.sync().unwrap_or_else(|e| vec![format!("skill sync failed: {e}")]);
            let _ = tx.send(StreamEvent::LearnNotes(notes));
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

    /// What this turn did before it stopped: the tools that ran, and those that
    /// failed (`None`: nothing yet).
    fn done_so_far(&self) -> Option<String> {
        let start = self.messages.iter().rposition(|m| m.role == "user")?;
        let turn = &self.messages[start + 1..];
        let failed = |id: &str| {
            turn.iter().find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(id)).is_none_or(|m| serde_json::from_str::<Value>(&m.content).is_ok_and(|v| v.get("error").is_some()))
        };
        let (mut ok, mut bad): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for c in turn.iter().flat_map(|m| m.tool_calls.iter()) {
            if failed(&c.id) { bad.push(c.function.name.clone()) } else { ok.push(c.function.name.clone()) }
        }
        if ok.is_empty() && bad.is_empty() {
            return None;
        }
        let counted = |names: &[String]| {
            let mut seen: Vec<(String, usize)> = Vec::new();
            for n in names {
                match seen.iter_mut().find(|(s, _)| s == n) {
                    Some((_, k)) => *k += 1,
                    None => seen.push((n.clone(), 1)),
                }
            }
            seen.iter().map(|(n, k)| if *k > 1 { format!("{n} ×{k}") } else { n.clone() }).collect::<Vec<_>>().join(", ")
        };
        let mut out = String::from("Done before it stopped: ");
        out += &if ok.is_empty() { "nothing yet".to_string() } else { counted(&ok) };
        if !bad.is_empty() {
            out += &format!("; didn't work: {}", counted(&bad));
        }
        Some(out)
    }

    /// Stop the reply being written (and any agent working for it).
    pub(crate) fn stop(&mut self) -> Result<String, String> {
        if !self.waiting {
            return Err("nothing is running".into());
        }
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        // Waiting in line for a browser: not any more.
        crate::sandbox::leave(&self.session_id);
        // A question waiting in the chat is dropped: the run carries on to its stop.
        self.asks.clear();
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

/// The decision model's per-turn questions (when `[decide]` is set up).
const MEMORY_GATE: &str = "Did the user share something worth remembering in future conversations: a lasting preference, \
     a decision, stable configuration or facts about their setup, or a completed multi-step task? Small talk and one-off questions are not.";
const LESSON_GATE: &str = "Does this conversation teach the assistant a reusable procedure or rule: the user corrected it, \
     showed a better way, or a multi-step task worked after a failed attempt?";

/// The first line of a transcript (the goal), as the query for similar memories.
fn goal_text_for(transcript: &str) -> String {
    transcript.lines().next().unwrap_or("").trim_start_matches("Goal: ").to_string()
}

/// `1,234 tokens`, or that the server didn't say.
fn usage_text(usage: Option<&stats::Usage>) -> String {
    usage.map_or("usage unknown".into(), |u| format!("{} tokens", u.prompt_tokens + u.completion_tokens))
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        // Drain everything the stream has produced since the last frame.
        app.decide_notes();
        while let Ok(event) = app.rx.try_recv() {
            app.handle(event);
        }
        if !app.waiting && app.last_schedule_check.elapsed() > Duration::from_secs(10 * 60) {
            app.scheduled();
        }
        if !app.waiting && !app.plan_busy && app.goals_checked.elapsed() > app.goals_every {
            app.goals_tick();
        }
        // Their tasks in a line: read again when they change, and each minute (days turn).
        if app.tasks_read.0 != tasks::revision() || app.tasks_read.1.elapsed() > Duration::from_secs(60) {
            app.tasks_read = (tasks::revision(), Instant::now());
            let today = chrono::Local::now().date_naive();
            app.tasks_line = tasks::settings().enabled.then(|| tasks::as_user(&app.owner, tasks::snapshot).map_or_else(|e| format!("⚠ {e}"), |s| s.line(today)));
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
            // + / - with nothing typed: 👍 / 👎 on the last reply.
            KeyCode::Char(c @ ('+' | '-')) if app.input.is_empty() && !app.waiting && app.messages.iter().any(|m| m.run.is_some()) => {
                let text = match app.rate(None, if c == '+' { "good" } else { "bad" }, "") {
                    Ok(t) if c == '-' => format!("{t}. What was wrong? /outcome bad <what>, and lyra learns from it"),
                    Ok(t) => t,
                    Err(e) => e,
                };
                app.messages.push(Message::new("info", text));
                app.scroll = None;
            }
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

/// A rating word (`good`, `bad`, `partial`; `copied`, `read`) as a verdict.
fn verdict_for(word: &str, note: &str) -> Result<outcomes::Verdict, String> {
    use outcomes::{Strength, Verdict};
    let note = note.trim();
    Ok(match word.trim() {
        "copied" => Verdict::new(SkillOutcome::Success, Strength::Mild, "they copied it"),
        "read" => Verdict::new(SkillOutcome::Success, Strength::Mild, "they had it read aloud"),
        w => {
            let outcome = outcomes::rating(w).ok_or("rate it good, bad or partial")?;
            let mut v = Verdict::new(outcome, Strength::Rated, match outcome {
                SkillOutcome::Success => "👍",
                SkillOutcome::Failure => "👎",
                _ => "partly right",
            });
            if !note.is_empty() {
                v.reason = format!("{}: {}", v.reason, note.chars().take(200).collect::<String>());
                v.said = Some(note.chars().take(500).collect());
                v.correction = outcome != SkillOutcome::Success;
            }
            v
        }
    })
}

fn main() {
    startup::main()
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
