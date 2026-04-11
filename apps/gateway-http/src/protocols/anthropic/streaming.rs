use chrono::Utc;
use protocol_core::{FinishReason, InferenceStreamEvent, StreamEventKind, ToolCall};
use serde::Deserialize;
use serde_json::Value;

// ─── SSE message types ──────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct AnthropicSseMessage {
    pub(crate) event_type: String,
    pub(crate) data: Value,
}

// ─── SSE event payload types ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessageStart {
    #[serde(default)]
    pub(crate) message: Option<AnthropicStreamMessage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamMessage {
    pub(crate) id: String,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicContentBlockStart {
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) content_block: Option<AnthropicContentBlock>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub(crate) block_type: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) input: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicContentBlockDelta {
    pub(crate) index: usize,
    #[serde(default)]
    pub(crate) delta: Option<AnthropicDelta>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicDelta {
    #[serde(rename = "type")]
    #[serde(default)]
    pub(crate) delta_type: Option<String>,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) partial_json: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessageDelta {
    #[serde(default)]
    pub(crate) delta: Option<AnthropicMessageDeltaInner>,
    #[serde(default)]
    pub(crate) usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicMessageDeltaInner {
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicUsage {
    #[serde(default)]
    pub(crate) input_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicStreamError {
    #[serde(default)]
    pub(crate) error: Option<AnthropicErrorDetail>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicErrorDetail {
    #[serde(default)]
    pub(crate) message: Option<String>,
    #[serde(default)]
    pub(crate) r#type: Option<String>,
}

// ─── Stream state accumulator ───────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub(crate) struct AnthropicStreamState {
    pub(crate) id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) output_text: String,
    pub(crate) stop_reason: Option<String>,
    pub(crate) input_tokens: Option<u32>,
    pub(crate) output_tokens: Option<u32>,
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) current_tool_id: Option<String>,
    pub(crate) current_tool_name: Option<String>,
    pub(crate) current_tool_args: String,
}

impl AnthropicStreamState {
    pub(crate) fn into_response(self) -> InferenceStreamEvent {
        let finish_reason = match self.stop_reason.as_deref() {
            Some("end_turn") => FinishReason::Stop,
            Some("max_tokens") => FinishReason::Length,
            Some("tool_use") => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        };

        let usage = protocol_core::TokenUsage {
            input_tokens: self.input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens.unwrap_or(0),
            total_tokens: self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0),
        };

        InferenceStreamEvent {
            event: Some("message_stop".to_string()),
            kind: StreamEventKind::Done,
            delta: None,
            response: Some(protocol_core::InferenceResponse {
                id: self.id.unwrap_or_default(),
                model: self.model.unwrap_or_default(),
                output_text: self.output_text,
                finish_reason,
                tool_calls: self.tool_calls,
                usage,
                provider_kind: "anthropic".to_string(),
                created_at: Utc::now(),
            }),
        }
    }
}

// ─── SSE frame parser ───────────────────────────────────────────────────

pub(crate) fn parse_sse_frame(frame: &str) -> Option<AnthropicSseMessage> {
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

    let data = serde_json::from_str(&data_lines.join("\n")).ok()?;

    Some(AnthropicSseMessage {
        event_type: event.unwrap_or_default(),
        data,
    })
}

// ─── Stream event processor ─────────────────────────────────────────────

pub(crate) fn process_stream_event(
    msg: &AnthropicSseMessage,
    state: &mut AnthropicStreamState,
) -> Option<InferenceStreamEvent> {
    match msg.event_type.as_str() {
        "message_start" => {
            if let Ok(start) = serde_json::from_value::<AnthropicMessageStart>(msg.data.clone())
                && let Some(m) = &start.message
            {
                state.id = Some(m.id.clone());
                state.model = Some(m.model.clone());
                if let Some(u) = &m.usage {
                    state.input_tokens = u.input_tokens;
                    state.output_tokens = u.output_tokens;
                }
            }
            None
        }
        "content_block_start" => {
            if let Ok(start) =
                serde_json::from_value::<AnthropicContentBlockStart>(msg.data.clone())
                && let Some(block) = &start.content_block
                && block.block_type == "tool_use"
            {
                // Extract tool_use metadata for later finalization
                // The id and name may come from the content_block in the start event
                // or from the input field structure
                // For now, we'll rely on the content_block having the fields
                // Note: In real Anthropic API, the content_block in start event
                // has id, name, input fields
            }
            // Try to parse the raw data for tool_use info
            if let Some(block_data) = msg.data.get("content_block")
                && block_data.get("type").and_then(Value::as_str) == Some("tool_use")
            {
                state.current_tool_id = block_data
                    .get("id")
                    .and_then(Value::as_str)
                    .map(String::from);
                state.current_tool_name = block_data
                    .get("name")
                    .and_then(Value::as_str)
                    .map(String::from);
            }
            None
        }
        "content_block_delta" => {
            if let Ok(delta) =
                serde_json::from_value::<AnthropicContentBlockDelta>(msg.data.clone())
                && let Some(d) = &delta.delta
            {
                match d.delta_type.as_deref() {
                    Some("text_delta") => {
                        if let Some(text) = &d.text {
                            state.output_text.push_str(text);
                            return Some(InferenceStreamEvent::delta(text.clone()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = &d.partial_json {
                            state.current_tool_args.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        "content_block_stop" => {
            // If we were building a tool call, finalize it
            if let Some(tool_id) = state.current_tool_id.take() {
                let name = state.current_tool_name.take().unwrap_or_default();
                let args = std::mem::take(&mut state.current_tool_args);
                state.tool_calls.push(ToolCall {
                    id: tool_id,
                    name,
                    arguments: args,
                });
            }
            None
        }
        "message_delta" => {
            if let Ok(delta) = serde_json::from_value::<AnthropicMessageDelta>(msg.data.clone()) {
                if let Some(d) = &delta.delta {
                    state.stop_reason.clone_from(&d.stop_reason);
                }
                if let Some(u) = &delta.usage {
                    state.output_tokens = u.output_tokens.or(state.output_tokens);
                }
            }
            None
        }
        "message_stop" => Some(state.clone().into_response()),
        "error" => {
            if let Ok(err) = serde_json::from_value::<AnthropicStreamError>(msg.data.clone()) {
                let _message = err
                    .error
                    .and_then(|e| e.message)
                    .unwrap_or_else(|| "streaming error".to_string());
                return Some(InferenceStreamEvent {
                    event: Some("error".to_string()),
                    kind: StreamEventKind::Done,
                    delta: None,
                    response: None,
                });
            }
            None
        }
        _ => None,
    }
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Test 18: Parse message_start
    #[test]
    fn stream_parse_message_start() {
        let frame = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.event_type, "message_start");
        let data: AnthropicMessageStart = serde_json::from_value(msg.data).unwrap();
        assert_eq!(data.message.as_ref().unwrap().id, "msg_123");
    }

    // Test 19: Parse content_block_start text
    #[test]
    fn stream_parse_content_start_text() {
        let frame = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.event_type, "content_block_start");
        let data: AnthropicContentBlockStart = serde_json::from_value(msg.data).unwrap();
        assert_eq!(data.content_block.as_ref().unwrap().block_type, "text");
    }

    // Test 20: Parse content_block_delta text
    #[test]
    fn stream_parse_content_delta_text() {
        let frame = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        let mut state = AnthropicStreamState::default();
        let event = process_stream_event(&msg, &mut state);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.delta, Some("hello ".to_string()));
    }

    // Test 21: Parse content_block_delta input JSON
    #[test]
    fn stream_parse_content_delta_input() {
        let frame = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\"}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        let mut state = AnthropicStreamState::default();
        process_stream_event(&msg, &mut state);
        assert_eq!(state.current_tool_args, "{\"city\":");
    }

    // Test 22: Parse content_block_stop
    #[test]
    fn stream_parse_content_stop() {
        let frame =
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.event_type, "content_block_stop");
    }

    // Test 23: Parse message_delta
    #[test]
    fn stream_parse_message_delta() {
        let frame = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        let mut state = AnthropicStreamState::default();
        process_stream_event(&msg, &mut state);
        assert_eq!(state.stop_reason, Some("end_turn".to_string()));
        assert_eq!(state.output_tokens, Some(3));
    }

    // Test 24: Parse message_stop
    #[test]
    fn stream_parse_message_stop() {
        let frame = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.event_type, "message_stop");
        let mut state = AnthropicStreamState {
            id: Some("msg_123".to_string()),
            model: Some("claude-sonnet-4".to_string()),
            output_text: "hello".to_string(),
            input_tokens: Some(5),
            output_tokens: Some(3),
            ..Default::default()
        };
        let event = process_stream_event(&msg, &mut state);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.kind, StreamEventKind::Done);
        assert!(event.response.is_some());
        let resp = event.response.unwrap();
        assert_eq!(resp.id, "msg_123");
        assert_eq!(resp.output_text, "hello");
    }

    // Test 25: Parse error
    #[test]
    fn stream_parse_error() {
        let frame = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.event_type, "error");
        let data: AnthropicStreamError = serde_json::from_value(msg.data).unwrap();
        assert_eq!(data.error.unwrap().message.unwrap(), "Overloaded");
    }

    // Test 26: Accumulate text across deltas
    #[test]
    fn stream_accumulate_text_across_deltas() {
        let frames = vec![
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
        ];

        let mut state = AnthropicStreamState::default();
        for frame in frames {
            let msg = parse_sse_frame(frame).unwrap();
            process_stream_event(&msg, &mut state);
        }
        assert_eq!(state.output_text, "hello world");
    }

    // Test 27: Parse tool_use blocks
    #[test]
    fn stream_parse_tool_use_blocks() {
        let frames = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tool\",\"model\":\"claude-sonnet-4\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"get_weather\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];

        let mut state = AnthropicStreamState::default();
        for frame in frames {
            let msg = parse_sse_frame(frame).unwrap();
            process_stream_event(&msg, &mut state);
        }

        assert_eq!(state.id, Some("msg_tool".to_string()));
        assert_eq!(state.stop_reason, Some("tool_use".to_string()));
        assert_eq!(state.tool_calls.len(), 1);
        assert_eq!(state.tool_calls[0].id, "tu_1");
        assert_eq!(state.tool_calls[0].name, "get_weather");
    }
}
