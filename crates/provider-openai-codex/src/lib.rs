use async_stream::stream;
use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use protocol_core::{
    ContentPart, FinishReason, InferenceRequest, InferenceResponse, InferenceStreamEvent,
    ModelCapability, ModelDescriptor, StreamEventKind, TokenUsage, ToolCall, ToolDefinition,
};
use provider_core::{
    AccountCapabilities, ProviderAccountEnvelope, ProviderAdapter, ProviderConnectionInfo,
    ProviderCredentialResolver, ProviderError, ProviderErrorKind, ProviderStream, QuotaSnapshot,
    ValidatedProviderAccount,
};
use reqwest::{
    Client, Response, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

pub struct OpenAiCodexProvider {
    client: Client,
    resolver: Arc<dyn ProviderCredentialResolver>,
}

impl OpenAiCodexProvider {
    #[must_use]
    pub fn shared(resolver: Arc<dyn ProviderCredentialResolver>) -> Arc<Self> {
        Arc::new(Self {
            client: Client::new(),
            resolver,
        })
    }

    #[cfg(test)]
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

    fn credential_secret<'a>(envelope: &'a ProviderAccountEnvelope) -> Option<&'a str> {
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
        "https://api.openai.com/v1"
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
    ) -> Result<ProviderConnectionInfo, ProviderError> {
        if envelope.provider != self.kind() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                400,
                "provider kind does not match openai_codex",
            ));
        }

        let bearer_token = Self::credential_secret(envelope).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                400,
                "credentials must include access_token, bearer_token, or api_key",
            )
        })?;
        let api_base = envelope
            .credentials
            .get("api_base")
            .and_then(Value::as_str)
            .or_else(|| envelope.metadata.get("api_base").and_then(Value::as_str))
            .unwrap_or(Self::default_api_base());
        let model_override = envelope
            .credentials
            .get("model_override")
            .and_then(Value::as_str)
            .or_else(|| {
                envelope
                    .metadata
                    .get("model_override")
                    .and_then(Value::as_str)
            })
            .map(ToString::to_string);

        let mut additional_headers = BTreeMap::new();
        additional_headers.extend(Self::extract_header_map(
            envelope.metadata.get("additional_headers"),
        ));
        additional_headers.extend(Self::extract_header_map(
            envelope.credentials.get("additional_headers"),
        ));

        Ok(ProviderConnectionInfo {
            account_id: Uuid::nil(),
            provider_kind: self.kind().to_string(),
            credential_kind: envelope.credential_kind.clone(),
            api_base: api_base.trim_end_matches('/').to_string(),
            bearer_token: bearer_token.to_string(),
            model_override,
            additional_headers,
        })
    }

    fn descriptor_for_model(&self, model_id: String) -> ModelDescriptor {
        let route_group = model_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("-");

        ModelDescriptor {
            route_group,
            upstream_model: model_id.clone(),
            id: model_id,
            provider_kind: self.kind().to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        }
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

    fn effective_model(
        &self,
        request: &InferenceRequest,
        connection: &ProviderConnectionInfo,
    ) -> String {
        connection
            .model_override
            .clone()
            .or_else(|| request.upstream_model.clone())
            .unwrap_or_else(|| request.public_model.clone())
    }

    fn endpoint_url(base: &str, path: &str) -> String {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn build_headers(
        &self,
        connection: &ProviderConnectionInfo,
    ) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
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

    fn uses_chatgpt_codex_endpoint(connection: &ProviderConnectionInfo) -> bool {
        connection.api_base.contains("/backend-api/codex")
    }

    fn codex_instructions(request: &InferenceRequest) -> String {
        let instructions = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, protocol_core::MessageRole::System))
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        if instructions.is_empty() {
            "You are Codex.".to_string()
        } else {
            instructions
        }
    }

    fn codex_input_items(request: &InferenceRequest) -> Vec<Value> {
        request
            .messages
            .iter()
            .filter(|message| !matches!(message.role, protocol_core::MessageRole::System))
            .flat_map(codex_input_items_for_message)
            .collect()
    }

    fn tool_payloads(request: &InferenceRequest) -> Vec<Value> {
        request.tools.iter().map(tool_payload).collect()
    }

    fn message_parts(message: &protocol_core::CanonicalMessage) -> Vec<ContentPart> {
        if !message.parts.is_empty() {
            return message.parts.clone();
        }

        if message.content.is_empty() {
            return Vec::new();
        }

        vec![ContentPart::Text {
            text: message.content.clone(),
        }]
    }

    async fn fetch_models_with_connection(
        &self,
        connection: &ProviderConnectionInfo,
    ) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let response = self
            .client
            .get(Self::endpoint_url(&connection.api_base, "models"))
            .bearer_auth(&connection.bearer_token)
            .headers(self.build_headers(connection)?)
            .send()
            .await
            .map_err(transport_error)?;
        let response = ensure_success(response).await?;
        let body: ModelsApiResponse = response.json().await.map_err(transport_error)?;

        Ok(body
            .data
            .into_iter()
            .map(|model| self.descriptor_for_model(model.id))
            .collect())
    }

    async fn send_chat_request_with_connection(
        &self,
        request: &InferenceRequest,
        connection: &ProviderConnectionInfo,
        stream: bool,
    ) -> Result<Response, ProviderError> {
        let model = self.effective_model(request, &connection);
        let payload = json!({
            "model": model,
            "messages": request.messages.iter().map(chat_message_payload).collect::<Vec<_>>(),
            "tools": Self::tool_payloads(request),
            "stream": stream
        });

        let response = self
            .client
            .post(Self::endpoint_url(&connection.api_base, "chat/completions"))
            .bearer_auth(&connection.bearer_token)
            .headers(self.build_headers(&connection)?)
            .json(&payload)
            .send()
            .await
            .map_err(transport_error)?;

        ensure_success(response).await
    }

    async fn send_responses_request_with_connection(
        &self,
        request: &InferenceRequest,
        connection: &ProviderConnectionInfo,
        stream: bool,
    ) -> Result<Response, ProviderError> {
        let model = self.effective_model(request, &connection);
        let tools = Self::tool_payloads(request);
        let payload = if Self::uses_chatgpt_codex_endpoint(connection) {
            let mut payload = json!({
                "model": model,
                "instructions": Self::codex_instructions(request),
                "input": Self::codex_input_items(request),
                "tools": tools,
                "stream": true,
                "store": false
            });
            if let Some(previous_response_id) = &request.previous_response_id {
                payload["previous_response_id"] = Value::String(previous_response_id.clone());
            }
            payload
        } else {
            let mut payload = json!({
                "model": model,
                "input": Self::codex_input_items(request),
                "tools": tools,
                "stream": stream
            });
            if let Some(previous_response_id) = &request.previous_response_id {
                payload["previous_response_id"] = Value::String(previous_response_id.clone());
            }
            payload
        };

        let response = self
            .client
            .post(Self::endpoint_url(&connection.api_base, "responses"))
            .bearer_auth(&connection.bearer_token)
            .headers(self.build_headers(&connection)?)
            .json(&payload)
            .send()
            .await
            .map_err(transport_error)?;

        ensure_success(response).await
    }

    async fn collect_responses_response(
        public_model: String,
        provider_kind: String,
        response: Response,
    ) -> Result<InferenceResponse, ProviderError> {
        let mut stream =
            Self::stream_responses_from_response(public_model, provider_kind, response);

        while let Some(item) = stream.next().await {
            let event = item?;
            if matches!(event.kind, StreamEventKind::Done) {
                return event.response.ok_or_else(|| {
                    ProviderError::new(
                        ProviderErrorKind::UpstreamUnavailable,
                        502,
                        "responses stream completed without a final response",
                    )
                });
            }
        }

        Err(ProviderError::new(
            ProviderErrorKind::UpstreamUnavailable,
            502,
            "responses stream ended unexpectedly",
        ))
    }

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

                while let Some(chunk) = bytes.next().await {
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

                        if let Some(choice) = chunk.choices.into_iter().next()
                            && let Some(delta) = choice.delta.content
                        {
                            output.push_str(&delta);
                            yield Ok(InferenceStreamEvent::delta(delta));
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

                while let Some(chunk) = bytes.next().await {
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
                                    .map(|payload| payload.delta)
                                    .unwrap_or_default();
                                output.push_str(&delta);
                                yield Ok(InferenceStreamEvent::delta(delta));
                            }
                            Some("response.completed") => {
                                let payload = match parse_responses_completion_payload(
                                    public_model.clone(),
                                    provider_kind.clone(),
                                    &message.data,
                                ) {
                                    Ok(payload) => payload,
                                    Err(error) => {
                                        yield Err(error);
                                        return;
                                    }
                                };
                                yield Ok(InferenceStreamEvent {
                                    event: Some("message_stop".to_string()),
                                    kind: StreamEventKind::Done,
                                    delta: None,
                                    response: Some(payload),
                                });
                                return;
                            }
                            Some("response.failed") => {
                                yield Err(parse_stream_error(&message.data));
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

#[async_trait]
impl ProviderAdapter for OpenAiCodexProvider {
    fn kind(&self) -> &'static str {
        "openai_codex"
    }

    async fn list_models(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let connection = self.connection_from_envelope(envelope)?;
        self.fetch_models_with_connection(&connection).await
    }

    async fn validate_credentials(
        &self,
        envelope: &ProviderAccountEnvelope,
    ) -> Result<ValidatedProviderAccount, ProviderError> {
        let secret = Self::credential_secret(envelope);
        self.connection_from_envelope(envelope)?;

        let external_account_id = envelope
            .credentials
            .get("account_id")
            .and_then(Value::as_str)
            .or_else(|| {
                envelope
                    .metadata
                    .get("external_account_id")
                    .and_then(Value::as_str)
            })
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "acct_{}",
                    Uuid::new_v5(&Uuid::NAMESPACE_URL, secret.expect("checked").as_bytes())
                        .simple()
                )
            });

        let redacted_display = envelope
            .metadata
            .get("email")
            .and_then(Value::as_str)
            .map(redact_email);

        let expires_at = envelope
            .credentials
            .get("expired")
            .or_else(|| envelope.credentials.get("expires_at"))
            .or_else(|| envelope.metadata.get("expired"))
            .or_else(|| envelope.metadata.get("expires_at"))
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));

        Ok(ValidatedProviderAccount {
            provider_account_id: external_account_id,
            redacted_display,
            expires_at,
        })
    }

    async fn probe_capabilities(
        &self,
        envelope: &ProviderAccountEnvelope,
        _account: &ValidatedProviderAccount,
    ) -> Result<AccountCapabilities, ProviderError> {
        Ok(AccountCapabilities {
            models: self.list_models(envelope).await?,
            supports_refresh: true,
            supports_quota_probe: false,
        })
    }

    async fn probe_quota(
        &self,
        _account: &ValidatedProviderAccount,
    ) -> Result<QuotaSnapshot, ProviderError> {
        Ok(QuotaSnapshot {
            plan_label: Some("unknown".to_string()),
            remaining_requests_hint: None,
            checked_at: Utc::now(),
        })
    }

    async fn chat(&self, request: InferenceRequest) -> Result<InferenceResponse, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let public_model = request.public_model.clone();
        if Self::uses_chatgpt_codex_endpoint(&connection) {
            let response = self
                .send_responses_request_with_connection(&request, &connection, true)
                .await?;
            return Self::collect_responses_response(
                public_model,
                self.kind().to_string(),
                response,
            )
            .await;
        }

        let response = self
            .send_chat_request_with_connection(&request, &connection, false)
            .await?;
        let response: ChatCompletionResponse = response.json().await.map_err(transport_error)?;
        Ok(parse_chat_response(
            public_model,
            self.kind().to_string(),
            response,
        ))
    }

    async fn responses(
        &self,
        request: InferenceRequest,
    ) -> Result<InferenceResponse, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let public_model = request.public_model.clone();
        if Self::uses_chatgpt_codex_endpoint(&connection) {
            let response = self
                .send_responses_request_with_connection(&request, &connection, true)
                .await?;
            return Self::collect_responses_response(
                public_model,
                self.kind().to_string(),
                response,
            )
            .await;
        }

        let response = self
            .send_responses_request_with_connection(&request, &connection, false)
            .await?;
        let response: ResponsesApiResponse = response.json().await.map_err(transport_error)?;
        Ok(parse_responses_response(
            public_model,
            self.kind().to_string(),
            response,
        ))
    }

    async fn stream_chat(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let public_model = request.public_model.clone();
        if Self::uses_chatgpt_codex_endpoint(&connection) {
            let response = self
                .send_responses_request_with_connection(&request, &connection, true)
                .await?;
            return Ok(Self::stream_responses_from_response(
                public_model,
                self.kind().to_string(),
                response,
            ));
        }

        let response = self
            .send_chat_request_with_connection(&request, &connection, true)
            .await?;
        Ok(Self::stream_chat_from_response(
            public_model,
            self.kind().to_string(),
            response,
        ))
    }

    async fn stream_responses(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let public_model = request.public_model.clone();
        let response = self
            .send_responses_request_with_connection(&request, &connection, true)
            .await?;
        Ok(Self::stream_responses_from_response(
            public_model,
            self.kind().to_string(),
            response,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChatCompletionToolCall>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChatChunkChoice>,
}

#[derive(Debug, Deserialize)]
struct ModelsApiResponse {
    #[serde(default)]
    data: Vec<ModelSummary>,
}

#[derive(Debug, Deserialize)]
struct ModelSummary {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ChatChunkChoice {
    delta: ChatChunkDelta,
}

#[derive(Debug, Deserialize)]
struct ChatChunkDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionToolCall {
    id: String,
    function: ChatCompletionFunctionCall,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesApiResponse {
    id: Option<String>,
    model: Option<String>,
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesOutputContent>,
}

#[derive(Debug, Deserialize)]
struct ResponsesOutputContent {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesDeltaEvent {
    #[serde(default)]
    delta: String,
}

#[derive(Debug, Deserialize)]
struct ResponsesCompletedEvent {
    response: ResponsesApiResponse,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

#[derive(Debug)]
struct SseMessage {
    event: Option<String>,
    data: String,
}

fn role_label(role: &protocol_core::MessageRole) -> &'static str {
    match role {
        protocol_core::MessageRole::System => "system",
        protocol_core::MessageRole::User => "user",
        protocol_core::MessageRole::Assistant => "assistant",
        protocol_core::MessageRole::Tool => "tool",
    }
}

fn tool_payload(tool: &ToolDefinition) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

fn chat_message_payload(message: &protocol_core::CanonicalMessage) -> Value {
    let mut payload = json!({
        "role": role_label(&message.role),
        "content": chat_content_value(message),
    });

    if !message.tool_calls.is_empty() {
        payload["tool_calls"] = Value::Array(
            message
                .tool_calls
                .iter()
                .map(|tool_call| {
                    json!({
                        "id": tool_call.id,
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": tool_call.arguments,
                        }
                    })
                })
                .collect(),
        );
    }

    if let Some(tool_call_id) = &message.tool_call_id {
        payload["tool_call_id"] = Value::String(tool_call_id.clone());
    }

    payload
}

fn chat_content_value(message: &protocol_core::CanonicalMessage) -> Value {
    let parts = OpenAiCodexProvider::message_parts(message);
    if parts.len() == 1
        && let Some(ContentPart::Text { text }) = parts.first()
    {
        return Value::String(text.clone());
    }

    Value::Array(parts.iter().map(chat_content_part_value).collect())
}

fn chat_content_part_value(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        ContentPart::ImageUrl { image_url } => json!({
            "type": "image_url",
            "image_url": {
                "url": image_url
            }
        }),
    }
}

fn codex_input_items_for_message(message: &protocol_core::CanonicalMessage) -> Vec<Value> {
    if matches!(message.role, protocol_core::MessageRole::Tool)
        && let Some(tool_call_id) = &message.tool_call_id
    {
        return vec![json!({
            "type": "function_call_output",
            "call_id": tool_call_id,
            "output": message.content,
        })];
    }

    let mut items = message
        .tool_calls
        .iter()
        .map(|tool_call| {
            json!({
                "type": "function_call",
                "call_id": tool_call.id,
                "name": tool_call.name,
                "arguments": tool_call.arguments,
            })
        })
        .collect::<Vec<_>>();

    let parts = OpenAiCodexProvider::message_parts(message);
    if message.tool_calls.is_empty() || !parts.is_empty() {
        items.push(json!({
            "type": "message",
            "role": role_label(&message.role),
            "content": parts.iter().map(codex_content_part_value).collect::<Vec<_>>()
        }));
    }

    items
}

fn codex_content_part_value(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({
            "type": "input_text",
            "text": text,
        }),
        ContentPart::ImageUrl { image_url } => json!({
            "type": "input_image",
            "image_url": image_url,
        }),
    }
}

fn to_tool_calls_from_chat(message: &ChatCompletionMessage) -> Vec<ToolCall> {
    message
        .tool_calls
        .iter()
        .map(|tool_call| ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.function.name.clone(),
            arguments: tool_call.function.arguments.clone(),
        })
        .collect()
}

fn extract_response_tool_calls(output: &[ResponsesOutputItem]) -> Vec<ToolCall> {
    output
        .iter()
        .filter(|item| item.item_type.as_deref() == Some("function_call"))
        .filter_map(|item| {
            Some(ToolCall {
                id: item.call_id.clone().or_else(|| item.id.clone())?,
                name: item.name.clone()?,
                arguments: item.arguments.clone().unwrap_or_default(),
            })
        })
        .collect()
}

async fn ensure_success(response: Response) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    Err(http_status_error(status, &body))
}

