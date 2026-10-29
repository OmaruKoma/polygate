# TRAE Provider 迁移文档（M14-D）

> 状态：**M14-D 协议核心完成，MockVerified**。日期 2026-10-29。
> 本文是 TRAE 适配的唯一协议依据：所有结论都标注了源码位置；没有源码依据的字段
> 一律不实现，也不猜测。
>
> 审计对象（只读，未复制代码）：
>
> - `.refs/RobbsLuo_Coding2API/src/provider/trae/`（下称 **Coding2API**）
> - `.refs/chengzhi-c_dsh-wtq-pool/src/trae-*.ts`（下称 **dsh**）
>
> 两个参考实现有明显冲突或不一致之处，本文逐条注明取舍。本项目只吸收协议事实，
> 独立实现；没有复制任何受限代码。

## 0. 范围

M14-D 只做**协议核心**：

- 凭据解析与区域隔离；
- 模型发现；
- 非流式 / SSE 流式聊天；
- Tool Calling；
- Usage / FinishReason；
- 错误归一化；
- 超时与断流。

以下能力按阶段划分属于 **M14-E**，已在 M14-E 实现（记录见 §18）：
token 刷新、额度查询、签到、回调授权、管理台 TRAE 授权入口（粘贴回调链接）。
§4 / §10 / §11 保留协议事实；§16 是 M14-E 的结项对照。

## 1. 结论摘要

| 结论 | 依据 |
| --- | --- |
| CN 与 Intl 是两套网关、两套账号；Token 不互通 | Coding2API `events.py:29-31`；dsh `trae-upstream.ts:47-58` |
| 聊天端点不是 OpenAI 兼容，必须重写请求体 | Coding2API `client.py:84-234` |
| 上游只提供流式；非流式由网关聚合 | Coding2API `client.py:320-350`；dsh `trae-shim.ts` |
| SSE 是命名事件（`output` / `token_usage` / `done` / `error`） | Coding2API `events.py:26-90` |
| Tool Call 使用 `function_call`，按 index 分片 | Coding2API `events.py:91-125` |
| 工具 `parameters` 必须是 JSON **字符串** | Coding2API `client.py:203-235`（4001 回归） |
| 上游无原生会话、无取消端点 | 两个参考实现均未发现；见 §9 |
| 官方客户端头族是 TRAE IDE 的身份 | Coding2API `client.py:237-300` |
| 设备号由登录/签到流程生成，本项目**不生成** | Coding2API `callback.py`、`credential.py:111-140` |
| 本阶段无真实账号、未发真实请求 | 本轮约束；状态 `experimental` |

## 2. 区域与端点

| 用途 | CN | International |
| --- | --- | --- |
| 聊天 + 模型目录 | `https://trae-api-cn.mchost.guru` | `https://coresg-normal.trae.ai` |
| 额度 / 签到 | `https://api.trae.cn` | `https://growsg-normal.trae.ai` |
| OAuth（ExchangeToken / GetUserInfo） | `https://api.trae.com.cn` | **未验证**：两个参考实现都没有给出可信主机 |
| 默认域 | `trae.cn` | `trae.ai` |

- 聊天路径：`POST /api/agent/v3/llm_utils_chat`。
- 模型目录路径：`POST /api/ide/v1/get_detail_param`。
- 代码：`src/providers/trae/region.rs`。
- CN 端点来自 Coding2API `events.py:29-44`；Intl 端点只来自 dsh
  `trae-upstream.ts:54-56`，因此 Intl 的 OAuth 主机留空，M14-E 不得按 CN 主机
  猜一个 Intl 地址。
- **隔离规则**：凭据的 `domain` 必须属于配置区域；`apiHost` 只允许本区域官方主机或
  `trae.*` 域。一条 CN 凭据不能因为改配置就在 Intl 生效。

## 3. 认证与凭据格式

### 3.1 上游要求的头（参考实现的观测）

Coding2API `client.py:237-300` 发送：

