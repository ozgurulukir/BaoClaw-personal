import React from "react";
import { Text, Box, useInput } from "ink";
import { colors, zen } from "../theme.js";
import { PendingPermission } from "../types.js";

export type PermissionDecision = "allow" | "allow_always" | "deny";

interface PermissionDialogProps {
  /** The request at the head of the queue; null renders nothing. */
  request: PendingPermission | null;
  onDecide: (decision: PermissionDecision) => void;
}

/**
 * Modal permission prompt — the TUI counterpart of the CLI's [y]/[a]/[n]
 * prompt. Mirrors HelpOverlay's visible-guard pattern: always mounted, it
 * renders nothing (and ignores keys) while `request` is null. Its useInput
 * is deliberately NOT gated on isStreaming (requests arrive mid-turn), so
 * the App suspends InputArea's handler while the dialog is open — Ink
 * delivers every keypress to all mounted useInput hooks with no precedence.
 */
export const PermissionDialog: React.FC<PermissionDialogProps> = ({
  request,
  onDecide,
}) => {
  useInput((inputChar, key) => {
    if (!request) return;
    if (inputChar === "y") {
      onDecide("allow");
    } else if (inputChar === "a") {
      onDecide("allow_always");
    } else if (inputChar === "n" || key.escape) {
      onDecide("deny");
    }
  });

  if (!request) return null;

  return (
    <Box
      flexDirection="column"
      borderStyle="double"
      borderColor={colors.status.warning}
      paddingX={1}
    >
      <Text color={colors.status.warning} bold>
        🔐 Permission Request
      </Text>
      <Text color={colors.text.primary} bold>
        {request.toolName}
      </Text>
      {request.inputPreview && (
        <Text color={colors.text.dim} wrap="truncate-end">
          {request.inputPreview}
        </Text>
      )}
      <Text>
        <Text color={colors.status.success}>[y]</Text>
        <Text color={colors.text.primary}> Allow </Text>
        <Text color={colors.status.success}>[a]</Text>
        <Text color={colors.text.primary}> Always </Text>
        <Text color={colors.status.error}>[n/Esc]</Text>
        <Text color={colors.text.primary}> Deny</Text>
        <Text color={colors.text.muted}>
          {" "}
          {zen.separator} auto-deny after 60s
        </Text>
      </Text>
    </Box>
  );
};

export default PermissionDialog;
