use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use protocol_core::{
    FinishReason, InferenceRequest, InferenceResponse, InferenceStreamEvent, MessageRole,
    ModelCapability, ModelDescriptor, StreamEventKind, TokenUsage,
};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderAdapter, ProviderConnectionInfo,
    ProviderCredentialResolver, ProviderError, ProviderErrorKind, ProviderStream, QuotaSnapshot,
    ValidatedProviderAccount,
};
use reqwest::{
    Client, Response, StatusCode,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub struct AnthropicProvider {
    client: Client,
    resolver: Arc<dyn ProviderCredentialResolver>,
}

impl AnthropicProvider {
    #[must_use]
    pub fn shared(resolver: Arc<dyn ProviderCredentialResolver>) -> Arc<Self> {
        Arc::new(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(300))
                .build()
                .expect("build anthropic client"),
            resolver,
        })
    }

    fn credential_secret(envelope: &ProviderAccountEnvelope) -> Option<&str> {
        envelope
            .credentials
            .get("api_key")
            .and_then(Value::as_str)
            .or_else(|| {
                envelope
                    .credentials
                    .get("bearer_token")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                envelope
                    .credentials
                    .get("access_token")
                    .and_then(Value::as_str)
            })
    }

    fn string_field<'a>(envelope: &'a ProviderAccountEnvelope, key: &str) -> Option<&'a str> {
        envelope
            .credentials
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| envelope.metadata.get(key).and_then(Value::as_str))
    }

    fn effective_model(request: &InferenceRequest, connection: &ProviderConnectionInfo) -> String {
        connection
            .model_override
            .clone()
            .or_else(|| request.upstream_model.clone())
            .unwrap_or_else(|| request.public_model.clone())
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
                    "missing provider_account_id metadata",
                )
            })
            .and_then(|value| {
                Uuid::parse_str(value).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        400,
                        "provider_account_id is not a valid UUID",
                    )
                })
            })?;

        self.resolver
            .resolve_connection(account_id)
            .await?
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorKind::InvalidCredentials,
                    404,
                    "provider credentials not found",
                )
            })
    }

    fn messages_payload(request: &InferenceRequest) -> (Vec<Value>, Option<String>) {
        let mut messages = Vec::new();
        let mut system_parts = Vec::new();

        for message in &request.messages {
            match message.role {
                MessageRole::System => system_parts.push(message.content.clone()),
                MessageRole::User | MessageRole::Assistant => {
                    messages.push(json!({
                        "role": match message.role {
                            MessageRole::User => "user",
                            MessageRole::Assistant => "assistant",
                            _ => unreachable!(),
                        },
                        "content": [{
                            "type": "text",
                            "text": message.content,
                        }]
                    }));
                }
                MessageRole::Tool => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "text",
                            "text": message.content,
                        }]
                    }));
                }
            }
        }

        let system = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));
        (messages, system)
    }

    fn build_headers(
        &self,
        connection: &ProviderConnectionInfo,
    ) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        headers.insert(
            HeaderName::from_static("x-api-key"),
            HeaderValue::from_str(&connection.bearer_token).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    "invalid anthropic api key header value",
                )
            })?,
        );

        for (key, value) in &connection.additional_headers {
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    format!("invalid upstream header name: {key}"),
                )
            })?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    400,
                    format!("invalid upstream header value for: {key}"),
                )
            })?;
            headers.insert(name, value);
        }

        Ok(headers)
    }

    async fn send_messages_request(
        &self,
        request: &InferenceRequest,
        connection: &ProviderConnectionInfo,
        stream: bool,
    ) -> Result<Response, ProviderError> {
        let model = Self::effective_model(request, connection);
        let (messages, system) = Self::messages_payload(request);
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "max_tokens": 1024,
        });
        if stream {
            payload["stream"] = Value::Bool(true);
        }
        if let Some(system) = system {
            payload["system"] = Value::String(system);
        }

        let response = self
            .client
            .post(format!(
                "{}/messages",
                connection.api_base.trim_end_matches('/')
            ))
            .headers(self.build_headers(connection)?)
            .json(&payload)
            .send()
            .await
            .map_err(transport_error)?;

        ensure_success(response).await
    }

    fn stream_chat_from_response(
        public_model: String,
        provider_kind: String,
        response: Response,
    ) -> ProviderStream {
        Box::pin(async_stream::stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer = String::new();
            let mut output = String::new();
            let mut final_model = public_model.clone();
            let mut usage = TokenUsage::default();
            let mut finish_reason = FinishReason::Stop;

            loop {
                let chunk = match tokio::time::timeout(
                    provider_core::STREAM_IDLE_TIMEOUT,
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
                    let event: AnthropicStreamEnvelope = match serde_json::from_str(&message.data) {
                        Ok(event) => event,
                        Err(error) => {
                            yield Err(ProviderError::new(
                                ProviderErrorKind::UpstreamUnavailable,
                                502,
                                format!("invalid anthropic stream payload: {error}"),
                            ));
                            return;
                        }
                    };

                    match event.event_type.as_str() {
                        "message_start" => {
                            if let Some(message) = event.message {
                                final_model = message.model.unwrap_or_else(|| public_model.clone());
                                if let Some(message_usage) = message.usage {
                                    usage.input_tokens = message_usage.input_tokens.unwrap_or(0);
                                    usage.output_tokens = message_usage.output_tokens.unwrap_or(0);
                                    usage.total_tokens = usage.input_tokens + usage.output_tokens;
                                }
                            }
                        }
                        "content_block_delta" => {
                            if let Some(delta) = event
                                .delta
                                .as_ref()
                                .and_then(|delta| delta.get("text"))
                                .and_then(Value::as_str)
                                .map(ToString::to_string)
                            {
                                output.push_str(&delta);
                                yield Ok(InferenceStreamEvent {
                                    event: None,
                                    kind: StreamEventKind::ContentDelta,
                                    delta: Some(delta),
                                    response: None,
                                });
                            }
                        }
                        "message_delta" => {
                            if let Some(delta) = event
                                .delta
                                .as_ref()
                                .and_then(|delta| serde_json::from_value::<AnthropicMessageDelta>(delta.clone()).ok())
                            {
                                finish_reason = match delta.stop_reason.as_deref() {
                                    Some("max_tokens") => FinishReason::Length,
                                    Some("tool_use") => FinishReason::ToolCalls,
                                    Some("error") => FinishReason::Error,
                                    _ => FinishReason::Stop,
                                };
                            }
                            if let Some(delta_usage) = event.usage
                                && let Some(output_tokens) = delta_usage.output_tokens
                            {
                                usage.output_tokens = output_tokens;
                                usage.total_tokens = usage.input_tokens + usage.output_tokens;
                            }
                        }
                        "message_stop" => {
                            yield Ok(InferenceStreamEvent {
                                event: Some("message_stop".to_string()),
                                kind: StreamEventKind::Done,
                                delta: None,
                                response: Some(InferenceResponse {
                                    id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
                                    model: final_model.clone(),
                                    output_text: output.clone(),
                                    finish_reason: finish_reason.clone(),
                                    tool_calls: Vec::new(),
                                    usage: usage.clone(),
                                    provider_kind: provider_kind.clone(),
                                    created_at: Utc::now(),
                                }),
                            });
                            return;
                        }
                        _ => {}
                    }
                }
            }
        })
    }
}

