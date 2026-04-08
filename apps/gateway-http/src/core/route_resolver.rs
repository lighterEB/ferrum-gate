use crate::core::types::{ExecutionError, RequestedCapability, ResolvedRoute, RouteTarget};
use protocol_core::ModelCapability;
use storage::PlatformStore;

pub(crate) struct RouteResolver;

impl RouteResolver {
    pub(crate) async fn resolve(
        store: &PlatformStore,
        public_model: &str,
        requested_capability: RequestedCapability,
    ) -> Result<ResolvedRoute, ExecutionError> {
        let route_groups = store
            .list_route_groups_for_public_model(public_model)
            .await
            .map_err(|error| ExecutionError::Internal(error.to_string()))?;
        let route_group = select_primary_route_group(store, public_model, &route_groups).await?;
        let fallback_chain = store
            .list_route_group_fallbacks(route_group.id)
            .await
            .map_err(|error| ExecutionError::Internal(error.to_string()))?;
        let fallback_chain = fallback_chain
            .into_iter()
            .filter_map(|record| {
                route_groups
                    .iter()
                    .find(|group| group.id == record.fallback_route_group_id)
                    .map(|group| RouteTarget {
                        route_group_id: group.id,
                        provider_kind: group.provider_kind.clone(),
                        upstream_model: group.upstream_model.clone(),
                    })
            })
            .collect();

        Ok(ResolvedRoute {
            route_group_id: route_group.id,
            public_model: route_group.public_model,
            provider_kind: route_group.provider_kind,
            upstream_model: route_group.upstream_model,
            fallback_chain,
            capability_contract: match requested_capability {
                RequestedCapability::Chat => vec![ModelCapability::Chat],
                RequestedCapability::Responses => vec![ModelCapability::Responses],
            },
        })
    }
}

async fn select_primary_route_group(
    store: &PlatformStore,
    public_model: &str,
    route_groups: &[storage::RouteGroupRecord],
) -> Result<storage::RouteGroupRecord, ExecutionError> {
    for route_group in route_groups {
        let fallbacks = store
            .list_route_group_fallbacks(route_group.id)
            .await
            .map_err(|error| ExecutionError::Internal(error.to_string()))?;
        if !fallbacks.is_empty() {
            return Ok(route_group.clone());
        }
    }

    store
        .resolve_route_group(public_model)
        .await
        .map_err(|error| ExecutionError::Internal(error.to_string()))?
        .ok_or(ExecutionError::UnknownModel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol_core::{ModelCapability, ModelDescriptor};
    use provider_core::{AccountCapabilities, ProviderAccountEnvelope, ValidatedProviderAccount};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn resolves_public_model_to_expected_provider_route() {
        let store = PlatformStore::demo();
        let account = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "anthropic".to_string(),
                    credential_kind: "api_key".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "api_key": "anthropic-key",
                        "api_base": "http://anthropic.example/v1"
                    }),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_anthropic".to_string(),
                    redacted_display: Some("a***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "opus-4.5".to_string(),
                        route_group: "opus-4.5".to_string(),
                        provider_kind: "anthropic".to_string(),
                        upstream_model: "claude-opus-4-5".to_string(),
                        capabilities: vec![ModelCapability::Chat, ModelCapability::Responses],
                    }],
                    supports_refresh: false,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("account");
        let route_group = store
            .create_route_group(
                "opus-4.5".to_string(),
                "anthropic".to_string(),
                "claude-opus-4-5".to_string(),
            )
            .await
            .expect("route group");
        store
            .bind_provider_account(route_group.id, account.id, 100, 8)
            .await
            .expect("binding");

        let resolved = RouteResolver::resolve(&store, "opus-4.5", RequestedCapability::Chat)
            .await
            .expect("resolve");

        assert_eq!(resolved.provider_kind, "anthropic");
        assert_eq!(resolved.upstream_model, "claude-opus-4-5");
        assert_eq!(resolved.public_model, "opus-4.5");
        assert_eq!(resolved.capability_contract, vec![ModelCapability::Chat]);
    }

    #[tokio::test]
    async fn resolves_public_model_with_ordered_fallback_routes() {
        let store = PlatformStore::demo();

        let primary_account = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "anthropic".to_string(),
                    credential_kind: "api_key".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "api_key": "anthropic-key",
                        "api_base": "http://anthropic.example/v1"
                    }),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_primary".to_string(),
                    redacted_display: Some("a***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "assistant/default".to_string(),
                        route_group: "assistant-default-anthropic".to_string(),
                        provider_kind: "anthropic".to_string(),
                        upstream_model: "claude-sonnet-4-5".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: false,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("primary account");
        let fallback_account = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "access_token": "codex-key",
                        "api_base": "http://codex.example/v1"
                    }),
                    metadata: json!({}),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                ValidatedProviderAccount {
                    provider_account_id: "acct_fallback".to_string(),
                    redacted_display: Some("c***".to_string()),
                    expires_at: None,
                },
                AccountCapabilities {
                    models: vec![ModelDescriptor {
                        id: "assistant/default".to_string(),
                        route_group: "assistant-default-codex".to_string(),
                        provider_kind: "openai_codex".to_string(),
                        upstream_model: "gpt-5-codex".to_string(),
                        capabilities: vec![ModelCapability::Chat],
                    }],
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("fallback account");

        let primary_route = store
            .create_route_group(
                "assistant/default".to_string(),
                "anthropic".to_string(),
                "claude-sonnet-4-5".to_string(),
            )
            .await
            .expect("primary route");
        let fallback_route = store
            .create_route_group(
                "assistant/default".to_string(),
                "openai_codex".to_string(),
                "gpt-5-codex".to_string(),
            )
            .await
            .expect("fallback route");
        store
            .bind_provider_account(primary_route.id, primary_account.id, 100, 8)
            .await
            .expect("primary binding");
        store
            .bind_provider_account(fallback_route.id, fallback_account.id, 100, 8)
            .await
            .expect("fallback binding");
        store
            .add_route_group_fallback(primary_route.id, fallback_route.id, 0)
            .await
            .expect("fallback relation");

        let resolved =
            RouteResolver::resolve(&store, "assistant/default", RequestedCapability::Chat)
                .await
                .expect("resolve");

        assert_eq!(resolved.provider_kind, "anthropic");
        assert_eq!(resolved.fallback_chain.len(), 1);
        assert_eq!(resolved.fallback_chain[0].provider_kind, "openai_codex");
        assert_eq!(resolved.fallback_chain[0].upstream_model, "gpt-5-codex");
    }
}
