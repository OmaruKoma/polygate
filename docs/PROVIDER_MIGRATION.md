# Provider 迁移清单（M10 源码审计）

> **2026-10-29：CodeBuddy 已从本项目范围移除。** 本文中 CodeBuddy/`codebuddy_intl`
> 相关内容仅作历史协议研究，不构成实现计划；代码与 UI 中不得再注册该 Provider。


> 审计对象：`https://github.com/RobbsLuo/Coding2API`（主要参考，本地 `.refs/RobbsLuo_Coding2API`），
> 以及 `.refs/chengzhi-c_dsh-wtq-pool`、`.refs/Johnsheng1_codearts2api`、`.refs/liu5269_zcode2api`。
>
> 本文只记录**从源码读到的事实**与迁移判断，不代表 PolyGate 已实现、已联调或已验证。
> 所有“实测”均指参考仓库作者在源码注释中记录的观测；本机没有用真实凭据复核。

## 0. 证据等级与状态口径

| 标记 | 含义 |
| --- | --- |
| `源码+单测` | 参考仓库有实现，且 `tests/` 下有对应单测/夹具；本机未接真实上游 |
| `源码` | 参考仓库有实现，未找到对应单测或只覆盖部分分支 |
| `作者实测` | 源码注释写明真实账号/真实流量观测日期；属于作者证据，不是本项目证据 |
| `活动不可用` | 端点存在或曾存在，但当前活动未开放、区域未开放或渠道显式关闭 |
| `不支持` | 参考源码没有该能力 |
| `已排除` | 本项目明确不接入 |

PolyGate 侧的验证状态仍遵循 `docs/PROVIDERS.md` §0：没有真实账号端到端验证，
一律不得标 `verified`。

## 1. 总表

Coding2API 的 Provider 注册表在 `src/main.py` 约 431–470 行，共 8 个 key：
`trae`、`codebuddy`、`codebuddy_intl`、`zen`、`kilo`、`qoder`、`codearts`、`officeace`。
其中 `zen` 按本项目要求**永不作为上游**；`kilo` 是无凭证免费渠道，是否纳入后续再定。

