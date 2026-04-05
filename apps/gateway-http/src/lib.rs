use anyhow::Result;
use async_stream::stream;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Json, Response,
        sse::{Event, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt;
use protocol_core::{
    CanonicalMessage, FrontendProtocol, InferenceRequest, InferenceResponse, MessageRole,
    StreamEventKind,
};
use provider_core::{ProviderError, ProviderErrorKind, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use scheduler::ProviderOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, convert::Infallible, net::SocketAddr};
use storage::{GatewayAuthContext, InMemoryPlatformStore};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
pub struct GatewayAppState {
    pub store: InMemoryPlatformStore,
    pub registry: ProviderRegistry,
}

impl GatewayAppState {
    #[must_use]
    pub fn demo() -> Self {
        let mut registry = ProviderRegistry::new();
        registry.register(OpenAiCodexProvider::shared());

        Self {
            store: InMemoryPlatformStore::demo(),
            registry,
        }
    }
}

pub fn app(state: GatewayAppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .with_state(state)
}

pub async fn run(addr: SocketAddr, state: GatewayAppState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("gateway-http listening on {addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn list_models(State(state): State<GatewayAppState>, headers: HeaderMap) -> Response {
    let auth = match authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let models = state.store.list_tenant_models(auth.tenant.id).await;
    Json(json!({
      "object": "list",
      "data": models.into_iter().map(|model| {
        json!({
          "id": model.id,
          "object": "model",
          "owned_by": "ferrum-gate",
          "provider_kind": model.provider_kind,
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
        Some(candidate) => candidate,
        None => {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy provider candidate",
            );
        }
    };

    let route_group = match state.store.resolve_route_group(&request.model).await {
        Some(route_group) => route_group,
        None => return openai_error(StatusCode::NOT_FOUND, "Unknown model"),
    };

    let Some(provider) = state.registry.get(&candidate.provider_kind) else {
        return openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered");
    };

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: Some(route_group.upstream_model),
        stream: request.stream,
        messages: request
            .messages
            .iter()
            .map(|message| CanonicalMessage {
                role: parse_message_role(&message.role),
                content: message.content.clone(),
            })
            .collect(),
        metadata: BTreeMap::new(),
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
                          if let Some(response) = event.response {
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
                              .await;
                          }
                          store
                            .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                            .await;
                          yield Ok(Event::default().data(payload.to_string()));
                          yield Ok(Event::default().data("[DONE]"));
                        }
                        _ => {}
                      },
                      Err(error) => {
                        let payload = json!({
                          "error": {
                            "message": error.message,
                            "type": "provider_error",
                            "code": format!("{:?}", error.kind).to_lowercase(),
                            "param": Value::Null
                          }
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
                    .await;
                return provider_error_response(error);
            }
        }
    }

    match provider.chat(canonical).await {
        Ok(response) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                .await;
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
                .await;
            Json(chat_completion_json(response)).into_response()
        }
        Err(error) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, provider_outcome_for_error(&error))
                .await;
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
        Some(candidate) => candidate,
        None => {
            return openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "No healthy provider candidate",
            );
        }
    };

    let route_group = match state.store.resolve_route_group(&request.model).await {
        Some(route_group) => route_group,
        None => return openai_error(StatusCode::NOT_FOUND, "Unknown model"),
    };
    let Some(provider) = state.registry.get(&candidate.provider_kind) else {
        return openai_error(StatusCode::BAD_GATEWAY, "Provider adapter not registered");
    };

    let canonical = InferenceRequest {
        protocol: FrontendProtocol::OpenAi,
        public_model: request.model.clone(),
        upstream_model: Some(route_group.upstream_model),
        stream: request.stream,
        messages: responses_input_to_messages(request.input),
        metadata: BTreeMap::new(),
    };

    if request.stream {
        match provider.stream_responses(canonical).await {
            Ok(mut provider_stream) => {
                let store = state.store.clone();
                let tenant_id = auth.tenant.id;
                let api_key_id = auth.api_key_id;
                let public_model = request.model.clone();
                let provider_kind = candidate.provider_kind.clone();
                let stream = stream! {
                  while let Some(item) = provider_stream.next().await {
                    match item {
                      Ok(event) => {
                        match event.kind {
                          StreamEventKind::ContentDelta => {
                            let payload = json!({ "delta": event.delta.unwrap_or_default(), "model": public_model });
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
                                  response.usage,
                                )
                                .await;
                            }
                            store
                              .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                              .await;
                            yield Ok(Event::default().event("response.completed").data("{}"));
                          }
                          _ => {}
                        }
                      }
                      Err(error) => {
                        yield Ok(Event::default().event("response.failed").data(json!({"message": error.message}).to_string()));
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
                    .await;
                return provider_error_response(error);
            }
        }
    }

    match provider.responses(canonical).await {
        Ok(response) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, ProviderOutcome::Success)
                .await;
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
                .await;
            Json(responses_json(response)).into_response()
        }
        Err(error) => {
            state
                .store
                .mark_scheduler_outcome(candidate.account_id, provider_outcome_for_error(&error))
                .await;
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

fn responses_input_to_messages(input: Value) -> Vec<CanonicalMessage> {
    match input {
        Value::String(text) => vec![CanonicalMessage {
            role: MessageRole::User,
            content: text,
        }],
        Value::Array(items) => items
            .into_iter()
            .map(|item| CanonicalMessage {
                role: MessageRole::User,
                content: item
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
        other => vec![CanonicalMessage {
            role: MessageRole::User,
            content: other.to_string(),
        }],
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
          "content": response.output_text,
        },
        "finish_reason": "stop"
      }],
      "usage": {
        "prompt_tokens": response.usage.input_tokens,
        "completion_tokens": response.usage.output_tokens,
        "total_tokens": response.usage.total_tokens
      }
    })
}

fn responses_json(response: InferenceResponse) -> Value {
    json!({
      "id": response.id,
      "object": "response",
      "created_at": response.created_at.timestamp(),
      "model": response.model,
      "output": [{
        "id": format!("msg_{}", Uuid::new_v4().simple()),
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
          "type": "output_text",
          "text": response.output_text
        }]
      }],
      "usage": {
        "input_tokens": response.usage.input_tokens,
        "output_tokens": response.usage.output_tokens,
        "total_tokens": response.usage.total_tokens
      }
    })
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
      "error": {
        "message": error.message,
        "type": "provider_error",
        "code": format!("{:?}", error.kind).to_lowercase(),
        "param": Value::Null
      }
    }))
    .into_response()
    .with_status(status)
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
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ResponsesRequest {
    model: String,
    input: Value,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::util::ServiceExt;

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
        let tenant_id = state.store.list_tenants().await[0].id;
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
}
