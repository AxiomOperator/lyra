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
  skills: string[];
  agents: string[];
}

export interface Approval {
  id: number;
  agent: string;
  what: string;
  detail: string;
  why: string;
  dangerous: boolean;
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
}

export interface Status {
  phase?: string;
  waiting?: boolean;
  model?: string;
  session?: string;
  title?: string | null;
  approvals?: Approval[];
  machines?: string[];
  agents?: { title: string; working: boolean; enabled: boolean }[];
  online?: { id: string; name: string }[];
  pairing?: PairRequest[];
  machines_detail?: Machine[];
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