| # | Provider / id | 国内/国际 | 认证 | 模型接口 | 推理 / Agent 接口 | 流式 | Tool Calling | 会话机制 | 额度查询 | 签到 | 源码位置 | 参考实现状态 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | CodeArts / `codearts` | CN（`snap-access.cn-north-4`） | STS 临时 AK/SK + `X-Security-Token` + `SDK-HMAC-SHA256`；一次性 refresh_token + P-256 DPoP | `GET /v1/model/builtin`（`Agent-Type: PromptCenter`） | `POST /api/v2/chat/completions`（旧 `/v1/chat/chat`） | 是（自有 SSE，`text` 为累计全文） | 是 | 无原生恢复；由 messages 摘要派生 32 位 `chat_id` 做 prompt-cache 亲和 | `GET /snap-manager/v1/statistics/plugin`（积分 + 到期阶梯） | `/v1/ops/delivery` → `/v1/ops/claim` → `/v1/ops/confirm` | `src/provider/codearts/client.py`；门面 `codearts/__init__.py` | 源码+单测；作者实测 |
| 2 | Qoder / `qoder` | CN / Intl（登录时显式选择；禁止自动推断/跨区） | COSY 私有签名；openapi 业务端点纯 Bearer；refresh_token | `GET /algo/api/v2/model/list?Encode=1`（必须 COSY 头） | `POST /algo/api/v2/service/pro/sse/agent_chat_generation` | 是（信封式 SSE） | 是（`tool_calls` + 旧式 `function_call`） | 无原生恢复；`agent_id=agent_common`，消息/工具透传 | `GET /api/v2/quota/usage`（旧文档另有 `/sash/api/v2/me/usage`） | `/sash/api/v1/me/campaigns` + `/{id}/claim`；回退旧 `daily-check-in/{status,claim}` | `src/provider/qoder/client.py`；门面 `qoder/__init__.py`；M13-D1 详见 `docs/QODER_PROVIDER_RESEARCH.md` | 源码+单测；作者实测；“活动制”已切换；PolyGate 未实测 |
| 3 | TRAE / `trae` | CN / Intl（按凭证 `api_host`） | `Cloud-IDE-JWT` + `X-Ide-Token` / `X-Cloudide-Token` + 设备头族；refreshToken 换 accessToken | `POST /api/ide/v1/get_detail_param` | `POST /api/agent/v3/llm_utils_chat`（`function=solo_work_lite`） | 是（命名事件） | 是（`function_call` 归一） | 无原生会话头；连续性靠客户端消息历史 | CN `ug_host` 的额度/签到接口；Intl 仅 pay-status，未统一 | `/trae/api/v2/ug/checkin_credits/status` + `/claim` | `src/provider/trae/client.py`、`events.py`、`credential.py` | 源码+单测；作者实测；Intl 签到未单独实现 |
| 4 | CodeBuddy CN / `codebuddy` | CN（`copilot.tencent.com`） | Bearer + `X-User-Id` / 企业/域头；OAuth 或手动 bearer | `GET /v3/config` | `POST /v2/chat/completions` | 是 | 是（含脏 tool_call 清理） | 会话链路头 `X-Conversation-*` 类；无原生恢复 | `GET /v2/billing/meter/get-user-resource`（企业版另有接口） | `/billing/meter/daily-checkin` + `/billing/meter/checkin-activity-status` | `src/provider/codebuddy/client.py`、`headers.py`、`checkin.py` | 源码+单测；作者实测 |
| 5 | CodeBuddy Intl / `codebuddy_intl` | Intl（`www.codebuddy.ai`） | 同 CN 协议与 Keycloak realm；首条必须是 system | `GET /v3/config`（按套餐过滤） | `POST /v2/chat/completions` | 是 | 是 | 同 CN | 同 CN | **显式关闭**（`checkin = None`） | `src/provider/codebuddy/intl.py` | 签到活动不可用 |
| 6 | OfficeAce / `officeace` | CN（华为云 AgentArts / officeace 域） | 聊天 Basic `app_key:app_secret`；控制面临时 AK/SK + `SDK-HMAC-SHA256` / `V11-HMAC-SHA256` | `GET /v2/models` | `POST /v2/chat/completions`（标准 OpenAI） | 是（伪 200 错误帧） | 是 | 无原生恢复 | `/v1/subscription`、`/v1/subscription/usage`（积分主账） | `/v1/subscription/bonus/claim`；状态来自 `/v1/subscription` 的 `daily` | `src/provider/officeace/client.py`、`events.py` | 源码+单测；作者实测 |
| 7 | Kilo / `kilo` | 国际 | 匿名（免费模型无需 Authorization） | `GET /api/gateway/models`（`isFree`） | `POST /api/gateway/chat/completions` | 是 | 是（标准 OpenAI） | 无 | 无额度接口 → `unknown` | 不支持 | `src/provider/kilo/client.py` | 源码；无凭证渠道 |
| 8 | OpenCode Zen / `zen` | 国际 | 免费层门禁伪装（UA/session/tools） | `/zen/v1/models` | `/zen/v1/chat/completions` | 是 | 是（注入 bash/read） | `x-opencode-session` 伪造 | — | 不支持 | `src/provider/zen/client.py` | **已排除** |

补充：Coding2API 没有独立的 WorkBuddy Provider。`docs/PROVIDERS.md` 中的 WorkBuddy
与 CodeBuddy 是同一腾讯上游的不同产品/区域分支；迁移时不要把 CodeBuddy 再拆成
第三个独立上游。OfficeAce 与 Kilo 在本项目现有 `ProviderKind` 中还没有枚举值。

**Agent 接口**：Coding2API 的 8 个 Provider 都没有独立的 Agent Task API；上表
“推理 / Agent 接口”列只列推理端点。OfficeAce/Qoder 等虽然名字里带 “agent”，
实际仍是 `chat/completions` 形状的流式推理，不能声明 `Capability::AgentTask`。

