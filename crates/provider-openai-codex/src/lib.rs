use async_stream::stream;
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use protocol_core::{
    InferenceRequest, InferenceResponse, InferenceStreamEvent, ModelCapability, ModelDescriptor,
};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderAdapter, ProviderError,
    ProviderErrorKind, ProviderStream, QuotaSnapshot, ValidatedProviderAccount,
};
use std::sync::Arc;

#[derive(Default)]
pub struct OpenAiCodexProvider;

impl OpenAiCodexProvider {
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self)
    }

    fn supported_models(&self) -> Vec<ModelDescriptor> {
        vec![
            ModelDescriptor {
                id: "gpt-4.1-mini".to_string(),
                route_group: "openai-gpt-4-1-mini".to_string(),
                provider_kind: self.kind().to_string(),
                upstream_model: "gpt-4.1-mini".to_string(),
                capabilities: vec![
                    ModelCapability::Chat,
                    ModelCapability::Responses,
                    ModelCapability::Streaming,
                ],
            },
            ModelDescriptor {
                id: "codex-mini-latest".to_string(),
                route_group: "openai-codex-mini".to_string(),
                provider_kind: self.kind().to_string(),
                upstream_model: "codex-mini-latest".to_string(),
                capabilities: vec![
                    ModelCapability::Chat,
                    ModelCapability::Responses,
                    ModelCapability::Streaming,
                ],
            },
        ]
    }

    fn render_stub_text(&self, request: &InferenceRequest) -> String {
        let prompt = request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, protocol_core::MessageRole::User))
            .map(|message| message.content.as_str())
            .unwrap_or("empty prompt");

        format!("FerrumGate stub reply from {} for: {prompt}", self.kind())
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiCodexProvider {
    fn kind(&self) -> &'static str {
        "openai_codex"
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        Ok(self.supported_models())
    }

    async fn validate_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<ValidatedProviderAccount, ProviderError> {
        if envelope.provider != self.kind() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                400,
                "provider kind does not match openai_codex",
            ));
        }

        let access_token = envelope
            .credentials
            .get("access_token")
            .and_then(serde_json::Value::as_str);
        let account_id = envelope
            .credentials
            .get("account_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                envelope
                    .metadata
                    .get("external_account_id")
                    .and_then(serde_json::Value::as_str)
            });

        if access_token.is_none() || account_id.is_none() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                400,
                "credentials must include access_token and account_id",
            ));
        }

        let redacted_display = envelope
            .metadata
            .get("email")
            .and_then(serde_json::Value::as_str)
            .map(|email| {
                let mut chars = email.chars();
                match chars.next() {
                    Some(first) => format!("{first}***@***"),
                    None => "***".to_string(),
                }
            });

        let expires_at = envelope
            .metadata
            .get("expired")
            .or_else(|| envelope.metadata.get("expires_at"))
            .and_then(serde_json::Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));

        Ok(ValidatedProviderAccount {
            provider_account_id: account_id.expect("account_id checked").to_string(),
            redacted_display,
            expires_at,
        })
    }

    async fn probe_capabilities(
        &self,
        _account: &ValidatedProviderAccount,
    ) -> Result<AccountCapabilities, ProviderError> {
        Ok(AccountCapabilities {
            models: self.supported_models(),
            supports_refresh: true,
            supports_quota_probe: true,
        })
    }

    async fn probe_quota(
        &self,
        _account: &ValidatedProviderAccount,
    ) -> Result<QuotaSnapshot, ProviderError> {
        Ok(QuotaSnapshot {
            plan_label: Some("stub-plan".to_string()),
            remaining_requests_hint: Some(1_000),
            checked_at: Utc::now(),
        })
    }

    async fn chat(&self, request: InferenceRequest) -> Result<InferenceResponse, ProviderError> {
        let output_text = self.render_stub_text(&request);
        Ok(InferenceResponse::text(
            request.public_model,
            self.kind(),
            output_text,
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
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let response = self.chat(request).await?;
        let words: Vec<String> = response
            .output_text
            .split_whitespace()
            .map(|word| format!("{word} "))
            .collect();

        Ok(Box::pin(
            stream! {
              for word in words {
                yield Ok(InferenceStreamEvent::delta(word));
              }
              yield Ok(InferenceStreamEvent::done(response));
            }
            .boxed(),
        ))
    }

    async fn stream_responses(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        self.stream_chat(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::ProviderAccountEnvelope;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn validates_expected_openai_shape() {
        let provider = OpenAiCodexProvider;
        let validated = provider
            .validate_credentials(&ProviderAccountEnvelope {
                provider: "openai_codex".to_string(),
                credential_kind: "oauth_tokens".to_string(),
                payload_version: "v1".to_string(),
                credentials: json!({
                  "access_token": "token",
                  "account_id": "acct_123"
                }),
                metadata: json!({ "email": "demo@example.com" }),
                labels: vec![],
                tags: BTreeMap::new(),
            })
            .await
            .expect("credentials should validate");

        assert_eq!(validated.provider_account_id, "acct_123");
    }
}
