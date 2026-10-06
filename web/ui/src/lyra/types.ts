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
  session: string;
  devices: number;
}

export interface ThisDevice {
  id: string;
  name: string;
  push: boolean;
}
