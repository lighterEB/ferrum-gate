use chrono::{DateTime, Utc};
use protocol_core::{ModelCapability, ModelDescriptor, TokenUsage};
use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};
use scheduler::{
    AccountRuntime, AccountState, ProviderAccountCandidate, ProviderOutcome, select_candidate,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

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

#[derive(Clone, Debug)]
struct TenantApiKeyRecord {
    view: TenantApiKeyView,
    secret: String,
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
pub enum AuthError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
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
pub struct InMemoryPlatformStore {
    inner: Arc<InnerStore>,
}

struct InnerStore {
    tenants: RwLock<BTreeMap<Uuid, Tenant>>,
    tenant_api_keys: RwLock<BTreeMap<Uuid, TenantApiKeyRecord>>,
    tenant_api_key_lookup: RwLock<HashMap<String, Uuid>>,
    tenant_management_tokens: RwLock<HashMap<String, TenantManagementPrincipal>>,
    service_accounts: RwLock<HashMap<String, ServiceAccountPrincipal>>,
    provider_accounts: RwLock<BTreeMap<Uuid, ProviderAccountRecord>>,
    route_groups: RwLock<BTreeMap<Uuid, RouteGroupRecord>>,
    route_group_bindings: RwLock<BTreeMap<Uuid, RouteGroupBindingRecord>>,
    runtimes: RwLock<HashMap<Uuid, AccountRuntime>>,
    requests: RwLock<Vec<RequestRecord>>,
    audits: RwLock<Vec<AuditEvent>>,
}

impl Default for InMemoryPlatformStore {
    fn default() -> Self {
        Self::demo()
    }
}

impl InMemoryPlatformStore {
    #[must_use]
    pub fn demo() -> Self {
        let tenant_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        let route_group_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000101").expect("valid uuid");
        let provider_account_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000201").expect("valid uuid");
        let api_key_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000301").expect("valid uuid");
        let binding_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000401").expect("valid uuid");
        let now = Utc::now();

        let tenant = Tenant {
            id: tenant_id,
            slug: "demo-tenant".to_string(),
            name: "Demo Tenant".to_string(),
            suspended: false,
            created_at: now,
        };

        let api_key_secret = "fgk_demo_gateway_key".to_string();
        let api_key = TenantApiKeyRecord {
            view: TenantApiKeyView {
                id: api_key_id,
                tenant_id,
                label: "default".to_string(),
                prefix: api_key_secret[..8].to_string(),
                status: TenantApiKeyStatus::Active,
                created_at: now,
                last_used_at: None,
            },
            secret: api_key_secret.clone(),
        };

        let provider_account = ProviderAccountRecord {
            id: provider_account_id,
            provider: "openai_codex".to_string(),
            credential_kind: "oauth_tokens".to_string(),
            payload_version: "v1".to_string(),
            state: AccountState::Active,
            external_account_id: "acct_demo_openai_codex".to_string(),
            redacted_display: Some("d***@***".to_string()),
            plan_type: Some("plus".to_string()),
            metadata: json!({ "email": "demo@example.com" }),
            labels: vec!["shared".to_string(), "prod".to_string()],
            tags: BTreeMap::from([("region".to_string(), "global".to_string())]),
            capabilities: vec![
                "chat".to_string(),
                "responses".to_string(),
                "streaming".to_string(),
            ],
            expires_at: None,
            last_validated_at: Some(now),
            created_at: now,
        };

        let route_group = RouteGroupRecord {
            id: route_group_id,
            slug: "openai-gpt-4-1-mini".to_string(),
            public_model: "gpt-4.1-mini".to_string(),
            provider_kind: "openai_codex".to_string(),
            upstream_model: "gpt-4.1-mini".to_string(),
            created_at: now,
        };

        let binding = RouteGroupBindingRecord {
            id: binding_id,
            route_group_id,
            provider_account_id,
            weight: 100,
            max_in_flight: 16,
            created_at: now,
        };

        let inner = InnerStore {
            tenants: RwLock::new(BTreeMap::from([(tenant_id, tenant)])),
            tenant_api_keys: RwLock::new(BTreeMap::from([(api_key_id, api_key)])),
            tenant_api_key_lookup: RwLock::new(HashMap::from([(api_key_secret, api_key_id)])),
            tenant_management_tokens: RwLock::new(HashMap::from([(
                "fg_tenant_admin_demo".to_string(),
                TenantManagementPrincipal {
                    subject: "tenant-admin-demo".to_string(),
                    tenant_id,
                },
            )])),
            service_accounts: RwLock::new(HashMap::from([
                (
                    "fg_cp_admin_demo".to_string(),
                    ServiceAccountPrincipal {
                        subject: "platform-admin-demo".to_string(),
                        role: Role::PlatformAdmin,
                        scopes: vec![ScopeTarget::Global],
                    },
                ),
                (
                    "fg_cp_routing_demo".to_string(),
                    ServiceAccountPrincipal {
                        subject: "routing-operator-demo".to_string(),
                        role: Role::RoutingOperator,
                        scopes: vec![ScopeTarget::Global],
                    },
                ),
            ])),
            provider_accounts: RwLock::new(BTreeMap::from([(
                provider_account_id,
                provider_account,
            )])),
            route_groups: RwLock::new(BTreeMap::from([(route_group_id, route_group)])),
            route_group_bindings: RwLock::new(BTreeMap::from([(binding_id, binding)])),
            runtimes: RwLock::new(HashMap::from([(
                provider_account_id,
                AccountRuntime::new(AccountState::Active, 16),
            )])),
            requests: RwLock::new(Vec::new()),
            audits: RwLock::new(Vec::new()),
        };

        Self {
            inner: Arc::new(inner),
        }
    }

    #[must_use]
    pub fn demo_gateway_key() -> &'static str {
        "fgk_demo_gateway_key"
    }

    #[must_use]
    pub fn demo_tenant_management_token() -> &'static str {
        "fg_tenant_admin_demo"
    }

    #[must_use]
    pub fn demo_control_plane_token() -> &'static str {
        "fg_cp_admin_demo"
    }

    pub async fn validate_gateway_api_key(&self, secret: &str) -> Option<GatewayAuthContext> {
        let api_key_id = self
            .inner
            .tenant_api_key_lookup
            .read()
            .await
            .get(secret)
            .copied()?;
        let mut api_keys = self.inner.tenant_api_keys.write().await;
        let record = api_keys.get_mut(&api_key_id)?;
        if record.view.status != TenantApiKeyStatus::Active {
            return None;
        }
        record.view.last_used_at = Some(Utc::now());
        let tenant = self
            .inner
            .tenants
            .read()
            .await
            .get(&record.view.tenant_id)
            .cloned()?;
        if tenant.suspended {
            return None;
        }
        Some(GatewayAuthContext { tenant, api_key_id })
    }

    pub async fn authenticate_tenant_management_token(
        &self,
        token: &str,
    ) -> Option<TenantManagementPrincipal> {
        self.inner
            .tenant_management_tokens
            .read()
            .await
            .get(token)
            .cloned()
    }

    pub async fn authorize_control(
        &self,
        token: &str,
        permission: Permission,
        target: ScopeTarget,
    ) -> Result<ServiceAccountPrincipal, AuthError> {
        let principal = self
            .inner
            .service_accounts
            .read()
            .await
            .get(token)
            .cloned()
            .ok_or(AuthError::Unauthorized)?;

        if !role_allows(&principal.role, &permission) || !scope_allows(&principal.scopes, &target) {
            return Err(AuthError::Forbidden);
        }

        Ok(principal)
    }

    pub async fn list_tenants(&self) -> Vec<Tenant> {
        self.inner.tenants.read().await.values().cloned().collect()
    }

    pub async fn create_tenant(&self, slug: String, name: String) -> Tenant {
        let tenant = Tenant {
            id: Uuid::new_v4(),
            slug,
            name,
            suspended: false,
            created_at: Utc::now(),
        };
        self.inner
            .tenants
            .write()
            .await
            .insert(tenant.id, tenant.clone());
        tenant
    }

    pub async fn list_tenant_api_keys(&self, tenant_id: Uuid) -> Vec<TenantApiKeyView> {
        self.inner
            .tenant_api_keys
            .read()
            .await
            .values()
            .filter(|record| record.view.tenant_id == tenant_id)
            .map(|record| record.view.clone())
            .collect()
    }

    pub async fn create_tenant_api_key(
        &self,
        tenant_id: Uuid,
        label: String,
    ) -> Result<CreatedApiKey, AuthError> {
        if !self.inner.tenants.read().await.contains_key(&tenant_id) {
            return Err(AuthError::Unauthorized);
        }

        let secret = format!("fgk_{}", Uuid::new_v4().simple());
        let view = TenantApiKeyView {
            id: Uuid::new_v4(),
            tenant_id,
            label,
            prefix: secret[..12].to_string(),
            status: TenantApiKeyStatus::Active,
            created_at: Utc::now(),
            last_used_at: None,
        };

        let record = TenantApiKeyRecord {
            view: view.clone(),
            secret: secret.clone(),
        };

        self.inner
            .tenant_api_key_lookup
            .write()
            .await
            .insert(secret.clone(), view.id);
        self.inner
            .tenant_api_keys
            .write()
            .await
            .insert(view.id, record);

        Ok(CreatedApiKey {
            record: view,
            secret,
        })
    }

    pub async fn rotate_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<CreatedApiKey, AuthError> {
        let mut api_keys = self.inner.tenant_api_keys.write().await;
        let record = api_keys
            .get_mut(&api_key_id)
            .ok_or(AuthError::Unauthorized)?;
        if record.view.tenant_id != tenant_id {
            return Err(AuthError::Forbidden);
        }

        self.inner
            .tenant_api_key_lookup
            .write()
            .await
            .remove(&record.secret);

        let secret = format!("fgk_{}", Uuid::new_v4().simple());
        record.secret = secret.clone();
        record.view.prefix = secret[..12].to_string();
        record.view.status = TenantApiKeyStatus::Active;
        record.view.last_used_at = None;
        self.inner
            .tenant_api_key_lookup
            .write()
            .await
            .insert(secret.clone(), api_key_id);

        Ok(CreatedApiKey {
            record: record.view.clone(),
            secret,
        })
    }

    pub async fn revoke_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<TenantApiKeyView, AuthError> {
        let mut api_keys = self.inner.tenant_api_keys.write().await;
        let record = api_keys
            .get_mut(&api_key_id)
            .ok_or(AuthError::Unauthorized)?;
        if record.view.tenant_id != tenant_id {
            return Err(AuthError::Forbidden);
        }

        self.inner
            .tenant_api_key_lookup
            .write()
            .await
            .remove(&record.secret);

        record.view.status = TenantApiKeyStatus::Revoked;
        Ok(record.view.clone())
    }

    pub async fn list_tenant_models(&self, _tenant_id: Uuid) -> Vec<ModelDescriptor> {
        self.inner
            .route_groups
            .read()
            .await
            .values()
            .map(|route_group| ModelDescriptor {
                id: route_group.public_model.clone(),
                route_group: route_group.slug.clone(),
                provider_kind: route_group.provider_kind.clone(),
                upstream_model: route_group.upstream_model.clone(),
                capabilities: vec![
                    ModelCapability::Chat,
                    ModelCapability::Responses,
                    ModelCapability::Streaming,
                ],
            })
            .collect()
    }

    pub async fn usage_summary(&self, tenant_id: Uuid) -> UsageSummary {
        let requests = self.inner.requests.read().await;
        let mut summary = UsageSummary {
            total_requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            last_request_at: None,
        };

        for record in requests
            .iter()
            .filter(|record| record.tenant_id == tenant_id)
        {
            summary.total_requests += 1;
            summary.input_tokens += u64::from(record.usage.input_tokens);
            summary.output_tokens += u64::from(record.usage.output_tokens);
            summary.last_request_at = Some(
                summary
                    .last_request_at
                    .map_or(record.created_at, |previous| {
                        previous.max(record.created_at)
                    }),
            );
        }

        summary
    }

    pub async fn tenant_requests(&self, tenant_id: Uuid) -> Vec<RequestRecord> {
        self.inner
            .requests
            .read()
            .await
            .iter()
            .filter(|record| record.tenant_id == tenant_id)
            .cloned()
            .collect()
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
    ) {
        self.inner.requests.write().await.push(RequestRecord {
            id: Uuid::new_v4(),
            tenant_id,
            api_key_id,
            public_model,
            provider_kind,
            status_code,
            latency_ms,
            usage,
            created_at: Utc::now(),
        });
    }

    pub async fn record_audit(
        &self,
        actor: impl Into<String>,
        action: impl Into<String>,
        resource: impl Into<String>,
        request_id: impl Into<String>,
        details: Value,
    ) {
        self.inner.audits.write().await.push(AuditEvent {
            id: Uuid::new_v4(),
            actor: actor.into(),
            action: action.into(),
            resource: resource.into(),
            request_id: request_id.into(),
            occurred_at: Utc::now(),
            details,
        });
    }

    pub async fn list_audit_events(&self) -> Vec<AuditEvent> {
        self.inner.audits.read().await.clone()
    }

    pub async fn list_provider_accounts(&self) -> Vec<ProviderAccountRecord> {
        self.inner
            .provider_accounts
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    pub async fn ingest_provider_account(
        &self,
        envelope: ProviderAccountEnvelope,
        validated: ValidatedProviderAccount,
        capabilities: AccountCapabilities,
    ) -> ProviderAccountRecord {
        let metadata = envelope.metadata.clone();
        let plan_type = metadata
            .get("plan_type")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let record = ProviderAccountRecord {
            id: Uuid::new_v4(),
            provider: envelope.provider,
            credential_kind: envelope.credential_kind,
            payload_version: envelope.payload_version,
            state: AccountState::Active,
            external_account_id: validated.provider_account_id,
            redacted_display: validated.redacted_display,
            plan_type,
            metadata,
            labels: envelope.labels,
            tags: envelope.tags,
            capabilities: capabilities
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect(),
            expires_at: validated.expires_at,
            last_validated_at: Some(Utc::now()),
            created_at: Utc::now(),
        };

        self.inner
            .runtimes
            .write()
            .await
            .insert(record.id, AccountRuntime::new(AccountState::Active, 16));
        self.inner
            .provider_accounts
            .write()
            .await
            .insert(record.id, record.clone());

        record
    }

    pub async fn set_provider_account_state(
        &self,
        account_id: Uuid,
        state: AccountState,
    ) -> Option<ProviderAccountRecord> {
        let mut accounts = self.inner.provider_accounts.write().await;
        let record = accounts.get_mut(&account_id)?;
        record.state = state.clone();
        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.state = state;
        }
        Some(record.clone())
    }

    pub async fn create_route_group(
        &self,
        public_model: String,
        provider_kind: String,
        upstream_model: String,
    ) -> RouteGroupRecord {
        let slug = public_model.replace('.', "-");
        let record = RouteGroupRecord {
            id: Uuid::new_v4(),
            slug,
            public_model,
            provider_kind,
            upstream_model,
            created_at: Utc::now(),
        };
        self.inner
            .route_groups
            .write()
            .await
            .insert(record.id, record.clone());
        record
    }

    pub async fn list_route_groups(&self) -> Vec<RouteGroupRecord> {
        self.inner
            .route_groups
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    pub async fn bind_provider_account(
        &self,
        route_group_id: Uuid,
        provider_account_id: Uuid,
        weight: u32,
        max_in_flight: u32,
    ) -> Result<RouteGroupBindingRecord, AuthError> {
        if !self
            .inner
            .route_groups
            .read()
            .await
            .contains_key(&route_group_id)
            || !self
                .inner
                .provider_accounts
                .read()
                .await
                .contains_key(&provider_account_id)
        {
            return Err(AuthError::Unauthorized);
        }

        let record = RouteGroupBindingRecord {
            id: Uuid::new_v4(),
            route_group_id,
            provider_account_id,
            weight,
            max_in_flight,
            created_at: Utc::now(),
        };
        self.inner
            .runtimes
            .write()
            .await
            .entry(provider_account_id)
            .and_modify(|runtime| runtime.max_in_flight = max_in_flight)
            .or_insert_with(|| AccountRuntime::new(AccountState::Active, max_in_flight));
        self.inner
            .route_group_bindings
            .write()
            .await
            .insert(record.id, record.clone());
        Ok(record)
    }

    pub async fn resolve_route_group(&self, public_model: &str) -> Option<RouteGroupRecord> {
        self.inner
            .route_groups
            .read()
            .await
            .values()
            .find(|route_group| route_group.public_model == public_model)
            .cloned()
    }

    pub async fn scheduler_candidates(&self, public_model: &str) -> Vec<ProviderAccountCandidate> {
        let Some(route_group) = self.resolve_route_group(public_model).await else {
            return Vec::new();
        };
        let bindings = self.inner.route_group_bindings.read().await;
        let runtimes = self.inner.runtimes.read().await;
        let accounts = self.inner.provider_accounts.read().await;

        bindings
            .values()
            .filter(|binding| binding.route_group_id == route_group.id)
            .filter_map(|binding| {
                let runtime = runtimes.get(&binding.provider_account_id)?.clone();
                let account = accounts.get(&binding.provider_account_id)?;
                Some(ProviderAccountCandidate {
                    account_id: binding.provider_account_id,
                    route_group_id: binding.route_group_id,
                    provider_kind: account.provider.clone(),
                    weight: binding.weight,
                    runtime,
                })
            })
            .collect()
    }

    pub async fn mark_scheduler_outcome(&self, account_id: Uuid, outcome: ProviderOutcome) {
        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.apply_outcome(outcome.clone(), Utc::now());
            if let Some(account) = self
                .inner
                .provider_accounts
                .write()
                .await
                .get_mut(&account_id)
            {
                account.state = runtime.state.clone();
            }
        }
    }

    pub async fn choose_candidate(&self, public_model: &str) -> Option<ProviderAccountCandidate> {
        let candidates = self.scheduler_candidates(public_model).await;
        let selected = select_candidate(Utc::now(), &candidates)?;
        candidates
            .into_iter()
            .find(|candidate| candidate.account_id == selected.account_id)
    }
}

