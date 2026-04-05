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
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use storage::{AuthError, Permission, PlatformStore, ScopeTarget, ServiceAccountPrincipal};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
pub struct ControlPlaneState {
    pub store: PlatformStore,
    pub registry: ProviderRegistry,
}

impl ControlPlaneState {
    #[must_use]
    pub fn demo() -> Self {
        let store = PlatformStore::demo();
        let mut registry = ProviderRegistry::new();
        registry.register(OpenAiCodexProvider::shared(Arc::new(store.clone())));

        Self { store, registry }
    }
}

pub fn app(state: ControlPlaneState) -> Router {
    Router::new()
        .route(
            "/internal/v1/provider-accounts",
            get(list_provider_accounts).post(import_provider_account),
        )
        .route(
            "/external/v1/provider-accounts/upload",
            post(upload_provider_account),
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
    import_provider_account_via(
        &state,
        &headers,
        envelope,
        "provider_account.imported",
        "internal_control_plane",
    )
    .await
}

async fn upload_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(envelope): Json<ProviderAccountEnvelope>,
) -> Response {
    import_provider_account_via(
        &state,
        &headers,
        envelope,
        "provider_account.uploaded",
        "external_upload_api",
    )
    .await
}

async fn import_provider_account_via(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    envelope: ProviderAccountEnvelope,
    audit_action: &str,
    source: &str,
) -> Response {
    let principal = match authorize(
        state,
        headers,
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
    let capabilities = match provider.probe_capabilities(&envelope, &validated).await {
        Ok(capabilities) => capabilities,
        Err(error) => return control_error(StatusCode::BAD_REQUEST, &error.message),
    };

    let record = match state
        .store
        .ingest_provider_account(envelope.clone(), validated, capabilities)
        .await
    {
        Ok(record) => record,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    state
        .store
        .record_audit(
            principal.subject,
            audit_action,
            format!("provider_account:{}", record.id),
            Uuid::new_v4().to_string(),
            json!({
                "provider": envelope.provider,
                "credential_kind": envelope.credential_kind,
                "source": source,
            }),
        )
        .await
        .ok();

    Json(json!(ProviderAccountUploadResponse {
        status: "imported",
        source: source.to_string(),
        provider_account: record,
    }))
    .into_response()
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

    match state.store.list_provider_accounts().await {
        Ok(accounts) => Json(json!({ "data": accounts })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
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
        Ok(Some(record)) => {
            state
                .store
                .record_audit(
                    principal.subject,
                    "provider_account.state_changed",
                    format!("provider_account:{id}"),
                    Uuid::new_v4().to_string(),
                    json!({ "state": format!("{new_state:?}") }),
                )
                .await
                .ok();
            Json(json!(record)).into_response()
        }
        Ok(None) => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
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

    if state.registry.get(&payload.provider_kind).is_none() {
        return control_error(StatusCode::BAD_REQUEST, "Provider adapter not registered");
    }

    let record = match state
        .store
        .create_route_group(
            payload.public_model,
            payload.provider_kind,
            payload.upstream_model,
        )
        .await
    {
        Ok(record) => record,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    state
        .store
        .record_audit(
            principal.subject,
            "route_group.created",
            format!("route_group:{}", record.id),
            Uuid::new_v4().to_string(),
            json!({ "public_model": record.public_model }),
        )
        .await
        .ok();
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

    match state.store.list_route_groups().await {
        Ok(route_groups) => Json(json!({ "data": route_groups })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
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

    let route_groups = match state.store.list_route_groups().await {
        Ok(route_groups) => route_groups,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let provider_accounts = match state.store.list_provider_accounts().await {
        Ok(provider_accounts) => provider_accounts,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let route_group_provider = route_groups
        .iter()
        .find(|route_group| route_group.id == route_group_id)
        .map(|route_group| route_group.provider_kind.as_str());
    let provider_account_provider = provider_accounts
        .iter()
        .find(|account| account.id == payload.provider_account_id)
        .map(|account| account.provider.as_str());
    if let (Some(route_group_provider), Some(provider_account_provider)) =
        (route_group_provider, provider_account_provider)
        && route_group_provider != provider_account_provider
    {
        return control_error(
            StatusCode::BAD_REQUEST,
            "Route group provider does not match provider account",
        );
    }

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
                .await
                .ok();
            Json(json!(binding)).into_response()
        }
        Err(AuthError::Unauthorized) => {
            control_error(StatusCode::NOT_FOUND, "Failed to bind provider account")
        }
        Err(AuthError::Forbidden) => control_error(StatusCode::FORBIDDEN, "Forbidden"),
        Err(AuthError::Storage(message)) => {
            control_error(StatusCode::INTERNAL_SERVER_ERROR, &message)
        }
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

    match state.store.list_tenants().await {
        Ok(tenants) => Json(json!({ "data": tenants })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
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

    let tenant = match state.store.create_tenant(payload.slug, payload.name).await {
        Ok(tenant) => tenant,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    state
        .store
        .record_audit(
            principal.subject,
            "tenant.created",
            format!("tenant:{}", tenant.id),
            Uuid::new_v4().to_string(),
            json!({ "slug": tenant.slug }),
        )
        .await
        .ok();
    Json(json!(tenant)).into_response()
}

async fn list_audit_events(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if authorize(&state, &headers, Permission::ViewAudit, ScopeTarget::Global)
        .await
        .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    match state.store.list_audit_events().await {
        Ok(events) => Json(json!({ "data": events })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
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
            AuthError::Storage(message) => {
                control_error(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
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

#[derive(Debug, Serialize)]
struct ProviderAccountUploadResponse {
    status: &'static str,
    source: String,
    provider_account: storage::ProviderAccountRecord,
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
        routing::get,
    };
    use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};
    use serde_json::json;
    use std::net::SocketAddr;
    use tower::util::ServiceExt;

    async fn spawn_models_server() -> SocketAddr {
        async fn models_handler(request: AxumRequest) -> impl IntoResponse {
            let auth = request
                .headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            assert_eq!(auth, "Bearer token");

            axum::Json(json!({
                "object": "list",
                "data": [
                    { "id": "gpt-5-codex" },
                    { "id": "gpt-5-codex-mini" }
                ]
            }))
            .into_response()
        }

        let app = Router::new().route("/v1/models", get(models_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    #[tokio::test]
    async fn platform_admin_can_import_provider_account() {
        let addr = spawn_models_server().await;
        let app = app(ControlPlaneState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "provider": "openai_codex",
                            "credential_kind": "oauth_tokens",
                            "payload_version": "v1",
                            "credentials": {
                                "access_token": "token",
                                "account_id": "acct_123",
                                "api_base": format!("http://{addr}/v1")
                            },
                            "metadata": {
                                "email": "demo@example.com",
                                "plan_type": "plus"
                            },
                            "labels": ["shared"],
                            "tags": { "region": "global" }
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
        assert!(body.contains("openai_codex"));
        assert!(body.contains("gpt-5-codex"));
    }

    #[tokio::test]
    async fn external_upload_endpoint_imports_provider_account() {
        let addr = spawn_models_server().await;
        let app = app(ControlPlaneState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/external/v1/provider-accounts/upload")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "provider": "openai_codex",
                            "credential_kind": "oauth_tokens",
                            "payload_version": "v1",
                            "credentials": {
                                "access_token": "token",
                                "account_id": "acct_external_123",
                                "api_base": format!("http://{addr}/v1")
                            },
                            "metadata": {
                                "email": "external@example.com",
                                "plan_type": "plus"
                            },
                            "labels": ["shared"],
                            "tags": { "region": "global" }
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
        assert!(body.contains("\"status\":\"imported\""));
        assert!(body.contains("\"source\":\"external_upload_api\""));
        assert!(body.contains("acct_external_123"));
        assert!(body.contains("gpt-5-codex-mini"));
    }

    #[tokio::test]
    async fn create_route_group_rejects_unregistered_provider_kind() {
        let app = app(ControlPlaneState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/route-groups")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "public_model": "qwen-max",
                            "provider_kind": "qwen",
                            "upstream_model": "qwen-max"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("Provider adapter not registered"));
    }

    #[tokio::test]
    async fn bind_route_group_rejects_provider_mismatch() {
        let state = ControlPlaneState::demo();
        let route_group = state
            .store
            .create_route_group(
                "gpt-5-codex".to_string(),
                "openai_codex".to_string(),
                "gpt-5-codex".to_string(),
            )
            .await
            .expect("route group");
        let mismatched_account = state
            .store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "qwen".to_string(),
                    credential_kind: "api_key".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "api_key": "test-key"
                    }),
                    metadata: json!({}),
                    labels: vec![],
                    tags: Default::default(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_qwen".to_string(),
                    redacted_display: None,
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![],
                    supports_refresh: false,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("provider account");
        let app = app(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/route-groups/{}/bindings",
                        route_group.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "provider_account_id": mismatched_account.id,
                            "weight": 100,
                            "max_in_flight": 16
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("Route group provider does not match provider account"));
    }
}