## 2. 逐 Provider 事实与迁移动作

### 2.1 CodeArts（最高优先级）

**认证与生命周期**

- `src/provider/codearts/__init__.py` 顶部说明：鉴权不是 bearer，是华为云
  `SDK-HMAC-SHA256`（AK/SK + STS security token）；refresh_token 一次性且与
  `client_id` + DPoP 私钥绑定，只能由 `RefreshTask` 独占轮转，额度探测不得顺手刷新。
- `src/provider/codearts/client.py:322` `fetch_checkin_status` 明确 `/v1/ops/*`
  不带 `snap-manager` 前缀；`:335` `claim_daily_credit` 把 claim + confirm 捆在一起；
  `:362` `_confirm_daily_credit`；`:380` `checkin` 是四态状态机。
- 迁移动作：PolyGate 需要新增 `AuthStatus`/凭据轮转之外的 **refresh 所有权**约束；
  `Provider` trait 目前没有 refresh 钩子（见 §4），M11/M12 要补。

**模型与推理**

- 模型：`GET {snap}/v1/model/builtin`，头 `Agent-Type: PromptCenter`。
- 推理：`POST {snap}/api/v2/chat/completions`，OpenAI 形状；旧 `/v1/chat/chat`
  是 `task=chat` 的自定义形状，仅作兼容。
- SSE 不是标准空行分隔，`text` 是累计全文，解析层必须做差量替换
  （`codearts/events.py` 的 `TextSnapshot`）。
- Tool calling：`codearts/events.py:263` 读取 `delta.tool_calls`，按增量合并。

**会话**

- 没有可恢复的原生会话；`client.py:71` `new_chat_id` / `:76` `derive_chat_id`
  用 messages 的稳定摘要生成 32 位 hex `chat_id`，目的是 prompt cache 亲和，
  不是会话恢复。PolyGate 的 `Capability::NativeSession` 对 CodeArts 应保持 false。

**额度**

- `GET {snap}/snap-manager/v1/statistics/plugin`，单位是积分；积分包带独立到期日，
  登记为 `expiry_ladder` 参与“快过期先用”。
- `docs/PROVIDERS.md` 记录两种参考实现的单位不一致（积分 vs token）；迁移时必须
  保留“单位未知”路径，不能把 token 当积分。

**签到**

见 `docs/CHECKIN_RESEARCH.md` §4.1。要点：`ELIGIBLE`→claim+confirm；
`CLAIMED`→只补 confirm；`CONFIRMED/CONSUMED`→已签；活动不存在/已结束→`ok=True`，
避免后台每 10 分钟刷失败。

**迁移判断**：第一个真实 Provider 应选 CodeArts。理由是本地 Go 参考实现最完整、
签名/刷新/额度/签到都有清晰状态机；风险是 DPoP 与一次性 refresh_token 的并发消费。

**M13-B 实现状态（2026-10-29）**

- `src/providers/codearts/` 已实现 `Provider`：`auth_status`/`list_models`/`quota`/
  `chat`/`chat_stream`；`chat_with_context` 接收会话绑定给出的上游 `chat_id`。
- 签名按参考实现逐项对齐（尾 `/` canonical URI、全头签名、`X-Sdk-Content-Sha256`）；
  流式解析同时支持 v2 `delta` chunk 与 legacy `text` 累计快照。
- 未实现旧 `/v1/chat/chat`（`task=chat`）路径：没有官方客户端证据表明仍在使用。
- 能力位：`authentication/model_discovery/quota/chat/streaming/tools = true`；
  `native_session = false`；`agent_task`/`cancellation = false`。
- refresh 由 Provider 内按账号串行锁 + `CredentialStore` revision CAS 完成；没有独立
  RefreshTask，也没有把 refresh 结果写回外部账号池。
- 签到执行器在 `src/checkin/codearts.rs`，走 `/v1/ops/*`；与本地 Go 工程的
  `/api/v1/benefit/claim`（Johnsheng1）**不混用**。
