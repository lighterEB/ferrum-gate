use async_stream::stream;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, Sse},
    },
};
use futures::StreamExt;
use protocol_core::{FrontendProtocol, InferenceRequest, StreamEventKind};
use std::{collections::BTreeMap, convert::Infallible};

use crate::{
    GatewayAppState,
    core::{
        execution_engine::ExecutionEngine,
        types::{ExecutionError, ExecutionOutput},
    },
    middleware::auth::authenticate_gateway,
    openai_http::{internal_error, provider_error_response},
    protocols::anthropic::{
        request::{AnthropicMessagesRequest, AnthropicSystemPrompt, message_to_canonical},
        response::anthropic_messages_json,
        streaming::AnthropicStreamState,
    },
};

pub(crate) async fn messages(
    State(state): State<GatewayAppState>,
    headers: HeaderMap,
    Json(request): Json<AnthropicMessagesRequest>,
) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    // Convert Anthropic messages to Canonical
    let mut canonical_messages = Vec::new();

    // Prepend system message if present
    if let Some(system) = &request.system {
        match system {
            AnthropicSystemPrompt::Text(text) => {
                canonical_messages.push(protocol_core::CanonicalMessage {
                    role: protocol_core::MessageRole::System,
                    content: text.clone(),
                    parts: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                });
            }
            AnthropicSystemPrompt::Blocks(blocks) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| b.text.as_ref())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    canonical_messages.push(protocol_core::CanonicalMessage {
                        role: protocol_core::MessageRole::System,
                        content: text,
                        parts: vec![],
                        tool_calls: vec![],
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    for msg in &request.messages {
        canonical_messages.push(message_to_canonical(msg));
    }

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::Anthropic,
        public_model: request.model.clone(),
        upstream_model: None,
        previous_response_id: None,
        reasoning: None,
        stream: request.stream,
        messages: canonical_messages,
        tools: request
            .tools
            .iter()
            .map(|t| protocol_core::ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: if t.input_schema.is_null() {
                    serde_json::json!({})
                } else {
                    t.input_schema.clone()
                },
            })
            .collect(),
        metadata: BTreeMap::new(),
    };

    match ExecutionEngine::execute(
        &state,
        &auth,
        canonical,
        crate::core::types::RequestedCapability::Chat,
    )
    .await
    {
        Ok(result) => match result.output {
            ExecutionOutput::Response(response) => {
                Json(anthropic_messages_json(response)).into_response()
            }
            ExecutionOutput::Stream(mut provider_stream) => {
                let stream_public_model = request.model.clone();
                let stream = stream! {
                    let id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                    let mut anthropic_state = AnthropicStreamState {
                        id: Some(id),
                        model: Some(stream_public_model.clone()),
                        ..Default::default()
                    };

                    // Emit message_start
                    let start_payload = serde_json::json!({
                        "type": "message_start",
                        "message": {
                            "id": anthropic_state.id,
                            "model": anthropic_state.model,
                            "usage": {"input_tokens": 0, "output_tokens": 0}
                        }
                    });
                    yield Ok::<Event, Infallible>(
                        Event::default().event("message_start").data(start_payload.to_string())
                    );

                    while let Some(item) = provider_stream.next().await {
                        match item {
                            Ok(event) => match event.kind {
                                StreamEventKind::ContentDelta => {
                                    let text = event.delta.unwrap_or_default();
                                    if !text.is_empty() {
                                        anthropic_state.output_text.push_str(&text);
                                        let delta_payload = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {"type": "text_delta", "text": text}
                                        });
                                        yield Ok(Event::default()
                                            .event("content_block_delta")
                                            .data(delta_payload.to_string()));
                                    }
                                }
                                StreamEventKind::Done => {
                                    if let Some(response) = event.response {
                                        anthropic_state.output_text = response.output_text;
                                        anthropic_state.input_tokens = Some(response.usage.input_tokens);
                                        anthropic_state.output_tokens = Some(response.usage.output_tokens);
                                    }

                                    // Emit message_delta with stop_reason
                                    let stop_reason = match anthropic_state.stop_reason.as_deref() {
                                        Some("tool_use") => "tool_use",
                                        Some("max_tokens") => "max_tokens",
                                        _ => "end_turn",
                                    };
                                    let delta_payload = serde_json::json!({
                                        "type": "message_delta",
                                        "delta": {"stop_reason": stop_reason},
                                        "usage": {
                                            "input_tokens": anthropic_state.input_tokens,
                                            "output_tokens": anthropic_state.output_tokens
                                        }
                                    });
                                    yield Ok(Event::default()
                                        .event("message_delta")
                                        .data(delta_payload.to_string()));

                                    // Emit message_stop
                                    yield Ok(Event::default()
                                        .event("message_stop")
                                        .data(r#"{"type":"message_stop"}"#));
                                }
                                _ => {}
                            },
                            Err(error) => {
                                let error_payload = serde_json::json!({
                                    "type": "error",
                                    "error": {
                                        "type": "api_error",
                                        "message": error.message
                                    }
                                });
                                yield Ok(Event::default()
                                    .event("error")
                                    .data(error_payload.to_string()));
                                yield Ok(Event::default()
                                    .event("message_stop")
                                    .data(r#"{"type":"message_stop"}"#));
                            }
                        }
                    }
                };

                Sse::new(stream).into_response()
            }
        },
        Err(ExecutionError::UnknownModel) => {
            anthropic_error(StatusCode::NOT_FOUND, "Unknown model")
        }
        Err(ExecutionError::NoHealthyCandidate) => anthropic_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No healthy provider candidate",
        ),
        Err(ExecutionError::ProviderNotRegistered(_)) => {
            anthropic_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered")
        }
        Err(ExecutionError::Internal(message)) => internal_error(&message),
        Err(ExecutionError::Provider(error)) => provider_error_response(error),
    }
}

fn anthropic_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": message
        }
    });
    (status, Json(body)).into_response()
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::anthropic::request::parse_anthropic_request;
    use axum::body::to_bytes;
    use protocol_core::FinishReason;

    // Test 28: messages_endpoint_parses_anthropic_body
    #[tokio::test]
    async fn messages_endpoint_parses_anthropic_body() {
        // This test verifies the handler can parse and route the request.
        // Full end-to-end with a real provider would need a running gateway with
        // properly seeded Anthropic accounts. Here we just verify the parsing.
        let request_json = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "hello"}]
        });

        let parsed = parse_anthropic_request(&request_json);
        assert!(parsed.is_some());
        let req = parsed.unwrap();
        assert_eq!(req.public_model, "claude-sonnet-4-20250514");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].content, "hello");
    }

    // Test 29: messages_endpoint_returns_anthropic_format
    #[test]
    fn messages_endpoint_returns_anthropic_format() {
        use crate::protocols::anthropic::response::anthropic_messages_json;
        use chrono::Utc;
        use protocol_core::{InferenceResponse, TokenUsage};

        let resp = InferenceResponse {
            id: "msg_123".to_string(),
            model: "claude-sonnet-4".to_string(),
            output_text: "Hello!".to_string(),
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
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hello!");
        assert_eq!(json["stop_reason"], "end_turn");
    }

    // Test 30: messages_endpoint_propagates_error
    #[tokio::test]
    async fn messages_endpoint_propagates_error() {
        let resp = anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, "test error");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Verify the error format matches Anthropic's error schema
        let body = to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "api_error");
        assert_eq!(json["error"]["message"], "test error");
    }
}
