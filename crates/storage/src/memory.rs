use crate::{
    AccountInspectionRecord, AccountInspectionStatus, AlertDeliveryReceipt, AuditEvent, AuthError,
    CreatedApiKey, GatewayAuthContext, Permission, ProbeDispatchLease,
    ProviderAccountQuotaSnapshotRecord, ProviderAccountRecord, RefreshDispatchLease, RequestRecord,
    Role, RouteGroupBindingRecord, RouteGroupFallbackRecord, RouteGroupRecord, ScopeTarget,
    ServiceAccountPrincipal, StoreError, Tenant, TenantApiKeyStatus, TenantApiKeyView,
    TenantManagementPrincipal, UsageSummary, default_model_capabilities, derive_route_group_slug,
    provider_connection_from_parts, role_allows, scope_allows,
};
use chrono::{DateTime, TimeDelta, Utc};
use protocol_core::{ModelDescriptor, TokenUsage};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderConnectionInfo, QuotaSnapshot,
    ValidatedProviderAccount,
};
use scheduler::{
    AccountRuntime, AccountState, ProviderAccountCandidate, ProviderOutcome, select_candidate,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug)]
struct TenantApiKeyRecord {
    view: TenantApiKeyView,
    secret: String,
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
    provider_credentials: RwLock<HashMap<Uuid, Value>>,
    route_groups: RwLock<BTreeMap<Uuid, RouteGroupRecord>>,
    route_group_bindings: RwLock<BTreeMap<Uuid, RouteGroupBindingRecord>>,
    route_group_fallbacks: RwLock<Vec<RouteGroupFallbackRecord>>,
    runtimes: RwLock<HashMap<Uuid, AccountRuntime>>,
    requests: RwLock<Vec<RequestRecord>>,
    audits: RwLock<Vec<AuditEvent>>,
    alert_deliveries: RwLock<Vec<AlertDeliveryReceipt>>,
    inspections: RwLock<Vec<AccountInspectionRecord>>,
    quota_snapshots: RwLock<HashMap<Uuid, ProviderAccountQuotaSnapshotRecord>>,
    probe_leases: RwLock<HashMap<Uuid, ProbeDispatchLease>>,
    refresh_leases: RwLock<HashMap<Uuid, RefreshDispatchLease>>,
}

const AUTO_BINDING_WEIGHT: u32 = 100;
const AUTO_BINDING_MAX_IN_FLIGHT: u32 = 16;

fn ensure_route_group_and_binding_in_maps(
    route_groups: &mut BTreeMap<Uuid, RouteGroupRecord>,
    bindings: &mut BTreeMap<Uuid, RouteGroupBindingRecord>,
    runtimes: &mut HashMap<Uuid, AccountRuntime>,
    provider_kind: &str,
    upstream_model: &str,
    account_id: Uuid,
) {
    let route_group_id = route_groups
        .values()
        .find(|route_group| {
            route_group.public_model == upstream_model && route_group.provider_kind == provider_kind
        })
        .map(|route_group| route_group.id)
        .unwrap_or_else(|| {
            let record = RouteGroupRecord {
                id: Uuid::new_v4(),
                slug: derive_route_group_slug(provider_kind, upstream_model),
                public_model: upstream_model.to_string(),
                provider_kind: provider_kind.to_string(),
                upstream_model: upstream_model.to_string(),
                created_at: Utc::now(),
            };
            let id = record.id;
            route_groups.insert(record.id, record);
            id
        });

    if bindings.values().any(|binding| {
        binding.route_group_id == route_group_id && binding.provider_account_id == account_id
    }) {
        return;
    }

    runtimes
        .entry(account_id)
        .or_insert_with(|| AccountRuntime::new(AccountState::Active, AUTO_BINDING_MAX_IN_FLIGHT));
    let binding = RouteGroupBindingRecord {
        id: Uuid::new_v4(),
        route_group_id,
        provider_account_id: account_id,
        weight: AUTO_BINDING_WEIGHT,
        max_in_flight: AUTO_BINDING_MAX_IN_FLIGHT,
        created_at: Utc::now(),
    };
    bindings.insert(binding.id, binding);
}

impl Default for InMemoryPlatformStore {
    fn default() -> Self {
        Self::demo()
    }
}

impl InMemoryPlatformStore {
    #[must_use]
    pub fn empty() -> Self {
        let inner = InnerStore {
            tenants: RwLock::new(BTreeMap::new()),
            tenant_api_keys: RwLock::new(BTreeMap::new()),
            tenant_api_key_lookup: RwLock::new(HashMap::new()),
            tenant_management_tokens: RwLock::new(HashMap::new()),
            service_accounts: RwLock::new(HashMap::new()),
            provider_accounts: RwLock::new(BTreeMap::new()),
            provider_credentials: RwLock::new(HashMap::new()),
            route_groups: RwLock::new(BTreeMap::new()),
            route_group_bindings: RwLock::new(BTreeMap::new()),
            route_group_fallbacks: RwLock::new(Vec::new()),
            runtimes: RwLock::new(HashMap::new()),
            requests: RwLock::new(Vec::new()),
            audits: RwLock::new(Vec::new()),
            alert_deliveries: RwLock::new(Vec::new()),
            inspections: RwLock::new(Vec::new()),
            quota_snapshots: RwLock::new(HashMap::new()),
            probe_leases: RwLock::new(HashMap::new()),
            refresh_leases: RwLock::new(HashMap::new()),
        };

        Self {
            inner: Arc::new(inner),
        }
    }