- **未验证**：以上全部只在 mock server + 脱敏 fixture 上通过；没有真实账号，verification
  保持 `experimental`。详见 `PROGRESS.md` §23。

### 2.2 Qoder

> M13-D1 已完成完整研究，见 `docs/QODER_PROVIDER_RESEARCH.md`。下文只保留
> 供迁移矩阵使用的摘要；证据等级以该文档为准（Qoder 目前无 `Live Verified`）。
> M13-D2-A 已实现离线协议基础层（`src/providers/qoder/`：region/COSY/错误/
> 转换/SSE），**未实现 Provider、未注册、未联网**。
> M13-D2-B 已实现设备码轮询、加密流程秘密、账号落库、单写者刷新与管理台流程
> （`src/providers/qoder/auth.rs`、`credential.rs`、`src/admin/auth/qoder.rs`）；
> M13-D2-C 注册了 `qoder-cn` Provider（认证/额度/模型/Chat/SSE/工具）与签到执行器
> （`provider.rs`、`http.rs`、`gateway.rs`、`quota.rs`、`cosy.rs`、`src/checkin/qoder.rs`）。
> 真实 CN 账号已验证授权、userinfo、额度、签到（含一次领取）、11 个模型发现、
> 非流式与流式 Chat、工具调用与多轮工具结果。COSY 签名以真实网关为准：
> 参考实现的 1.1.64 身份/信封形状被 403 `Signature invalid` 拒绝，现采用官方客户端
> 的固定顺序身份与 payload，见 `PROGRESS.md` §31.2。刷新与 Intl 仍未验证。

**区域（CN / Intl 独立服务面）**

| 项 | CN | Intl |
| --- | --- | --- |
| openapi | `openapi.qoder.com.cn` | `openapi.qoder.sh` |
| gateway | `gateway.qoder.com.cn` | `api1.qoder.sh`（回退 api2/api3） |
| website | `qoder.cn` | `qoder.com` |
| domain / UA | `qoder.com.cn` / `QoderWork/1.1.64` | `qoder.com` / `Qoder/1.1.64` |
| 授权 nonce | 带横线 UUID | 无横线 hex UUID |

来源：`.refs/RobbsLuo_Coding2API/src/provider/qoder/events.py:71`（`REALM_CONFIGS`）。
CN/Intl 的账号、凭据、额度、签到不得混用或跨区回退；`detect_realm_from_domain`
（`events.py:172`）只用于导入提示，登录流程必须显式选 region。

**认证**

- 设备码 + PKCE(S256) 轮询：授权 URL 为
  `{website}/device/selectAccounts?challenge=&challenge_method=S256&nonce=&client_id=&machine_id=`，
  **不带 `redirect_uri`**（`auth.py:100`）。
- 轮询 `GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256`；
  `202/404` 表示等待，`200` 且带 `token`/`device_token` 表示成功（`auth.py:204`、`:227`）。
- 成功后 `GET /api/v1/userinfo` 补 uid/昵称/组织；状态 TTL 600s，单所有者、成功即消费
  （`auth.py:46`、`:132`）。
- 刷新 `POST /api/v1/deviceToken/refresh`，body 仅 `{"refresh_token"}`，不带
  Authorization（`client.py:577`）；到期优先 `expires_in`（毫秒）、其次
  `expires_at`(RFC3339)、最后 JWT `exp`，都不可得保持未知，不编造 TTL
  （`credential.py:156`）。刷新必须单写者 + revision CAS。

**COSY 签名**

- `Authorization: Bearer COSY.<payload_b64>.<md5>`；签名串
  `md5(payload_b64 \n cosy_key \n date \n body \n path)`，path 去掉 `/algo`
  前缀、query 不参与（`cosy.py:177`、`:185`）。
- `info` 为身份 JSON 的 AES-128-CBC 密文（key == iv），`cosy_key` 为
  RSA-PKCS1v15 包裹的 16 字节会话密钥；服务端公钥硬编码在 `cosy.py:133`。
