use anyhow::Result;
use async_stream::stream;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{
        IntoResponse, Json, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt;
use protocol_core::{
    CanonicalMessage, ContentPart, FinishReason, FrontendProtocol, InferenceRequest,
    InferenceResponse, MessageRole, ModelCapability, ReasoningConfig, StreamEventKind, ToolCall,
    ToolDefinition,
};
use provider_core::{ProviderError, ProviderErrorKind, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use scheduler::ProviderOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, convert::Infallible, net::SocketAddr, sync::Arc};
use storage::{GatewayAuthContext, PlatformStore};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
pub struct GatewayAppState {
    pub store: PlatformStore,
    pub registry: ProviderRegistry,
}

impl GatewayAppState {
    #[must_use]
    pub fn demo() -> Self {
        let store = PlatformStore::demo();
        let mut registry = ProviderRegistry::new();
        registry.register(OpenAiCodexProvider::shared(Arc::new(store.clone())));

        Self { store, registry }
    }
}

pub fn app(state: GatewayAppState) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .with_state(state);

    if let Some(cors) = console_cors_layer() {
        router.layer(cors)
    } else {
        router
    }
}

pub async fn run(addr: SocketAddr, state: GatewayAppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("gateway-http listening on {addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

fn console_cors_layer() -> Option<CorsLayer> {
    let allowed_origins = std::env::var("FERRUMGATE_CONSOLE_ALLOWED_ORIGINS")
        .or_else(|_| std::env::var("FERRUMGATE_TENANT_API_ALLOWED_ORIGINS"))
        .ok()?;
    let origins = allowed_origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(HeaderValue::from_str)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    if origins.is_empty() {
        return None;
    }

    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
            ]),
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn list_models(State(state): State<GatewayAppState>, headers: HeaderMap) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let models = match state.store.list_tenant_models(auth.tenant.id).await {
        Ok(models) => models,
        Err(error) => return internal_error(&error.to_string()),
    };
    Json(json!({
      "object": "list",
      "data": models.into_iter().map(|model| {
        json!({
          "id": model.id,
          "object": "model",
          "owned_by": "ferrum-gate",
          "provider_kind": model.provider_kind,
          "capabilities": model.capabilities.iter().map(model_capability_label).collect::<Vec<_>>(),
        })
      }).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn chat_completions(
    State(state): State<GatewayAppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let candidate = match state.store.choose_candidate(&request.model).await {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy provider candidate",
            );
        }
        Err(error) => return internal_error(&error.to_string()),
    };

    let route_group = match state.store.resolve_route_group(&request.model).await {
        Ok(Some(route_group)) => route_group,
        Ok(None) => return openai_error(StatusCode::NOT_FOUND, "Unknown model"),
        Err(error) => return internal_error(&error.to_string()),
    };

    let Some(provider) = state.registry.get(&candidate.provider_kind) else {
        return openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered");
    };

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: Some(route_group.upstream_model),
        previous_response_id: None,
        reasoning: request.reasoning.clone(),
        stream: request.stream,
        messages: request
            .messages
            .iter()
            .map(openai_message_to_canonical_message)
            .collect(),
        tools: openai_tools_to_canonical_tools(&request.tools),
        metadata: BTreeMap::from([
            (
                "provider_account_id".to_string(),
                candidate.account_id.to_string(),
            ),
            ("route_group_id".to_string(), route_group.id.to_string()),
        ]),
    };

    if request.stream {
        match provider.stream_chat(canonical).await {
            Ok(mut provider_stream) => {
                let tenant_id = auth.tenant.id;
                let api_key_id = auth.api_key_id;
                let public_model = request.model.clone();
                let provider_kind = candidate.provider_kind.clone();
                let store = state.store.clone();
                let stream = stream! {
                  while let Some(item) = provider_stream.next().await {
                    match item {
                      Ok(event) => match event.kind {
                        StreamEventKind::ContentDelta => {
                          let payload = json!({
                            "id": format!("chatcmpl_{}", Uuid::new_v4().simple()),
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": public_model,
                            "choices": [{
                              "index": 0,
                              "delta": { "content": event.delta.unwrap_or_default() },
                              "finish_reason": Value::Null
                            }]
                          });
                          yield Ok::<Event, Infallible>(Event::default().data(payload.to_string()));
                        }
                        StreamEventKind::Done => {
                          if let Some(response) = event.response {
                            if !response.tool_calls.is_empty() {
                              let payload = json!({
                                "id": format!("chatcmpl_{}", Uuid::new_v4().simple()),
                                "object": "chat.completion.chunk",
                                "created": chrono::Utc::now().timestamp(),
                                "model": public_model,
                                "choices": [{
                                  "index": 0,
                                  "delta": { "tool_calls": stream_tool_calls_json(&response.tool_calls) },
                                  "finish_reason": Value::Null
                                }]
                              });
                              yield Ok(Event::default().data(payload.to_string()));
                            }
                            let final_payload = json!({
                              "id": format!("chatcmpl_{}", Uuid::new_v4().simple()),
                              "object": "chat.completion.chunk",
                              "created": chrono::Utc::now().timestamp(),
                              "model": public_model,
                              "choices": [{
                                "index": 0,
                                "delta": {},
                                "finish_reason": finish_reason_label(&response.finish_reason)
                              }]
                            });
                            store
                              .record_request(
                                tenant_id,
                                Some(api_key_id),
                                public_model.clone(),
                                provider_kind.clone(),
                                200,
                                10,
                                response.usage,
                              )
                              .await
                              .ok();
                            store
                              .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                              .await
                              .ok();
                            yield Ok(Event::default().data(final_payload.to_string()));
                          } else {
                            let payload = json!({
                              "id": format!("chatcmpl_{}", Uuid::new_v4().simple()),
                              "object": "chat.completion.chunk",
                              "created": chrono::Utc::now().timestamp(),
                              "model": public_model,
                              "choices": [{
                                "index": 0,
                                "delta": {},
                                "finish_reason": "stop"
                              }]
                            });
                            yield Ok(Event::default().data(payload.to_string()));
                          }
                          yield Ok(Event::default().data("[DONE]"));
                        }
                        _ => {}
                      },
                      Err(error) => {
                        store
                          .mark_scheduler_outcome(
                            candidate.account_id,
                            provider_outcome_for_error(&error),
                          )
                          .await
                          .ok();
                        let payload = json!({
                          "error": provider_error_body(&error)
                        });
                        yield Ok(Event::default().data(payload.to_string()));
                        yield Ok(Event::default().data("[DONE]"));
                      }
                    }
                  }
                };

                return Sse::new(stream).into_response();
            }
            Err(error) => {
                state
                    .store
                    .mark_scheduler_outcome(
                        candidate.account_id,
                        provider_outcome_for_error(&error),
                    )
                    .await
                    .ok();
                return provider_error_response(error);
            }
        }
    }

    match provider.chat(canonical).await {
        Ok(response) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                .await
                .ok();
            state
                .store
                .record_request(
                    auth.tenant.id,
                    Some(auth.api_key_id),
                    request.model.clone(),
                    candidate.provider_kind,
                    200,
                    8,
                    response.usage.clone(),
                )
                .await
                .ok();
            Json(chat_completion_json(response)).into_response()
        }
        Err(error) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, provider_outcome_for_error(&error))
                .await
                .ok();
            provider_error_response(error)
        }
    }
}

