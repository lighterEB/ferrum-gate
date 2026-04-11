use protocol_core::{CanonicalMessage, ContentPart, InferenceRequest, MessageRole, ToolDefinition};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ─── Request types (mirrored from openai_http.rs for isolation) ─────────

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiMessage {
    role: String,
    #[serde(default)]
    content: OpenAiMessageContent,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OpenAiToolCall>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

impl Default for OpenAiMessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiContentPart {
    #[serde(rename = "type")]
    part_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    image_url: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDefinition {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Value,
}

// ─── Conversion functions ───────────────────────────────────────────────

fn extract_image_url(value: &Value) -> Option<String> {
    value.as_str().map(ToString::to_string).or_else(|| {
        value
            .get("url")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn parse_message_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn content_parts(content: &OpenAiMessageContent) -> Vec<ContentPart> {
    match content {
        OpenAiMessageContent::Text(text) if text.is_empty() => Vec::new(),
        OpenAiMessageContent::Text(text) => vec![ContentPart::Text { text: text.clone() }],
        OpenAiMessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part.part_type.as_str() {
                "text" | "input_text" | "output_text" => part
                    .text
                    .as_ref()
                    .map(|text| ContentPart::Text { text: text.clone() }),
                "image_url" | "input_image" => part
                    .image_url
                    .as_ref()
                    .and_then(extract_image_url)
                    .map(|image_url| ContentPart::ImageUrl { image_url }),
                _ => None,
            })
            .collect(),
    }
}

fn text_from_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn openai_message_to_canonical(message: &OpenAiMessage) -> CanonicalMessage {
    let parts = content_parts(&message.content);
    CanonicalMessage {
        role: parse_message_role(&message.role),
        content: text_from_parts(&parts),
        parts,
        tool_calls: message
            .tool_calls
            .iter()
            .map(|tc| protocol_core::ToolCall {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

pub(crate) fn openai_tools_to_canonical(tools: &[OpenAiToolDefinition]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter(|t| t.tool_type == "function")
        .map(|t| ToolDefinition {
            name: t.function.name.clone(),
            description: t.function.description.clone(),
            parameters: t.function.parameters.clone(),
        })
        .collect()
}

pub(crate) fn parse_chat_request(raw: &Value) -> Option<InferenceRequest> {
    let model = raw.get("model")?.as_str()?.to_string();
    let messages: Vec<OpenAiMessage> = serde_json::from_value(raw["messages"].clone()).ok()?;
    let tools_raw: Vec<OpenAiToolDefinition> = raw
        .get("tools")
        .and_then(|t| serde_json::from_value(t.clone()).ok())
        .unwrap_or_default();

    Some(InferenceRequest {
        protocol: protocol_core::FrontendProtocol::OpenAi,
        public_model: model,
        upstream_model: None,
        previous_response_id: None,
        reasoning: raw
            .get("reasoning")
            .and_then(|r| serde_json::from_value(r.clone()).ok()),
        stream: raw.get("stream").and_then(Value::as_bool).unwrap_or(false),
        messages: messages.iter().map(openai_message_to_canonical).collect(),
        tools: openai_tools_to_canonical(&tools_raw),
        metadata: BTreeMap::new(),
    })
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Test 1: Basic user message
    #[test]
    fn parse_basic_user_message() {
        let msg: OpenAiMessage = serde_json::from_value(json!({
            "role": "user",
            "content": "hi"
        }))
        .unwrap();

        let canonical = openai_message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::User);
        assert_eq!(canonical.content, "hi");
        assert!(canonical.parts.is_empty() || canonical.parts.len() == 1);
        assert!(canonical.tool_calls.is_empty());
    }

    // Test 2: System message
    #[test]
    fn parse_system_message() {
        let msg: OpenAiMessage = serde_json::from_value(json!({
            "role": "system",
            "content": "You are helpful"
        }))
        .unwrap();

        let canonical = openai_message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::System);
        assert_eq!(canonical.content, "You are helpful");
    }

    // Test 3: Assistant message
    #[test]
    fn parse_assistant_message() {
        let msg: OpenAiMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": "hello"
        }))
        .unwrap();

        let canonical = openai_message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::Assistant);
        assert_eq!(canonical.content, "hello");
    }

    // Test 4: Tool message
    #[test]
    fn parse_tool_message() {
        let msg: OpenAiMessage = serde_json::from_value(json!({
            "role": "tool",
            "tool_call_id": "tc_1",
            "content": "result"
        }))
        .unwrap();

        let canonical = openai_message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::Tool);
        assert_eq!(canonical.tool_call_id, Some("tc_1".to_string()));
        assert_eq!(canonical.content, "result");
    }

    // Test 5: Messages array
    #[test]
    fn parse_messages_array() {
        let raw = json!([
            {"role": "system", "content": "You are helpful"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "tool", "tool_call_id": "tc_1", "content": "result"}
        ]);

        let messages: Vec<OpenAiMessage> = serde_json::from_value(raw).unwrap();
        let canonicals: Vec<CanonicalMessage> =
            messages.iter().map(openai_message_to_canonical).collect();

        assert_eq!(canonicals.len(), 4);
        assert_eq!(canonicals[0].role, MessageRole::System);
        assert_eq!(canonicals[1].role, MessageRole::User);
        assert_eq!(canonicals[2].role, MessageRole::Assistant);
        assert_eq!(canonicals[3].role, MessageRole::Tool);
    }

    // Test 6: Tools array
    #[test]
    fn parse_tools_array() {
        let tools_raw: Vec<OpenAiToolDefinition> = serde_json::from_value(json!([
            {
                "type": "function",
                "function": {
                    "name": "foo",
                    "description": "A foo tool",
                    "parameters": {"type": "object", "properties": {}}
                }
            }
        ]))
        .unwrap();

        let tools = openai_tools_to_canonical(&tools_raw);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "foo");
        assert_eq!(tools[0].description, Some("A foo tool".to_string()));
    }

    // Test 7: Full request
    #[test]
    fn parse_request_with_all_fields() {
        let raw = json!({
            "model": "gpt-4-mini",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": "hi"}
            ],
            "tools": [{
                "type": "function",
                "function": {"name": "bar", "parameters": {}}
            }],
            "stream": true,
            "reasoning": {"effort": "high"}
        });

        let req = parse_chat_request(&raw).expect("parse");
        assert_eq!(req.public_model, "gpt-4-mini");
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "bar");
        assert!(req.stream);
    }
}
