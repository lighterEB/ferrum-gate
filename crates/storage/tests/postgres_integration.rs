use pg_embed::{
    pg_enums::PgAuthMethod,
    pg_fetch::{PG_V17, PgFetchSettings},
    postgres::{PgEmbed, PgSettings},
};
use protocol_core::{ModelCapability, ModelDescriptor};
use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};
use std::{collections::BTreeMap, net::TcpListener, time::Duration};
use storage::PostgresPlatformStore;
use tempfile::TempDir;

fn random_open_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("local addr")
        .port()
}

async fn start_embedded_postgres() -> (TempDir, PgEmbed, String) {
    let tempdir = TempDir::new().expect("tempdir");
    let port = random_open_port();
    let settings = PgSettings {
        database_dir: tempdir.path().join("db"),
        port,
        user: "postgres".to_string(),
        password: "password".to_string(),
        auth_method: PgAuthMethod::MD5,
        persistent: false,
        timeout: Some(Duration::from_secs(20)),
        migration_dir: None,
    };
    let fetch_settings = PgFetchSettings {
        version: PG_V17,
        ..Default::default()
    };

    let mut pg = PgEmbed::new(settings, fetch_settings)
        .await
        .expect("pg embed");
    pg.setup().await.expect("setup");
    pg.start_db().await.expect("start");
    pg.create_database("ferrum_gate_test")
        .await
        .expect("database");
    let db_url = pg.full_db_uri("ferrum_gate_test");

    (tempdir, pg, db_url)
}

#[tokio::test]
async fn postgres_store_round_trips_keys_and_provider_credentials() {
    let (_dir, mut pg, db_url) = start_embedded_postgres().await;
    let store = PostgresPlatformStore::connect(&db_url, "integration-master-key", false)
        .await
        .expect("store");

    let tenant = store
        .create_tenant("integration".to_string(), "Integration Tenant".to_string())
        .await
        .expect("tenant");
    let created = store
        .create_tenant_api_key(tenant.id, "primary".to_string())
        .await
        .expect("created key");
    assert!(
        store
            .validate_gateway_api_key(&created.secret)
            .await
            .expect("auth")
            .is_some()
    );

    let rotated = store
        .rotate_tenant_api_key(tenant.id, created.record.id)
        .await
        .expect("rotated key");
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
        .revoke_tenant_api_key(tenant.id, created.record.id)
        .await
        .expect("revoked key");
    assert_eq!(revoked.status, storage::TenantApiKeyStatus::Revoked);
    assert!(
        store
            .validate_gateway_api_key(&rotated.secret)
            .await
            .expect("auth")
            .is_none()
    );

    let record = store
        .ingest_provider_account(
            ProviderAccountEnvelope {
                provider: "openai_codex".to_string(),
                credential_kind: "oauth_tokens".to_string(),
                payload_version: "v1".to_string(),
                credentials: serde_json::json!({
                    "access_token": "test-token",
                    "account_id": "acct_integration",
                    "api_base": "http://127.0.0.1:8787/v1",
                    "additional_headers": {
                        "x-project": "integration"
                    }
                }),
                metadata: serde_json::json!({
                    "email": "demo@example.com",
                    "plan_type": "plus"
                }),
                labels: vec!["shared".to_string()],
                tags: BTreeMap::from([("region".to_string(), "global".to_string())]),
            },
            ValidatedProviderAccount {
                provider_account_id: "acct_integration".to_string(),
                redacted_display: Some("d***@***".to_string()),
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
        .expect("provider account");

    let connection = store
        .resolve_provider_connection(record.id)
        .await
        .expect("connection")
        .expect("provider connection");
    assert_eq!(connection.bearer_token, "test-token");
    assert_eq!(connection.api_base, "http://127.0.0.1:8787/v1");
    assert_eq!(
        connection
            .additional_headers
            .get("x-project")
            .map(String::as_str),
        Some("integration")
    );

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

    store
        .revalidate_provider_account(
            record.id,
            ValidatedProviderAccount {
                provider_account_id: "acct_integration".to_string(),
                redacted_display: Some("d***@***".to_string()),
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

    let models = store
        .list_tenant_models(tenant.id)
        .await
        .expect("tenant models");
    assert!(models.iter().any(|model| model.id == "gpt-4.1-mini"));
    assert!(models.iter().any(|model| model.id == "codex-mini-latest"));

    pg.stop_db().await.expect("stop");
}
