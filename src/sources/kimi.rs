//! Kimi Code adapter.
//!
//! Kimi Code (Moonshot AI's terminal agent) writes session logs under
//! `~/.kimi/` in a JSONL shape that is broadly Claude-Code-compatible
//! (`type` / `sessionId` / `message.content` blocks / `usage`), with Kimi
//! (k2/moonshot) model names. We reuse the Claude adapter for structure and
//! then re-estimate cost against the Kimi pricing family.

use serde_json::Value;

use crate::sources::{estimate_cost_for, AgentSource, Enrichment};

/// Normalise one Kimi Code record.
pub fn enrich(raw: &Value) -> Enrichment {
    let mut e = crate::sources::claude::enrich(raw);
    // Kimi logs don't always carry `costUSD`; the Claude adapter only sets a
    // cost when it can estimate one, but it does so against Claude pricing.
    // Recompute against Kimi pricing whenever we have usage and no explicit
    // cost was provided by the record.
    let explicit = raw
        .get("costUSD")
        .or_else(|| raw.get("cost"))
        .and_then(|v| v.as_f64());
    e.cost_usd = match (explicit, &e.usage) {
        (Some(c), _) => Some(c),
        (None, Some(u)) => Some(estimate_cost_for(AgentSource::Kimi, e.model.as_deref(), u)),
        (None, None) => None,
    };
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_shaped_record_is_parsed() {
        let e = enrich(&json!({
            "type": "assistant",
            "sessionId": "k1",
            "message": {
                "model": "kimi-k2-0905-preview",
                "content": [{"type":"text","text":"hi"}],
                "usage": {"input_tokens": 1_000_000, "output_tokens": 0}
            }
        }));
        assert_eq!(e.event_type, "assistant");
        assert_eq!(e.model.as_deref(), Some("kimi-k2-0905-preview"));
        // Kimi pricing: $0.60 / Mtok input.
        let c = e.cost_usd.unwrap();
        assert!((c - 0.60).abs() < 0.001, "got {c}");
    }

    #[test]
    fn explicit_cost_respected() {
        let e = enrich(
            &json!({"type":"assistant","costUSD":0.01,"message":{"usage":{"input_tokens":5}}}),
        );
        assert_eq!(e.cost_usd, Some(0.01));
    }
}
