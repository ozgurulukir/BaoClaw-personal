import React from "react";
import { Text, Box } from "ink";
import { colors, zen } from "../theme.js";
import { ToolProgress } from "../types.js";

interface ToolsPanelProps {
  tools: ToolProgress[];
  selectedIdx?: number;
  isNormalMode?: boolean;
}

export const ToolsPanel: React.FC<ToolsPanelProps> = ({
  tools,
  selectedIdx = 0,
  isNormalMode = false,
}) => {
  if (tools.length === 0) return null;

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={colors.tool}
      paddingX={1}
      marginY={1}
    >
      <Box marginBottom={1} justifyContent="space-between">
        <Box>
          <Text color={colors.tool} bold>
            🛠️ Tools ({tools.length})
          </Text>
        </Box>
        {isNormalMode && (
          <Box>
            <Text color={colors.text.dim}>
              [j/k: Navigate • Space/Enter: Toggle • y: Copy]
            </Text>
          </Box>
        )}
      </Box>

      {tools.map((tool, idx) => {
        const isSelected = isNormalMode && idx === selectedIdx;
        const isExpanded = tool.isExpanded ?? false;
        const lineCount = tool.output ? tool.output.split("\n").length : 0;

        const statusIcon =
          tool.status === "running"
            ? "◐"
            : tool.status === "error"
              ? zen.cross
              : zen.check;

        const statusColor =
          tool.status === "running"
            ? colors.status.warning
            : tool.status === "error"
              ? colors.status.error
              : colors.status.success;

        const inputStr =
          tool.input && typeof tool.input === "object"
            ? JSON.stringify(tool.input)
            : "";
        const inputPreview =
          inputStr && inputStr !== "{}"
            ? ` ${inputStr.slice(0, 40)}${inputStr.length > 40 ? "…" : ""}`
            : "";

        return (
          <Box key={tool.id || idx} flexDirection="column" marginY={0}>
            {/* Header row */}
            <Box>
              <Text
                color={isSelected ? colors.status.warning : colors.text.muted}
              >
                {isSelected ? "❯ " : "  "}
              </Text>
              <Text color={statusColor} bold>
                {isExpanded ? "▼ " : "▶ "}
              </Text>
              <Text
                color={isSelected ? colors.status.warning : colors.text.primary}
                bold
              >
                {tool.name}
              </Text>
              {inputPreview && (
                <Text color={colors.text.dim}>{inputPreview}</Text>
              )}
              <Text color={colors.text.muted}>
                {" "}
                ({lineCount} {lineCount === 1 ? "line" : "lines"})
              </Text>
              <Text color={statusColor}>
                {" "}
                [{statusIcon} {tool.status}]
              </Text>
            </Box>

            {/* Expanded Content Box */}
            {isExpanded && (
              <Box
                flexDirection="column"
                paddingLeft={3}
                paddingRight={1}
                marginY={1}
                borderStyle="single"
                borderColor={colors.border}
              >
                {Boolean(tool.input) && (
                  <Box flexDirection="column" marginBottom={1}>
                    <Text color={colors.markdown.keyword} bold>
                      Parameters:
                    </Text>
                    <Text color={colors.text.dim}>
                      {JSON.stringify(tool.input, null, 2)}
                    </Text>
                  </Box>
                )}
                <Box flexDirection="column">
                  <Text
                    color={
                      tool.status === "error"
                        ? colors.status.error
                        : colors.markdown.fn
                    }
                    bold
                  >
                    Output:
                  </Text>
                  <Text color={colors.text.secondary}>
                    {tool.output || "(no output returned)"}
                  </Text>
                </Box>
              </Box>
            )}
          </Box>
        );
      })}
    </Box>
  );
};

export default ToolsPanel;
