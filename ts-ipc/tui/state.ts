// TUI State Management
import {
  TuiState,
  Action,
  ActionType,
  Message,
  ContentBlock,
  Session,
  ToolProgress,
  PendingPermission,
} from "./types.js";

export const INITIAL_STATE: TuiState = {
  messages: [],
  isStreaming: false,
  streamingContent: "",
  thinkingContent: "",
  currentTools: [],
  session: null,
  mode: "insert",
  selectedToolIndex: 0,
  input: "",
  error: null,
  flashMessage: null,
  pendingPermissions: [],
  autoAllow: true,
  usage: {
    promptTokens: 0,
    completionTokens: 0,
    totalTokens: 0,
    contextWindow: 200000,
    cost: 0,
  },
};

type ActionHandler = (state: TuiState, action: Action) => TuiState;

function addMessage(state: TuiState, action: Action): TuiState {
  const msg = action.payload as Message;
  return {
    ...state,
    messages: [...state.messages, msg],
  };
}

function setStreaming(state: TuiState, action: Action): TuiState {
  const isStarting = action.payload as boolean;
  return {
    ...state,
    isStreaming: isStarting,
    streamingContent: "",
    currentTools: isStarting ? [] : state.currentTools,
    selectedToolIndex: 0,
  };
}

function appendStream(state: TuiState, action: Action): TuiState {
  const content = action.payload as string;
  return {
    ...state,
    streamingContent: state.streamingContent + content,
  };
}

function setThinking(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    thinkingContent: action.payload as string,
  };
}

function appendThinking(state: TuiState, action: Action): TuiState {
  const content = action.payload as string;
  return {
    ...state,
    thinkingContent: state.thinkingContent + content,
  };
}

function setTools(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    currentTools: action.payload as ToolProgress[],
  };
}

function updateTool(state: TuiState, action: Action): TuiState {
  const { id, update } = action.payload as {
    id: string;
    update: Partial<ToolProgress>;
  };
  return {
    ...state,
    currentTools: state.currentTools.map((tool) =>
      tool.id === id || tool.name === id ? { ...tool, ...update } : tool,
    ),
  };
}

function setSession(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    session: action.payload as Session,
  };
}

function updateUsage(state: TuiState, action: Action): TuiState {
  const usage = action.payload as Partial<TuiState["usage"]>;
  return {
    ...state,
    usage: {
      ...state.usage,
      ...usage,
    },
  };
}

function setNavMode(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    mode: action.payload as "insert" | "normal",
  };
}

function setSelectedToolIndex(state: TuiState, action: Action): TuiState {
  const idx = action.payload as number;
  const maxIdx = Math.max(0, state.currentTools.length - 1);
  return {
    ...state,
    selectedToolIndex: Math.max(0, Math.min(idx, maxIdx)),
  };
}

function toggleToolExpand(state: TuiState, action: Action): TuiState {
  const { index, toolId } =
    (action.payload as {
      index?: number;
      toolId?: string;
    }) || {};
  const targetIdx =
    index !== undefined
      ? index
      : toolId
        ? state.currentTools.findIndex((t) => t.id === toolId)
        : state.selectedToolIndex;

  if (targetIdx >= 0 && targetIdx < state.currentTools.length) {
    const tools = [...state.currentTools];
    tools[targetIdx] = {
      ...tools[targetIdx],
      isExpanded: !tools[targetIdx].isExpanded,
    };
    return { ...state, currentTools: tools };
  }
  return state;
}

function addToolUse(state: TuiState, action: Action): TuiState {
  const { toolName, toolId, input } = action.payload as {
    toolName: string;
    toolId: string;
    input: unknown;
  };
  const block: ContentBlock = {
    type: "tool_use",
    content: JSON.stringify(input, null, 2),
    toolName,
    toolId,
    input,
    isExpanded: false,
  };

  const newTool: ToolProgress = {
    id: toolId,
    name: toolName,
    status: "running",
    input,
    isExpanded: false,
  };

  const currentTools = [...state.currentTools, newTool];

  // Append to last assistant message's content
  const messages = [...state.messages];
  if (
    messages.length > 0 &&
    messages[messages.length - 1].role === "assistant"
  ) {
    const last = messages[messages.length - 1];
    messages[messages.length - 1] = {
      ...last,
      content: [...last.content, block],
    };
  } else {
    // No ongoing assistant message, create one
    messages.push({
      id: generateId(),
      role: "assistant",
      content: [block],
      timestamp: new Date(),
    });
  }
  return { ...state, messages, currentTools };
}

