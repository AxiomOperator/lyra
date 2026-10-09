mod acting;
mod alerts;
mod agents;
mod backup;
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
mod connect;
mod context;
mod decide;
mod diagnose;
mod evolve;
mod feedback;
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
mod lock;
mod mem;
mod notes;
mod people;
mod plan;
mod planner;
mod pmi;
mod proactive;
mod migrate;
mod retrieval;
mod routines;
mod serve;
mod search;
mod secrets;
mod sessions;
mod settings;
mod stats;
mod style;
mod status;
mod teams;
mod text;
mod templates;
mod tools;
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
    /// The decision model found nothing for a memory capture (`memory`) or a
    /// skill review to look at.
    GateClosed { memory: bool },
    /// A backup finished (`nightly` ones only log).
    BackedUp { nightly: bool, result: backup::Made },
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
    /// The reply stopped at its tool-call limit (after `Done`): it can be continued.
    Limit(usize),
    Error(String),
    /// Progress note from the worker for the activity log.
    Log(String),
    /// What the vision model read off a file the user attached: kept with their message.
    Seen(String),
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
    /// The last reply stopped at its tool-call limit: Continue picks it up.
    can_continue: bool,
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
            can_continue: false,
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
                // Working memory tracks what both sides mention (M7): the owner's only.
                if let Some(mem) = self.mem().filter(|_| self.personal().is_none()) {
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
            StreamEvent::Limit(rounds) => {
                let own = limits::own_tool_rounds(&self.owner).is_some();
                self.log(Level::Agent, format!("stopped at the tool-call limit ({rounds} rounds{}): it can be continued", if own { ", this person's own" } else { "" }));
                self.messages.push(Message::new("info", limits::stopped_note(rounds, own, self.admin)));
                self.can_continue = true;
                self.save_session();
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
            StreamEvent::Seen(text) => {
                // With the message, so later turns know it too.
                if let Some(m) = self.messages.iter_mut().rev().find(|m| m.role == "user") {
                    m.content.push_str(&text);
                }
            }
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
            StreamEvent::MemoriesApplied { ids, texts, tokens } => {
                // What they say, for the app's sources under the reply.
                for (id, text) in ids.iter().zip(texts) {
                    self.memory_texts.insert(id.clone(), text);
                }
                self.log(Level::Memory, format!("recalled {} memor{}: {}", ids.len(), if ids.len() == 1 { "y" } else { "ies" }, ids.join(" ")));
                self.applied_memories = ids;
                self.applied_memories_tokens = tokens;
            }
            StreamEvent::BackedUp { nightly, result } => {
                let text = match &result {
                    Ok((b, notes)) => {
                        for n in notes {
                            self.log(Level::Info, format!("backup: {n}"));
                        }
                        format!("💾 backed up lyra: {} ({})", b.name, backup::size_text(b.size))
                    }
                    Err(e) => format!("backup failed: {e}"),
                };
                self.log(if result.is_ok() { Level::Info } else { Level::Error }, text.clone());
                if !nightly {
                    self.messages.push(Message::new(if result.is_ok() { "info" } else { "error" }, text));
                }
            }
            StreamEvent::GateClosed { memory } => {
                if memory {
                    self.capturing = false;
                } else {
                    self.reviewing = false;
                }
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
        // A first look at everything lyra depends on (lyra serve keeps checking).
        if self.status.enabled {
            let inputs = self.status_inputs();
            thread::spawn(move || status::set_latest(&status::Board::plain(status::pass(inputs))));
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
        if self.personal().is_some() {
            self.last_run = None;
            return;
        }
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
        // The owner's lessons become shared skills; anyone else's are theirs alone.
        let owner = self.personal();
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
        // Nothing the keywords catch: the decision model (when set up) looks at the turn.
        let ask_decide = trigger.is_none() && decide::model().is_some() && history.len() >= 2;
        if trigger.is_none() && !ask_decide {
            return;
        }
        let mut transcript = learn::transcript(&history, 16);
        if !self.corrected_skills.is_empty() {
            transcript += &format!(
                "\n\n[skills that were in the prompt for the corrected answer] {}",
                self.corrected_skills.join(", ")
            );
        }
        if let Some(trigger) = trigger {
            self.log(Level::Learn, format!("reviewing for a lesson: {trigger}"));
        }
        self.reviewing = true;

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, run) = (self.model.clone(), self.tx.clone(), self.last_run.as_ref().map(|r| r.id));
        thread::spawn(move || {
            let trigger = match trigger {
                Some(t) => t,
                None => match decide::yes("a lesson here?", &transcript, LESSON_GATE) {
                    Some((true, _)) => "the decision model saw a reusable lesson",
                    _ => {
                        let _ = tx.send(StreamEvent::GateClosed { memory: false });
                        return;
                    }
                },
            };
            let review = learning.review(&url, &model, trigger, &transcript, run, owner.as_deref());
            let _ = tx.send(StreamEvent::Reviewed(review));
        });
    }

    /// What the decision model decided (or that it's down), into Activity.
    fn decide_notes(&mut self) {
        for note in decide::take_notes() {
            self.log(Level::Info, note);
        }
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
        // Anyone but the owner: captured into their own memories only.
        let scope = self.personal().map(|u| format!("user:{u}"));
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
        // Nothing the keywords catch: the decision model (when set up) looks at the turn.
        let ask_decide = reason.is_none() && !turn.model_saved && decide::model().is_some();
        if reason.is_none() && !ask_decide {
            return;
        }
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let transcript = learn::transcript(&history, if episode { 30 } else { 8 });
        let reply = self.messages.iter().rev().find(|m| m.role == "assistant").map_or("", |m| m.content.as_str());
        let query = format!("{user}\n{reply}");
        if let Some(reason) = reason {
            self.log(Level::Memory, format!("capturing: {reason}"));
        }
        self.capturing = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, run) = (self.model.clone(), self.tx.clone(), self.last_run.as_ref().map(|r| r.id));
        let started = self.session_started;
        let owner = self.owner.clone();
        thread::spawn(move || {
            // Counted (and kept) for the conversation's person.
            crate::acting::set(&owner);
            let reason = match reason {
                Some(r) => r,
                None => match decide::yes("worth remembering?", &transcript, MEMORY_GATE) {
                    Some((true, _)) => "the decision model found something worth remembering",
                    _ => {
                        let _ = tx.send(StreamEvent::GateClosed { memory: true });
                        return;
                    }
                },
            };
            let review = mem.capture(&url, &model, reason, &transcript, &query, run, Some(started), scope.as_deref());
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
                let review = mem.capture(&url, &model, reason, &transcript, &goal_text_for(&transcript), Some(plan_id), Some(started), None);
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
            let (model, tx, owner) = (self.model.clone(), self.tx.clone(), self.personal());
            self.log(Level::Learn, "reviewing the plan's recoveries for a lesson".into());
            thread::spawn(move || {
                let review = learning.review(&url, &model, "a failed step was replaced by a working one (plan execution)", &transcript, None, owner.as_deref());
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

    fn refresh_evolution(&self) {
        let Some(evolution) = self.evolution.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Evolution(evolution.snapshot()));
        });
    }

    /// E1: record the turn just answered (or failed) for evolution. Returns its id.
    fn record_chat_run(&mut self, stats: &Stats, error: Option<&str>) -> Option<Uuid> {
        // Evolution learns from the owner's conversations only.
        if self.personal().is_some() {
            return None;
        }
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
        let mut s = sessions::Session::from_messages(&self.session_id, self.session_started, &self.messages);
        s.owner = self.owner.clone();
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
            return Ok(format!("{}\n\n/resume <id> to switch", sessions::describe(&sessions::list_for(&dir, &self.owner), 20)));
        }
        let s = sessions::find_for(&dir, arg, &self.owner)?;
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
        app.owner = self.owner.clone();
        app.admin = self.admin;
        app.refresh_agents();
        app
    }

    /// The memory scope this conversation is kept to: its person's own
    /// (`user:<id>`) for anyone but the owner.
    fn personal(&self) -> Option<String> {
        (self.owner != lyra_web::users::OWNER).then(|| self.owner.clone())
    }

    /// A new conversation for a user: theirs, with their rights and their USER.md.
    fn fork_for(&self, who: &lyra_web::Who) -> App {
        let config = Config::load().unwrap_or_default();
        let mut app = App::new(config, Context::load_for(&who.user, &who.name), self.shared.clone());
        app.hub = self.hub.clone();
        app.primary = false;
        app.owner = who.user.clone();
        app.admin = who.admin;
        if !crate::acting::is_owner(&who.user) {
            app.goals = goals::for_user(&who.user);
        }
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
