import React from "react";
import { Text, Box } from "ink";
import { colors, zen } from "../theme.js";
import { Session, TokenUsage } from "../types.js";

interface StatusBarProps {
  session: Session | null;
  isStreaming: boolean;
  mode?: "insert" | "normal";
  usage?: TokenUsage;
  flashMessage?: string | null;
  /** Persisted auto-allow knob; badge shows only when prompting is enforced. */
  autoAllow?: boolean;
}

function renderProgressBar(
  used: number,
  total: number,
  width: number = 8,
): string {
  if (total <= 0) return "░".repeat(width);
  const ratio = Math.min(1, Math.max(0, used / total));
  const filled = Math.round(ratio * width);
  const empty = width - filled;
  return "█".repeat(filled) + "░".repeat(empty);
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}k`;
  return String(n);
}

export const StatusBar: React.FC<StatusBarProps> = ({
  session,
  isStreaming,
  mode = "insert",
  usage,
  flashMessage,
  autoAllow,
}) => {
  const statusColor = isStreaming
    ? colors.status.streaming
    : session?.status === "error"
      ? colors.status.error
      : colors.status.success;

  const statusText = isStreaming
    ? "◐ Streaming"
    : session?.status === "error"
      ? "✗ Error"
      : "● Ready";

  const totalTokens = usage?.totalTokens || 0;
  const contextLimit = usage?.contextWindow || 200000;
  const tokenRatio = totalTokens / contextLimit;

  const gaugeColor =
    tokenRatio > 0.85
      ? colors.status.error
      : tokenRatio > 0.65
        ? colors.status.warning
        : colors.status.success;

  const progressBar = renderProgressBar(totalTokens, contextLimit, 6);
  const percentStr = `${Math.round(tokenRatio * 100)}%`;

  return (
    <Box
      width="100%"
      paddingX={1}
      borderStyle="single"
      borderColor={colors.border}
    >
      {/* Mode Badge */}
      <Box marginRight={1}>
        <Text
          color={
            mode === "insert" ? colors.status.success : colors.status.warning
          }
          bold
        >
          [{mode.toUpperCase()}]
        </Text>
      </Box>

      {/* Auto-allow badge — only rendered when prompting is enforced, so
          the default (auto-allow on) keeps the bar uncluttered. */}
      {autoAllow === false && (
        <Box marginRight={1}>
          <Text color={colors.status.warning} bold>
            [ASK]
          </Text>
        </Box>
      )}

      {/* Model name */}
      <Box marginRight={1}>
        <Text color={colors.status.info} bold>
          {session?.model || "BaoClaw"}
        </Text>
      </Box>

      {/* Flash notification or Context Gauge */}
      <Box flexGrow={1}>
        {flashMessage ? (
          <Text color={colors.status.warning} bold>
            {flashMessage}
          </Text>
        ) : (
          <Box>
            <Text color={colors.text.muted}>Ctx: </Text>
            <Text color={gaugeColor}>[{progressBar}] </Text>
            <Text color={colors.text.secondary}>
              {percentStr} ({formatTokens(totalTokens)}/
              {formatTokens(contextLimit)})
            </Text>
            {usage?.cost !== undefined && usage.cost > 0 && (
              <Text color={colors.text.dim}>
                {" "}
                {zen.separator} ${usage.cost.toFixed(4)}
              </Text>
            )}
          </Box>
        )}
      </Box>

      {/* Status indicator */}
      <Box width={14} justifyContent="flex-end">
        <Text color={statusColor}>{statusText}</Text>
      </Box>
    </Box>
  );
};

export default StatusBar;
