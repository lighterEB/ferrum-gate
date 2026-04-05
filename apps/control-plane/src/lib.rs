use anyhow::Result;
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use provider_core::{ProviderAccountEnvelope, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use scheduler::AccountState;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use storage::{AuthError, InMemoryPlatformStore, Permission, ScopeTarget, ServiceAccountPrincipal};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
pub struct ControlPlaneState {
    pub store: InMemoryPlatformStore,
    pub registry: ProviderRegistry,
}

impl ControlPlaneState {
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

pub fn app(state: ControlPlaneState) -> Router {
    Router::new()
        .route(
            "/internal/v1/provider-accounts",
            get(list_provider_accounts).post(import_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/enable",
            post(enable_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/disable",
            post(disable_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/drain",
            post(drain_provider_account),
        )
        .route(
            "/internal/v1/route-groups",
            get(list_route_groups).post(create_route_group),
        )
        .route(
            "/internal/v1/route-groups/{id}/bindings",
            post(bind_route_group),
        )
        .route(
            "/internal/v1/tenants",
            get(list_tenants).post(create_tenant),
        )
        .route(
            "/internal/v1/runtime/provider-accounts",
            get(list_provider_accounts),
        )
        .route("/internal/v1/audit/events", get(list_audit_events))
        .with_state(state)
}

pub async fn run(addr: SocketAddr, state: ControlPlaneState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("control-plane listening on {addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

async fn import_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(envelope): Json<ProviderAccountEnvelope>,
) -> Response {
    let principal = match authorize(
        &state,
        &headers,
        Permission::ImportProviderAccounts,
        ScopeTarget::ProviderPool(envelope.provider.clone()),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let Some(provider) = state.registry.get(&envelope.provider) else {
        return control_error(StatusCode::BAD_REQUEST, "Unknown provider");
    };

    let validated = match provider.validate_credentials(&envelope).await {
        Ok(validated) => validated,
        Err(error) => return control_error(StatusCode::BAD_REQUEST, &error.message),
    };
    let capabilities = match provider.probe_capabilities(&validated).await {
        Ok(capabilities) => capabilities,
        Err(error) => return control_error(StatusCode::BAD_REQUEST, &error.message),
    };

    let record = state
        .store
        .ingest_provider_account(envelope.clone(), validated, capabilities)
        .await;
    state
        .store
        .record_audit(
            principal.subject,
            "provider_account.imported",
            format!("provider_account:{}", record.id),
            Uuid::new_v4().to_string(),
            json!({ "provider": envelope.provider }),
        )
        .await;

    Json(json!(record)).into_response()
}

async fn list_provider_accounts(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if authorize(
        &state,
        &headers,
        Permission::ViewRuntime,
        ScopeTarget::Global,
    )
    .await
    .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    Json(json!({
      "data": state.store.list_provider_accounts().await
    }))
    .into_response()
}

async fn enable_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    mutate_provider_state(state, headers, id, AccountState::Active).await
}

async fn disable_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    mutate_provider_state(state, headers, id, AccountState::Disabled).await
}

async fn drain_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    mutate_provider_state(state, headers, id, AccountState::Draining).await
}

async fn mutate_provider_state(
    state: ControlPlaneState,
    headers: HeaderMap,
    id: Uuid,
    new_state: AccountState,
) -> Response {
    let principal = match authorize(
        &state,
        &headers,
        Permission::ManageProviderState,
        ScopeTarget::Global,
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state
        .store
        .set_provider_account_state(id, new_state.clone())
        .await
    {
        Some(record) => {
            state
                .store
                .record_audit(
                    principal.subject,
                    "provider_account.state_changed",
                    format!("provider_account:{id}"),
                    Uuid::new_v4().to_string(),
                    json!({ "state": format!("{new_state:?}") }),
                )
                .await;
            Json(json!(record)).into_response()
        }
        None => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
    }
}

async fn create_route_group(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<CreateRouteGroupRequest>,
) -> Response {
    let principal = match authorize(
        &state,
        &headers,
        Permission::ManageRouteGroups,
        ScopeTarget::Global,
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let record = state
        .store
        .create_route_group(
            payload.public_model,
            payload.provider_kind,
            payload.upstream_model,
        )
        .await;
    state
        .store
        .record_audit(
            principal.subject,
            "route_group.created",
            format!("route_group:{}", record.id),
            Uuid::new_v4().to_string(),
            json!({ "public_model": record.public_model }),
        )
        .await;
    Json(json!(record)).into_response()
}

async fn list_route_groups(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if authorize(
        &state,
        &headers,
        Permission::ViewRuntime,
        ScopeTarget::Global,
    )
    .await
    .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    Json(json!({
      "data": state.store.list_route_groups().await
    }))
    .into_response()
}

async fn bind_route_group(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(route_group_id): Path<Uuid>,
    Json(payload): Json<CreateBindingRequest>,
) -> Response {
    let principal = match authorize(
        &state,
        &headers,
        Permission::ManageBindings,
        ScopeTarget::RouteGroup(route_group_id),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state
        .store
        .bind_provider_account(
            route_group_id,
            payload.provider_account_id,
            payload.weight,
            payload.max_in_flight,
        )
        .await
    {
        Ok(binding) => {
            state
                .store
                .record_audit(
                    principal.subject,
                    "route_group.binding_created",
                    format!("route_group:{route_group_id}"),
                    Uuid::new_v4().to_string(),
                    json!({ "provider_account_id": payload.provider_account_id }),
                )
                .await;
            Json(json!(binding)).into_response()
        }
        Err(_) => control_error(StatusCode::BAD_REQUEST, "Failed to bind provider account"),
    }
}

async fn list_tenants(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if authorize(
        &state,
        &headers,
        Permission::ManageTenants,
        ScopeTarget::Global,
    )
    .await
    .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    Json(json!({
      "data": state.store.list_tenants().await
    }))
    .into_response()
}

async fn create_tenant(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<CreateTenantRequest>,
) -> Response {
    let principal = match authorize(
        &state,
        &headers,
        Permission::ManageTenants,
        ScopeTarget::Global,
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let tenant = state.store.create_tenant(payload.slug, payload.name).await;
    state
        .store
        .record_audit(
            principal.subject,
            "tenant.created",
            format!("tenant:{}", tenant.id),
            Uuid::new_v4().to_string(),
            json!({ "slug": tenant.slug }),
        )
        .await;
    Json(json!(tenant)).into_response()
}

async fn list_audit_events(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if authorize(&state, &headers, Permission::ViewAudit, ScopeTarget::Global)
        .await
        .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    Json(json!({
      "data": state.store.list_audit_events().await
    }))
    .into_response()
}

async fn authorize(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    permission: Permission,
    target: ScopeTarget,
) -> Result<ServiceAccountPrincipal, Response> {
    let Some(token) = parse_bearer_token(headers) else {
        return Err(control_error(
            StatusCode::UNAUTHORIZED,
            "Missing control-plane token",
        ));
    };

    state
        .store
        .authorize_control(&token, permission, target)
        .await
        .map_err(|error| match error {
            AuthError::Unauthorized => control_error(StatusCode::UNAUTHORIZED, "Unauthorized"),
            AuthError::Forbidden => control_error(StatusCode::FORBIDDEN, "Forbidden"),
        })
}

fn parse_bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(ToString::to_string)
}

fn control_error(status: StatusCode, message: &str) -> Response {
    Json(json!({ "error": { "message": message } }))
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
struct CreateRouteGroupRequest {
    public_model: String,
    provider_kind: String,
    upstream_model: String,
}

#[derive(Debug, Deserialize)]
struct CreateBindingRequest {
    provider_account_id: Uuid,
    weight: u32,
    max_in_flight: u32,
}

#[derive(Debug, Deserialize)]
struct CreateTenantRequest {
    slug: String,
    name: String,
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
    async fn platform_admin_can_import_provider_account() {
        let app = app(ControlPlaneState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{
              "provider":"openai_codex",
              "credential_kind":"oauth_tokens",
              "payload_version":"v1",
              "credentials":{"access_token":"token","account_id":"acct_123"},
              "metadata":{"email":"demo@example.com","plan_type":"plus"},
              "labels":["shared"],
              "tags":{"region":"global"}
            }"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("openai_codex"));
    }
}