fn http_status_error(status: StatusCode, body: &str) -> ProviderError {
    let kind = match status.as_u16() {
        400 | 404 => ProviderErrorKind::InvalidRequest,
        401 | 403 => ProviderErrorKind::InvalidCredentials,
        429 => ProviderErrorKind::RateLimited,
        500..=599 => ProviderErrorKind::UpstreamUnavailable,
        _ => ProviderErrorKind::Unsupported,
    };

    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
        })
        .unwrap_or_else(|| format!("upstream returned {status}"));

    ProviderError::new(kind, status.as_u16(), message)
}

fn transport_error(error: impl ToString) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::UpstreamUnavailable,
        502,
        format!("upstream transport failed: {}", error.to_string()),
    )
}

fn parse_chat_response(
    public_model: String,
    provider_kind: String,
    response: ChatCompletionResponse,
) -> InferenceResponse {
    let (output_text, tool_calls, finish_reason) =
        if let Some(choice) = response.choices.into_iter().next() {
            let tool_calls = to_tool_calls_from_chat(&choice.message);
            let finish_reason = choice
                .finish_reason
                .as_deref()
                .map(parse_finish_reason)
                .unwrap_or_else(|| {
                    if tool_calls.is_empty() {
                        FinishReason::Stop
                    } else {
                        FinishReason::ToolCalls
                    }
                });
            (
                choice.message.content.unwrap_or_default(),
                tool_calls,
                finish_reason,
            )
        } else {
            (String::new(), Vec::new(), FinishReason::Stop)
        };
    let usage = response
        .usage
        .map(usage_from_api)
        .unwrap_or_else(|| estimate_usage(&output_text));

    InferenceResponse {
        id: response
            .id
            .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple())),
        model: response.model.unwrap_or(public_model),
        output_text,
        finish_reason,
        tool_calls,
        usage,
        provider_kind,
        created_at: Utc::now(),
    }
}

