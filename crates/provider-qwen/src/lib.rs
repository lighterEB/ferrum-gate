use async_stream::stream;
use async_trait::async_trait;
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;
use protocol_core::{
    ContentPart, FinishReason, InferenceRequest, InferenceResponse, InferenceStreamEvent,
    ModelCapability, ModelDescriptor, StreamEventKind, TokenUsage, ToolCall, ToolDefinition,
};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderAdapter, ProviderConnectionInfo,
    ProviderCredentialResolver, ProviderError, ProviderErrorKind, ProviderStream, QuotaSnapshot,
    RefreshedProviderCredentials, STREAM_IDLE_TIMEOUT, ValidatedProviderAccount,
};
use reqwest::{
    Client, Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

/// Qwen OAuth token endpoint (hardcoded from upstream source).
const QWEN_OAUTH_TOKEN_ENDPOINT: &str = "https://chat.qwen.ai/api/v1/oauth2/token";

/// Qwen OAuth client ID (hardcoded from upstream source).
const QWEN_OAUTH_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";

/// Default DashScope API base URL for OpenAI-compatible endpoints.
const QWEN_DEFAULT_API_BASE: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";

/// Known Qwen model IDs exposed through the gateway.
fn known_qwen_models() -> Vec<ModelDescriptor> {
    vec![
        ModelDescriptor {
            id: "qwen-max".to_string(),
            route_group: "qwen-qwen-max".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen-max".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen-plus".to_string(),
            route_group: "qwen-qwen-plus".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen-plus".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen-turbo".to_string(),
            route_group: "qwen-qwen-turbo".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen-turbo".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen-coder".to_string(),
            route_group: "qwen-qwen-coder".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen-coder".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
    ]
}

#[derive(Clone)]
pub struct QwenProvider {
    client: Client,
    resolver: Arc<dyn ProviderCredentialResolver>,
}

impl QwenProvider {
    #[must_use]
    pub fn shared(resolver: Arc<dyn ProviderCredentialResolver>) -> Arc<Self> {
        Arc::new(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(300))
                .build()
                .expect("build qwen client"),
            resolver,
        })
    }

    fn credential_secret(envelope: &ProviderAccountEnvelope) -> Option<&str> {
        envelope
            .credentials
            .get("access_token")
            .and_then(Value::as_str)
            .or_else(|| {
                envelope
                    .credentials
                    .get("bearer_token")
                    .and_then(Value::as_str)
            })
            .or_else(|| envelope.credentials.get("api_key").and_then(Value::as_str))
    }

    fn default_api_base() -> &'static str {
        QWEN_DEFAULT_API_BASE
    }

    fn string_field<'a>(envelope: &'a ProviderAccountEnvelope, key: &str) -> Option<&'a str> {
        envelope
            .credentials
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| envelope.metadata.get(key).and_then(Value::as_str))
    }

    fn api_base(envelope: &ProviderAccountEnvelope) -> String {
        Self::string_field(envelope, "api_base")
            .unwrap_or(Self::default_api_base())
            .trim_end_matches('/')
            .to_string()
    }

    fn extract_header_map(value: Option<&Value>) -> BTreeMap<String, String> {
        value
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn connection_from_envelope(
        &self,
        envelope: &ProviderAccountEnvelope,
        account_id: Uuid,
    ) -> Result<ProviderConnectionInfo, ProviderError> {
        if envelope.provider != self.kind() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                400,
                format!(
                    "expected provider {}, got {}",
                    self.kind(),
                    envelope.provider
                ),
            ));
        }
        let secret = Self::credential_secret(envelope).unwrap_or("").to_string();
        let api_base = Self::api_base(envelope);
        let additional_headers =
            Self::extract_header_map(envelope.credentials.get("additional_headers"));

        Ok(ProviderConnectionInfo {
            account_id,
            provider_kind: self.kind().to_string(),
            credential_kind: envelope.credential_kind.clone(),
            api_base,
            bearer_token: secret,
            model_override: None,
            additional_headers,
        })
    }

    async fn resolve_connection(
        &self,
        request: &InferenceRequest,
    ) -> Result<ProviderConnectionInfo, ProviderError> {
        let account_id = request
            .metadata
            .get("provider_account_id")
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    "request metadata missing provider_account_id",
                )
            })
            .and_then(|v| {
                Uuid::parse_str(v).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        400,
                        format!("invalid provider_account_id: {v}"),
                    )
                })
            })?;
        self.resolver
            .resolve_connection(account_id)
            .await?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    format!("no connection found for account {account_id}"),
                )
            })
    }

    fn decode_jwt_claims(token: &str) -> Option<Value> {
        let payload = token.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
            .or_else(|| URL_SAFE.decode(payload).ok())?;
        serde_json::from_slice(&bytes).ok()
    }

    fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
        let claims = Self::decode_jwt_claims(token)?;
        let seconds = claims.get("exp").and_then(Value::as_i64).or_else(|| {
            claims
                .get("exp")
                .and_then(Value::as_u64)
                .and_then(|value| i64::try_from(value).ok())
        })?;
        DateTime::from_timestamp(seconds, 0)
    }

    async fn ensure_success(response: Response) -> Result<Response, ProviderError> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let kind = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ProviderErrorKind::InvalidCredentials
            }
            StatusCode::TOO_MANY_REQUESTS => ProviderErrorKind::RateLimited,
            StatusCode::BAD_REQUEST => ProviderErrorKind::InvalidRequest,
            _ => ProviderErrorKind::UpstreamUnavailable,
        };
        let message =
            parse_error_message(&body).unwrap_or_else(|| body.chars().take(200).collect());
        Err(ProviderError::new(kind, status.as_u16(), message))
    }

    fn build_headers(connection: &ProviderConnectionInfo) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        for (key, value) in &connection.additional_headers {
            if let (Ok(name), Ok(val)) = (HeaderName::try_from(key), HeaderValue::try_from(value)) {
                headers.insert(name, val);
            }
        }
        Ok(headers)
    }

    fn chat_message_payload(message: &protocol_core::CanonicalMessage) -> Value {
        match message.role {
            protocol_core::MessageRole::System => {
                json!({ "role": "system", "content": message.content })
            }
            protocol_core::MessageRole::User => {
                if !message.parts.is_empty() {
                    let parts: Vec<Value> = message
                        .parts
                        .iter()
                        .map(|part| match part {
                            ContentPart::Text { text } => {
                                json!({ "type": "text", "text": text })
                            }
                            ContentPart::ImageUrl { image_url } => {
                                json!({ "type": "image_url", "image_url": { "url": image_url } })
                            }
                        })
                        .collect();
                    json!({ "role": "user", "content": parts })
                } else {
                    json!({ "role": "user", "content": message.content })
                }
            }
            protocol_core::MessageRole::Assistant => {
                let mut obj = json!({ "role": "assistant", "content": message.content });
                if !message.tool_calls.is_empty() {
                    obj["tool_calls"] = tool_calls_json(&message.tool_calls);
                }
                obj
            }
            protocol_core::MessageRole::Tool => {
                json!({
                    "role": "tool",
                    "tool_call_id": message.tool_call_id,
                    "content": message.content,
                })
            }
        }
    }

    fn tool_payload(tool: &ToolDefinition) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }
        })
    }
}

