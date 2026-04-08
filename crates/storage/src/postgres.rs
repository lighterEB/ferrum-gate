use crate::{
    AccountInspectionRecord, AccountInspectionStatus, AlertDeliveryReceipt, AuditEvent, AuthError,
    CreatedApiKey, GatewayAuthContext, Permission, ProbeDispatchLease, ProviderAccountCandidate,
    ProviderAccountQuotaSnapshotRecord, ProviderAccountRecord, RefreshDispatchLease, RequestRecord,
    Role, RouteGroupBindingRecord, RouteGroupFallbackRecord, RouteGroupRecord, ScopeTarget,
    ServiceAccountPrincipal, StoreError, Tenant, TenantApiKeyStatus, TenantApiKeyView,
    TenantManagementPrincipal, UsageSummary, default_model_capabilities, derive_route_group_slug,
    provider_connection_from_parts, role_allows, scope_allows,
};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use chrono::{TimeDelta, Utc};
use protocol_core::{ModelDescriptor, TokenUsage};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderConnectionInfo, QuotaSnapshot,
    ValidatedProviderAccount,
};
use rand::RngCore;
use scheduler::{AccountRuntime, AccountState, ProviderOutcome, select_candidate};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Executor, PgPool, Row,
    postgres::{PgPoolOptions, PgRow},
    types::Json,
};
use std::{collections::BTreeMap, env};
use uuid::Uuid;

const DEFAULT_MASTER_KEY: &str = "ferrum-gate-development-master-key";
const DEMO_PROVIDER_ACCOUNT_ID: &str = "00000000-0000-0000-0000-000000000201";
const DEMO_API_KEY_ID: &str = "00000000-0000-0000-0000-000000000301";
const DEMO_TENANT_ID: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Clone)]
pub struct PostgresPlatformStore {
    pool: PgPool,
    encryption_key: [u8; 32],
}

impl PostgresPlatformStore {
    pub async fn connect_from_env() -> Result<Self, StoreError> {
        let database_url = env::var("DATABASE_URL").map_err(|_| {
            StoreError::Backend("DATABASE_URL is required for postgres backend".to_string())
        })?;
        let master_key =
            env::var("FERRUMGATE_MASTER_KEY").unwrap_or_else(|_| DEFAULT_MASTER_KEY.to_string());
        let seed_demo_data = env_flag("FERRUMGATE_SEED_DEMO_DATA");

        Self::connect(&database_url, &master_key, seed_demo_data).await
    }

    pub async fn connect(
        database_url: &str,
        master_key: &str,
        seed_demo_data: bool,
    ) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(store_backend_error)?;

        let encryption_key = derive_encryption_key(master_key);

