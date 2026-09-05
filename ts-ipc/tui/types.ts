// TUI Types for BaoClaw

export type ContentBlockType =
  "text" | "thinking" | "tool_use" | "tool_result" | "code";

export interface ContentBlock {
  type: ContentBlockType;
  content: string;
  language?: string;
  toolName?: string;
  toolId?: string;
  input?: unknown; // tool_use input parameters
  isError?: boolean; // tool_result error flag
  isExpanded?: boolean; // accordion expansion state
}

export interface Message {
  id: string;
  role: "user" | "assistant";
  content: ContentBlock[];
  timestamp: Date;
}

export interface TokenUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  contextWindow: number;
  cost?: number;
}

export interface Session {
  id: string;
  model: string;
  status: "idle" | "streaming" | "thinking" | "error";
  usage?: TokenUsage;
}

export interface ToolProgress {
  id?: string;
  name: string;
  status: "running" | "completed" | "error";
  input?: unknown;
  output?: string;
  isExpanded?: boolean;
}

/** A tool invocation waiting for the user's allow/deny decision. */
export interface PendingPermission {
  toolUseId: string;
  toolName: string;
  /** Truncated JSON.stringify of the daemon event's `input`. */
  inputPreview: string;
}

export type ActionType =
  | "ADD_MESSAGE"
  | "SET_STREAMING"
  | "APPEND_STREAM"
  | "SET_THINKING"
  | "APPEND_THINKING"
  | "SET_TOOLS"
  | "UPDATE_TOOL"
  | "ADD_TOOL_USE"
  | "ADD_TOOL_RESULT"
  | "TOGGLE_TOOL_EXPAND"
  | "SET_SESSION"
  | "UPDATE_USAGE"
  | "SET_NAV_MODE"
  | "SET_SELECTED_TOOL_INDEX"
  | "SET_INPUT"
  | "SET_ERROR"
  | "CLEAR_ERROR"
  | "SET_FLASH"
  | "QUEUE_PERMISSION"
  | "RESOLVE_PERMISSION"
  | "SET_AUTO_ALLOW"
  | "RESET";

export interface Action {
  type: ActionType;
  payload?: unknown;
}

export interface TuiState {
  messages: Message[];
  isStreaming: boolean;
  streamingContent: string;
  thinkingContent: string;
  currentTools: ToolProgress[];
  session: Session | null;
  mode: "insert" | "normal";
  selectedToolIndex: number;
  input: string;
  error: string | null;
  flashMessage: string | null;
  usage: TokenUsage;
  /** Permission requests awaiting a decision; the head is shown first. */
  pendingPermissions: PendingPermission[];
  /**
   * Whether tool permission requests are auto-allowed. Seeded from the
   * daemon-persisted knob (permissions.auto_allow_channels.tui, absent =
   * true) and toggled with p / Ctrl+P, which persists via the daemon.
   */
  autoAllow: boolean;
}
