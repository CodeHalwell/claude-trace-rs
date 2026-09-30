//! Canonical, agent-neutral conversation messages.
//!
//! Every adapter translates its agent's native record shape into a
//! [`Message`]: a role plus Anthropic-style content blocks. Exporters and the
//! dashboard's conversation view consume only this model, so adding an agent
//! never means teaching every consumer a new wire format.
//!
//! Records that are not part of the transcript (session headers, token
//! counters, UI echoes) carry no message.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Who produced a message. Tool results travel on the user side, matching the
/// Anthropic Messages convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

/// One content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// A string, or an array of Anthropic content blocks.
        content: Value,
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
    Image {
        source: Value,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A transcript message in canonical form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn new(role: Role, content: Vec<Block>) -> Self {
        Self { role, content }
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self::new(role, vec![Block::Text { text: text.into() }])
    }

    /// `Some(self)` unless the message has no blocks, so adapters can write
    /// `Message::new(..).non_empty()` without special-casing empty turns.
    pub fn non_empty(self) -> Option<Self> {
        if self.content.is_empty() {
            None
        } else {
            Some(self)
        }
    }

    /// Concatenated text blocks, newline separated.
    pub fn plain_text(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for b in &self.content {
            if let Block::Text { text } = b {
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        parts.join("\n")
    }

    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            Block::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    pub fn tool_results(&self) -> impl Iterator<Item = (&str, &Value, bool)> {
        self.content.iter().filter_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id.as_str(), content, *is_error)),
            _ => None,
        })
    }

    pub fn has_text(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty()))
    }

    /// Parse an Anthropic Messages `content` value (a string or an array of
    /// blocks). Unknown block kinds are dropped; server-side tool blocks are
    /// folded into the ordinary tool_use / tool_result pair.
    pub fn from_anthropic(role: Role, content: &Value) -> Option<Self> {
        Self::new(role, anthropic_blocks(content)).non_empty()
    }

    /// Parse one OpenAI Chat Completions message (`{role, content,
    /// tool_calls, tool_call_id}`), including the `reasoning_content` field
    /// used by DeepSeek/Qwen/Kimi-style providers.
    pub fn from_openai(msg: &Value) -> Option<Self> {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "tool" || role == "function" {
            let id = msg
                .get("tool_call_id")
                .or_else(|| msg.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let content = match msg.get("content") {
                Some(Value::String(s)) => Value::String(s.clone()),
                Some(Value::Array(parts)) => Value::String(openai_parts_text(parts)),
                Some(Value::Null) | None => Value::String(String::new()),
                Some(other) => Value::String(other.to_string()),
            };
            return Some(Self::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content,
                    is_error: false,
                }],
            ));
        }

        let role = match role {
            "assistant" | "model" => Role::Assistant,
            "system" | "developer" => Role::System,
            _ => Role::User,
        };
        let mut blocks = Vec::new();
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = msg.get(key).and_then(Value::as_str) {
                if !r.trim().is_empty() {
                    blocks.push(Block::Thinking {
                        thinking: r.to_owned(),
                    });
                }
            }
        }
        match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => blocks.push(Block::Text { text: s.clone() }),
            Some(Value::Array(parts)) => blocks.extend(openai_part_blocks(parts)),
            _ => {}
        }
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                blocks.push(openai_tool_call(call));
            }
        }
        if let Some(fc) = msg.get("function_call") {
            blocks.push(openai_tool_call(&json!({ "function": fc })));
        }
        Self::new(role, blocks).non_empty()
    }
}

/// Anthropic content → blocks.
pub fn anthropic_blocks(content: &Value) -> Vec<Block> {
    match content {
        Value::String(s) if !s.is_empty() => vec![Block::Text { text: s.clone() }],
        Value::Array(arr) => arr.iter().filter_map(anthropic_block).collect(),
        _ => Vec::new(),
    }
}

