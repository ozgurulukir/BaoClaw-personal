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
}
