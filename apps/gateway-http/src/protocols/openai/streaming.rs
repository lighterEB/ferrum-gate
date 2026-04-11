use protocol_core::{FinishReason, InferenceStreamEvent, StreamEventKind, ToolCall};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) struct SseMessage {
    pub event: Option<String>,
    pub data: String,
}

pub(crate) fn parse_sse_frame(frame: &str) -> Option<SseMessage> {
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

pub(crate) fn chat_stream_delta(model: &str, delta: &str) -> String {
    format_chunk(model, json!({ "content": delta }), Value::Null)
}

pub(crate) fn chat_stream_tool_delta(model: &str, tool_calls: &[ToolCall]) -> String {
    format_chunk(
        model,
        json!({ "tool_calls": stream_tool_calls_json(tool_calls) }),
        Value::Null,
    )
}

pub(crate) fn chat_stream_done(model: &str, reason: &FinishReason) -> String {
    format_chunk(model, json!({}), json!(finish_reason_label(reason)))
}

pub(crate) fn chat_stream_error(error_message: &str) -> String {
    let body = json!({
        "error": {
            "message": error_message,
            "type": "server_error",
            "code": null,
            "param": null
        }
    });
    format!("data: {}\n\n", body)
}

fn format_chunk(model: &str, delta: Value, finish: Value) -> String {
    let payload = json!({
        "id": "",
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish
        }]
    });
    format!("data: {}\n\n", payload)
}

fn stream_tool_calls_json(tool_calls: &[ToolCall]) -> Vec<Value> {
    tool_calls
        .iter()
        .map(|tc| {
            json!({
                "index": 0,
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.name,
                    "arguments": tc.arguments
                }
            })
        })
        .collect()
}

fn finish_reason_label(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Error => "error",
    }
}

pub(crate) fn done_marker() -> InferenceStreamEvent {
    InferenceStreamEvent {
        event: Some("message_stop".to_string()),
        kind: StreamEventKind::Done,
        delta: None,
        response: None,
    }
}

// ─── TDD Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Test 15: Parse text delta
    #[test]
    fn stream_parse_text_delta() {
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert!(msg.event.is_none());
        let parsed: Value = serde_json::from_str(&msg.data).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");
    }

    // Test 16: Parse tool_call delta
    #[test]
    fn stream_parse_tool_call_delta() {
        let frame = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"tc_1","type":"function","function":{"name":"foo","arguments":"{}"}}]}}]}

"#;
        let msg = parse_sse_frame(frame).expect("parsed");
        let parsed: Value = serde_json::from_str(&msg.data).unwrap();
        let tc = &parsed["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["id"], "tc_1");
        assert_eq!(tc["function"]["name"], "foo");
    }

    // Test 17: Parse [DONE] marker
    #[test]
    fn stream_parse_done_marker() {
        let frame = "data: [DONE]\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        assert_eq!(msg.data, "[DONE]");
    }

    // Test 18: Parse error event
    #[test]
    fn stream_parse_error_event() {
        let err = chat_stream_error("rate limited");
        assert!(err.contains("rate limited"));
        let body: Value = serde_json::from_str(err.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(body["error"]["message"], "rate limited");
        assert_eq!(body["error"]["type"], "server_error");
    }

    // Test 19: Accumulate multiple deltas
    #[test]
    fn stream_accumulate_multiple_deltas() {
        let frames = vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n\n",
        ];

        let mut text = String::new();
        for frame in frames {
            let msg = parse_sse_frame(frame).unwrap();
            let parsed: Value = serde_json::from_str(&msg.data).unwrap();
            if let Some(d) = parsed["choices"][0]["delta"]["content"].as_str() {
                text.push_str(d);
            }
        }
        assert_eq!(text, "Hello!");
    }

    // Test 20: Parse empty delta
    #[test]
    fn stream_parse_empty_delta() {
        let frame = "data: {\"choices\":[{\"delta\":{}}]}\n\n";
        let msg = parse_sse_frame(frame).expect("parsed");
        let parsed: Value = serde_json::from_str(&msg.data).unwrap();
        assert!(parsed["choices"][0]["delta"]["content"].is_null());
    }
}