fn anthropic_block(b: &Value) -> Option<Block> {
    let kind = b.get("type").and_then(Value::as_str).unwrap_or("");
    let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    match kind {
        "text" | "input_text" | "output_text" => {
            let text = s("text");
            (!text.is_empty()).then_some(Block::Text { text })
        }
        "thinking" => {
            let thinking = b
                .get("thinking")
                .or_else(|| b.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            (!thinking.is_empty()).then_some(Block::Thinking { thinking })
        }
        "tool_use" | "server_tool_use" => Some(Block::ToolUse {
            id: s("id"),
            name: s("name"),
            input: b.get("input").cloned().unwrap_or_else(|| json!({})),
        }),
        "tool_result" | "web_search_tool_result" | "code_execution_tool_result" => {
            Some(Block::ToolResult {
                tool_use_id: s("tool_use_id"),
                content: b.get("content").cloned().unwrap_or(Value::Null),
                is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            })
        }
        "image" => Some(Block::Image {
            source: b.get("source").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    }
}

fn openai_part_blocks(parts: &[Value]) -> Vec<Block> {
    parts
        .iter()
        .filter_map(|p| {
            let kind = p.get("type").and_then(Value::as_str).unwrap_or("text");
            match kind {
                "text" | "input_text" | "output_text" => {
                    let t = p.get("text").and_then(Value::as_str)?;
                    (!t.is_empty()).then(|| Block::Text { text: t.to_owned() })
                }
                "image_url" | "input_image" => Some(Block::Image {
                    source: p.get("image_url").cloned().unwrap_or_else(|| p.clone()),
                }),
                // Anthropic-style blocks occasionally appear inside OpenAI
                // shaped logs (proxies, mixed-provider agents).
                _ => anthropic_block(p),
            }
        })
        .collect()
}

fn openai_parts_text(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn openai_tool_call(call: &Value) -> Block {
    let f = call.get("function").unwrap_or(call);
    Block::ToolUse {
        id: call
            .get("id")
            .or_else(|| call.get("call_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        name: f
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        input: parse_json_arguments(f.get("arguments").or_else(|| f.get("input"))),
    }
}

/// Tool arguments are frequently serialised as a JSON *string*; decode them
/// so every exporter sees structured input. Non-JSON strings are kept as-is.
pub fn parse_json_arguments(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        Some(other) => other.clone(),
        None => json!({}),
    }
}

/// Render a tool result's content (string or blocks) as plain text.
pub fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .map(|b| match b.get("text").and_then(Value::as_str) {
                Some(t) => t.to_owned(),
                None => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_string_and_blocks() {
        let m = Message::from_anthropic(Role::User, &json!("hi")).unwrap();
        assert_eq!(m.plain_text(), "hi");
        let m = Message::from_anthropic(
            Role::Assistant,
            &json!([
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"ok"},
                {"type":"tool_use","id":"t1","name":"Read","input":{"p":1}},
                {"type":"mystery"}
            ]),
        )
        .unwrap();
        assert_eq!(m.content.len(), 3);
        assert_eq!(m.tool_uses().next().unwrap().1, "Read");
    }

    #[test]
    fn empty_content_is_no_message() {
        assert!(Message::from_anthropic(Role::User, &json!("")).is_none());
        assert!(Message::from_anthropic(Role::User, &json!([])).is_none());
    }

    #[test]
    fn openai_assistant_with_tool_calls_and_reasoning() {
        let m = Message::from_openai(&json!({
            "role":"assistant",
            "content":"calling",
            "reasoning_content":"think first",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"bash","arguments":"{\"cmd\":\"ls\"}"}}]
        }))
        .unwrap();
        assert_eq!(m.role, Role::Assistant);
        assert!(matches!(m.content[0], Block::Thinking { .. }));
        let (id, name, input) = m.tool_uses().next().unwrap();
        assert_eq!((id, name), ("c1", "bash"));
        assert_eq!(input["cmd"], "ls");
    }

    #[test]
    fn openai_tool_message_becomes_user_tool_result() {
        let m = Message::from_openai(&json!({"role":"tool","tool_call_id":"c1","content":"done"}))
            .unwrap();
        assert_eq!(m.role, Role::User);
        let (id, content, _) = m.tool_results().next().unwrap();
        assert_eq!(id, "c1");
        assert_eq!(content, &json!("done"));
    }

    #[test]
    fn blocks_serialise_in_anthropic_shape() {
        let m = Message::new(
            Role::Assistant,
            vec![Block::ToolUse {
                id: "a".into(),
                name: "Bash".into(),
                input: json!({}),
            }],
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"][0]["type"], "tool_use");
        assert_eq!(v["content"][0]["name"], "Bash");
    }
}