- 头族包括 `cosy-clienttype: 5`、`cosy-date`、`cosy-user`、`cosy-key`、
  `cosy-version: 1.1.64`、`cosy-machineid/machinetoken`、`login-version: v2`、
  `x-model-key`/`x-model-source: system`（`cosy.py:296`）。
  **合规风险**：这些是官方客户端身份字段，`AGENTS.md` §2 禁止客户端身份冒充；
  M13-D2 开工前必须先确认口径（见研究文档 §6.1、§12.5）。

**模型与推理**

- 模型：`GET /algo/api/v2/model/list?Encode=1`，必须带整套 COSY 头，签名 body 为
  `qoder_encode("")`，方法固定 GET（`events.py:136`、`:138`）。字段
  `key/display_name/price_factor/max_input_tokens/is_vl/supportsToolCall/is_reasoning`
  （`events.py:467`）。
- 推理：`POST /algo/api/v2/service/pro/sse/agent_chat_generation`；请求体
  `build_chat_body`（`client.py:201`）保留 messages/tools/参数，加
  `agent_id=agent_common`、`stream=true`；清理孤立 tool 消息、把 `content:null`
  归一为空串（否则上游 400 tool 配对错误）。
- 上游只支持流式；PolyGate 的非流式 `chat` 由网关聚合 SSE（设计决定）。
- SSE 是信封式：`data:{"statusCodeValue":...,"body":"<内层 chunk>",...}`；
  保活帧内层为 `"null"`，`[DONE]` 结束（`events.py:295`、`:331`、`:404`）。
- Tool calling：`events.py:391` `_tool_calls` 同时兼容 `tool_calls` 与旧式
  `function_call`；多轮必须保持 OpenAI `type:"function"` 包装（与 CodeArts
  经验一致）。

**错误分类**

`events.py:207` `classify_error_code` 对 HTTP 与流内信封统一判据：
402/日计费次数用尽 → 额度；排队 → 限流；401 或 403+会话失效 → 凭据；
403 其他/418/5xx → 上游瞬时；429 → 模型级限流；400/404/422 → 请求/模型无效
（节点故障 `[FAIL]node:` 例外，按模型瞬时处理）。映射到 PolyGate `ErrorKind`
的完整表见研究文档 §6.5。

**额度**：`GET /api/v2/quota/usage`（`events.py:109`），解析 `userQuota` +
`addOnQuota` 的 `remaining/total/used` 与 `expiresAt`；单位为 credits。
字段缺失时 PolyGate 必须返回 `Unknown/Partial`（`remaining/total = null`），
不能按参考实现的缺省 0 处理。`/api/v2/user/plan` 只有常量、未解析，Unverified。

**签到**：`client.py:435` `checkin` 优先活动制 `/sash/api/v1/me/campaigns`
（必须 `Cosy-ClientType: 10`，否则静默空列表），只筛 `actionType=CLAIM_BENEFIT`；
404/405/410 才回退旧 `daily-check-in`。claim 的 `replayed:true` 与
`BLOCKED/SAME_PERSON_ALREADY_CLAIMED` 都归一为“今日已签”；明确 `BLOCKED`
才是失败。窗口按 UTC+8 10:00 轮换，`seal_until` 取 `endAt` 最大值
（`events.py:587`），封到下一轮开放而不是自然日 00:00。

PolyGate 映射注意：`CheckinOutcome` 没有 `Unavailable`，而
`CheckinStatus::Failed` 允许自动重试；因此“本区域无签到接口”必须映射为
`CheckinOutcome::Unsupported`（settled、不重试），不能映射为 `Failed`。
claim 默认关闭，只能先查状态，claim 需单独显式授权。

**会话**：无 upstream session id、无恢复、无任务 API、无取消 API；
`NativeSession`/`AgentTask`/`Cancellation` 全部 false。COSY 会话缓存只是
签名材料，不是会话。下游粘性只能绑账号，不得伪造上游 session。

**迁移判断**：COSY 是最大风险，必须先用固定测试向量验证 signer；D2 顺序见
研究文档 §12.4（政策确认 → signer → 认证/管理台 → 模型/额度 → Chat/SSE/tools
→ 会话/并发 → 签到 → 真实验收）。真实能力逐项验证前保持 `Experimental`。