```text
Content-Type / Accept
User-Agent: Trae/<IDE_VERSION>
Authorization: Cloud-IDE-JWT <token>
X-Cloudide-Token / X-Ide-Token: <token>
X-App-Id / X-App-Version / X-Ide-Version / X-Ide-Version-Code / X-App-Version-Code
X-Ide-Version-Type / X-Device-Type / X-OS-Version / X-Device-Brand
Request-Traffic-Type: prod
X-Uid / X-Machine-Id / X-Device-Id        (when known)
```

dsh 另外附带 `referer`、`timezone`、`language`（`trae-upstream.ts:1026-1027`）。

### 3.2 本项目的取舍（合规边界）

- **默认（`honest`）只发送认证必需头**：`Authorization: Cloud-IDE-JWT`、
  `X-Cloudide-Token`、`X-Ide-Token`、`X-Uid`，以及运维显式导入的
  `X-Machine-Id` / `X-Device-Id`。UA 为 `polygate/<version>`。
- **`client.identity = "impersonate"`**：额外发送 `X-App-Id`、IDE 版本族、设备类型/品牌/
  OS、`Request-Traffic-Type` 与 `Trae/<ver>` UA（chat 与计费端点都发）。常量见
  `src/providers/profile.rs`。这些是客户端公开常量，不是设备指纹。
- **两种模式都不生成设备号**：`X-Machine-Id` / `X-Device-Id` 只透传导入值，缺失就缺失。
- 代码：`src/providers/trae/http.rs`、`src/providers/profile.rs`；测试
  `honest_headers_never_impersonate_the_ide`、
  `imported_device_identifiers_are_forwarded_not_invented`、
  `impersonation_sends_the_ide_constants_on_chat`。

### 3.3 凭据字段

Coding2API `credential.py:73-108` 兼容两种形状：

| 扁平 | 嵌套（IDE 落盘） | 含义 |
| --- | --- | --- |
| `accessToken` / `access_token` | `auth.accessToken` | 必填 |
| `refreshToken` / `refresh_token` | `auth.refreshToken` | M14-E 刷新用 |
| `uid` | `account.uid` | 必填 |
| `expiresAt` / `expires_at` | `auth.expiresAt` | 秒或毫秒；缺失回落 JWT `exp` |
| `domain` | `auth.domain` | 必须属于区域 |
| `apiHost` | `auth.apiHost` | 只用于 OAuth；必须命中区域白名单 |
| `machineId` | `auth.machineId` | 只透传 |
| `deviceId` | `auth.deviceId` | 只透传 |
| `enterpriseId` | `account.enterpriseId` | 可选 |
| `nickname` | `account.nickname` | 可选 |

- `expires_at` 只保存上游显式给出的值；JWT 派生值只在 `token_expires_at()` 内计算，
  **不写回** JSON，避免刷新后旧派生值变成“权威”到期时间。
- JWT 的 `sub` 与 `uid` 不一致 → `AuthInvalid`（防止把 A 的 token 配到 B 的账号）。
- Debug 与 Drop 同时覆盖 access / refresh token。
- 代码：`src/providers/trae/credential.rs`（9 个单元测试）。

## 4. Token 刷新（M14-E 已实现，见 §18.1）

- 端点：`POST {oauth_base}/cloudide/api/v3/trae/oauth/ExchangeToken`
  （Coding2API `events.py:41`）。
- 请求体（`client.py:478-500`）：
  `{"ClientID": "<client id>", "RefreshToken": "…", "ClientSecret": "-", "UserID": ""}`。
- 响应：`Result.Token`、`Result.RefreshToken`、`Result.TokenExpireAt`（毫秒）或
  `Result.TokenExpireDuration`（秒）。
- `apiHost` 是用户可控输入：Coding2API 用白名单校验，越界时回退官方主机
  （`client.py:607-619`）。本项目在**导入时**直接拒绝越界值，让问题立刻可见。
