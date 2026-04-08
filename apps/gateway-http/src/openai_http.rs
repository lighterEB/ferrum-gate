use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use protocol_core::{
    CanonicalMessage, ContentPart, FinishReason, InferenceResponse, MessageRole, ReasoningConfig,
    ToolCall, ToolDefinition,
};
use provider_core::{ProviderError, ProviderErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::middleware::request_id::new_openai_object_id;

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<OpenAiMessage>,
    #[serde(default)]
    pub(crate) tools: Vec<OpenAiToolDefinition>,
    #[serde(default)]
    pub(crate) reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub(crate) stream: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesRequest {
    pub(crate) model: String,
    pub(crate) input: Value,
    #[serde(default, deserialize_with = "deserialize_optional_string_placeholder")]
    pub(crate) previous_response_id: Option<String>,
    #[serde(default)]
    pub(crate) tools: Vec<ResponsesToolDefinition>,
    #[serde(default)]
    pub(crate) reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub(crate) stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiMessage {
    role: String,
    #[serde(default)]
    content: OpenAiMessageContent,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

impl Default for OpenAiMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiContentPart {
    #[serde(rename = "type")]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesToolDefinition {
    Flat(FlatResponsesToolDefinition),
    Nested(OpenAiToolDefinition),
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct FlatResponsesToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiFunctionDefinition {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

pub(crate) fn openai_message_to_canonical_message(message: &OpenAiMessage) -> CanonicalMessage {
    let parts = openai_content_parts(&message.content);
    CanonicalMessage {
        role: parse_message_role(&message.role),
        content: text_from_parts(&parts),
        parts,
        tool_calls: message
            .tool_calls
            .iter()
            .map(|tool_call| ToolCall {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                arguments: tool_call.function.arguments.clone(),
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

pub(crate) fn openai_tools_to_canonical_tools(
    tools: &[OpenAiToolDefinition],
) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter(|tool| tool.tool_type == "function")
        .map(|tool| ToolDefinition {
            name: tool.function.name.clone(),
            description: tool.function.description.clone(),
            parameters: tool.function.parameters.clone(),
        })
        .collect()
}

pub(crate) fn responses_tools_to_canonical_tools(
    tools: &[ResponsesToolDefinition],
) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter_map(|tool| match tool {
            ResponsesToolDefinition::Flat(tool) if tool.tool_type == "function" => {
                Some(ToolDefinition {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                })
            }
            ResponsesToolDefinition::Nested(tool) if tool.tool_type == "function" => {
                Some(ToolDefinition {
                    name: tool.function.name.clone(),
                    description: tool.function.description.clone(),
                    parameters: tool.function.parameters.clone(),
                })
            }
            _ => None,
        })
        .collect()
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

pub(crate) fn chat_stream_content_delta_json(model: &str, delta: &str) -> Value {
    chat_chunk_json(model, json!({ "content": delta }), Value::Null)
}

pub(crate) fn chat_stream_tool_calls_delta_json(model: &str, tool_calls: &[ToolCall]) -> Value {
    chat_chunk_json(
        model,
        json!({ "tool_calls": stream_tool_calls_json(tool_calls) }),
        Value::Null,
    )
}

pub(crate) fn chat_stream_done_json(model: &str, reason: &FinishReason) -> Value {
    chat_chunk_json(model, json!({}), json!(finish_reason_label(reason)))
}

pub(crate) fn chat_stream_error_json(error: &ProviderError) -> Value {
    json!({
      "error": provider_error_body(error)
    })
}

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
    output.extend(response.tool_calls.iter().map(|tool_call| {
        json!({
          "id": new_openai_object_id("fc"),
          "type": "function_call",
          "call_id": tool_call.id,
          "name": tool_call.name,
          "arguments": tool_call.arguments
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

pub(crate) fn responses_stream_created_json(response_id: &str, model: &str) -> Value {
    json!({
      "type": "response.created",
      "response": {
        "id": response_id,
        "object": "response",
        "created_at": Utc::now().timestamp(),
        "model": model,
        "output": [],
        "usage": {
          "input_tokens": 0,
          "output_tokens": 0,
          "total_tokens": 0
        }
      }
    })
}

pub(crate) fn responses_stream_delta_json(response_id: &str, item_id: &str, delta: &str) -> Value {
    json!({
      "type": "response.output_text.delta",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": 0,
      "content_index": 0,
      "delta": delta
    })
}

pub(crate) fn responses_stream_done_json(response_id: &str, item_id: &str, text: &str) -> Value {
    json!({
      "type": "response.output_text.done",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": 0,
      "content_index": 0,
      "text": text
    })
}

pub(crate) fn responses_stream_output_item_added_json(response_id: &str, item_id: &str) -> Value {
    json!({
      "type": "response.output_item.added",
      "response_id": response_id,
      "output_index": 0,
      "item": {
        "id": item_id,
        "type": "message",
        "status": "in_progress",
        "role": "assistant",
        "content": []
      }
    })
}

pub(crate) fn responses_stream_content_part_added_json(response_id: &str, item_id: &str) -> Value {
    json!({
      "type": "response.content_part.added",
      "response_id": response_id,
      "output_index": 0,
      "item_id": item_id,
      "content_index": 0,
      "part": {
        "type": "output_text",
        "text": ""
      }
    })
}

pub(crate) fn responses_stream_content_part_done_json(
    response_id: &str,
    item_id: &str,
    text: &str,
) -> Value {
    json!({
      "type": "response.content_part.done",
      "response_id": response_id,
      "output_index": 0,
      "item_id": item_id,
      "content_index": 0,
      "part": {
        "type": "output_text",
        "text": text
      }
    })
}

pub(crate) fn responses_stream_output_item_done_json(
    response_id: &str,
    item_id: &str,
    text: &str,
) -> Value {
    json!({
      "type": "response.output_item.done",
      "response_id": response_id,
      "output_index": 0,
      "item": {
        "id": item_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
          "type": "output_text",
          "text": text
        }]
      }
    })
}

pub(crate) fn responses_stream_function_call_output_item_added_json(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    tool_call: &ToolCall,
) -> Value {
    json!({
      "type": "response.output_item.added",
      "response_id": response_id,
      "output_index": output_index,
      "item": {
        "id": item_id,
        "type": "function_call",
        "call_id": tool_call.id,
        "name": tool_call.name,
        "arguments": ""
      }
    })
}

pub(crate) fn responses_stream_function_call_arguments_delta_json(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    tool_call: &ToolCall,
) -> Value {
    json!({
      "type": "response.function_call_arguments.delta",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": output_index,
      "delta": tool_call.arguments
    })
}

pub(crate) fn responses_stream_function_call_arguments_done_json(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    tool_call: &ToolCall,
) -> Value {
    json!({
      "type": "response.function_call_arguments.done",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": output_index,
      "arguments": tool_call.arguments,
    })
}

pub(crate) fn responses_stream_function_call_output_item_done_json(
    response_id: &str,
    item_id: &str,
    output_index: usize,
    tool_call: &ToolCall,
) -> Value {
    json!({
      "type": "response.output_item.done",
      "response_id": response_id,
      "output_index": output_index,
      "item": {
        "id": item_id,
        "type": "function_call",
        "call_id": tool_call.id,
        "name": tool_call.name,
        "arguments": tool_call.arguments
      }
    })
}

pub(crate) fn responses_stream_completed_json(
    response_id: &str,
    message_item_id: Option<&str>,
    tool_call_item_ids: &BTreeMap<String, String>,
    response: InferenceResponse,
) -> Value {
    let mut payload = responses_json(response);
    payload["id"] = Value::String(response_id.to_string());
    if let Some(output) = payload.get_mut("output").and_then(Value::as_array_mut) {
        let mut patched_message = false;
        for item in output.iter_mut() {
            match item.get("type").and_then(Value::as_str) {
                Some("message") if !patched_message => {
                    if let Some(message_item_id) = message_item_id {
                        item["id"] = Value::String(message_item_id.to_string());
                        patched_message = true;
                    }
                }
                Some("function_call") => {
                    if let Some(call_id) = item.get("call_id").and_then(Value::as_str)
                        && let Some(item_id) = tool_call_item_ids.get(call_id)
                    {
                        item["id"] = Value::String(item_id.clone());
                    }
                }
                _ => {}
            }
        }
    }
    json!({
      "type": "response.completed",
      "response": payload
    })
}

pub(crate) fn responses_stream_failed_json(response_id: &str, error: &ProviderError) -> Value {
    json!({
      "type": "response.failed",
      "response_id": response_id,
      "error": provider_error_body(error)
    })
}

pub(crate) fn provider_error_response(error: ProviderError) -> Response {
    let status = StatusCode::from_u16(error.status_code).unwrap_or(StatusCode::BAD_GATEWAY);
    Json(json!({
      "error": provider_error_body(&error)
    }))
    .into_response()
    .with_status(status)
}

pub(crate) fn openai_error(status: StatusCode, message: &str) -> Response {
    Json(json!({
      "error": {
        "message": message,
        "type": "invalid_request_error",
        "code": "gateway_error",
        "param": Value::Null
      }
    }))
    .into_response()
    .with_status(status)
}

pub(crate) fn internal_error(message: &str) -> Response {
    Json(json!({
      "error": {
        "message": message,
        "type": "server_error",
        "code": "storage_error",
        "param": Value::Null
      }
    }))
    .into_response()
    .with_status(StatusCode::INTERNAL_SERVER_ERROR)
}

fn parse_message_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn openai_content_parts(content: &OpenAiMessageContent) -> Vec<ContentPart> {
    match content {
        OpenAiMessageContent::Text(text) if text.is_empty() => Vec::new(),
        OpenAiMessageContent::Text(text) => vec![ContentPart::Text { text: text.clone() }],
        OpenAiMessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part.part_type.as_str() {
                "text" | "input_text" | "output_text" => part
                    .text
                    .as_ref()
                    .map(|text| ContentPart::Text { text: text.clone() }),
                "image_url" | "input_image" => part
                    .image_url
                    .as_ref()
                    .and_then(extract_image_url)
                    .map(|image_url| ContentPart::ImageUrl { image_url }),
                _ => None,
            })
            .collect(),
    }
}

fn extract_image_url(value: &Value) -> Option<String> {
    value.as_str().map(ToString::to_string).or_else(|| {
        value
            .get("url")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn text_from_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

fn parse_responses_input_item(item: Value) -> Option<CanonicalMessage> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => Some(CanonicalMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            parts: vec![],
            tool_calls: vec![ToolCall {
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
            let parts = match &content {
                Value::String(text) if !text.is_empty() => {
                    vec![ContentPart::Text { text: text.clone() }]
                }
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                        Some("input_text") | Some("text") | Some("output_text") => part
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| ContentPart::Text {
                                text: text.to_string(),
                            }),
                        Some("input_image") | Some("image_url") => part
                            .get("image_url")
                            .or_else(|| part.get("url"))
                            .and_then(extract_image_url)
                            .map(|image_url| ContentPart::ImageUrl { image_url }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };

            Some(CanonicalMessage {
                role,
                content: match content {
                    Value::String(text) => text,
                    _ => text_from_parts(&parts),
                },
                parts,
                tool_calls: vec![],
                tool_call_id: item
                    .get("tool_call_id")
                    .or_else(|| item.get("call_id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        }
    }
}

fn chat_chunk_json(model: &str, delta: Value, finish_reason: Value) -> Value {
    json!({
      "id": new_openai_object_id("chatcmpl"),
      "object": "chat.completion.chunk",
      "created": Utc::now().timestamp(),
      "model": model,
      "choices": [{
        "index": 0,
        "delta": delta,
        "finish_reason": finish_reason
      }]
    })
}

fn tool_calls_json(tool_calls: &[ToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .map(|tool_call| {
            json!({
              "id": tool_call.id,
              "type": "function",
              "function": {
                "name": tool_call.name,
                "arguments": tool_call.arguments
              }
            })
        })
        .collect()
}

fn stream_tool_calls_json(tool_calls: &[ToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .enumerate()
        .map(|(index, tool_call)| {
            json!({
              "index": index,
              "id": tool_call.id,
              "type": "function",
              "function": {
                "name": tool_call.name,
                "arguments": tool_call.arguments
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

fn provider_error_body(error: &ProviderError) -> Value {
    let error_type = match error.kind {
        ProviderErrorKind::InvalidRequest
        | ProviderErrorKind::InvalidCredentials
        | ProviderErrorKind::Unsupported => "invalid_request_error",
        ProviderErrorKind::RateLimited => "rate_limit_error",
        ProviderErrorKind::UpstreamUnavailable => "server_error",
    };
    let default_code = match error.kind {
        ProviderErrorKind::InvalidRequest => "invalid_request",
        ProviderErrorKind::InvalidCredentials => "invalid_credentials",
        ProviderErrorKind::RateLimited => "rate_limited",
        ProviderErrorKind::UpstreamUnavailable => "upstream_unavailable",
        ProviderErrorKind::Unsupported => "unsupported",
    };

    json!({
      "message": error.message,
      "type": error_type,
      "code": error.code.clone().unwrap_or_else(|| default_code.to_string()),
      "param": Value::Null
    })
}

trait ResponseExt {
    fn with_status(self, status: StatusCode) -> Response;
}

impl ResponseExt for Response {
    fn with_status(mut self, status: StatusCode) -> Response {
        *self.status_mut() = status;
        self
    }
}

fn deserialize_optional_string_placeholder<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        if value == "[undefined]" {
            None
        } else {
            Some(value)
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── openai_message_to_canonical_message ──

    #[test]
    fn converts_user_text_message() {
        let msg = OpenAiMessage {
            role: "user".to_string(),
            content: OpenAiMessageContent::Text("hello world".to_string()),
            tool_call_id: None,
            tool_calls: vec![],
        };
        let result = openai_message_to_canonical_message(&msg);
        assert_eq!(result.role, MessageRole::User);
        assert_eq!(result.content, "hello world");
        // Text content is also stored as a ContentPart
        assert_eq!(result.parts.len(), 1);
        assert!(matches!(&result.parts[0], ContentPart::Text { text } if text == "hello world"));
    }

    #[test]
    fn converts_assistant_message_with_tool_calls() {
        let msg = OpenAiMessage {
            role: "assistant".to_string(),
            content: OpenAiMessageContent::Text("let me help".to_string()),
            tool_call_id: None,
            tool_calls: vec![OpenAiToolCall {
                id: "call_123".to_string(),
                tool_type: "function".to_string(),
                function: OpenAiFunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"tokyo"}"#.to_string(),
                },
            }],
        };
        let result = openai_message_to_canonical_message(&msg);
        assert_eq!(result.role, MessageRole::Assistant);
        assert_eq!(result.content, "let me help");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].id, "call_123");
        assert_eq!(result.tool_calls[0].name, "get_weather");
    }

    #[test]
    fn converts_tool_result_message() {
        let msg = OpenAiMessage {
            role: "tool".to_string(),
            content: OpenAiMessageContent::Text("sunny, 25C".to_string()),
            tool_call_id: Some("call_123".to_string()),
            tool_calls: vec![],
        };
        let result = openai_message_to_canonical_message(&msg);
        assert_eq!(result.role, MessageRole::Tool);
        assert_eq!(result.content, "sunny, 25C");
        assert_eq!(result.tool_call_id.as_deref(), Some("call_123"));
    }

    #[test]
    fn converts_multi_part_content_with_image() {
        let msg = OpenAiMessage {
            role: "user".to_string(),
            content: OpenAiMessageContent::Parts(vec![OpenAiContentPart {
                part_type: "image_url".to_string(),
                text: None,
                image_url: Some(json!({"url": "https://example.com/img.png"})),
            }]),
            tool_call_id: None,
            tool_calls: vec![],
        };
        let result = openai_message_to_canonical_message(&msg);
        assert_eq!(result.role, MessageRole::User);
        assert_eq!(result.parts.len(), 1);
        assert!(
            matches!(&result.parts[0], ContentPart::ImageUrl { image_url } if image_url == "https://example.com/img.png")
        );
    }

    // ── openai_tools_to_canonical_tools ──

    #[test]
    fn converts_openai_tools_to_canonical() {
        let tools = vec![
            OpenAiToolDefinition {
                tool_type: "function".to_string(),
                function: OpenAiFunctionDefinition {
                    name: "get_weather".to_string(),
                    description: Some("Get weather".to_string()),
                    parameters: json!({"type": "object"}),
                },
            },
            OpenAiToolDefinition {
                tool_type: "code_interpreter".to_string(),
                function: OpenAiFunctionDefinition {
                    name: "ignored".to_string(),
                    description: None,
                    parameters: json!({}),
                },
            },
        ];
        let result = openai_tools_to_canonical_tools(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "get_weather");
    }

    // ── responses_input_to_messages ──

    #[test]
    fn responses_input_string_becomes_user_message() {
        let input = Value::String("tell me a joke".to_string());
        let result = responses_input_to_messages(input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::User);
        assert_eq!(result[0].content, "tell me a joke");
    }

    #[test]
    fn responses_input_array_parses_message_items() {
        let input = json!([
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi there"}
        ]);
        let result = responses_input_to_messages(input);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, MessageRole::User);
        assert_eq!(result[1].role, MessageRole::Assistant);
    }

    #[test]
    fn responses_input_function_call_item_becomes_assistant_message() {
        let input = json!([
            {
                "type": "function_call",
                "id": "fc_1",
                "name": "get_weather",
                "arguments": {"city": "tokyo"},
                "call_id": "call_1"
            }
        ]);
        let result = responses_input_to_messages(input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::Assistant);
        assert_eq!(result[0].tool_calls.len(), 1);
        assert_eq!(result[0].tool_calls[0].name, "get_weather");
    }

    #[test]
    fn responses_input_function_call_output_item_becomes_tool_message() {
        let input = json!([
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "sunny, 25C"
            }
        ]);
        let result = responses_input_to_messages(input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::Tool);
        assert_eq!(result[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(result[0].content, "sunny, 25C");
    }

    #[test]
    fn responses_input_non_string_non_array_becomes_user_message() {
        let input = Value::Number(42.into());
        let result = responses_input_to_messages(input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, MessageRole::User);
        assert_eq!(result[0].content, "42");
    }

    // ── deserialize_optional_string_placeholder ──

    #[test]
    fn placeholder_undefined_becomes_none_via_request() {
        // The deserialize_optional_string_placeholder is used in ResponsesRequest.previous_response_id
        // Test through the actual request deserialization
        let json = r#"{"model":"gpt-5","input":"hello","previous_response_id":"[undefined]"}"#;
        let req: ResponsesRequest = serde_json::from_str(json).expect("valid request");
        assert!(req.previous_response_id.is_none());
    }

    #[test]
    fn normal_string_value_is_preserved_via_request() {
        let json = r#"{"model":"gpt-5","input":"hello","previous_response_id":"resp_abc123"}"#;
        let req: ResponsesRequest = serde_json::from_str(json).expect("valid request");
        assert_eq!(req.previous_response_id.as_deref(), Some("resp_abc123"));
    }

    #[test]
    fn null_value_becomes_none_via_request() {
        let json = r#"{"model":"gpt-5","input":"hello","previous_response_id":null}"#;
        let req: ResponsesRequest = serde_json::from_str(json).expect("valid request");
        assert!(req.previous_response_id.is_none());
    }

    // ── provider_error_body ──

    #[test]
    fn error_body_invalid_request_maps_to_invalid_request_error() {
        let error = ProviderError::new(
            ProviderErrorKind::InvalidRequest,
            400,
            "bad param".to_string(),
        );
        let body = provider_error_body(&error);
        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["code"], "invalid_request");
        assert_eq!(body["message"], "bad param");
        assert!(body["param"].is_null());
    }

    #[test]
    fn error_body_invalid_credentials_maps_to_invalid_request_error() {
        let error = ProviderError::new(
            ProviderErrorKind::InvalidCredentials,
            401,
            "wrong key".to_string(),
        );
        let body = provider_error_body(&error);
        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["code"], "invalid_credentials");
    }

    #[test]
    fn error_body_rate_limited_maps_to_rate_limit_error() {
        let error = ProviderError::new(ProviderErrorKind::RateLimited, 429, "too many".to_string());
        let body = provider_error_body(&error);
        assert_eq!(body["type"], "rate_limit_error");
        assert_eq!(body["code"], "rate_limited");
    }

    #[test]
    fn error_body_upstream_unavailable_maps_to_server_error() {
        let error = ProviderError::new(
            ProviderErrorKind::UpstreamUnavailable,
            502,
            "upstream down".to_string(),
        );
        let body = provider_error_body(&error);
        assert_eq!(body["type"], "server_error");
        assert_eq!(body["code"], "upstream_unavailable");
    }

    #[test]
    fn error_body_unsupported_maps_to_invalid_request_error() {
        let error = ProviderError::new(
            ProviderErrorKind::Unsupported,
            400,
            "not supported".to_string(),
        );
        let body = provider_error_body(&error);
        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["code"], "unsupported");
    }

    #[test]
    fn error_body_uses_custom_code_when_provided() {
        let error = ProviderError::new(ProviderErrorKind::InvalidRequest, 400, "err".to_string())
            .with_code("custom_code");
        let body = provider_error_body(&error);
        assert_eq!(body["code"], "custom_code");
    }

    // ── chat_completion_json ──

    #[test]
    fn chat_completion_json_has_correct_structure() {
        let response = InferenceResponse {
            id: "chatcmpl_test".to_string(),
            created_at: Utc::now(),
            model: "gpt-4".to_string(),
            provider_kind: "openai_codex".to_string(),
            output_text: "Hello!".to_string(),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            usage: protocol_core::TokenUsage {
                input_tokens: 5,
                output_tokens: 3,
                total_tokens: 8,
            },
        };
        let json = chat_completion_json(response);
        assert_eq!(json["object"], "chat.completion");
        assert_eq!(json["choices"][0]["message"]["role"], "assistant");
        assert_eq!(json["choices"][0]["message"]["content"], "Hello!");
        assert!(json["choices"][0]["message"]["tool_calls"].is_null());
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert_eq!(json["usage"]["total_tokens"], 8);
    }

    #[test]
    fn chat_completion_json_content_is_null_when_only_tool_calls() {
        let response = InferenceResponse {
            id: "chatcmpl_test".to_string(),
            created_at: Utc::now(),
            model: "gpt-4".to_string(),
            provider_kind: "openai_codex".to_string(),
            output_text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
            }],
            finish_reason: FinishReason::ToolCalls,
            usage: protocol_core::TokenUsage {
                input_tokens: 5,
                output_tokens: 2,
                total_tokens: 7,
            },
        };
        let json = chat_completion_json(response);
        assert!(json["choices"][0]["message"]["content"].is_null());
        assert!(!json["choices"][0]["message"]["tool_calls"].is_null());
        assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
    }

    // ── responses_json ──

    #[test]
    fn responses_json_has_correct_structure() {
        let response = InferenceResponse {
            id: "resp_test".to_string(),
            created_at: Utc::now(),
            model: "gpt-5-codex".to_string(),
            provider_kind: "openai_codex".to_string(),
            output_text: "Response text".to_string(),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            usage: protocol_core::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = responses_json(response);
        assert_eq!(json["object"], "response");
        assert_eq!(json["output"][0]["type"], "message");
        assert_eq!(json["output"][0]["role"], "assistant");
        assert_eq!(json["usage"]["total_tokens"], 15);
    }

    #[test]
    fn responses_json_includes_tool_call_outputs() {
        let response = InferenceResponse {
            id: "resp_test".to_string(),
            created_at: Utc::now(),
            model: "gpt-5-codex".to_string(),
            provider_kind: "openai_codex".to_string(),
            output_text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "get_weather".to_string(),
                arguments: r#"{"city":"tokyo"}"#.to_string(),
            }],
            finish_reason: FinishReason::ToolCalls,
            usage: protocol_core::TokenUsage {
                input_tokens: 5,
                output_tokens: 2,
                total_tokens: 7,
            },
        };
        let json = responses_json(response);
        assert_eq!(json["output"][0]["type"], "function_call");
        assert_eq!(json["output"][0]["name"], "get_weather");
        assert_eq!(json["output"][0]["call_id"], "call_1");
    }
}
