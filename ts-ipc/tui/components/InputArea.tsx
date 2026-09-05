import React, { useState } from "react";
import { Text, Box, useInput } from "ink";
import { colors, zen } from "../theme.js";

interface InputAreaProps {
  input: string;
  isStreaming: boolean;
  mode?: "insert" | "normal";
  /** While the permission dialog is open, all keys here are suppressed. */
  suspendInput?: boolean;
  onSubmit: (text: string) => void;
  onInputChange: (text: string) => void;
  onToggleMode?: () => void;
  onNavigate?: (delta: number) => void;
  onToggleExpand?: () => void;
  onCopy?: () => void;
  onToggleHelp?: () => void;
  onToggleAutoAllow?: () => void;
}

export const InputArea: React.FC<InputAreaProps> = ({
  input,
  isStreaming,
  mode = "insert",
  suspendInput = false,
  onSubmit,
  onInputChange,
  onToggleMode,
  onNavigate,
  onToggleExpand,
  onCopy,
  onToggleHelp,
  onToggleAutoAllow,
}) => {
  const [cursorVisible, setCursorVisible] = useState(true);

  // Blink cursor
  React.useEffect(() => {
    const timer = setInterval(() => {
      setCursorVisible((v) => !v);
    }, 500);
    return () => clearInterval(timer);
  }, []);

  // Handle keyboard input
  useInput((inputChar, key) => {
    if (isStreaming || suspendInput) return;

    if (mode === "normal") {
      // Normal / Navigation mode keybindings
      if (key.escape || inputChar === "i" || inputChar === "a") {
        onToggleMode?.();
      } else if (key.return) {
        onToggleExpand?.();
      } else if (inputChar === " ") {
        onToggleExpand?.();
      } else if (inputChar === "j" || key.downArrow) {
        onNavigate?.(1);
      } else if (inputChar === "k" || key.upArrow) {
        onNavigate?.(-1);
      } else if (inputChar === "y" || (key.ctrl && inputChar === "y")) {
        onCopy?.();
      } else if (inputChar === "?" || (key.ctrl && inputChar === "h")) {
        onToggleHelp?.();
      } else if (inputChar === "p") {
        onToggleAutoAllow?.();
      }
      return;
    }

    // Insert Mode
    if (key.escape) {
      onToggleMode?.();
      return;
    }

    if (key.return) {
      if (input.trim()) {
        onSubmit(input.trim());
        onInputChange("");
      }
    } else if (key.backspace || key.delete) {
      onInputChange(input.slice(0, -1));
    } else if (key.ctrl && inputChar === "y") {
      onCopy?.();
    } else if (key.ctrl && inputChar === "h") {
      onToggleHelp?.();
    } else if (key.ctrl && inputChar === "p") {
      onToggleAutoAllow?.();
    } else if (!key.ctrl && !key.meta && inputChar) {
      onInputChange(input + inputChar);
    }
  });

  const displayText =
    mode === "normal"
      ? "Normal Mode (press 'i' to type, 'j/k' to navigate tools, 'Space' to expand)"
      : input || "Type your message...";

  const displayColor =
    mode === "normal"
      ? colors.status.warning
      : input
        ? colors.text.primary
        : colors.text.dim;

  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={mode === "normal" ? colors.status.warning : colors.border}
      paddingX={1}
    >
      {/* Input row */}
      <Box>
        <Text
          color={mode === "normal" ? colors.status.warning : colors.role.user}
          bold
        >
          {mode === "normal" ? "NAV" : "You"}
        </Text>
        <Text color={colors.text.muted}> {zen.separator} </Text>
        <Text color={displayColor}>{displayText}</Text>
        {mode === "insert" && cursorVisible && input && (
          <Text color={colors.text.primary}>█</Text>
        )}
      </Box>

      {/* Help text */}
      <Box justifyContent="space-between">
        <Box>
          <Text color={colors.text.dim}>
            {mode === "normal"
              ? "Space/Enter: Toggle Tool • j/k: Navigate • y: Copy • p: Auto-allow • i: Insert"
              : "Enter: Send • Esc: Normal Mode • Ctrl+Y: Copy • Ctrl+P: Auto-allow • Ctrl+H: Help"}
          </Text>
        </Box>
        <Box>
          <Text color={colors.text.muted}>[{mode.toUpperCase()}]</Text>
        </Box>
      </Box>
    </Box>
  );
};

export default InputArea;