    #[must_use]
    pub fn demo() -> Self {
        let tenant_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        let provider_account_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000201").expect("valid uuid");
        let qwen_account_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000202").expect("valid uuid");
        let api_key_id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000301").expect("valid uuid");
        let now = Utc::now();

        let tenant = Tenant {
            id: tenant_id,
            slug: "demo-tenant".to_string(),
            name: "Demo Tenant".to_string(),
            suspended: false,
            created_at: now,
        };

        let api_key_secret = Self::demo_gateway_key().to_string();
        let api_key = TenantApiKeyRecord {
            view: TenantApiKeyView {
                id: api_key_id,
                tenant_id,
                label: "default".to_string(),
                prefix: api_key_secret[..8].to_string(),
                status: TenantApiKeyStatus::Active,
                created_at: now,
                last_used_at: None,
                expires_at: None,
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
            capabilities: vec!["gpt-4.1-mini".to_string(), "codex-mini-latest".to_string()],
            expires_at: None,
            last_validated_at: Some(now),
            created_at: now,
        };

        // Qwen demo account
        let qwen_provider_account = ProviderAccountRecord {
            id: qwen_account_id,
            provider: "qwen".to_string(),
            credential_kind: "oauth".to_string(),
            payload_version: "v1".to_string(),
            state: AccountState::Active,
            external_account_id: "acct_demo_qwen".to_string(),
            redacted_display: None,
            plan_type: None,
            metadata: json!({
                "public_model": "coder-model",
                "upstream_model": "coder-model",
                "api_base": "https://portal.qwen.ai/v1"
            }),
            labels: vec!["shared".to_string(), "prod".to_string()],
            tags: BTreeMap::from([("region".to_string(), "global".to_string())]),
            capabilities: vec![
                "qwen3-coder-plus".to_string(),
                "qwen3-coder-flash".to_string(),
                "qwen3.5-plus".to_string(),
                "qwen3.6-plus".to_string(),
                "coder-model".to_string(),
            ],
            expires_at: None,
            last_validated_at: Some(now),
            created_at: now,
        };

        let mut route_groups = BTreeMap::new();
        let mut route_group_bindings = BTreeMap::new();
        let mut runtimes = HashMap::from([
            (
                provider_account_id,
                AccountRuntime::new(AccountState::Active, AUTO_BINDING_MAX_IN_FLIGHT),
            ),
            (
                qwen_account_id,
                AccountRuntime::new(AccountState::Active, AUTO_BINDING_MAX_IN_FLIGHT),
            ),
        ]);
        for model in &provider_account.capabilities {
            ensure_route_group_and_binding_in_maps(
                &mut route_groups,
                &mut route_group_bindings,
                &mut runtimes,
                &provider_account.provider,
                model,
                provider_account_id,
            );
        }
        for model in &qwen_provider_account.capabilities {
            ensure_route_group_and_binding_in_maps(
                &mut route_groups,
                &mut route_group_bindings,
                &mut runtimes,
                &qwen_provider_account.provider,
                model,
                qwen_account_id,
            );
        }

        let inner = InnerStore {
            tenants: RwLock::new(BTreeMap::from([(tenant_id, tenant)])),
            tenant_api_keys: RwLock::new(BTreeMap::from([(api_key_id, api_key)])),
            tenant_api_key_lookup: RwLock::new(HashMap::from([(api_key_secret, api_key_id)])),
            tenant_management_tokens: RwLock::new(HashMap::from([(
                Self::demo_tenant_management_token().to_string(),
                TenantManagementPrincipal {
                    subject: "tenant-admin-demo".to_string(),
                    tenant_id,
                },
            )])),
            service_accounts: RwLock::new(HashMap::from([
                (
                    Self::demo_control_plane_token().to_string(),
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
            provider_accounts: RwLock::new(BTreeMap::from([
                (provider_account_id, provider_account),
                (qwen_account_id, qwen_provider_account),
            ])),
            provider_credentials: RwLock::new(HashMap::from([
                (
                    provider_account_id,
                    json!({
                        "access_token": "demo-access-token",
                        "account_id": "acct_demo_openai_codex",
                        "api_base": "https://api.openai.com/v1"
                    }),
                ),
                (
                    qwen_account_id,
                    json!({
                        "access_token": "vOj-SdwAD18JBCfGy4J3bkYKTZ6a7Ve8a_giUlBvlxy-A-4UH2nevA_ky-XcMXcYZgxeE-C3WK3Efm5ASYLBFg",
                        "refresh_token": "4-6JvJecdhNrRFQLjbLkwjfukU5gMjEfXC-VQQUEhAanwP21RnHvAf8dywFjrpTh7V9_BZenUaB0VJYrfiIq4tQ",
                        "resource_url": "portal.qwen.ai",
                        "api_base": "https://portal.qwen.ai/v1"
                    }),
                ),
            ])),
            route_groups: RwLock::new(route_groups),
            route_group_bindings: RwLock::new(route_group_bindings),
            route_group_fallbacks: RwLock::new(Vec::new()),
            runtimes: RwLock::new(runtimes),
            requests: RwLock::new(Vec::new()),
            audits: RwLock::new(Vec::new()),
            alert_deliveries: RwLock::new(Vec::new()),
            inspections: RwLock::new(Vec::new()),
            quota_snapshots: RwLock::new(HashMap::new()),
            probe_leases: RwLock::new(HashMap::new()),
            refresh_leases: RwLock::new(HashMap::new()),
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

    pub async fn validate_gateway_api_key(
        &self,
        secret: &str,
    ) -> Result<Option<GatewayAuthContext>, StoreError> {
        let api_key_id = self
            .inner
            .tenant_api_key_lookup
            .read()
            .await
            .get(secret)
            .copied();
        let Some(api_key_id) = api_key_id else {
            return Ok(None);
        };

        let mut api_keys = self.inner.tenant_api_keys.write().await;
        let Some(record) = api_keys.get_mut(&api_key_id) else {
            return Ok(None);
        };
        if record.view.status != TenantApiKeyStatus::Active {
            return Ok(None);
        }
        // Check expiry
        if let Some(expiry) = record.view.expires_at
            && Utc::now() >= expiry
        {
            return Ok(None);
        }
        record.view.last_used_at = Some(Utc::now());
        let tenant = self
            .inner
            .tenants
            .read()
            .await
            .get(&record.view.tenant_id)
            .cloned();
        let Some(tenant) = tenant else {
            return Ok(None);
        };
        if tenant.suspended {
            return Ok(None);
        }
        Ok(Some(GatewayAuthContext { tenant, api_key_id }))
    }

    pub async fn authenticate_tenant_management_token(
        &self,
        token: &str,
    ) -> Result<Option<TenantManagementPrincipal>, StoreError> {
        Ok(self
            .inner
            .tenant_management_tokens
            .read()
            .await
            .get(token)
            .cloned())
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

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>, StoreError> {
        Ok(self.inner.tenants.read().await.values().cloned().collect())
    }

    pub async fn create_tenant(&self, slug: String, name: String) -> Result<Tenant, StoreError> {
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
        Ok(tenant)
    }

    pub async fn list_tenant_api_keys(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantApiKeyView>, StoreError> {
        Ok(self
            .inner
            .tenant_api_keys
            .read()
            .await
            .values()
            .filter(|record| record.view.tenant_id == tenant_id)
            .map(|record| record.view.clone())
            .collect())
    }

    pub async fn create_tenant_api_key(
        &self,
        tenant_id: Uuid,
        label: String,
        expires_at: Option<DateTime<Utc>>,
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
            expires_at,
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

    pub async fn list_tenant_models(
        &self,
        _tenant_id: Uuid,
    ) -> Result<Vec<ModelDescriptor>, StoreError> {
        let accounts = self.inner.provider_accounts.read().await;
        let route_groups = self.inner.route_groups.read().await;
        let mut active_models = BTreeMap::new();

        for account in accounts.values() {
            if account.state != AccountState::Active {
                continue;
            }

            for model_id in &account.capabilities {
                active_models
                    .entry(model_id.clone())
                    .or_insert_with(|| account.provider.clone());
            }
        }

        Ok(active_models
            .into_iter()
            .map(|(model_id, provider_kind)| {
                route_groups
                    .values()
                    .find(|route_group| {
                        route_group.public_model == model_id
                            && route_group.provider_kind == provider_kind
                    })
                    .or_else(|| {
                        route_groups
                            .values()
                            .find(|route_group| route_group.public_model == model_id)
                    })
                    .map_or_else(
                        || ModelDescriptor {
                            id: model_id.clone(),
                            route_group: derive_route_group_slug(&provider_kind, &model_id),
                            provider_kind: provider_kind.clone(),
                            upstream_model: model_id.clone(),
                            capabilities: default_model_capabilities(),
                        },
                        |route_group| ModelDescriptor {
                            id: route_group.public_model.clone(),
                            route_group: route_group.slug.clone(),
                            provider_kind: route_group.provider_kind.clone(),
                            upstream_model: route_group.upstream_model.clone(),
                            capabilities: default_model_capabilities(),
                        },
                    )
            })
            .collect())
    }

    pub async fn usage_summary(&self, tenant_id: Uuid) -> Result<UsageSummary, StoreError> {
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

        Ok(summary)
    }

    pub async fn tenant_requests(&self, tenant_id: Uuid) -> Result<Vec<RequestRecord>, StoreError> {
        Ok(self
            .inner
            .requests
            .read()
            .await
            .iter()
            .filter(|record| record.tenant_id == tenant_id)
            .cloned()
            .collect())
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
        Ok(())
    }

    pub async fn record_audit(
        &self,
        actor: impl Into<String>,
        action: impl Into<String>,
        resource: impl Into<String>,
        request_id: impl Into<String>,
        details: Value,
    ) -> Result<(), StoreError> {
        self.inner.audits.write().await.push(AuditEvent {
            id: Uuid::new_v4(),
            actor: actor.into(),
            action: action.into(),
            resource: resource.into(),
            request_id: request_id.into(),
            occurred_at: Utc::now(),
            details,
        });
        Ok(())
    }

    pub async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StoreError> {
        Ok(self.inner.audits.read().await.clone())
    }

    pub async fn record_alert_delivery(
        &self,
        alert_id: Uuid,
        destination: impl Into<String>,
    ) -> Result<bool, StoreError> {
        let destination = destination.into();
        let mut deliveries = self.inner.alert_deliveries.write().await;
        if deliveries
            .iter()
            .any(|receipt| receipt.alert_id == alert_id && receipt.destination == destination)
        {
            return Ok(false);
        }

        deliveries.push(AlertDeliveryReceipt {
            id: Uuid::new_v4(),
            alert_id,
            destination,
            delivered_at: Utc::now(),
        });
        Ok(true)
    }

    pub async fn list_alert_delivery_receipts(
        &self,
        destination: &str,
    ) -> Result<Vec<AlertDeliveryReceipt>, StoreError> {
        Ok(self
            .inner
            .alert_deliveries
            .read()
            .await
            .iter()
            .filter(|receipt| receipt.destination == destination)
            .cloned()
            .collect())
    }

    pub async fn list_provider_accounts(&self) -> Result<Vec<ProviderAccountRecord>, StoreError> {
        Ok(self
            .inner
            .provider_accounts
            .read()
            .await
            .values()
            .cloned()
            .collect())
    }

    pub async fn provider_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        Ok(self
            .inner
            .provider_accounts
            .read()
            .await
            .get(&account_id)
            .cloned())
    }

    pub async fn record_account_inspection(
        &self,
        provider_account_id: Uuid,
        actor: String,
        status: AccountInspectionStatus,
        error_kind: Option<String>,
        error_code: Option<String>,
        error_message: Option<String>,
    ) -> Result<AccountInspectionRecord, StoreError> {
        let record = AccountInspectionRecord {
            id: Uuid::new_v4(),
            provider_account_id,
            actor,
            status,
            error_kind,
            error_code,
            error_message,
            inspected_at: Utc::now(),
        };
        self.inner.inspections.write().await.push(record.clone());
        Ok(record)
    }

    pub async fn list_account_inspections(
        &self,
        provider_account_id: Uuid,
    ) -> Result<Vec<AccountInspectionRecord>, StoreError> {
        let mut inspections = self
            .inner
            .inspections
            .read()
            .await
            .iter()
            .filter(|inspection| inspection.provider_account_id == provider_account_id)
            .cloned()
            .collect::<Vec<_>>();
        inspections.sort_by(|left, right| {
            right
                .inspected_at
                .cmp(&left.inspected_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(inspections)
    }

    pub async fn upsert_provider_account_quota_snapshot(
        &self,
        provider_account_id: Uuid,
        snapshot: QuotaSnapshot,
    ) -> Result<ProviderAccountQuotaSnapshotRecord, StoreError> {
        let record = ProviderAccountQuotaSnapshotRecord {
            provider_account_id,
            plan_label: snapshot.plan_label,
            remaining_requests_hint: snapshot.remaining_requests_hint,
            details: snapshot.details.unwrap_or_else(|| json!({})),
            checked_at: snapshot.checked_at,
        };
        self.inner
            .quota_snapshots
            .write()
            .await
            .insert(provider_account_id, record.clone());
        Ok(record)
    }

    pub async fn provider_account_quota_snapshot(
        &self,
        provider_account_id: Uuid,
    ) -> Result<Option<ProviderAccountQuotaSnapshotRecord>, StoreError> {
        Ok(self
            .inner
            .quota_snapshots
            .read()
            .await
            .get(&provider_account_id)
            .cloned())
    }

    pub async fn provider_account_envelope(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderAccountEnvelope>, StoreError> {
        let account = self
            .inner
            .provider_accounts
            .read()
            .await
            .get(&account_id)
            .cloned();
        let credentials = self
            .inner
            .provider_credentials
            .read()
            .await
            .get(&account_id)
            .cloned();

        Ok(match (account, credentials) {
            (Some(account), Some(credentials)) => Some(ProviderAccountEnvelope {
                provider: account.provider,
                credential_kind: account.credential_kind,
                payload_version: account.payload_version,
                credentials,
                metadata: account.metadata,
                labels: account.labels,
                tags: account.tags,
            }),
            _ => None,
        })
    }

    pub async fn ingest_provider_account(
        &self,
        envelope: ProviderAccountEnvelope,
        validated: ValidatedProviderAccount,
        capabilities: AccountCapabilities,
    ) -> Result<ProviderAccountRecord, StoreError> {
        let credentials = envelope.credentials.clone();
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
        self.inner
            .provider_credentials
            .write()
            .await
            .insert(record.id, credentials);
        for model_id in &record.capabilities {
            self.ensure_route_group_and_binding(&record.provider, model_id, record.id)
                .await?;
        }

        Ok(record)
    }

    pub async fn revalidate_provider_account(
        &self,
        account_id: Uuid,
        validated: ValidatedProviderAccount,
        capabilities: AccountCapabilities,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let mut accounts = self.inner.provider_accounts.write().await;
        let Some(record) = accounts.get_mut(&account_id) else {
            return Ok(None);
        };

        record.state = AccountState::Active;
        record.external_account_id = validated.provider_account_id;
        record.redacted_display = validated.redacted_display;
        record.capabilities = capabilities
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect();
        record.expires_at = validated.expires_at;
        record.last_validated_at = Some(Utc::now());

        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.state = AccountState::Active;
            runtime.cooldown_until = None;
            runtime.circuit_open_until = None;
            runtime.consecutive_failures = 0;
        }

        let provider_kind = record.provider.clone();
        let capability_ids = record.capabilities.clone();
        let updated_record = record.clone();
        drop(accounts);
        for model_id in &capability_ids {
            self.ensure_route_group_and_binding(&provider_kind, model_id, account_id)
                .await?;
        }

        Ok(Some(updated_record))
    }

    pub async fn rotate_provider_account_secret(
        &self,
        account_id: Uuid,
        credentials: Value,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let mut accounts = self.inner.provider_accounts.write().await;
        let Some(record) = accounts.get_mut(&account_id) else {
            return Ok(None);
        };

        record.state = AccountState::Active;
        record.expires_at = expires_at;
        record.last_validated_at = Some(Utc::now());

        self.inner
            .provider_credentials
            .write()
            .await
            .insert(account_id, credentials);

        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.state = AccountState::Active;
            runtime.cooldown_until = None;
            runtime.circuit_open_until = None;
            runtime.consecutive_failures = 0;
        }

        Ok(Some(record.clone()))
    }

    pub async fn set_provider_account_state(
        &self,
        account_id: Uuid,
        state: AccountState,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let mut accounts = self.inner.provider_accounts.write().await;
        let Some(record) = accounts.get_mut(&account_id) else {
            return Ok(None);
        };
        record.state = state.clone();
        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.state = state;
        }
        Ok(Some(record.clone()))
    }

    /// Physically deletes a provider account and all associated data.
    /// Only accounts in `Disabled` or `InvalidCredentials` state can be deleted.
    /// Returns `Ok(true)` if the account was deleted, `Ok(false)` if it didn't exist.
    pub async fn delete_provider_account(&self, account_id: Uuid) -> Result<bool, StoreError> {
        // Check state first
        {
            let accounts = self.inner.provider_accounts.read().await;
            let Some(record) = accounts.get(&account_id) else {
                return Ok(false);
            };
            match &record.state {
                AccountState::Disabled | AccountState::InvalidCredentials => {}
                other => {
                    return Err(StoreError::Backend(format!(
                        "account must be in Disabled or InvalidCredentials state to delete, currently: {other:?}"
                    )));
                }
            }
        }

        // Remove from all stores
        self.inner
            .provider_accounts
            .write()
            .await
            .remove(&account_id);
        self.inner
            .provider_credentials
            .write()
            .await
            .remove(&account_id);
        self.inner.runtimes.write().await.remove(&account_id);

        // Remove route group bindings
        {
            let mut bindings = self.inner.route_group_bindings.write().await;
            bindings.retain(|_, b| b.provider_account_id != account_id);
        }

        // Remove quota snapshots
        self.inner.quota_snapshots.write().await.remove(&account_id);

        // Remove probe/refresh leases
        self.inner.probe_leases.write().await.remove(&account_id);
        self.inner.refresh_leases.write().await.remove(&account_id);

        Ok(true)
    }

    pub async fn create_route_group(
        &self,
        public_model: String,
        provider_kind: String,
        upstream_model: String,
    ) -> Result<RouteGroupRecord, StoreError> {
        let mut route_groups = self.inner.route_groups.write().await;
        let slug = derive_route_group_slug(&provider_kind, &public_model);
        let record = if let Some(existing) = route_groups.values_mut().find(|route_group| {
            route_group.public_model == public_model && route_group.provider_kind == provider_kind
        }) {
            existing.slug = slug;
            existing.upstream_model = upstream_model;
            existing.clone()
        } else {
            let record = RouteGroupRecord {
                id: Uuid::new_v4(),
                slug,
                public_model,
                provider_kind,
                upstream_model,
                created_at: Utc::now(),
            };
            route_groups.insert(record.id, record.clone());
            record
        };
        Ok(record)
    }

    pub async fn ensure_route_group_and_binding(
        &self,
        provider_kind: &str,
        upstream_model: &str,
        account_id: Uuid,
    ) -> Result<(), StoreError> {
        let mut route_groups = self.inner.route_groups.write().await;
        let mut runtimes = self.inner.runtimes.write().await;
        let mut bindings = self.inner.route_group_bindings.write().await;
        ensure_route_group_and_binding_in_maps(
            &mut route_groups,
            &mut bindings,
            &mut runtimes,
            provider_kind,
            upstream_model,
            account_id,
        );
        Ok(())
    }

    pub async fn list_route_groups(&self) -> Result<Vec<RouteGroupRecord>, StoreError> {
        let mut route_groups = self
            .inner
            .route_groups
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        route_groups.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(route_groups)
    }

    pub async fn list_route_group_bindings(
        &self,
    ) -> Result<Vec<RouteGroupBindingRecord>, StoreError> {
        let mut bindings = self
            .inner
            .route_group_bindings
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        bindings.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(bindings)
    }

    pub async fn add_route_group_fallback(
        &self,
        route_group_id: Uuid,
        fallback_route_group_id: Uuid,
        position: u32,
    ) -> Result<RouteGroupFallbackRecord, StoreError> {
        let record = RouteGroupFallbackRecord {
            route_group_id,
            fallback_route_group_id,
            position,
            created_at: Utc::now(),
        };
        let mut fallbacks = self.inner.route_group_fallbacks.write().await;
        if let Some(existing) = fallbacks.iter_mut().find(|existing| {
            existing.route_group_id == route_group_id
                && existing.fallback_route_group_id == fallback_route_group_id
        }) {
            existing.position = position;
            return Ok(existing.clone());
        }
        fallbacks.push(record.clone());
        Ok(record)
    }

    pub async fn list_route_group_fallbacks(
        &self,
        route_group_id: Uuid,
    ) -> Result<Vec<RouteGroupFallbackRecord>, StoreError> {
        let mut fallbacks = self
            .inner
            .route_group_fallbacks
            .read()
            .await
            .iter()
            .filter(|record| record.route_group_id == route_group_id)
            .cloned()
            .collect::<Vec<_>>();
        fallbacks.sort_by(|left, right| {
            left.position.cmp(&right.position).then(
                left.fallback_route_group_id
                    .cmp(&right.fallback_route_group_id),
            )
        });
        Ok(fallbacks)
    }

    pub async fn list_all_route_group_fallbacks(
        &self,
    ) -> Result<Vec<RouteGroupFallbackRecord>, StoreError> {
        let mut fallbacks = self
            .inner
            .route_group_fallbacks
            .read()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        fallbacks.sort_by(|left, right| {
            left.route_group_id
                .cmp(&right.route_group_id)
                .then(left.position.cmp(&right.position))
                .then(
                    left.fallback_route_group_id
                        .cmp(&right.fallback_route_group_id),
                )
        });
        Ok(fallbacks)
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
        let mut bindings = self.inner.route_group_bindings.write().await;
        if let Some(existing) = bindings.values_mut().find(|binding| {
            binding.route_group_id == route_group_id
                && binding.provider_account_id == provider_account_id
        }) {
            existing.weight = weight;
            existing.max_in_flight = max_in_flight;
            return Ok(existing.clone());
        }

        bindings.insert(record.id, record.clone());
        Ok(record)
    }

    pub async fn resolve_route_group(
        &self,
        public_model: &str,
    ) -> Result<Option<RouteGroupRecord>, StoreError> {
        Ok(self
            .inner
            .route_groups
            .read()
            .await
            .values()
            .find(|route_group| route_group.public_model == public_model)
            .cloned())
    }

    pub async fn scheduler_candidates(
        &self,
        public_model: &str,
    ) -> Result<Vec<ProviderAccountCandidate>, StoreError> {
        let route_group_ids = self
            .inner
            .route_groups
            .read()
            .await
            .values()
            .filter(|route_group| route_group.public_model == public_model)
            .map(|route_group| route_group.id)
            .collect::<Vec<_>>();
        if route_group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let bindings = self.inner.route_group_bindings.read().await;
        let runtimes = self.inner.runtimes.read().await;
        let accounts = self.inner.provider_accounts.read().await;

        Ok(bindings
            .values()
            .filter(|binding| route_group_ids.contains(&binding.route_group_id))
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
            .collect())
    }

    pub async fn mark_scheduler_outcome(
        &self,
        account_id: Uuid,
        outcome: ProviderOutcome,
    ) -> Result<(), StoreError> {
        if let Some(runtime) = self.inner.runtimes.write().await.get_mut(&account_id) {
            runtime.apply_outcome(outcome, Utc::now());
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
        Ok(())
    }

    pub async fn dispatch_due_provider_account_probes(
        &self,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchLease>, StoreError> {
        let now = Utc::now();
        let accounts = self.inner.provider_accounts.read().await;
        let mut leases = self.inner.probe_leases.write().await;

        leases.retain(|_, lease| lease.leased_until > now);

        let mut due_accounts = accounts
            .values()
            .filter(|account| {
                matches!(
                    account.state,
                    AccountState::Active | AccountState::Cooling | AccountState::QuotaExhausted
                ) && !leases.contains_key(&account.id)
            })
            .cloned()
            .collect::<Vec<_>>();

        due_accounts.sort_by(|left, right| {
            left.last_validated_at
                .cmp(&right.last_validated_at)
                .then(left.created_at.cmp(&right.created_at))
                .then(left.id.cmp(&right.id))
        });

        let selected = due_accounts
            .into_iter()
            .take(limit)
            .map(|account| {
                let lease = ProbeDispatchLease {
                    lease_id: Uuid::new_v4(),
                    account_id: account.id,
                    leased_at: now,
                    leased_until: now + TimeDelta::minutes(5),
                };
                leases.insert(account.id, lease.clone());
                lease
            })
            .collect();

        Ok(selected)
    }

    pub async fn dispatch_due_provider_account_refreshes(
        &self,
        limit: usize,
        refresh_before_seconds: i64,
    ) -> Result<Vec<RefreshDispatchLease>, StoreError> {
        let now = Utc::now();
        let refresh_before =
            TimeDelta::try_seconds(refresh_before_seconds.max(0)).unwrap_or_else(TimeDelta::zero);
        let due_before = now + refresh_before;
        let accounts = self.inner.provider_accounts.read().await;
        let credentials = self.inner.provider_credentials.read().await;
        let mut leases = self.inner.refresh_leases.write().await;

        leases.retain(|_, lease| lease.leased_until > now);

        let mut due_accounts = accounts
            .values()
            .filter(|account| {
                matches!(
                    account.state,
                    AccountState::Active | AccountState::Cooling | AccountState::QuotaExhausted
                ) && account.credential_kind == "oauth_tokens"
                    && account
                        .expires_at
                        .is_some_and(|expires_at| expires_at <= due_before)
                    && !leases.contains_key(&account.id)
                    && credentials
                        .get(&account.id)
                        .and_then(|value| value.get("refresh_token"))
                        .and_then(Value::as_str)
                        .is_some()
            })
            .cloned()
            .collect::<Vec<_>>();

        due_accounts.sort_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then(left.last_validated_at.cmp(&right.last_validated_at))
                .then(left.created_at.cmp(&right.created_at))
                .then(left.id.cmp(&right.id))
        });

        let selected = due_accounts
            .into_iter()
            .take(limit)
            .map(|account| {
                let lease = RefreshDispatchLease {
                    lease_id: Uuid::new_v4(),
                    account_id: account.id,
                    leased_at: now,
                    leased_until: now + TimeDelta::minutes(5),
                };
                leases.insert(account.id, lease.clone());
                lease
            })
            .collect();

        Ok(selected)
    }

    pub async fn choose_candidate(
        &self,
        public_model: &str,
    ) -> Result<Option<ProviderAccountCandidate>, StoreError> {
        let candidates = self.scheduler_candidates(public_model).await?;
        let Some(selected) = select_candidate(Utc::now(), &candidates) else {
            return Ok(None);
        };
        Ok(candidates
            .into_iter()
            .find(|candidate| candidate.account_id == selected.account_id))
    }

    pub async fn resolve_provider_connection(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderConnectionInfo>, StoreError> {
        let account = self
            .inner
            .provider_accounts
            .read()
            .await
            .get(&account_id)
            .cloned();
        let credentials = self
            .inner
            .provider_credentials
            .read()
            .await
            .get(&account_id)
            .cloned();

        match (account, credentials) {
            (Some(account), Some(credentials)) => provider_connection_from_parts(
                account.id,
                &account.provider,
                &account.credential_kind,
                &account.metadata,
                &credentials,
            )
            .map(Some),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol_core::{ModelCapability, ModelDescriptor};
    use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};

    #[tokio::test]
    async fn create_rotate_and_revoke_tenant_key() {
        let store = InMemoryPlatformStore::demo();
        let tenant_id = store
            .list_tenants()
            .await
            .expect("tenants")
            .first()
            .expect("tenant")
            .id;

        let created = store
            .create_tenant_api_key(tenant_id, "integration".to_string(), None)
            .await
            .expect("key should be created");
        assert!(
            store
                .validate_gateway_api_key(&created.secret)
                .await
                .expect("auth")
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
                .expect("auth")
                .is_none()
        );
        assert!(
            store
                .validate_gateway_api_key(&rotated.secret)
                .await
                .expect("auth")
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
            .await
            .expect("ingest");

        assert_eq!(record.state, AccountState::Active);
    }

    #[tokio::test]
    async fn ingest_provider_account_auto_creates_route_group_and_binding() {
        let store = InMemoryPlatformStore::empty();
        let record = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token"}),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_auto".to_string(),
                    redacted_display: Some("a***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-4.1-mini".to_string(),
                        route_group: "gpt-4.1-mini".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-4.1-mini".to_string(),
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
            .expect("ingest");

        let route_groups = store.list_route_groups().await.expect("route groups");
        assert!(route_groups.iter().any(|route_group| {
            route_group.public_model == "gpt-4.1-mini"
                && route_group.provider_kind == "openai_codex"
                && route_group.upstream_model == "gpt-4.1-mini"
        }));

        let candidates = store
            .scheduler_candidates("gpt-4.1-mini")
            .await
            .expect("scheduler candidates");
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.account_id == record.id)
        );
    }

    #[tokio::test]
    async fn revalidate_provider_account_auto_creates_route_group_for_new_capability() {
        let store = InMemoryPlatformStore::empty();
        let record = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token"}),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_auto".to_string(),
                    redacted_display: Some("a***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-4.1-mini".to_string(),
                        route_group: "gpt-4.1-mini".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-4.1-mini".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("ingest");

        store
            .revalidate_provider_account(
                record.id,
                ValidatedProviderAccount {
                    provider_account_id: "acct_auto".to_string(),
                    redacted_display: Some("a***@***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![
                        ModelDescriptor {
                            id: "gpt-4.1-mini".to_string(),
                            route_group: "gpt-4.1-mini".to_string(),
                            provider_kind: "openai_codex".to_string(),
                            upstream_model: "gpt-4.1-mini".to_string(),
                            capabilities: vec![ModelCapability::Chat],
                        },
                        ModelDescriptor {
                            id: "codex-mini-latest".to_string(),
                            route_group: "codex-mini-latest".to_string(),
                            provider_kind: "openai_codex".to_string(),
                            upstream_model: "codex-mini-latest".to_string(),
                            capabilities: vec![ModelCapability::Responses],
                        },
                    ],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("revalidate")
            .expect("record");

        let route_groups = store.list_route_groups().await.expect("route groups");
        assert!(route_groups.iter().any(|route_group| {
            route_group.public_model == "codex-mini-latest"
                && route_group.provider_kind == "openai_codex"
        }));
    }

    #[tokio::test]
    async fn list_tenant_models_aggregates_active_capabilities_without_duplicates() {
        let store = InMemoryPlatformStore::empty();
        let tenant_id = store
            .create_tenant("demo".to_string(), "Demo".to_string())
            .await
            .expect("tenant")
            .id;

        let first = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token-a"}),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_a".to_string(),
                    redacted_display: None,
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-4.1-mini".to_string(),
                        route_group: "gpt-4.1-mini".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-4.1-mini".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("first account");
        store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token-b"}),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_b".to_string(),
                    redacted_display: None,
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-4.1-mini".to_string(),
                        route_group: "gpt-4.1-mini".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-4.1-mini".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("second account");
        let inactive = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({"access_token":"token-c"}),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_c".to_string(),
                    redacted_display: None,
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "gpt-5".to_string(),
                        route_group: "gpt-5".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-5".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("inactive account");
        store
            .set_provider_account_state(inactive.id, AccountState::Disabled)
            .await
            .expect("state change");

        let models = store
            .list_tenant_models(tenant_id)
            .await
            .expect("tenant models");
        assert_eq!(
            models
                .iter()
                .filter(|model| model.id == "gpt-4.1-mini")
                .count(),
            1
        );
        assert!(models.iter().any(|model| model.id == "gpt-4.1-mini"));
        assert!(models.iter().all(|model| model.id != "gpt-5"));

        let candidates = store
            .scheduler_candidates("gpt-4.1-mini")
            .await
            .expect("candidates");
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.account_id == first.id)
        );
    }

    #[tokio::test]
    async fn resolve_provider_connection_returns_bearer_and_base() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        let connection = store
            .resolve_provider_connection(account.id)
            .await
            .expect("connection")
            .expect("provider connection");

        assert_eq!(connection.api_base, "https://api.openai.com/v1");
        assert_eq!(connection.provider_kind, "openai_codex");
        assert_eq!(connection.bearer_token, "demo-access-token");
    }

    // ─── TDD Tests: delete_provider_account ───────────────────────────────

    // Test 2.1: delete_disabled_account
    #[tokio::test]
    async fn delete_disabled_account() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        // First disable the account
        store
            .set_provider_account_state(account.id, AccountState::Disabled)
            .await
            .expect("disable");

        // Now delete it
        let result = store.delete_provider_account(account.id).await;
        assert!(result.is_ok(), "delete disabled account should succeed");
        assert!(
            result.unwrap(),
            "delete should return true when account existed"
        );

        // Verify it's gone
        let remaining = store.list_provider_accounts().await.expect("accounts");
        assert!(
            remaining.iter().all(|a| a.id != account.id),
            "account should be removed"
        );
    }

    // Test 2.2: delete_invalid_credentials_account
    #[tokio::test]
    async fn delete_invalid_credentials_account() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        store
            .set_provider_account_state(account.id, AccountState::InvalidCredentials)
            .await
            .expect("set state");

        let result = store.delete_provider_account(account.id).await;
        assert!(result.is_ok() && result.unwrap());
    }

    // Test 2.3: delete_active_account_fails
    #[tokio::test]
    async fn delete_active_account_fails() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        let result = store.delete_provider_account(account.id).await;
        assert!(result.is_err(), "deleting active account should fail");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("must be in Disabled")
                || err.to_string().contains("InvalidCredentials"),
            "error should mention state requirement: {}",
            err
        );
    }

    // Test 2.4: delete_draining_account_fails
    #[tokio::test]
    async fn delete_draining_account_fails() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        store
            .set_provider_account_state(account.id, AccountState::Draining)
            .await
            .expect("set draining");

        let result = store.delete_provider_account(account.id).await;
        assert!(result.is_err(), "deleting draining account should fail");
    }

    // Test 2.7: delete_nonexistent_account
    #[tokio::test]
    async fn delete_nonexistent_account() {
        let store = InMemoryPlatformStore::demo();
        let fake_id = Uuid::new_v4();
        let result = store.delete_provider_account(fake_id).await;
        assert!(
            result.is_ok() && !result.unwrap(),
            "delete nonexistent account should return Ok(false)"
        );
    }

    // Test 2.6: delete_removes_bindings
    #[tokio::test]
    async fn delete_removes_runtime() {
        let store = InMemoryPlatformStore::demo();
        let account = store
            .list_provider_accounts()
            .await
            .expect("accounts")
            .into_iter()
            .next()
            .expect("demo account");

        let account_id = account.id;

        // Verify runtime exists
        {
            let runtimes = store.inner.runtimes.read().await;
            assert!(
                runtimes.contains_key(&account_id),
                "runtime should exist before delete"
            );
        }

        // Disable and delete
        store
            .set_provider_account_state(account_id, AccountState::Disabled)
            .await
            .expect("disable");
        store
            .delete_provider_account(account_id)
            .await
            .expect("delete");

        // Verify runtime is gone
        {
            let runtimes = store.inner.runtimes.read().await;
            assert!(
                !runtimes.contains_key(&account_id),
                "runtime should be removed after delete"
            );
        }

        // Verify credentials are gone
        {
            let creds = store.inner.provider_credentials.read().await;
            assert!(
                !creds.contains_key(&account_id),
                "credentials should be removed after delete"
            );
        }
    }
}