- `ClientID` 属于官方客户端常量；M14-E 审定后作为**协议必需参数**引入
  （`en1oxy7wnw8j9n`，非密钥、不含设备信息），但**不引入**官方客户端头族。
- 记录：刷新失败不得改写原凭据字段；refresh token 轮换必须先持久化再使用
  （与 WorkBuddy 的纪律一致）。

## 5. 模型发现

- 请求（Coding2API `client.py:354-363`）：

```json
{
  "function": "solo_work_lite",
  "config_names": null,
  "need_prompt": false,
  "current_config_info": null,
  "poly_prompt": true,
  "mode_type": null,
  "agent_type": null
}
```

- 响应：`config_info_list[]`，每项 `config_name`、`display_config.display_name`、
  `context_window_tokens.{dev,max}`（Coding2API `client.py:365-405`）。
- `context_window_tokens` 是两档：`max` 是真实上限（1M 模型在 `dev` 下会只报
  200k），`max > dev` 时取 `max`，否则取 `dev`。
- 空列表 / 缺字段 → `UpstreamServer`，**不回退静态模型表**。Coding2API 有
  `STATIC_MODELS`（`client.py:70-82`），但其中包含 `custom_model_*`、子代理等
  非通用模型；在无真实账号的情况下照抄会发布无法验证的目录。此差异记入 §15。
- `max_output_tokens` 本轮不解析：参考实现也没有给出证据，保持 `null`。
- 代码：`src/providers/trae/models.rs`。

## 6. 聊天接口

### 6.1 信封

`llm_utils_chat` 只接受自己的字段（Coding2API `client.py:84-103`）：

```json
{
  "function": "solo_work_lite",
  "config_name": "<model>",
  "model": "<model>",
  "stream": true,
  "messages": [...],
  "tools": [...]        // 可选
}
```

- `stream` 强制 `true`；非流式由网关聚合。
- 不转发 `temperature` / `top_p` / `max_tokens` / `stop`：没有任何源码证据表明
  该信封接受这些字段，凭猜测发送属于协议臆造。
- 不转发 `metadata`、不拼接系统提示、不做提示词改写。

### 6.2 消息改写

| 改写 | 原因 |
| --- | --- |
| `developer` → `system` | TRAE 不认 `developer`，会静默返回空流（3003） |
| 字符串 content → `[{"type":"text","text":…}]` | 上游只接受内容块数组 |
| assistant `tool_calls[].function` → `function_call` | SOLO 的函数调用字段名 |
| 纯工具调用轮次的 content → `null` | 与 IDE 自己的占位消息一致 |
| tools 的 `parameters` 对象 → JSON 字符串 | 上游字段类型是 string；不转会 4001 |
| `tool_choice: "none"` → 删除 `tools` / `tool_choice` | 上游不接受“有策略但无工具” |
| `tool_choice` 函数对象 → 函数名 | 上游只接受字符串 |
| 无 tools 时省略 `tool_choice` | 同上 |

### 6.3 与参考实现的差异（有意）

- **孤儿 tool 结果 / 无 name 的 tool call 直接 `BadRequest`**。Coding2API 会静默
  丢弃（`client.py:104-130`、`_drop_orphan_tool_results`）；本项目的会话历史完整性
  规则要求脏配对必须显式报错，不能在网关内悄悄重写调用方的历史。
- 代码：`src/providers/trae/convert.rs`。

## 7. SSE 事件格式

上游事件序列（Coding2API `events.py:1-25`）：

```text
event: metadata      → event: timing_cost → event: output ×N
→ event: extra_info  → event: token_usage → event: done
```

