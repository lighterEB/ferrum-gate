use protocol_core::{CanonicalMessage, InferenceResponse, MessageRole};
use serde_json::{Value, json};

use crate::middleware::request_id::new_openai_object_id;

pub(crate) fn responses_json(response: InferenceResponse) -> Value {
    let mut output = Vec::new();
    if !response.output_text.is_empty() {
        output.push(json!({
          "id": new_openai_object_id("msg"),
          "type": "message",
          "status": "completed",
          "role": "assistant",
          "content": [{
            "type": "output_text",
            "text": response.output_text
          }]
        }));
    }
    output.extend(response.tool_calls.iter().map(|tc| {
        json!({
          "id": new_openai_object_id("fc"),
          "type": "function_call",
          "call_id": tc.id,
          "name": tc.name,
          "arguments": tc.arguments
        })
    }));

    json!({
      "id": response.id,
      "object": "response",
      "created_at": response.created_at.timestamp(),
      "model": response.model,
      "output": output,
      "usage": {
        "input_tokens": response.usage.input_tokens,
        "output_tokens": response.usage.output_tokens,
        "total_tokens": response.usage.total_tokens
      }
    })
}

pub(crate) fn responses_input_to_messages(input: Value) -> Vec<CanonicalMessage> {
    match input {
        Value::String(text) => vec![CanonicalMessage {
            role: MessageRole::User,
            content: text,
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }],
        Value::Array(items) => items
            .into_iter()
            .filter_map(parse_responses_input_item)
            .collect(),
        other => vec![CanonicalMessage {
            role: MessageRole::User,
            content: other.to_string(),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }],
    }
}

fn parse_message_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn parse_responses_input_item(item: Value) -> Option<CanonicalMessage> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => Some(CanonicalMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            parts: vec![],
            tool_calls: vec![protocol_core::ToolCall {
                id: item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)?
                    .to_string(),
                name: item.get("name").and_then(Value::as_str)?.to_string(),
                arguments: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
            tool_call_id: None,
        }),
        Some("function_call_output") => Some(CanonicalMessage {
            role: MessageRole::Tool,
            content: item
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: item
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }),
        _ => {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .map(parse_message_role)
                .unwrap_or(MessageRole::User);
            let content = item.get("content").cloned().unwrap_or(Value::Null);
            let text = match content {
                Value::String(s) => s,
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => other.to_string(),
            };
            Some(CanonicalMessage {
                role,
                content: text,
                parts: vec![],
                tool_calls: vec![],
                tool_call_id: None,
            })
        }
    }
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Test 21: String input
    #[test]
    fn parse_responses_request_string_input() {
        let msgs = responses_input_to_messages(json!("hello"));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content, "hello");
    }

    // Test 22: Array input
    #[test]
    fn parse_responses_request_array_input() {
        let input = json!([
            {"role": "user", "content": "hi"},
            {"type": "function_call", "call_id": "fc_1", "name": "foo", "arguments": "{}"}
        ]);
        let msgs = responses_input_to_messages(input);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[1].role, MessageRole::Assistant);
        assert_eq!(msgs[1].tool_calls.len(), 1);
        assert_eq!(msgs[1].tool_calls[0].id, "fc_1");
    }

    // Test 23: Basic response format
    #[test]
    fn format_responses_response_basic() {
        let resp = InferenceResponse {
            id: "resp_123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: "Hello".to_string(),
            finish_reason: protocol_core::FinishReason::Stop,
            tool_calls: vec![],
            usage: protocol_core::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: chrono::Utc::now(),
        };
        let json = responses_json(resp);
        assert_eq!(json["id"], "resp_123");
        assert_eq!(json["object"], "response");
        assert_eq!(json["output"][0]["type"], "message");
        assert_eq!(json["output"][0]["content"][0]["text"], "Hello");
    }

    // Test 24: Response with tool_call
    #[test]
    fn format_responses_response_with_tool_call() {
        let resp = InferenceResponse {
            id: "resp_123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: String::new(),
            finish_reason: protocol_core::FinishReason::ToolCalls,
            tool_calls: vec![protocol_core::ToolCall {
                id: "fc_1".to_string(),
                name: "foo".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: protocol_core::TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: chrono::Utc::now(),
        };
        let json = responses_json(resp);
        assert_eq!(json["output"][0]["type"], "function_call");
        assert_eq!(json["output"][0]["call_id"], "fc_1");
        assert_eq!(json["output"][0]["name"], "foo");
    }
}