fn role_allows(role: &Role, permission: &Permission) -> bool {
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

fn scope_allows(scopes: &[ScopeTarget], target: &ScopeTarget) -> bool {
    scopes.iter().any(|scope| match (scope, target) {
        (ScopeTarget::Global, _) => true,
        (ScopeTarget::ProviderPool(left), ScopeTarget::ProviderPool(right)) => left == right,
        (ScopeTarget::RouteGroup(left), ScopeTarget::RouteGroup(right)) => left == right,
        (ScopeTarget::Tenant(left), ScopeTarget::Tenant(right)) => left == right,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};

    #[tokio::test]
    async fn create_rotate_and_revoke_tenant_key() {
        let store = InMemoryPlatformStore::demo();
        let tenant_id = store.list_tenants().await[0].id;

        let created = store
            .create_tenant_api_key(tenant_id, "integration".to_string())
            .await
            .expect("key should be created");
        assert!(
            store
                .validate_gateway_api_key(&created.secret)
                .await
                .is_some()
        );

        let rotated = store
            .rotate_tenant_api_key(tenant_id, created.record.id)
            .await
            .expect("key should rotate");
        assert!(
            store
                .validate_gateway_api_key(&created.secret)
                .await
                .is_none()
        );
        assert!(
            store
                .validate_gateway_api_key(&rotated.secret)
                .await
                .is_some()
        );

        let revoked = store
            .revoke_tenant_api_key(tenant_id, created.record.id)
            .await
            .expect("key should revoke");
        assert_eq!(revoked.status, TenantApiKeyStatus::Revoked);
    }

    #[tokio::test]
    async fn scope_authorization_blocks_missing_permissions() {
        let store = InMemoryPlatformStore::demo();
        let result = store
            .authorize_control(
                "fg_cp_routing_demo",
                Permission::ImportProviderAccounts,
                ScopeTarget::ProviderPool("openai_codex".to_string()),
            )
            .await;

        assert_eq!(
            result.expect_err("should be forbidden"),
            AuthError::Forbidden
        );
    }

    #[tokio::test]
    async fn ingest_provider_account_becomes_active() {
        let store = InMemoryPlatformStore::demo();
        let record = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token"}),
                    metadata: json!({"plan_type":"plus"}),
                    labels: vec!["shared".to_string()],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_new".to_string(),
                    redacted_display: Some("n***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![],
                    supports_refresh: true,
                    supports_quota_probe: true,
                },
            )
            .await;

        assert_eq!(record.state, AccountState::Active);
    }
}