| 事件 | data | 处理 |
| --- | --- | --- |
| `output` | `{response, reasoning_content, tool_calls}` | 三者都转发；同一帧可同时带内容与推理 |
| `token_usage` | `{prompt_tokens, completion_tokens, total_tokens, reasoning_tokens, cache_read_input_tokens}` | 缓存命中链：`prompt_tokens_details.cached_tokens` → `cached_tokens` → `cache_read_input_tokens`；真实的 `0` 保留 |
| `done` | `{finish_reason}` | 立即结束流；缺失 reason 视为 `stop` |
| `error` | `{code, message}` | 转为流内错误，按 §11 分类 |
| `metadata` / `timing_cost` / `extra_info` / `progress_notice` / 其它 | 任意 | 无下游语义，跳过，**不解析**（`progress_notice` 的 data 可能不是 JSON） |

补充：

- `data: [DONE]` 也被 dsh 接受（`trae-sse.ts:152-160`），本项目同样当作结束。
- dsh 接受**无事件名**的 `data: {...}` 帧；本项目只在载荷含
  `response` / `reasoning_content` / `tool_calls` 时按 `output` 处理，作为兼容兜底。
- 上游在元数据帧里回显真实模型名（`frame_model`）。本轮不把该名字写入响应：
  下游看到的 `model` 是请求模型，静默替换应由 M14-E 的观测/告警处理，而不是由
  适配器悄悄改写。
- 解码器支持 CRLF、跨 chunk 的 UTF-8 切分、多行 `data:`、行/帧大小上限。
- 代码：`src/providers/trae/events.rs`。

## 8. Tool Calling

- 请求：OpenAI `tools[].function.parameters` 必须字符串化（§6.2）。
- 响应：`tool_calls[]` 用 `function_call{name, arguments}`；按 `index` 分片：
  - 首片带 `name`（通常也带 `id`）；
  - 后续片只有 `arguments`，**没有 name 也必须保留**，否则客户端的参数 JSON 被截断
    （Coding2API `events.py:91-125` 的显式注释）。
- 无 name 且 arguments 为空/`{}` 的片段是噪声，丢弃。
- PolyGate 事件模型：`ToolCallStart{id,name,index}` + `ToolCallArgsDelta{id,delta}`；
  按 index 归并，缺 id 时生成 `call_<index>`，聚合后得到完整 `ToolCall`。
- 测试：`tool_calls_survive_streaming_fragments`、`chat_tools_and_choice_reach_the_upstream`、
  `tool_history_is_converted_to_solo_shape`。

## 9. 会话生命周期

- 两个参考实现都没有发现稳定的上游会话头或恢复接口；连续性靠客户端重放消息历史。
- 因此：`native_session = false`，不读取也不伪造 `ProviderContext.upstream_session_id`。
- 没有取消端点：`cancellation = false`；客户端断开时网关直接丢弃流。
- `agent_task = false`：TRAE 的 SOLO 接口不是 Agent Task API。
- 网关自己的会话粘性仍然生效，但只决定“选哪个账号”，不会向上游声明会话。

## 10. 额度查询（M14-E 已实现，见 §18.2）

协议事实：

- 端点：`POST {ug_base}/trae/api/v2/pay/ide_user_ent_usage`，空对象请求体
  （Coding2API `client.py:407-456`）。
- 响应：`user_entitlement_pack_list[]`；每包
  `entitlement_base_info.quota.credits_limit`、`usage.credits_amount`、到期时间
  `entitlement_base_info.end_time` / `expire_time`。
- 汇总：`remaining = Σ(limit - used)`，逐包保留明细与到期时间；过期/已用完的包不进入
  到期阶梯。
- 单位是 credits，**不做 token 换算**；无法解析时保持 `Unknown`（`null`），不得写 0。
- Intl 的额度接口在两个参考实现中不一致（dsh 用 pay-status，Coding2API 只有 CN）。
  M14-E 的处理：只用 `ide_user_pay_status` 报订阅计划，数值保持 `null`（§18.2）。

## 11. 签到能力（M14-E 已实现，默认不领取；见 §18.3）

- 端点（Coding2API `events.py:43-44`）：
  `POST {ug_base}/trae/api/v2/ug/checkin_credits/status`、
  `POST {ug_base}/trae/api/v2/ug/checkin_credits/claim`。