fn parse_responses_response(
    public_model: String,
    provider_kind: String,
    response: ResponsesApiResponse,
) -> InferenceResponse {
    let tool_calls = extract_response_tool_calls(&response.output);
    let output_text = response
        .output_text
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| extract_response_text(&response.output));
    let usage = response
        .usage
        .map(usage_from_api)
        .unwrap_or_else(|| estimate_usage(&output_text));

    InferenceResponse {
        id: response
            .id
            .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple())),
        model: response.model.unwrap_or(public_model),
        output_text,
        finish_reason: if tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        },
        tool_calls,
        usage,
        provider_kind,
        created_at: Utc::now(),
    }
}

fn parse_responses_completion_payload(
    public_model: String,
    provider_kind: String,
    payload: &str,
) -> Result<InferenceResponse, ProviderError> {
    if let Ok(payload) = serde_json::from_str::<ResponsesCompletedEvent>(payload) {
        return Ok(parse_responses_response(
            public_model,
            provider_kind,
            payload.response,
        ));
    }

    if let Ok(payload) = serde_json::from_str::<ResponsesApiResponse>(payload) {
        return Ok(parse_responses_response(
            public_model,
            provider_kind,
            payload,
        ));
    }

    Err(ProviderError::new(
        ProviderErrorKind::UpstreamUnavailable,
        502,
        "invalid responses completion payload",
    ))
}

