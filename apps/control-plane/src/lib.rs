use anyhow::Result;
use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use provider_core::{ProviderAccountEnvelope, ProviderError, ProviderErrorKind, ProviderRegistry};
use provider_openai_codex::OpenAiCodexProvider;
use reqwest::Client;
use scheduler::{AccountState, ProviderOutcome};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashSet, net::SocketAddr, sync::Arc};
use storage::{AuthError, Permission, PlatformStore, ScopeTarget, ServiceAccountPrincipal};
use tower_http::cors::CorsLayer;
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
    let router = Router::new()
        .route(
            "/internal/v1/provider-accounts",
            get(list_provider_accounts).post(import_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/probe",
            post(batch_probe_provider_accounts),
        )
        .route(
            "/internal/v1/provider-accounts/probe/dispatch",
            post(dispatch_provider_account_probes),
        )
        .route(
            "/internal/v1/provider-accounts/refresh/dispatch",
            post(dispatch_provider_account_refreshes),
        )
        .route(
            "/internal/v1/provider-accounts/refresh/run",
            post(run_due_provider_account_refreshes),
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
            "/internal/v1/provider-accounts/{id}/probe",
            post(probe_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/quota",
            get(get_provider_account_quota),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/quota/probe",
            post(probe_provider_account_quota),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/refresh",
            post(refresh_provider_account),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/inspections",
            get(list_provider_account_inspections),
        )
        .route(
            "/internal/v1/provider-accounts/{id}/drain",
            post(drain_provider_account),
        )
        .route(
            "/internal/v1/route-groups",
            get(list_route_groups).post(create_route_group),
        )
        .route("/internal/v1/routing/overview", get(routing_overview))
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
        .route(
            "/internal/v1/alerts/outbox",
            get(list_alerts_outbox).post(deliver_alerts_outbox),
        )
        .with_state(state);

    if let Some(cors) = console_cors_layer() {
        router.layer(cors)
    } else {
        router
    }
}

pub async fn run(addr: SocketAddr, state: ControlPlaneState) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("control-plane listening on {addr}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}

fn console_cors_layer() -> Option<CorsLayer> {
    http_utils::console_cors_layer_from_env()
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
        Ok(accounts) => {
            let mut views = Vec::with_capacity(accounts.len());
            for account in accounts {
                let quota = state
                    .store
                    .provider_account_quota_snapshot(account.id)
                    .await
                    .ok()
                    .flatten();
                views.push(ProviderAccountRuntimeView { account, quota });
            }
            Json(json!({ "data": views })).into_response()
        }
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

async fn probe_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
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

    match execute_provider_account_probe(&state, &principal.subject, id).await {
        Ok(Some(result)) => Json(json!(result)).into_response(),
        Ok(None) => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
        Err(response) => response,
    }
}

async fn list_provider_account_inspections(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
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

    match state.store.list_account_inspections(id).await {
        Ok(records) => Json(json!({ "data": records })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn batch_probe_provider_accounts(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<BatchProbeRequest>,
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

    let mut results = Vec::with_capacity(payload.account_ids.len());
    let mut healthy = 0_u64;
    let mut unhealthy = 0_u64;

    for account_id in payload.account_ids {
        let result =
            match execute_provider_account_probe(&state, &principal.subject, account_id).await {
                Ok(Some(result)) => result,
                Ok(None) => ProbeProviderAccountResult {
                    account_id,
                    status: "not_found".to_string(),
                    provider_account: None,
                    error: None,
                },
                Err(response) => return response,
            };

        if result.status == "healthy" {
            healthy += 1;
        } else {
            unhealthy += 1;
        }
        results.push(result);
    }

    Json(json!({
        "total": results.len(),
        "healthy": healthy,
        "unhealthy": unhealthy,
        "results": results,
    }))
    .into_response()
}

async fn dispatch_provider_account_probes(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<DispatchProbeRequest>,
) -> Response {
    if authorize(
        &state,
        &headers,
        Permission::ManageProviderState,
        ScopeTarget::Global,
    )
    .await
    .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    match state
        .store
        .dispatch_due_provider_account_probes(payload.limit.max(1))
        .await
    {
        Ok(leases) => Json(json!({ "data": leases })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn dispatch_provider_account_refreshes(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<DispatchRefreshRequest>,
) -> Response {
    if authorize(
        &state,
        &headers,
        Permission::ManageProviderState,
        ScopeTarget::Global,
    )
    .await
    .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    match state
        .store
        .dispatch_due_provider_account_refreshes(
            payload.limit.max(1),
            payload
                .refresh_before_seconds
                .unwrap_or(DEFAULT_REFRESH_BEFORE_SECONDS)
                .max(0),
        )
        .await
    {
        Ok(leases) => Json(json!({ "data": leases })).into_response(),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn run_due_provider_account_refreshes(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<DispatchRefreshRequest>,
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

    let leases = match state
        .store
        .dispatch_due_provider_account_refreshes(
            payload.limit.max(1),
            payload
                .refresh_before_seconds
                .unwrap_or(DEFAULT_REFRESH_BEFORE_SECONDS)
                .max(0),
        )
        .await
    {
        Ok(leases) => leases,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };

    let mut refreshed = 0_u64;
    let mut failed = 0_u64;
    let mut results = Vec::with_capacity(leases.len());

    for lease in leases {
        match execute_provider_account_refresh(&state, &principal.subject, lease.account_id).await {
            Ok(Some(result)) => {
                if result.status == "healthy" {
                    refreshed += 1;
                } else {
                    failed += 1;
                }
                results.push(result);
            }
            Ok(None) => {
                failed += 1;
                results.push(ProbeProviderAccountResult {
                    account_id: lease.account_id,
                    status: "not_found".to_string(),
                    provider_account: None,
                    error: None,
                });
            }
            Err(response) => {
                failed += 1;
                results.push(refresh_result_from_response(lease.account_id, response).await);
            }
        }
    }

    Json(json!({
        "total": results.len(),
        "refreshed": refreshed,
        "failed": failed,
        "results": results,
    }))
    .into_response()
}

async fn drain_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    mutate_provider_state(state, headers, id, AccountState::Draining).await
}

async fn probe_provider_account_quota(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
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

    match execute_provider_account_quota_probe(&state, &principal.subject, id).await {
        Ok(Some(result)) => Json(json!(result)).into_response(),
        Ok(None) => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
        Err(response) => response,
    }
}

async fn get_provider_account_quota(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
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

    match state.store.provider_account(id).await {
        Ok(Some(_)) => match state.store.provider_account_quota_snapshot(id).await {
            Ok(quota) => Json(json!(ProviderAccountQuotaResponse {
                account_id: id,
                quota,
            }))
            .into_response(),
            Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        },
        Ok(None) => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
        Err(error) => control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn refresh_provider_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
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

    match execute_provider_account_refresh(&state, &principal.subject, id).await {
        Ok(Some(result)) => Json(json!(result)).into_response(),
        Ok(None) => control_error(StatusCode::NOT_FOUND, "Provider account not found"),
        Err(response) => response,
    }
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

async fn routing_overview(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
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

    let route_groups = match state.store.list_route_groups().await {
        Ok(route_groups) => route_groups,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let bindings = match state.store.list_route_group_bindings().await {
        Ok(bindings) => bindings,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };

    let binding_counts = bindings.into_iter().fold(
        std::collections::HashMap::<Uuid, usize>::new(),
        |mut counts, binding| {
            *counts.entry(binding.route_group_id).or_insert(0) += 1;
            counts
        },
    );
    let route_groups = route_groups
        .into_iter()
        .map(|route_group| RoutingOverviewItem {
            binding_count: binding_counts
                .get(&route_group.id)
                .copied()
                .unwrap_or_default(),
            route_group,
        })
        .collect::<Vec<_>>();

    Json(json!(RoutingOverviewResponse {
        route_groups,
        bindings_count: binding_counts.values().sum(),
        auto_derived: true,
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

async fn list_alerts_outbox(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AlertsOutboxQuery>,
) -> Response {
    if authorize(&state, &headers, Permission::ViewAudit, ScopeTarget::Global)
        .await
        .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    match collect_alerts_outbox(&state, query.resource.as_deref()).await {
        Ok(alerts) => Json(json!({ "data": alerts })).into_response(),
        Err(response) => response,
    }
}

async fn deliver_alerts_outbox(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<DeliverAlertsRequest>,
) -> Response {
    if authorize(&state, &headers, Permission::ViewAudit, ScopeTarget::Global)
        .await
        .is_err()
    {
        return control_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let alerts = match collect_alerts_outbox(&state, payload.resource.as_deref()).await {
        Ok(alerts) => alerts,
        Err(response) => return response,
    };
    let receipts = match state
        .store
        .list_alert_delivery_receipts(&payload.webhook_url)
        .await
    {
        Ok(receipts) => receipts,
        Err(error) => return control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    let delivered_ids = receipts
        .into_iter()
        .map(|receipt| receipt.alert_id)
        .collect::<HashSet<_>>();
    let client = Client::new();

    let mut delivered = 0_u64;
    let mut skipped = 0_u64;
    let mut failed = 0_u64;
    let mut results = Vec::new();

    for alert in alerts.into_iter().take(payload.limit.max(1)) {
        if delivered_ids.contains(&alert.id) {
            skipped += 1;
            results.push(AlertDeliveryResult {
                alert_id: alert.id,
                status: "skipped".to_string(),
                response_status: None,
                error: None,
            });
            continue;
        }

        match client.post(&payload.webhook_url).json(&alert).send().await {
            Ok(response) if response.status().is_success() => {
                match state
                    .store
                    .record_alert_delivery(alert.id, payload.webhook_url.clone())
                    .await
                {
                    Ok(true) => {
                        delivered += 1;
                        results.push(AlertDeliveryResult {
                            alert_id: alert.id,
                            status: "delivered".to_string(),
                            response_status: Some(response.status().as_u16()),
                            error: None,
                        });
                    }
                    Ok(false) => {
                        skipped += 1;
                        results.push(AlertDeliveryResult {
                            alert_id: alert.id,
                            status: "skipped".to_string(),
                            response_status: Some(response.status().as_u16()),
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed += 1;
                        results.push(AlertDeliveryResult {
                            alert_id: alert.id,
                            status: "failed".to_string(),
                            response_status: Some(response.status().as_u16()),
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Ok(response) => {
                failed += 1;
                results.push(AlertDeliveryResult {
                    alert_id: alert.id,
                    status: "failed".to_string(),
                    response_status: Some(response.status().as_u16()),
                    error: Some(format!("webhook returned {}", response.status())),
                });
            }
            Err(error) => {
                failed += 1;
                results.push(AlertDeliveryResult {
                    alert_id: alert.id,
                    status: "failed".to_string(),
                    response_status: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Json(json!({
        "total": results.len(),
        "delivered": delivered,
        "skipped": skipped,
        "failed": failed,
        "results": results,
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

async fn probe_provider_account_failure(
    state: &ControlPlaneState,
    principal_subject: &str,
    id: Uuid,
    error: &ProviderError,
) -> ProbeProviderAccountResult {
    state
        .store
        .mark_scheduler_outcome(id, provider_outcome_for_error(error))
        .await
        .ok();
    state
        .store
        .record_account_inspection(
            id,
            principal_subject,
            storage::AccountInspectionStatus::Unhealthy,
            Some(provider_error_kind_name(&error.kind).to_string()),
            error.code.clone(),
            Some(error.message.clone()),
        )
        .await
        .ok();
    state
        .store
        .record_audit(
            principal_subject,
            "provider_account.probed",
            format!("provider_account:{id}"),
            Uuid::new_v4().to_string(),
            json!({
                "status": "unhealthy",
                "error_kind": format!("{:?}", error.kind),
                "error_message": error.message,
            }),
        )
        .await
        .ok();
    ProbeProviderAccountResult {
        account_id: id,
        status: "unhealthy".to_string(),
        provider_account: None,
        error: Some(ProbeProviderAccountError {
            message: error.message.clone(),
            kind: format!("{:?}", error.kind),
            status_code: error.status_code,
            code: error.code.clone(),
        }),
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

fn provider_error_kind_name(kind: &ProviderErrorKind) -> &'static str {
    match kind {
        ProviderErrorKind::InvalidRequest => "invalid_request",
        ProviderErrorKind::InvalidCredentials => "invalid_credentials",
        ProviderErrorKind::RateLimited => "rate_limited",
        ProviderErrorKind::UpstreamUnavailable => "upstream_unavailable",
        ProviderErrorKind::Unsupported => "unsupported",
    }
}

fn provider_error_status(error: &ProviderError) -> StatusCode {
    StatusCode::from_u16(error.status_code).unwrap_or(StatusCode::BAD_GATEWAY)
}

async fn refresh_result_from_response(
    account_id: Uuid,
    response: Response,
) -> ProbeProviderAccountResult {
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/message"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(&body).trim().to_string();
            if text.is_empty() {
                format!("refresh run failed with status {status}")
            } else {
                text
            }
        });

    ProbeProviderAccountResult {
        account_id,
        status: "failed".to_string(),
        provider_account: None,
        error: Some(ProbeProviderAccountError {
            message,
            kind: "ControlPlaneError".to_string(),
            status_code: status.as_u16(),
            code: None,
        }),
    }
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

#[derive(Debug, Deserialize)]
struct BatchProbeRequest {
    account_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
struct DispatchProbeRequest {
    limit: usize,
}

const DEFAULT_REFRESH_BEFORE_SECONDS: i64 = 30 * 60;

#[derive(Debug, Deserialize)]
struct DispatchRefreshRequest {
    limit: usize,
    #[serde(default)]
    refresh_before_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AlertsOutboxQuery {
    resource: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeliverAlertsRequest {
    webhook_url: String,
    #[serde(default = "default_alert_delivery_limit")]
    limit: usize,
    #[serde(default)]
    resource: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProbeProviderAccountResult {
    account_id: Uuid,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_account: Option<storage::ProviderAccountRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ProbeProviderAccountError>,
}

#[derive(Debug, Serialize)]
struct ProviderAccountQuotaResponse {
    account_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    quota: Option<storage::ProviderAccountQuotaSnapshotRecord>,
}

#[derive(Debug, Serialize)]
struct ProviderAccountRuntimeView {
    #[serde(flatten)]
    account: storage::ProviderAccountRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    quota: Option<storage::ProviderAccountQuotaSnapshotRecord>,
}

#[derive(Debug, Serialize)]
struct RoutingOverviewResponse {
    route_groups: Vec<RoutingOverviewItem>,
    bindings_count: usize,
    auto_derived: bool,
}

#[derive(Debug, Serialize)]
struct RoutingOverviewItem {
    #[serde(flatten)]
    route_group: storage::RouteGroupRecord,
    binding_count: usize,
}

#[derive(Debug, Serialize)]
struct ProbeProviderAccountError {
    message: String,
    kind: String,
    status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

#[derive(Debug, Serialize)]
struct AlertsOutboxItem {
    id: Uuid,
    kind: String,
    severity: String,
    resource: String,
    message: String,
    occurred_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
struct AlertDeliveryResult {
    alert_id: Uuid,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn default_alert_delivery_limit() -> usize {
    50
}

async fn execute_provider_account_probe(
    state: &ControlPlaneState,
    principal_subject: &str,
    id: Uuid,
) -> Result<Option<ProbeProviderAccountResult>, Response> {
    let envelope = match state.store.provider_account_envelope(id).await {
        Ok(Some(envelope)) => envelope,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(control_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &error.to_string(),
            ));
        }
    };

    let Some(provider) = state.registry.get(&envelope.provider) else {
        return Err(control_error(
            StatusCode::BAD_REQUEST,
            "Provider adapter not registered",
        ));
    };

    match provider.validate_credentials(&envelope).await {
        Ok(validated) => match provider.probe_capabilities(&envelope, &validated).await {
            Ok(capabilities) => match state
                .store
                .revalidate_provider_account(id, validated, capabilities)
                .await
            {
                Ok(Some(record)) => {
                    state
                        .store
                        .record_account_inspection(
                            id,
                            principal_subject,
                            storage::AccountInspectionStatus::Healthy,
                            None,
                            None,
                            None,
                        )
                        .await
                        .ok();
                    state
                        .store
                        .record_audit(
                            principal_subject,
                            "provider_account.probed",
                            format!("provider_account:{id}"),
                            Uuid::new_v4().to_string(),
                            json!({ "status": "healthy" }),
                        )
                        .await
                        .ok();
                    Ok(Some(ProbeProviderAccountResult {
                        account_id: id,
                        status: "healthy".to_string(),
                        provider_account: Some(record),
                        error: None,
                    }))
                }
                Ok(None) => Ok(None),
                Err(error) => Err(control_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &error.to_string(),
                )),
            },
            Err(error) => Ok(Some(
                probe_provider_account_failure(state, principal_subject, id, &error).await,
            )),
        },
        Err(error) => Ok(Some(
            probe_provider_account_failure(state, principal_subject, id, &error).await,
        )),
    }
}

async fn execute_provider_account_quota_probe(
    state: &ControlPlaneState,
    principal_subject: &str,
    id: Uuid,
) -> Result<Option<ProbeProviderAccountResult>, Response> {
    let envelope = match state.store.provider_account_envelope(id).await {
        Ok(Some(envelope)) => envelope,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(control_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &error.to_string(),
            ));
        }
    };

    let Some(provider) = state.registry.get(&envelope.provider) else {
        return Err(control_error(
            StatusCode::BAD_REQUEST,
            "Provider adapter not registered",
        ));
    };

    let validated = match provider.validate_credentials(&envelope).await {
        Ok(validated) => validated,
        Err(error) => {
            return Ok(Some(
                probe_provider_account_failure(state, principal_subject, id, &error).await,
            ));
        }
    };

    match provider.probe_quota(&envelope, &validated).await {
        Ok(snapshot) => {
            state
                .store
                .upsert_provider_account_quota_snapshot(id, snapshot.clone())
                .await
                .ok();

            if snapshot.remaining_requests_hint == Some(0) {
                state
                    .store
                    .mark_scheduler_outcome(id, ProviderOutcome::QuotaExhausted)
                    .await
                    .ok();
                state
                    .store
                    .record_account_inspection(
                        id,
                        principal_subject,
                        storage::AccountInspectionStatus::Unhealthy,
                        Some("quota_exhausted".to_string()),
                        None,
                        Some("provider reported no remaining requests".to_string()),
                    )
                    .await
                    .ok();
                state
                    .store
                    .record_audit(
                        principal_subject,
                        "provider_account.quota_probed",
                        format!("provider_account:{id}"),
                        Uuid::new_v4().to_string(),
                        json!({ "status": "unhealthy", "error_kind": "quota_exhausted" }),
                    )
                    .await
                    .ok();

                let record = match state.store.provider_account(id).await {
                    Ok(Some(record)) => record,
                    Ok(None) => return Ok(None),
                    Err(error) => {
                        return Err(control_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &error.to_string(),
                        ));
                    }
                };

                return Ok(Some(ProbeProviderAccountResult {
                    account_id: id,
                    status: "unhealthy".to_string(),
                    provider_account: Some(record),
                    error: Some(ProbeProviderAccountError {
                        message: "provider reported no remaining requests".to_string(),
                        kind: "quota_exhausted".to_string(),
                        status_code: 429,
                        code: None,
                    }),
                }));
            }

            state
                .store
                .set_provider_account_state(id, AccountState::Active)
                .await
                .ok();
            state
                .store
                .record_account_inspection(
                    id,
                    principal_subject,
                    storage::AccountInspectionStatus::Healthy,
                    None,
                    None,
                    None,
                )
                .await
                .ok();

            let record = match state.store.provider_account(id).await {
                Ok(Some(record)) => record,
                Ok(None) => return Ok(None),
                Err(error) => {
                    return Err(control_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &error.to_string(),
                    ));
                }
            };

            Ok(Some(ProbeProviderAccountResult {
                account_id: id,
                status: "healthy".to_string(),
                provider_account: Some(record),
                error: None,
            }))
        }
        Err(error)
            if matches!(
                error.kind,
                ProviderErrorKind::InvalidCredentials
                    | ProviderErrorKind::RateLimited
                    | ProviderErrorKind::UpstreamUnavailable
            ) =>
        {
            Ok(Some(
                probe_provider_account_failure(state, principal_subject, id, &error).await,
            ))
        }
        Err(error) => Err(control_error(provider_error_status(&error), &error.message)),
    }
}

async fn execute_provider_account_refresh(
    state: &ControlPlaneState,
    principal_subject: &str,
    id: Uuid,
) -> Result<Option<ProbeProviderAccountResult>, Response> {
    let Some(envelope) = state
        .store
        .provider_account_envelope(id)
        .await
        .map_err(|error| control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
    else {
        return Ok(None);
    };

    let Some(provider) = state.registry.get(&envelope.provider) else {
        return Err(control_error(StatusCode::BAD_REQUEST, "Unknown provider"));
    };

    let refreshed = match provider.refresh_credentials(&envelope).await {
        Ok(refreshed) => refreshed,
        Err(error)
            if matches!(
                error.kind,
                ProviderErrorKind::InvalidCredentials
                    | ProviderErrorKind::RateLimited
                    | ProviderErrorKind::UpstreamUnavailable
            ) =>
        {
            return Ok(Some(
                probe_provider_account_failure(state, principal_subject, id, &error).await,
            ));
        }
        Err(error) => return Err(control_error(provider_error_status(&error), &error.message)),
    };

    let record = state
        .store
        .rotate_provider_account_secret(id, refreshed.credentials, refreshed.expires_at)
        .await
        .map_err(|error| control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;

    let Some(record) = record else {
        return Ok(None);
    };

    state
        .store
        .record_account_inspection(
            id,
            principal_subject,
            storage::AccountInspectionStatus::Healthy,
            None,
            None,
            None,
        )
        .await
        .ok();
    state
        .store
        .record_audit(
            principal_subject,
            "provider_account.refreshed",
            format!("provider_account:{id}"),
            Uuid::new_v4().to_string(),
            json!({ "status": "healthy" }),
        )
        .await
        .ok();

    Ok(Some(ProbeProviderAccountResult {
        account_id: id,
        status: "healthy".to_string(),
        provider_account: Some(record),
        error: None,
    }))
}

fn parse_provider_account_resource(resource: &str) -> Option<Uuid> {
    resource
        .strip_prefix("provider_account:")
        .and_then(|value| Uuid::parse_str(value).ok())
}

async fn collect_alerts_outbox(
    state: &ControlPlaneState,
    resource: Option<&str>,
) -> Result<Vec<AlertsOutboxItem>, Response> {
    let resources = match resource {
        Some(resource) => vec![resource.to_string()],
        None => state
            .store
            .list_provider_accounts()
            .await
            .map_err(|error| control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?
            .into_iter()
            .map(|account| format!("provider_account:{}", account.id))
            .collect(),
    };

    let mut alerts = Vec::new();
    for resource in resources {
        let Some(account_id) = parse_provider_account_resource(&resource) else {
            return Err(control_error(
                StatusCode::BAD_REQUEST,
                "Unsupported resource",
            ));
        };
        let inspections = state
            .store
            .list_account_inspections(account_id)
            .await
            .map_err(|error| {
                control_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
            })?;
        alerts.extend(
            inspections
                .into_iter()
                .filter_map(|inspection| alert_from_inspection(&resource, inspection)),
        );
    }

    alerts.sort_by(|left, right| {
        left.occurred_at
            .cmp(&right.occurred_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(alerts)
}

fn alert_from_inspection(
    resource: &str,
    inspection: storage::AccountInspectionRecord,
) -> Option<AlertsOutboxItem> {
    if inspection.status != storage::AccountInspectionStatus::Unhealthy {
        return None;
    }

    let (kind, severity) = match inspection.error_kind.as_deref() {
        Some("invalid_credentials") => ("provider_account.invalid_credentials", "critical"),
        Some("quota_exhausted") => ("provider_account.quota_exhausted", "warning"),
        Some("rate_limited") => ("provider_account.rate_limited", "warning"),
        Some("upstream_unavailable") => ("provider_account.upstream_unavailable", "warning"),
        Some(_) | None => ("provider_account.degraded", "warning"),
    };

    Some(AlertsOutboxItem {
        id: inspection.id,
        kind: kind.to_string(),
        severity: severity.to_string(),
        resource: resource.to_string(),
        message: inspection
            .error_message
            .unwrap_or_else(|| "provider account requires attention".to_string()),
        occurred_at: inspection.inspected_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::Request as AxumRequest,
        http::Request,
        response::IntoResponse,
        routing::{get, post},
    };
    use chrono::{TimeDelta, Utc};
    use protocol_core::{InferenceRequest, InferenceResponse, ModelCapability, ModelDescriptor};
    use provider_core::{
        AccountCapabilities, ProviderAccountEnvelope, ProviderAdapter, ProviderError,
        ProviderErrorKind, QuotaSnapshot, ValidatedProviderAccount,
    };
    use serde_json::json;
    use std::{
        net::SocketAddr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use tower::util::ServiceExt;

    async fn spawn_models_server() -> SocketAddr {
        spawn_models_server_with_response(
            "Bearer token".to_string(),
            StatusCode::OK,
            json!({
                "object": "list",
                "data": [
                    { "id": "gpt-5-codex" },
                    { "id": "gpt-5-codex-mini" }
                ]
            }),
        )
        .await
    }

    async fn spawn_models_server_with_response(
        expected_auth: String,
        status: StatusCode,
        body: serde_json::Value,
    ) -> SocketAddr {
        let app = Router::new().route(
            "/v1/models",
            get(move |request: AxumRequest| {
                let expected_auth = expected_auth.clone();
                let body = body.clone();
                async move {
                    let auth = request
                        .headers()
                        .get(http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();

                    assert_eq!(auth, expected_auth);

                    (status, axum::Json(body)).into_response()
                }
            }),
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

    async fn spawn_flaky_models_server(expected_auth: String) -> (SocketAddr, Arc<AtomicBool>) {
        let fail_after_upload = Arc::new(AtomicBool::new(false));
        let switch = fail_after_upload.clone();
        let app = Router::new().route(
            "/v1/models",
            get(move |request: AxumRequest| {
                let expected_auth = expected_auth.clone();
                let switch = switch.clone();
                async move {
                    let auth = request
                        .headers()
                        .get(http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();

                    assert_eq!(auth, expected_auth);

                    if switch.load(Ordering::SeqCst) {
                        return (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({
                                "error": {
                                    "message": "token invalidated",
                                    "type": "authentication_error",
                                    "code": "token_invalidated"
                                }
                            })),
                        )
                            .into_response();
                    }

                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "object": "list",
                            "data": [
                                { "id": "gpt-5-codex" },
                                { "id": "gpt-5-codex-mini" }
                            ]
                        })),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        (addr, fail_after_upload)
    }

    #[derive(Clone, Copy)]
    enum RefreshServerBehavior {
        Success,
        InvalidGrant,
    }

    async fn spawn_refresh_token_server(
        expected_refresh_token: String,
        expected_client_id: String,
        behavior: RefreshServerBehavior,
    ) -> SocketAddr {
        let app = Router::new().route(
            "/oauth/token",
            post(move |request: AxumRequest| {
                let expected_refresh_token = expected_refresh_token.clone();
                let expected_client_id = expected_client_id.clone();
                async move {
                    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                        .await
                        .expect("refresh body");
                    let form = String::from_utf8(body.to_vec()).expect("refresh body utf8");

                    assert!(form.contains("grant_type=refresh_token"));
                    assert!(form.contains(&format!("refresh_token={expected_refresh_token}")));
                    assert!(form.contains(&format!("client_id={expected_client_id}")));

                    match behavior {
                        RefreshServerBehavior::Success => (
                            StatusCode::OK,
                            axum::Json(json!({
                                "access_token": "new-access-token",
                                "refresh_token": "new-refresh-token",
                                "id_token": "new-id-token",
                                "expires_in": 3600
                            })),
                        )
                            .into_response(),
                        RefreshServerBehavior::InvalidGrant => (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": "invalid_grant",
                                "error_description": "refresh token rejected"
                            })),
                        )
                            .into_response(),
                    }
                }
            }),
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

    async fn spawn_alert_webhook_server() -> (SocketAddr, Arc<Mutex<Vec<serde_json::Value>>>) {
        let deliveries = Arc::new(Mutex::new(Vec::new()));
        let shared = deliveries.clone();
        let app = Router::new().route(
            "/webhook",
            post(move |request: AxumRequest| {
                let shared = shared.clone();
                async move {
                    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                        .await
                        .expect("webhook body");
                    let payload: serde_json::Value =
                        serde_json::from_slice(&body).expect("webhook payload");
                    shared.lock().expect("deliveries lock").push(payload);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        (addr, deliveries)
    }

    async fn seed_openai_provider_account(
        state: &ControlPlaneState,
        api_base: String,
        access_token: &str,
        account_id: &str,
    ) -> storage::ProviderAccountRecord {
        state
            .store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "access_token": access_token,
                        "account_id": account_id,
                        "api_base": api_base
                    }),
                    metadata: json!({
                        "email": "probe@example.com",
                        "plan_type": "plus"
                    }),
                    labels: vec!["shared".to_string()],
                    tags: Default::default(),
                },
                ValidatedProviderAccount {
                    provider_account_id: account_id.to_string(),
                    redacted_display: Some("p***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("provider account")
    }

    async fn provider_account(
        state: &ControlPlaneState,
        account_id: Uuid,
    ) -> storage::ProviderAccountRecord {
        state
            .store
            .list_provider_accounts()
            .await
            .expect("provider accounts")
            .into_iter()
            .find(|record| record.id == account_id)
            .expect("provider account")
    }

    #[derive(Clone, Copy)]
    enum QuotaTestBehavior {
        Healthy,
        Exhausted,
        Suspended,
    }

    struct QuotaTestProvider {
        behavior: QuotaTestBehavior,
    }

    #[async_trait]
    impl ProviderAdapter for QuotaTestProvider {
        fn kind(&self) -> &'static str {
            "quota_test"
        }

        async fn list_models(
            &self,
            _envelope: &ProviderAccountEnvelope,
        ) -> Result<Vec<ModelDescriptor>, ProviderError> {
            Ok(vec![ModelDescriptor {
                id: "quota-test-model".to_string(),
                route_group: "quota-test".to_string(),
                provider_kind: "quota_test".to_string(),
                upstream_model: "quota-test-model".to_string(),
                capabilities: vec![ModelCapability::Chat],
            }])
        }

        async fn validate_credentials(
            &self,
            envelope: &ProviderAccountEnvelope,
        ) -> Result<ValidatedProviderAccount, ProviderError> {
            Ok(ValidatedProviderAccount {
                provider_account_id: envelope
                    .credentials
                    .get("account_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("quota-account")
                    .to_string(),
                redacted_display: Some("q***@***".to_string()),
                expires_at: Some(Utc::now() + TimeDelta::days(7)),
            })
        }

        async fn probe_capabilities(
            &self,
            envelope: &ProviderAccountEnvelope,
            _account: &ValidatedProviderAccount,
        ) -> Result<AccountCapabilities, ProviderError> {
            Ok(AccountCapabilities {
                models: self.list_models(envelope).await?,
                supports_refresh: false,
                supports_quota_probe: true,
            })
        }

        async fn probe_quota(
            &self,
            _envelope: &ProviderAccountEnvelope,
            _account: &ValidatedProviderAccount,
        ) -> Result<QuotaSnapshot, ProviderError> {
            match self.behavior {
                QuotaTestBehavior::Healthy => Ok(QuotaSnapshot {
                    plan_label: Some("team".to_string()),
                    remaining_requests_hint: Some(42),
                    details: Some(json!({
                        "plan_type": "team",
                        "rate_limit": {
                            "allowed": true,
                            "limit_reached": false,
                            "primary_window": {
                                "used_percent": 12,
                                "limit_window_seconds": 604800,
                                "reset_after_seconds": 3600,
                                "reset_at": 1776064545
                            }
                        },
                        "code_review_rate_limit": {
                            "allowed": true,
                            "limit_reached": false
                        },
                        "credits": {
                            "has_credits": false,
                            "unlimited": false
                        },
                        "spend_control": {
                            "reached": false
                        }
                    })),
                    checked_at: Utc::now(),
                }),
                QuotaTestBehavior::Exhausted => Ok(QuotaSnapshot {
                    plan_label: Some("team".to_string()),
                    remaining_requests_hint: Some(0),
                    details: Some(json!({
                        "plan_type": "team",
                        "rate_limit": {
                            "allowed": false,
                            "limit_reached": true
                        }
                    })),
                    checked_at: Utc::now(),
                }),
                QuotaTestBehavior::Suspended => Err(ProviderError::new(
                    ProviderErrorKind::InvalidCredentials,
                    403,
                    "account suspended",
                )
                .with_code("account_suspended")),
            }
        }

        async fn chat(
            &self,
            _request: InferenceRequest,
        ) -> Result<InferenceResponse, ProviderError> {
            Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                501,
                "not implemented in tests",
            ))
        }

        async fn responses(
            &self,
            _request: InferenceRequest,
        ) -> Result<InferenceResponse, ProviderError> {
            Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                501,
                "not implemented in tests",
            ))
        }

        async fn stream_chat(
            &self,
            _request: InferenceRequest,
        ) -> Result<provider_core::ProviderStream, ProviderError> {
            Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                501,
                "not implemented in tests",
            ))
        }

        async fn stream_responses(
            &self,
            _request: InferenceRequest,
        ) -> Result<provider_core::ProviderStream, ProviderError> {
            Err(ProviderError::new(
                ProviderErrorKind::Unsupported,
                501,
                "not implemented in tests",
            ))
        }
    }

    fn quota_test_state(behavior: QuotaTestBehavior) -> ControlPlaneState {
        let store = storage::PlatformStore::demo();
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(QuotaTestProvider { behavior }));
        ControlPlaneState { store, registry }
    }

    async fn seed_quota_test_provider_account(
        state: &ControlPlaneState,
        account_id: &str,
    ) -> storage::ProviderAccountRecord {
        state
            .store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "quota_test".to_string(),
                    credential_kind: "api_key".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "api_key": "quota-key",
                        "account_id": account_id,
                        "api_base": "https://quota.test.local"
                    }),
                    metadata: json!({
                        "email": "quota@example.com",
                        "plan_type": "team"
                    }),
                    labels: vec!["shared".to_string()],
                    tags: Default::default(),
                },
                ValidatedProviderAccount {
                    provider_account_id: account_id.to_string(),
                    redacted_display: Some("q***@***".to_string()),
                    expires_at: Some(Utc::now() + TimeDelta::days(7)),
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "quota-test-model".to_string(),
                        route_group: "quota-test".to_string(),
                        provider_kind: "quota_test".to_string(),
                        upstream_model: "quota-test-model".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: false,
                    supports_quota_probe: true,
                },
            )
            .await
            .expect("quota test provider account")
    }

    async fn seed_refreshable_provider_account(
        state: &ControlPlaneState,
        account_id: &str,
        token_endpoint: String,
        expires_in: TimeDelta,
    ) -> storage::ProviderAccountRecord {
        state
            .store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "access_token": "stale-access-token",
                        "refresh_token": "refresh-token",
                        "client_id": "app_refresh_test",
                        "token_endpoint": token_endpoint,
                        "account_id": account_id,
                        "api_base": "https://chatgpt.com/backend-api/codex"
                    }),
                    metadata: json!({
                        "email": "refresh@example.com",
                        "plan_type": "team"
                    }),
                    labels: vec!["shared".to_string()],
                    tags: Default::default(),
                },
                ValidatedProviderAccount {
                    provider_account_id: account_id.to_string(),
                    redacted_display: Some("r***@***".to_string()),
                    expires_at: Some(Utc::now() + expires_in),
                },
                AccountCapabilities {
                    models: vec![],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("refreshable provider account")
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
    async fn routing_overview_reports_auto_derived_route_groups() {
        let app = app(ControlPlaneState::demo());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/internal/v1/routing/overview")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");

        assert_eq!(
            body.get("auto_derived")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert!(
            body.pointer("/bindings_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
        assert!(
            body.pointer("/route_groups")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|route_groups| route_groups.iter().any(|route_group| {
                    route_group
                        .get("public_model")
                        .and_then(serde_json::Value::as_str)
                        == Some("gpt-4.1-mini")
                }))
        );
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

    #[tokio::test]
    async fn manual_probe_endpoint_revalidates_account_and_refreshes_capabilities() {
        let state = ControlPlaneState::demo();
        let addr = spawn_models_server().await;
        let record = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "token",
            "acct_probe_ok",
        )
        .await;
        let validated_at_before = record.last_validated_at.expect("validated at");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::Active);
        assert!(
            updated.last_validated_at.expect("updated validated at") > validated_at_before,
            "probe should refresh last_validated_at"
        );
        assert!(updated.capabilities.contains(&"gpt-5-codex".to_string()));
        assert!(
            updated
                .capabilities
                .contains(&"gpt-5-codex-mini".to_string())
        );
    }

    #[tokio::test]
    async fn manual_probe_endpoint_marks_invalid_credentials_after_401_probe() {
        let state = ControlPlaneState::demo();
        let addr = spawn_models_server_with_response(
            "Bearer expired-token".to_string(),
            StatusCode::UNAUTHORIZED,
            json!({
                "error": {
                    "message": "token invalidated",
                    "type": "authentication_error",
                    "code": "token_invalidated"
                }
            }),
        )
        .await;
        let record = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "expired-token",
            "acct_probe_invalid",
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::InvalidCredentials);
    }

    #[tokio::test]
    async fn manual_probe_endpoint_temporarily_excludes_account_after_transport_failure() {
        let state = ControlPlaneState::demo();
        let record = seed_openai_provider_account(
            &state,
            "http://127.0.0.1:1/v1".to_string(),
            "token",
            "acct_probe_transport",
        )
        .await;
        let route_group = state
            .store
            .create_route_group(
                "probe-health-model".to_string(),
                "openai_codex".to_string(),
                "gpt-5-codex".to_string(),
            )
            .await
            .expect("route group");
        state
            .store
            .bind_provider_account(route_group.id, record.id, 100, 16)
            .await
            .expect("binding");

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let candidate = state
            .store
            .scheduler_candidates("probe-health-model")
            .await
            .expect("candidates")
            .into_iter()
            .find(|candidate| candidate.account_id == record.id)
            .expect("candidate");
        assert!(
            candidate.runtime.circuit_open_until.is_some(),
            "probe transport failures should open a temporary circuit"
        );
        assert!(candidate.runtime.health_score < 100);
        assert!(
            state
                .store
                .choose_candidate("probe-health-model")
                .await
                .expect("selected candidate")
                .is_none(),
            "transport failures should temporarily exclude the account from scheduling"
        );
    }

    #[tokio::test]
    async fn batch_probe_endpoint_processes_multiple_accounts_and_returns_summary() {
        let state = ControlPlaneState::demo();
        let healthy_addr = spawn_models_server().await;
        let invalid_addr = spawn_models_server_with_response(
            "Bearer expired-token".to_string(),
            StatusCode::UNAUTHORIZED,
            json!({
                "error": {
                    "message": "token invalidated",
                    "type": "authentication_error",
                    "code": "token_invalidated"
                }
            }),
        )
        .await;

        let healthy = seed_openai_provider_account(
            &state,
            format!("http://{healthy_addr}/v1"),
            "token",
            "acct_batch_healthy",
        )
        .await;
        let invalid = seed_openai_provider_account(
            &state,
            format!("http://{invalid_addr}/v1"),
            "expired-token",
            "acct_batch_invalid",
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/probe")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "account_ids": [healthy.id, invalid.id]
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
        assert!(body.contains("\"total\":2"));
        assert!(body.contains("\"healthy\":1"));
        assert!(body.contains("\"unhealthy\":1"));
        assert!(body.contains(&healthy.id.to_string()));
        assert!(body.contains(&invalid.id.to_string()));

        let updated_healthy = provider_account(&state, healthy.id).await;
        let updated_invalid = provider_account(&state, invalid.id).await;
        assert_eq!(updated_healthy.state, AccountState::Active);
        assert_eq!(updated_invalid.state, AccountState::InvalidCredentials);
    }

    #[tokio::test]
    async fn probe_marks_account_invalid_when_token_expires_after_successful_upload() {
        let state = ControlPlaneState::demo();
        let (addr, fail_after_upload) =
            spawn_flaky_models_server("Bearer rotating-token".to_string()).await;

        let upload_response = app(state.clone())
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
                                "access_token": "rotating-token",
                                "account_id": "acct_uploaded_then_expired",
                                "api_base": format!("http://{addr}/v1")
                            },
                            "metadata": {
                                "email": "rotate@example.com",
                                "plan_type": "team"
                            },
                            "labels": ["shared"],
                            "tags": { "region": "global" }
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("upload response");

        assert_eq!(upload_response.status(), StatusCode::OK);
        let upload_body = to_bytes(upload_response.into_body(), usize::MAX)
            .await
            .expect("upload body");
        let upload_json: serde_json::Value =
            serde_json::from_slice(&upload_body).expect("upload json");
        let account_id = upload_json["provider_account"]["id"]
            .as_str()
            .expect("account id")
            .to_string();

        fail_after_upload.store(true, Ordering::SeqCst);

        let probe_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/internal/v1/provider-accounts/{account_id}/probe"))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("probe request"),
            )
            .await
            .expect("probe response");

        assert_eq!(probe_response.status(), StatusCode::OK);

        let updated = state
            .store
            .list_provider_accounts()
            .await
            .expect("provider accounts")
            .into_iter()
            .find(|record| record.id.to_string() == account_id)
            .expect("updated record");
        assert_eq!(updated.state, AccountState::InvalidCredentials);
    }

    #[tokio::test]
    async fn manual_probe_endpoint_persists_a_healthy_inspection_record() {
        let state = ControlPlaneState::demo();
        let addr = spawn_models_server().await;
        let record = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "token",
            "acct_probe_record_ok",
        )
        .await;

        let probe_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("probe request"),
            )
            .await
            .expect("probe response");

        assert_eq!(probe_response.status(), StatusCode::OK);

        let inspections_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("inspections request"),
            )
            .await
            .expect("inspections response");

        assert_eq!(inspections_response.status(), StatusCode::OK);

        let inspections_body = to_bytes(inspections_response.into_body(), usize::MAX)
            .await
            .expect("inspections body");
        let inspections_json: serde_json::Value =
            serde_json::from_slice(&inspections_body).expect("inspections json");
        let data = inspections_json["data"]
            .as_array()
            .expect("inspection list");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["provider_account_id"], record.id.to_string());
        assert_eq!(data[0]["actor"], "platform-admin-demo");
        assert_eq!(data[0]["status"], "healthy");
        assert!(data[0]["error_kind"].is_null());
        assert!(data[0]["error_code"].is_null());
    }

    #[tokio::test]
    async fn batch_probe_endpoint_persists_individual_inspection_results() {
        let state = ControlPlaneState::demo();
        let healthy_addr = spawn_models_server().await;
        let invalid_addr = spawn_models_server_with_response(
            "Bearer expired-token".to_string(),
            StatusCode::UNAUTHORIZED,
            json!({
                "error": {
                    "message": "token invalidated",
                    "type": "authentication_error",
                    "code": "token_invalidated"
                }
            }),
        )
        .await;

        let healthy = seed_openai_provider_account(
            &state,
            format!("http://{healthy_addr}/v1"),
            "token",
            "acct_batch_record_healthy",
        )
        .await;
        let invalid = seed_openai_provider_account(
            &state,
            format!("http://{invalid_addr}/v1"),
            "expired-token",
            "acct_batch_record_invalid",
        )
        .await;

        let batch_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/probe")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "account_ids": [healthy.id, invalid.id]
                        })
                        .to_string(),
                    ))
                    .expect("batch request"),
            )
            .await
            .expect("batch response");

        assert_eq!(batch_response.status(), StatusCode::OK);

        let healthy_inspections = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        healthy.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("healthy inspections request"),
            )
            .await
            .expect("healthy inspections response");
        assert_eq!(healthy_inspections.status(), StatusCode::OK);
        let healthy_body = to_bytes(healthy_inspections.into_body(), usize::MAX)
            .await
            .expect("healthy inspections body");
        let healthy_json: serde_json::Value =
            serde_json::from_slice(&healthy_body).expect("healthy inspections json");
        let healthy_data = healthy_json["data"]
            .as_array()
            .expect("healthy inspection list");
        assert_eq!(healthy_data.len(), 1);
        assert_eq!(healthy_data[0]["status"], "healthy");

        let invalid_inspections = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        invalid.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("invalid inspections request"),
            )
            .await
            .expect("invalid inspections response");
        assert_eq!(invalid_inspections.status(), StatusCode::OK);
        let invalid_body = to_bytes(invalid_inspections.into_body(), usize::MAX)
            .await
            .expect("invalid inspections body");
        let invalid_json: serde_json::Value =
            serde_json::from_slice(&invalid_body).expect("invalid inspections json");
        let invalid_data = invalid_json["data"]
            .as_array()
            .expect("invalid inspection list");
        assert_eq!(invalid_data.len(), 1);
        assert_eq!(invalid_data[0]["status"], "unhealthy");
        assert_eq!(invalid_data[0]["error_kind"], "invalid_credentials");
        assert_eq!(invalid_data[0]["error_code"], "token_invalidated");
    }

    #[tokio::test]
    async fn scheduled_probe_dispatch_leases_due_accounts_and_skips_accounts_with_active_leases() {
        let state = ControlPlaneState::demo();
        let addr = spawn_models_server().await;
        let _first = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "token",
            "acct_dispatch_first",
        )
        .await;
        let _second = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "token",
            "acct_dispatch_second",
        )
        .await;

        let first_dispatch = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/probe/dispatch")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "limit": 1 }).to_string()))
                    .expect("dispatch request"),
            )
            .await
            .expect("dispatch response");
        assert_eq!(first_dispatch.status(), StatusCode::OK);
        let first_body = to_bytes(first_dispatch.into_body(), usize::MAX)
            .await
            .expect("dispatch body");
        let first_json: serde_json::Value =
            serde_json::from_slice(&first_body).expect("dispatch json");
        let first_items = first_json["data"].as_array().expect("dispatch items");
        assert_eq!(first_items.len(), 1);
        assert!(first_items[0]["lease_id"].as_str().is_some());

        let leased_id = first_items[0]["account_id"]
            .as_str()
            .expect("leased account id");
        assert!(
            !leased_id.is_empty(),
            "dispatch should return a leased provider account id"
        );

        let second_dispatch = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/probe/dispatch")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "limit": 2 }).to_string()))
                    .expect("second dispatch request"),
            )
            .await
            .expect("second dispatch response");
        assert_eq!(second_dispatch.status(), StatusCode::OK);
        let second_body = to_bytes(second_dispatch.into_body(), usize::MAX)
            .await
            .expect("second dispatch body");
        let second_json: serde_json::Value =
            serde_json::from_slice(&second_body).expect("second dispatch json");
        let second_items = second_json["data"]
            .as_array()
            .expect("second dispatch items");
        assert!(
            second_items
                .iter()
                .all(|item| item["account_id"].as_str() != Some(leased_id)),
            "an account with an active lease should not be dispatched twice"
        );
    }

    #[tokio::test]
    async fn quota_probe_marks_account_quota_exhausted_when_remaining_requests_are_zero() {
        let state = quota_test_state(QuotaTestBehavior::Exhausted);
        let record = seed_quota_test_provider_account(&state, "acct_quota_exhausted").await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota probe request"),
            )
            .await
            .expect("quota probe response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::QuotaExhausted);

        let inspections = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("inspections request"),
            )
            .await
            .expect("inspections response");
        assert_eq!(inspections.status(), StatusCode::OK);
        let inspections_body = to_bytes(inspections.into_body(), usize::MAX)
            .await
            .expect("inspections body");
        let inspections_json: serde_json::Value =
            serde_json::from_slice(&inspections_body).expect("inspections json");
        let latest = inspections_json["data"][0].clone();
        assert_eq!(latest["status"], "unhealthy");
        assert_eq!(latest["error_kind"], "quota_exhausted");
    }

    #[tokio::test]
    async fn quota_details_endpoint_returns_null_before_first_probe() {
        let state = quota_test_state(QuotaTestBehavior::Healthy);
        let record = seed_quota_test_provider_account(&state, "acct_quota_empty").await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota get request"),
            )
            .await
            .expect("quota get response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("quota get body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("quota get json");
        assert_eq!(json["account_id"], record.id.to_string());
        assert!(json["quota"].is_null());
    }

    #[tokio::test]
    async fn quota_details_endpoint_returns_latest_snapshot_after_probe() {
        let state = quota_test_state(QuotaTestBehavior::Healthy);
        let record = seed_quota_test_provider_account(&state, "acct_quota_details").await;

        let probe = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota probe request"),
            )
            .await
            .expect("quota probe response");
        assert_eq!(probe.status(), StatusCode::OK);

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota get request"),
            )
            .await
            .expect("quota get response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("quota get body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("quota get json");
        assert_eq!(json["account_id"], record.id.to_string());
        assert_eq!(json["quota"]["plan_label"], "team");
        assert_eq!(json["quota"]["remaining_requests_hint"], 42);
        assert_eq!(
            json["quota"]["details"]["rate_limit"]["primary_window"]["used_percent"],
            12
        );
        assert_eq!(json["quota"]["details"]["spend_control"]["reached"], false);
    }

    #[tokio::test]
    async fn runtime_provider_accounts_list_includes_latest_quota_snapshot() {
        let state = quota_test_state(QuotaTestBehavior::Healthy);
        let record = seed_quota_test_provider_account(&state, "acct_quota_runtime_list").await;

        let probe = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota probe request"),
            )
            .await
            .expect("quota probe response");
        assert_eq!(probe.status(), StatusCode::OK);

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/internal/v1/runtime/provider-accounts")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("runtime provider accounts request"),
            )
            .await
            .expect("runtime provider accounts response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("runtime provider accounts body");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("runtime provider accounts json");
        let item = json["data"]
            .as_array()
            .expect("runtime provider accounts array")
            .iter()
            .find(|item| item["id"] == json!(record.id))
            .cloned()
            .expect("quota-enabled provider account");

        assert_eq!(item["quota"]["plan_label"], "team");
        assert_eq!(item["quota"]["remaining_requests_hint"], 42);
        assert_eq!(
            item["quota"]["details"]["rate_limit"]["primary_window"]["used_percent"],
            12
        );
    }

    #[tokio::test]
    async fn quota_probe_marks_account_invalid_when_provider_reports_account_suspension() {
        let state = quota_test_state(QuotaTestBehavior::Suspended);
        let record = seed_quota_test_provider_account(&state, "acct_quota_suspended").await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/quota/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("quota probe request"),
            )
            .await
            .expect("quota probe response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::InvalidCredentials);

        let inspections = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("inspections request"),
            )
            .await
            .expect("inspections response");
        assert_eq!(inspections.status(), StatusCode::OK);
        let inspections_body = to_bytes(inspections.into_body(), usize::MAX)
            .await
            .expect("inspections body");
        let inspections_json: serde_json::Value =
            serde_json::from_slice(&inspections_body).expect("inspections json");
        let latest = inspections_json["data"][0].clone();
        assert_eq!(latest["status"], "unhealthy");
        assert_eq!(latest["error_kind"], "invalid_credentials");
        assert_eq!(latest["error_code"], "account_suspended");
    }

    #[tokio::test]
    async fn scheduled_refresh_dispatch_leases_expiring_accounts_and_skips_fresh_ones() {
        let state = ControlPlaneState::demo();
        let refresh_addr = spawn_refresh_token_server(
            "refresh-token".to_string(),
            "app_refresh_test".to_string(),
            RefreshServerBehavior::Success,
        )
        .await;
        let due = seed_refreshable_provider_account(
            &state,
            "acct_refresh_due",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::minutes(5),
        )
        .await;
        let _fresh = seed_refreshable_provider_account(
            &state,
            "acct_refresh_fresh",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::hours(4),
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/refresh/dispatch")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "limit": 10, "refresh_before_seconds": 1800 }).to_string(),
                    ))
                    .expect("refresh dispatch request"),
            )
            .await
            .expect("refresh dispatch response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("refresh dispatch body");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("refresh dispatch json");
        let items = payload["data"].as_array().expect("refresh dispatch items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["account_id"], json!(due.id));

        let second = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/refresh/dispatch")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "limit": 10, "refresh_before_seconds": 1800 }).to_string(),
                    ))
                    .expect("second refresh dispatch request"),
            )
            .await
            .expect("second refresh dispatch response");
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second refresh dispatch body");
        let second_payload: serde_json::Value =
            serde_json::from_slice(&second_body).expect("second refresh dispatch json");
        let second_items = second_payload["data"]
            .as_array()
            .expect("second refresh dispatch items");
        assert!(second_items.is_empty());
    }

    #[tokio::test]
    async fn scheduled_refresh_run_rotates_only_due_accounts() {
        let state = ControlPlaneState::demo();
        let refresh_addr = spawn_refresh_token_server(
            "refresh-token".to_string(),
            "app_refresh_test".to_string(),
            RefreshServerBehavior::Success,
        )
        .await;
        let due = seed_refreshable_provider_account(
            &state,
            "acct_refresh_due_run",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::minutes(5),
        )
        .await;
        let fresh = seed_refreshable_provider_account(
            &state,
            "acct_refresh_fresh_run",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::hours(4),
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/provider-accounts/refresh/run")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "limit": 10, "refresh_before_seconds": 1800 }).to_string(),
                    ))
                    .expect("refresh run request"),
            )
            .await
            .expect("refresh run response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("refresh run body");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("refresh run json");
        assert_eq!(payload["total"], json!(1));
        assert_eq!(payload["refreshed"], json!(1));
        assert_eq!(payload["failed"], json!(0));
        assert_eq!(payload["results"][0]["account_id"], json!(due.id));
        assert_eq!(payload["results"][0]["status"], json!("healthy"));

        let due_envelope = state
            .store
            .provider_account_envelope(due.id)
            .await
            .expect("due envelope")
            .expect("due account");
        assert_eq!(
            due_envelope.credentials["access_token"],
            json!("new-access-token")
        );

        let fresh_envelope = state
            .store
            .provider_account_envelope(fresh.id)
            .await
            .expect("fresh envelope")
            .expect("fresh account");
        assert_eq!(
            fresh_envelope.credentials["access_token"],
            json!("stale-access-token")
        );
    }

    #[tokio::test]
    async fn manual_refresh_rotates_access_token_and_keeps_account_active() {
        let state = ControlPlaneState::demo();
        let refresh_addr = spawn_refresh_token_server(
            "refresh-token".to_string(),
            "app_refresh_test".to_string(),
            RefreshServerBehavior::Success,
        )
        .await;
        let record = seed_refreshable_provider_account(
            &state,
            "acct_refresh_success",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::minutes(5),
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/refresh",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("refresh request"),
            )
            .await
            .expect("refresh response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::Active);

        let envelope = state
            .store
            .provider_account_envelope(record.id)
            .await
            .expect("provider envelope")
            .expect("provider account envelope");
        assert_eq!(
            envelope.credentials["access_token"],
            json!("new-access-token")
        );
        assert_eq!(
            envelope.credentials["refresh_token"],
            json!("new-refresh-token")
        );
    }

    #[tokio::test]
    async fn manual_refresh_marks_account_invalid_when_refresh_token_is_rejected() {
        let state = ControlPlaneState::demo();
        let refresh_addr = spawn_refresh_token_server(
            "refresh-token".to_string(),
            "app_refresh_test".to_string(),
            RefreshServerBehavior::InvalidGrant,
        )
        .await;
        let record = seed_refreshable_provider_account(
            &state,
            "acct_refresh_failure",
            format!("http://{refresh_addr}/oauth/token"),
            TimeDelta::minutes(5),
        )
        .await;

        let response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/refresh",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("refresh request"),
            )
            .await
            .expect("refresh response");

        assert_eq!(response.status(), StatusCode::OK);

        let updated = provider_account(&state, record.id).await;
        assert_eq!(updated.state, AccountState::InvalidCredentials);

        let inspections = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/inspections",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("inspections request"),
            )
            .await
            .expect("inspections response");
        assert_eq!(inspections.status(), StatusCode::OK);
        let inspections_body = to_bytes(inspections.into_body(), usize::MAX)
            .await
            .expect("inspections body");
        let inspections_json: serde_json::Value =
            serde_json::from_slice(&inspections_body).expect("inspections json");
        let latest = inspections_json["data"][0].clone();
        assert_eq!(latest["status"], "unhealthy");
        assert_eq!(latest["error_kind"], "invalid_credentials");
        assert_eq!(latest["error_code"], "invalid_grant");
    }

    #[tokio::test]
    async fn alerts_outbox_exposes_invalid_credentials_events_for_operator_follow_up() {
        let state = ControlPlaneState::demo();
        let addr = spawn_models_server_with_response(
            "Bearer expired-token".to_string(),
            StatusCode::UNAUTHORIZED,
            json!({
                "error": {
                    "message": "token invalidated",
                    "type": "authentication_error",
                    "code": "token_invalidated"
                }
            }),
        )
        .await;
        let record = seed_openai_provider_account(
            &state,
            format!("http://{addr}/v1"),
            "expired-token",
            "acct_alert_invalid",
        )
        .await;

        let probe_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/provider-accounts/{}/probe",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("probe request"),
            )
            .await
            .expect("probe response");
        assert_eq!(probe_response.status(), StatusCode::OK);

        let alerts_response = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/internal/v1/alerts/outbox?resource=provider_account:{}",
                        record.id
                    ))
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .body(Body::empty())
                    .expect("alerts request"),
            )
            .await
            .expect("alerts response");

        assert_eq!(alerts_response.status(), StatusCode::OK);
        let alerts_body = to_bytes(alerts_response.into_body(), usize::MAX)
            .await
            .expect("alerts body");
        let alerts_json: serde_json::Value =
            serde_json::from_slice(&alerts_body).expect("alerts json");
        let first = alerts_json["data"][0].clone();
        assert_eq!(first["kind"], "provider_account.invalid_credentials");
        assert_eq!(first["severity"], "critical");
        assert_eq!(first["resource"], format!("provider_account:{}", record.id));
    }

    #[tokio::test]
    async fn alerts_outbox_delivery_posts_webhook_once_and_skips_duplicate_alerts() {
        let state = ControlPlaneState::demo();
        let record = seed_quota_test_provider_account(&state, "acct_alert_delivery").await;
        state
            .store
            .record_account_inspection(
                record.id,
                "probe-worker".to_string(),
                storage::AccountInspectionStatus::Unhealthy,
                Some("invalid_credentials".to_string()),
                Some("token_invalidated".to_string()),
                Some("token invalidated".to_string()),
            )
            .await
            .expect("inspection");

        let (webhook_addr, deliveries) = spawn_alert_webhook_server().await;
        let webhook_url = format!("http://{webhook_addr}/webhook");

        let first = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/alerts/outbox")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "webhook_url": webhook_url,
                            "limit": 10,
                            "resource": format!("provider_account:{}", record.id)
                        })
                        .to_string(),
                    ))
                    .expect("deliver alerts request"),
            )
            .await
            .expect("deliver alerts response");
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first deliver body");
        let first_json: serde_json::Value =
            serde_json::from_slice(&first_body).expect("first deliver json");
        assert_eq!(first_json["delivered"], json!(1));
        assert_eq!(first_json["skipped"], json!(0));
        assert_eq!(deliveries.lock().expect("deliveries lock").len(), 1);

        let delivered_payload = deliveries.lock().expect("deliveries lock")[0].clone();
        assert_eq!(
            delivered_payload["kind"],
            "provider_account.invalid_credentials"
        );
        assert_eq!(
            delivered_payload["resource"],
            json!(format!("provider_account:{}", record.id))
        );

        let second = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/alerts/outbox")
                    .header(http::header::AUTHORIZATION, "Bearer fg_cp_admin_demo")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "webhook_url": format!("http://{webhook_addr}/webhook"),
                            "limit": 10,
                            "resource": format!("provider_account:{}", record.id)
                        })
                        .to_string(),
                    ))
                    .expect("deliver alerts request 2"),
            )
            .await
            .expect("deliver alerts response 2");
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second deliver body");
        let second_json: serde_json::Value =
            serde_json::from_slice(&second_body).expect("second deliver json");
        assert_eq!(second_json["delivered"], json!(0));
        assert_eq!(second_json["skipped"], json!(1));
        assert_eq!(deliveries.lock().expect("deliveries lock").len(), 1);
    }
}