- `ug_headers`（`client.py:266-300`）要求 `X-User-Region: CN` 与一个**数字串**
  `X-Device-Id`；Coding2API 每次生成新的 16 位随机数字串
  （`credential.py:111-140`），并注明 `code:0` 对已签到账号是幂等返回，必须回查
  `status.checked_in` 与 `credits` 增量才算成功。
- **本项目的策略边界**：
  - 生成设备号用于通过上游校验属于设备指纹伪造，`AGENTS.md` §2 禁止；
  - 因此默认**不实现** TRAE 签到。若以后要做，只能在运维显式提供静态设备号并
    明确授权的前提下，作为独立显式操作；
  - claim 改变账号奖励状态，必须独立开关（沿用 WorkBuddy 的
    `checkin.claim_enabled` 模式），先查状态，再决定是否领取，禁止反复重试。
- M14-D 本轮零代码、零请求；M14-E 在不伪造设备号的前提下实现了状态查询与
  可选领取（运维提供 16 位数字设备号，见 §18.3）。

## 12. 错误码映射

### 12.1 HTTP 状态 / 信封业务码

参考：Coding2API `events.py:212-274`；dsh `trae-upstream.ts:411-420`。

| 上游 | 参考语义 | PolyGate `ErrorKind` |
| --- | --- | --- |
| `402`，或业务码 `14018` / `4008` | CREDIT（余额/请求额度耗尽） | `QuotaExhausted` |
| 业务码 `1005` | PLAN（权益/套餐不足） | `ChannelRestricted` |
| `400` / `404` + 业务码 `11102` | BLOCKED | `AccountDisabled` |
| `401` | DEAD | `AuthExpired`（见下） |
| `404` | SOFT（软限流） | `RateLimited` |
| `429` | SOFT；带 `6004` 时 MODEL | `RateLimited` / `ModelUnavailable` |
| `400` | INVALID（参数/模型不可用） | `BadRequest` |
| `408` / `425` | 超时 | `Timeout` |
| `5xx` | 上游故障 | `UpstreamServer` |
| 其它 | OTHER | `UpstreamServer` |

- 业务码从 `code` / `error.code` / `data.code` 提取（数字或数字字符串），
  非 0 才生效；HTTP 200 + 非 0 `code` 同样按错误处理。
- `401` 的取舍：参考实现把它当“凭证已死”。本项目有刷新能力（M14-E）后，
  `AuthExpired` 允许“刷新一次再重试”的既定策略，因此不用 `AuthInvalid`。
  当前 M14-D 没有刷新，过期凭据在本地就被拦下，不会发请求。
- `404 → RateLimited` 看似反直觉，但参考实现明确把它归为 SOFT，不是“模型不存在”；
  不凭 HTTP 语义覆盖源码证据。
- 错误消息脱敏并截断（512 字符），保留 `upstream_status`，**不**透传 token。

### 12.2 流内 `event: error`

| 业务码 | `ErrorKind` |
| --- | --- |
| `1005` | `ChannelRestricted` |
| `14018` / `4008` | `QuotaExhausted` |
| `6004` | `ModelUnavailable` |
| `4001` | `BadRequest` |
| 其它 / 缺失 | `UpstreamServer` |

代码：`src/providers/trae/error.rs`（7 个单元测试）。

## 13. 客户端特有依赖 / CompatibilityGap

