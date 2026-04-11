use protocol_core::{
    CanonicalMessage, ContentPart, FrontendProtocol, InferenceRequest, MessageRole, ToolDefinition,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ─── Anthropic request types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessagesRequest {
    pub(crate) model: String,
    pub(crate) messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub(crate) system: Option<AnthropicSystemPrompt>,
    #[serde(default)]
    pub(crate) max_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) tools: Vec<AnthropicToolDefinition>,
    #[serde(default)]
    pub(crate) stream: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessage {
    pub(crate) role: String,
    #[serde(default)]
    pub(crate) content: AnthropicMessageContent,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
pub(crate) enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
    #[default]
    Empty,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub(crate) block_type: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) source: Option<AnthropicImageSource>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) input: Option<Value>,
    #[serde(default)]
    pub(crate) tool_use_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicImageSource {
    #[serde(rename = "type")]
    pub(crate) source_type: String,
    #[serde(default)]
    pub(crate) media_type: Option<String>,
    #[serde(default)]
    pub(crate) data: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum AnthropicSystemPrompt {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicToolDefinition {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) input_schema: Value,
}

// ─── Conversion functions ───────────────────────────────────────────────

fn content_blocks_to_parts(blocks: &[AnthropicContentBlock]) -> Vec<ContentPart> {
    blocks
        .iter()
        .filter_map(|block| match block.block_type.as_str() {
            "text" => block
                .text
                .as_ref()
                .map(|t| ContentPart::Text { text: t.clone() }),
            "image" => {
                let url = block.source.as_ref().and_then(|src| {
                    if src.source_type == "base64" {
                        Some(format!(
                            "data:{};base64,{}",
                            src.media_type.as_deref().unwrap_or("image/png"),
                            src.data.as_deref().unwrap_or("")
                        ))
                    } else {
                        src.url.clone()
                    }
                });
                url.map(|image_url| ContentPart::ImageUrl { image_url })
            }
            _ => None,
        })
        .collect()
}

fn anthropic_content_to_parts(content: &AnthropicMessageContent) -> Vec<ContentPart> {
    match content {
        AnthropicMessageContent::Text(text) if text.is_empty() => Vec::new(),
        AnthropicMessageContent::Text(text) => {
            vec![ContentPart::Text { text: text.clone() }]
        }
        AnthropicMessageContent::Blocks(blocks) => content_blocks_to_parts(blocks),
        AnthropicMessageContent::Empty => Vec::new(),
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

fn parse_role(role: &str) -> MessageRole {
    match role {
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn message_to_canonical(msg: &AnthropicMessage) -> CanonicalMessage {
    let parts = anthropic_content_to_parts(&msg.content);
    let text = text_from_parts(&parts);

    // Check if this is a tool result message (user role with tool_result blocks)
    if msg.role == "user"
        && let AnthropicMessageContent::Blocks(blocks) = &msg.content
        && let Some(tool_result_block) = blocks.iter().find(|b| b.block_type == "tool_result")
    {
        return CanonicalMessage {
            role: MessageRole::Tool,
            content: tool_result_block
                .text
                .as_deref()
                .unwrap_or(&text)
                .to_string(),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: tool_result_block.tool_use_id.clone(),
        };
    }

    // Check for tool_use blocks in assistant messages
    if msg.role == "assistant"
        && let AnthropicMessageContent::Blocks(blocks) = &msg.content
    {
        let tool_calls: Vec<_> = blocks
            .iter()
            .filter(|b| b.block_type == "tool_use")
            .filter_map(|b| {
                Some(protocol_core::ToolCall {
                    id: b.id.clone()?,
                    name: b.name.clone()?,
                    arguments: b
                        .input
                        .as_ref()
                        .map(|v| serde_json::to_string(v).unwrap_or_default())
                        .unwrap_or_default(),
                })
            })
            .collect();

        if !tool_calls.is_empty() {
            return CanonicalMessage {
                role: MessageRole::Assistant,
                content: text,
                parts,
                tool_calls,
                tool_call_id: None,
            };
        }
    }

    CanonicalMessage {
        role: parse_role(&msg.role),
        content: text,
        parts,
        tool_calls: vec![],
        tool_call_id: None,
    }
}

fn system_to_canonical(system: &AnthropicSystemPrompt) -> Option<CanonicalMessage> {
    match system {
        AnthropicSystemPrompt::Text(text) => Some(CanonicalMessage {
            role: MessageRole::System,
            content: text.clone(),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: None,
        }),
        AnthropicSystemPrompt::Blocks(blocks) => {
            let parts = content_blocks_to_parts(blocks);
            let text = text_from_parts(&parts);
            if text.is_empty() {
                None
            } else {
                Some(CanonicalMessage {
                    role: MessageRole::System,
                    content: text,
                    parts,
                    tool_calls: vec![],
                    tool_call_id: None,
                })
            }
        }
    }
}

fn anthropic_tools_to_canonical(tools: &[AnthropicToolDefinition]) -> Vec<ToolDefinition> {
    tools
        .iter()
        .map(|t| ToolDefinition {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: if t.input_schema.is_null() {
                serde_json::json!({})
            } else {
                t.input_schema.clone()
            },
        })
        .collect()
}

pub(crate) fn parse_anthropic_request(raw: &Value) -> Option<InferenceRequest> {
    let model = raw.get("model")?.as_str()?.to_string();
    let messages: Vec<AnthropicMessage> = serde_json::from_value(raw["messages"].clone()).ok()?;
    let tools: Vec<AnthropicToolDefinition> = raw
        .get("tools")
        .and_then(|t| serde_json::from_value(t.clone()).ok())
        .unwrap_or_default();

    let mut canonical_messages: Vec<CanonicalMessage> = Vec::new();

    // Prepend system message if present
    if let Some(system_raw) = raw.get("system") {
        let system: AnthropicSystemPrompt = serde_json::from_value(system_raw.clone()).ok()?;
        if let Some(sys_msg) = system_to_canonical(&system) {
            canonical_messages.push(sys_msg);
        }
    }

    // Add conversation messages
    for msg in &messages {
        canonical_messages.push(message_to_canonical(msg));
    }

    Some(InferenceRequest {
        protocol: FrontendProtocol::Anthropic,
        public_model: model,
        upstream_model: None,
        previous_response_id: None,
        reasoning: None,
        stream: raw.get("stream").and_then(Value::as_bool).unwrap_or(false),
        messages: canonical_messages,
        tools: anthropic_tools_to_canonical(&tools),
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
        let msg: AnthropicMessage = serde_json::from_value(json!({
            "role": "user",
            "content": "hi"
        }))
        .unwrap();

        let canonical = message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::User);
        assert_eq!(canonical.content, "hi");
    }

    // Test 2: System as string
    #[test]
    fn parse_system_as_string() {
        let system: AnthropicSystemPrompt =
            serde_json::from_value(json!("You are Claude")).unwrap();
        let canonical = system_to_canonical(&system).unwrap();
        assert_eq!(canonical.role, MessageRole::System);
        assert_eq!(canonical.content, "You are Claude");
    }

    // Test 3: System as array
    #[test]
    fn parse_system_as_array() {
        let system: AnthropicSystemPrompt = serde_json::from_value(json!([
            {"type": "text", "text": "Rule 1"},
            {"type": "text", "text": "Rule 2"}
        ]))
        .unwrap();
        let canonical = system_to_canonical(&system).unwrap();
        assert_eq!(canonical.role, MessageRole::System);
        assert_eq!(canonical.content, "Rule 1\nRule 2");
    }

    // Test 4: Multi-content user message
    #[test]
    fn parse_multi_content_user() {
        let msg: AnthropicMessage = serde_json::from_value(json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hi"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc123"}}
            ]
        }))
        .unwrap();

        let canonical = message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::User);
        assert_eq!(canonical.parts.len(), 2);
        assert!(matches!(&canonical.parts[0], ContentPart::Text { text } if text == "hi"));
        assert!(
            matches!(&canonical.parts[1], ContentPart::ImageUrl { image_url } if image_url.contains("data:image/png"))
        );
    }

    // Test 5: Assistant message
    #[test]
    fn parse_assistant_message() {
        let msg: AnthropicMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": "hello"
        }))
        .unwrap();

        let canonical = message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::Assistant);
        assert_eq!(canonical.content, "hello");
    }

    // Test 6: Tool result message
    #[test]
    fn parse_tool_result_message() {
        let msg: AnthropicMessage = serde_json::from_value(json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "text": "result", "content": "result"}
            ]
        }))
        .unwrap();

        let canonical = message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::Tool);
        assert_eq!(canonical.tool_call_id, Some("tu_1".to_string()));
    }

    // Test 7: Tool use from assistant
    #[test]
    fn parse_tool_use_from_assistant() {
        let msg: AnthropicMessage = serde_json::from_value(json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "tu_1", "name": "foo", "input": {"city": "Shanghai"}}
            ]
        }))
        .unwrap();

        let canonical = message_to_canonical(&msg);
        assert_eq!(canonical.role, MessageRole::Assistant);
        assert_eq!(canonical.tool_calls.len(), 1);
        assert_eq!(canonical.tool_calls[0].id, "tu_1");
        assert_eq!(canonical.tool_calls[0].name, "foo");
    }

    // Test 8: Tools parameter
    #[test]
    fn parse_tools_parameter() {
        let tools: Vec<AnthropicToolDefinition> = serde_json::from_value(json!([
            {
                "name": "foo",
                "description": "A foo tool",
                "input_schema": {"type": "object", "properties": {}}
            }
        ]))
        .unwrap();

        let canonical = anthropic_tools_to_canonical(&tools);
        assert_eq!(canonical.len(), 1);
        assert_eq!(canonical[0].name, "foo");
        assert_eq!(canonical[0].description, Some("A foo tool".to_string()));
    }

    // Test 9: Full request
    #[test]
    fn parse_full_request() {
        let raw = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": "hi"}
            ],
            "tools": [{
                "name": "bar",
                "input_schema": {}
            }],
            "stream": true
        });

        let req = parse_anthropic_request(&raw).expect("parse");
        assert_eq!(req.public_model, "claude-sonnet-4-20250514");
        assert_eq!(req.messages.len(), 2); // system + user
        assert_eq!(req.messages[0].role, MessageRole::System);
        assert_eq!(req.messages[1].role, MessageRole::User);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "bar");
        assert!(req.stream);
    }
}