fn usage_from_api(usage: ApiUsage) -> TokenUsage {
    let input_tokens = usage
        .input_tokens
        .or(usage.prompt_tokens)
        .unwrap_or_default();
    let output_tokens = usage
        .output_tokens
        .or(usage.completion_tokens)
        .unwrap_or_default();
    let total_tokens = usage
        .total_tokens
        .unwrap_or(input_tokens.saturating_add(output_tokens));

    TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    }
}

fn estimate_usage(output_text: &str) -> TokenUsage {
    let output_tokens = output_text.split_whitespace().count() as u32;
    TokenUsage {
        input_tokens: 0,
        output_tokens,
        total_tokens: output_tokens,
    }
}

fn parse_finish_reason(value: &str) -> FinishReason {
    match value {
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        "error" => FinishReason::Error,
        _ => FinishReason::Stop,
    }
}

fn extract_response_text(output: &[ResponsesOutputItem]) -> String {
    output
        .iter()
        .flat_map(|item| item.content.iter())
        .filter_map(|content| content.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

fn parse_sse_frame(frame: &str) -> Option<SseMessage> {
    let mut event = None;
    let mut data = Vec::new();

    for line in frame.lines().filter(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_string());
        }
    }

    if data.is_empty() {
        return None;
    }

    Some(SseMessage {
        event,
        data: data.join("\n"),
    })
}

fn finalize_stream_response(
    public_model: String,
    final_model: Option<String>,
    provider_kind: String,
    output_text: String,
) -> InferenceResponse {
    InferenceResponse {
        id: format!("resp_{}", Uuid::new_v4().simple()),
        model: final_model.unwrap_or(public_model),
        usage: estimate_usage(&output_text),
        output_text,
        finish_reason: FinishReason::Stop,
        tool_calls: Vec::new(),
        provider_kind,
        created_at: Utc::now(),
    }
}

fn parse_stream_error(payload: &str) -> ProviderError {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(|message| {
                    ProviderError::new(ProviderErrorKind::UpstreamUnavailable, 502, message)
                })
                .or_else(|| {
                    value
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .map(|message| {
                            ProviderError::new(ProviderErrorKind::UpstreamUnavailable, 502, message)
                        })
                })
        })
        .unwrap_or_else(|| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                "upstream stream failed",
            )
        })
}

