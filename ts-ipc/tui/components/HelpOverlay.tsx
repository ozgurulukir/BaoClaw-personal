import React from "react";
import { Text, Box, useInput } from "ink";
import { colors, zen } from "../theme.js";

interface HelpOverlayProps {
  visible: boolean;
  onClose: () => void;
}

const shortcuts = [
  { key: "Enter", action: "Send message (Insert Mode)" },
  { key: "Esc", action: "Toggle between Insert & Normal Mode" },
  { key: "i / a", action: "Enter Insert Mode (from Normal Mode)" },
  { key: "j / k / ↓ / ↑", action: "Navigate tools & messages (Normal Mode)" },
  {
    key: "Space / Enter",
    action: "Expand / Collapse selected tool (Normal Mode)",
  },
  { key: "Ctrl+Y / y", action: "Copy output or message to Clipboard" },
  { key: "Ctrl+H / ?", action: "Toggle this Help Overlay" },
  {
    key: "Ctrl+P / p",
    action: "Toggle auto-allow of tool permissions (persisted)",
  },
  { key: "Ctrl+C", action: "Exit BaoClaw TUI" },
];

export const HelpOverlay: React.FC<HelpOverlayProps> = ({
  visible,
  onClose,
}) => {
  useInput((_input, _key) => {
    if (visible) {
      onClose();
    }
  });

  if (!visible) return null;

  return (
    <Box
      flexDirection="column"
      width="100%"
      borderStyle="double"
      borderColor={colors.status.info}
      padding={1}
      marginY={1}
    >
      <Box marginBottom={1}>
        <Text color={colors.status.info} bold>
          ⌨️ Keyboard Shortcuts & Modal Navigation
        </Text>
      </Box>

      {shortcuts.map((s, idx) => (
        <Box key={idx} marginBottom={0}>
          <Box width={20}>
            <Text color={colors.role.user} bold>
              {s.key}
            </Text>
          </Box>
          <Text color={colors.text.primary}>
            {zen.arrow} {s.action}
          </Text>
        </Box>
      ))}

      <Box marginTop={1}>
        <Text color={colors.status.warning}>
          Press any key to close this help overlay
        </Text>
      </Box>
    </Box>
  );
};

export default HelpOverlay;
