use protocol_core::{FinishReason, InferenceResponse};
use serde_json::{Value, json};

// ─── Response formatting ────────────────────────────────────────────────

pub(crate) fn anthropic_messages_json(response: InferenceResponse) -> Value {
    let mut content = Vec::new();

    // Add text content if present
    if !response.output_text.is_empty() {
        content.push(json!({
            "type": "text",
            "text": response.output_text
        }));
    }

    // Add tool calls as tool_use blocks
    if !response.tool_calls.is_empty() {
        for tc in &response.tool_calls {
            let input: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": input
            }));
        }
    }

    json!({
        "id": response.id,
        "type": "message",
        "role": "assistant",
        "model": response.model,
        "content": content,
        "stop_reason": stop_reason_label(&response.finish_reason),
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens
        }
    })
}

fn stop_reason_label(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "end_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::ToolCalls => "tool_use",
        FinishReason::ContentFilter => "end_turn",
        FinishReason::Error => "stop_sequence",
    }
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use protocol_core::{TokenUsage, ToolCall};

    // Test 10: Basic text response
    #[test]
    fn format_basic_text_response() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: "Hello".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };

        let json = anthropic_messages_json(resp);
        assert_eq!(json["id"], "msg_123");
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hello");
        assert_eq!(json["stop_reason"], "end_turn");
        assert_eq!(json["usage"]["input_tokens"], 10);
        assert_eq!(json["usage"]["output_tokens"], 5);
    }

    // Test 11: Response with tool_use
    #[test]
    fn format_response_tool_use() {
        let resp = InferenceResponse {
            id: "msg_456".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: String::new(),
            finish_reason: FinishReason::ToolCalls,
            tool_calls: vec![ToolCall {
                id: "tu_1".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city":"Shanghai"}"#.to_string(),
            }],
            usage: TokenUsage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };

        let json = anthropic_messages_json(resp);
        assert_eq!(json["content"][0]["type"], "tool_use");
        assert_eq!(json["content"][0]["id"], "tu_1");
        assert_eq!(json["content"][0]["name"], "get_weather");
        assert_eq!(json["content"][0]["input"]["city"], "Shanghai");
        assert_eq!(json["stop_reason"], "tool_use");
    }

    // Test 12: stop_reason end_turn
    #[test]
    fn format_stop_end_turn() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: "done".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["stop_reason"], "end_turn");
    }

    // Test 13: stop_reason max_tokens
    #[test]
    fn format_stop_max_tokens() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: "done".to_string(),
            finish_reason: FinishReason::Length,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["stop_reason"], "max_tokens");
    }

    // Test 14: stop_reason tool_use
    #[test]
    fn format_stop_tool_use() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: String::new(),
            finish_reason: FinishReason::ToolCalls,
            tool_calls: vec![ToolCall {
                id: "tu_1".to_string(),
                name: "x".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["stop_reason"], "tool_use");
    }

    // Test 15: stop_reason content_filter
    #[test]
    fn format_stop_content_filter() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: "filtered".to_string(),
            finish_reason: FinishReason::ContentFilter,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["stop_reason"], "end_turn");
    }

    // Test 16: Format usage
    #[test]
    fn format_usage() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            output_text: "hi".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["usage"]["input_tokens"], 100);
        assert_eq!(json["usage"]["output_tokens"], 50);
        assert!(json["usage"].get("total_tokens").is_none()); // Anthropic doesn't return total
    }

    // Test 17: Format model field
    #[test]
    fn format_response_model_field() {
        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-opus-4-20250514".to_string(),
            output_text: "hi".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "anthropic".to_string(),
            created_at: Utc::now(),
        };
        let json = anthropic_messages_json(resp);
        assert_eq!(json["model"], "claude-opus-4-20250514");
    }
}