fn redact_email(email: &str) -> String {
    let mut chars = email.chars();
    match chars.next() {
        Some(first) => format!("{first}***@***"),
        None => "***".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::Request,
        http::StatusCode as HttpStatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use provider_core::{ProviderAccountEnvelope, ProviderRegistry};
    use serde_json::json;
    use std::{collections::BTreeMap, net::SocketAddr};
    use storage::PlatformStore;

    async fn spawn_mock_server() -> SocketAddr {
        async fn models_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            assert_eq!(auth, "Bearer test-token");

            axum::Json(json!({
                "object": "list",
                "data": [
                    { "id": "gpt-4.1-mini" },
                    { "id": "gpt-5.1" }
                ]
            }))
            .into_response()
        }

        async fn chat_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

            assert_eq!(auth, "Bearer test-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-4.1-mini")
            );

            if stream {
                let payload = concat!(
                    "data: {\"model\":\"gpt-4.1-mini\",\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\n",
                    "data: {\"model\":\"gpt-4.1-mini\",\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\n",
                    "data: [DONE]\n\n"
                );
                return ([(http::header::CONTENT_TYPE, "text/event-stream")], payload)
                    .into_response();
            }

            axum::Json(json!({
                "id": "chat_123",
                "model": "gpt-4.1-mini",
                "choices": [{
                    "message": { "content": "hello world" },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 5,
                    "completion_tokens": 2,
                    "total_tokens": 7
                }
            }))
            .into_response()
        }

        async fn responses_handler(request: Request) -> impl IntoResponse {
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

            if stream {
                let payload = concat!(
                    "event: response.output_text.delta\n",
                    "data: {\"delta\":\"hello \"}\n\n",
                    "event: response.output_text.delta\n",
                    "data: {\"delta\":\"world\"}\n\n",
                    "event: response.completed\n",
                    "data: {\"id\":\"resp_123\",\"model\":\"gpt-4.1-mini\",\"output_text\":\"hello world\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}\n\n"
                );
                return ([(http::header::CONTENT_TYPE, "text/event-stream")], payload)
                    .into_response();
            }

            axum::Json(json!({
                "id": "resp_123",
                "model": "gpt-4.1-mini",
                "output_text": "hello world",
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 2,
                    "total_tokens": 7
                }
            }))
            .into_response()
        }

        async fn error_handler() -> impl IntoResponse {
            (
                HttpStatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({
                    "error": {
                        "message": "rate limited"
                    }
                })),
            )
        }

        let app = Router::new()
            .route("/v1/models", get(models_handler))
            .route("/v1/chat/completions", post(chat_handler))
            .route("/v1/responses", post(responses_handler))
            .route("/v1/error", post(error_handler))
            .fallback(|| async { (HttpStatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_endpoint_server() -> SocketAddr {
        async fn method_not_allowed() -> impl IntoResponse {
            (
                HttpStatusCode::METHOD_NOT_ALLOWED,
                axum::Json(json!({ "detail": "Method Not Allowed" })),
            )
        }

        async fn codex_models_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            assert_eq!(auth, "Bearer test-token");

            axum::Json(json!({
                "object": "list",
                "data": [
                    { "id": "gpt-5-codex" },
                    { "id": "gpt-5-codex-mini" }
                ]
            }))
            .into_response()
        }

        async fn codex_responses_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer test-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );
            assert_eq!(
                body.get("instructions").and_then(Value::as_str),
                Some("You are Codex.")
            );
            assert_eq!(body.get("store").and_then(Value::as_bool), Some(false));
            assert_eq!(body.get("stream").and_then(Value::as_bool), Some(true));
            assert_eq!(
                body.pointer("/input/0/type").and_then(Value::as_str),
                Some("message")
            );
            assert_eq!(
                body.pointer("/input/0/role").and_then(Value::as_str),
                Some("user")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/type")
                    .and_then(Value::as_str),
                Some("input_text")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("hello")
            );

            let payload = concat!(
                "event: response.created\n",
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_codex_123\",\"model\":\"gpt-5.1-codex\",\"output\":[]}}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"rs_codex_123\",\"type\":\"reasoning\",\"summary\":[]},\"output_index\":0}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"rs_codex_123\",\"type\":\"reasoning\",\"summary\":[]},\"output_index\":0}\n\n",
                "event: response.output_item.added\n",
                "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"msg_codex_123\",\"type\":\"message\",\"status\":\"in_progress\",\"content\":[],\"role\":\"assistant\"},\"output_index\":1}\n\n",
                "event: response.content_part.added\n",
                "data: {\"type\":\"response.content_part.added\",\"content_index\":0,\"item_id\":\"msg_codex_123\",\"output_index\":1,\"part\":{\"type\":\"output_text\",\"annotations\":[],\"text\":\"\"}}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"hello \",\"item_id\":\"msg_codex_123\",\"output_index\":1}\n\n",
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"from codex\",\"item_id\":\"msg_codex_123\",\"output_index\":1}\n\n",
                "event: response.output_text.done\n",
                "data: {\"type\":\"response.output_text.done\",\"content_index\":0,\"item_id\":\"msg_codex_123\",\"output_index\":1,\"text\":\"hello from codex\"}\n\n",
                "event: response.content_part.done\n",
                "data: {\"type\":\"response.content_part.done\",\"content_index\":0,\"item_id\":\"msg_codex_123\",\"output_index\":1,\"part\":{\"type\":\"output_text\",\"annotations\":[],\"text\":\"hello from codex\"}}\n\n",
                "event: response.output_item.done\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"msg_codex_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"hello from codex\"}],\"role\":\"assistant\"},\"output_index\":1}\n\n",
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_codex_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"rs_codex_123\",\"type\":\"reasoning\",\"summary\":[]},{\"id\":\"msg_codex_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"hello from codex\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":3,\"total_tokens\":8}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        async fn codex_chat_handler() -> impl IntoResponse {
            (
                HttpStatusCode::FORBIDDEN,
                [
                    ("content-type", "text/html; charset=UTF-8"),
                    ("cf-mitigated", "challenge"),
                ],
                "<html><body>Enable JavaScript and cookies to continue</body></html>",
            )
                .into_response()
        }

        let app = Router::new()
            .route("/backend-api/codex/models", get(codex_models_handler))
            .route(
                "/backend-api/codex/responses",
                get(method_not_allowed).post(codex_responses_handler),
            )
            .route(
                "/backend-api/codex/chat/completions",
                post(codex_chat_handler),
            )
            .fallback(|| async { (HttpStatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_tool_call_server() -> SocketAddr {
        async fn codex_responses_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer test-token");
            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some("gpt-5-codex")
            );
            assert_eq!(
                body.pointer("/tools/0/type").and_then(Value::as_str),
                Some("function")
            );
            assert_eq!(
                body.pointer("/tools/0/name").and_then(Value::as_str),
                Some("get_weather")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/type")
                    .and_then(Value::as_str),
                Some("input_text")
            );
            assert_eq!(
                body.pointer("/input/0/content/0/text")
                    .and_then(Value::as_str),
                Some("What is the weather in Shanghai?")
            );
            assert_eq!(
                body.pointer("/input/0/content/1/type")
                    .and_then(Value::as_str),
                Some("input_image")
            );
            assert_eq!(
                body.pointer("/input/0/content/1/image_url")
                    .and_then(Value::as_str),
                Some("https://example.com/weather.png")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"fc_123\",\"type\":\"function_call\",\"call_id\":\"call_weather_123\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}],\"usage\":{\"input_tokens\":12,\"output_tokens\":4,\"total_tokens\":16}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (HttpStatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn spawn_codex_tool_result_server() -> SocketAddr {
        async fn codex_responses_handler(request: Request) -> impl IntoResponse {
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("body");
            let body: Value = serde_json::from_slice(&body).expect("json body");

            assert_eq!(auth, "Bearer test-token");
            assert_eq!(
                body.get("previous_response_id").and_then(Value::as_str),
                Some("resp_tool_123")
            );
            assert_eq!(
                body.pointer("/input/0/type").and_then(Value::as_str),
                Some("function_call")
            );
            assert_eq!(
                body.pointer("/input/0/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/0/name").and_then(Value::as_str),
                Some("get_weather")
            );
            assert_eq!(
                body.pointer("/input/0/arguments").and_then(Value::as_str),
                Some("{\"city\":\"Shanghai\"}")
            );
            assert_eq!(
                body.pointer("/input/1/type").and_then(Value::as_str),
                Some("function_call_output")
            );
            assert_eq!(
                body.pointer("/input/1/call_id").and_then(Value::as_str),
                Some("call_weather_123")
            );
            assert_eq!(
                body.pointer("/input/1/output").and_then(Value::as_str),
                Some("{\"temperature_c\":25}")
            );

            let payload = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tool_result_123\",\"model\":\"gpt-5.1-codex\",\"output\":[{\"id\":\"msg_tool_result_123\",\"type\":\"message\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"annotations\":[],\"text\":\"Shanghai is 25C.\"}],\"role\":\"assistant\"}],\"usage\":{\"input_tokens\":14,\"output_tokens\":4,\"total_tokens\":18}}}\n\n"
            );

            ([(http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
        }

        let app = Router::new()
            .route(
                "/backend-api/codex/responses",
                post(codex_responses_handler),
            )
            .fallback(|| async { (HttpStatusCode::NOT_FOUND, Body::empty()) });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    async fn setup_provider(
        api_base: &str,
    ) -> (
        Arc<OpenAiCodexProvider>,
        protocol_core::InferenceRequest,
        PlatformStore,
    ) {
        let store = PlatformStore::demo();
        let provider = OpenAiCodexProvider::shared(Arc::new(store.clone()));
        let validated = provider
            .validate_credentials(&ProviderAccountEnvelope {
                provider: "openai_codex".to_string(),
                credential_kind: "oauth_tokens".to_string(),
                payload_version: "v1".to_string(),
                credentials: json!({
                    "access_token": "test-token",
                    "account_id": "acct_test",
                    "api_base": api_base
                }),
                metadata: json!({ "email": "demo@example.com" }),
                labels: vec![],
                tags: BTreeMap::new(),
            })
            .await
            .expect("validated");
        let record = store
            .ingest_provider_account(
                ProviderAccountEnvelope {
                    provider: "openai_codex".to_string(),
                    credential_kind: "oauth_tokens".to_string(),
                    payload_version: "v1".to_string(),
                    credentials: json!({
                        "access_token": "test-token",
                        "account_id": "acct_test",
                        "api_base": api_base
                    }),
                    metadata: json!({ "email": "demo@example.com" }),
                    labels: vec![],
                    tags: BTreeMap::new(),
                },
                validated,
                AccountCapabilities {
                    models: provider.supported_models(),
                    supports_refresh: true,
                    supports_quota_probe: false,
                },
            )
            .await
            .expect("record");
        let request = InferenceRequest {
            protocol: protocol_core::FrontendProtocol::OpenAi,
            public_model: "gpt-4.1-mini".to_string(),
            upstream_model: Some("gpt-4.1-mini".to_string()),
            previous_response_id: None,
            stream: false,
            messages: vec![protocol_core::CanonicalMessage {
                role: protocol_core::MessageRole::User,
                content: "hello".to_string(),
                parts: vec![],
                tool_calls: vec![],
                tool_call_id: None,
            }],
            tools: vec![],
            metadata: BTreeMap::from([("provider_account_id".to_string(), record.id.to_string())]),
        };

        (provider, request, store)
    }

    #[tokio::test]
    async fn validates_expected_openai_shape() {
        let store = PlatformStore::demo();
        let provider = OpenAiCodexProvider::shared(Arc::new(store));
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

    #[tokio::test]
    async fn chat_calls_real_http_upstream() {
        let addr = spawn_mock_server().await;
        let (provider, request, _) = setup_provider(&format!("http://{addr}/v1")).await;

        let response = provider.chat(request).await.expect("chat");
        assert_eq!(response.output_text, "hello world");
        assert_eq!(response.usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn probe_capabilities_fetches_models_from_openai_upstream() {
        let addr = spawn_mock_server().await;
        let store = PlatformStore::demo();
        let provider = OpenAiCodexProvider::shared(Arc::new(store));
        let envelope = ProviderAccountEnvelope {
            provider: "openai_codex".to_string(),
            credential_kind: "oauth_tokens".to_string(),
            payload_version: "v1".to_string(),
            credentials: json!({
                "access_token": "test-token",
                "account_id": "acct_test",
                "api_base": format!("http://{addr}/v1")
            }),
            metadata: json!({ "email": "demo@example.com" }),
            labels: vec![],
            tags: BTreeMap::new(),
        };
        let validated = provider
            .validate_credentials(&envelope)
            .await
            .expect("validated");

        let capabilities = provider
            .probe_capabilities(&envelope, &validated)
            .await
            .expect("capabilities");

        let model_ids = capabilities
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(model_ids, vec!["gpt-4.1-mini", "gpt-5.1"]);
    }

    #[tokio::test]
    async fn probe_capabilities_fetches_models_from_codex_upstream() {
        let addr = spawn_codex_endpoint_server().await;
        let store = PlatformStore::demo();
        let provider = OpenAiCodexProvider::shared(Arc::new(store));
        let envelope = ProviderAccountEnvelope {
            provider: "openai_codex".to_string(),
            credential_kind: "oauth_tokens".to_string(),
            payload_version: "v1".to_string(),
            credentials: json!({
                "access_token": "test-token",
                "account_id": "acct_test",
                "api_base": format!("http://{addr}/backend-api/codex")
            }),
            metadata: json!({ "email": "demo@example.com" }),
            labels: vec![],
            tags: BTreeMap::new(),
        };
        let validated = provider
            .validate_credentials(&envelope)
            .await
            .expect("validated");

        let capabilities = provider
            .probe_capabilities(&envelope, &validated)
            .await
            .expect("capabilities");

        let model_ids = capabilities
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(model_ids, vec!["gpt-5-codex", "gpt-5-codex-mini"]);
    }

    #[tokio::test]
    async fn responses_on_codex_endpoint_supports_images_and_tools() {
        let addr = spawn_codex_tool_call_server().await;
        let (provider, mut request, _) =
            setup_provider(&format!("http://{addr}/backend-api/codex")).await;
        request.public_model = "gpt-5-codex".to_string();
        request.upstream_model = Some("gpt-5-codex".to_string());
        request.messages = vec![protocol_core::CanonicalMessage {
            role: protocol_core::MessageRole::User,
            content: "What is the weather in Shanghai?".to_string(),
            parts: vec![
                protocol_core::ContentPart::Text {
                    text: "What is the weather in Shanghai?".to_string(),
                },
                protocol_core::ContentPart::ImageUrl {
                    image_url: "https://example.com/weather.png".to_string(),
                },
            ],
            tool_calls: vec![],
            tool_call_id: None,
        }];
        request.tools = vec![protocol_core::ToolDefinition {
            name: "get_weather".to_string(),
            description: Some("Fetch current weather".to_string()),
            parameters: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        }];

        let response = provider.responses(request).await.expect("responses");

        assert_eq!(response.model, "gpt-5.1-codex");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert!(response.output_text.is_empty());
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id, "call_weather_123");
        assert_eq!(response.tool_calls[0].name, "get_weather");
        assert_eq!(response.tool_calls[0].arguments, "{\"city\":\"Shanghai\"}");
    }

    #[tokio::test]
    async fn responses_on_codex_endpoint_supports_previous_response_ids_and_tool_results() {
        let addr = spawn_codex_tool_result_server().await;
        let (provider, mut request, _) =
            setup_provider(&format!("http://{addr}/backend-api/codex")).await;
        request.public_model = "gpt-5-codex".to_string();
        request.upstream_model = Some("gpt-5-codex".to_string());
        request.previous_response_id = Some("resp_tool_123".to_string());
        request.messages = vec![
            protocol_core::CanonicalMessage {
                role: protocol_core::MessageRole::Assistant,
                content: String::new(),
                parts: vec![],
                tool_calls: vec![protocol_core::ToolCall {
                    id: "call_weather_123".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"Shanghai\"}".to_string(),
                }],
                tool_call_id: None,
            },
            protocol_core::CanonicalMessage {
                role: protocol_core::MessageRole::Tool,
                content: "{\"temperature_c\":25}".to_string(),
                parts: vec![],
                tool_calls: vec![],
                tool_call_id: Some("call_weather_123".to_string()),
            },
        ];

        let response = provider.responses(request).await.expect("responses");

        assert_eq!(response.model, "gpt-5.1-codex");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(response.output_text, "Shanghai is 25C.");
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn stream_chat_parses_sse_deltas() {
        let addr = spawn_mock_server().await;
        let (provider, mut request, _) = setup_provider(&format!("http://{addr}/v1")).await;
        request.stream = true;

        let mut stream = provider.stream_chat(request).await.expect("stream");
        let mut deltas = Vec::new();
        let mut final_text = None;
        while let Some(event) = stream.next().await {
            let event = event.expect("event");
            match event.kind {
                StreamEventKind::ContentDelta => deltas.push(event.delta.expect("delta")),
                StreamEventKind::Done => {
                    final_text = event.response.map(|response| response.output_text);
                }
                _ => {}
            }
        }

        assert_eq!(deltas.join(""), "hello world");
        assert_eq!(final_text.as_deref(), Some("hello world"));
    }

    #[tokio::test]
    async fn responses_calls_real_http_upstream() {
        let addr = spawn_mock_server().await;
        let (provider, request, _) = setup_provider(&format!("http://{addr}/v1")).await;

        let response = provider.responses(request).await.expect("responses");
        assert_eq!(response.output_text, "hello world");
        assert_eq!(response.usage.total_tokens, 7);
    }

    #[tokio::test]
    async fn stream_responses_parses_sse_events() {
        let addr = spawn_mock_server().await;
        let (provider, mut request, _) = setup_provider(&format!("http://{addr}/v1")).await;
        request.stream = true;

        let mut stream = provider.stream_responses(request).await.expect("stream");
        let mut deltas = Vec::new();
        let mut final_text = None;
        while let Some(event) = stream.next().await {
            let event = event.expect("event");
            match event.kind {
                StreamEventKind::ContentDelta => deltas.push(event.delta.expect("delta")),
                StreamEventKind::Done => {
                    final_text = event.response.map(|response| response.output_text);
                }
                _ => {}
            }
        }

        assert_eq!(deltas.join(""), "hello world");
        assert_eq!(final_text.as_deref(), Some("hello world"));
    }

    #[tokio::test]
    async fn responses_on_codex_endpoint_collects_streamed_response() {
        let addr = spawn_codex_endpoint_server().await;
        let (provider, mut request, _) =
            setup_provider(&format!("http://{addr}/backend-api/codex")).await;
        request.public_model = "gpt-5-codex".to_string();
        request.upstream_model = Some("gpt-5-codex".to_string());

        let response = provider.responses(request).await.expect("responses");

        assert_eq!(response.model, "gpt-5.1-codex");
        assert_eq!(response.output_text, "hello from codex");
        assert_eq!(response.usage.total_tokens, 8);
    }

    #[tokio::test]
    async fn stream_responses_parses_chatgpt_codex_sse_shape() {
        let addr = spawn_codex_endpoint_server().await;
        let (provider, mut request, _) =
            setup_provider(&format!("http://{addr}/backend-api/codex")).await;
        request.public_model = "gpt-5-codex".to_string();
        request.upstream_model = Some("gpt-5-codex".to_string());
        request.stream = true;

        let mut stream = provider.stream_responses(request).await.expect("stream");
        let mut deltas = Vec::new();
        let mut final_text = None;
        let mut final_model = None;

        while let Some(event) = stream.next().await {
            let event = event.expect("event");
            match event.kind {
                StreamEventKind::ContentDelta => deltas.push(event.delta.expect("delta")),
                StreamEventKind::Done => {
                    let response = event.response.expect("response");
                    final_model = Some(response.model);
                    final_text = Some(response.output_text);
                }
                _ => {}
            }
        }

        assert_eq!(deltas.join(""), "hello from codex");
        assert_eq!(final_model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(final_text.as_deref(), Some("hello from codex"));
    }

    #[tokio::test]
    async fn chat_on_codex_endpoint_uses_responses_api() {
        let addr = spawn_codex_endpoint_server().await;
        let (provider, mut request, _) =
            setup_provider(&format!("http://{addr}/backend-api/codex")).await;
        request.public_model = "gpt-5-codex".to_string();
        request.upstream_model = Some("gpt-5-codex".to_string());

        let response = provider.chat(request).await.expect("chat");

        assert_eq!(response.model, "gpt-5.1-codex");
        assert_eq!(response.output_text, "hello from codex");
        assert_eq!(response.usage.total_tokens, 8);
    }

    #[tokio::test]
    #[ignore = "hits live chatgpt.com upstream"]
    async fn live_probe_chatgpt_codex_endpoint_shape() {
        let client = Client::builder()
            .user_agent("reqwest/0.12.28")
            .build()
            .expect("client");

        let get_responses = client
            .get("https://chatgpt.com/backend-api/codex/responses")
            .send()
            .await
            .expect("GET /responses should complete");
        assert_eq!(get_responses.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            get_responses
                .headers()
                .get(http::header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("POST")
        );
        let get_responses_body: Value = get_responses.json().await.expect("json body");
        assert_eq!(
            get_responses_body.get("detail").and_then(Value::as_str),
            Some("Method Not Allowed")
        );

        let post_responses = client
            .post("https://chatgpt.com/backend-api/codex/responses")
            .json(&json!({
                "model": "gpt-5-codex",
                "input": "hello"
            }))
            .send()
            .await
            .expect("POST /responses should complete");
        assert_eq!(post_responses.status(), StatusCode::UNAUTHORIZED);
        let post_responses_body: Value = post_responses.json().await.expect("json body");
        assert_eq!(
            post_responses_body.get("detail").and_then(Value::as_str),
            Some("Unauthorized")
        );

        let post_chat = client
            .post("https://chatgpt.com/backend-api/codex/chat/completions")
            .json(&json!({
                "model": "gpt-5-codex",
                "messages": [{ "role": "user", "content": "hello" }]
            }))
            .send()
            .await
            .expect("POST /chat/completions should complete");
        assert_eq!(post_chat.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            post_chat
                .headers()
                .get("cf-mitigated")
                .and_then(|value| value.to_str().ok()),
            Some("challenge")
        );
        let post_chat_body = post_chat.text().await.expect("html body");
        assert!(post_chat_body.contains("Enable JavaScript and cookies to continue"));
    }

    #[tokio::test]
    async fn registry_keeps_provider_registered() {
        let store = PlatformStore::demo();
        let provider = OpenAiCodexProvider::shared(Arc::new(store));
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        assert!(registry.get("openai_codex").is_some());
    }
}