function addToolResult(state: TuiState, action: Action): TuiState {
  const { toolId, output, isError } = action.payload as {
    toolId: string;
    output: string;
    isError: boolean;
  };
  const block: ContentBlock = {
    type: "tool_result",
    content: output,
    toolId,
    isError,
    isExpanded: false,
  };

  const currentTools = state.currentTools.map((t) =>
    t.id === toolId
      ? {
          ...t,
          output,
          status: (isError ? "error" : "completed") as ToolProgress["status"],
        }
      : t,
  );

  const messages = [...state.messages];
  // Append to last assistant message
  if (
    messages.length > 0 &&
    messages[messages.length - 1].role === "assistant"
  ) {
    const last = messages[messages.length - 1];
    messages[messages.length - 1] = {
      ...last,
      content: [...last.content, block],
    };
  }
  return { ...state, messages, currentTools };
}

function setInput(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    input: action.payload as string,
  };
}

function setError(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    error: action.payload as string,
  };
}

function clearError(state: TuiState): TuiState {
  return {
    ...state,
    error: null,
  };
}

function setFlash(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    flashMessage: action.payload as string | null,
  };
}

function queuePermission(state: TuiState, action: Action): TuiState {
  const request = action.payload as PendingPermission;
  return {
    ...state,
    pendingPermissions: [...state.pendingPermissions, request],
  };
}

function resolvePermission(state: TuiState, action: Action): TuiState {
  const toolUseId = action.payload as string;
  return {
    ...state,
    pendingPermissions: state.pendingPermissions.filter(
      (p) => p.toolUseId !== toolUseId,
    ),
  };
}

function setAutoAllow(state: TuiState, action: Action): TuiState {
  return {
    ...state,
    autoAllow: action.payload as boolean,
  };
}

function reset(): TuiState {
  return INITIAL_STATE;
}

const handlers: Record<ActionType, ActionHandler> = {
  ADD_MESSAGE: addMessage,
  SET_STREAMING: setStreaming,
  APPEND_STREAM: appendStream,
  SET_THINKING: setThinking,
  APPEND_THINKING: appendThinking,
  SET_TOOLS: setTools,
  UPDATE_TOOL: updateTool,
  ADD_TOOL_USE: addToolUse,
  ADD_TOOL_RESULT: addToolResult,
  TOGGLE_TOOL_EXPAND: toggleToolExpand,
  SET_SESSION: setSession,
  UPDATE_USAGE: updateUsage,
  SET_NAV_MODE: setNavMode,
  SET_SELECTED_TOOL_INDEX: setSelectedToolIndex,
  SET_INPUT: setInput,
  SET_ERROR: setError,
  CLEAR_ERROR: clearError,
  SET_FLASH: setFlash,
  QUEUE_PERMISSION: queuePermission,
  RESOLVE_PERMISSION: resolvePermission,
  SET_AUTO_ALLOW: setAutoAllow,
  RESET: reset,
};

export function reducer(state: TuiState, action: Action): TuiState {
  const handler = handlers[action.type];
  return handler ? handler(state, action) : state;
}

// Helper to generate unique IDs
export function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

// Helper to create a user message
export function createUserMessage(content: string): Message {
  return {
    id: generateId(),
    role: "user",
    content: [{ type: "text", content }],
    timestamp: new Date(),
  };
}

// Helper to create an assistant message
export function createAssistantMessage(content: ContentBlock[]): Message {
  return {
    id: generateId(),
    role: "assistant",
    content,
    timestamp: new Date(),
  };
}