#[async_trait]
impl ProviderAdapter for AnthropicProvider {
    fn kind(&self) -> &'static str {
        "anthropic"
    }

    async fn list_models(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let model = Self::string_field(envelope, "model")
            .unwrap_or("claude-opus-4-5")
            .to_string();
        Ok(vec![ModelDescriptor {
            id: model.clone(),
            route_group: format!("anthropic-{model}"),
            provider_kind: self.kind().to_string(),
            upstream_model: model,
            capabilities: vec![ModelCapability::Chat, ModelCapability::Responses],
        }])
    }

    async fn validate_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<ValidatedProviderAccount, ProviderError> {
        let _ = Self::credential_secret(envelope).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidCredentials,
                401,
                "anthropic account is missing an API key",
            )
        })?;

        Ok(ValidatedProviderAccount {
            provider_account_id: Self::string_field(envelope, "account_id")
                .unwrap_or("anthropic-account")
                .to_string(),
            redacted_display: Some("a***@***".to_string()),
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
        _envelope: &ProviderAccountEnvelope,
        _account: &ValidatedProviderAccount,
    ) -> Result<QuotaSnapshot, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            501,
            "anthropic quota probe not implemented",
        ))
    }

    async fn chat(&self, request: InferenceRequest) -> Result<InferenceResponse, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let response = self
            .send_messages_request(&request, &connection, false)
            .await?;
        let response: MessagesResponse = response.json().await.map_err(transport_error)?;
        let output_text = response
            .content
            .into_iter()
            .filter_map(|block| match block {
                MessagesContentBlock::Text { text } => Some(text),
                MessagesContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let output_tokens = response.usage.output_tokens;

        Ok(InferenceResponse {
            id: response.id,
            model: response
                .model
                .unwrap_or_else(|| Self::effective_model(&request, &connection)),
            output_text,
            finish_reason: match response.stop_reason.as_deref() {
                Some("max_tokens") => FinishReason::Length,
                Some("tool_use") => FinishReason::ToolCalls,
                Some("error") => FinishReason::Error,
                _ => FinishReason::Stop,
            },
            tool_calls: Vec::new(),
            usage: TokenUsage {
                input_tokens: response.usage.input_tokens,
                output_tokens,
                total_tokens: response.usage.input_tokens + output_tokens,
            },
            provider_kind: self.kind().to_string(),
            created_at: Utc::now(),
        })
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
        let connection = self.resolve_connection(&request).await?;
        let public_model = request.public_model.clone();
        let response = self
            .send_messages_request(&request, &connection, true)
            .await?;
        Ok(Self::stream_chat_from_response(
            public_model,
            self.kind().to_string(),
            response,
        ))
    }

    async fn stream_responses(
        &self,
        _request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::Unsupported,
            501,
            "anthropic streaming responses is not implemented",
        ))
    }
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Vec<MessagesContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    usage: MessagesUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MessagesContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct MessagesUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamEnvelope {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    message: Option<AnthropicStreamMessage>,
    #[serde(default)]
    delta: Option<Value>,
    #[serde(default)]
    usage: Option<AnthropicStreamUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicStreamUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicStreamUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

fn transport_error(error: reqwest::Error) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::UpstreamUnavailable,
        502,
        format!("anthropic transport error: {error}"),
    )
}

