mod middleware;
mod openai_http;
mod routes;

use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, Method},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use protocol_core::ModelCapability;
use provider_core::{ProviderError, ProviderErrorKind, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use scheduler::ProviderOutcome;
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use storage::PlatformStore;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;

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

/// Gateway app wiring and shared helpers stay here; route handlers, auth middleware,
/// and OpenAI wire DTO/formatting logic live in dedicated ingress modules.
pub fn app(state: GatewayAppState) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(routes::chat::chat_completions))
        .route("/v1/responses", post(routes::responses::responses))
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
    let auth = match middleware::auth::authenticate_gateway(&state, &headers).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };

    let models = match state.store.list_tenant_models(auth.tenant.id).await {
        Ok(models) => models,
        Err(error) => return openai_http::internal_error(&error.to_string()),
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

fn model_capability_label(capability: &ModelCapability) -> &'static str {
    match capability {
        ModelCapability::Chat => "chat",
        ModelCapability::Responses => "responses",
        ModelCapability::Streaming => "streaming",
        ModelCapability::Tools => "tools",
    }
}

pub(crate) fn provider_outcome_for_error(error: &ProviderError) -> ProviderOutcome {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Request as AxumRequest,
        http::{Request, StatusCode},
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
    async fn chat_completions_endpoint_routes_codex_requests_and_records_usage() {
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
            body.get("object").and_then(Value::as_str),
            Some("chat.completion")
        );
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-5.1-codex")
        );
        assert_eq!(
            body.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("hello from codex")
        );
        assert_eq!(
            body.pointer("/choices/0/finish_reason")
                .and_then(Value::as_str),
            Some("stop")
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
