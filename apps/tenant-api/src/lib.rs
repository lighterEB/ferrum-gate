use anyhow::Result;
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use storage::{AuthError, PlatformStore, TenantManagementPrincipal};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
pub struct TenantApiState {
    pub store: PlatformStore,
}

impl TenantApiState {
    #[must_use]
    pub fn demo() -> Self {
        Self {
            store: PlatformStore::demo(),
        }
    }
}

pub fn app(state: TenantApiState) -> Router {
    let router = Router::new()
        .route("/tenant/v1/me", get(me))
        .route("/tenant/v1/models", get(models))
        .route(
            "/tenant/v1/api-keys",
            get(list_api_keys).post(create_api_key),
        )
        .route("/tenant/v1/api-keys/{id}/rotate", post(rotate_api_key))
        .route("/tenant/v1/api-keys/{id}/revoke", post(revoke_api_key))
        .route("/tenant/v1/usage", get(usage))
        .route("/tenant/v1/requests", get(requests))
        .route("/tenant/v1/limits", get(limits))
        .with_state(state);

    if let Some(layer) = tenant_api_cors_layer() {
        router.layer(layer)
    } else {
        router
    }
}

pub async fn run(addr: SocketAddr, state: TenantApiState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("tenant-api listening on {addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

async fn me(State(state): State<TenantApiState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let tenant = match state.store.list_tenants().await {
        Ok(tenants) => tenants
            .into_iter()
            .find(|tenant| tenant.id == principal.tenant_id),
        Err(error) => return tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };

    match tenant {
        Some(tenant) => Json(json!(tenant)).into_response(),
        None => tenant_error(StatusCode::NOT_FOUND, "Tenant not found"),
    }
}

async fn models(State(state): State<TenantApiState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state.store.list_tenant_models(principal.tenant_id).await {
        Ok(models) => Json(json!({ "data": models })).into_response(),
        Err(error) => tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn list_api_keys(State(state): State<TenantApiState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    match state.store.list_tenant_api_keys(principal.tenant_id).await {
        Ok(keys) => Json(json!({ "data": keys })).into_response(),
        Err(error) => tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn create_api_key(
    State(state): State<TenantApiState>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state
        .store
        .create_tenant_api_key(principal.tenant_id, payload.label)
        .await
    {
        Ok(created) => Json(json!(created)).into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn rotate_api_key(
    State(state): State<TenantApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state
        .store
        .rotate_tenant_api_key(principal.tenant_id, id)
        .await
    {
        Ok(created) => Json(json!(created)).into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn revoke_api_key(
    State(state): State<TenantApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state
        .store
        .revoke_tenant_api_key(principal.tenant_id, id)
        .await
    {
        Ok(record) => Json(json!(record)).into_response(),
        Err(error) => auth_error_response(error),
    }
}

async fn usage(State(state): State<TenantApiState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state.store.usage_summary(principal.tenant_id).await {
        Ok(summary) => Json(json!(summary)).into_response(),
        Err(error) => tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn requests(State(state): State<TenantApiState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match state.store.tenant_requests(principal.tenant_id).await {
        Ok(requests) => Json(json!({ "data": requests })).into_response(),
        Err(error) => tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn limits() -> Json<Value> {
    Json(json!({
      "requests_per_minute": 60,
      "burst": 10,
      "concurrent_requests": 8
    }))
}

async fn authenticate(
    state: &TenantApiState,
    headers: &HeaderMap,
) -> Result<TenantManagementPrincipal, Response> {
    let Some(token) = parse_bearer_token(headers) else {
        return Err(tenant_error(
            StatusCode::UNAUTHORIZED,
            "Missing tenant management token",
        ));
    };
    state
        .store
        .authenticate_tenant_management_token(&token)
        .await
        .map_err(|error| tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
        .ok_or_else(|| tenant_error(StatusCode::UNAUTHORIZED, "Invalid tenant management token"))
}

fn parse_bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(http::header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(ToString::to_string)
}

fn tenant_error(status: StatusCode, message: &str) -> Response {
    Json(json!({ "error": { "message": message } }))
        .into_response()
        .with_status(status)
}

fn auth_error_response(error: AuthError) -> Response {
    match error {
        AuthError::Unauthorized => tenant_error(StatusCode::NOT_FOUND, "Resource not found"),
        AuthError::Forbidden => tenant_error(StatusCode::FORBIDDEN, "Forbidden"),
        AuthError::Storage(message) => tenant_error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    }
}

fn tenant_api_cors_layer() -> Option<CorsLayer> {
    let allowed_origins = std::env::var("FERRUMGATE_TENANT_API_ALLOWED_ORIGINS").ok()?;
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
            .allow_headers([http::header::AUTHORIZATION, http::header::CONTENT_TYPE]),
    )
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
struct CreateApiKeyRequest {
    label: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use std::sync::OnceLock;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[tokio::test]
    async fn tenant_admin_can_create_api_key() {
        let app = app(TenantApiState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tenant/v1/api-keys")
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"label":"sdk"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("fgk_"));
    }

    #[tokio::test]
    async fn tenant_admin_can_read_tenant_profile() {
        let app = app(TenantApiState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tenant/v1/me")
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("demo-tenant"));
    }

    #[tokio::test]
    async fn tenant_admin_can_list_rotate_and_revoke_api_keys() {
        let app = app(TenantApiState::demo());

        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tenant/v1/api-keys")
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"label":"sdk"}"#))
                    .expect("create request"),
            )
            .await
            .expect("create response");
        assert_eq!(created.status(), StatusCode::OK);
        let created_body = to_bytes(created.into_body(), usize::MAX)
            .await
            .expect("create body");
        let created_json: Value = serde_json::from_slice(&created_body).expect("create json");
        let api_key_id = created_json["record"]["id"]
            .as_str()
            .expect("api key id")
            .to_string();

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/tenant/v1/api-keys")
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .body(Body::empty())
                    .expect("list request"),
            )
            .await
            .expect("list response");
        assert_eq!(listed.status(), StatusCode::OK);
        let listed_body = to_bytes(listed.into_body(), usize::MAX)
            .await
            .expect("list body");
        assert!(String::from_utf8_lossy(&listed_body).contains("sdk"));

        let rotated = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tenant/v1/api-keys/{api_key_id}/rotate"))
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .body(Body::empty())
                    .expect("rotate request"),
            )
            .await
            .expect("rotate response");
        assert_eq!(rotated.status(), StatusCode::OK);
        let rotated_body = to_bytes(rotated.into_body(), usize::MAX)
            .await
            .expect("rotate body");
        assert!(String::from_utf8_lossy(&rotated_body).contains("fgk_"));

        let revoked = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/tenant/v1/api-keys/{api_key_id}/revoke"))
                    .header(http::header::AUTHORIZATION, "Bearer fg_tenant_admin_demo")
                    .body(Body::empty())
                    .expect("revoke request"),
            )
            .await
            .expect("revoke response");
        assert_eq!(revoked.status(), StatusCode::OK);
        let revoked_body = to_bytes(revoked.into_body(), usize::MAX)
            .await
            .expect("revoke body");
        assert!(String::from_utf8_lossy(&revoked_body).contains("revoked"));
    }

    #[tokio::test]
    async fn cors_preflight_allows_explicit_origin() {
        let _guard = env_lock().lock().await;
        unsafe {
            std::env::set_var(
                "FERRUMGATE_TENANT_API_ALLOWED_ORIGINS",
                "http://127.0.0.1:5173",
            );
        }

        let app = app(TenantApiState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/tenant/v1/me")
                    .header(header::ORIGIN, "http://127.0.0.1:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("http://127.0.0.1:5173")
        );

        unsafe {
            std::env::remove_var("FERRUMGATE_TENANT_API_ALLOWED_ORIGINS");
        }
    }

    #[tokio::test]
    async fn cors_preflight_does_not_allow_unconfigured_origin() {
        let _guard = env_lock().lock().await;
        unsafe {
            std::env::set_var(
                "FERRUMGATE_TENANT_API_ALLOWED_ORIGINS",
                "http://127.0.0.1:5173",
            );
        }

        let app = app(TenantApiState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/tenant/v1/me")
                    .header(header::ORIGIN, "http://127.0.0.1:4173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("http://127.0.0.1:4173")
        );

        unsafe {
            std::env::remove_var("FERRUMGATE_TENANT_API_ALLOWED_ORIGINS");
        }
    }
}