fn tool_calls_json(tool_calls: &[ToolCall]) -> Value {
    Value::Array(
        tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments,
                    }
                })
            })
            .collect(),
    )
}

fn parse_error_message(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    json.pointer("/error/message")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            json.get("message")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn estimate_usage(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    text.split_whitespace().count() as u32
}

fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        _ => FinishReason::Stop,
    }
}

fn transport_error(error: reqwest::Error) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::UpstreamUnavailable,
        502,
        error.to_string(),
    )
}

fn finalize_stream_response(
    public_model: String,
    final_model: Option<String>,
    provider_kind: String,
    output: String,
) -> InferenceResponse {
    let output_tokens = estimate_usage(&output);
    InferenceResponse {
        id: format!("chatcmpl_{}", Uuid::new_v4().simple()),
        model: final_model.unwrap_or(public_model),
        output_text: output,
        finish_reason: FinishReason::Stop,
        tool_calls: vec![],
        usage: TokenUsage {
            input_tokens: 0,
            output_tokens,
            total_tokens: output_tokens,
        },
        provider_kind,
        created_at: Utc::now(),
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    id: String,
    model: String,
    choices: Vec<ChatCompletionChoice>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: Option<ChatMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallPayload>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ToolCallPayload {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: ToolCallFunctionPayload,
}

#[derive(Debug, Deserialize)]
struct ToolCallFunctionPayload {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    model: Option<String>,
    choices: Vec<ChatCompletionChunkChoice>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatCompletionChunkChoice {
    delta: ChatCompletionDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatCompletionDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallPayload>>,
}

struct SseMessage {
    event: Option<String>,
    data: String,
}

fn parse_sse_frame(frame: &str) -> Option<SseMessage> {
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim().to_string());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseMessage {
        event,
        data: data_lines.join("\n"),
    })
}

#[async_trait]
impl ProviderAdapter for QwenProvider {
    fn kind(&self) -> &'static str {
        "qwen"
    }

    async fn list_models(
        &self,
        _envelope: &ProviderAccountEnvelope,
    ) -> Result<Vec<ModelDescriptor>, ProviderError> {
        Ok(known_qwen_models())
    }

    async fn validate_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<ValidatedProviderAccount, ProviderError> {
        let secret = Self::credential_secret(envelope).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidCredentials,
                401,
                "missing access_token, bearer_token, or api_key in credentials",
            )
        })?;

        let account_id = Uuid::new_v4();
        let connection = self.connection_from_envelope(envelope, account_id)?;
        let headers = Self::build_headers(&connection)?;
        let api_base = &connection.api_base;

        let response = self
            .client
            .get(format!("{api_base}/models"))
            .headers(headers)
            .bearer_auth(secret)
            .send()
            .await
            .map_err(transport_error)?;

        let response = Self::ensure_success(response).await?;
        let _body: Value = response.json().await.map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                format!("failed to parse models response: {e}"),
            )
        })?;

        let account_id_str = envelope
            .credentials
            .get("account_id")
            .or_else(|| envelope.credentials.get("resource_url"))
            .and_then(Value::as_str)
            .unwrap_or("qwen-account")
            .to_string();

        let redacted = secret
            .chars()
            .take(3)
            .chain("***".chars())
            .collect::<String>();

        let expires_at = envelope
            .credentials
            .get("access_token")
            .and_then(Value::as_str)
            .and_then(Self::jwt_expiry)
            .or_else(|| {
                envelope
                    .credentials
                    .get("expiry_date")
                    .and_then(Value::as_i64)
                    .and_then(|ms| {
                        DateTime::from_timestamp(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
                    })
            });

        Ok(ValidatedProviderAccount {
            provider_account_id: account_id_str,
            redacted_display: Some(redacted),
            expires_at,
        })
    }

    async fn probe_capabilities(
        &self,
        _envelope: &ProviderAccountEnvelope,
        _account: &ValidatedProviderAccount,
    ) -> Result<AccountCapabilities, ProviderError> {
        let models = known_qwen_models();
        Ok(AccountCapabilities {
            models,
            supports_refresh: true,
            supports_quota_probe: false,
        })
    }

    async fn refresh_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<RefreshedProviderCredentials, ProviderError> {
        let refresh_token = envelope
            .credentials
            .get("refresh_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    "credentials missing refresh_token",
                )
            })?;

        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", QWEN_OAUTH_CLIENT_ID),
        ];

        let response = self
            .client
            .post(QWEN_OAUTH_TOKEN_ENDPOINT)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await
            .map_err(transport_error)?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let error_kind =
                if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    ProviderErrorKind::InvalidCredentials
                } else {
                    ProviderErrorKind::UpstreamUnavailable
                };
            let message =
                parse_error_message(&body).unwrap_or_else(|| body.chars().take(200).collect());
            return Err(ProviderError::new(error_kind, status.as_u16(), message));
        }

        let refresh_response: QwenOAuthRefreshResponse = response.json().await.map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                format!("failed to parse OAuth refresh response: {e}"),
            )
        })?;

        let mut credentials = envelope.credentials.clone();
        if let Some(obj) = credentials.as_object_mut() {
            obj.insert(
                "access_token".to_string(),
                Value::String(refresh_response.access_token.clone()),
            );
            if let Some(rt) = &refresh_response.refresh_token {
                obj.insert("refresh_token".to_string(), Value::String(rt.clone()));
            }
        }

        let expires_at = if let Some(expires_in) = refresh_response.expires_in {
            Some(Utc::now() + TimeDelta::seconds(expires_in))
        } else {
            refresh_response
                .access_token
                .split('.')
                .nth(1)
                .and_then(|payload| {
                    let bytes = URL_SAFE_NO_PAD
                        .decode(payload)
                        .ok()
                        .or_else(|| URL_SAFE.decode(payload).ok())?;
                    let claims: Value = serde_json::from_slice(&bytes).ok()?;
                    let seconds = claims.get("exp").and_then(Value::as_i64).or_else(|| {
                        claims
                            .get("exp")
                            .and_then(Value::as_u64)
                            .and_then(|v| i64::try_from(v).ok())
                    })?;
                    DateTime::from_timestamp(seconds, 0)
                })
        };

        if let Some(obj) = credentials.as_object_mut() {
            obj.insert(
                "last_refresh".to_string(),
                Value::String(Utc::now().to_rfc3339()),
            );
            if let Some(exp) = expires_at {
                obj.insert("expires_at".to_string(), Value::String(exp.to_rfc3339()));
            }
        }

        Ok(RefreshedProviderCredentials {
            credentials,
            expires_at,
        })
    }

    async fn probe_quota(
        &self,
        _envelope: &ProviderAccountEnvelope,
        _account: &ValidatedProviderAccount,
    ) -> Result<QuotaSnapshot, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            501,
            "qwen does not expose a quota probe endpoint",
        ))
    }

    async fn chat(&self, request: InferenceRequest) -> Result<InferenceResponse, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let headers = Self::build_headers(&connection)?;
        let api_base = &connection.api_base;
        let model = request.upstream_model.as_deref().unwrap_or("qwen-max");

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(Self::tool_payload).collect());
        }

        let response = self
            .client
            .post(format!("{api_base}/chat/completions"))
            .headers(headers)
            .bearer_auth(&connection.bearer_token)
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;

        let response = Self::ensure_success(response).await?;
        let completion: ChatCompletionResponse = response.json().await.map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                format!("failed to parse chat completion: {e}"),
            )
        })?;

        let choice = completion.choices.into_iter().next().ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                "chat completion returned no choices",
            )
        })?;

        let message = choice.message.unwrap_or(ChatMessage {
            content: None,
            tool_calls: None,
        });

        let output_text = message.content.unwrap_or_default();
        let tool_calls = message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        let usage = completion.usage.unwrap_or_else(|| TokenUsage {
            input_tokens: estimate_usage(&output_text),
            output_tokens: estimate_usage(&output_text),
            total_tokens: estimate_usage(&output_text) * 2,
        });

        Ok(InferenceResponse {
            id: completion.id,
            model: completion.model,
            output_text,
            finish_reason: parse_finish_reason(choice.finish_reason.as_deref()),
            tool_calls,
            usage,
            provider_kind: self.kind().to_string(),
            created_at: Utc::now(),
        })
    }

    async fn responses(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ProviderError> {
        let mut req = request.clone();
        req.upstream_model = req.upstream_model.or_else(|| Some("qwen-max".to_string()));
        self.chat(req).await
    }

    async fn stream_chat(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let headers = Self::build_headers(&connection)?;
        let api_base = &connection.api_base;
        let public_model = request.public_model.clone();
        let provider_kind = self.kind().to_string();
        let model = request.upstream_model.as_deref().unwrap_or("qwen-max");

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(Self::tool_payload).collect());
        }

        let response = self
            .client
            .post(format!("{api_base}/chat/completions"))
            .headers(headers)
            .bearer_auth(&connection.bearer_token)
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;

        let response = Self::ensure_success(response).await?;
        Ok(Self::stream_chat_from_response(
            public_model,
            provider_kind,
            response,
        ))
    }

    async fn stream_responses(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let headers = Self::build_headers(&connection)?;
        let api_base = &connection.api_base;
        let public_model = request.public_model.clone();
        let provider_kind = self.kind().to_string();
        let model = request.upstream_model.as_deref().unwrap_or("qwen-max");

        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        let body = json!({
            "model": model,
            "input": messages,
            "stream": true,
        });

        let response = self
            .client
            .post(format!("{api_base}/responses"))
            .headers(headers)
            .bearer_auth(&connection.bearer_token)
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;

        let response = Self::ensure_success(response).await?;
        Ok(Self::stream_responses_from_response(
            public_model,
            provider_kind,
            response,
        ))
    }
}

