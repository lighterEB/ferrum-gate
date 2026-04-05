use crate::{
    AuditEvent, AuthError, CreatedApiKey, GatewayAuthContext, Permission, ProviderAccountRecord,
    RequestRecord, Role, RouteGroupBindingRecord, RouteGroupRecord, ScopeTarget,
    ServiceAccountPrincipal, StoreError, Tenant, TenantApiKeyStatus, TenantApiKeyView,
    TenantManagementPrincipal, UsageSummary, provider_connection_from_parts, role_allows,
    scope_allows,
};
use chrono::Utc;
use protocol_core::{ModelCapability, ModelDescriptor, TokenUsage};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderConnectionInfo, ValidatedProviderAccount,
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
            runtimes: RwLock::new(HashMap::new()),
            requests: RwLock::new(Vec::new()),
            audits: RwLock::new(Vec::new()),
        };

        Self {
            inner: Arc::new(inner),
        }
    }

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
            provider_accounts: RwLock::new(BTreeMap::from([(
                provider_account_id,
                provider_account,
            )])),
            provider_credentials: RwLock::new(HashMap::from([(
                provider_account_id,
                json!({
                    "access_token": "demo-access-token",
                    "account_id": "acct_demo_openai_codex",
                    "api_base": "https://api.openai.com/v1"
                }),
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

    pub async fn list_tenant_models(
        &self,
        _tenant_id: Uuid,
    ) -> Result<Vec<ModelDescriptor>, StoreError> {
        let route_groups = self.inner.route_groups.read().await;
        let bindings = self.inner.route_group_bindings.read().await;
        let accounts = self.inner.provider_accounts.read().await;

        Ok(route_groups
            .values()
            .filter(|route_group| {
                bindings.values().any(|binding| {
                    if binding.route_group_id != route_group.id {
                        return false;
                    }

                    accounts
                        .get(&binding.provider_account_id)
                        .map(|account| {
                            account.provider == route_group.provider_kind
                                && account
                                    .capabilities
                                    .iter()
                                    .any(|model| model == &route_group.upstream_model)
                        })
                        .unwrap_or(false)
                })
            })
            .map(|route_group| ModelDescriptor {
                id: route_group.public_model.clone(),
                route_group: route_group.slug.clone(),
                provider_kind: route_group.provider_kind.clone(),
                upstream_model: route_group.upstream_model.clone(),
                capabilities: vec![
                    ModelCapability::Chat,
                    ModelCapability::Responses,
                    ModelCapability::Streaming,
                    ModelCapability::Tools,
                ],
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

        Ok(record)
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

    pub async fn create_route_group(
        &self,
        public_model: String,
        provider_kind: String,
        upstream_model: String,
    ) -> Result<RouteGroupRecord, StoreError> {
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
        Ok(record)
    }

    pub async fn list_route_groups(&self) -> Result<Vec<RouteGroupRecord>, StoreError> {
        Ok(self
            .inner
            .route_groups
            .read()
            .await
            .values()
            .cloned()
            .collect())
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
        let Some(route_group) = self.resolve_route_group(public_model).await? else {
            return Ok(Vec::new());
        };
        let bindings = self.inner.route_group_bindings.read().await;
        let runtimes = self.inner.runtimes.read().await;
        let accounts = self.inner.provider_accounts.read().await;

        Ok(bindings
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
            .create_tenant_api_key(tenant_id, "integration".to_string())
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
}
