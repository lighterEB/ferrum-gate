mod core;
mod middleware;
mod openai_http;
mod routes;

use anyhow::Result;
use axum::{
    Router,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use protocol_core::ModelCapability;
use provider_anthropic::AnthropicProvider;
use provider_core::{ProviderError, ProviderErrorKind, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use provider_qwen::QwenProvider;
use scheduler::ProviderOutcome;
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use storage::PlatformStore;
use tower_http::cors::CorsLayer;
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
        registry.register(AnthropicProvider::shared(Arc::new(store.clone())));
        registry.register(OpenAiCodexProvider::shared(Arc::new(store.clone())));
        registry.register(QwenProvider::shared(Arc::new(store.clone())));

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
    http_utils::console_cors_layer_from_env()
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
mod tests;