impl QwenProvider {
    fn stream_chat_from_response(
        public_model: String,
        provider_kind: String,
        response: Response,
    ) -> ProviderStream {
        Box::pin(
            stream! {
                let mut bytes = response.bytes_stream();
                let mut buffer = String::new();
                let mut output = String::new();
                let mut final_model: Option<String> = None;

                loop {
                    let chunk = match tokio::time::timeout(
                        STREAM_IDLE_TIMEOUT,
                        bytes.next(),
                    )
                    .await
                    {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(_) => {
                            yield Err(ProviderError::new(
                                ProviderErrorKind::UpstreamUnavailable,
                                504,
                                "stream idle timeout exceeded".to_string(),
                            ));
                            return;
                        }
                    };
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            yield Err(transport_error(error));
                            return;
                        }
                    };

                    buffer.push_str(&String::from_utf8_lossy(&chunk).replace('\r', ""));

                    while let Some(index) = buffer.find("\n\n") {
                        let frame = buffer[..index].to_string();
                        buffer.drain(..index + 2);

                        let Some(message) = parse_sse_frame(&frame) else {
                            continue;
                        };

                        if message.data == "[DONE]" {
                            yield Ok(InferenceStreamEvent {
                                event: Some("message_stop".to_string()),
                                kind: StreamEventKind::Done,
                                delta: None,
                                response: Some(finalize_stream_response(
                                    public_model.clone(),
                                    final_model.clone(),
                                    provider_kind.clone(),
                                    output.clone(),
                                )),
                            });
                            return;
                        }

                        let chunk: ChatCompletionChunk = match serde_json::from_str(&message.data) {
                            Ok(chunk) => chunk,
                            Err(error) => {
                                yield Err(ProviderError::new(
                                    ProviderErrorKind::UpstreamUnavailable,
                                    502,
                                    format!("invalid chat completion stream payload: {error}"),
                                ));
                                return;
                            }
                        };

                        if let Some(model) = chunk.model {
                            final_model = Some(model);
                        }

                        for choice in chunk.choices {
                            if let Some(delta) = choice.delta.content {
                                output.push_str(&delta);
                                yield Ok(InferenceStreamEvent::delta(delta));
                            }
                        }
                    }
                }

                yield Ok(InferenceStreamEvent {
                    event: Some("message_stop".to_string()),
                    kind: StreamEventKind::Done,
                    delta: None,
                    response: Some(finalize_stream_response(
                        public_model,
                        final_model,
                        provider_kind,
                        output,
                    )),
                });
            }
            .boxed(),
        )
    }

    fn stream_responses_from_response(
        public_model: String,
        provider_kind: String,
        response: Response,
    ) -> ProviderStream {
        Box::pin(
            stream! {
                let mut bytes = response.bytes_stream();
                let mut buffer = String::new();
                let mut output = String::new();
                let final_model: Option<String> = None;

                loop {
                    let chunk = match tokio::time::timeout(
                        STREAM_IDLE_TIMEOUT,
                        bytes.next(),
                    )
                    .await
                    {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(_) => {
                            yield Err(ProviderError::new(
                                ProviderErrorKind::UpstreamUnavailable,
                                504,
                                "stream idle timeout exceeded".to_string(),
                            ));
                            return;
                        }
                    };
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            yield Err(transport_error(error));
                            return;
                        }
                    };

                    buffer.push_str(&String::from_utf8_lossy(&chunk).replace('\r', ""));

                    while let Some(index) = buffer.find("\n\n") {
                        let frame = buffer[..index].to_string();
                        buffer.drain(..index + 2);

                        let Some(message) = parse_sse_frame(&frame) else {
                            continue;
                        };

                        if message.data == "[DONE]" {
                            yield Ok(InferenceStreamEvent {
                                event: Some("message_stop".to_string()),
                                kind: StreamEventKind::Done,
                                delta: None,
                                response: Some(finalize_stream_response(
                                    public_model.clone(),
                                    final_model.clone(),
                                    provider_kind.clone(),
                                    output.clone(),
                                )),
                            });
                            return;
                        }

                        match message.event.as_deref() {
                            Some("response.output_text.delta") => {
                                let delta = serde_json::from_str::<ResponsesDeltaEvent>(&message.data)
                                    .map(|p: ResponsesDeltaEvent| p.delta)
                                    .unwrap_or_default();
                                output.push_str(&delta);
                                yield Ok(InferenceStreamEvent::delta(delta));
                            }
                            Some("response.completed") => {
                                let payload: Result<InferenceResponse, _> =
                                    serde_json::from_str(&message.data);
                                match payload {
                                    Ok(mut resp) => {
                                        if resp.output_text.is_empty() && !output.is_empty() {
                                            resp.output_text = output.clone();
                                        }
                                        yield Ok(InferenceStreamEvent {
                                            event: Some("message_stop".to_string()),
                                            kind: StreamEventKind::Done,
                                            delta: None,
                                            response: Some(resp),
                                        });
                                        return;
                                    }
                                    Err(error) => {
                                        yield Err(ProviderError::new(
                                            ProviderErrorKind::UpstreamUnavailable,
                                            502,
                                            format!("invalid responses completion payload: {error}"),
                                        ));
                                        return;
                                    }
                                }
                            }
                            Some("response.failed") => {
                                let msg = parse_error_message(&message.data)
                                    .unwrap_or_else(|| "responses request failed".to_string());
                                yield Err(ProviderError::new(
                                    ProviderErrorKind::UpstreamUnavailable,
                                    502,
                                    msg,
                                ));
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                yield Ok(InferenceStreamEvent {
                    event: Some("message_stop".to_string()),
                    kind: StreamEventKind::Done,
                    delta: None,
                    response: Some(finalize_stream_response(
                        public_model,
                        final_model,
                        provider_kind,
                        output,
                    )),
                });
            }
            .boxed(),
        )
    }
}

#[derive(Debug, Deserialize)]
struct ResponsesDeltaEvent {
    #[serde(default)]
    delta: String,
}

#[derive(Debug, Deserialize)]
struct QwenOAuthRefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::ProviderRegistry;

    #[test]
    fn kind_returns_qwen() {
        let store = storage::PlatformStore::demo();
        let provider = QwenProvider::shared(Arc::new(store));
        assert_eq!(provider.kind(), "qwen");
    }

    #[test]
    fn known_models_contains_expected_ids() {
        let models = known_qwen_models();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"qwen-max"));
        assert!(ids.contains(&"qwen-plus"));
        assert!(ids.contains(&"qwen-turbo"));
        assert!(ids.contains(&"qwen-coder"));
    }

    #[test]
    fn oauth_constants_are_correct() {
        assert_eq!(
            QWEN_OAUTH_TOKEN_ENDPOINT,
            "https://chat.qwen.ai/api/v1/oauth2/token"
        );
        assert_eq!(QWEN_OAUTH_CLIENT_ID, "f0304373b74a44d2b584a3fb70ca9e56");
        assert!(QWEN_DEFAULT_API_BASE.ends_with("/compatible-mode/v1"));
    }

    #[tokio::test]
    async fn registry_accepts_qwen_provider() {
        let store = storage::PlatformStore::demo();
        let provider = QwenProvider::shared(Arc::new(store));
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        assert!(registry.get("qwen").is_some());
    }
}
