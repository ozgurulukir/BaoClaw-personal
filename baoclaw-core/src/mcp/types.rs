//! Wire-level DTOs for the MCP client and the mapping between MCP tool-call
//! results and the engine's `ToolResult` shape.

use serde_json::{json, Value};

/// One tool advertised by a server via `tools/list`.
#[derive(Clone, Debug)]
pub struct McpToolDef {
    /// ORIGINAL remote name — sent verbatim in `tools/call`.
    pub name: String,
    pub description: Option<String>,
    /// Raw MCP inputSchema object (passed through to the API untouched).
    pub input_schema: Value,
}

/// Parse a `tools/list` result. Entries without a string `name` are skipped
/// (lenient: a single malformed entry must not drop the whole catalog).
pub fn parse_tool_defs(result: &Value) -> Vec<McpToolDef> {
    let empty = Vec::new();
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(Value::as_str)?.to_string();
            Some(McpToolDef {
                name,
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .map(String::from),
                input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
            })
        })
        .collect()
}

/// Outcome of a `tools/call` mapped into the engine's `ToolResult` payload.
#[derive(Clone, Debug)]
pub struct CallToolOutcome {
    pub data: Value,
    pub is_error: bool,
}

/// Map a `tools/call` result to a `ToolResult` payload.
///
/// - `result.isError == true` → `is_error = true` (the call executed but the
///   server reports failure; the model can recover, so this stays a RESULT).
/// - Text-only content arrays collapse to a single string.
/// - Mixed content (images etc., which gateways render in MCP shape
///   `{type:"image",data,mimeType}`) keeps the full array alongside the text.
/// - Missing/empty content falls back to `structuredContent`, else empty text.
pub fn map_call_result(result: &Value) -> CallToolOutcome {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let content = result
        .get("content")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty());

    let data = match content {
        Some(items) => {
            let mut texts: Vec<&str> = Vec::new();
            let mut has_non_text = false;
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    texts.push(item.get("text").and_then(Value::as_str).unwrap_or(""));
                } else {
                    has_non_text = true;
                }
            }
            let joined = texts.join("\n");
            if has_non_text {
                json!({ "text": joined, "content": items })
            } else {
                Value::String(joined)
            }
        }
        None => result
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
    };

    CallToolOutcome { data, is_error }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_entries_without_name() {
        let result = json!({ "tools": [
            {"name": "good", "description": "d", "inputSchema": {"type": "object"}},
            {"description": "no name"},
        ]});
        let defs = parse_tool_defs(&result);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "good");
        assert_eq!(defs[0].input_schema, json!({"type": "object"}));
    }

    #[test]
    fn parse_tolerates_missing_schema_and_description() {
        let defs = parse_tool_defs(&json!({ "tools": [{"name": "bare"}]}));
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].input_schema, json!({}));
        assert!(defs[0].description.is_none());
    }

    #[test]
    fn text_only_content_collapses_to_string() {
        let outcome = map_call_result(&json!({
            "content": [
                {"type": "text", "text": "line1"},
                {"type": "text", "text": "line2"}
            ],
            "isError": false
        }));
        assert_eq!(outcome.data, json!("line1\nline2"));
        assert!(!outcome.is_error);
    }

    #[test]
    fn mixed_content_keeps_full_array() {
        let content = json!([
            {"type": "image", "data": "base64==", "mimeType": "image/png"},
            {"type": "text", "text": "caption"}
        ]);
        let outcome = map_call_result(&json!({ "content": content }));
        assert_eq!(outcome.data["text"], json!("caption"));
        assert_eq!(outcome.data["content"], content);
    }

    #[test]
    fn is_error_flag_is_honored() {
        let outcome = map_call_result(&json!({
            "content": [{"type": "text", "text": "boom"}],
            "isError": true
        }));
        assert!(outcome.is_error);
        assert_eq!(outcome.data, json!("boom"));
    }

    #[test]
    fn structured_content_used_when_content_missing() {
        let outcome = map_call_result(&json!({"structuredContent": {"key": "value"}}));
        assert_eq!(outcome.data, json!({"key": "value"}));
    }

    #[test]
    fn empty_result_maps_to_empty_string() {
        let outcome = map_call_result(&json!({}));
        assert_eq!(outcome.data, json!(""));
        assert!(!outcome.is_error);
    }
}
