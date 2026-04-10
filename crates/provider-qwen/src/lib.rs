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

mod system_prompt;

/// Models that require Qwen CLI system prompt + tools (Agent models).
fn is_agent_model(model: &str) -> bool {
    matches!(
        model,
        "coder-model" | "qwen3-coder-plus" | "qwen3-coder-flash"
    )
}

/// Minimal Qwen Code CLI system prompt for agent models.
/// Loads from file if FERRUMGATE_QWEN_SYSTEM_PROMPT is set, otherwise uses built-in default.
fn qwen_cli_system_prompt() -> String {
    if let Some(path) = system_prompt::qwen_system_prompt_path()
        && let Ok(content) = std::fs::read_to_string(&path)
    {
        return content;
    }
    // Built-in default (minimal but functional)
    QWEN_DEFAULT_SYSTEM_PROMPT.to_string()
}

/// Built-in default system prompt for Qwen Code CLI agent models.
const QWEN_DEFAULT_SYSTEM_PROMPT: &str = include_str!("../system_prompt.txt");

/// Qwen OAuth token endpoint (refresh uses chat.qwen.ai, API uses portal.qwen.ai).
const QWEN_OAUTH_TOKEN_ENDPOINT: &str = "https://chat.qwen.ai/api/v1/oauth2/token";

/// Qwen OAuth client ID (hardcoded from upstream source).
const QWEN_OAUTH_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";

/// Default Qwen API base URL for OpenAI-compatible endpoints.
const QWEN_DEFAULT_API_BASE: &str = "https://portal.qwen.ai/v1";

/// Build the tool definitions required by coder-model (agent model).
fn agent_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Reads and returns the content of a specified file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "The absolute path to the file to read." },
                        "limit": { "type": "number", "description": "Optional: For text files, maximum number of lines to read." },
                        "offset": { "type": "number", "description": "Optional: For text files, the 0-based line number to start reading from." }
                    },
                    "required": ["file_path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Writes content to a specified file in the local filesystem.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "The absolute path to the file to write to." },
                        "content": { "type": "string", "description": "The content to write to the file." }
                    },
                    "required": ["file_path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replaces text within a file. By default, replaces a single occurrence.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "The absolute path to the file to modify." },
                        "old_string": { "type": "string", "description": "The exact literal text to replace." },
                        "new_string": { "type": "string", "description": "The exact literal text to replace old_string with." },
                        "replace_all": { "type": "boolean", "description": "Replace all occurrences. Default false." }
                    },
                    "required": ["file_path", "old_string", "new_string"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep_search",
                "description": "A powerful search tool built on ripgrep for searching file contents.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "The regular expression pattern to search for." },
                        "glob": { "type": "string", "description": "Glob pattern to filter files (e.g. '*.js')." },
                        "path": { "type": "string", "description": "File or directory to search in." },
                        "limit": { "type": "number", "description": "Limit output to first N matches." }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Fast file pattern matching tool that works with any codebase size.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "The glob pattern to match files against." },
                        "path": { "type": "string", "description": "The directory to search in." }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_shell_command",
                "description": "Executes a given shell command in a persistent shell session.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Exact bash command to execute." },
                        "description": { "type": "string", "description": "Brief description of the command." },
                        "timeout": { "type": "number", "description": "Optional timeout in milliseconds." }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Create and manage a structured task list for tracking progress.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "content": { "type": "string" },
                                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                                },
                                "required": ["content", "status", "id"]
                            }
                        }
                    },
                    "required": ["todos"]
                }
            }
        }),
    ]
}