async fn ensure_success(response: Response) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let kind = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderErrorKind::InvalidCredentials,
        StatusCode::TOO_MANY_REQUESTS => ProviderErrorKind::RateLimited,
        _ if status.is_client_error() => ProviderErrorKind::InvalidRequest,
        _ => ProviderErrorKind::UpstreamUnavailable,
    };

    Err(ProviderError::new(
        kind,
        status.as_u16(),
        if body.is_empty() {
            format!("anthropic upstream returned {status}")
        } else {
            body
        },
    ))
}

struct ParsedSseFrame {
    data: String,
}

fn parse_sse_frame(frame: &str) -> Option<ParsedSseFrame> {
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        None
    } else {
        Some(ParsedSseFrame { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router, body::to_bytes, extract::Request, response::IntoResponse, routing::post,
    };
    use provider_core::ProviderCredentialResolver;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[derive(Clone)]
    struct TestResolver {
        connection: ProviderConnectionInfo,
    }

    #[async_trait]
    impl ProviderCredentialResolver for TestResolver {
        async fn resolve_connection(
            &self,
            account_id: Uuid,
        ) -> Result<Option<ProviderConnectionInfo>, ProviderError> {
            if self.connection.account_id == account_id {
                Ok(Some(self.connection.clone()))
            } else {
                Ok(None)
            }
        }
    }

    async fn spawn_anthropic_server() -> std::net::SocketAddr {
        async fn messages_handler(request: Request) -> impl IntoResponse {
            let api_key = request
                .headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let version = request
                .headers()
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let api_key = api_key.to_string();
            let version = version.to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(api_key, "anthropic-test-key");
            assert_eq!(version, "2023-06-01");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("claude-opus-4-5")
            );
            assert_eq!(
                body.get("system").and_then(Value::as_str),
                Some("You are helpful.")
            );
            assert_eq!(
                body.pointer("/messages/0/content/0/text")
                    .and_then(Value::as_str),
                Some("hello")
            );

            Json(json!({
                "id": "msg_123",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "text",
                    "text": "hello from anthropic"
                }],
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 3
                }
            }))
        }

        let app = Router::new().route("/v1/messages", post(messages_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        addr
    }

    async fn spawn_anthropic_streaming_server() -> std::net::SocketAddr {
        async fn messages_handler(request: Request) -> impl IntoResponse {
            let api_key = request
                .headers()
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(api_key, "anthropic-test-key");
            assert_eq!(body.get("stream").and_then(Value::as_bool), Some(true));

            let payload = concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream_123\",\"model\":\"claude-opus-4-5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
                "event: message_delta\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
                "event: message_stop\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new().route("/v1/messages", post(messages_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        addr
    }

    fn request_with_account(account_id: Uuid) -> InferenceRequest {
        InferenceRequest {
            protocol: protocol_core::FrontendProtocol::OpenAi,
            public_model: "opus-4.5".to_string(),
            upstream_model: Some("claude-opus-4-5".to_string()),
            previous_response_id: None,
            reasoning: None,
            stream: false,
            messages: vec![
                protocol_core::CanonicalMessage {
                    role: MessageRole::System,
                    content: "You are helpful.".to_string(),
                    parts: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                protocol_core::CanonicalMessage {
                    role: MessageRole::User,
                    content: "hello".to_string(),
                    parts: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ],
            tools: vec![],
            metadata: BTreeMap::from([("provider_account_id".to_string(), account_id.to_string())]),
        }
    }

    #[tokio::test]
    async fn chat_translates_openai_style_request_to_anthropic_messages_api() {
        let addr = spawn_anthropic_server().await;
        let account_id = Uuid::new_v4();
        let provider = AnthropicProvider::shared(Arc::new(TestResolver {
            connection: ProviderConnectionInfo {
                account_id,
                provider_kind: "anthropic".to_string(),
                credential_kind: "api_key".to_string(),
                api_base: format!("http://{addr}/v1"),
                bearer_token: "anthropic-test-key".to_string(),
                model_override: None,
                additional_headers: BTreeMap::new(),
            },
        }));

        let response = provider
            .chat(request_with_account(account_id))
            .await
            .expect("chat");
        assert_eq!(response.model, "claude-opus-4-5");
        assert_eq!(response.output_text, "hello from anthropic");
        assert_eq!(response.usage.total_tokens, 8);
    }

    #[tokio::test]
    async fn responses_reuses_messages_api_translation_for_non_streaming_proof() {
        let addr = spawn_anthropic_server().await;
        let account_id = Uuid::new_v4();
        let provider = AnthropicProvider::shared(Arc::new(TestResolver {
            connection: ProviderConnectionInfo {
                account_id,
                provider_kind: "anthropic".to_string(),
                credential_kind: "api_key".to_string(),
                api_base: format!("http://{addr}/v1"),
                bearer_token: "anthropic-test-key".to_string(),
                model_override: None,
                additional_headers: BTreeMap::new(),
            },
        }));

        let response = provider
            .responses(request_with_account(account_id))
            .await
            .expect("responses");
        assert_eq!(response.output_text, "hello from anthropic");
    }

    #[tokio::test]
    async fn stream_chat_translates_anthropic_sse_to_inference_stream_events() {
        let addr = spawn_anthropic_streaming_server().await;
        let account_id = Uuid::new_v4();
        let provider = AnthropicProvider::shared(Arc::new(TestResolver {
            connection: ProviderConnectionInfo {
                account_id,
                provider_kind: "anthropic".to_string(),
                credential_kind: "api_key".to_string(),
                api_base: format!("http://{addr}/v1"),
                bearer_token: "anthropic-test-key".to_string(),
                model_override: None,
                additional_headers: BTreeMap::new(),
            },
        }));

        let mut stream = provider
            .stream_chat(request_with_account(account_id))
            .await
            .expect("stream");
        let mut deltas = Vec::new();
        let mut final_response = None;
        while let Some(item) = stream.next().await {
            let event = item.expect("event");
            match event.kind {
                protocol_core::StreamEventKind::ContentDelta => {
                    deltas.push(event.delta.expect("delta"));
                }
                protocol_core::StreamEventKind::Done => {
                    final_response = event.response;
                }
                _ => {}
            }
        }

        assert_eq!(deltas.join(""), "hello world");
        let final_response = final_response.expect("final response");
        assert_eq!(final_response.output_text, "hello world");
        assert_eq!(final_response.model, "claude-opus-4-5");
    }

    #[test]
    fn stream_idle_timeout_error_has_correct_shape() {
        let error = ProviderError::new(
            ProviderErrorKind::UpstreamUnavailable,
            504,
            "stream idle timeout exceeded".to_string(),
        );
        assert_eq!(error.kind, ProviderErrorKind::UpstreamUnavailable);
        assert_eq!(error.status_code, 504);
        assert_eq!(error.message, "stream idle timeout exceeded");
    }
}
