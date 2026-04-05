use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use protocol_core::{InferenceRequest, InferenceResponse, InferenceStreamEvent, ModelDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, sync::Arc};
use thiserror::Error;
use uuid::Uuid;

pub type ProviderStream = BoxStream<'static, Result<InferenceStreamEvent, ProviderError>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderAccountEnvelope {
    pub provider: String,
    pub credential_kind: String,
    pub payload_version: String,
    pub credentials: Value,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatedProviderAccount {
    pub provider_account_id: String,
    pub redacted_display: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountCapabilities {
    pub models: Vec<ModelDescriptor>,
    pub supports_refresh: bool,
    pub supports_quota_probe: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaSnapshot {
    pub plan_label: Option<String>,
    pub remaining_requests_hint: Option<u64>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConnectionInfo {
    pub account_id: Uuid,
    pub provider_kind: String,
    pub credential_kind: String,
    pub api_base: String,
    pub bearer_token: String,
    pub model_override: Option<String>,
    pub additional_headers: BTreeMap<String, String>,
}

impl fmt::Debug for ProviderConnectionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConnectionInfo")
            .field("account_id", &self.account_id)
            .field("provider_kind", &self.provider_kind)
            .field("credential_kind", &self.credential_kind)
            .field("api_base", &self.api_base)
            .field("bearer_token", &"<redacted>")
            .field("model_override", &self.model_override)
            .field("additional_headers", &self.additional_headers)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    InvalidRequest,
    InvalidCredentials,
    RateLimited,
    UpstreamUnavailable,
    Unsupported,
}

#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub status_code: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ProviderError {
    #[must_use]
    pub fn new(kind: ProviderErrorKind, status_code: u16, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status_code,
            code: None,
        }
    }

    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
}

#[async_trait]
pub trait ProviderCredentialResolver: Send + Sync {
    async fn resolve_connection(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderConnectionInfo>, ProviderError>;
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> &'static str;

    async fn list_models(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<Vec<ModelDescriptor>, ProviderError>;

    async fn validate_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<ValidatedProviderAccount, ProviderError>;

    async fn probe_capabilities(
        &self,
        envelope: &ProviderAccountEnvelope,
        account: &ValidatedProviderAccount,
    ) -> Result<AccountCapabilities, ProviderError>;

    async fn probe_quota(
        &self,
        account: &ValidatedProviderAccount,
    ) -> Result<QuotaSnapshot, ProviderError>;

    async fn chat(&self, request: InferenceRequest) -> Result<InferenceResponse, ProviderError>;

    async fn responses(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ProviderError>;

    async fn stream_chat(&self, request: InferenceRequest)
    -> Result<ProviderStream, ProviderError>;

    async fn stream_responses(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError>;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    adapters: BTreeMap<String, Arc<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, adapter: Arc<dyn ProviderAdapter>) {
        self.adapters.insert(adapter.kind().to_string(), adapter);
    }

    #[must_use]
    pub fn get(&self, kind: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.get(kind).cloned()
    }

    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use futures::stream;
    use protocol_core::{
        FrontendProtocol, InferenceRequest, InferenceResponse, ModelCapability, ModelDescriptor,
    };

    struct DummyProvider;

    #[async_trait]
    impl ProviderAdapter for DummyProvider {
        fn kind(&self) -> &'static str {
            "dummy"
        }

        async fn list_models(
            &self,
            _envelope: &ProviderAccountEnvelope,
        ) -> Result<Vec<ModelDescriptor>, ProviderError> {
            Ok(vec![ModelDescriptor {
                id: "dummy-model".to_string(),
                route_group: "dummy".to_string(),
                provider_kind: "dummy".to_string(),
                upstream_model: "dummy-model".to_string(),
                capabilities: vec![ModelCapability::Chat],
            }])
        }

        async fn validate_credentials(
            &self,
            _envelope: &ProviderAccountEnvelope,
        ) -> Result<ValidatedProviderAccount, ProviderError> {
            Ok(ValidatedProviderAccount {
                provider_account_id: "dummy-account".to_string(),
                redacted_display: None,
                expires_at: None,
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
                supports_quota_probe: false,
            })
        }

        async fn probe_quota(
            &self,
            _account: &ValidatedProviderAccount,
        ) -> Result<QuotaSnapshot, ProviderError> {
            Ok(QuotaSnapshot {
                plan_label: Some("demo".to_string()),
                remaining_requests_hint: Some(10),
                checked_at: Utc::now(),
            })
        }

        async fn chat(
            &self,
            request: InferenceRequest,
        ) -> Result<InferenceResponse, ProviderError> {
            Ok(InferenceResponse::text(
                request.public_model,
                self.kind(),
                "ok",
            ))
        }

        async fn responses(
            &self,
            request: InferenceRequest,
        ) -> Result<InferenceResponse, ProviderError> {
            self.chat(request).await
        }

        async fn stream_chat(
            &self,
            _request: InferenceRequest,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(Box::pin(stream::empty()))
        }

        async fn stream_responses(
            &self,
            _request: InferenceRequest,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(Box::pin(stream::empty()))
        }
    }

    #[tokio::test]
    async fn registry_returns_registered_adapter() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(DummyProvider));

        let adapter = registry.get("dummy").expect("adapter should exist");
        let response = adapter
            .chat(InferenceRequest {
                protocol: FrontendProtocol::OpenAi,
                public_model: "dummy-model".to_string(),
                upstream_model: None,
                previous_response_id: None,
                stream: false,
                messages: vec![],
                tools: vec![],
                metadata: BTreeMap::new(),
            })
            .await
            .expect("chat should succeed");

        assert_eq!(response.provider_kind, "dummy");
    }
}