| 缺口 | 影响 | 处理 |
| --- | --- | --- |
| 官方 IDE 头族（`X-App-Id`、版本、设备类型/品牌/OS、`Request-Traffic-Type`） | 上游可能拒绝或降级 | `client.identity=impersonate` 时发送；未实测，状态 `experimental` |
| `Trae/…` UA / Referer / timezone | 同上 | UA 随 impersonate 发送；Referer/timezone 仍不发 |
| 设备号生成（登录 hex32 / 签到 16 位数字） | 认证/签到可能无法完成 | 只透传导入值，不生成 |
| `X-Plugin-Channel` | 旧文档提到，两个参考实现的 `solo_headers` 都没有 | 不发送 |
| 上游对模型名大小写敏感（Coding2API 观测：小写名 4001） | 目录返回的名字原样使用；请求模型也原样发送 | 不做大小写“纠正” |
| 动态模型失败回退静态表 | 无真实账号时静态表不可验证 | 不回退，直接报发现失败 |
| `reasoning_effort` 映射 | dsh 把网关档位翻译成 TRAE 拼写 | 网关协议暂无该字段，M14-E 再评估；不发明 |
| 请求节流（pacer） | 参考实现按账号节流 | 由网关调度/限流层统一处理，不在适配器内重复实现 |
| 单请求积分估算（pricing） | Coding2API 用官方单价折算并标 `estimated` | 不实现：估算不是上游事实，`usage` 只保留 token |
| 会话/取消 | 上游无端点 | `native_session` / `cancellation` 保持 false |

## 14. Rust 实现结构

| 文件 | 职责 |
| --- | --- |
| `src/providers/trae/region.rs` | CN/Intl realm、路径、域与 `apiHost` 白名单 |
| `src/providers/trae/credential.rs` | 扁平/嵌套凭据、区域校验、JWT 到期回落、脱敏 |
| `src/providers/trae/error.rs` | HTTP / 信封 / 流内错误分类 |
| `src/providers/trae/http.rs` | 诚实头、模型 POST、聊天流打开、传输错误 |
| `src/providers/trae/models.rs` | `get_detail_param` 请求体与目录解析 |
| `src/providers/trae/convert.rs` | 请求体/消息/tools 改写、非流式聚合 |
| `src/providers/trae/events.rs` | 命名事件 SSE 解码与翻译 |
| `src/providers/trae/provider.rs` | `Provider` 实现、超时、断流、刷新/CAS/租约、额度 |
| `src/providers/trae/auth.rs` | 登录 URL、回调解析、`ExchangeToken`、`GetUserInfo` |
| `src/providers/trae/quota.rs` | 积分包解析、到期阶梯、Intl 订阅快照 |
| `src/providers/trae/tests.rs` | 本地 Mock HTTP 集成测试 |
| `src/admin/auth/trae.rs` | 管理台授权流（设备 ID 校验、粘贴回调、单活动流） |
| `src/checkin/trae.rs` | CN 签到执行器（状态查询、可选领取） |

能力声明：

```text
authentication      true   (本地凭据校验；有账号上下文时刷新)
model_discovery     true
quota               true   (CN credits / Intl 订阅，数值未知保持 null)
chat / streaming    true
tools               true
native_session      false
agent_task          false
cancellation        false
```

运行时：`ProviderKind::Trae` 在 `build_registry` 中注册，并注入
`AccountCredentialStore` 与 `checkin.device_id`；管理台提供 TRAE 授权流
（`/admin/api/auth/trae/*`，粘贴回调链接完成）。`base_url` 可覆盖（仅测试）。

## 15. 与参考实现的差异（汇总）

| 差异 | 理由 |
| --- | --- |
| 孤儿 tool 结果 / 无 name 的 tool call 报 `BadRequest` | 历史完整性；不静默改写调用方历史 |
| 不回退静态模型表 | 静态表包含未验证/内部模型，Mock 结果不能当真实目录 |
| 不发送官方客户端头与设备号 | 合规红线 |
| 不支持未观测的采样字段 | 不臆造信封 |
| 过期凭据本地拦截 | M14-D 没有刷新；发出去只会得到 401 |
| 不估算积分 | 只上报上游真实 usage |
| `401 → AuthExpired`（参考为 DEAD） | 401 后强制刷新一次再重试；AuthExpired 区分于 AuthInvalid |
| 不做 pacer / 提示词改写 / 渠道规避 | 属于风险控制绕过或内容改写，明确不迁移 |
| 不生成 machine/device id | 设备指纹伪造红线；改为运维显式提供并随凭据持久化 |
| `userJwt` 回退优先 `RefreshToken` 再 `Token` | 与参考 `callback.py:117-122` 一致；`Token` 不能当刷新令牌优先使用 |
| 额度只用 `ide_user_ent_usage`（CN） | Coding2API 主路径；dsh 的 `usage_summary` 仅在响应自带时采用 |
| Intl 额度只报订阅计划 | 只有 `ide_user_pay_status` 证据；数字一律 `null`，不按 CN 猜测 |
| 签到设备号只接受 16 位数字且由运维提供 | 与登录 hex32 设备号分离；缺失时 `Unsupported`，不发请求 |

