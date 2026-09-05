import React, {
  useReducer,
  useEffect,
  useState,
  useCallback,
  useRef,
} from "react";
import { Box, useApp, useInput, Text } from "ink";
import { IpcClient } from "../../client.js";
import type { ControlChannel } from "../../controlChannel.js";
import StatusBar from "./StatusBar.js";
import MessageList from "./MessageList.js";
import StreamOutput from "./StreamOutput.js";
import InputArea from "./InputArea.js";
import HelpOverlay from "./HelpOverlay.js";
import ToolsPanel from "./ToolsPanel.js";
import PermissionDialog, {
  type PermissionDecision,
} from "./PermissionDialog.js";
import { copyToClipboard } from "../clipboard.js";
import {
  reducer,
  INITIAL_STATE,
  createUserMessage,
  createAssistantMessage,
} from "../state.js";
import { subscribeToEvents, sendMessage } from "../ipc.js";
import { colors } from "../theme.js";
import { ContentBlock, PendingPermission } from "../types.js";

interface AppProps {
  client: IpcClient;
  model: string;
  /** Dedicated connection for mid-turn permission responses. */
  control: ControlChannel;
}

export const App: React.FC<AppProps> = ({ client, model, control }) => {
  const [state, dispatch] = useReducer(reducer, {
    ...INITIAL_STATE,
    session: {
      id: "init",
      model,
      status: "idle" as const,
    },
  });
  const [showHelp, setShowHelp] = useState(false);
  const { exit } = useApp();

  // Use ref to track streaming content for the result handler
  const streamingContentRef = useRef("");
  const thinkingContentRef = useRef("");

  // Update refs when state changes
  useEffect(() => {
    streamingContentRef.current = state.streamingContent;
    thinkingContentRef.current = state.thinkingContent;
  }, [state.streamingContent, state.thinkingContent]);

  // Seed the auto-allow knob from the daemon-persisted permission context
  // (permissions.auto_allow_channels.tui). Absent key or transient failure
  // keeps the ON default, so a fetch error can never silently disable it.
  useEffect(() => {
    control
      .request<{ auto_allow_channels?: Record<string, boolean> }>(
        "permissions.info",
      )
      .then((ctx) => {
        dispatch({
          type: "SET_AUTO_ALLOW",
          payload: ctx?.auto_allow_channels?.tui ?? true,
        });
      })
      .catch(() => {});
  }, [control]);

  // ── Permission queue handling ──
  const head = state.pendingPermissions[0] ?? null;
  const dialogOpen = !state.autoAllow && state.pendingPermissions.length > 0;

  const decide = useCallback(
    async (request: PendingPermission, decision: PermissionDecision) => {
      try {
        const res = await control.request<{ delivered: boolean }>(
          "permissionResponse",
          {
            tool_use_id: request.toolUseId,
            decision,
            ...(decision === "allow_always" ? { rule: request.toolName } : {}),
          },
        );
        const delivered = res?.delivered === true;
        dispatch({ type: "RESOLVE_PERMISSION", payload: request.toolUseId });
        if (!delivered) {
          dispatch({
            type: "SET_FLASH",
            payload: "Permission request was already handled elsewhere",
          });
        }
      } catch {
        // Transport failure: the daemon's gate is still parked, so keep the
        // request queued and let the user retry (or the timer re-deny).
        dispatch({
          type: "SET_FLASH",
          payload: "Failed to send permission response",
        });
      }
    },
    [control],
  );

  // The queue head owns auto-allow and the 60s local deny timer. Keyed on
  // the head's id so each request gets a fresh timer and the timer is
  // cancelled on any decision.
  useEffect(() => {
    if (!head) return;
    if (state.autoAllow) {
      void decide(head, "allow");
      return;
    }
    const timer = setTimeout(() => {
      void decide(head, "deny");
    }, 60_000);
    return () => clearTimeout(timer);
  }, [head, state.autoAllow, decide]);

  // Toggle the persisted auto-allow knob (p in normal mode, Ctrl+P anywhere).
  const toggleAutoAllow = useCallback(() => {
    const next = !state.autoAllow;
    dispatch({ type: "SET_AUTO_ALLOW", payload: next });
    control
      .request("permissions.setAutoAllow", { channel: "tui", enabled: next })
      .catch(() => {
        dispatch({ type: "SET_AUTO_ALLOW", payload: !next });
        dispatch({
          type: "SET_FLASH",
          payload: "Failed to persist auto-allow toggle",
        });
      });
  }, [control, state.autoAllow]);

  useInput((inputChar, key) => {
    // Dialog modality: y/a/n belong to the PermissionDialog while it's open.
    if (state.pendingPermissions.length > 0) return;
    if (key.ctrl && inputChar === "p") {
      toggleAutoAllow();
    } else if (state.mode === "normal" && inputChar === "p") {
      toggleAutoAllow();
    }
  });

  // Subscribe to IPC events
  useEffect(() => {
    const unsubscribe = subscribeToEvents(client, dispatch);
    return unsubscribe;
  }, [client]);

  // Handle result event - add message only if streamingContent is non-empty
  useEffect(() => {
    const handler = (params: unknown) => {
      // Only add message if we have streaming content
      const content = streamingContentRef.current.trim();
      const thinking = thinkingContentRef.current.trim();

      if (content) {
        const blocks: ContentBlock[] = [{ type: "text", content }];
        if (thinking) {
          blocks.unshift({ type: "thinking", content: thinking });
        }
        const msg = createAssistantMessage(blocks);
        dispatch({ type: "ADD_MESSAGE", payload: msg });
      }

      // Reset streaming state
      dispatch({ type: "SET_STREAMING", payload: false });
    };

    const unsub = client.onNotification("stream/event", (params) => {
      const p = params as { type?: string };
      if (p.type === "result") {
        handler(params);
      }
    });
    return unsub;
  }, [client]);

  // Handle clipboard copy action
  const handleCopy = useCallback(() => {
    let textToCopy = "";
    if (
      state.currentTools.length > 0 &&
      state.selectedToolIndex < state.currentTools.length
    ) {
      const tool = state.currentTools[state.selectedToolIndex];
      textToCopy = tool.output || JSON.stringify(tool.input || {}, null, 2);
    } else if (state.messages.length > 0) {
      const lastMsg = state.messages[state.messages.length - 1];
      textToCopy = lastMsg.content.map((b) => b.content).join("\n");
    }

    if (textToCopy) {
      const success = copyToClipboard(textToCopy);
      if (success) {
        dispatch({ type: "SET_FLASH", payload: "✓ Copied to clipboard!" });
        setTimeout(() => {
          dispatch({ type: "SET_FLASH", payload: null });
        }, 2500);
      }
    }
  }, [state.currentTools, state.selectedToolIndex, state.messages]);

  const handleSubmit = useCallback(
    async (text: string) => {
      // Add user message
      const userMsg = createUserMessage(text);
      dispatch({ type: "ADD_MESSAGE", payload: userMsg });
      dispatch({ type: "SET_STREAMING", payload: true });

      try {
        await sendMessage(client, text);
      } catch (err) {
        const error = err as Error;
        dispatch({ type: "SET_ERROR", payload: error.message });
      }
    },
    [client],
  );

  const handleInputChange = useCallback((text: string) => {
    dispatch({ type: "SET_INPUT", payload: text });
  }, []);

  const handleToggleMode = useCallback(() => {
    dispatch({
      type: "SET_NAV_MODE",
      payload: state.mode === "insert" ? "normal" : "insert",
    });
  }, [state.mode]);

  const handleNavigate = useCallback(
    (delta: number) => {
      dispatch({
        type: "SET_SELECTED_TOOL_INDEX",
        payload: state.selectedToolIndex + delta,
      });
    },
    [state.selectedToolIndex],
  );

  const handleToggleExpand = useCallback(() => {
    dispatch({
      type: "TOGGLE_TOOL_EXPAND",
      payload: { index: state.selectedToolIndex },
    });
  }, [state.selectedToolIndex]);

  return (
    <Box flexDirection="column" width="100%" height="100%" padding={1}>
      {/* Status bar with live context gauge & mode badge */}
      <StatusBar
        session={state.session}
        isStreaming={state.isStreaming}
        mode={state.mode}
        usage={state.usage}
        flashMessage={state.flashMessage}
        autoAllow={state.autoAllow}
      />

      {/* Messages area */}
      <Box
        flexGrow={1}
        flexDirection="column"
        borderStyle="round"
        borderColor={colors.border}
        padding={1}
      >
        <MessageList messages={state.messages} />

        {/* Streaming output */}
        {state.isStreaming && (
          <StreamOutput
            content={state.streamingContent}
            thinking={state.thinkingContent}
            tools={state.currentTools}
          />
        )}

        {/* Collapsible Tools panel */}
        {!state.isStreaming && state.currentTools.length > 0 && (
          <ToolsPanel
            tools={state.currentTools}
            selectedIdx={state.selectedToolIndex}
            isNormalMode={state.mode === "normal"}
          />
        )}

        {/* Error display */}
        {state.error && (
          <Box
            borderStyle="round"
            borderColor={colors.status.error}
            padding={1}
          >
            <Text color={colors.status.error}>Error: {state.error}</Text>
          </Box>
        )}
      </Box>

      {/* Modal Input area — suspended while the permission dialog holds the
          keyboard (Ink broadcasts keys to every useInput handler). */}
      <InputArea
        input={state.input}
        isStreaming={state.isStreaming}
        mode={state.mode}
        suspendInput={dialogOpen}
        onSubmit={handleSubmit}
        onInputChange={handleInputChange}
        onToggleMode={handleToggleMode}
        onNavigate={handleNavigate}
        onToggleExpand={handleToggleExpand}
        onCopy={handleCopy}
        onToggleHelp={() => setShowHelp((h) => !h)}
        onToggleAutoAllow={toggleAutoAllow}
      />

      {/* Help overlay — suppressed while the permission dialog is open so
          its swallow-all key handler can't eat the y/a/n decision keys. */}
      <HelpOverlay
        visible={showHelp && !dialogOpen}
        onClose={() => setShowHelp(false)}
      />

      {/* Permission prompt — modal, queue head first */}
      <PermissionDialog
        request={dialogOpen ? head : null}
        onDecide={(decision) => {
          if (head) void decide(head, decision);
        }}
      />
    </Box>
  );
};

export default App;
