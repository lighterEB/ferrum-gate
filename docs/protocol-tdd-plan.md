# 多协议网关 TDD 计划

> 本文档记录每个协议适配器的 TDD 测试计划，仅供本地参考，不纳入 git 追踪。
> 最后更新: 2026-04-11

## 目录

- [OpenAI 协议 TDD](#openai-协议-tdd)
- [Anthropic 协议 TDD](#anthropic-协议-tdd)
- [Gemini 协议 TDD](#gemini-协议-tdd)
- [Grok/xAI 协议 TDD](#grokxai-协议-tdd)
- [执行原则](#执行原则所有阶段通用)

---

## OpenAI 协议 TDD

### 阶段 1-A：OpenAI 请求解析

**文件**：`apps/gateway-http/src/protocols/openai/request.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 1 | `parse_basic_user_message` | `{"role":"user","content":"hi"}` | `CanonicalMessage { role: User, content: "hi", parts: [] }` |
| 2 | `parse_system_message` | `{"role":"system","content":"You are helpful"}` | `role: System, content: "You are helpful"` |
| 3 | `parse_assistant_message` | `{"role":"assistant","content":"hello"}` | `role: Assistant, content: "hello"` |
| 4 | `parse_tool_message` | `{"role":"tool","tool_call_id":"tc_1","content":"result"}` | `role: Tool, tool_call_id: Some("tc_1"), content: "result"` |
| 5 | `parse_messages_array` | 4 条消息数组 | 4 个 `CanonicalMessage` 按序映射 |
| 6 | `parse_tools_array` | `tools: [{type:"function",function:{name:"foo",parameters:{}}}]` | `ToolDefinition { name: "foo", ... }` |
| 7 | `parse_request_with_all_fields` | model + messages + tools + stream + reasoning | 全部正确映射到 `InferenceRequest` |

---

### 阶段 1-B：OpenAI 响应格式化

**文件**：`apps/gateway-http/src/protocols/openai/response.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 8 | `format_basic_text_response` | `InferenceResponse { output_text: "Hello" }` | `{"choices":[{"message":{"content":"Hello"}}]}` |
| 9 | `format_response_with_tool_calls` | 带 tool_calls 的响应 | tool_calls 数组正确输出 |
| 10 | `format_finish_stop` | `FinishReason::Stop` | `"finish_reason":"stop"` |
| 11 | `format_finish_length` | `FinishReason::Length` | `"finish_reason":"length"` |
| 12 | `format_finish_tool_calls` | `FinishReason::ToolCalls` | `"finish_reason":"tool_calls"` |
| 13 | `format_usage` | usage 数据 | `prompt_tokens / completion_tokens / total_tokens` 正确 |
| 14 | `format_response_id_and_model` | id / model / created | 字段正确出现在响应顶层 |

---

### 阶段 1-C：OpenAI 流式解析

**文件**：`apps/gateway-http/src/protocols/openai/streaming.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 15 | `stream_parse_text_delta` | `data: {"choices":[{"delta":{"content":"Hello"}}]}` | delta = "Hello" |
| 16 | `stream_parse_tool_call_delta` | delta 含 tool_call 片段 | id/name/arguments 正确解析 |
| 17 | `stream_parse_done_marker` | `data: [DONE]` | 返回 Done 事件 |
| 18 | `stream_parse_error_event` | error payload | 提取 message 和 code |
| 19 | `stream_accumulate_multiple_deltas` | 连续 3 个 delta | 文本正确拼接 |
| 20 | `stream_parse_empty_delta` | `delta: {}` | 忽略，不产出 |

---

### 阶段 1-D：OpenAI Responses API

**文件**：`apps/gateway-http/src/protocols/openai/responses.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 21 | `parse_responses_request_string_input` | `"input": "hello"` | 单条 user message |
| 22 | `parse_responses_request_array_input` | `input: [{role, content}, {type: "function_call"}]` | 正确映射 |
| 23 | `format_responses_response_basic` | `InferenceResponse` | `{"output": [{type: "message", content: [...]}]}` |
| 24 | `format_responses_response_with_tool_call` | 带 tool_calls | `output` 包含 `{type: "function_call", ...}` |

---

## Anthropic 协议 TDD

### 阶段 2-A：Anthropic 请求解析

**文件**：`apps/gateway-http/src/protocols/anthropic/request.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 1 | `parse_basic_user_message` | `{"role":"user","content":"hi"}` | `CanonicalMessage { role: User, content: "hi" }` |
| 2 | `parse_system_as_string` | `system: "You are Claude"` | `CanonicalMessage { role: System, content: "You are Claude" }` |
| 3 | `parse_system_as_array` | `system: [{type:"text",text:"R1"},{type:"text",text:"R2"}]` | 合并为一条 system message |
| 4 | `parse_multi_content_user` | `content: [{type:"text",text:"hi"},{type:"image",source:{type:"base64",data:"..."}}]` | text + image_url parts |
| 5 | `parse_assistant_message` | `role: "assistant"` | `role: Assistant` |
| 6 | `parse_tool_result_message` | `role: "user", content: [{type:"tool_result",tool_use_id:"tu_1",content:"result"}]` | `role: Tool, tool_call_id: "tu_1"` |
| 7 | `parse_tool_use_from_assistant` | assistant 的 `content: [{type:"tool_use",id:"tu_1",name:"foo",input:{}}]` | `tool_calls` 填充 |
| 8 | `parse_tools_parameter` | `tools: [{name:"foo",description:"...",input_schema:{}}]` | `ToolDefinition` 数组 |
| 9 | `parse_full_request` | model + max_tokens + system + messages + tools | 全部正确映射到 `InferenceRequest` |

---

### 阶段 2-B：Anthropic 响应格式化

**文件**：`apps/gateway-http/src/protocols/anthropic/response.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 10 | `format_basic_text_response` | `output_text: "Hello"` | `{content:[{type:"text",text:"Hello"}], role:"assistant", stop_reason:"end_turn", usage:{input_tokens:10,output_tokens:5}}` |
| 11 | `format_response_tool_use` | tool_calls | `content: [{type:"tool_use",id:"...",name:"...",input:{}}]` |
| 12 | `format_stop_end_turn` | `FinishReason::Stop` | `stop_reason: "end_turn"` |
| 13 | `format_stop_max_tokens` | `FinishReason::Length` | `stop_reason: "max_tokens"` |
| 14 | `format_stop_tool_use` | `FinishReason::ToolCalls` | `stop_reason: "tool_use"` |
| 15 | `format_stop_content_filter` | `FinishReason::ContentFilter` | `stop_reason: "end_turn"` |
| 16 | `format_usage` | usage 数据 | `input_tokens / output_tokens` 正确 |
| 17 | `format_response_model_field` | model 字段 | 出现在响应顶层 |

---

### 阶段 2-C：Anthropic 流式解析

**文件**：`apps/gateway-http/src/protocols/anthropic/streaming.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 18 | `stream_parse_message_start` | `type: "message_start"` | 初始化响应 |
| 19 | `stream_parse_content_start_text` | `type: "content_block_start", content_block: {type:"text"}` | 标记文本块开始 |
| 20 | `stream_parse_content_delta_text` | `type: "content_block_delta", delta: {text:"Hello"}` | delta = "Hello" |
| 21 | `stream_parse_content_delta_input` | `type: "content_block_delta", delta: {partial_json:"{\"key\":"}` | 累积 JSON |
| 22 | `stream_parse_content_stop` | `type: "content_block_stop"` | 块结束 |
| 23 | `stream_parse_message_delta` | `type: "message_delta", delta: {stop_reason:"end_turn"}` | 记录 stop_reason |
| 24 | `stream_parse_message_stop` | `type: "message_stop"` | 返回 Done |
| 25 | `stream_parse_error` | `type: "error"` | 提取 error message |
| 26 | `stream_accumulate_text_across_deltas` | 5 个连续 delta | 拼接完整文本 |
| 27 | `stream_parse_tool_use_blocks` | tool_use start + input delta + stop | 正确组装 tool_call |

---

### 阶段 2-D：Anthropic 端点 Handler

**文件**：`apps/gateway-http/src/routes/messages.rs`

| # | 测试名 | 预期行为 |
|---|--------|---------|
| 28 | `messages_endpoint_parses_anthropic_body` | POST /v1/messages → 解析 → 返回 Anthropic 格式 |
| 29 | `messages_endpoint_returns_anthropic_format` | 响应 `content-type: application/json`，body 是 Anthropic 格式 |
| 30 | `messages_endpoint_propagates_error` | 上游失败 → 返回 Anthropic 格式错误 `{type: "error", error: {message: "..."}}` |

---

## Gemini 协议 TDD

### 阶段 3-A：Gemini 请求解析（骨架）

**文件**：`apps/gateway-http/src/protocols/gemini/request.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 1 | `parse_basic_text_prompt` | `{"contents":[{"role":"user","parts":[{"text":"hello"}]}]}` | `CanonicalMessage { role: User, content: "hello" }` |
| 2 | `parse_system_instruction` | `systemInstruction: {parts:[{text:"You are helpful"}]}` | `role: System` |
| 3 | `parse_multi_content` | 多个 parts（text + inlineData） | 正确映射 |

---

### 阶段 3-B：Gemini 响应格式化（骨架）

**文件**：`apps/gateway-http/src/protocols/gemini/response.rs`

| # | 测试名 | 输入 | 预期输出 |
|---|--------|------|---------|
| 4 | `format_basic_text_response` | `output_text: "Hello"` | `{"candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}` |
| 5 | `format_usage` | usage 数据 | 映射到 `usageMetadata` |
| 6 | `format_finish_reason` | stop / length | 正确映射到 `finishReason` |

---

## Grok/xAI 协议 TDD

### 阶段 4-A：Grok 协议（复用 OpenAI 格式）

**文件**：`apps/gateway-http/src/protocols/grok/mod.rs`

| # | 测试名 | 预期行为 |
|---|--------|---------|
| 1 | `grok_uses_openai_request_format` | 验证 Grok 请求体就是 OpenAI 格式 → 直接复用 `openai::request` |
| 2 | `grok_uses_openai_response_format` | 验证 Grok 响应就是 OpenAI 格式 → 直接复用 `openai::response` |

**实现**：re-export openai 模块 + 文档说明 Grok 兼容 OpenAI 协议。

---

## 执行原则（所有阶段通用）

```
每个阶段内:
  1. RED:   写测试 → cargo test → 确认失败
  2. GREEN: 写最少实现让测试通过
  3. REFACTOR: 清理代码 → cargo test 确认仍通过

阶段间隔离:
  - 每个阶段有独立的文件（无交叉依赖）
  - 每个阶段完成后 cargo test --all 必须全部通过
  - 每个阶段不影响其他协议的端点行为
  - 每个阶段可独立提交（如需）

文件组织:
  apps/gateway-http/src/protocols/
  ├── openai/
  │   ├── mod.rs
  │   ├── request.rs      (测试 1-7)
  │   ├── response.rs     (测试 8-14)
  │   ├── streaming.rs    (测试 15-20)
  │   └── responses.rs    (测试 21-24)
  ├── anthropic/
  │   ├── mod.rs
  │   ├── request.rs      (测试 1-9)
  │   ├── response.rs     (测试 10-17)
  │   ├── streaming.rs    (测试 18-27)
  │   └── routes/messages.rs (测试 28-30)
  ├── gemini/
  │   ├── mod.rs
  │   ├── request.rs      (测试 1-3)
  │   └── response.rs     (测试 4-6)
  └── grok/
      └── mod.rs          (测试 1-2, re-export openai)
```