## 16. M14-E 结项对照

| 待办 | 状态 |
| --- | --- |
| `ExchangeToken` 刷新（ClientID、轮换、CAS、租约、401 单次重试） | 完成，MockVerified |
| 额度（CN 积分包、到期阶梯、Unknown 保持 null） | 完成，MockVerified |
| Intl 额度 | 完成“订阅计划 + 数值未知”，MockVerified |
| 签到 | 完成状态查询与可选领取；默认不领取，MockVerified |
| 管理台授权入口 | 完成（运维提供 hex32 设备 ID，粘贴回调链接） |
| 真实账号验证 | **未执行**：无 TRAE 账号，所有能力保持 `experimental` |

## 17. M14-D 验证记录（2026-10-29）

```text
$ cargo fmt --check
（无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile ... (0 warnings)
$ cargo test
test result: ok. 552 passed; 0 failed; 0 ignored; ...
$ cargo build
Finished `dev` profile ...
```

- 新增 63 个测试（489 → 552）：TRAE 协议层 62 个 + runtime 注册 1 个。
- 全部离线：`test_support::spawn_server` 只监听 `127.0.0.1`，无真实网络、无真实凭据。
- 没有对 TRAE 上游发出任何请求；没有真实账号。

## 18. M14-E 实现与验证（2026-10-29）

### 18.1 认证生命周期与刷新

| 文件 | 内容 |
| --- | --- |
| `src/providers/trae/auth.rs` | 登录 URL 构造、回调解析、`ExchangeToken`、`GetUserInfo` |
| `src/admin/auth/trae.rs` | 管理台授权流：校验设备 ID、加密保存、单活动流、绑定回调 |
| `src/admin/api/mod.rs`、`src/admin/mod.rs` | `/admin/api/auth/trae/start`、`{flow_id}`、`{flow_id}/complete`、`{flow_id}/cancel` |
| `src/providers/trae/provider.rs` | 刷新、CAS、跨进程租约、401 单次重试、`auth_status_with_context` |
| `src/admin/account/mod.rs` | `persist_trae_login`：外部身份 `trae:<realm>:<uid>`，元数据不含 token |

- 登录 URL：`https://www.trae.cn/authorization`，携带
  `client_id=en1oxy7wnw8j9n`、`auth_callback_url`、`login_trace_id`
  （`machine_id + device_id` 的后 16 位）与运维提供的 `machine_id` / `device_id`。
- **设备身份策略**：PolyGate **不生成** machine/device id。`start` 要求两者均为
  32 位十六进制，否则 `BadRequest`；Intl 登录返回 `Unsupported`（没有可信 OAuth 主机）。
  设备值随授权流加密（AAD 绑定 flow id）保存，授权成功后写入凭据。
- 回调：`/oauth/callback` 收到 `refreshToken` 时**不**完成授权、不回显 token，
  只渲染“请把地址栏链接粘贴回面板”的页面；面板用已认证的
  `POST /admin/api/auth/trae/{flow_id}/complete` 提交完整 URL。
  `loginTraceID` 与流绑定的 trace 不一致时保持 pending，便于重贴正确链接。
