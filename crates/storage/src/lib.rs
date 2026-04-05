mod memory;
mod postgres;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use protocol_core::{ModelDescriptor, TokenUsage};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderConnectionInfo,
    ProviderCredentialResolver, ProviderError, ProviderErrorKind, ValidatedProviderAccount,
};
use scheduler::{AccountState, ProviderAccountCandidate, ProviderOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

pub use memory::InMemoryPlatformStore;
pub use postgres::PostgresPlatformStore;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tenant {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub suspended: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TenantApiKeyStatus {
    Active,
    Revoked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantApiKeyView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
    pub prefix: String,
    pub status: TenantApiKeyStatus,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreatedApiKey {
    pub record: TenantApiKeyView,
    pub secret: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayAuthContext {
    pub tenant: Tenant,
    pub api_key_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantManagementPrincipal {
    pub subject: String,
    pub tenant_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    PlatformAdmin,
    SecurityAdmin,
    RoutingOperator,
    TenantAdmin,
    Viewer,
    AutomationService,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceAccountPrincipal {
    pub subject: String,
    pub role: Role,
    pub scopes: Vec<ScopeTarget>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ScopeTarget {
    Global,
    ProviderPool(String),
    RouteGroup(Uuid),
    Tenant(Uuid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Permission {
    ImportProviderAccounts,
    ManageProviderState,
    ManageRouteGroups,
    ManageBindings,
    ManageTenants,
    ManageTenantApiKeys,
    ViewRuntime,
    ViewAudit,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("storage backend error: {0}")]
    Backend(String),
    #[error("record not found")]
    NotFound,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("storage backend error: {0}")]
    Storage(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderAccountRecord {
    pub id: Uuid,
    pub provider: String,
    pub credential_kind: String,
    pub payload_version: String,
    pub state: AccountState,
    pub external_account_id: String,
    pub redacted_display: Option<String>,
    pub plan_type: Option<String>,
    pub metadata: Value,
    pub labels: Vec<String>,
    pub tags: BTreeMap<String, String>,
    pub capabilities: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteGroupRecord {
    pub id: Uuid,
    pub slug: String,
    pub public_model: String,
    pub provider_kind: String,
    pub upstream_model: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteGroupBindingRecord {
    pub id: Uuid,
    pub route_group_id: Uuid,
    pub provider_account_id: Uuid,
    pub weight: u32,
    pub max_in_flight: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub api_key_id: Option<Uuid>,
    pub public_model: String,
    pub provider_kind: String,
    pub status_code: u16,
    pub latency_ms: u64,
    pub usage: TokenUsage,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageSummary {
    pub total_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub last_request_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub request_id: String,
    pub occurred_at: DateTime<Utc>,
    pub details: Value,
}

#[derive(Clone)]
pub enum PlatformStore {
    InMemory(InMemoryPlatformStore),
    Postgres(PostgresPlatformStore),
}

impl Default for PlatformStore {
    fn default() -> Self {
        Self::demo()
    }
}

impl PlatformStore {
    #[must_use]
    pub fn empty() -> Self {
        Self::InMemory(InMemoryPlatformStore::empty())
    }

    #[must_use]
    pub fn demo() -> Self {
        Self::InMemory(InMemoryPlatformStore::demo())
    }

    pub async fn from_env_or_demo() -> Result<Self, StoreError> {
        let backend = std::env::var("FERRUMGATE_STORE_BACKEND").ok();
        let database_url = std::env::var("DATABASE_URL").ok();

        let use_postgres = matches!(backend.as_deref(), Some("postgres"))
            || (backend.is_none() && database_url.is_some());

        if use_postgres {
            return Ok(Self::Postgres(
                PostgresPlatformStore::connect_from_env().await?,
            ));
        }

        Ok(Self::demo())
    }

    #[must_use]
    pub fn demo_gateway_key() -> &'static str {
        InMemoryPlatformStore::demo_gateway_key()
    }

    #[must_use]
    pub fn demo_tenant_management_token() -> &'static str {
        InMemoryPlatformStore::demo_tenant_management_token()
    }

    #[must_use]
    pub fn demo_control_plane_token() -> &'static str {
        InMemoryPlatformStore::demo_control_plane_token()
    }

    pub async fn validate_gateway_api_key(
        &self,
        secret: &str,
    ) -> Result<Option<GatewayAuthContext>, StoreError> {
        match self {
            Self::InMemory(store) => store.validate_gateway_api_key(secret).await,
            Self::Postgres(store) => store.validate_gateway_api_key(secret).await,
        }
    }

    pub async fn authenticate_tenant_management_token(
        &self,
        token: &str,
    ) -> Result<Option<TenantManagementPrincipal>, StoreError> {
        match self {
            Self::InMemory(store) => store.authenticate_tenant_management_token(token).await,
            Self::Postgres(store) => store.authenticate_tenant_management_token(token).await,
        }
    }

    pub async fn authorize_control(
        &self,
        token: &str,
        permission: Permission,
        target: ScopeTarget,
    ) -> Result<ServiceAccountPrincipal, AuthError> {
        match self {
            Self::InMemory(store) => store.authorize_control(token, permission, target).await,
            Self::Postgres(store) => store.authorize_control(token, permission, target).await,
        }
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_tenants().await,
            Self::Postgres(store) => store.list_tenants().await,
        }
    }

    pub async fn create_tenant(&self, slug: String, name: String) -> Result<Tenant, StoreError> {
        match self {
            Self::InMemory(store) => store.create_tenant(slug, name).await,
            Self::Postgres(store) => store.create_tenant(slug, name).await,
        }
    }

    pub async fn list_tenant_api_keys(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantApiKeyView>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_tenant_api_keys(tenant_id).await,
            Self::Postgres(store) => store.list_tenant_api_keys(tenant_id).await,
        }
    }

    pub async fn create_tenant_api_key(
        &self,
        tenant_id: Uuid,
        label: String,
    ) -> Result<CreatedApiKey, AuthError> {
        match self {
            Self::InMemory(store) => store.create_tenant_api_key(tenant_id, label).await,
            Self::Postgres(store) => store.create_tenant_api_key(tenant_id, label).await,
        }
    }

    pub async fn rotate_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<CreatedApiKey, AuthError> {
        match self {
            Self::InMemory(store) => store.rotate_tenant_api_key(tenant_id, api_key_id).await,
            Self::Postgres(store) => store.rotate_tenant_api_key(tenant_id, api_key_id).await,
        }
    }

    pub async fn revoke_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<TenantApiKeyView, AuthError> {
        match self {
            Self::InMemory(store) => store.revoke_tenant_api_key(tenant_id, api_key_id).await,
            Self::Postgres(store) => store.revoke_tenant_api_key(tenant_id, api_key_id).await,
        }
    }

    pub async fn list_tenant_models(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<ModelDescriptor>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_tenant_models(tenant_id).await,
            Self::Postgres(store) => store.list_tenant_models(tenant_id).await,
        }
    }

    pub async fn usage_summary(&self, tenant_id: Uuid) -> Result<UsageSummary, StoreError> {
        match self {
            Self::InMemory(store) => store.usage_summary(tenant_id).await,
            Self::Postgres(store) => store.usage_summary(tenant_id).await,
        }
    }

    pub async fn tenant_requests(&self, tenant_id: Uuid) -> Result<Vec<RequestRecord>, StoreError> {
        match self {
            Self::InMemory(store) => store.tenant_requests(tenant_id).await,
            Self::Postgres(store) => store.tenant_requests(tenant_id).await,
        }
    }

    pub async fn record_request(
        &self,
        tenant_id: Uuid,
        api_key_id: Option<Uuid>,
        public_model: String,
        provider_kind: String,
        status_code: u16,
        latency_ms: u64,
        usage: TokenUsage,
    ) -> Result<(), StoreError> {
        match self {
            Self::InMemory(store) => {
                store
                    .record_request(
                        tenant_id,
                        api_key_id,
                        public_model,
                        provider_kind,
                        status_code,
                        latency_ms,
                        usage,
                    )
                    .await
            }
            Self::Postgres(store) => {
                store
                    .record_request(
                        tenant_id,
                        api_key_id,
                        public_model,
                        provider_kind,
                        status_code,
                        latency_ms,
                        usage,
                    )
                    .await
            }
        }
    }

    pub async fn record_audit(
        &self,
        actor: impl Into<String>,
        action: impl Into<String>,
        resource: impl Into<String>,
        request_id: impl Into<String>,
        details: Value,
    ) -> Result<(), StoreError> {
        let actor = actor.into();
        let action = action.into();
        let resource = resource.into();
        let request_id = request_id.into();
        match self {
            Self::InMemory(store) => {
                store
                    .record_audit(actor, action, resource, request_id, details)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .record_audit(actor, action, resource, request_id, details)
                    .await
            }
        }
    }

    pub async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_audit_events().await,
            Self::Postgres(store) => store.list_audit_events().await,
        }
    }

    pub async fn list_provider_accounts(&self) -> Result<Vec<ProviderAccountRecord>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_provider_accounts().await,
            Self::Postgres(store) => store.list_provider_accounts().await,
        }
    }

    pub async fn ingest_provider_account(
        &self,
        envelope: ProviderAccountEnvelope,
        validated: ValidatedProviderAccount,
        capabilities: AccountCapabilities,
    ) -> Result<ProviderAccountRecord, StoreError> {
        match self {
            Self::InMemory(store) => {
                store
                    .ingest_provider_account(envelope, validated, capabilities)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .ingest_provider_account(envelope, validated, capabilities)
                    .await
            }
        }
    }

    pub async fn set_provider_account_state(
        &self,
        account_id: Uuid,
        state: AccountState,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        match self {
            Self::InMemory(store) => store.set_provider_account_state(account_id, state).await,
            Self::Postgres(store) => store.set_provider_account_state(account_id, state).await,
        }
    }

    pub async fn create_route_group(
        &self,
        public_model: String,
        provider_kind: String,
        upstream_model: String,
    ) -> Result<RouteGroupRecord, StoreError> {
        match self {
            Self::InMemory(store) => {
                store
                    .create_route_group(public_model, provider_kind, upstream_model)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .create_route_group(public_model, provider_kind, upstream_model)
                    .await
            }
        }
    }

    pub async fn list_route_groups(&self) -> Result<Vec<RouteGroupRecord>, StoreError> {
        match self {
            Self::InMemory(store) => store.list_route_groups().await,
            Self::Postgres(store) => store.list_route_groups().await,
        }
    }

    pub async fn bind_provider_account(
        &self,
        route_group_id: Uuid,
        provider_account_id: Uuid,
        weight: u32,
        max_in_flight: u32,
    ) -> Result<RouteGroupBindingRecord, AuthError> {
        match self {
            Self::InMemory(store) => {
                store
                    .bind_provider_account(
                        route_group_id,
                        provider_account_id,
                        weight,
                        max_in_flight,
                    )
                    .await
            }
            Self::Postgres(store) => {
                store
                    .bind_provider_account(
                        route_group_id,
                        provider_account_id,
                        weight,
                        max_in_flight,
                    )
                    .await
            }
        }
    }

    pub async fn resolve_route_group(
        &self,
        public_model: &str,
    ) -> Result<Option<RouteGroupRecord>, StoreError> {
        match self {
            Self::InMemory(store) => store.resolve_route_group(public_model).await,
            Self::Postgres(store) => store.resolve_route_group(public_model).await,
        }
    }

    pub async fn scheduler_candidates(
        &self,
        public_model: &str,
    ) -> Result<Vec<ProviderAccountCandidate>, StoreError> {
        match self {
            Self::InMemory(store) => store.scheduler_candidates(public_model).await,
            Self::Postgres(store) => store.scheduler_candidates(public_model).await,
        }
    }

    pub async fn mark_scheduler_outcome(
        &self,
        account_id: Uuid,
        outcome: ProviderOutcome,
    ) -> Result<(), StoreError> {
        match self {
            Self::InMemory(store) => store.mark_scheduler_outcome(account_id, outcome).await,
            Self::Postgres(store) => store.mark_scheduler_outcome(account_id, outcome).await,
        }
    }

    pub async fn choose_candidate(
        &self,
        public_model: &str,
    ) -> Result<Option<ProviderAccountCandidate>, StoreError> {
        match self {
            Self::InMemory(store) => store.choose_candidate(public_model).await,
            Self::Postgres(store) => store.choose_candidate(public_model).await,
        }
    }

    pub async fn resolve_provider_connection(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderConnectionInfo>, StoreError> {
        match self {
            Self::InMemory(store) => store.resolve_provider_connection(account_id).await,
            Self::Postgres(store) => store.resolve_provider_connection(account_id).await,
        }
    }
}

#[async_trait]
impl ProviderCredentialResolver for PlatformStore {
    async fn resolve_connection(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderConnectionInfo>, ProviderError> {
        self.resolve_provider_connection(account_id)
            .await
            .map_err(store_error_to_provider_error)
    }
}

pub(crate) fn role_allows(role: &Role, permission: &Permission) -> bool {
    match role {
        Role::PlatformAdmin => true,
        Role::SecurityAdmin => matches!(
            permission,
            Permission::ImportProviderAccounts
                | Permission::ManageProviderState
                | Permission::ViewRuntime
        ),
        Role::RoutingOperator => matches!(
            permission,
            Permission::ManageRouteGroups | Permission::ManageBindings | Permission::ViewRuntime
        ),
        Role::TenantAdmin => matches!(
            permission,
            Permission::ManageTenants | Permission::ManageTenantApiKeys | Permission::ViewRuntime
        ),
        Role::Viewer => matches!(permission, Permission::ViewRuntime | Permission::ViewAudit),
        Role::AutomationService => matches!(
            permission,
            Permission::ImportProviderAccounts
                | Permission::ManageBindings
                | Permission::ViewRuntime
        ),
    }
}

pub(crate) fn scope_allows(scopes: &[ScopeTarget], target: &ScopeTarget) -> bool {
    scopes.iter().any(|scope| match (scope, target) {
        (ScopeTarget::Global, _) => true,
        (ScopeTarget::ProviderPool(left), ScopeTarget::ProviderPool(right)) => left == right,
        (ScopeTarget::RouteGroup(left), ScopeTarget::RouteGroup(right)) => left == right,
        (ScopeTarget::Tenant(left), ScopeTarget::Tenant(right)) => left == right,
        _ => false,
    })
}

pub(crate) fn store_error_to_provider_error(error: StoreError) -> ProviderError {
    ProviderError::new(ProviderErrorKind::InvalidRequest, 500, error.to_string())
}

pub(crate) fn provider_connection_from_parts(
    account_id: Uuid,
    provider_kind: &str,
    credential_kind: &str,
    metadata: &Value,
    credentials: &Value,
) -> Result<ProviderConnectionInfo, StoreError> {
    let api_base = credentials
        .get("api_base")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("api_base").and_then(Value::as_str))
        .map(ToString::to_string)
        .or_else(|| default_api_base(provider_kind))
        .ok_or_else(|| {
            StoreError::Backend(format!("provider account {account_id} is missing api_base"))
        })?;

    let bearer_token = credentials
        .get("access_token")
        .and_then(Value::as_str)
        .or_else(|| credentials.get("bearer_token").and_then(Value::as_str))
        .or_else(|| credentials.get("api_key").and_then(Value::as_str))
        .map(ToString::to_string)
        .ok_or_else(|| {
            StoreError::Backend(format!(
                "provider account {account_id} is missing a bearer credential"
            ))
        })?;

    let mut additional_headers = BTreeMap::new();
    additional_headers.extend(extract_header_map(metadata.get("additional_headers")));
    additional_headers.extend(extract_header_map(credentials.get("additional_headers")));

    let model_override = credentials
        .get("model_override")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("model_override").and_then(Value::as_str))
        .map(ToString::to_string);

    Ok(ProviderConnectionInfo {
        account_id,
        provider_kind: provider_kind.to_string(),
        credential_kind: credential_kind.to_string(),
        api_base: api_base.trim_end_matches('/').to_string(),
        bearer_token,
        model_override,
        additional_headers,
    })
}

fn extract_header_map(value: Option<&Value>) -> BTreeMap<String, String> {
    let Some(Value::Object(object)) = value else {
        return BTreeMap::new();
    };

    object
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect()
}

fn default_api_base(provider_kind: &str) -> Option<String> {
    match provider_kind {
        "openai_codex" => Some(
            std::env::var("FERRUMGATE_OPENAI_API_BASE")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        ),
        _ => None,
    }
}