### 2.3 TRAE

**认证与区域**

- `src/provider/trae/client.py:630` `TraeProvider`，id `trae`；`:645` 导入时要求
  `uid`，并把 `api_host` 限制在 `ALLOWED_OAUTH_HOSTS`（`trae/events.py` 含
  `https://api.trae.cn`、`https://api.trae.com.cn`）。
- 认证头族：`Cloud-IDE-JWT` + `X-Ide-Token` / `X-Cloudide-Token` + 设备/版本头
  （后两类属 IDE 身份，本项目不迁移，见 `TRAE_MIGRATION.md` §13）；
  回调登录只给 refreshToken，必须 ExchangeToken 换 accessToken。

**模型与推理**

- 模型：`POST /api/ide/v1/get_detail_param`；动态拉取失败有 5 分钟负缓存 + 静态表兜底。
- 推理：`POST /api/agent/v3/llm_utils_chat`；`prepare_body`（`client.py:84`）把
  `messages.content` 字符串转内容块数组、`developer`→`system`、`model` 同时写
  `config_name`、`function=solo_work_lite`、强制 `stream=true`。
- SSE 使用命名事件；有下游语义的是 `output` / `token_usage` / `done` / `error`，
  排队/进度/metadata 等帧被跳过（`events.py:26` `KNOWN_EVENT_NAMES`）。TRAE 的
  `function_call` 需要归一成 OpenAI `tool_calls`（`events.py:126` `_named_tool_calls`）。
- 没有稳定原生会话头；`Capability::NativeSession` 必须 false。

**额度**：`fetch_quota` 走 `ug_host` 的 pay/usage 接口；Intl 只有 pay-status，
CN 有积分与签到。额度字段不统一，未知必须保持 `unknown`。

**签到**：`client.py:743` `checkin_scope`（`trae|uid`）；`:747` `checkin` 的纪律：
claim 返回 `code:0` **不能**当成功，必须回查 `status.credits` 有增量且
`checked_in=true`；9074 按限流软失败，一轮内换新设备号重试，
`CHECKIN_ATTEMPTS` 次后返回失败。设备号由 `credential.py:122`
`new_checkin_device_id()` 生成 16 位数字。

**迁移判断**：TRAE 的签到实现最“防伪”，但也最依赖设备号。按本项目合规红线，
“生成新设备号”不能被当作规避平台限制的手段；迁移前必须确认该设备号是上游
正常客户端也会生成的请求参数，还是反滥用标识。若无法确认，签到应保持关闭。

### 2.4 CodeBuddy CN

**认证/协议**：`src/provider/codebuddy/headers.py` 定义 CN
`https://copilot.tencent.com`、Intl `https://www.codebuddy.ai`；聊天
`POST /v2/chat/completions`；额度 `GET /v2/billing/meter/get-user-resource`；
签到 `/billing/meter/daily-checkin`、状态 `/billing/meter/checkin-activity-status`。
`X-Domain` 必须由同一个 endpoint 派生；`X-Department-Info` 要 UTF-8 百分号编码。

**请求改写**：`client.py` 会清理历史里空名的脏 `tool_call`（`:112` 附近），
并对渠道指纹做脱敏；改写只作用于副本，不能污染 PolyGate 的粘性指纹原文。

**签到**：`client.py:814` `checkin` 调用 `codebuddy/checkin.py` 的 `claim`；
`parse_checkin_response`（`checkin.py:46`）只有 `code==0` 且 `data.credit` 是
有限非布尔数值才算成功；`code` 非 0 但 message 含“已签到”算 `already_checked_in`。
`checkin_scope_key`（`:112`）是 `endpoint|user_id`，身份未知返回空串，
调用方回落到 `credential_id`，避免所有未知账号算成同一 scope。

**迁移判断**：CodeBuddy 的签到协议最简单，适合作为通用签到框架的第二个接入点；
但要注意 CN/Intl 端点派生与 `checkin = None` 的 Intl 差异。