async fn responses(
    State(state): State<GatewayAppState>,
    headers: HeaderMap,
    Json(request): Json<ResponsesRequest>,
) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let candidate = match state.store.choose_candidate(&request.model).await {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy provider candidate",
            );
        }
        Err(error) => return internal_error(&error.to_string()),
    };

    let route_group = match state.store.resolve_route_group(&request.model).await {
        Ok(Some(route_group)) => route_group,
        Ok(None) => return openai_error(StatusCode::NOT_FOUND, "Unknown model"),
        Err(error) => return internal_error(&error.to_string()),
    };
    let Some(provider) = state.registry.get(&candidate.provider_kind) else {
        return openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered");
    };

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: Some(route_group.upstream_model),
        previous_response_id: request.previous_response_id.clone(),
        reasoning: request.reasoning.clone(),
        stream: request.stream,
        messages: responses_input_to_messages(request.input),
        tools: responses_tools_to_canonical_tools(&request.tools),
        metadata: BTreeMap::from([
            (
                "provider_account_id".to_string(),
                candidate.account_id.to_string(),
            ),
            ("route_group_id".to_string(), route_group.id.to_string()),
        ]),
    };

    if request.stream {
        match provider.stream_responses(canonical).await {
            Ok(mut provider_stream) => {
                let store = state.store.clone();
                let tenant_id = auth.tenant.id;
                let api_key_id = auth.api_key_id;
                let public_model = request.model.clone();
                let provider_kind = candidate.provider_kind.clone();
                let stream_response_id = format!("resp_{}", Uuid::new_v4().simple());
                let stream_message_item_id = format!("msg_{}", Uuid::new_v4().simple());
                let stream = stream! {
                  let mut streamed_text = String::new();
                  let mut text_stream_started = false;
                  let created_payload = responses_stream_created_json(&stream_response_id, &public_model);
                  yield Ok::<Event, Infallible>(
                    Event::default()
                      .event("response.created")
                      .data(created_payload.to_string())
                  );
                  while let Some(item) = provider_stream.next().await {
                    match item {
                      Ok(event) => {
                        match event.kind {
                          StreamEventKind::ContentDelta => {
                            if !text_stream_started {
                              text_stream_started = true;
                              let output_item_added_payload = responses_stream_output_item_added_json(
                                &stream_response_id,
                                &stream_message_item_id,
                              );
                              yield Ok(
                                Event::default()
                                  .event("response.output_item.added")
                                  .data(output_item_added_payload.to_string())
                              );
                              let content_part_added_payload = responses_stream_content_part_added_json(
                                &stream_response_id,
                                &stream_message_item_id,
                              );
                              yield Ok(
                                Event::default()
                                  .event("response.content_part.added")
                                  .data(content_part_added_payload.to_string())
                              );
                            }
                            let delta = event.delta.unwrap_or_default();
                            streamed_text.push_str(&delta);
                            let payload = responses_stream_delta_json(
                              &stream_response_id,
                              &stream_message_item_id,
                              &delta,
                            );
                            yield Ok::<Event, Infallible>(
                              Event::default()
                                .event("response.output_text.delta")
                                .data(payload.to_string())
                            );
                          }
                          StreamEventKind::Done => {
                            if let Some(response) = event.response {
                              store
                                .record_request(
                                  tenant_id,
                                  Some(api_key_id),
                                  public_model.clone(),
                                  provider_kind.clone(),
                                  200,
                                  10,
                                  response.usage.clone(),
                                )
                                .await
                                .ok();
                              store
                                .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                                .await
                                .ok();
                              let final_text = if response.output_text.is_empty() {
                                streamed_text.clone()
                              } else {
                                response.output_text.clone()
                              };
                              if text_stream_started && !final_text.is_empty() {
                                let done_payload = responses_stream_done_json(
                                  &stream_response_id,
                                  &stream_message_item_id,
                                  &final_text,
                                );
                                yield Ok(Event::default().event("response.output_text.done").data(done_payload.to_string()));
                                let content_part_done_payload = responses_stream_content_part_done_json(
                                  &stream_response_id,
                                  &stream_message_item_id,
                                  &final_text,
                                );
                                yield Ok(Event::default().event("response.content_part.done").data(content_part_done_payload.to_string()));
                                let output_item_done_payload = responses_stream_output_item_done_json(
                                  &stream_response_id,
                                  &stream_message_item_id,
                                  &final_text,
                                );
                                yield Ok(Event::default().event("response.output_item.done").data(output_item_done_payload.to_string()));
                              }
                              let mut tool_call_item_ids = BTreeMap::new();
                              let tool_call_output_index_base = usize::from(text_stream_started && !final_text.is_empty());
                              for (index, tool_call) in response.tool_calls.iter().enumerate() {
                                let item_id = format!("fc_{}", Uuid::new_v4().simple());
                                tool_call_item_ids.insert(tool_call.id.clone(), item_id.clone());
                                let output_index = tool_call_output_index_base + index;
                                let added_payload = responses_stream_function_call_output_item_added_json(
                                  &stream_response_id,
                                  &item_id,
                                  output_index,
                                  tool_call,
                                );
                                yield Ok(Event::default().event("response.output_item.added").data(added_payload.to_string()));
                                if !tool_call.arguments.is_empty() {
                                  let delta_payload = responses_stream_function_call_arguments_delta_json(
                                    &stream_response_id,
                                    &item_id,
                                    output_index,
                                    tool_call,
                                  );
                                  yield Ok(Event::default().event("response.function_call_arguments.delta").data(delta_payload.to_string()));
                                }
                                let arguments_done_payload = responses_stream_function_call_arguments_done_json(
                                  &stream_response_id,
                                  &item_id,
                                  output_index,
                                  tool_call,
                                );
                                yield Ok(Event::default().event("response.function_call_arguments.done").data(arguments_done_payload.to_string()));
                                let output_item_done_payload = responses_stream_function_call_output_item_done_json(
                                  &stream_response_id,
                                  &item_id,
                                  output_index,
                                  tool_call,
                                );
                                yield Ok(Event::default().event("response.output_item.done").data(output_item_done_payload.to_string()));
                              }
                              let completed_payload = responses_stream_completed_json(
                                &stream_response_id,
                                text_stream_started.then_some(stream_message_item_id.as_str()),
                                &tool_call_item_ids,
                                response,
                              );
                              yield Ok(Event::default().event("response.completed").data(completed_payload.to_string()));
                            }
                          }
                          _ => {}
                        }
                      }
                      Err(error) => {
                        store
                          .mark_scheduler_outcome(
                            candidate.account_id,
                            provider_outcome_for_error(&error),
                          )
                          .await
                          .ok();
                        yield Ok(Event::default().event("response.failed").data(json!({
                          "type": "response.failed",
                          "response_id": stream_response_id,
                          "error": provider_error_body(&error)
                        }).to_string()));
                      }
                    }
                  }
                };

                return Sse::new(stream).into_response();
            }
            Err(error) => {
                state
                    .store
                    .mark_scheduler_outcome(
                        candidate.account_id,
                        provider_outcome_for_error(&error),
                    )
                    .await
                    .ok();
                return provider_error_response(error);
            }
        }
    }

    match provider.responses(canonical).await {
        Ok(response) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                .await
                .ok();
            state
                .store
                .record_request(
                    auth.tenant.id,
                    Some(auth.api_key_id),
                    request.model.clone(),
                    candidate.provider_kind,
                    200,
                    8,
                    response.usage.clone(),
                )
                .await
                .ok();
            Json(responses_json(response)).into_response()
        }
        Err(error) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, provider_outcome_for_error(&error))
                .await
                .ok();
            provider_error_response(error)
        }
    }
}