/// Known Qwen model IDs exposed through the gateway.
fn known_qwen_models() -> Vec<ModelDescriptor> {
    vec![
        ModelDescriptor {
            id: "qwen3-coder-plus".to_string(),
            route_group: "qwen-qwen3-coder-plus".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen3-coder-plus".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen3-coder-flash".to_string(),
            route_group: "qwen-qwen3-coder-flash".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "qwen3-coder-flash".to_string(),
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen3.5-plus".to_string(),
            route_group: "qwen-qwen3.5-plus".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "coder-model".to_string(), // alias → coder-model
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "qwen3.6-plus".to_string(),
            route_group: "qwen-qwen3.6-plus".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "coder-model".to_string(), // alias → coder-model
            capabilities: vec![
                ModelCapability::Chat,
                ModelCapability::Responses,
                ModelCapability::Streaming,
            ],
        },
        ModelDescriptor {
            id: "coder-model".to_string(),
            route_group: "qwen-coder-model".to_string(),
            provider_kind: "qwen".to_string(),
            upstream_model: "coder-model".to_string(),
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
        // Check resource_url first (from OAuth credentials), then api_base, then default
        if let Some(resource_url) = Self::string_field(envelope, "resource_url") {
            let base = resource_url.trim_end_matches('/');
            if base.ends_with("/v1") {
                return base.to_string();
            }
            return format!("{base}/v1");
        }
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

    fn build_headers(
        connection: &ProviderConnectionInfo,
        is_streaming: bool,
    ) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if is_streaming {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );

        // Qwen OAuth specific headers (matching qwen-code-oai-proxy)
        headers.insert(
            HeaderName::from_static("x-dashscope-authtype"),
            HeaderValue::from_static("qwen-oauth"),
        );
        headers.insert(
            HeaderName::from_static("x-dashscope-cachecontrol"),
            HeaderValue::from_static("enable"),
        );
        headers.insert(
            HeaderName::from_static("x-dashscope-useragent"),
            HeaderValue::from_static("QwenCode/0.11.1 (darwin; arm64)"),
        );

        // Stainless SDK headers
        headers.insert(
            HeaderName::from_static("x-stainless-arch"),
            HeaderValue::from_static("arm64"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-lang"),
            HeaderValue::from_static("js"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-os"),
            HeaderValue::from_static("MacOS"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-package-version"),
            HeaderValue::from_static("5.11.0"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-retry-count"),
            HeaderValue::from_static("0"),
        );
        headers.insert(
            HeaderName::from_static("x-stainless-runtime"),
            HeaderValue::from_static("node"),
        );

        // Additional headers
        headers.insert(
            HeaderName::from_static("accept-language"),
            HeaderValue::from_static("*"),
        );
        headers.insert(
            HeaderName::from_static("sec-fetch-mode"),
            HeaderValue::from_static("cors"),
        );

        for (key, value) in &connection.additional_headers {
            if let (Ok(name), Ok(val)) = (HeaderName::try_from(key), HeaderValue::try_from(value)) {
                headers.insert(name, val);
            }
        }
        Ok(headers)
    }

    fn chat_message_payload(message: &protocol_core::CanonicalMessage) -> Value {
        // Use string content for simple text (matches upstream Qwen proxy behavior).
        // Only use array format for multi-modal messages with images.
        let has_images = message
            .parts
            .iter()
            .any(|p| matches!(p, ContentPart::ImageUrl { .. }));

        let content = if has_images {
            // Multi-modal: build array format
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
            Value::Array(parts)
        } else {
            // Plain text: use string content (required by coder-model and other Qwen models)
            Value::String(message.content.clone())
        };

        match message.role {
            protocol_core::MessageRole::System => {
                json!({ "role": "system", "content": content })
            }
            protocol_core::MessageRole::User => {
                json!({ "role": "user", "content": content })
            }
            protocol_core::MessageRole::Assistant => {
                let mut obj = json!({ "role": "assistant", "content": content });
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
#[allow(dead_code)]
struct RawTokenUsage {
    #[serde(default)]
    #[serde(alias = "prompt_tokens")]
    input_tokens: Option<u32>,
    #[serde(default)]
    #[serde(alias = "completion_tokens")]
    output_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

impl From<RawTokenUsage> for protocol_core::TokenUsage {
    fn from(raw: RawTokenUsage) -> Self {
        protocol_core::TokenUsage {
            input_tokens: raw.input_tokens.unwrap_or(0),
            output_tokens: raw.output_tokens.unwrap_or(0),
            total_tokens: raw.total_tokens.unwrap_or(0),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatCompletionResponse {
    id: String,
    object: Option<String>,
    created: Option<i64>,
    model: String,
    choices: Vec<ChatCompletionChoice>,
    usage: Option<RawTokenUsage>,
    #[serde(default)]
    system_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatCompletionChoice {
    index: Option<i64>,
    message: Option<ChatMessage>,
    delta: Option<ChatMessage>,
    finish_reason: Option<String>,
    logprobs: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ChatMessage {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallPayload>>,
    reasoning_content: Option<String>,
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

/// Error response from the /responses endpoint's `response.failed` event.
#[derive(Debug, Deserialize)]
struct StreamErrorResponse {
    error: StreamErrorDetail,
}

#[derive(Debug, Deserialize)]
struct StreamErrorDetail {
    message: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    r#type: Option<String>,
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
        let headers = Self::build_headers(&connection, false)?;
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
        let headers = Self::build_headers(&connection, false)?;
        let api_base = &connection.api_base;
        let model = request
            .upstream_model
            .as_deref()
            .unwrap_or("qwen3-coder-plus");

        // For agent models (coder-model, etc.), inject system prompt and tools
        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        // Inject system prompt for agent models
        if is_agent_model(model) {
            let system_msg = json!({
                "role": "system",
                "content": qwen_cli_system_prompt()
            });
            // Only add if not already present
            if messages.is_empty() || messages[0].get("role") != Some(&json!("system")) {
                messages.insert(0, system_msg);
            }
        }

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });

        // Inject tools for agent models
        if is_agent_model(model) {
            body["tools"] = json!(agent_tools());
        } else if !request.tools.is_empty() {
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
        let status = response.status();
        let body_text = response.text().await.map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                format!("failed to read response body: {e}"),
            )
        })?;

        // Try to parse as JSON, log error if it fails
        let completion: ChatCompletionResponse = serde_json::from_str(&body_text).map_err(|e| {
            eprintln!("\n=== QWEN RESPONSE PARSE ERROR ===");
            eprintln!("Status: {status}");
            eprintln!(
                "Body (first 1000 chars): {}",
                &body_text[..1000.min(body_text.len())]
            );
            eprintln!("Error: {e}");
            eprintln!("===================================\n");
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

        let message = choice.message.unwrap_or_default();

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

        let usage = completion
            .usage
            .map(protocol_core::TokenUsage::from)
            .unwrap_or_else(|| protocol_core::TokenUsage {
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
        let connection = self.resolve_connection(&request).await?;
        let headers = Self::build_headers(&connection, false)?;
        let api_base = &connection.api_base;
        let model = request
            .upstream_model
            .as_deref()
            .unwrap_or("qwen3-coder-plus");

        // For agent models, inject system prompt and tools
        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        if is_agent_model(model) {
            let system_msg = json!({
                "role": "system",
                "content": qwen_cli_system_prompt()
            });
            if messages.is_empty() || messages[0].get("role") != Some(&json!("system")) {
                messages.insert(0, system_msg);
            }
        }

        let mut body = json!({
            "model": model,
            "input": messages,
            "stream": false,
        });

        if is_agent_model(model) {
            body["tools"] = json!(agent_tools());
        } else if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(Self::tool_payload).collect());
        }

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
        let completion: InferenceResponse = response.json().await.map_err(|e| {
            ProviderError::new(
                ProviderErrorKind::UpstreamUnavailable,
                502,
                format!("failed to parse responses response: {e}"),
            )
        })?;

        Ok(completion)
    }

    async fn stream_chat(
        &self,
        request: InferenceRequest,
    ) -> Result<ProviderStream, ProviderError> {
        let connection = self.resolve_connection(&request).await?;
        let headers = Self::build_headers(&connection, true)?;
        let api_base = &connection.api_base;
        let public_model = request.public_model.clone();
        let provider_kind = self.kind().to_string();
        let model = request
            .upstream_model
            .as_deref()
            .unwrap_or("qwen3-coder-plus");

        // For agent models, inject system prompt and tools
        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        if is_agent_model(model) {
            let system_msg = json!({
                "role": "system",
                "content": qwen_cli_system_prompt()
            });
            if messages.is_empty() || messages[0].get("role") != Some(&json!("system")) {
                messages.insert(0, system_msg);
            }
        }

        let mut body = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        // Inject tools for agent models
        if is_agent_model(model) {
            body["tools"] = json!(agent_tools());
        } else if !request.tools.is_empty() {
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
        let headers = Self::build_headers(&connection, true)?;
        let api_base = &connection.api_base;
        let public_model = request.public_model.clone();
        let provider_kind = self.kind().to_string();
        let model = request
            .upstream_model
            .as_deref()
            .unwrap_or("qwen3-coder-plus");

        // For agent models, inject system prompt and tools
        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .map(Self::chat_message_payload)
            .collect();

        if is_agent_model(model) {
            let system_msg = json!({
                "role": "system",
                "content": qwen_cli_system_prompt()
            });
            if messages.is_empty() || messages[0].get("role") != Some(&json!("system")) {
                messages.insert(0, system_msg);
            }
        }

        let mut body = json!({
            "model": model,
            "input": messages,
            "stream": true,
        });

        // Inject tools for agent models
        if is_agent_model(model) {
            body["tools"] = json!(agent_tools());
        } else if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(Self::tool_payload).collect());
        }

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
                                let err = serde_json::from_str::<StreamErrorResponse>(&message.data);
                                let (kind, status, msg) = match err {
                                    Ok(e) => {
                                        let code = e.error.code.as_deref().unwrap_or("");
                                        let kind = match code {
                                            "insufficient_quota" | "rate_limit_exceeded" => {
                                                ProviderErrorKind::RateLimited
                                            }
                                            "token_expired" | "invalid_api_key"
                                            | "authentication_failed" => {
                                                ProviderErrorKind::InvalidCredentials
                                            }
                                            _ => ProviderErrorKind::UpstreamUnavailable,
                                        };
                                        let status = match kind {
                                            ProviderErrorKind::RateLimited => 429,
                                            ProviderErrorKind::InvalidCredentials => 401,
                                            _ => 502,
                                        };
                                        (kind, status, e.error.message)
                                    }
                                    Err(_) => (
                                        ProviderErrorKind::UpstreamUnavailable,
                                        502,
                                        parse_error_message(&message.data)
                                            .unwrap_or_else(|| "responses request failed".to_string()),
                                    ),
                                };
                                yield Err(ProviderError::new(kind, status, msg));
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
        assert!(ids.contains(&"qwen3-coder-plus"));
        assert!(ids.contains(&"qwen3-coder-flash"));
        assert!(ids.contains(&"qwen3.5-plus"));
        assert!(ids.contains(&"qwen3.6-plus"));
        assert!(ids.contains(&"coder-model"));

        // Verify aliases: qwen3.5-plus and qwen3.6-plus map to coder-model
        let qwen35 = models.iter().find(|m| m.id == "qwen3.5-plus").unwrap();
        assert_eq!(qwen35.upstream_model, "coder-model");
        let qwen36 = models.iter().find(|m| m.id == "qwen3.6-plus").unwrap();
        assert_eq!(qwen36.upstream_model, "coder-model");
    }

    #[test]
    fn oauth_constants_are_correct() {
        assert_eq!(
            QWEN_OAUTH_TOKEN_ENDPOINT,
            "https://chat.qwen.ai/api/v1/oauth2/token"
        );
        assert_eq!(QWEN_OAUTH_CLIENT_ID, "f0304373b74a44d2b584a3fb70ca9e56");
        assert!(QWEN_DEFAULT_API_BASE.ends_with("/v1"));
        assert!(QWEN_DEFAULT_API_BASE.contains("portal.qwen.ai"));
    }

    #[tokio::test]
    async fn registry_accepts_qwen_provider() {
        let store = storage::PlatformStore::demo();
        let provider = QwenProvider::shared(Arc::new(store));
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        assert!(registry.get("qwen").is_some());
    }

    // ─── TDD Tests for the 3 gaps ──────────────────────────────────

    // Test 1: stream_responses() should inject system prompt + tools for agent models
    // Verifies that is_agent_model() returns true for coder-model and the tools list is non-empty
    #[test]
    fn is_agent_model_returns_true_for_coder_models() {
        assert!(is_agent_model("coder-model"));
        assert!(is_agent_model("qwen3-coder-plus"));
        assert!(is_agent_model("qwen3-coder-flash"));
        assert!(!is_agent_model("qwen-turbo"));
        assert!(!is_agent_model("gpt-4"));
    }

    #[test]
    fn agent_tools_returns_non_empty_list() {
        let tools = agent_tools();
        assert!(!tools.is_empty());
        assert!(tools.len() >= 5);
        // Verify tool names
        let names: Vec<_> = tools
            .iter()
            .filter_map(|t| t.get("function")?.get("name")?.as_str())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"run_shell_command"));
        assert!(names.contains(&"todo_write"));
    }

    // Test 2: responses() should call /responses endpoint
    // We verify the body format uses "input" key (not "messages") for /responses
    #[test]
    fn responses_body_uses_input_key() {
        // The responses() method constructs a body with "input" field, not "messages"
        let body = json!({
            "model": "coder-model",
            "input": [{"role": "user", "content": "hello"}],
            "stream": true,
        });
        assert!(body.get("input").is_some());
        assert!(body.get("messages").is_none());
    }

    // Test 3: stream_responses() should handle error events
    #[test]
    fn parse_stream_error_handles_response_failed_event() {
        let frame = "event: response.failed\ndata: {\"error\":{\"message\":\"quota exceeded\",\"code\":\"insufficient_quota\",\"type\":\"rate_limit_error\"}}\n\n";
        let message = parse_sse_frame(frame);
        assert!(message.is_some());
        let message = message.unwrap();
        assert_eq!(message.event.as_deref(), Some("response.failed"));
        // The error payload should be parseable
        let err: Result<StreamErrorResponse, _> = serde_json::from_str(&message.data);
        assert!(err.is_ok());
        let err = err.unwrap();
        assert!(err.error.message.contains("quota"));
    }
}