### 2.5 CodeBuddy Intl

`src/provider/codebuddy/intl.py:83` 明确：

```python
checkin = None
checkin_scope = None
checkin_status = None
```

文件顶部说明：国际站网关把 `/v2/activity/*` 挂在认证插件后面，401 不能证明子路由
存在；签到/成长中心全部关闭，避免在未验证端点盲跑不可逆动作。迁移时 PolyGate
必须保留“接口存在但活动不可用”的状态，不能因为 CN 有签到就推断 Intl 也有。

### 2.6 OfficeAce

**认证**：聊天是标准 OpenAI + Basic `app_key:app_secret`（这对凭据不过期）；
控制面用临时 AK/SK + 华为云签名。把 STS 当 Basic 会得到 `APIG.0303 app not found`
（`client.py` 顶部注释，作者实测）。

**协议**：`/v2/chat/completions` 标准 OpenAI；限流会回 HTTP 200 +
`data:{"error":...}`，且可能首帧正常、第二帧才是错误，所以解析层必须把错误帧
照常产出。`81004`/`81009` 是模型级拒绝（403/404），不能按状态码判 DEAD。

**额度与签到**：积分是主账（`/v1/subscription`、`/v1/subscription/usage`）；
签到 `POST /v1/subscription/bonus/claim`（`client.py:290`），请求体必须是空 JSON
对象且带 `Content-Type: application/json`（裸 POST 会 500 `OfficeAce.11020001`）。
“领到多少”按签到前后剩余量的增量算，`points` 是活动累计发放量，不能当本次收获。
`daily.expired_time` 是当日 23:59:59（UTC+8），`seal_until` 封到北京时间次日 0 点。

**迁移判断**：OfficeAce 是纯 OpenAI 上游，适配成本低；但它是华为云另一套账号体系，
与 CodeArts 凭证不通用，`ProviderKind` 需要新增枚举。签到属于“领当日奖励积分”，
按本项目默认不开启权益领取，需配置显式开启。

### 2.7 Kilo

`src/provider/kilo/client.py`：`https://api.kilo.ai/api/gateway`，匿名即可；
`/models` 每个条目带 `isFree`，免费集由上游标记决定，不做探活；429/502/503/504
都按模型级冷却。无额度接口，`probe_quota` 返回 `Quota(probe_failed=True)`，
健康度保持 unknown。无签到。

**迁移判断**：若纳入，只实现 chat/models/streaming/tools；`Quota` 必须 unknown；
`Authentication` 能力为 false 或无凭证占位。不要把免费模型做成“无限额度”。

### 2.8 OpenCode Zen（明确排除）

`src/provider/zen/client.py` 的免费层依赖 UA 版本、`x-opencode-session` 时间编码、
body 必须 `stream=true`、必须注入 `bash`/`read` 工具。这属于客户端门禁伪装，
且本项目已明确排除 Zen。**不迁移任何代码、端点或门禁规则。**

## 3. 参考仓库覆盖差异

| 仓库 | 覆盖的 Provider | 与 Coding2API 的差异 |
| --- | --- | --- |
| Coding2API | 8 个（含 Zen） | 功能最全：签到、批次、定时、额度、调度、管理台 |
| dsh-wtq-pool | WorkBuddy/CodeBuddy、Qoder、TRAE | TypeScript 插件；签到槽位持久化、Qoder 10:00 重试、按账号节流更细；无 CodeArts/OfficeAce |
| Johnsheng1/codearts2api | CodeArts | Python 单文件；签到是独立脚本 `benefit.py`，走 `GET/POST /api/v1/benefit/claim`（opengw），与 Coding2API 的 `/v1/ops/*` 不同 |
| 本地 `codearts2api`（Go） | CodeArts | 未发现签到代码；操作者本人作品，代码可复用（作者授权）；可作为账号池/调度参考（见 `docs/RESEARCH.md` §10） |
| liu5269/zcode2api | ZCode（**已明确排除**） | AGPL-3.0；带验证码求解（Node/jsdom）与 `X-Aliyun-Captcha-Verify-Param`；**无签到**；**不迁移任何功能**，仅保留历史协议事实 |

