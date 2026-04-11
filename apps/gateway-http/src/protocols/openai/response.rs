use protocol_core::{FinishReason, InferenceResponse, ToolCall};
use serde_json::{Value, json};

pub(crate) fn chat_completion_json(response: InferenceResponse) -> Value {
    json!({
      "id": response.id,
      "object": "chat.completion",
      "created": response.created_at.timestamp(),
      "model": response.model,
      "choices": [{
        "index": 0,
        "message": {
          "role": "assistant",
          "content": if response.output_text.is_empty() && !response.tool_calls.is_empty() {
            Value::Null
          } else {
            Value::String(response.output_text.clone())
          },
          "tool_calls": if response.tool_calls.is_empty() {
            Value::Null
          } else {
            Value::Array(tool_calls_json(&response.tool_calls))
          }
        },
        "finish_reason": finish_reason_label(&response.finish_reason)
      }],
      "usage": {
        "prompt_tokens": response.usage.input_tokens,
        "completion_tokens": response.usage.output_tokens,
        "total_tokens": response.usage.total_tokens
      }
    })
}

pub(crate) fn tool_calls_json(tool_calls: &[ToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .map(|tc| {
            json!({
              "id": tc.id,
              "type": "function",
              "function": {
                "name": tc.name,
                "arguments": tc.arguments
              }
            })
        })
        .collect()
}

fn finish_reason_label(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Error => "error",
    }
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use protocol_core::TokenUsage;

    #[test]
    fn format_basic_text_response() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: "Hello".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };

        let json = chat_completion_json(resp);
        assert_eq!(json["choices"][0]["message"]["content"], "Hello");
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["prompt_tokens"], 10);
        assert_eq!(json["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn format_response_with_tool_calls() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: String::new(),
            finish_reason: FinishReason::ToolCalls,
            tool_calls: vec![ToolCall {
                id: "tc_1".to_string(),
                name: "foo".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };

        let json = chat_completion_json(resp);
        assert!(json["choices"][0]["message"]["content"].is_null());
        assert_eq!(json["choices"][0]["message"]["tool_calls"][0]["id"], "tc_1");
        assert_eq!(
            json["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "foo"
        );
        assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn format_finish_stop() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: "done".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };
        let json = chat_completion_json(resp);
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn format_finish_length() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: "done".to_string(),
            finish_reason: FinishReason::Length,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };
        let json = chat_completion_json(resp);
        assert_eq!(json["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn format_finish_tool_calls() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: String::new(),
            finish_reason: FinishReason::ToolCalls,
            tool_calls: vec![ToolCall {
                id: "tc_1".to_string(),
                name: "x".to_string(),
                arguments: "{}".to_string(),
            }],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };
        let json = chat_completion_json(resp);
        assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn format_usage() {
        let resp = InferenceResponse {
            id: "chatcmpl-123".to_string(),
            model: "gpt-4-mini".to_string(),
            output_text: "hi".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };
        let json = chat_completion_json(resp);
        assert_eq!(json["usage"]["prompt_tokens"], 100);
        assert_eq!(json["usage"]["completion_tokens"], 50);
        assert_eq!(json["usage"]["total_tokens"], 150);
    }

    #[test]
    fn format_response_id_and_model() {
        let resp = InferenceResponse {
            id: "chatcmpl-abc123".to_string(),
            model: "gpt-5.1".to_string(),
            output_text: "hi".to_string(),
            finish_reason: FinishReason::Stop,
            tool_calls: vec![],
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
            provider_kind: "openai_codex".to_string(),
            created_at: Utc::now(),
        };
        let json = chat_completion_json(resp);
        assert_eq!(json["id"], "chatcmpl-abc123");
        assert_eq!(json["model"], "gpt-5.1");
        assert_eq!(json["object"], "chat.completion");
        assert!(json["created"].as_i64().is_some());
    }
}