async fn authenticate_gateway(
    state: &GatewayAppState,
    headers: &HeaderMap,
) -> Result<GatewayAuthContext, Response> {
    let Some(token) = parse_bearer_token(headers) else {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Missing bearer token",
        ));
    };

    state
        .store
        .validate_gateway_api_key(&token)
        .await
        .map_err(|error| internal_error(&error.to_string()))?
        .ok_or_else(|| openai_error(StatusCode::UNAUTHORIZED, "Invalid API key"))
}

fn parse_bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(ToString::to_string)
}

fn parse_message_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn openai_message_to_canonical_message(message: &OpenAiMessage) -> CanonicalMessage {
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

fn openai_tools_to_canonical_tools(tools: &[OpenAiToolDefinition]) -> Vec<ToolDefinition> {
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

fn responses_tools_to_canonical_tools(tools: &[ResponsesToolDefinition]) -> Vec<ToolDefinition> {
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
        .join("\n")
}

fn responses_input_to_messages(input: Value) -> Vec<CanonicalMessage> {
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

fn chat_completion_json(response: InferenceResponse) -> Value {
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

fn responses_json(response: InferenceResponse) -> Value {
    let mut output = Vec::new();
    if !response.output_text.is_empty() {
        output.push(json!({
          "id": format!("msg_{}", Uuid::new_v4().simple()),
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
          "id": format!("fc_{}", Uuid::new_v4().simple()),
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

fn responses_stream_created_json(response_id: &str, model: &str) -> Value {
    json!({
      "type": "response.created",
      "response": {
        "id": response_id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
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

fn responses_stream_delta_json(response_id: &str, item_id: &str, delta: &str) -> Value {
    json!({
      "type": "response.output_text.delta",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": 0,
      "content_index": 0,
      "delta": delta
    })
}

fn responses_stream_done_json(response_id: &str, item_id: &str, text: &str) -> Value {
    json!({
      "type": "response.output_text.done",
      "response_id": response_id,
      "item_id": item_id,
      "output_index": 0,
      "content_index": 0,
      "text": text
    })
}

fn responses_stream_output_item_added_json(response_id: &str, item_id: &str) -> Value {
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

fn responses_stream_content_part_added_json(response_id: &str, item_id: &str) -> Value {
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

fn responses_stream_content_part_done_json(response_id: &str, item_id: &str, text: &str) -> Value {
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

fn responses_stream_output_item_done_json(response_id: &str, item_id: &str, text: &str) -> Value {
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

fn responses_stream_function_call_output_item_added_json(
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

fn responses_stream_function_call_arguments_delta_json(
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

fn responses_stream_function_call_arguments_done_json(
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

fn responses_stream_function_call_output_item_done_json(
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

fn responses_stream_completed_json(
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

fn model_capability_label(capability: &ModelCapability) -> &'static str {
    match capability {
        ModelCapability::Chat => "chat",
        ModelCapability::Responses => "responses",
        ModelCapability::Streaming => "streaming",
        ModelCapability::Tools => "tools",
    }
}

fn provider_outcome_for_error(error: &ProviderError) -> ProviderOutcome {
    match error.kind {
        ProviderErrorKind::RateLimited => ProviderOutcome::RateLimited {
            retry_after_seconds: Some(30),
        },
        ProviderErrorKind::InvalidCredentials => ProviderOutcome::InvalidCredentials,
        ProviderErrorKind::UpstreamUnavailable => ProviderOutcome::UpstreamFailure,
        ProviderErrorKind::InvalidRequest | ProviderErrorKind::Unsupported => {
            ProviderOutcome::TransportFailure
        }
    }
}

fn provider_error_response(error: ProviderError) -> Response {
    let status = StatusCode::from_u16(error.status_code).unwrap_or(StatusCode::BAD_GATEWAY);
    Json(json!({
      "error": provider_error_body(&error)
    }))
    .into_response()
    .with_status(status)
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

fn openai_error(status: StatusCode, message: &str) -> Response {
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

fn internal_error(message: &str) -> Response {
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

trait ResponseExt {
    fn with_status(self, status: StatusCode) -> Response;
}

impl ResponseExt for Response {
    fn with_status(mut self, status: StatusCode) -> Response {
        *self.status_mut() = status;
        self
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(default)]
    tools: Vec<OpenAiToolDefinition>,
    #[serde(default)]
    reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ResponsesRequest {
    model: String,
    input: Value,
    #[serde(default, deserialize_with = "deserialize_optional_string_placeholder")]
    previous_response_id: Option<String>,
    #[serde(default)]
    tools: Vec<ResponsesToolDefinition>,
    #[serde(default)]
    reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiMessage {
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
enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

impl Default for OpenAiMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiContentPart {
    #[serde(rename = "type")]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum ResponsesToolDefinition {
    Flat(FlatResponsesToolDefinition),
    Nested(OpenAiToolDefinition),
}

#[derive(Debug, Deserialize, Serialize)]
struct FlatResponsesToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiFunctionDefinition {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
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
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Request as AxumRequest,
        http::Request,
        response::IntoResponse,
        routing::{get, post},
    };
    use protocol_core::{ModelCapability, ModelDescriptor};
    use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};
    use serde_json::json;
    use std::{collections::BTreeMap, net::SocketAddr};
    use tower::util::ServiceExt;

    fn sse_event_payloads(body: &str, event_name: &str) -> Vec<Value> {
        body.split("\n\n")
            .filter_map(|frame| {
                let mut lines = frame.lines();
                let event = lines.next()?.strip_prefix("event: ")?;
                if event != event_name {
                    return None;
                }
                let data = lines.next()?.strip_prefix("data: ")?;
                serde_json::from_str(data).ok()
            })
            .collect()
    }

    fn sse_data_payloads(body: &str) -> Vec<String> {
        body.split("\n\n")
            .filter_map(|frame| {
                frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: ").map(ToString::to_string))
            })
            .collect()
    }

    async fn spawn_codex_endpoint_server() -> SocketAddr {
        async fn method_not_allowed() -> impl IntoResponse {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                axum::Json(json!({ "detail": "Method Not Allowed" })),
            )
        }

        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );
            assert_eq!(
                body.get("instructions").and_then(Value::as_str),
                Some("You are Codex.")
            );
            assert_eq!(body.get("store").and_then(Value::as_bool), Some(false));
            assert_eq!(body.get("stream").and_then(Value::as_bool), Some(true));
            assert_eq!(
                body.pointer("/input/0/type").and_then(Value::as_str),
                Some("message")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("hello")
            );

            let payload = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_codex_123\",\"model\":\"gpt-5.1-codex\",\"output\":[]}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"hello \",\"item_id\":\"msg_codex_123\",\"output_index\":0}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"from codex\",\"item_id\":\"msg_codex_123\",\"output_index\":0}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_codex_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_codex_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"hello from codex\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":3,\"total_tokens\":8}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        async fn codex_chat_handler() -> impl IntoResponse {
            (
                StatusCode::FORBIDDEN,
                [
                    (
                        http::header::CONTENT_TYPE.as_str(),
                        "text/html; charset=UTF-8",
                    ),
                    ("cf-mitigated", "challenge"),
                ],
                "<html><body>Enable JavaScript and cookies to continue</body></html>",
            )
                .into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                get(method_not_allowed).post(codex_responses_handler),
            )
            .route(
                "/backend-api/codex/chat/completions",
                post(codex_chat_handler),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_tool_call_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.pointer("/tools/0/type").and_then(Value::as_str),
                Some("function")
            );
            assert_eq!(
                body.pointer("/tools/0/name").and_then(Value::as_str),
                Some("get_weather")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/type")
                    .and_then(Value::as_str),
                Some("input_text")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("What is the weather in Shanghai?")
            );
            assert_eq!(
                body.pointer("/input/0/content/1/type")
                    .and_then(Value::as_str),
                Some("input_image")
            );
            assert_eq!(
                body.pointer("/input/0/content/1/image_url")
                    .and_then(Value::as_str),
                Some("https://example.com/weather.png")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"fc_123\",\"type\":\"function_call\",\"call_id\":\"call_weather_123\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}],\"usage\":{\"input_tokens\":12,\"output_tokens\":4,\"total_tokens\":16}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_tool_result_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(
                body.pointer("/input/0/type").and_then(Value::as_str),
                Some("function_call")
            );
            assert_eq!(
                body.pointer("/input/0/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/0/name").and_then(Value::as_str),
                Some("get_weather")
            );
            assert_eq!(
                body.pointer("/input/0/arguments").and_then(Value::as_str),
                Some("{\"city\":\"Shanghai\"}")
            );
            assert_eq!(
                body.pointer("/input/1/type").and_then(Value::as_str),
                Some("function_call_output")
            );
            assert_eq!(
                body.pointer("/input/1/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/1/output").and_then(Value::as_str),
                Some("{\"temperature_c\":25}")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_result_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_tool_result_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"Shanghai is 25C.\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":14,\"output_tokens\":4,\"total_tokens\":18}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_empty_completed_output_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(body.get("stream").and_then(Value::as_bool), Some(true),);
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("Reply with exactly: pong")
            );

            let payload = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_pong_123\",\"model\":\"gpt-5.1-codex\",\"output\":[]}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"msg_pong_123\",\"type\":\"message\",\"status\":\"in_progress\",\"content\":[],\"role\":\"assistant\"},\"output_index\":0}\n\n",
                "event: response.content_part.added\n",
                "data: {\"type\":\"response.content_part.added\",\"content_index\":0,\"item_id\":\"msg_pong_123\",\"output_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"pong\",\"item_id\":\"msg_pong_123\",\"output_index\":0}\n\n",
                "event: response.output_text.done\n",
                "data: {\"type\":\"response.output_text.done\",\"content_index\":0,\"item_id\":\"msg_pong_123\",\"output_index\":0,\"text\":\"pong\"}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg_pong_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"pong\"}],\"role\":\"assistant\"},\"output_index\":0}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_pong_123\",\"model\":\"gpt-5.1-codex\",\"output\":[],\"usage\":{\"input_tokens\":20,\"output_tokens\":5,\"total_tokens\":25}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_chat_tool_result_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert!(body.get("previous_response_id").is_none());
            assert_eq!(
                body.pointer("/input/0/type").and_then(Value::as_str),
                Some("message")
            );
            assert_eq!(
                body.pointer("/input/0/role").and_then(Value::as_str),
                Some("user")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("What is the weather in Shanghai?")
            );
            assert_eq!(
                body.pointer("/input/1/type").and_then(Value::as_str),
                Some("function_call")
            );
            assert_eq!(
                body.pointer("/input/1/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/1/name").and_then(Value::as_str),
                Some("get_weather")
            );
            assert_eq!(
                body.pointer("/input/1/arguments").and_then(Value::as_str),
                Some("{\"city\":\"Shanghai\"}")
            );
            assert_eq!(
                body.pointer("/input/2/type").and_then(Value::as_str),
                Some("function_call_output")
            );
            assert_eq!(
                body.pointer("/input/2/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/2/output").and_then(Value::as_str),
                Some("{\"temperature_c\":25}")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_result_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_tool_result_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"Shanghai is 25C.\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":14,\"output_tokens\":4,\"total_tokens\":18}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_reasoning_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.pointer("/reasoning/effort").and_then(Value::as_str),
                Some("xhigh")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_reasoning_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_reasoning_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"reasoned\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":8,\"output_tokens\":2,\"total_tokens\":10}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_assistant_history_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.pointer("/input/0/role").and_then(Value::as_str),
                Some("user")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/type")
                    .and_then(Value::as_str),
                Some("input_text")
            );
            assert_eq!(
                body.pointer("/input/1/role").and_then(Value::as_str),
                Some("assistant")
            );
            assert_eq!(
                body.pointer("/input/1/content/0/type")
                    .and_then(Value::as_str),
                Some("output_text")
            );
            assert_eq!(
                body.pointer("/input/1/content/0/text")
                    .and_then(Value::as_str),
                Some("第一轮回答")
            );
            assert_eq!(
                body.pointer("/input/2/role").and_then(Value::as_str),
                Some("user")
            );
            assert_eq!(
                body.pointer("/input/2/content/0/type")
                    .and_then(Value::as_str),
                Some("input_text")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_history_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_history_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"第二轮回答\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":11,\"output_tokens\":3,\"total_tokens\":14}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_failure_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );

            let payload = concat!(
                "event: response.failed\n",
                "data: {\"message\":\"upstream exploded\"}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_stream_token_invalidated_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );

            let payload = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_token_invalidated_123\",\"model\":\"gpt-5.1-codex\",\"output\":[]}}\n\n",
                "event: response.failed\n",
                "data: {\"type\":\"response.failed\",\"response_id\":\"resp_token_invalidated_123\",\"error\":{\"message\":\"Your authentication token has been invalidated. Please try signing in again.\",\"type\":\"invalid_request_error\",\"code\":\"token_invalidated\",\"param\":null}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_token_invalidated_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );

            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({
                    "error": {
                        "message": "Your authentication token has been invalidated. Please try signing in again.",
                        "type": "invalid_request_error",
                        "code": "token_invalidated",
                        "param": Value::Null
                    }
                })),
            )
                .into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_challenge_server() -> SocketAddr {
        async fn codex_responses_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer gateway-codex-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );

            (
                StatusCode::FORBIDDEN,
                [
                    (
                        http::header::CONTENT_TYPE.as_str(),
                        "text/html; charset=UTF-8",
                    ),
                    ("cf-mitigated", "challenge"),
                ],
                "<html><body>Enable JavaScript and cookies to continue</body></html>",
            )
                .into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (StatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn state_with_codex_route(api_base: &str) -> GatewayAppState {
        let state = GatewayAppState::demo();
        let account = state
            .store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "access_token": "gateway-codex-token",
                        "account_id": "acct_gateway_codex",
                        "api_base": api_base
                    }),
                    metadata: json!({ "email": "gateway@example.com" }),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_gateway_codex".to_string(),
                    redacted_display: Some("g***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-5-codex".to_string(),
                        route_group: "gpt-5-codex".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-5-codex".to_string(),
                        capabilities: vec![
                            ModelCapability::Chat,
                            ModelCapability::Responses,
                            ModelCapability::Streaming,
                        ],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("provider account");
        let route_group = state
            .store
            .create_route_group(
                "gpt-5-codex".to_string(),
                "openai_codex".to_string(),
                "gpt-5-codex".to_string(),
            )
            .await
            .expect("route group");
        state
            .store
            .bind_provider_account(route_group.id, account.id, 100, 16)
            .await
            .expect("binding");
        state
    }

    async fn demo_tenant_id(state: &GatewayAppState) -> uuid::Uuid {
        state
            .store
            .list_tenants()
            .await
            .expect("tenants")
            .first()
            .expect("tenant")
            .id
    }

    #[tokio::test]
    async fn models_endpoint_requires_valid_key() {
        let app = app(GatewayAppState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn newly_created_tenant_key_can_access_gateway() {
        let state = GatewayAppState::demo();
        let tenant_id = state
            .store
            .list_tenants()
            .await
            .expect("tenants")
            .first()
            .expect("tenant")
            .id;
        let created = state
            .store
            .create_tenant_api_key(tenant_id, "integration".to_string())
            .await
            .expect("key");
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(
                        http::header::AUTHORIZATION,
                        format!("Bearer {}", created.secret),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("gpt-4.1-mini"));
    }

    #[tokio::test]
    async fn responses_endpoint_routes_codex_requests_and_records_usage() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let tenant_id = demo_tenant_id(&state).await;
        let app = app(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body.get("object").and_then(Value::as_str), Some("response"));
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-5.1-codex")
        );
        assert_eq!(
            body.pointer("/output/0/content/0/text")
                .and_then(Value::as_str),
            Some("hello from codex")
        );
        assert_eq!(
            body.pointer("/usage/total_tokens").and_then(Value::as_u64),
            Some(8)
        );

        let requests = state
            .store
            .tenant_requests(tenant_id)
            .await
            .expect("requests");
        let record = requests
            .into_iter()
            .find(|request| request.public_model == "gpt-5-codex")
            .expect("recorded request");
        assert_eq!(record.provider_kind, "openai_codex");
        assert_eq!(record.status_code, 200);
        assert_eq!(record.usage.total_tokens, 8);
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_openai_style_response_events() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("event: response.created"));
        assert!(body.contains("\"type\":\"response.created\""));
        assert!(body.contains("event: response.output_text.delta"));
        assert!(body.contains("\"type\":\"response.output_text.delta\""));
        assert!(body.contains("event: response.output_text.done"));
        assert!(body.contains("\"type\":\"response.output_text.done\""));
        assert!(body.contains("event: response.completed"));
        assert!(body.contains("\"type\":\"response.completed\""));
        assert!(body.contains("\"object\":\"response\""));
        assert!(body.contains("\"model\":\"gpt-5.1-codex\""));
        assert!(body.contains("\"text\":\"hello from codex\""));
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_output_item_added_event() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("event: response.output_item.added"));
        assert!(body.contains("\"type\":\"response.output_item.added\""));
        assert!(body.contains("\"status\":\"in_progress\""));
        assert!(body.contains("\"role\":\"assistant\""));
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_content_part_and_output_item_done_events() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("event: response.content_part.added"));
        assert!(body.contains("\"type\":\"response.content_part.added\""));
        assert!(body.contains("event: response.content_part.done"));
        assert!(body.contains("\"type\":\"response.content_part.done\""));
        assert!(body.contains("event: response.output_item.done"));
        assert!(body.contains("\"type\":\"response.output_item.done\""));
        assert!(body.contains("\"status\":\"completed\""));
        assert!(body.contains("\"text\":\"hello from codex\""));
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_tool_call_events() {
        let addr = spawn_codex_tool_call_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": [{
                                "type": "message",
                                "role": "user",
                                "content": [
                                    { "type": "input_text", "text": "What is the weather in Shanghai?" },
                                    {
                                        "type": "input_image",
                                        "image_url": "https://example.com/weather.png"
                                    }
                                ]
                            }],
                            "tools": [{
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "description": "Fetch current weather",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {
                                            "city": { "type": "string" }
                                        },
                                        "required": ["city"]
                                    }
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);

        let added = sse_event_payloads(&body, "response.output_item.added");
        assert_eq!(added.len(), 1);
        assert_eq!(
            added[0].pointer("/item/type").and_then(Value::as_str),
            Some("function_call")
        );
        assert_eq!(
            added[0].pointer("/item/name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            added[0].pointer("/item/arguments").and_then(Value::as_str),
            Some("")
        );

        let item_id = added[0]
            .pointer("/item/id")
            .and_then(Value::as_str)
            .expect("tool call item id")
            .to_string();

        let argument_deltas = sse_event_payloads(&body, "response.function_call_arguments.delta");
        assert_eq!(argument_deltas.len(), 1);
        assert_eq!(
            argument_deltas[0]
                .pointer("/item_id")
                .and_then(Value::as_str),
            Some(item_id.as_str())
        );
        assert_eq!(
            argument_deltas[0].pointer("/delta").and_then(Value::as_str),
            Some("{\"city\":\"Shanghai\"}")
        );

        let argument_done = sse_event_payloads(&body, "response.function_call_arguments.done");
        assert_eq!(argument_done.len(), 1);
        assert_eq!(
            argument_done[0].pointer("/item_id").and_then(Value::as_str),
            Some(item_id.as_str())
        );
        assert_eq!(
            argument_done[0]
                .pointer("/arguments")
                .and_then(Value::as_str),
            Some("{\"city\":\"Shanghai\"}")
        );

        let output_item_done = sse_event_payloads(&body, "response.output_item.done");
        assert_eq!(output_item_done.len(), 1);
        assert_eq!(
            output_item_done[0]
                .pointer("/item/id")
                .and_then(Value::as_str),
            Some(item_id.as_str())
        );
        assert_eq!(
            output_item_done[0]
                .pointer("/item/type")
                .and_then(Value::as_str),
            Some("function_call")
        );

        let completed = sse_event_payloads(&body, "response.completed");
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0]
                .pointer("/response/output/0/id")
                .and_then(Value::as_str),
            Some(item_id.as_str())
        );
        assert_eq!(
            completed[0]
                .pointer("/response/output/0/call_id")
                .and_then(Value::as_str),
            Some("call_weather_123")
        );
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_failed_event_payload() {
        let addr = spawn_codex_failure_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("event: response.created"));
        assert!(body.contains("event: response.failed"));
        assert!(body.contains("\"type\":\"response.failed\""));
        assert!(body.contains("\"message\":\"upstream exploded\""));
    }

    #[tokio::test]
    async fn responses_streaming_endpoint_emits_openai_style_error_details() {
        let addr = spawn_codex_stream_token_invalidated_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        let created = sse_event_payloads(&body, "response.created");
        let failed = sse_event_payloads(&body, "response.failed");
        assert_eq!(created.len(), 1);
        assert_eq!(failed.len(), 1);
        assert_eq!(
            failed[0].pointer("/response_id").and_then(Value::as_str),
            created[0].pointer("/response/id").and_then(Value::as_str)
        );
        assert_eq!(
            failed[0].pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            failed[0].pointer("/error/code").and_then(Value::as_str),
            Some("token_invalidated")
        );
        assert_eq!(
            failed[0].pointer("/error/message").and_then(Value::as_str),
            Some("Your authentication token has been invalidated. Please try signing in again.")
        );
    }

    #[tokio::test]
    async fn responses_endpoint_maps_token_invalidated_to_openai_error() {
        let addr = spawn_codex_token_invalidated_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(
            body.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            body.pointer("/error/code").and_then(Value::as_str),
            Some("token_invalidated")
        );
        assert_eq!(
            body.pointer("/error/message").and_then(Value::as_str),
            Some("Your authentication token has been invalidated. Please try signing in again.")
        );
    }

    #[tokio::test]
    async fn responses_endpoint_maps_upstream_challenge_to_server_error() {
        let addr = spawn_codex_challenge_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "input": "hello"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(
            body.pointer("/error/type").and_then(Value::as_str),
            Some("server_error")
        );
        assert_eq!(
            body.pointer("/error/code").and_then(Value::as_str),
            Some("upstream_challenge")
        );
        assert_eq!(
            body.pointer("/error/message").and_then(Value::as_str),
            Some("Upstream challenge requires interactive verification.")
        );
    }

    #[tokio::test]
    async fn models_endpoint_only_lists_route_groups_supported_by_bound_upstream_account() {
        let state = GatewayAppState::demo();
        let route_group = state
            .store
            .create_route_group(
                "gpt-5-codex".to_string(),
                "openai_codex".to_string(),
                "gpt-5-codex".to_string(),
            )
            .await
            .expect("route group");
        let account_id = state
            .store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .first()
            .expect("account")
            .id;
        state
            .store
            .bind_provider_account(route_group.id, account_id, 100, 16)
            .await
            .expect("binding");

        let app = app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("gpt-4.1-mini"));
        assert!(!body.contains("gpt-5-codex"));
    }

    #[tokio::test]
    async fn models_endpoint_exposes_model_capabilities() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        let codex_model = body["data"]
            .as_array()
            .expect("models array")
            .iter()
            .find(|model| model.get("id").and_then(Value::as_str) == Some("gpt-5-codex"))
            .expect("codex model");

        let capabilities = codex_model["capabilities"]
            .as_array()
            .expect("capabilities array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(capabilities.contains(&"chat"));
        assert!(capabilities.contains(&"responses"));
        assert!(capabilities.contains(&"streaming"));
        assert!(capabilities.contains(&"tools"));
    }

    #[tokio::test]
    async fn chat_completions_supports_image_inputs_and_tool_calls() {
        let addr = spawn_codex_tool_call_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "messages": [{
                                "role": "user",
                                "content": [
                                    { "type": "text", "text": "What is the weather in Shanghai?" },
                                    {
                                        "type": "image_url",
                                        "image_url": { "url": "https://example.com/weather.png" }
                                    }
                                ]
                            }],
                            "tools": [{
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "description": "Fetch current weather",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {
                                            "city": { "type": "string" }
                                        },
                                        "required": ["city"]
                                    }
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/choices/0/finish_reason")
                .and_then(Value::as_str),
            Some("tool_calls")
        );
        assert!(body.pointer("/choices/0/message/content").is_some());
        assert_eq!(
            body.pointer("/choices/0/message/tool_calls/0/function/name")
                .and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            body.pointer("/choices/0/message/tool_calls/0/function/arguments")
                .and_then(Value::as_str),
            Some("{\"city\":\"Shanghai\"}")
        );
    }

    #[tokio::test]
    async fn responses_supports_image_inputs_and_tool_calls() {
        let addr = spawn_codex_tool_call_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "input": [{
                                "type": "message",
                                "role": "user",
                                "content": [
                                    { "type": "input_text", "text": "What is the weather in Shanghai?" },
                                    {
                                        "type": "input_image",
                                        "image_url": "https://example.com/weather.png"
                                    }
                                ]
                            }],
                            "tools": [{
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "description": "Fetch current weather",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {
                                            "city": { "type": "string" }
                                        },
                                        "required": ["city"]
                                    }
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body.get("object").and_then(Value::as_str), Some("response"));
        assert_eq!(
            body.pointer("/output/0/type").and_then(Value::as_str),
            Some("function_call")
        );
        assert_eq!(
            body.pointer("/output/0/name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            body.pointer("/output/0/arguments").and_then(Value::as_str),
            Some("{\"city\":\"Shanghai\"}")
        );
    }

    #[tokio::test]
    async fn responses_accepts_flat_function_tools_and_undefined_previous_response_id() {
        let addr = spawn_codex_tool_call_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "previous_response_id": "[undefined]",
                            "input": [{
                                "type": "message",
                                "role": "user",
                                "content": [
                                    { "type": "input_text", "text": "What is the weather in Shanghai?" },
                                    {
                                        "type": "input_image",
                                        "image_url": "https://example.com/weather.png"
                                    }
                                ]
                            }],
                            "tools": [{
                                "type": "function",
                                "name": "get_weather",
                                "description": "Fetch current weather",
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "city": { "type": "string" }
                                    },
                                    "required": ["city"]
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/output/0/name").and_then(Value::as_str),
            Some("get_weather")
        );
    }

    #[tokio::test]
    async fn responses_non_stream_uses_streamed_text_when_completed_payload_omits_output() {
        let addr = spawn_codex_empty_completed_output_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": false,
                            "input": "Reply with exactly: pong"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/output/0/content/0/text")
                .and_then(Value::as_str),
            Some("pong")
        );
    }

    #[tokio::test]
    async fn responses_stream_completed_payload_uses_streamed_text_when_completed_output_is_empty()
    {
        let addr = spawn_codex_empty_completed_output_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "input": "Reply with exactly: pong"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        let completed = sse_event_payloads(&body, "response.completed");
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0]
                .pointer("/response/output/0/content/0/text")
                .and_then(Value::as_str),
            Some("pong")
        );
    }

    #[test]
    fn request_id_helper_generates_prefixed_openai_ids() {
        let id = crate::middleware::request_id::new_openai_object_id("chatcmpl");

        assert!(id.starts_with("chatcmpl_"));
        assert!(id.len() > "chatcmpl_".len());
    }

    #[tokio::test]
    async fn chat_completions_supports_tool_result_roundtrip() {
        let addr = spawn_codex_chat_tool_result_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "messages": [
                                {
                                    "role": "user",
                                    "content": "What is the weather in Shanghai?"
                                },
                                {
                                    "role": "assistant",
                                    "content": "",
                                    "tool_calls": [{
                                        "id": "call_weather_123",
                                        "type": "function",
                                        "function": {
                                            "name": "get_weather",
                                            "arguments": "{\"city\":\"Shanghai\"}"
                                        }
                                    }]
                                },
                                {
                                    "role": "tool",
                                    "tool_call_id": "call_weather_123",
                                    "content": "{\"temperature_c\":25}"
                                }
                            ],
                            "tools": [{
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "description": "Fetch current weather",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {
                                            "city": { "type": "string" }
                                        },
                                        "required": ["city"]
                                    }
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("Shanghai is 25C.")
        );
        assert_eq!(
            body.pointer("/choices/0/finish_reason")
                .and_then(Value::as_str),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn responses_support_previous_response_ids_and_function_call_outputs() {
        let addr = spawn_codex_tool_result_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "previous_response_id": "resp_tool_123",
                            "input": [
                                {
                                    "type": "function_call",
                                    "call_id": "call_weather_123",
                                    "name": "get_weather",
                                    "arguments": "{\"city\":\"Shanghai\"}"
                                },
                                {
                                    "type": "function_call_output",
                                    "call_id": "call_weather_123",
                                    "output": "{\"temperature_c\":25}"
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/output/0/content/0/text")
                .and_then(Value::as_str),
            Some("Shanghai is 25C.")
        );
    }

    #[tokio::test]
    async fn chat_completions_forward_reasoning_effort_to_codex_responses() {
        let addr = spawn_codex_reasoning_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "reasoning": {
                                "effort": "xhigh"
                            },
                            "messages": [{
                                "role": "user",
                                "content": "hello"
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("reasoned")
        );
    }

    #[tokio::test]
    async fn responses_forward_reasoning_effort_to_codex_responses() {
        let addr = spawn_codex_reasoning_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/responses")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "reasoning": {
                                "effort": "xhigh"
                            },
                            "input": [{
                                "role": "user",
                                "content": [{
                                    "type": "input_text",
                                    "text": "hello"
                                }]
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/output/0/content/0/text")
                .and_then(Value::as_str),
            Some("reasoned")
        );
    }

    #[tokio::test]
    async fn chat_completions_streaming_endpoint_wraps_codex_output_as_openai_chunks() {
        let addr = spawn_codex_endpoint_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let tenant_id = demo_tenant_id(&state).await;
        let app = app(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "messages": [{
                                "role": "user",
                                "content": "hello"
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .contains("text/event-stream")
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("\"object\":\"chat.completion.chunk\""));
        assert!(body.contains("\"content\":\"hello \""));
        assert!(body.contains("\"content\":\"from codex\""));
        assert!(body.contains("[DONE]"));

        let requests = state
            .store
            .tenant_requests(tenant_id)
            .await
            .expect("requests");
        let record = requests
            .into_iter()
            .find(|request| request.public_model == "gpt-5-codex")
            .expect("recorded request");
        assert_eq!(record.provider_kind, "openai_codex");
        assert_eq!(record.status_code, 200);
        assert_eq!(record.usage.total_tokens, 8);
    }

    #[tokio::test]
    async fn chat_completions_supports_assistant_history_on_second_turn() {
        let addr = spawn_codex_assistant_history_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "messages": [
                                { "role": "user", "content": "第一轮提问" },
                                { "role": "assistant", "content": "第一轮回答" },
                                { "role": "user", "content": "继续问第二轮" }
                            ]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            body.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("第二轮回答")
        );
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-5.1-codex")
        );
    }

    #[tokio::test]
    async fn chat_completions_streaming_endpoint_emits_indexed_tool_call_chunks() {
        let addr = spawn_codex_tool_call_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "messages": [{
                                "role": "user",
                                "content": [
                                    { "type": "text", "text": "What is the weather in Shanghai?" },
                                    {
                                        "type": "image_url",
                                        "image_url": { "url": "https://example.com/weather.png" }
                                    }
                                ]
                            }],
                            "tools": [{
                                "type": "function",
                                "function": {
                                    "name": "get_weather",
                                    "description": "Fetch current weather",
                                    "parameters": {
                                        "type": "object",
                                        "properties": {
                                            "city": { "type": "string" }
                                        },
                                        "required": ["city"]
                                    }
                                }
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        let payloads = sse_data_payloads(&body);
        let json_payloads = payloads
            .iter()
            .filter(|payload| payload.as_str() != "[DONE]")
            .map(|payload| serde_json::from_str::<Value>(payload).expect("json payload"))
            .collect::<Vec<_>>();

        let tool_call_chunk = json_payloads
            .iter()
            .find(|payload| payload.pointer("/choices/0/delta/tool_calls").is_some())
            .expect("tool call chunk");
        assert_eq!(
            tool_call_chunk
                .pointer("/choices/0/delta/tool_calls/0/index")
                .and_then(Value::as_u64),
            Some(0)
        );
        assert_eq!(
            tool_call_chunk
                .pointer("/choices/0/delta/tool_calls/0/id")
                .and_then(Value::as_str),
            Some("call_weather_123")
        );
        assert_eq!(
            tool_call_chunk
                .pointer("/choices/0/delta/tool_calls/0/type")
                .and_then(Value::as_str),
            Some("function")
        );
        assert_eq!(
            tool_call_chunk
                .pointer("/choices/0/delta/tool_calls/0/function/name")
                .and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            tool_call_chunk
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                .and_then(Value::as_str),
            Some("{\"city\":\"Shanghai\"}")
        );

        let final_chunk = json_payloads.last().expect("final chunk");
        assert_eq!(
            final_chunk
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str),
            Some("tool_calls")
        );
        assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"));
    }

    #[tokio::test]
    async fn chat_completions_streaming_endpoint_emits_openai_style_error_chunk() {
        let addr = spawn_codex_stream_token_invalidated_server().await;
        let state = state_with_codex_route(&format!("http://{addr}/backend-api/codex")).await;
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(http::Method::POST)
                    .uri("/v1/chat/completions")
                    .header(http::header::AUTHORIZATION, "Bearer fgk_demo_gateway_key")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5-codex",
                            "stream": true,
                            "messages": [{
                                "role": "user",
                                "content": "hello"
                            }]
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        let payloads = sse_data_payloads(&body);
        let error_chunk = payloads
            .iter()
            .filter(|payload| payload.as_str() != "[DONE]")
            .map(|payload| serde_json::from_str::<Value>(payload).expect("json payload"))
            .find(|payload| payload.get("error").is_some())
            .expect("error chunk");

        assert_eq!(
            error_chunk.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            error_chunk.pointer("/error/code").and_then(Value::as_str),
            Some("token_invalidated")
        );
        assert_eq!(
            error_chunk
                .pointer("/error/message")
                .and_then(Value::as_str),
            Some("Your authentication token has been invalidated. Please try signing in again.")
        );
        assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"));
    }
}
