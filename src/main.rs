mod config;
mod context;
mod evolve;
mod learn;
mod mem;
mod plan;
mod migrate;
mod retrieval;
mod stats;
mod tools;
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
        }
    }

    /// Whether the model sees this message (info and error lines are UI-only).
    fn is_history(&self) -> bool {
        matches!(self.role.as_str(), "user" | "assistant" | "tool")
    }
}

/// A complete tool call, as sent back to the model in the conversation history.
#[derive(Clone, Default, Serialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCall,
}

#[derive(Clone, Default, Serialize)]
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
}

enum StreamEvent {
    Token(String),
    Reasoning(String),
    /// The model asked to run these tools.
    ToolCalls(Vec<ToolCall>),
    /// A tool finished; `content` is what the model will see.
    ToolResult { id: String, name: String, content: String },
    Done(Stats),
    Error(String),
    /// Progress note from the worker for the activity log.
    Log(String),
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
}

/// What `main` opened before the UI starts.
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
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Tool,
    Learn,
    Memory,
    Plan,
    Evolve,
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
        let Services { tools, memory_status, learning, learning_status, engine, planning_status, evolution, evolution_status } =
            services;
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
        if content.is_empty() || self.waiting {
            return;
        }
        self.input.clear();
        self.scroll = None;
        if content.starts_with('/') {
            self.command(&content);
            return;
        }
        self.judge_last_run(&content);
        self.messages.push(Message::new("user", content.clone()));
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
            .map(|m| serde_json::to_value(m).expect("message serializes"))
            .collect();
        let (model, tools, tx) = (self.model.clone(), self.tools.clone(), self.tx.clone());
        let (learning, evolution) = (self.learning.clone(), self.evolution.clone());
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
            if let Some(tools) = &tools {
                apply_memories(&tools.mem, &content, run, &mut history, &tx);
            }
            if let Some(learning) = learning {
                apply_skills(&learning, &content, run, &mut history, &tx);
            }
            let event = match converse(&url, &model, history, tools.as_deref(), max_rounds, run, &tx) {
                Ok(stats) => StreamEvent::Done(stats),
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }

    fn handle(&mut self, event: StreamEvent) {
        match event {
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
                let reply = self.reply();
                reply.stats = Some(stats);
                reply.skills = skills.clone();
                reply.memories = memories.clone();
                self.last_run = self.run.take().map(|id| LastRun { id, skills, memories: memories.len(), evo });
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
            }
            StreamEvent::Log(text) => self.log(Level::Info, text),
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
        self.reload_evolution();
        self.check_models();
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
            "/help" => Ok(format!("{COMMANDS}\n{}\n{HELP_END}", evolve::COMMANDS)),
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
        self.messages.push(Message::new(role, format!("> {line}\n{text}")));
        let memory_sub = arg.split_whitespace().next().unwrap_or("");
        match name {
            "/memory" if ok && memory_sub == "curate" => self.memory_curate(),
            "/memory" if ok && memory_sub == "episode" => self.capture(true),
            "/memory" if ok && matches!(memory_sub, "forget" | "archive" | "restore" | "purge" | "correct" | "approve" | "reject" | "working" | "project") => {
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

    fn runtime(&self) -> LyraRuntime {
        LyraRuntime {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            forbidden_tools: self.forbidden_tools.clone(),
            evolution: self.evolution.clone(),
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
        self.tools_changed();
        self.refresh_evolution();
    }

    /// The tool list may have changed (composite tools): recount it.
    fn tools_changed(&mut self) {
        let definitions = self.tools.as_ref().map(|t| t.definitions());
        self.tools_tokens = definitions.as_ref().map_or(0, |d| learn::approx_tokens(&d.to_string()));
        self.tool_count = definitions.as_ref().and_then(|d| d.as_array().map(Vec::len)).unwrap_or(0);
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
                    mem.reconfigure(config.memory.settings.clone(), config.embedding.clone());
                    mem.set_project(config.memory.project());
                }
                self.memory_curate_every = config.memory.curate.every_days();
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
    fn reply(&mut self) -> &mut Message {
        if self.messages.last().is_none_or(|m| m.role != "assistant") {
            self.messages.push(Message::new("assistant", String::new()));
        }
        self.messages.last_mut().unwrap()
    }
}

const COMMANDS: &str = "\
/skills                      skills and changes waiting for review
/approve <id>                apply a proposal, or (re)activate a skill
/reject <id>                 discard a proposal or proposed skill for good
/deprecate <id>              stop using a skill without deleting it
/forget-skill <id>           delete a skill's file (its history is kept)
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
/outcome good|bad|partial    (also) how the last reply went, for evolution";

const HELP_END: &str = "/help                        this list";

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
fn converse(
    url: &str,
    model: &str,
    mut history: Vec<Value>,
    tools: Option<&Tools>,
    max_rounds: usize,
    run: Uuid,
    tx: &Sender<StreamEvent>,
) -> Result<Stats, String> {
    let start = Instant::now();
    let mut total: Option<Stats> = None;
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
        if let Some(tools) = tools {
            body["tools"] = tools.definitions();
        }
        let round = stream(url, &body, tx)?;
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        let Some(tools) = tools.filter(|_| !round.tool_calls.is_empty()) else {
            let mut stats = total.expect("at least one round");
            stats.elapsed = start.elapsed();
            return Ok(stats);
        };

        tx.send(StreamEvent::ToolCalls(round.tool_calls.clone())).map_err(|e| e.to_string())?;
        history.push(json!({
            "role": "assistant",
            "content": round.content,
            "tool_calls": round.tool_calls,
        }));
        for call in &round.tool_calls {
            let ctx = CallContext { run: Some(run), call_id: &call.id };
            let content = tools.run(&call.function.name, &call.function.arguments, ctx);
            history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            let (id, name) = (call.id.clone(), call.function.name.clone());
            tx.send(StreamEvent::ToolResult { id, name, content }).map_err(|e| e.to_string())?;
        }
    }
    Err(format!("stopped after {max_rounds} rounds of tool calls"))
}

/// POST the request, forward each delta as it arrives, and measure the reply.
fn stream(url: &str, body: &Value, tx: &Sender<StreamEvent>) -> Result<Round, String> {
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
    for line in BufReader::new(resp).lines() {
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
    Ok(Round { stats, content, tool_calls })
}

fn main() {
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
    // Memory is async (sqlx); a small runtime lets lyra's threads call into it.
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (tools, memory_status) = open_memory(&config, runtime.handle());
    let (learning, learning_status) = open_learning(&config, runtime.handle());
    let (engine, planning_status) = open_planning(&config, runtime.handle());
    let (evolution, evolution_status) = open_evolution(&config, runtime.handle());
    let services =
        Services { tools, memory_status, learning, learning_status, engine, planning_status, evolution, evolution_status };
    learn::configure(learn::Structured {
        max_tokens: config.structured_max_tokens,
        thinking: config.structured_thinking,
    });
    let mut app = App::new(config, Context::load(), services);
    for note in migrated {
        app.log(Level::Info, note);
    }
    app.start();
    ratatui::run(|terminal| run(terminal, &mut app)).expect("terminal error");
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
fn open_memory(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> (Option<Arc<Tools>>, Result<String, String>) {
    if !config.memory.enabled {
        return (None, Ok("memory off".into()));
    }
    let Some(path) = config.memory.path() else {
        return (None, Err("memory off: no data directory (set [memory] path)".into()));
    };
    match runtime.block_on(lyra_memory::MemoryManager::open(&path, config.memory.settings.clone())) {
        Ok(manager) => {
            let shown = context::show(&path);
            let mem = Mem::new(manager, runtime.clone(), config.embedding.clone(), shown.clone(), config.memory.project());
            (Some(Arc::new(Tools::new(Arc::new(mem)))), Ok(shown))
        }
        Err(e) => (None, Err(format!("memory off: {e:#}"))),
    }
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

        terminal.draw(|f| ui::draw(f, app))?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.show_reasoning = !app.show_reasoning;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => app.reload(),
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
            }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        }
    }
}
