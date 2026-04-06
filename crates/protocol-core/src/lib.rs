use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrontendProtocol {
    OpenAi,
    Anthropic,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReasoningConfig {
    pub effort: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalMessage {
    pub role: MessageRole,
    pub content: String,
    #[serde(default)]
    pub parts: Vec<ContentPart>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub protocol: FrontendProtocol,
    pub public_model: String,
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub reasoning: Option<ReasoningConfig>,
    pub stream: bool,
    pub messages: Vec<CanonicalMessage>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Chat,
    Responses,
    Streaming,
    Tools,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDescriptor {
    pub id: String,
    pub route_group: String,
    pub provider_kind: String,
    pub upstream_model: String,
    pub capabilities: Vec<ModelCapability>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceResponse {
    pub id: String,
    pub model: String,
    pub output_text: String,
    pub finish_reason: FinishReason,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub provider_kind: String,
    pub created_at: DateTime<Utc>,
}

impl InferenceResponse {
    #[must_use]
    pub fn text(
        model: impl Into<String>,
        provider_kind: impl Into<String>,
        output_text: impl Into<String>,
    ) -> Self {
        let output_text = output_text.into();
        let output_tokens = output_text.split_whitespace().count() as u32;

        Self {
            id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            model: model.into(),
            output_text,
            finish_reason: FinishReason::Stop,
            tool_calls: Vec::new(),
            usage: TokenUsage {
                input_tokens: 16,
                output_tokens,
                total_tokens: 16 + output_tokens,
            },
            provider_kind: provider_kind.into(),
            created_at: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamEventKind {
    MessageStart,
    ContentDelta,
    MessageStop,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceStreamEvent {
    pub event: Option<String>,
    pub kind: StreamEventKind,
    pub delta: Option<String>,
    pub response: Option<InferenceResponse>,
}

impl InferenceStreamEvent {
    #[must_use]
    pub fn delta(delta: impl Into<String>) -> Self {
        Self {
            event: None,
            kind: StreamEventKind::ContentDelta,
            delta: Some(delta.into()),
            response: None,
        }
    }

    #[must_use]
    pub fn done(response: InferenceResponse) -> Self {
        Self {
            event: Some("message_stop".to_string()),
            kind: StreamEventKind::Done,
            delta: None,
            response: Some(response),
        }
    }
}
