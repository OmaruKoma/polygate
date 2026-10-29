# PolyGate API

三种接口共享同一条内部链路：

```
HTTP DTO → 内部 ChatRequest → 模型解析/别名 → 账号调度/会话 → Provider → 内部 ChatResponse/StreamEvent → HTTP DTO
```

Provider 适配器不感知客户端方言；三种接口的差异只存在于 `src/api/{openai,responses,anthropic}.rs`
与各自的 SSE 状态机。

## 鉴权

```http
Authorization: Bearer <gateway-api-key>
```

也接受 `x-api-key: <gateway-api-key>`。缺失或错误返回 `401`。网关 Key 与上游账号
凭据完全分离，管理台登录态不能调用 `/v1`。

## 通用约定

- 请求体必须是 JSON，未知字段会被拒绝（`400`），不会静默忽略。
- 返回 `429`/`503` 时可能带 `Retry-After`。
- 会话粘性：三接口都识别 `x-session-id` 头；也可由首条用户文本自动派生。
  同一 session 串行访问同一账号，不同 session 可并发。
- 模型名可以是原生 ID、`provider::model`（同名模型消歧）或管理台创建的别名。

## 1. OpenAI Chat Completions

`POST /v1/chat/completions`

支持的字段：`model`、`messages`、`stream`、`stream_options.include_usage`、
`max_tokens` / `max_completion_tokens`（二选一）、`temperature`（0–2）、`top_p`（0–1）、
`stop`（字符串或数组）、`tools`（仅 `function`）、`tool_choice`、`n`（仅 1）、`user`。

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{
    "model": "coding-auto",
    "messages": [{"role": "user", "content": "用一句话解释 TCP"}],
    "stream": false
  }'
```

响应为 `chat.completion`；`message.content` 可为 `null`（仅工具调用时），推理内容放在
`message.reasoning_content`，工具调用为 `message.tool_calls[]`。

流式响应：`chat.completion.chunk`，每个 chunk 的 `choices[0].delta` 可能是文本、
`reasoning_content` 或 `tool_calls` 参数片段；最后一个数据帧是 `data: [DONE]`。
`include_usage: true` 时在 `[DONE]` 前追加一个 `choices: []` 的 usage chunk。

## 2. OpenAI Responses

`POST /v1/responses`

支持字段：`model`、`input`、`instructions`、`tools`（扁平 `function` 定义）、
`tool_choice`、`max_output_tokens`、`temperature`、`top_p`、`stream`、`metadata`、
`user`、`store`（仅 `false`）、`parallel_tool_calls`（仅 `true`）。

`input` 可以是字符串，或以下 item 数组：

| item | 说明 |
| --- | --- |
| `{"role":"user","content":"..."}` | 普通消息；`content` 可为字符串或 `input_text` / `output_text` 部件 |
| `{"type":"function_call","call_id","name","arguments"}` | 上一轮助手的工具调用 |
| `{"type":"function_call_output","call_id","output"}` | 工具结果（字符串或文本部件） |

不支持 `previous_response_id`、非 function 工具、`strict: true` schema、图片以外的
未知部件；都会返回明确的 `400`。

```bash
curl http://127.0.0.1:8787/v1/responses \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"model":"coding-auto","input":"写一个 Fibonacci 函数"}'
```

非流式响应：

```json
{
  "id": "resp-...",
  "object": "response",
  "status": "completed",
  "model": "coding-auto",
  "output": [
    {"id":"msg-...","type":"message","status":"completed","role":"assistant",
     "content":[{"type":"output_text","text":"...","annotations":[]}]},
    {"id":"fc-...","type":"function_call","status":"completed",
     "call_id":"call-1","name":"get_weather","arguments":"{\"city\":\"Paris\"}"}
  ],
  "output_text": "...",
  "usage": {"input_tokens": 12, "output_tokens": 40, "total_tokens": 52}
}
```

`status` 为 `incomplete` 时附带 `incomplete_details.reason`
（`max_output_tokens` / `content_filter`）。

流式事件（`event:` 与 `data.type` 同名）：

```
response.created
response.output_item.added
response.content_part.added
response.output_text.delta / .done
response.content_part.done
response.output_item.done
response.function_call_arguments.delta / .done
response.completed      # 或 response.failed
```

`response.created` 先于任何内容；`response.completed` 携带完整 `output` 与 `usage`。
流中错误以 `response.failed` 结束；已经输出的内容不会被重发。

## 3. Anthropic Messages

`POST /v1/messages`

支持字段：`model`、`max_tokens`（必填）、`messages`、`system`（字符串或文本块数组）、
`tools`（`name` / `description` / `input_schema`）、`tool_choice`
（`auto` / `any` / `tool` / `none`）、`temperature`（0–1）、`top_p`、`stop_sequences`、
`stream`、`metadata`。

内容块：`text`、`image`（需目标模型声明 vision）、`tool_use`、`tool_result`。
`thinking` / `redacted_thinking` 与 `top_k` 明确不支持并返回 `400`。

```bash
curl http://127.0.0.1:8787/v1/messages \
  -H "x-api-key: $KEY" -H "Content-Type: application/json" \
  -d '{"model":"coding-auto","max_tokens":512,
       "messages":[{"role":"user","content":"hi"}]}'
