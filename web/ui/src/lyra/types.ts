// What `lyra serve` sends (see src/serve.rs on the Rust side).

export type Role = "user" | "assistant" | "tool" | "agent_tool" | "info" | "error" | "agent" | "approval";

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface ChatMessage {
  role: Role;
  content: string;
  reasoning: string;
  tools: string[];
  calls: ToolCall[];
  tool_call_id: string | null;
  stats: string | null;
  memories: string[];
  /** What the recalled memories say (when lyra still has them). */
  memory_notes?: { id: string; text: string }[];
  skills: string[];
  agents: string[];
  /** The reply's tokens, time and cost, for its context meter. */
  usage?: { input: number; cached: number; output: number; ms: number; estimated: boolean; cost: number; currency: string } | null;
}

export interface Approval {
  id: number;
  agent: string;
  what: string;
  detail: string;
  why: string;
  dangerous: boolean;
  /** The tool it's for: the question shows at that call. */
  tool?: string;
}

export interface PairRequest {
  id: string;
  code: string;
  name: string;
  kind: string;
  hostname: string;
  os: string;
}

export interface Machine {
  name: string;
  id: string;
  online: boolean;
  hostname: string | null;
  os: string | null;
  user: string | null;
  version: string | null;
  build: string | null;
  self_update: boolean;
  update_available: boolean;
  last_seen: string;
  /** Its latest health report, with what's wrong in it. */
  health?: Health | null;
  /** Coding agents it has: {"claude": "2.1.291 (Claude Code)", "opencode": "1.18.29"}. */
  harnesses?: Record<string, string> | null;
}

export interface Health {
  at: string;
  disks: { mount: string; used_pct: number; size_kb: number; avail_kb: number }[];
  memory: { total_kb: number; available_kb: number; used_pct: number } | null;
  load: number[];
  cpus: number;
  uptime_s: number | null;
  failed_units: string[] | null;
  updates: number | null;
  problems: string[];
  summary: string;
}

export interface Status {
  /** Bug reports and feature requests with news for this person. */
  feedback_news?: number;
  /** Moves whenever any feedback changes (the page looks again). */
  feedback_rev?: number;
  /** This lyra's version (major.minor.fix.build). */
  version?: string;
  /** The chat model's context window in tokens (0: unknown). */
  context_window?: number;
  /** The plan this conversation is running (see chat-parts2's PlanView). */
  plan?: unknown;
  phase?: string;
  waiting?: boolean;
  /** The last reply stopped at its tool-call limit: Continue picks it up. */
  can_continue?: boolean;
  model?: string;
  session?: string;
  title?: string | null;
  approvals?: Approval[];
  machines?: string[];
  agents?: { title: string; working: boolean; enabled: boolean }[];
  online?: { id: string; name: string }[];
  pairing?: PairRequest[];
  machines_detail?: Machine[];
  /** Problems researched by themselves (read-only), newest first. */
  diagnoses?: Diagnosis[] | null;
  briefing?: Briefing | null;
  pmi?: PmiState | null;
  users_waiting?: number;
  /** Everything lyra depends on, checked every minute. */
  status?: StatusBoard | null;
  /** Named sets of machines (`[groups]`), for @group. */
  groups?: { name: string; machines: string[] }[] | null;
  /** Coding agents installed on the server itself. */
  server_harnesses?: Record<string, string> | null;
  /** The server's own health (as a machine's). */
  server_health?: Health | null;
  /** Conversations loaded on the server, and which are answering. */
  conversations?: { session: string; title: string | null; answering: boolean }[];
  /** The decision model (`[decide]`), with what it has done since lyra started. */
  decide?: { model: string; decided: number; to_chat: number; ms: number } | null;
  /** Routines with their next run and last result (`runs` holds the latest one). */
  routines?: Routine[] | null;
  /** The newest backup of lyra, and whether one is being made. */
  backup?: { name: string; made: string; size: number } | null;
  backing_up?: boolean;
}