        let store = Self {
            pool,
            encryption_key,
        };
        store.apply_schema().await?;
        if seed_demo_data {
            store.bootstrap_demo_data().await?;
        }
        Ok(store)
    }

    async fn apply_schema(&self) -> Result<(), StoreError> {
        self.pool
            .execute(include_str!("../../../migrations/0001_initial.sql"))
            .await
            .map(|_| ())
            .map_err(store_backend_error)
    }

    async fn bootstrap_demo_data(&self) -> Result<(), StoreError> {
        let row = sqlx::query_scalar::<_, i64>("select count(*) from tenants")
            .fetch_one(&self.pool)
            .await
            .map_err(store_backend_error)?;
        if row > 0 {
            return Ok(());
        }

        let tenant_id = Uuid::parse_str(DEMO_TENANT_ID).expect("uuid");
        let provider_account_id = Uuid::parse_str(DEMO_PROVIDER_ACCOUNT_ID).expect("uuid");
        let api_key_id = Uuid::parse_str(DEMO_API_KEY_ID).expect("uuid");
        let secret_version_id = Uuid::new_v4();
        let now = Utc::now();

        sqlx::query(
            "insert into tenants (id, slug, name, suspended, created_at) values ($1, $2, $3, false, $4)",
        )
        .bind(tenant_id)
        .bind("demo-tenant")
        .bind("Demo Tenant")
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into tenant_api_keys (id, tenant_id, prefix, label, status, secret_hash, created_at)
             values ($1, $2, $3, $4, 'active', $5, $6)",
        )
        .bind(api_key_id)
        .bind(tenant_id)
        .bind("fgk_demo_")
        .bind("default")
        .bind(hash_token(crate::InMemoryPlatformStore::demo_gateway_key()))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into tenant_management_tokens (id, tenant_id, subject, token_hash, created_at)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind("tenant-admin-demo")
        .bind(hash_token(
            crate::InMemoryPlatformStore::demo_tenant_management_token(),
        ))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into service_accounts (id, subject, role, token_hash, scopes, created_at)
             values ($1, $2, $3, $4, $5, $6), ($7, $8, $9, $10, $11, $12)",
        )
        .bind(Uuid::new_v4())
        .bind("platform-admin-demo")
        .bind("platform_admin")
        .bind(hash_token(
            crate::InMemoryPlatformStore::demo_control_plane_token(),
        ))
        .bind(Json(vec![ScopeTarget::Global]))
        .bind(now)
        .bind(Uuid::new_v4())
        .bind("routing-operator-demo")
        .bind("routing_operator")
        .bind(hash_token("fg_cp_routing_demo"))
        .bind(Json(vec![ScopeTarget::Global]))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into provider_accounts
             (id, provider, credential_kind, payload_version, state, external_account_id, redacted_display, plan_type, metadata, labels, tags, capabilities, last_validated_at, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(provider_account_id)
        .bind("openai_codex")
        .bind("oauth_tokens")
        .bind("v1")
        .bind("active")
        .bind("acct_demo_openai_codex")
        .bind("d***@***")
        .bind("plus")
        .bind(json!({ "email": "demo@example.com" }))
        .bind(Json(vec!["shared".to_string(), "prod".to_string()]))
        .bind(Json(BTreeMap::from([(
            "region".to_string(),
            "global".to_string(),
        )])))
        .bind(Json(vec![
            "gpt-4.1-mini".to_string(),
            "codex-mini-latest".to_string(),
        ]))
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into provider_account_secret_versions (id, provider_account_id, cipher_text, key_version, created_at)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(secret_version_id)
        .bind(provider_account_id)
        .bind(self.encrypt_json(&json!({
            "access_token": "demo-access-token",
            "account_id": "acct_demo_openai_codex"
        }))?)
        .bind("v1")
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into account_runtime
             (provider_account_id, state, health_score, consecutive_failures, in_flight, max_in_flight)
             values ($1, 'active', 100, 0, 0, 16)",
        )
        .bind(provider_account_id)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        for model_id in ["gpt-4.1-mini", "codex-mini-latest"] {
            self.ensure_route_group_and_binding("openai_codex", model_id, provider_account_id)
                .await?;
        }

        Ok(())
    }

    pub async fn validate_gateway_api_key(
        &self,
        secret: &str,
    ) -> Result<Option<GatewayAuthContext>, StoreError> {
        let row = sqlx::query(
            "select k.id as api_key_id, t.id, t.slug, t.name, t.suspended, t.created_at
             from tenant_api_keys k
             join tenants t on t.id = k.tenant_id
             where k.secret_hash = $1 and k.status = 'active'
             limit 1",
        )
        .bind(hash_token(secret))
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(None);
        };

        let tenant = Tenant {
            id: row.try_get("id").map_err(store_backend_error)?,
            slug: row.try_get("slug").map_err(store_backend_error)?,
            name: row.try_get("name").map_err(store_backend_error)?,
            suspended: row.try_get("suspended").map_err(store_backend_error)?,
            created_at: row.try_get("created_at").map_err(store_backend_error)?,
        };
        if tenant.suspended {
            return Ok(None);
        }
        let api_key_id: Uuid = row.try_get("api_key_id").map_err(store_backend_error)?;
        sqlx::query("update tenant_api_keys set last_used_at = now() where id = $1")
            .bind(api_key_id)
            .execute(&self.pool)
            .await
            .map_err(store_backend_error)?;

        Ok(Some(GatewayAuthContext { tenant, api_key_id }))
    }

    pub async fn authenticate_tenant_management_token(
        &self,
        token: &str,
    ) -> Result<Option<TenantManagementPrincipal>, StoreError> {
        let row = sqlx::query(
            "select subject, tenant_id from tenant_management_tokens where token_hash = $1 limit 1",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        Ok(row.map(|row| TenantManagementPrincipal {
            subject: row.try_get("subject").expect("subject"),
            tenant_id: row.try_get("tenant_id").expect("tenant_id"),
        }))
    }

    pub async fn authorize_control(
        &self,
        token: &str,
        permission: Permission,
        target: ScopeTarget,
    ) -> Result<ServiceAccountPrincipal, AuthError> {
        let row = sqlx::query(
            "select subject, role, scopes from service_accounts where token_hash = $1 limit 1",
        )
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await
        .map_err(auth_backend_error)?;

        let Some(row) = row else {
            return Err(AuthError::Unauthorized);
        };

        let Json(scopes): Json<Vec<ScopeTarget>> =
            row.try_get("scopes").map_err(auth_backend_error)?;
        let principal = ServiceAccountPrincipal {
            subject: row.try_get("subject").map_err(auth_backend_error)?,
            role: role_from_db(
                row.try_get::<String, _>("role")
                    .map_err(auth_backend_error)?
                    .as_str(),
            )?,
            scopes,
        };

        if !role_allows(&principal.role, &permission) || !scope_allows(&principal.scopes, &target) {
            return Err(AuthError::Forbidden);
        }

        Ok(principal)
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>, StoreError> {
        let rows = sqlx::query(
            "select id, slug, name, suspended, created_at from tenants order by created_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter().map(|row| tenant_from_row(&row)).collect()
    }

    pub async fn create_tenant(&self, slug: String, name: String) -> Result<Tenant, StoreError> {
        let tenant = Tenant {
            id: Uuid::new_v4(),
            slug,
            name,
            suspended: false,
            created_at: Utc::now(),
        };
        sqlx::query(
            "insert into tenants (id, slug, name, suspended, created_at) values ($1, $2, $3, $4, $5)",
        )
        .bind(tenant.id)
        .bind(&tenant.slug)
        .bind(&tenant.name)
        .bind(tenant.suspended)
        .bind(tenant.created_at)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;
        Ok(tenant)
    }

    pub async fn list_tenant_api_keys(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<TenantApiKeyView>, StoreError> {
        let rows = sqlx::query(
            "select id, tenant_id, label, prefix, status, created_at, last_used_at
             from tenant_api_keys where tenant_id = $1 order by created_at desc",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| tenant_api_key_view_from_row(&row))
            .collect()
    }

    pub async fn create_tenant_api_key(
        &self,
        tenant_id: Uuid,
        label: String,
    ) -> Result<CreatedApiKey, AuthError> {
        let tenant_exists =
            sqlx::query_scalar::<_, i64>("select count(*) from tenants where id = $1")
                .bind(tenant_id)
                .fetch_one(&self.pool)
                .await
                .map_err(auth_backend_error)?;
        if tenant_exists == 0 {
            return Err(AuthError::Unauthorized);
        }

        let secret = format!("fgk_{}", Uuid::new_v4().simple());
        let record = TenantApiKeyView {
            id: Uuid::new_v4(),
            tenant_id,
            label,
            prefix: secret[..12].to_string(),
            status: TenantApiKeyStatus::Active,
            created_at: Utc::now(),
            last_used_at: None,
        };

        sqlx::query(
            "insert into tenant_api_keys
             (id, tenant_id, prefix, label, status, secret_hash, created_at, last_used_at)
             values ($1, $2, $3, $4, 'active', $5, $6, $7)",
        )
        .bind(record.id)
        .bind(record.tenant_id)
        .bind(&record.prefix)
        .bind(&record.label)
        .bind(hash_token(&secret))
        .bind(record.created_at)
        .bind(record.last_used_at)
        .execute(&self.pool)
        .await
        .map_err(auth_backend_error)?;

        Ok(CreatedApiKey { record, secret })
    }

    pub async fn rotate_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<CreatedApiKey, AuthError> {
        self.assert_api_key_owner(tenant_id, api_key_id).await?;

        let secret = format!("fgk_{}", Uuid::new_v4().simple());
        let row = sqlx::query(
            "update tenant_api_keys
             set prefix = $1, status = 'active', secret_hash = $2, last_used_at = null
             where id = $3
             returning id, tenant_id, label, prefix, status, created_at, last_used_at",
        )
        .bind(&secret[..12])
        .bind(hash_token(&secret))
        .bind(api_key_id)
        .fetch_one(&self.pool)
        .await
        .map_err(auth_backend_error)?;

        Ok(CreatedApiKey {
            record: tenant_api_key_view_from_row(&row).map_err(auth_backend_error)?,
            secret,
        })
    }

    pub async fn revoke_tenant_api_key(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<TenantApiKeyView, AuthError> {
        self.assert_api_key_owner(tenant_id, api_key_id).await?;

        let row = sqlx::query(
            "update tenant_api_keys
             set status = 'revoked'
             where id = $1
             returning id, tenant_id, label, prefix, status, created_at, last_used_at",
        )
        .bind(api_key_id)
        .fetch_one(&self.pool)
        .await
        .map_err(auth_backend_error)?;

        tenant_api_key_view_from_row(&row).map_err(auth_backend_error)
    }

    pub async fn list_tenant_models(
        &self,
        _tenant_id: Uuid,
    ) -> Result<Vec<ModelDescriptor>, StoreError> {
        let accounts = self.list_provider_accounts().await?;
        let route_groups = self.list_route_groups().await?;
        let mut active_models = BTreeMap::new();

        for account in accounts {
            if account.state != AccountState::Active {
                continue;
            }

            let provider_kind = account.provider.clone();
            for model_id in account.capabilities {
                active_models
                    .entry(model_id)
                    .or_insert_with(|| provider_kind.clone());
            }
        }

        Ok(active_models
            .into_iter()
            .map(|(model_id, provider_kind)| {
                route_groups
                    .iter()
                    .find(|route_group| {
                        route_group.public_model == model_id
                            && route_group.provider_kind == provider_kind
                    })
                    .or_else(|| {
                        route_groups
                            .iter()
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
        let requests = self.tenant_requests(tenant_id).await?;
        let mut summary = UsageSummary {
            total_requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            last_request_at: None,
        };

        for record in requests {
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
        let rows = sqlx::query(
            "select id, tenant_id, api_key_id, public_model, provider_kind, status_code, latency_ms, usage, created_at
             from usage_ledger where tenant_id = $1 order by created_at desc",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter().map(|row| request_from_row(&row)).collect()
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
        sqlx::query(
            "insert into usage_ledger
             (id, tenant_id, api_key_id, public_model, provider_kind, status_code, latency_ms, usage, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(api_key_id)
        .bind(public_model)
        .bind(provider_kind)
        .bind(i32::from(status_code))
        .bind(latency_ms as i64)
        .bind(Json(usage))
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;
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
        sqlx::query(
            "insert into audit_events (id, actor, action, resource, request_id, details, occurred_at)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(actor.into())
        .bind(action.into())
        .bind(resource.into())
        .bind(request_id.into())
        .bind(details)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;
        Ok(())
    }

    pub async fn list_audit_events(&self) -> Result<Vec<AuditEvent>, StoreError> {
        let rows = sqlx::query(
            "select id, actor, action, resource, request_id, details, occurred_at
             from audit_events order by occurred_at desc",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| audit_event_from_row(&row))
            .collect()
    }

    pub async fn record_alert_delivery(
        &self,
        alert_id: Uuid,
        destination: impl Into<String>,
    ) -> Result<bool, StoreError> {
        let result = sqlx::query(
            "insert into alert_delivery_receipts (id, alert_id, destination, delivered_at)
             values ($1, $2, $3, $4)
             on conflict (alert_id, destination) do nothing",
        )
        .bind(Uuid::new_v4())
        .bind(alert_id)
        .bind(destination.into())
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_alert_delivery_receipts(
        &self,
        destination: &str,
    ) -> Result<Vec<AlertDeliveryReceipt>, StoreError> {
        let rows = sqlx::query(
            "select id, alert_id, destination, delivered_at
             from alert_delivery_receipts
             where destination = $1
             order by delivered_at desc",
        )
        .bind(destination)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| alert_delivery_receipt_from_row(&row))
            .collect()
    }

    pub async fn list_provider_accounts(&self) -> Result<Vec<ProviderAccountRecord>, StoreError> {
        let rows = sqlx::query(
            "select id, provider, credential_kind, payload_version, state, external_account_id,
                    redacted_display, plan_type, metadata, labels, tags, capabilities, expires_at,
                    last_validated_at, created_at
             from provider_accounts order by created_at desc",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| provider_account_from_row(&row))
            .collect()
    }

    pub async fn provider_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let row = sqlx::query(
            "select id, provider, credential_kind, payload_version, state, external_account_id,
                    redacted_display, plan_type, metadata, labels, tags, capabilities, expires_at,
                    last_validated_at, created_at
             from provider_accounts
             where id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;
        row.map(|row| provider_account_from_row(&row)).transpose()
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

        sqlx::query(
            "insert into account_inspections
             (id, provider_account_id, actor, status, error_kind, error_code, error_message, inspected_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(record.id)
        .bind(record.provider_account_id)
        .bind(&record.actor)
        .bind(account_inspection_status_to_db(&record.status))
        .bind(&record.error_kind)
        .bind(&record.error_code)
        .bind(&record.error_message)
        .bind(record.inspected_at)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        Ok(record)
    }

    pub async fn list_account_inspections(
        &self,
        provider_account_id: Uuid,
    ) -> Result<Vec<AccountInspectionRecord>, StoreError> {
        let rows = sqlx::query(
            "select id, provider_account_id, actor, status, error_kind, error_code, error_message, inspected_at
             from account_inspections
             where provider_account_id = $1
             order by inspected_at desc, id desc",
        )
        .bind(provider_account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| account_inspection_from_row(&row))
            .collect()
    }

    pub async fn upsert_provider_account_quota_snapshot(
        &self,
        provider_account_id: Uuid,
        snapshot: QuotaSnapshot,
    ) -> Result<ProviderAccountQuotaSnapshotRecord, StoreError> {
        let details = snapshot.details.unwrap_or_else(|| json!({}));
        sqlx::query(
            "insert into account_quota_snapshots
             (provider_account_id, plan_label, remaining_requests_hint, details, checked_at)
             values ($1, $2, $3, $4, $5)
             on conflict (provider_account_id) do update
             set plan_label = excluded.plan_label,
                 remaining_requests_hint = excluded.remaining_requests_hint,
                 details = excluded.details,
                 checked_at = excluded.checked_at",
        )
        .bind(provider_account_id)
        .bind(snapshot.plan_label)
        .bind(snapshot.remaining_requests_hint.map(|value| value as i64))
        .bind(details)
        .bind(snapshot.checked_at)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        self.provider_account_quota_snapshot(provider_account_id)
            .await?
            .ok_or(StoreError::NotFound)
    }

    pub async fn provider_account_quota_snapshot(
        &self,
        provider_account_id: Uuid,
    ) -> Result<Option<ProviderAccountQuotaSnapshotRecord>, StoreError> {
        let row = sqlx::query(
            "select provider_account_id, plan_label, remaining_requests_hint, details, checked_at
             from account_quota_snapshots
             where provider_account_id = $1",
        )
        .bind(provider_account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;
        row.map(|row| provider_account_quota_snapshot_from_row(&row))
            .transpose()
    }

    pub async fn provider_account_envelope(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderAccountEnvelope>, StoreError> {
        let row = sqlx::query(
            "select p.provider, p.credential_kind, p.payload_version, p.metadata, p.labels, p.tags, s.cipher_text
             from provider_accounts p
             join lateral (
                select cipher_text
                from provider_account_secret_versions
                where provider_account_id = p.id
                order by created_at desc
                limit 1
             ) s on true
             where p.id = $1
             limit 1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(None);
        };

        let Json(labels): Json<Vec<String>> = row.try_get("labels").map_err(store_backend_error)?;
        let Json(tags): Json<BTreeMap<String, String>> =
            row.try_get("tags").map_err(store_backend_error)?;
        let metadata: Value = row.try_get("metadata").map_err(store_backend_error)?;
        let cipher_text: Vec<u8> = row.try_get("cipher_text").map_err(store_backend_error)?;
        let credentials = self.decrypt_json(&cipher_text)?;

        Ok(Some(ProviderAccountEnvelope {
            provider: row.try_get("provider").map_err(store_backend_error)?,
            credential_kind: row
                .try_get("credential_kind")
                .map_err(store_backend_error)?,
            payload_version: row
                .try_get("payload_version")
                .map_err(store_backend_error)?,
            credentials,
            metadata,
            labels,
            tags,
        }))
    }

    pub async fn ingest_provider_account(
        &self,
        envelope: ProviderAccountEnvelope,
        validated: ValidatedProviderAccount,
        capabilities: AccountCapabilities,
    ) -> Result<ProviderAccountRecord, StoreError> {
        let record = ProviderAccountRecord {
            id: Uuid::new_v4(),
            provider: envelope.provider.clone(),
            credential_kind: envelope.credential_kind.clone(),
            payload_version: envelope.payload_version.clone(),
            state: AccountState::Active,
            external_account_id: validated.provider_account_id,
            redacted_display: validated.redacted_display,
            plan_type: envelope
                .metadata
                .get("plan_type")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            metadata: envelope.metadata.clone(),
            labels: envelope.labels.clone(),
            tags: envelope.tags.clone(),
            capabilities: capabilities
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect(),
            expires_at: validated.expires_at,
            last_validated_at: Some(Utc::now()),
            created_at: Utc::now(),
        };

        sqlx::query(
            "insert into provider_accounts
             (id, provider, credential_kind, payload_version, state, external_account_id, redacted_display,
              plan_type, metadata, labels, tags, capabilities, expires_at, last_validated_at, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(record.id)
        .bind(&record.provider)
        .bind(&record.credential_kind)
        .bind(&record.payload_version)
        .bind(account_state_to_db(&record.state))
        .bind(&record.external_account_id)
        .bind(&record.redacted_display)
        .bind(&record.plan_type)
        .bind(&record.metadata)
        .bind(Json(record.labels.clone()))
        .bind(Json(record.tags.clone()))
        .bind(Json(record.capabilities.clone()))
        .bind(record.expires_at)
        .bind(record.last_validated_at)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into provider_account_secret_versions (id, provider_account_id, cipher_text, key_version, created_at)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(record.id)
        .bind(self.encrypt_json(&envelope.credentials)?)
        .bind(envelope.payload_version)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "insert into account_runtime
             (provider_account_id, state, health_score, consecutive_failures, in_flight, max_in_flight)
             values ($1, 'active', 100, 0, 0, 16)",
        )
        .bind(record.id)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

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
        let now = Utc::now();
        let capability_ids = capabilities
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();

        let row = sqlx::query(
            "update provider_accounts
             set state = 'active',
                 external_account_id = $1,
                 redacted_display = $2,
                 capabilities = $3,
                 expires_at = $4,
                 last_validated_at = $5
             where id = $6
             returning id, provider, credential_kind, payload_version, state, external_account_id,
                       redacted_display, plan_type, metadata, labels, tags, capabilities, expires_at,
                       last_validated_at, created_at",
        )
        .bind(validated.provider_account_id)
        .bind(validated.redacted_display)
        .bind(Json(capability_ids))
        .bind(validated.expires_at)
        .bind(now)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(None);
        };

        sqlx::query(
            "update account_runtime
             set state = 'active', cooldown_until = null, circuit_open_until = null,
                 consecutive_failures = 0
             where provider_account_id = $1",
        )
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let record = provider_account_from_row(&row)?;
        for model_id in &record.capabilities {
            self.ensure_route_group_and_binding(&record.provider, model_id, account_id)
                .await?;
        }

        Ok(Some(record))
    }

    pub async fn rotate_provider_account_secret(
        &self,
        account_id: Uuid,
        credentials: Value,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let now = Utc::now();
        let row = sqlx::query(
            "update provider_accounts
             set state = 'active', expires_at = $1, last_validated_at = $2
             where id = $3
             returning id, provider, credential_kind, payload_version, state, external_account_id,
                       redacted_display, plan_type, metadata, labels, tags, capabilities, expires_at,
                       last_validated_at, created_at",
        )
        .bind(expires_at)
        .bind(now)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(None);
        };

        let record = provider_account_from_row(&row)?;

        sqlx::query(
            "insert into provider_account_secret_versions
             (id, provider_account_id, cipher_text, key_version, created_at)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(self.encrypt_json(&credentials)?)
        .bind(record.payload_version.clone())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query(
            "update account_runtime
             set state = 'active', cooldown_until = null, circuit_open_until = null,
                 consecutive_failures = 0
             where provider_account_id = $1",
        )
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        Ok(Some(record))
    }

    pub async fn set_provider_account_state(
        &self,
        account_id: Uuid,
        state: AccountState,
    ) -> Result<Option<ProviderAccountRecord>, StoreError> {
        let state_db = account_state_to_db(&state);
        let row = sqlx::query(
            "update provider_accounts
             set state = $1
             where id = $2
             returning id, provider, credential_kind, payload_version, state, external_account_id,
                       redacted_display, plan_type, metadata, labels, tags, capabilities, expires_at,
                       last_validated_at, created_at",
        )
        .bind(state_db)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        if row.is_none() {
            return Ok(None);
        }
        sqlx::query("update account_runtime set state = $1 where provider_account_id = $2")
            .bind(state_db)
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(store_backend_error)?;

        row.map(|row| provider_account_from_row(&row)).transpose()
    }

    pub async fn create_route_group(
        &self,
        public_model: String,
        provider_kind: String,
        upstream_model: String,
    ) -> Result<RouteGroupRecord, StoreError> {
        let row = sqlx::query(
            "insert into route_groups (id, slug, public_model, provider_kind, upstream_model, created_at)
             values ($1, $2, $3, $4, $5, $6)
             on conflict (public_model, provider_kind) do update
             set slug = excluded.slug,
                 upstream_model = excluded.upstream_model
             returning id, slug, public_model, provider_kind, upstream_model, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(derive_route_group_slug(&provider_kind, &public_model))
        .bind(public_model)
        .bind(provider_kind)
        .bind(upstream_model)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
        .map_err(store_backend_error)?;
        route_group_from_row(&row)
    }

    pub async fn ensure_route_group_and_binding(
        &self,
        provider_kind: &str,
        upstream_model: &str,
        account_id: Uuid,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(store_backend_error)?;
        let route_group = sqlx::query(
            "insert into route_groups (id, slug, public_model, provider_kind, upstream_model, created_at)
             values ($1, $2, $3, $4, $5, $6)
             on conflict (public_model, provider_kind) do update
             set slug = excluded.slug,
                 upstream_model = excluded.upstream_model
             returning id, slug, public_model, provider_kind, upstream_model, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(derive_route_group_slug(provider_kind, upstream_model))
        .bind(upstream_model)
        .bind(provider_kind)
        .bind(upstream_model)
        .bind(Utc::now())
        .fetch_one(&mut *tx)
        .await
        .map_err(store_backend_error)
        .and_then(|row| route_group_from_row(&row))?;

        sqlx::query(
            "insert into route_group_bindings
             (id, route_group_id, provider_account_id, weight, max_in_flight, created_at)
             values ($1, $2, $3, 100, 16, $4)
             on conflict (route_group_id, provider_account_id) do nothing",
        )
        .bind(Uuid::new_v4())
        .bind(route_group.id)
        .bind(account_id)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await
        .map_err(store_backend_error)?;

        tx.commit().await.map_err(store_backend_error)
    }

    pub async fn list_route_groups(&self) -> Result<Vec<RouteGroupRecord>, StoreError> {
        let rows = sqlx::query(
            "select id, slug, public_model, provider_kind, upstream_model, created_at
             from route_groups order by created_at desc",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| route_group_from_row(&row))
            .collect()
    }

    pub async fn list_route_group_bindings(
        &self,
    ) -> Result<Vec<RouteGroupBindingRecord>, StoreError> {
        let rows = sqlx::query(
            "select id, route_group_id, provider_account_id, weight, max_in_flight, created_at
             from route_group_bindings
             order by created_at desc, id desc",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| route_group_binding_from_row(&row))
            .collect()
    }

    pub async fn add_route_group_fallback(
        &self,
        route_group_id: Uuid,
        fallback_route_group_id: Uuid,
        position: u32,
    ) -> Result<RouteGroupFallbackRecord, StoreError> {
        let row = sqlx::query(
            "insert into route_group_fallbacks
             (route_group_id, fallback_route_group_id, position, created_at)
             values ($1, $2, $3, $4)
             on conflict (route_group_id, fallback_route_group_id) do update
             set position = excluded.position
             returning route_group_id, fallback_route_group_id, position, created_at",
        )
        .bind(route_group_id)
        .bind(fallback_route_group_id)
        .bind(position as i32)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
        .map_err(store_backend_error)?;
        route_group_fallback_from_row(&row)
    }

    pub async fn list_route_group_fallbacks(
        &self,
        route_group_id: Uuid,
    ) -> Result<Vec<RouteGroupFallbackRecord>, StoreError> {
        let rows = sqlx::query(
            "select route_group_id, fallback_route_group_id, position, created_at
             from route_group_fallbacks
             where route_group_id = $1
             order by position, fallback_route_group_id",
        )
        .bind(route_group_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| route_group_fallback_from_row(&row))
            .collect()
    }

    pub async fn bind_provider_account(
        &self,
        route_group_id: Uuid,
        provider_account_id: Uuid,
        weight: u32,
        max_in_flight: u32,
    ) -> Result<RouteGroupBindingRecord, AuthError> {
        let route_exists =
            sqlx::query_scalar::<_, i64>("select count(*) from route_groups where id = $1")
                .bind(route_group_id)
                .fetch_one(&self.pool)
                .await
                .map_err(auth_backend_error)?;
        let account_exists =
            sqlx::query_scalar::<_, i64>("select count(*) from provider_accounts where id = $1")
                .bind(provider_account_id)
                .fetch_one(&self.pool)
                .await
                .map_err(auth_backend_error)?;
        if route_exists == 0 || account_exists == 0 {
            return Err(AuthError::Unauthorized);
        }

        let row = sqlx::query(
            "insert into route_group_bindings
             (id, route_group_id, provider_account_id, weight, max_in_flight, created_at)
             values ($1, $2, $3, $4, $5, $6)
             on conflict (route_group_id, provider_account_id) do update
             set weight = excluded.weight,
                 max_in_flight = excluded.max_in_flight
             returning id, route_group_id, provider_account_id, weight, max_in_flight, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(route_group_id)
        .bind(provider_account_id)
        .bind(weight as i32)
        .bind(max_in_flight as i32)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
        .map_err(auth_backend_error)?;
        let record = route_group_binding_from_row(&row).map_err(auth_backend_error)?;

        sqlx::query("update account_runtime set max_in_flight = $1 where provider_account_id = $2")
            .bind(max_in_flight as i32)
            .bind(provider_account_id)
            .execute(&self.pool)
            .await
            .map_err(auth_backend_error)?;

        Ok(record)
    }

    pub async fn resolve_route_group(
        &self,
        public_model: &str,
    ) -> Result<Option<RouteGroupRecord>, StoreError> {
        let row = sqlx::query(
            "select id, slug, public_model, provider_kind, upstream_model, created_at
             from route_groups where public_model = $1 limit 1",
        )
        .bind(public_model)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;
        row.map(|row| route_group_from_row(&row)).transpose()
    }

    pub async fn scheduler_candidates(
        &self,
        public_model: &str,
    ) -> Result<Vec<ProviderAccountCandidate>, StoreError> {
        let rows = sqlx::query(
            "select b.provider_account_id, b.route_group_id, b.weight, r.state, r.health_score, r.cooldown_until,
                    r.circuit_open_until, r.consecutive_failures, r.in_flight, r.max_in_flight,
                    r.last_used_at, p.provider
             from route_groups rg
             join route_group_bindings b on b.route_group_id = rg.id
             join account_runtime r on r.provider_account_id = b.provider_account_id
             join provider_accounts p on p.id = b.provider_account_id
             where rg.public_model = $1",
        )
        .bind(public_model)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;
        rows.into_iter()
            .map(|row| candidate_from_row(&row))
            .collect()
    }

    pub async fn mark_scheduler_outcome(
        &self,
        account_id: Uuid,
        outcome: ProviderOutcome,
    ) -> Result<(), StoreError> {
        let row = sqlx::query(
            "select state, health_score, cooldown_until, circuit_open_until, consecutive_failures,
                    in_flight, max_in_flight, last_used_at
             from account_runtime where provider_account_id = $1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(());
        };

        let mut runtime = account_runtime_from_row(&row)?;
        runtime.apply_outcome(outcome, Utc::now());
        sqlx::query(
            "update account_runtime
             set state = $1, health_score = $2, cooldown_until = $3, circuit_open_until = $4,
                 consecutive_failures = $5, in_flight = $6, max_in_flight = $7, last_used_at = $8
             where provider_account_id = $9",
        )
        .bind(account_state_to_db(&runtime.state))
        .bind(i32::from(runtime.health_score))
        .bind(runtime.cooldown_until)
        .bind(runtime.circuit_open_until)
        .bind(runtime.consecutive_failures as i32)
        .bind(runtime.in_flight as i32)
        .bind(runtime.max_in_flight as i32)
        .bind(runtime.last_used_at)
        .bind(account_id)
        .execute(&self.pool)
        .await
        .map_err(store_backend_error)?;

        sqlx::query("update provider_accounts set state = $1 where id = $2")
            .bind(account_state_to_db(&runtime.state))
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(store_backend_error)?;

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

    pub async fn dispatch_due_provider_account_probes(
        &self,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchLease>, StoreError> {
        let now = Utc::now();
        let lease_until = now + TimeDelta::minutes(5);
        let rows = sqlx::query(
            "select p.id
             from provider_accounts p
             left join account_probe_leases l
               on l.provider_account_id = p.id
              and l.leased_until > $1
             where p.state in ('active', 'cooling', 'quota_exhausted')
               and l.provider_account_id is null
               and coalesce(p.last_validated_at, p.created_at) <= $1
             order by coalesce(p.last_validated_at, p.created_at), p.created_at, p.id
             limit $2",
        )
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let mut leases = Vec::with_capacity(rows.len());
        for row in rows {
            let account_id: Uuid = row.try_get("id").map_err(store_backend_error)?;
            let lease = ProbeDispatchLease {
                lease_id: Uuid::new_v4(),
                account_id,
                leased_at: now,
                leased_until: lease_until,
            };
            let result = sqlx::query(
                "insert into account_probe_leases (provider_account_id, lease_id, leased_at, leased_until)
                 values ($1, $2, $3, $4)
                 on conflict (provider_account_id) do update
                 set lease_id = excluded.lease_id,
                     leased_at = excluded.leased_at,
                     leased_until = excluded.leased_until
                 where account_probe_leases.leased_until <= excluded.leased_at",
            )
            .bind(lease.account_id)
            .bind(lease.lease_id)
            .bind(lease.leased_at)
            .bind(lease.leased_until)
            .execute(&self.pool)
            .await
            .map_err(store_backend_error)?;

            if result.rows_affected() > 0 {
                leases.push(lease);
            }
        }

        Ok(leases)
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
        let lease_until = now + TimeDelta::minutes(5);
        let candidate_ids = sqlx::query(
            "select p.id
             from provider_accounts p
             left join account_refresh_leases l
               on l.provider_account_id = p.id
              and l.leased_until > $1
             where p.state in ('active', 'cooling', 'quota_exhausted')
               and p.credential_kind = 'oauth_tokens'
               and p.expires_at is not null
               and p.expires_at <= $2
               and l.provider_account_id is null
             order by p.expires_at, coalesce(p.last_validated_at, p.created_at), p.created_at, p.id
             limit $3",
        )
        .bind(now)
        .bind(due_before)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let mut leases = Vec::with_capacity(candidate_ids.len());
        for row in candidate_ids {
            let account_id: Uuid = row.try_get("id").map_err(store_backend_error)?;
            let envelope = self.provider_account_envelope(account_id).await?;
            let has_refresh_token = envelope
                .as_ref()
                .and_then(|value| value.credentials.get("refresh_token"))
                .and_then(Value::as_str)
                .is_some();
            if !has_refresh_token {
                continue;
            }

            let lease = RefreshDispatchLease {
                lease_id: Uuid::new_v4(),
                account_id,
                leased_at: now,
                leased_until: lease_until,
            };
            let result = sqlx::query(
                "insert into account_refresh_leases (provider_account_id, lease_id, leased_at, leased_until)
                 values ($1, $2, $3, $4)
                 on conflict (provider_account_id) do update
                 set lease_id = excluded.lease_id,
                     leased_at = excluded.leased_at,
                     leased_until = excluded.leased_until
                 where account_refresh_leases.leased_until <= excluded.leased_at",
            )
            .bind(lease.account_id)
            .bind(lease.lease_id)
            .bind(lease.leased_at)
            .bind(lease.leased_until)
            .execute(&self.pool)
            .await
            .map_err(store_backend_error)?;

            if result.rows_affected() > 0 {
                leases.push(lease);
            }
        }

        Ok(leases)
    }

    pub async fn resolve_provider_connection(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderConnectionInfo>, StoreError> {
        let row = sqlx::query(
            "select p.id, p.provider, p.credential_kind, p.metadata, s.cipher_text
             from provider_accounts p
             join lateral (
                select cipher_text
                from provider_account_secret_versions
                where provider_account_id = p.id
                order by created_at desc
                limit 1
             ) s on true
             where p.id = $1
             limit 1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_backend_error)?;

        let Some(row) = row else {
            return Ok(None);
        };

        let metadata: Value = row.try_get("metadata").map_err(store_backend_error)?;
        let cipher_text: Vec<u8> = row.try_get("cipher_text").map_err(store_backend_error)?;
        let credentials = self.decrypt_json(&cipher_text)?;

        provider_connection_from_parts(
            row.try_get("id").map_err(store_backend_error)?,
            row.try_get::<String, _>("provider")
                .map_err(store_backend_error)?
                .as_str(),
            row.try_get::<String, _>("credential_kind")
                .map_err(store_backend_error)?
                .as_str(),
            &metadata,
            &credentials,
        )
        .map(Some)
    }

    async fn assert_api_key_owner(
        &self,
        tenant_id: Uuid,
        api_key_id: Uuid,
    ) -> Result<(), AuthError> {
        let owner = sqlx::query_scalar::<_, Option<Uuid>>(
            "select tenant_id from tenant_api_keys where id = $1",
        )
        .bind(api_key_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(auth_backend_error)?;

        match owner.flatten() {
            Some(owner_tenant_id) if owner_tenant_id == tenant_id => Ok(()),
            Some(_) => Err(AuthError::Forbidden),
            None => Err(AuthError::Unauthorized),
        }
    }

    fn encrypt_json(&self, value: &Value) -> Result<Vec<u8>, StoreError> {
        let plaintext =
            serde_json::to_vec(value).map_err(|error| StoreError::Backend(error.to_string()))?;
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let mut nonce_bytes = [0_u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let mut encoded = nonce_bytes.to_vec();
        encoded.extend(ciphertext);
        Ok(encoded)
    }

    fn decrypt_json(&self, value: &[u8]) -> Result<Value, StoreError> {
        if value.len() < 13 {
            return Err(StoreError::Backend(
                "encrypted provider secret is truncated".to_string(),
            ));
        }

        let (nonce_bytes, cipher_text) = value.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(&self.encryption_key)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), cipher_text)
            .map_err(|error| StoreError::Backend(error.to_string()))?;

        serde_json::from_slice(&plaintext).map_err(|error| StoreError::Backend(error.to_string()))
    }
}

fn store_backend_error(error: impl ToString) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn auth_backend_error(error: impl ToString) -> AuthError {
    AuthError::Storage(error.to_string())
}

fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

fn derive_encryption_key(master_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(master_key);
    hasher.finalize().into()
}

fn hash_token(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

fn tenant_from_row(row: &PgRow) -> Result<Tenant, StoreError> {
    Ok(Tenant {
        id: row.try_get("id").map_err(store_backend_error)?,
        slug: row.try_get("slug").map_err(store_backend_error)?,
        name: row.try_get("name").map_err(store_backend_error)?,
        suspended: row.try_get("suspended").map_err(store_backend_error)?,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn tenant_api_key_view_from_row(row: &PgRow) -> Result<TenantApiKeyView, StoreError> {
    Ok(TenantApiKeyView {
        id: row.try_get("id").map_err(store_backend_error)?,
        tenant_id: row.try_get("tenant_id").map_err(store_backend_error)?,
        label: row.try_get("label").map_err(store_backend_error)?,
        prefix: row.try_get("prefix").map_err(store_backend_error)?,
        status: tenant_api_key_status_from_db(
            row.try_get::<String, _>("status")
                .map_err(store_backend_error)?
                .as_str(),
        )?,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
        last_used_at: row.try_get("last_used_at").map_err(store_backend_error)?,
    })
}

fn request_from_row(row: &PgRow) -> Result<RequestRecord, StoreError> {
    let Json(usage): Json<TokenUsage> = row.try_get("usage").map_err(store_backend_error)?;
    Ok(RequestRecord {
        id: row.try_get("id").map_err(store_backend_error)?,
        tenant_id: row.try_get("tenant_id").map_err(store_backend_error)?,
        api_key_id: row.try_get("api_key_id").map_err(store_backend_error)?,
        public_model: row.try_get("public_model").map_err(store_backend_error)?,
        provider_kind: row.try_get("provider_kind").map_err(store_backend_error)?,
        status_code: row
            .try_get::<i32, _>("status_code")
            .map_err(store_backend_error)? as u16,
        latency_ms: row
            .try_get::<i64, _>("latency_ms")
            .map_err(store_backend_error)? as u64,
        usage,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn audit_event_from_row(row: &PgRow) -> Result<AuditEvent, StoreError> {
    Ok(AuditEvent {
        id: row.try_get("id").map_err(store_backend_error)?,
        actor: row.try_get("actor").map_err(store_backend_error)?,
        action: row.try_get("action").map_err(store_backend_error)?,
        resource: row.try_get("resource").map_err(store_backend_error)?,
        request_id: row.try_get("request_id").map_err(store_backend_error)?,
        occurred_at: row.try_get("occurred_at").map_err(store_backend_error)?,
        details: row.try_get("details").map_err(store_backend_error)?,
    })
}

fn alert_delivery_receipt_from_row(row: &PgRow) -> Result<AlertDeliveryReceipt, StoreError> {
    Ok(AlertDeliveryReceipt {
        id: row.try_get("id").map_err(store_backend_error)?,
        alert_id: row.try_get("alert_id").map_err(store_backend_error)?,
        destination: row.try_get("destination").map_err(store_backend_error)?,
        delivered_at: row.try_get("delivered_at").map_err(store_backend_error)?,
    })
}

fn provider_account_from_row(row: &PgRow) -> Result<ProviderAccountRecord, StoreError> {
    let Json(labels): Json<Vec<String>> = row.try_get("labels").map_err(store_backend_error)?;
    let Json(tags): Json<BTreeMap<String, String>> =
        row.try_get("tags").map_err(store_backend_error)?;
    let Json(capabilities): Json<Vec<String>> =
        row.try_get("capabilities").map_err(store_backend_error)?;
    Ok(ProviderAccountRecord {
        id: row.try_get("id").map_err(store_backend_error)?,
        provider: row.try_get("provider").map_err(store_backend_error)?,
        credential_kind: row
            .try_get("credential_kind")
            .map_err(store_backend_error)?,
        payload_version: row
            .try_get("payload_version")
            .map_err(store_backend_error)?,
        state: account_state_from_db(
            row.try_get::<String, _>("state")
                .map_err(store_backend_error)?
                .as_str(),
        )?,
        external_account_id: row
            .try_get("external_account_id")
            .map_err(store_backend_error)?,
        redacted_display: row
            .try_get("redacted_display")
            .map_err(store_backend_error)?,
        plan_type: row.try_get("plan_type").map_err(store_backend_error)?,
        metadata: row.try_get("metadata").map_err(store_backend_error)?,
        labels,
        tags,
        capabilities,
        expires_at: row.try_get("expires_at").map_err(store_backend_error)?,
        last_validated_at: row
            .try_get("last_validated_at")
            .map_err(store_backend_error)?,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn route_group_from_row(row: &PgRow) -> Result<RouteGroupRecord, StoreError> {
    Ok(RouteGroupRecord {
        id: row.try_get("id").map_err(store_backend_error)?,
        slug: row.try_get("slug").map_err(store_backend_error)?,
        public_model: row.try_get("public_model").map_err(store_backend_error)?,
        provider_kind: row.try_get("provider_kind").map_err(store_backend_error)?,
        upstream_model: row.try_get("upstream_model").map_err(store_backend_error)?,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn route_group_binding_from_row(row: &PgRow) -> Result<RouteGroupBindingRecord, StoreError> {
    Ok(RouteGroupBindingRecord {
        id: row.try_get("id").map_err(store_backend_error)?,
        route_group_id: row.try_get("route_group_id").map_err(store_backend_error)?,
        provider_account_id: row
            .try_get("provider_account_id")
            .map_err(store_backend_error)?,
        weight: row
            .try_get::<i32, _>("weight")
            .map_err(store_backend_error)? as u32,
        max_in_flight: row
            .try_get::<i32, _>("max_in_flight")
            .map_err(store_backend_error)? as u32,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn route_group_fallback_from_row(row: &PgRow) -> Result<RouteGroupFallbackRecord, StoreError> {
    Ok(RouteGroupFallbackRecord {
        route_group_id: row.try_get("route_group_id").map_err(store_backend_error)?,
        fallback_route_group_id: row
            .try_get("fallback_route_group_id")
            .map_err(store_backend_error)?,
        position: row
            .try_get::<i32, _>("position")
            .map_err(store_backend_error)? as u32,
        created_at: row.try_get("created_at").map_err(store_backend_error)?,
    })
}

fn account_inspection_from_row(row: &PgRow) -> Result<AccountInspectionRecord, StoreError> {
    Ok(AccountInspectionRecord {
        id: row.try_get("id").map_err(store_backend_error)?,
        provider_account_id: row
            .try_get("provider_account_id")
            .map_err(store_backend_error)?,
        actor: row.try_get("actor").map_err(store_backend_error)?,
        status: account_inspection_status_from_db(
            row.try_get::<String, _>("status")
                .map_err(store_backend_error)?
                .as_str(),
        )?,
        error_kind: row.try_get("error_kind").map_err(store_backend_error)?,
        error_code: row.try_get("error_code").map_err(store_backend_error)?,
        error_message: row.try_get("error_message").map_err(store_backend_error)?,
        inspected_at: row.try_get("inspected_at").map_err(store_backend_error)?,
    })
}

fn provider_account_quota_snapshot_from_row(
    row: &PgRow,
) -> Result<ProviderAccountQuotaSnapshotRecord, StoreError> {
    let remaining_requests_hint = row
        .try_get::<Option<i64>, _>("remaining_requests_hint")
        .map_err(store_backend_error)?
        .map(|value| u64::try_from(value).map_err(|error| StoreError::Backend(error.to_string())))
        .transpose()?;

    Ok(ProviderAccountQuotaSnapshotRecord {
        provider_account_id: row
            .try_get("provider_account_id")
            .map_err(store_backend_error)?,
        plan_label: row.try_get("plan_label").map_err(store_backend_error)?,
        remaining_requests_hint,
        details: row.try_get("details").map_err(store_backend_error)?,
        checked_at: row.try_get("checked_at").map_err(store_backend_error)?,
    })
}

fn candidate_from_row(row: &PgRow) -> Result<ProviderAccountCandidate, StoreError> {
    Ok(ProviderAccountCandidate {
        account_id: row
            .try_get("provider_account_id")
            .map_err(store_backend_error)?,
        route_group_id: row.try_get("route_group_id").map_err(store_backend_error)?,
        provider_kind: row.try_get("provider").map_err(store_backend_error)?,
        weight: row
            .try_get::<i32, _>("weight")
            .map_err(store_backend_error)? as u32,
        runtime: account_runtime_from_row(row)?,
    })
}

fn account_runtime_from_row(row: &PgRow) -> Result<AccountRuntime, StoreError> {
    Ok(AccountRuntime {
        state: account_state_from_db(
            row.try_get::<String, _>("state")
                .map_err(store_backend_error)?
                .as_str(),
        )?,
        health_score: row
            .try_get::<i32, _>("health_score")
            .map_err(store_backend_error)? as u8,
        cooldown_until: row.try_get("cooldown_until").map_err(store_backend_error)?,
        circuit_open_until: row
            .try_get("circuit_open_until")
            .map_err(store_backend_error)?,
        consecutive_failures: row
            .try_get::<i32, _>("consecutive_failures")
            .map_err(store_backend_error)? as u32,
        in_flight: row
            .try_get::<i32, _>("in_flight")
            .map_err(store_backend_error)? as u32,
        max_in_flight: row
            .try_get::<i32, _>("max_in_flight")
            .map_err(store_backend_error)? as u32,
        last_used_at: row.try_get("last_used_at").map_err(store_backend_error)?,
    })
}

fn account_state_to_db(state: &AccountState) -> &'static str {
    match state {
        AccountState::PendingValidation => "pending_validation",
        AccountState::Active => "active",
        AccountState::Cooling => "cooling",
        AccountState::Draining => "draining",
        AccountState::QuotaExhausted => "quota_exhausted",
        AccountState::InvalidCredentials => "invalid_credentials",
        AccountState::Disabled => "disabled",
    }
}

fn account_state_from_db(value: &str) -> Result<AccountState, StoreError> {
    match value {
        "pending_validation" => Ok(AccountState::PendingValidation),
        "active" => Ok(AccountState::Active),
        "cooling" => Ok(AccountState::Cooling),
        "draining" => Ok(AccountState::Draining),
        "quota_exhausted" => Ok(AccountState::QuotaExhausted),
        "invalid_credentials" => Ok(AccountState::InvalidCredentials),
        "disabled" => Ok(AccountState::Disabled),
        other => Err(StoreError::Backend(format!(
            "unknown account state: {other}"
        ))),
    }
}

fn account_inspection_status_to_db(status: &AccountInspectionStatus) -> &'static str {
    match status {
        AccountInspectionStatus::Healthy => "healthy",
        AccountInspectionStatus::Unhealthy => "unhealthy",
    }
}

fn account_inspection_status_from_db(value: &str) -> Result<AccountInspectionStatus, StoreError> {
    match value {
        "healthy" => Ok(AccountInspectionStatus::Healthy),
        "unhealthy" => Ok(AccountInspectionStatus::Unhealthy),
        other => Err(StoreError::Backend(format!(
            "unknown account inspection status: {other}"
        ))),
    }
}

fn tenant_api_key_status_from_db(value: &str) -> Result<TenantApiKeyStatus, StoreError> {
    match value {
        "active" => Ok(TenantApiKeyStatus::Active),
        "revoked" => Ok(TenantApiKeyStatus::Revoked),
        other => Err(StoreError::Backend(format!(
            "unknown tenant api key status: {other}"
        ))),
    }
}

fn role_from_db(value: &str) -> Result<Role, AuthError> {
    match value {
        "platform_admin" => Ok(Role::PlatformAdmin),
        "security_admin" => Ok(Role::SecurityAdmin),
        "routing_operator" => Ok(Role::RoutingOperator),
        "tenant_admin" => Ok(Role::TenantAdmin),
        "viewer" => Ok(Role::Viewer),
        "automation_service" => Ok(Role::AutomationService),
        other => Err(AuthError::Storage(format!("unknown role: {other}"))),
    }
}
