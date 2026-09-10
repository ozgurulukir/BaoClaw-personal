//! Minimal SSE event parser for MCP HTTP transports.
//!
//! Distinct from `api/client.rs`'s stream parser on purpose: the LLM
//! streaming path type-maps `data:` payloads and can ignore `event:` names,
//! while MCP's legacy HTTP+SSE transport routes on them (`endpoint`
//! announces the POST URL) and joins multi-line `data:` fields per the SSE
//! wire format. Keeping this separate leaves the model-streaming path
//! untouched.

/// One server-sent event: the `event:` name (if any) and the joined `data:`
/// payload.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Byte-stream to event-stream collector. Feed raw network chunks (which may
/// split UTF-8 sequences and events at arbitrary points); pop complete
/// events.
#[derive(Default)]
pub(crate) struct SseParser {
    /// Raw bytes not yet decodable (a chunk split a UTF-8 sequence).
    pending_bytes: Vec<u8>,
    /// Decoded text not yet forming a complete event (no blank line yet).
    buffer: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.pending_bytes.extend_from_slice(chunk);
        // Decode as much as possible; an incomplete trailing sequence stays
        // buffered until its continuation arrives.
        match std::str::from_utf8(&self.pending_bytes) {
            Ok(text) => {
                self.buffer.push_str(text);
                self.pending_bytes.clear();
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    let text =
                        std::str::from_utf8(&self.pending_bytes[..valid]).expect("validated slice");
                    self.buffer.push_str(text);
                    self.pending_bytes.drain(..valid);
                }
            }
        }
        // CRLF-only servers must not starve the blank-line split.
        if self.buffer.contains("\r\n") {
            self.buffer = self.buffer.replace("\r\n", "\n");
        }
    }

    /// Pop the next complete event, if the fed text contains one.
    pub fn next_event(&mut self) -> Option<SseEvent> {
        let sep = self.buffer.find("\n\n")?;
        let block: String = self.buffer.drain(..sep + 2).collect();
        parse_event_block(&block)
    }
}

/// Parse one blank-line-delimited event block. `event:` sets the name (last
/// one wins, per spec), multiple `data:` lines join with `\n`; comment
/// (`:`), `id:`, and `retry:` lines are ignored.
fn parse_event_block(block: &str) -> Option<SseEvent> {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    for line in block.lines() {
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest; // comment
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value.to_string()),
            _ => {}
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(parser: &mut SseParser) -> Vec<SseEvent> {
        std::iter::from_fn(|| parser.next_event()).collect()
    }

    #[test]
    fn single_event_with_name_and_data() {
        let mut p = SseParser::new();
        p.feed(b"event: endpoint\ndata: /message?sid=1\n\n");
        let events = collect(&mut p);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("endpoint"));
        assert_eq!(events[0].data, "/message?sid=1");
    }

    #[test]
    fn event_name_optional() {
        let mut p = SseParser::new();
        p.feed(b"data: {\"jsonrpc\":\"2.0\"}\n\n");
        let events = collect(&mut p);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data, "{\"jsonrpc\":\"2.0\"}");
    }

    #[test]
    fn multiline_data_joins_with_newline() {
        let mut p = SseParser::new();
        p.feed(b"data: line1\ndata: line2\n\n");
        let events = collect(&mut p);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn chunk_boundaries_and_crlf() {
        let mut p = SseParser::new();
        // Split mid-field, use CRLF line endings throughout.
        p.feed(b"event: messa");
        p.feed(b"ge\r\ndata: hel");
        p.feed(b"lo\r\n\r");
        p.feed(b"\n");
        let events = collect(&mut p);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn incomplete_utf8_sequence_is_buffered() {
        let mut p = SseParser::new();
        // "é" in UTF-8 is [0xC3, 0xA9] — split the sequence across feeds.
        p.feed(b"data: \xc3");
        assert!(p.next_event().is_none());
        p.feed(b"\xa9\n\n");
        let events = collect(&mut p);
        assert_eq!(events[0].data, "é");
    }

    #[test]
    fn comments_and_other_fields_ignored() {
        let mut p = SseParser::new();
        p.feed(b": keep-alive comment\nid: 42\nretry: 100\ndata: x\n\n");
        let events = collect(&mut p);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn incomplete_event_not_emitted() {
        let mut p = SseParser::new();
        p.feed(b"data: partial\n");
        assert!(p.next_event().is_none());
        p.feed(b"\ndata: more\n\n");
        let events = collect(&mut p);
        // The blank line terminates event 1 (data "partial"); "more" is
        // event 2.
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "partial");
        assert_eq!(events[1].data, "more");
    }
}