- 刷新：`ExchangeToken`（与首次换取同一接口）。默认提前 300 秒刷新；使用
  `CredentialStore` 的跨进程租约（60 秒）保证单写者；写回使用 revision CAS。
  CAS 冲突时重新加载，若已是新凭据直接使用，**绝不**用已轮换的 refresh token
  再刷一次；写失败为 `Storage`，可重试；刷新被 401/403 拒绝映射为 `AuthInvalid`。
- 聊天/额度遇到 `AuthExpired` 时强制刷新一次并重试；第二次失败即终止。

### 18.2 额度

- CN：`POST {pay_base}/trae/api/v2/pay/ide_user_ent_usage`，请求体 `{}`；
  解析 `user_entitlement_pack_list[]`，汇总 `Σlimit` 与 `Σremaining`，逐包保留
  金额与到期时间，`raw.period = per_package`，并生成只含未过期且有余额包的
  `expiry_ladder`。响应自带 `usage_summary` 时以其为准。
- Intl：`POST {pay_base}/trae/api/v1/pay/ide_user_pay_status`，只给出订阅计划
  （`user_pay_identity_str` / `trial_status.is_in_trial`）；
  `total`/`remaining`/`unit` 全部保持 `null`/`Unknown`，`raw.period = subscription`。
- 单位保持 TRAE credits，不做 token 换算；无包时 `Unknown`，从不写 0。
- 配置 `checkin.device_id`（16 位数字）作为 `X-Device-Id` 透传，仅当运维提供。

### 18.3 签到（CN，默认不领取）

- `src/checkin/trae.rs`：先查
  `POST {pay_base}/trae/api/v2/ug/checkin_credits/status`；仅当
  `checkin.claim_enabled = true` 且状态可签到才调用 `claim`。
- 设备号必须是运维提供的 16 位数字；缺失或非法时返回 `Unsupported`/`BadRequest`
  并且**不发请求**。Intl 直接 `Unsupported`，零请求。
- `code:0` 不代表成功：必须回查 `checked_in` 且 credits 增加才判成功；
  `9095` 记为已领取、`9074` 记为设备/参与上限（不重试）。
- 同一账号/自然日只执行一次（由 `CheckinService` 去重），失败不循环领取。

### 18.4 管理台与 UI

- Web UI 启用 TRAE 子导航：授权弹窗要求 machineId/deviceId，启动后显示回调链接
  输入框与“完成授权”按钮；Intl 站点在登录弹窗中被禁用（凭据仍可导入并刷新）。
- 签到按钮对 TRAE/WorkBuddy 的 Intl 账号隐藏；CN 账号可执行。
- 账号详情展示昵称、授权来源与外部身份；不展示任何 token。

### 18.5 测试与验证

```text
$ cargo fmt --check
（无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile ... (0 warnings)
$ cargo test --lib
test result: ok. 597 passed; 0 failed; 0 ignored; ...
$ cargo build
Finished `dev` profile ...
```

- 新增 45 个测试（552 → 597）：管理台授权 3、Provider 刷新/额度 11、
  auth 8、quota 7、checkin 11、admin flow 2、credential 3。
- 全部离线；Mock 只监听 `127.0.0.1`。没有对 TRAE 上游发出任何请求，没有真实账号。
- 管理台浏览器 smoke：初始化 → TRAE 弹窗（Intl 禁用、设备字段）→ 非法 hex 拦截 →
  合法 hex 创建流程 → 粘贴区渲染与空提交提示 → 公开回调页不回显 token、不落库。
  未执行真实授权（没有账号，未打开上游登录页）。
- 状态：**MockVerified**。额度/签到/刷新均未在真实账号上验证；
  `experimental` 标记保留。

### 18.6 已知边界

- Intl 登录与签到未实现（无证据），仅支持凭据导入后经 `apiHost` 刷新。
- 上游是否接受“诚实非 IDE 头 + 运维设备号”仍未知；被拒时记录 CompatibilityGap，
  不通过伪造头解决。
- 额度端点的两份参考不一致：以 Coding2API 为准，dsh 字段只在响应存在时采用。