export interface Command {
  usage: string;
  description: string;
  completion: string;
}

export interface Device {
  id: string;
  name: string;
  kind: string;
  online: boolean;
  created: string;
  last_seen: string;
  push: boolean;
}

export interface Session {
  id: string;
  title: string;
  turns: number;
  updated: string;
  current: boolean;
  /** Loaded on the server (some device has it open, or recently). */
  open?: boolean;
  /** lyra is writing a reply in it right now. */
  answering?: boolean;
  pinned?: boolean;
  archived?: boolean;
  /** The person's own folder for it. */
  folder?: string | null;
  /** The folder lyra suggests, until taken or dismissed. */
  suggested?: string | null;
}

export interface ActivityLine {
  time: string;
  level: string;
  text: string;
}

export interface About {
  lyra: string;
  app: string;
  node_build: string | null;
  model: string;
  /** The decision model (`[decide]`), when one is set up. */
  decide: string | null;
  backups: {
    dir: string;
    enabled: boolean;
    at: string;
    keep: number;
    count: number;
    last: { name: string; made: string; size: number } | null;
  } | null;
  session: string;
  devices: number;
}

export interface ThisDevice {
  id: string;
  name: string;
  push: boolean;
}

export interface MemoryRow {
  id: string;
  kind: string;
  scope: string;
  content: string;
  importance: number;
  confidence: number;
  status: string;
  updated: string;
  tags: string[];
  score: number | null;
}

export interface MemoryPageData {
  memories: MemoryRow[];
  proposals: { id: string; text: string }[];
  scopes: { scope: string; count: number }[];
  active: number;
  error?: string;
}

export interface SkillRow {
  id: string;
  name: string;
  description: string;
  instructions: string;
  status: "proposed" | "active" | "deprecated" | "rejected";
  confidence: number;
  agent: string | null;
  record: string;
  updated: string;
  /** Theirs alone (learned from and for them). */
  mine?: boolean;
  /** They may approve, reject or stop it (their own; shared ones: admins). */
  can_decide?: boolean;
}

export interface SkillsPageData {
  mode: string;
  skills: SkillRow[];
  proposals: { id: string; change: string; reason: string; detail: string }[];
  error?: string;
}

export interface GoalRow {
  id: string;
  title: string;
  description: string;
  status: string;
  priority: number;
  progress: number;
  score: number | null;
  parent: boolean;
  due: string | null;
  blocked: string | null;
}

export interface GoalsPageData {
  goals: GoalRow[];
  mode: string;
  error?: string;
}

export interface ModelsData {
  current: string;
  models: string[];
  error?: string;
}

export interface SystemRules {
  enabled: boolean;
  shell: string;
  timeout_seconds: number;
  max_output: number;
  allow_commands: string[];
  write_roots: string[];
  deny_paths: string[];
  ssh_hosts: string[];
  http_timeout_seconds: number;
  approval_timeout_seconds: number;
}

export interface RulesData {
  system?: SystemRules;
  path?: string;
  error?: string;
}

export interface RoutineRun {
  at: string;
  seconds: number;
  needs_user: boolean;
  outcome: string;
  summary: string;
  session: string;
  decided_by: string;
}

export interface Routine {
  name: string;
  schedule: string;
  prompt: string;
  notify: "problems" | "always" | "never";
  enabled: boolean;
  /** It may change things (asking first); otherwise it only looks. */
  changes: boolean;
  valid: boolean;
  next: string | null;
  running: boolean;
  runs: RoutineRun[];
}

export type CheckState = "up" | "degraded" | "down" | "off";

export interface StatusRow {
  id: string;
  group: string;
  name: string;
  target: string;
  state: CheckState;
  latency_ms: number | null;
  detail: string;
  uptime_24h: number | null;
  uptime_7d: number | null;
  spark: (number | null)[];
  since: string | null;
  changes: { at: string; from: CheckState; to: CheckState; detail: string }[];
}