```

响应：

```json
{
  "id": "msg-...",
  "type": "message",
  "role": "assistant",
  "model": "coding-auto",
  "content": [
    {"type": "text", "text": "..."},
    {"type": "tool_use", "id": "call-1", "name": "get_weather", "input": {"city": "Paris"}}
  ],
  "stop_reason": "end_turn",
  "stop_sequence": null,
  "usage": {"input_tokens": 12, "output_tokens": 40}
}
```

`stop_reason` 映射：完成 → `end_turn`，达到上限 → `max_tokens`，工具调用 →
`tool_use`，内容过滤 → `refusal`。上游返回无法识别的 stop reason 时按 `end_turn`
处理（CompatibilityGap）。

流式事件：

```
message_start
content_block_start
content_block_delta   # text_delta 或 input_json_delta
content_block_stop
message_delta         # stop_reason + output_tokens
message_stop
```

错误事件为 `event: error`，body 与下方 Anthropic 错误体相同。

### Anthropic 兼容性缺口（如实记录）

- `message_start.message.usage.input_tokens` 固定为 `0`：上游只在流末尾报告 usage，
  而 Messages 协议没有后续写入 input tokens 的位置。`message_delta.usage.output_tokens`
  是真实值；完整 usage 仍写入请求日志与用量统计。
- 推理/thinking 内容不会以 `text` 或 `thinking` 块回放（客户端未请求 thinking）。
- 一个 user turn 同时含文本与 `tool_result` 时，工具结果先转换为内部 tool 消息，
  其余文本作为用户消息；这是内部协议的结构限制。
- `top_k`、extended thinking 不支持。

## 错误格式

OpenAI 两种接口：

```json
{"error":{"message":"...","type":"invalid_request_error","code":"...","param":null}}
```

Anthropic：

```json
{"type":"error","error":{"type":"invalid_request_error","message":"..."}}
```

| 状态 | 含义 |
| --- | --- |
| 400 | 参数/消息结构错误、能力不满足（如工具/视觉） |
| 401 | 网关 Key 无效，或上游凭据失效 |
| 402 | 上游额度耗尽 |
| 403 | 账号被禁用/渠道受限 |
| 404 | 模型不存在或未启用 |
| 409 | 会话绑定失效 |
| 429 | 上游限流 |
| 501 | 该 Provider 不支持所请求能力 |
| 502/504 | 上游错误 / 超时 |
| 503 | 当前没有可用账号 |

重试策略：只有在**尚未向客户端输出任何内容**且失败可安全重放时（认证失败、402、
429、本地无容量）才会切换账号或目标；传输错误、超时、5xx 视为结果不确定，直接返回，
绝不自动重发。流式请求一旦开始传输，错误只会以流内错误事件结束。

## 模型列表

`GET /v1/models` 返回原生模型与可用别名；每项 `polygate` 字段说明来源 Provider、
Region、是否 Mock、验证状态。别名只要有一个启用的目标就会出现在列表中。

## 请求日志

每个 `/v1` 请求在 `request_logs` 表记录一条元数据：`request_id`、Provider/账号内部 ID、
别名、上游模型、API 种类、是否流式、HTTP 状态、错误类别、延迟与 token 数。
提示词、消息、工具参数与凭据永不入表。管理台“请求日志”页可查看并按天数清理。