CodeArts 签到存在两套上游协议（`/v1/ops/delivery|claim|confirm` 与
`/api/v1/benefit/claim`）。它们可能对应不同产品/套餐；迁移时必须以真实账号
探测结果为准，不能把两套端点混用。

## 4. 与 PolyGate `Provider` 契约的差距

现状（M6/M9）：

- `src/providers/mod.rs:87` `Capability` 只有
  `Authentication`、`ModelDiscovery`、`Quota`、`Chat`、`Streaming`、
  `ToolCalling`、`NativeSession`、`AgentTask`、`Cancellation`。
- `src/providers/mod.rs:442` `Provider` trait 只有
  `info`/`capabilities`/`auth_status`/`list_models`/`quota`/`chat`/`chat_stream`/
  `agent_task`/`cancel`；**没有** refresh、check-in、growth、activity、模型目录刷新。
- `src/config/mod.rs:220` `ProviderKind` 只有 Codearts/Qoder/Trae/Workbuddy/
  Codebuddy/Zcode/Mock；**没有 OfficeAce、Kilo**。

迁移需要（M11/M12 规划，不在 M10 实现）：

1. 新增 `Capability::Checkin`、`Capability::QuotaRefresh`（或把 refresh 放
   `Authentication` 子能力），默认 false。
2. 新增中立类型：`CheckinOutcome`（`done` / `already` / `unavailable` /
   `failed` / `unsupported`，带 `credit`、`message`、`seal_until`、`activity`），
   不能只用 `Result<()>`，否则“活动未开放”会被当成失败每轮重试。
3. 新增 `Provider::checkin` / `checkin_status` 默认返回 `Unsupported`；
   不要让任务层用 `getattr` 探测 Python 式能力。
4. `ProviderKind` 增补 OfficeAce/Kilo（若纳入），并保持 CN/Intl 同 kind 不同 region。
5. 为 CodeArts/Qoder/OfficeAce 的“活动制/区域不可用”保留明确状态，不能折叠成
   `ok=false` 后无限重试。

## 5. 迁移顺序建议

按协议风险与可验证性：

1. **CodeArts**：先打通签名、refresh 所有权、模型、额度、聊天、错误分类。
2. **CodeBuddy CN**：OpenAI 形状 + 最简单的签到解析，适合验证通用签到框架。
3. **OfficeAce**：纯 OpenAI 上游，验证 Basic 认证与伪 200 错误帧。
4. **Qoder**：COSY 签名工作量大，必须先用固定测试向量验证 signer；M13-D2 先做
   signer 与合规口径确认，再碰真实账号（见 `docs/QODER_PROVIDER_RESEARCH.md` §12.4）。
5. **TRAE**：设备头族与签到设备号合规风险最高，放后。
6. **Kilo**：可选，只做无凭证免费渠道。
7. **Zen / ZCode**：排除或不作为上游；ZCode 已整体明确排除（2026-10-29），不实现、不注册。

## 6. 未验证与风险清单

- 所有 Provider 的“作者实测”都不是本项目实测；上线前必须用真实账号做
  授权 → 模型发现 → 普通请求 → 流式 → 工具调用 → 额度 → 错误/取消的端到端验证。
- CodeArts 的 refresh_token 是一次性的；任何并发刷新都会烧票。必须保证单写者。
- Qoder 的 COSY 签名、TRAE 的设备身份、CodeBuddy 的渠道头族都属于“客户端身份”
  范畴；不得伪造设备指纹或绕过平台限制。Qoder 的 `cosy-clientip`/UA/版本字段
  是 M13-D2 的显式政策确认项（见研究文档 §6.1）。
- OfficeAce 的签到是权益领取；按 `AGENTS.md` 默认不实现，除非上游明确允许自动化。
- Kilo 无额度接口，任何“剩余额度”数字都必须是 unknown，不能填 0 或无限。
- 参考仓库的测试均为 MockTransport；测试通过不等于上游当前仍接受同样协议。