export interface StatusBoard {
  at: string;
  overall: CheckState;
  rows: StatusRow[];
}

export interface Diagnosis {
  key: string;
  machine: string;
  problem: string;
  state: "queued" | "running" | "done" | "failed";
  summary: string;
  session: string;
  at: string;
  resolved: boolean;
}

/** The daily briefing (lyra serve, `[briefing]`). */
export type BriefingLevel = "attention" | "note" | "ok";

export interface BriefingItem {
  level: BriefingLevel;
  text: string;
  /** The page it's about: machines, status, routines, coding, goals. */
  link: string;
}

export interface Briefing {
  at: string;
  since: string;
  headline: string;
  attention: number;
  takeaway?: string | null;
  sections: { name: string; items: BriefingItem[] }[];
}

/** PMI, the project-management app: a task as lyra shows it. */
export interface PmiTask {
  id: string;
  title: string;
  status: "todo" | "in_progress" | "done";
  /** "personal", "project Website", "team IT". */
  where: string;
  due: string | null;
  priority: "low" | "medium" | "high" | "urgent";
  assignees?: string[];
  blocked?: boolean;
  approval?: string;
  checklist?: string;
  repeat?: string;
}

export interface PmiProject {
  id: string;
  name: string;
  status: string;
  health: "on_track" | "at_risk" | "off_track" | "no_tasks";
  reported_health: string | null;
  tasks: { total: number; done: number; open: number; overdue: number; dueSoon: number };
  risks: { open: number; high: number };
  last_update: string | null;
  leads: string[];
}

export interface PmiInboxItem {
  id: string;
  reason: string;
  at: string;
  read: boolean;
  task?: { id: string; title: string; where: string };
  what?: string;
  by?: string;
  points?: string;
}

export interface PmiState {
  at: string | null;
  error: string | null;
  live: boolean;
  user: string;
  org: string;
  tasks: PmiTask[];
  waiting: { task_transfers?: { id: string; task: string; expires: string }[]; project_transfers?: { id: string; project: string; expires: string }[]; approvals?: PmiInboxItem[] };
  inbox_unread: number;
  inbox: PmiInboxItem[];
  projects: PmiProject[];
}

/** Who this device's user is. */
export interface Me {
  user: string;
  name: string;
  admin: boolean;
}

export interface UserRow {
  id: string;
  name: string;
  email: string;
  role: "admin" | "member";
  status: "active" | "pending" | "disabled";
  created: string;
  last_seen: string | null;
  microsoft: boolean;
  devices: { name: string; last_seen: string }[];
  /** Their own limit on tool calls in one reply; the shared default when null. */
  tool_rounds?: number | null;
  default_rounds?: number;
}

/** An Outlook calendar event as lyra shows it. */
export interface CalEvent {
  id: string;
  title: string;
  start: string | null;
  end: string | null;
  all_day?: boolean;
  where?: string;
  with?: string[];
  organizer?: string;
  your_answer?: string;
  online?: boolean;
}

export interface CalendarToday {
  available: boolean;
  connected?: boolean;
  /** The connection includes mail. */
  mail?: boolean;
  /** …and Teams chats and files. */
  teams?: boolean;
  /** They can read their Teams meetings' transcripts (meeting follow-ups). */
  meetings?: boolean;
  /** The server asks for transcripts when connecting ([web.entra] meetings). */
  meetings_available?: boolean;
  error?: string;
  events?: CalEvent[];
  clashes?: string[];
  invites?: CalEvent[];
}

/** A mail message as lyra shows it. */
export interface MailMessage {
  id: string;
  from: string;
  subject: string;
  received: string;
  preview: string;
  unread?: boolean;
  important?: boolean;
  attachments?: boolean;
  flagged?: boolean;
}

export interface MailGlance {
  connected: boolean;
  error?: string;
  unread?: number;
  recent?: MailMessage[];
  flagged?: MailMessage[];
}
