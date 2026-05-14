use serde::{Deserialize, Serialize};

/// Canonical transport object for a single Claude Code JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub session_id: String,
    /// Zero-based line position within the source file.
    pub line_index: usize,
    /// Raw parsed Claude Code event record.
    pub entry: serde_json::Value,
    /// Server timestamp in RFC 3339 format.
    pub observed_at: String,
    /// Short, operator-friendly description of the event.
    pub summary: String,
}

impl TraceEvent {
    /// Construct a `TraceEvent` from a raw JSON value, enriching it with
    /// server-side metadata.
    pub fn from_raw(session_id: &str, line_index: usize, raw: serde_json::Value) -> Self {
        Self {
            session_id: session_id.to_owned(),
            line_index,
            observed_at: chrono::Utc::now().to_rfc3339(),
            summary: summarise(&raw),
            entry: raw,
        }
    }
}

/// Produce a short human-readable summary for a raw Claude Code JSONL record.
pub fn summarise(val: &serde_json::Value) -> String {
    let event_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match event_type {
        "user" => {
            let preview = extract_text_preview(val, 80);
            format!("👤 User: {preview}")
        }
        "assistant" => {
            let cost = val
                .get("costUSD")
                .and_then(|v| v.as_f64())
                .map(|c| format!(" [${c:.4}]"))
                .unwrap_or_default();

            let input_tokens = val
                .pointer("/message/usage/input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = val
                .pointer("/message/usage/output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let tokens = if input_tokens > 0 || output_tokens > 0 {
                format!(" [{input_tokens}↑ {output_tokens}↓]")
            } else {
                String::new()
            };

            let preview = extract_text_preview(val, 60);
            format!("🤖 Assistant{cost}{tokens}: {preview}")
        }
        "tool_use" => {
            let name = val
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("🔧 Tool use: {name}")
        }
        "tool_result" => {
            let id = val
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("📦 Tool result: {id}")
        }
        "system" => {
            let preview = extract_text_preview(val, 80);
            format!("⚙️  System: {preview}")
        }
        other => {
            format!("❓ {other}")
        }
    }
}

/// Extract a printable text preview from a JSON value.
/// Checks common locations: top-level `content` string, first `content` block
/// with `text`, or the `text` field directly.
fn extract_text_preview(val: &serde_json::Value, max_len: usize) -> String {
    // Try direct text field
    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
        return truncate(text, max_len);
    }

    // Try content as plain string
    if let Some(s) = val.get("content").and_then(|v| v.as_str()) {
        return truncate(s, max_len);
    }

    // Try content as array, grab the first text block
    if let Some(arr) = val.get("content").and_then(|v| v.as_array()) {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    return truncate(text, max_len);
                }
            }
        }
    }

    // Try message.content
    if let Some(content) = val.pointer("/message/content") {
        if let Some(s) = content.as_str() {
            return truncate(s, max_len);
        }
        if let Some(arr) = content.as_array() {
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        return truncate(text, max_len);
                    }
                }
            }
        }
    }

    String::new()
}

fn truncate(s: &str, max_len: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.len() <= max_len {
        s
    } else {
        // Truncate at a char boundary
        let mut end = max_len;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_summarise_user() {
        let val = json!({ "type": "user", "content": "Hello, world!" });
        let s = summarise(&val);
        assert!(s.starts_with("👤 User:"), "got: {s}");
        assert!(s.contains("Hello, world!"));
    }

    #[test]
    fn test_summarise_assistant_with_cost() {
        let val = json!({
            "type": "assistant",
            "costUSD": 0.0023,
            "message": {
                "content": [{ "type": "text", "text": "Here is the code" }],
                "usage": { "input_tokens": 100, "output_tokens": 50 }
            }
        });
        let s = summarise(&val);
        assert!(s.starts_with("🤖 Assistant"), "got: {s}");
        assert!(s.contains("$0.0023"), "got: {s}");
        assert!(s.contains("100↑"), "got: {s}");
        assert!(s.contains("50↓"), "got: {s}");
    }

    #[test]
    fn test_summarise_assistant_no_cost() {
        let val = json!({ "type": "assistant", "message": {} });
        let s = summarise(&val);
        assert!(s.starts_with("🤖 Assistant"), "got: {s}");
    }

    #[test]
    fn test_summarise_tool_use() {
        let val = json!({ "type": "tool_use", "name": "read_file" });
        let s = summarise(&val);
        assert_eq!(s, "🔧 Tool use: read_file");
    }

    #[test]
    fn test_summarise_tool_result() {
        let val = json!({ "type": "tool_result", "tool_use_id": "abc123" });
        let s = summarise(&val);
        assert_eq!(s, "📦 Tool result: abc123");
    }

    #[test]
    fn test_summarise_system() {
        let val = json!({ "type": "system", "content": "System initialised" });
        let s = summarise(&val);
        assert!(s.starts_with("⚙️"), "got: {s}");
        assert!(s.contains("System initialised"), "got: {s}");
    }

    #[test]
    fn test_summarise_unknown() {
        let val = json!({ "type": "something_new" });
        let s = summarise(&val);
        assert!(s.contains("something_new"), "got: {s}");
    }

    #[test]
    fn test_summarise_missing_type() {
        let val = json!({ "foo": "bar" });
        let s = summarise(&val);
        assert!(s.contains("unknown"), "got: {s}");
    }

    #[test]
    fn test_truncate_long_text() {
        let long = "a".repeat(200);
        let val = json!({ "type": "user", "content": long });
        let s = summarise(&val);
        // Summary should not be excessively long
        assert!(s.len() < 200, "summary too long: {}", s.len());
    }

    #[test]
    fn test_trace_event_from_raw() {
        let raw = json!({ "type": "user", "content": "hi" });
        let event = TraceEvent::from_raw("session-1", 0, raw);
        assert_eq!(event.session_id, "session-1");
        assert_eq!(event.line_index, 0);
        assert!(!event.observed_at.is_empty());
        assert!(!event.summary.is_empty());
    }
}
