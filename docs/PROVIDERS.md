# Provider 接入规范（规划）

本文档描述每个上游 Provider 的接入边界、已验证事实、未验证项与允许的能力声明。
**原则：未用真实账号测试通过的 Provider 一律标记 `experimental` / `unavailable`，不得标记 `verified`。**

> M10 已按 Coding2API 源码重新盘点 8 个上游（含 OfficeAce/Kilo）并给出迁移判断，
> 见 `docs/PROVIDER_MIGRATION.md`；签到专项见 `docs/CHECKIN_RESEARCH.md`。
> 本文 §9/§13 的既有状态不因 M10 改变；冲突时以 M10 文档的证据与日期为准。

## 0. 状态定义

| 状态 | 含义 |
| --- | --- |
| `verified` | 真实账号实测通过：授权、模型发现、普通请求、流式、会话保持、额度、错误处理 |
| `experimental` | 依据公开源码/协议研究实现，仅部分验证或无真实凭据 |
| `unsupported` | 平台不支持或本项目不实现 |
| `unavailable` | 无法验证（无凭据/无访问/区域不可用） |
| `mock` | 本地确定性测试 Provider，不连接上游 |

## 1. Huawei CodeArts

- **工程边界**：CodeArts 只有一个 Provider。核心服务是 `codearts2api`（账号池、签名、网关），`codearttray` 是同一工程的 Windows 托盘前端，**不是独立 Provider**，也不单独计入能力矩阵。托盘对 `/api/ui/*` 的调用只作为管理 API 需求参考（见 `RESEARCH.md` §10.17）。
- **签到门控**：`/v1/ops/claim` + `/v1/ops/confirm` 属权益领取，默认不实现；仅在上游明确允许自动化并经运维确认后才作为默认关闭的可选项。
- **区域**：CN（`cn-north-4`）；国际站（`ap-southeast-1`）存在，但未验证，默认 `unavailable`。
- **基址**：`https://snap-access.cn-north-4.myhuaweicloud.com`
- **认证**：STS 临时 AK/SK + `security_token` + `refresh_token` + `client_id` + P-256 DPoP 私钥 + `domain_id`/`uid`。签名 `SDK-HMAC-SHA256`，头 `X-Security-Token`。refresh_token 一次性，刷新后必须原子替换。**刷新必须复用签发 refresh token 的 `client_id`**（登录默认 `codearts-snap`；用错 client 会得到终态 `invalid_grant`，表现为 STS 到期即掉线）。
- **模型发现**：`GET /v1/model/builtin`（`Agent-Type: PromptCenter`）给模型清单；长度/输出上限从
  `GET /v1/agent-center/agents/useragents` + `GET /v1/agent-center/agents/detail?agent_id=…`
  （`Agent-Type: AgentCenter`）的 `model_parameters` 合并，最多探测 6 个 agent，失败不影响清单。
- **推理**：`POST /api/v2/chat/completions`（OpenAI 形状，SSE），头 `x-auth-token`、`app-id: CodeAgent3.0`、`is_confidential: false`。
- **会话**：上游以 `x-ot-session-id` / `user-session-id` 头识别；本地 Go 参考实现存在 3 slot 上限。本项目按“每账号最大并发 + 上游会话上限”双重限制，不通过轮换突破。
- **额度**：`GET /snap-manager/v1/statistics/plugin`。两种参考实现单位不一致（积分 vs token），**首版只展示原始字段并标 `unknown` 单位**，除非运维配置已验证换算。
- **错误分类**：401/403→AuthInvalid；402/14018→QuotaExhausted；1005→ChannelRestricted/Plan；11102→ModelNotFound；`APIG.0602`/`TM.00001041`/并发文本→RateLimited/Concurrency；HTTP 200 内嵌 `error_code` 必须解析。
- **当前状态**：`experimental`（凭据与刷新流程仅源码核对）。

## 2. Alibaba Qoder（CN / International）

- **区域独立配置**：CN 与 Intl 的 gateway / openapi 主机不同，账号与 Token 不假定互通。
- **基址**：CN `https://gateway.qoder.com.cn`、`https://openapi.qoder.com.cn`；Intl `https://api3.qoder.sh`、`https://openapi.qoder.sh`。
- **认证**：COSY 私有协议。随机 AES-128 密钥（CBC，key=IV）加密身份 JSON，RSA 包装为 `Cosy-Key`，`Authorization: Bearer COSY.<payload>.<sig>`；配套 `Cosy-User/Date/Version/Machineid/...`。不对“设备指纹伪造”做迁移；机器标识只能由用户提供静态值或留空。
- **模型发现**：`GET /algo/api/v2/model/list?Encode=1`（自定义编码）。
- **推理**：`POST /algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`；SSE 为普通 `data:` JSON，可能带内错误。
- **额度**：`GET /sash/api/v2/me/usage`，读取 `user_quota`/`add_on_quota`（total/remaining/expiresAt）；备选 `/api/v2/quota/usage`。
- **错误**：`110`/`10605`/`1210`/429→RateLimited；`105`/`TOKEN_EXPIRE`→AuthInvalid；402→QuotaExhausted。
- **当前状态**：`experimental`。COSY 实现工作量大，需完整签名/编码测试向量后才可发布为可用。

## 3. ByteDance TRAE（CN / International）

- **基址**：CN chat `https://trae-api-cn.mchost.guru`、pay/签到 `https://api.trae.cn`、oauth `https://api.trae.com.cn`；Intl chat `https://coresg-normal.trae.ai`、pay `https://growsg-normal.trae.ai`（Intl oauth 无源码证据）。模型目录 `/api/ide/v1/get_detail_param`；聊天 `/api/agent/v3/llm_utils_chat`。
- **认证**：`Authorization: Cloud-IDE-JWT <token>` + `X-Ide-Token` + `X-Cloudide-Token`，可选 `X-Uid` 与运维导入的 `X-Machine-Id`/`X-Device-Id`。官方 IDE 还会发送 `X-App-Id`、版本族、设备/OS 头与 `Trae/…` UA；本项目**不迁移**（客户端身份冒充），见 `docs/TRAE_MIGRATION.md` §13。
- **协议**：自有信封 `function=solo_work_lite` / `config_name` / `model` / `stream=true`；消息需改写（`developer`→`system`、content 数组化、`tool_calls[].function`→`function_call`、`tools.parameters` 字符串化）；SSE 为命名事件 `output` / `token_usage` / `done` / `error`（`[DONE]` 兜底）。
- **会话**：源码未发现稳定上游会话头或取消端点；连续性靠客户端消息历史。`native_session=false`、`cancellation=false`、`agent_task=false`。
- **额度（已实现，MockVerified）**：CN `POST /trae/api/v2/pay/ide_user_ent_usage`（`{}`）逐包汇总，单位 credits，保留逐包到期阶梯，未知保持 `Unknown`（从不写 0）；Intl `POST /trae/api/v1/pay/ide_user_pay_status` 只报订阅计划，数值全部 `null`。
- **签到（已实现，CN，默认不领取）**：`/trae/api/v2/ug/checkin_credits/status|claim`；设备号必须是运维提供的 16 位数字，缺失即 `Unsupported` 且零请求；claim 需 `checkin.claim_enabled=true`，成功后回查 `checked_in` 与 credits 增量。不生成设备号。
- **刷新（已实现，MockVerified）**：`ExchangeToken`；提前 300 s、跨进程 60 s 租约、revision CAS、401 后强制刷新一次重试；Intl 登录 `Unsupported`，可用导入凭据的 `apiHost` 刷新。
- **错误**：`401` AuthExpired；`402`/`14018`/`4008` 额度；`1005` 权益/渠道；`11102` 账号受限；`404` 软限流；`429` 限流（`6004` 模型不可用）；`400` 请求无效；流内错误码同表。
- **管理台**：`/admin/api/auth/trae/*` 支持 start / status / complete / cancel；登录需运维提供 hex32 machineId+deviceId，回调链接粘贴回面板完成绑定；公开回调页不回显 token。
- **当前状态**：`experimental`。M14-D 协议核心 + M14-E 认证刷新/额度/CN 签到/管理台均已实现，全部 **MockVerified**，**无真实账号、未发真实请求**。协议与差异见 **`docs/TRAE_MIGRATION.md`**，执行记录见 `PROGRESS.md` §38/§39 与 `docs/MIGRATION_MATRIX.md` §10/§11。

## 4. Tencent WorkBuddy（CN / International）

- **基址**：CN `https://copilot.tencent.com`（计费 `https://www.codebuddy.cn`）；Intl `https://www.workbuddy.ai`。`workbuddy.ai` 与 `codebuddy.ai` Token **不可互换**。
- **认证**：`Authorization: Bearer` + `X-User-Id` + `X-Enterprise-Id` + `X-Tenant-Id` + `X-Domain`；UA/`X-Product`/会话头族。凭据包含 access/refresh token、uid、enterpriseId、domain、expiresAt。
- **模型**：CN `GET /v2/enterprises/{personal|eid}/models`；Intl `GET /v3/config`（仅桌面 UA）。
- **推理**：`POST /v2/chat/completions`（OpenAI 形状，上游实际偏流式；非流式由网关聚合）。
- **刷新**：`POST /v2/plugin/auth/token/refresh`（`X-Refresh-Token` + `X-Auth-Refresh-Source: plugin`）。加密来源（`$wbEncrypted`）只读，不回写。reference 语义：refresh token 轮换时先持久化再使用；轮换后写库失败不得当作成功。
- **设备码授权**：`POST /v2/plugin/auth/state?platform=CLI` → 官方 `authUrl` + `state`；`GET /v2/plugin/auth/token?state=`（`11217` 等待）→ `GET /v2/plugin/login/account?state=`（`12151` 等待）。上游不提供单独的短用户码，界面只展示真实 URL。
- **额度**：`POST /v2/billing/meter/get-user-resource`；月度包 `CycleCapacity*`、一次性包 `Capacity*`，`*Precise` 优先；单位 credits，**不得**当 token 换算；无有效包保持 `null`。
- **签到（仅 CN）**：`POST /v2/billing/meter/checkin-activity-status`（只读）与 `POST /v2/billing/meter/daily-checkin`。`1001`/`10001`/“已签到”归一为已签到；claim 只在 `checkin.claim_enabled = true` 时发送。Intl 明确 `Unsupported`。
- **错误分类（必须区分）**：
  - 登录态失效：`401`、`12153`、“Offline user session not found” → AuthExpired（可先刷新一次），刷新失败则 AuthInvalid（标记账号，不换号）。
  - 账号/授权：`11140`、`11142`、`14015`、`14016`、`14017` → AuthInvalid/AccountDisabled/ChannelRestricted。
  - 额度：`402`、`412`、`14001`、`14012-14014`、`14018`、`14019` → QuotaExhausted。
  - 限流：`6000-6007`、`6020-6022`、`14003` → RateLimited（账号级/队列级）。
  - 模型级：`6004`/`6008` → 该账号该模型冷却，不是账号失效。
  - 渠道：`11128` → 请求级（渠道/提示词风控，语义未定），不得转成普通 429，也不得自动改写提示词。
  - M14-A 修正：本节旧文本曾把 `402`、`3004` 列入“会话失效”，并把 `12153` 当作 SessionInvalid。`402`/`412` 实为 credits 用尽（`dsh` 与 Coding2API 一致）；`3004` 仅见于一张编码损坏的错误表，已撤回。
- **设备令牌**：`X-Device-Token` 由腾讯 Turing Shield SDK 生成。本项目**不实现**该 SDK、不伪造设备指纹；仅在运维显式提供静态头时透传，否则功能可能被上游降级/拒绝，状态保持 `experimental`。
- **当前状态**：`experimental`。M14-B 已实现原生 Rust 核心（凭据读取、模型发现、非流式/SSE、工具调用、错误归一化、CN/Intl 隔离）；M14-C 补齐刷新生命周期、设备码授权、额度与 CN 签到。全部为 **MockVerified**，**无真实账号、未发真实请求**。协议、架构适配、CN/Intl 矩阵与诚实头探针见 **`docs/WORKBUDDY_MIGRATION.md`**，执行记录见 `PROGRESS.md` §37 与 `docs/MIGRATION_MATRIX.md` §9。

## 5. Tencent CodeBuddy（CN / International）— 已移除

> **2026-10-29：CodeBuddy 已从本项目范围移除。** `ProviderKind::Codebuddy` 枚举、
> 管理台子导航与产品文档中的计划均已删除；本节仅保留历史协议研究，不得据此实现。

- **基址**：CN `https://copilot.tencent.com`；Intl `https://www.codebuddy.ai`。
- **认证/协议**：与 WorkBuddy 同源，Bearer + 企业/域头，`POST /v2/chat/completions`；模型与额度端点同族。
- **与 WorkBuddy 的关系**：可能共享企业积分。默认**不共享**；只有同一 `Quota Group` 且运维确认“同一余额池”时，聚合才按 `shared=true` 去重（取最新余额而非求和）。
- **错误**：`1005` 套餐、`6004` 模型限流、`11102` 模型不可用、`11140` 风控/账号限制。
- **当前状态**：`experimental`。

## 6. ZCode（明确排除 / Explicitly Excluded）

- **状态**：2026-10-29 起从 PolyGate 支持范围中移除。不实现 `ZCodeProvider`，不迁移 `zcode2api` 的任何功能，不注册、不出现在模型列表、不提供配置或管理界面。
- **原因**：参考仓库为 AGPL-3.0（禁止复制），其可用性依赖第三方验证码参数与设备指纹，与本项目边界冲突；且不在当前必需 Provider 列表内。
- **残留资料**：`docs/RESEARCH.md` §12 与 `.refs/liu5269_zcode2api` 仅作为历史审计记录保留，**不得据此实现或重新纳入**。

## 7. 明确排除

- **ZCode（Z.AI / BigModel）**：见 §6；不实现任何能力，不因 Coding2API 支持而重新加入。

- **OpenCode Zen**：不作为任何形式的上游 Provider（无账号、模型、额度、路由、认证）。
- **OpenCode 客户端**：作为下游，必须保持 OpenAI 兼容（`/v1/chat/completions`、`/v1/models`、SSE、工具调用）。

## 8. 配额组规则（规划）

| 场景 | 聚合方式 | UI |
| --- | --- | --- |
| 独立额度 | 各成员 `remaining` 求和（仅 Known） | 显示总和，未知成员标 Unknown |
| 共享额度（确认同一池） | 取最新 Known 快照，不求和 | 标注 Shared |
| 全部未知 | `remaining = null` | 显示 “Unknown”，禁止 0/∞ |
| 部分未知 | 已知部分求和 | `partial` + 成员明细 |

跨 Provider（WorkBuddy ↔ CodeBuddy）默认独立，除非运维显式配置 `shared=true` 并承担真实性责任。

> M13-B（2026-10-29）：CodeArts CN 的 chat/SSE/tools/额度已实现并通过 mock HTTP 测试；
> 尚未用真实账号验证，`verification` 保持 `experimental`。详见 `PROGRESS.md` §23。

## 9. Provider 能力声明表

| Provider | 模型推理 | SSE | Tools | 原生会话恢复 | 额度 | Agent Task | 当前状态 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CodeArts CN | 已实现（M13-B） | 已实现 | 已实现 | Unsupported（`chat_id` 仅缓存亲和） | 已实现（积分；未知保留） | Unsupported | experimental（无真实账号） |
| Qoder CN | 已实现（COSY 签名；Live Verified） | 已实现（信封 SSE；Live Verified） | 已实现（tool_calls；Live Verified） | Unsupported（无 upstream session / 取消接口） | 已实现（积分；Live Verified） | 已实现（campaign；Live Verified 一次领取） | experimental（授权/模型/额度/Chat/SSE/工具/签到均已 Live Verified） |
| Qoder Intl | 计划 | 计划 | 计划 | 计划 | 已实现（未实测） | Unsupported | unavailable（授权与签到未实测） |
| TRAE CN | 已实现（M14-D；MockVerified） | 已实现（MockVerified） | 已实现（MockVerified） | Unsupported（无 upstream session） | 已实现（credits；M14-E，MockVerified） | Unsupported | experimental（授权/刷新/额度/签到均 MockVerified；无真实账号） |
| TRAE Intl | 已实现（M14-D；MockVerified） | 已实现（MockVerified） | 已实现（MockVerified） | Unsupported | 已实现（订阅计划，数值 Unknown；M14-E） | Unsupported | unavailable（无真实账号；OAuth 主机未验证，登录/签到 Unsupported） |
| WorkBuddy CN | 已实现（M14-B；模型发现 + Chat/SSE Live Verified 2026-10-30） | 已实现（真实账号发现 15 个模型） | 已实现（Chat 非流式与 SSE 均已实测；工具调用 MockVerified） | Unsupported（无 upstream session） | 已实现（credits；M14-C，MockVerified，未点刷新实测） | Unsupported（growth/checkin 不是 Agent Task API） | experimental（授权/模型/Chat 已 Live Verified；额度刷新与签到待管理台点击确认） |
| WorkBuddy Intl | 已实现（MockVerified） | 已实现（MockVerified） | 已实现（MockVerified） | Unsupported | 已实现（credits；M14-C，MockVerified） | Unsupported | unavailable（首条 system/会话头未适配；签到 Unsupported） |
| Loomy CN | 已实现（会话导入；模型发现/Chat/SSE/额度 Live Verified 2026-10-30） | 已实现（真实账号 10 个 chat 模型） | 已实现（Live Verified；工具调用未实测） | Unsupported（无原生会话） | 已实现（credits；Live Verified） | Unsupported（不做签到/领奖） | experimental（Chat/SSE/额度已实测；无 refresh，14 天需重导） |
| OfficeAce CN | 已实现（M17；华为云 PKCE + DPoP 轮询） | 已实现（MockVerified） | 已实现（OpenAI 协议；MockVerified） | Unsupported（无原生会话） | 已实现（officeace_points 主账；MockVerified） | 已实现（每日 bonus claim；默认关闭，MockVerified） | experimental（无真实账号；V11/V4 签名已按参考向量逐字节比对） |
| CodeBuddy CN | 已移除 | 已移除 | 已移除 | 已移除 | 已移除 | 已移除 | excluded |
| CodeBuddy Intl | 已移除 | 已移除 | 已移除 | 已移除 | 已移除 | 已移除 | excluded |

> ZCode 已明确排除（2026-10-29），不再列入能力矩阵。

> M13-D2-D（2026-10-29）：Qoder 错误语义收尾——`[EXCEED_QUOTA]` 归一为额度耗尽、
> 空闲 120 s 断流、模型级错误（节点故障/模型不存在/429/402）分别映射到
> `UpstreamServer`/`ModelUnavailable`/`RateLimited`/`QuotaExhausted`，详见
> `PROGRESS.md` §32。

> M13-D2-E（2026-10-29）：会话生命周期与租约测试——客户端断连、空闲超时、同会话并发、
> 流内额度耗尽、会话失效恢复、网关重启。租约在 SSE body 丢弃时立即释放；同会话请求串行化；
> 失效会话只在同一账号重置 upstream id，认证失败不静默换号。全部以本地 Mock 网关 +
> 真实 Router/Provider 验证，真实账号仅冒烟；双账号负载均衡未实现，详见 `PROGRESS.md` §33。

> M15-G（2026-10-30）：Qoder 目录解析补齐——响应按 scene 分组（桌面端读 `app`，抓包协议是
> `chat`），现在先取 `chat`、缺失时回退 `app`；每条记录读取 `display_name`、`max_input_tokens`
> （缺失时回退 `default_context_window`）与 `max_output_tokens`。上游 `is_vl` **不作为视觉能力
> 依据**：同日实测 `kmodel` 附图答 "Blue"、无图答 "Missing"，而 `dmodel`/`gmodel` 在 `is_vl=true`
> 下仍答 "Unknown" 并在推理中承认没有收到图像。视觉仍以 `verified_vision` 的实测名单为准
> （本次新增 `kmodel`），详见 `PROGRESS.md` §69。

> “计划”表示已设计接入但尚未用真实凭据验证；发布前必须以实测结果替换此表。
> 以源码事实为依据的最新能力矩阵见 §13；两表冲突时以 §13 为准。

---

# 10. 【CodeBuddy】接入细则（历史研究，已移除）

> **2026-10-29：CodeBuddy 已从范围移除。** 本节仅作历史审计记录，不构成实现计划。
> `ProviderKind::Codebuddy` 与 UI 入口均已删除；后续不得重新引入。

> 审计对象：`.refs/RobbsLuo_Coding2API/src/provider/codebuddy/`（MIT，只读研究，未复制代码）。
> 这些是 Coding2API 已实现的协议事实；**本项目尚未用真实凭据验证**。

## 10.1 区域与端点

| 用途 | CN | International |
| --- | --- | --- |
| API 基址 | `https://copilot.tencent.com` | `https://www.codebuddy.ai` |
| 聊天 | `POST /v2/chat/completions` | 同路径 |
| 模型 | `GET /v3/config`（另有 CN 企业模型 `/v2/enterprises/{eid}/models`） | `GET /v3/config` |
| 个人额度 | `POST /v2/billing/meter/get-user-resource` | 同路径 |
| 企业额度 | `GET /v2/billing/meter/get-enterprise-user-usage` | 同路径（未逐一验证） |
| Token 刷新 | `POST /v2/plugin/auth/token/refresh` | 同路径 |
| OAuth 设备码 | `/v2/plugin/auth/state` → `/v2/plugin/auth/token` → `/v2/plugin/login/account` | 同路径 |

关键差异（Coding2API 观测）：

- CN 与 Intl 是**两套账号体系**，Token 不互通。
- Intl 网关硬性要求 `messages[0]` 为 system；缺失会 400 / 11128。CN 无此要求。
- 两者共用同一套路由与 `/v2/chat/completions` 形状。

## 10.2 凭据字段

| 字段 | 说明 |
| --- | --- |
| `bearer_token`（或 `access_token`/`token`） | 必填 |
| `user_id` | 上游用户 ID |
| `account_uid` | OAuth 账号 ID，优先于 user_id |
| `domain` | 进入 `Host` / `X-Domain`，必须为合法主机名 |
| `enterprise_id` | 企业 ID；写入 `X-Enterprise-Id` 与 `X-Tenant-Id` |
| `department_full_name` | 可选，UTF-8 百分号编码后放 `X-Department-Info` |
| `refresh_token` | 仅 OAuth |
| `expires_at` | 缺失时回落 JWT `exp`；CN OAuth 响应常不带上限字段 |
| `auth_source` | `manual` / `oauth`；手动凭据永不刷新 |
| `quota_probe_mode` | `personal` / `enterprise` |

## 10.3 请求头（合规边界）

Coding2API 会发送：

- `Authorization: Bearer <token>`
- `X-User-Id`、`X-Enterprise-Id`、`X-Tenant-Id`、`X-Domain`、`Host`（同一 endpoint 派生）
- `X-Requested-With: XMLHttpRequest`
- 会话头族：`X-Conversation-ID`、`X-Conversation-Request-ID`、`X-Conversation-Message-ID`、`X-Request-ID`
- `X-Agent-Intent: craft`、`X-Agent-Purpose: conversation`、`X-IDE-*`、`X-Product: SaaS`、`X-CodeBuddy-Request: 1`
- `User-Agent: CLI/<ver> CodeBuddy/<ver>`
- 额度探测时**不发送**企业上下文头（quota_only 模式）

**M15 起改为开关**：上述头族全部内置在 `src/providers/profile.rs`，由 provider 选项
`client.identity` 控制。

- `honest`（默认）：只发协议必需头 + `polygate/<version>` UA。
- `impersonate`：按请求类别发送官方客户端身份——chat 带会话 id 族与 chat UA，额度/签到
  带桌面 UA 与闸门头，refresh 只带 `X-Refresh-Token` + `X-Auth-Refresh-Source`。
  Intl 缺会话族会被 11128 拒绝，所以 Intl 必须开这一档。

**仍然不做**：

- Turing 设备令牌 `X-Device-Token`、`X-Machine-ID`/`X-Session-ID`（原生 SDK 生成）。
- 验证码流程、批量注册。
- 为规避 11128 而“中和正文中的客户端指纹”等风控逃避逻辑（主体改写）。

身份头是公开常量与按 uid 派生的值，不是设备指纹伪造。**Qoder CN 已做真实对照**：honest 与
`impersonate` 两种模式对 `/v1/chat/completions` 均返回 200，签名链路未被新增头破坏。
WorkBuddy、TRAE 的伪装模式仍未真实联调，因此状态不因开关改变。

## 10.4 请求改写（需独立实现并测试）

Coding2API 的适配行为：

1. 强制 `stream=true`；非流式由网关聚合。
2. `stream_options.include_usage=true`。
3. `developer` 角色 → `system`（上游不认 developer，会触发 11128）。
4. Intl：若首条不是 system，注入中性最小 system（不覆盖客户端已有 system）。
5. 缺省补 `enable_thinking=true`、`reasoning_effort=medium`；不覆盖客户端显式值。
6. 清理孤儿 `tool_calls` 与 `role:"tool"`（按 assistant 消息整体清理）。
7. 工具调用参数按增量转发。

这些是协议兼容行为，不是设备/身份伪造，**可以迁移**；但每一条都需要真实请求验证。

本项目当前只做客户端显式值的透传：#5 不补默认值，只在客户端设置
`enable_thinking` / `reasoning_effort` 时原样放进上游 body。CodeArts 之外的 Provider
没有对应上游字段证据，网关接受这些字段但不下发（模型自身是否输出 reasoning 由上游决定）。

## 10.5 模型发现

- `/v3/config` 返回 `data.models[]`，字段：`id`、`name`、`credits`（倍率，如 “x0.29 credits”）、`maxInputTokens`、`maxOutputTokens`、`supportsImages`、`supportsToolCall`、`supportsReasoning`、`reasoning`（默认 effort）。
- Coding2API 缓存 10 分钟；失败回退静态 `glm-5.2`、`deepseek-v4-pro`。
- Intl 会额外探测账号对每个模型的真实访问权限，过滤掉套餐不允许的模型；探测失败不误删。
- **本项目**：模型列表必须标注来源（config/discovered）与“是否本账号可用”；静态兜底不得冒充完整目录。

## 10.6 额度语义

- 个人额度：逐套餐累加 `CycleCapacitySize` 与 `CycleCapacityRemain`（Precise 优先）；**不使用**顶层 `TotalDosage`（它按上周期口径，实时性错误）。
- 企业额度：`limitNum` 与 `credit`，remaining = limit - used。
- 单位：**积分（credits）**，不是 token。
- 到期阶梯：逐包 `CycleEndTime` + 剩余积分；已过期或剩余 0 的包不进入调度。
- **本项目**：`QuotaSnapshot.unit = credits`；Quota Group 默认独立，只有确认同一余额池时才 `shared=true` 去重。

## 10.7 错误分类（必须保留的区分）

| 信号 | 语义 | 本项目映射 | 冷却/动作 |
| --- | --- | --- | --- |
| 402 / 14018 | 积分耗尽 | `QuotaExhausted` | 到下一刷新周期（Coding2API 用次日 04:00） |
| 1005 | 套餐/权益耗尽 | `ChannelRestricted` 或 `QuotaExhausted` | 长冷却，不换号绕过 |
| 400/404 + 11102 | 该后端无此模型 | `ModelNotFound` | (账号, 模型) 负缓存 |
| 11140 | 账号被风控限制 | `AccountDisabled` / `ChannelRestricted` | 账号级冷却；**当前请求不再换号** |
| 401/403 | 凭据失效 | `AuthInvalid` | 标记凭据不可用，等待重新授权 |
| 404 | 路径/模型不存在 | `ModelNotFound`/`Soft` | 不累计账号错误 |
| 429 + 6004 / 14003 | 模型级限流/饱和 | `RateLimited`（模型作用域） | 只冷却该模型 |
| 429 其他 | 账号级限流 | `RateLimited` | 短冷却 |
| 400 + 11101/11115/11128/11135 | 请求体/上下文/渠道/图片问题 | `BadRequest` / `ChannelRestricted`（请求级） | 不惩罚账号；可换号重试 |
| 400 其他 | 无效请求 | `BadRequest` | 直接 400，不冷却 |

> 特别注意：11128 是**窗口型瞬时渠道拦截**，不是 429、不是账号失效；把它当 429 会误冷却整个账号，把它当 401 会误禁用账号。本项目应将其标为请求级 `ChannelRestricted`（不罚账号），并在日志中单独统计。

## 10.8 会话行为

- CodeBuddy 上游没有稳定会话头；Coding2API 每次请求生成新的 `X-Conversation-*`。
- 会话粘性由网关 `ConversationAffinity` 负责（显式键 > 请求头 > 消息前缀哈希，TTL + LRU），纯内存，重启丢失。
- **本项目**：会话绑定持久化到 SQLite；CodeBuddy 的能力声明中 `native_session = false`，只声称“网关侧粘性”。

## 10.9 CodeBuddy 迁移结论

| 模块 | 迁移 | 难度 | 说明 |
| --- | --- | --- | --- |
| 聊天/SSE 适配 | 是 | 中 | OpenAI 形状，重点是工具与错误 |
| 模型发现 | 是 | 低-中 | `/v3/config` + 套餐过滤 |
| 额度 | 是 | 中 | 逐包口径，别用 TotalDosage |
| 凭据/刷新 | 是 | 中 | OAuth 刷新旋转需原子持久化 |
| 错误分类 | 是 | 中 | 11140/11128/6004 必须分开 |
| OAuth 登录 UI/设备码 | 可推迟 | 中 | 首版可只支持手动导入 bearer |
| 签到/成长/抽奖 | 否 | - | 非网关核心、含不可逆权益操作 |
| SDK 伪装/身份头 | **可选**（`client.identity`） | - | M15 移植，见 `providers/profile.rs` |
| 设备令牌/验证码 | **否** | - | 违反项目安全限制 |

**状态**：CN/Intl 均保持 `experimental`（协议有源码依据，但本项目未实测）；未获得真实凭据时不得标记 `verified`。

---

# 11. Coding2API 对其他 Provider 的实现对照

> 仅记录实现方式，不复制代码；细节见 `docs/RESEARCH.md` §9。

| Provider | Coding2API 实现方式 | 对本项目的影响 |
| --- | --- | --- |
| Qoder | 独立 COSY 客户端：RSA 包装 AES 密钥、自定义 base64/字符置换、`/algo/...` 聊天、OpenAPI 额度 | 认证复杂，Rust 需分步实现并用测试向量验证；不能只换 Base URL 声称支持 Intl |
| TRAE | 独立客户端：`Cloud-IDE-JWT` + 设备/版本头族、命名 SSE 事件 | 无稳定上游会话；`native_session=false`；区域额度接口不同 |
| CodeArts | STS 临时 AK/SK + DPoP、HMAC 签名、`x-ot-session-id`、`/snap-manager` 额度 | 与本地 Go 实现事实一致；额度单位需保持 unknown |
| OfficeAce | `V11-HMAC-SHA256`、Basic 模型密钥、伪 200 错误帧 | 不在本项目 Provider 范围；仅作为错误处理参考 |
| Zen | OpenCode Zen（免费档、工具门控） | **本项目明确排除**，不实现任何 Zen 逻辑 |
| Kilo | 标准 OpenAI 网关、免费模型标记 | 不在本项目 Provider 范围 |
| CodeBuddy (CN/Intl) | 见 §10 | **已移除**（2026-10-29），不实现 |
| 其他（Growth/Checkin） | CodeBuddy 专用签到、成长、抽奖 | 不迁移 |

---

# 12. dsh-wtq-pool 源码审计（2026-10-28）

> 审计对象：`.refs/chengzhi-c_dsh-wtq-pool`，包版本 `1.2.0`，提交 `f7888dc5759bb1e5a4360ac0c18ccabf28a88ca8`，MIT License。仅做静态源码研究：未安装依赖、未运行测试、未启动服务、未读取真实登录态、未调用真实上游。以下事实不能将本项目中的任何 Provider 状态提升为 `verified`。

## 12.1 注册、凭据与共同网关边界

- 源码不是静态 pi-ai 凭据 Provider 列表；`src/index.ts` 运行时注册六个本地 OpenAI-compatible Provider，pi-ai 的 `authKind` 为 `none`，真实厂商凭据由 gateway 每次请求从账号池读取。
- 六个区域注册项位于 `src/host.ts`：WorkBuddy CN/Global、TRAE CN/AI、Qoder CN/Global。每个区域使用独立 loopback gateway 和账号池；CN 与 International 不假定 token、额度或账号互通。
- WorkBuddy 账号发现位于 `src/accounts.ts`：读取桌面 `.info` 登录文件、插件自管 `signins/` 与归档；支持 OAuth/device authorization 的面板流程（`src/login.ts`）。
- TRAE 凭据来自 `src/trae-discovery.ts` 的桌面 `storage.json` / `iCubeAuthInfo`，并支持 CN CLI token；国际 CLI token 分支明确未实现。设备/机器身份来自持久化本机数据，缺失时不伪造。
- Qoder 凭据来自 `Local State` / `auth.v1.dat`（`src/qoder-discovery.ts`），由 Windows DPAPI 解密后只保存在内存；旧 `state.vscdb` 布局未实现。
- 共同 gateway 仅绑定 loopback，并检查来源、Host、Origin 和本地 Bearer 形状；厂商 token 不暴露给 renderer 或下游。renderer 通过 panel bridge 调用 host，不直接访问上游。

## 12.2 Provider 实际协议矩阵

| Provider | 区域与上游 | 模型发现 | 聊天/流式 | 额度与状态 | 源码边界 |
| --- | --- | --- | --- | --- | --- |
| WorkBuddy | CN chat `https://copilot.tencent.com`，billing `https://www.codebuddy.cn`；Global 默认 `https://www.workbuddy.ai`，并识别 `codebuddy.ai` | CN `/v2/enterprises/personal/models`；Global `/v3/config` | `/v2/chat/completions`，上游强制流式并转为 OpenAI-shaped SSE | credits、套餐、过期包；账号/模型 cooldown、session-dead、restricted hold；支持 preferred、round-robin、expiry、sticky | CodeBuddy 没有独立 adapter/provider；由同一 WorkBuddy upstream 的 domain/region 分支处理 |
| TRAE | CN chat `https://trae-api-cn.mchost.guru`、remote `https://solo.trae.cn/api/remote/v1`、pay `https://api.trae.cn`；AI chat/remote `https://coresg-normal.trae.ai`、pay `https://growsg-normal.trae.ai` | 两区 `/api/ide/v1/get_detail_param`，按多个 function 合并 directory | `/api/agent/v3/llm_utils_chat`；命名 SSE 事件，转换 text/reasoning/tool/usage | CN usage `/trae/api/v2/pay/web_user_ent_usage` 与 daily check-in；AI 读取 subscription/trial，不读取 CN work-credit 或 check-in | `TraeRegion` 是 `cn` / `ai`，不是 `global`；未证明原生 session resume |
| Qoder | CN `https://gateway.qoder.com.cn/` + `https://openapi.qoder.com.cn`；Global `https://api3.qoder.sh/` + `https://openapi.qoder.sh` | `/algo/api/v2/model/list?Encode=1` | `/algo/api/v2/service/pro/sse/agent_chat_generation?...`；普通 `data:` JSON 行，不是命名事件 | `/sash/api/v2/me/usage`，回退 `/api/v2/quota/usage`；user/add-on/dedicated packages 与 campaigns | COSY AES/RSA/custom encoding；目录没有可靠 output limit，host 使用默认 max tokens |
| CodeBuddy | 没有独立区域 Provider；CN/Global host 由 WorkBuddy upstream 识别 | 复用 WorkBuddy CN/Global 目录 | 复用 WorkBuddy `/v2/chat/completions` | 复用 WorkBuddy 额度与账号状态语义 | 不应在 Rust 侧把它误写成与 WorkBuddy 完全独立的已实现 adapter |

## 12.3 调度、会话与错误恢复事实

- WorkBuddy gateway 单请求最多尝试八个账号；软限流、额度、session-dead、WAF、模型层级和账号受限等错误按不同作用域处理。首个输出前可以 failover；已有输出后保持 200，在 SSE 中发送错误并结束，不能回滚已发送内容。
- TRAE/Qoder gateway 都有模型目录缓存、账号级候选排序、首 token 前的响应探测与失败分类。response abort 会取消上游；无 finish 的 EOF 会补齐 finish 与 `[DONE]`。
- 三家都实现了“网关侧账号亲和/轮换”，但源码没有证明 TRAE/Qoder 的稳定原生会话恢复；这不能写成 provider-native session resume。
- `source-pool.ts` 共享 TRAE/Qoder 的 preferred、expiry、round-robin、failure hold 和 rotation，但状态 key 包含 source + region + label，避免同名账号跨区污染。模型发现/健康读取不推进 chat rotation。
- 额度查询失败保留 previous cache 或显示 unknown，不能在本项目改写为 0；账号池的 reserve/cooldown 可能影响候选资格，而不是纯 UI 数字。

## 12.4 可迁移事实与明确禁止迁移项

可独立重写的事实包括：端点、协议字段、SSE 事件/帧语义、错误作用域、模型目录缓存、额度包到期处理、loopback bridge 的访问边界和“首 token 前可切换、首 token 后不重放”的流式生命周期。

不复制 dsh 的 TypeScript、CSS、图标、静态资源或表达性文案；不迁移设备指纹伪造、客户端身份冒充、验证码/风控绕过、封禁后换号继续调用等逻辑。dsh 的 MIT 许可证只说明参考仓库可阅读和独立重写，不等于本项目已有真实 Provider 验证。

**本项目状态：** Qoder CN/TRAE CN/WorkBuddy CN 以及对应 International/AI 变体继续保持 `experimental` 或 `unavailable`，直到真实账号完成授权、模型发现、普通请求、流式、会话、额度和错误处理验证。

> M13-D2-B（2026-10-29）：Qoder 设备码授权、流程秘密加密、账号落库、单写者刷新
> 与管理台流程已实现；一个 CN 账号已完成真实设备码授权并落库（授权/userinfo/凭据加密
> 为 Live Verified）。仍未注册 Provider，未调用模型/Chat/额度；刷新与 Intl 未验证。
> 详见 `PROGRESS.md` §29。

---

# 13. Provider 能力矩阵（2026-10-28 汇总）

> 本表整合 §1–§12 的研究结果，不新增未列举的源码证据。**已验证能力数为 0**：没有任何 Provider 使用真实账号完成过授权、推理、流式、额度与错误处理的端到端验证。

## 13.1 读表说明

| 标记 | 含义 |
| --- | --- |
| `源码` | 参考仓库（Coding2API / dsh-wtq-pool / codearts2api / zcode2api）中存在实现证据，本项目**未实测** |
| `部分` | 参考实现只覆盖端点透传、固定列表或有限语义，不能当完整能力 |
| `需转换` | 上游协议不是该形状，必须在网关层转换（dsh 已有部分转换实现） |
| `原生` | 上游本身即该协议形状 |
| `—` | 参考源码中未发现该能力；本项目不得声称支持 |

`OpenAI API` / `Anthropic API` 两列描述**上游原生形状**，不是本项目向下游暴露的能力；本项目向下游同时提供 OpenAI Chat、OpenAI Responses、Anthropic Messages 三套兼容层，与上游形状无关。

## 13.2 能力矩阵

| Provider / 区域 | 认证 | 模型发现 | 额度查询 | 会话管理 | SSE | Tool Calling | OpenAI API | Anthropic API | Agent Tasks | 状态 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| CodeArts CN | 源码（STS AK/SK + security_token + DPoP P-256，`SDK-HMAC-SHA256`） | 源码（`/v1/model/builtin`，`Agent-Type: PromptCenter`） | 源码（`/snap-manager/v1/statistics/plugin`，**单位 Unknown**） | 部分（`x-ot-session-id`/`user-session-id`；本地 Go 用共享槽，非每会话独占） | 源码（OpenAI SSE，重组） | 源码（增量合并） | 原生 | 需转换 | — | experimental |
| CodeArts Intl | 未验证（仅预留 `ap-southeast-1` 配置） | 未验证 | 未验证 | 未验证 | 未验证 | 未验证 | — | — | — | unavailable |
| Qoder CN | 已实现并实测（COSY：AES-128-CBC 身份密文 + RSA 包装 + 自定义 base64；仅 4 个签名头） | 已实现并实测（`/algo/api/v2/model/list?Encode=1`） | 已实现并实测（`/api/v2/quota/usage`） | 无原生会话（`native_session=false`） | 已实现并实测（`{statusCodeValue, body}` 信封） | 已实现并实测（聚合流式 `tool_calls`；多轮工具结果） | 需转换 | 需转换 | — | experimental（CN 已 Live Verified） |
| Qoder Intl | 源码（独立 `global` 凭据，不与 CN 互通） | 源码（同路径，独立主机） | 源码（`openapi.qoder.sh`） | 部分（同上） | 源码 | 源码 | 需转换 | 需转换 | — | unavailable |
| TRAE CN | 已实现（回调授权 + `ExchangeToken` 刷新/CAS/租约；`Cloud-IDE-JWT` + `X-Ide-Token`/`X-Cloudide-Token`；IDE 头族与生成设备号**不迁移**；MockVerified） | 已实现（`/api/ide/v1/get_detail_param`；`max>dev` 取 max；MockVerified） | 已实现（`ide_user_ent_usage` 逐包 credits + 到期阶梯；MockVerified） | —（无稳定会话头，仅客户端消息历史） | 已实现（命名事件 `output`/`token_usage`/`done`/`error`；MockVerified） | 已实现（`function_call` 分片合并；MockVerified） | 需转换（**已实现**） | 需转换（**已实现**） | — | experimental（M14-D/M14-E；MockVerified） |
| TRAE Intl | 已实现（独立域 `trae.ai`；登录 Unsupported，可用导入 `apiHost` 刷新；MockVerified） | 已实现（同路径，独立主机；MockVerified） | 已实现（`ide_user_pay_status` 订阅计划，数值 Unknown；MockVerified） | —（同上） | 已实现（**MockVerified**） | 已实现（**MockVerified**） | 需转换（**已实现**） | 需转换（**已实现**） | — | unavailable（无真实账号） |
| WorkBuddy CN | 源码（Bearer + `X-User-Id`/`X-Enterprise-Id`/`X-Domain`；会话头族**未迁移**） | 源码（`/v2/enterprises/{id|personal}/models`；**已实现**，MockVerified） | 源码（credits + 套餐/资源包，逐包到期；**已实现**，MockVerified） | 部分（无原生会话；网关 sticky） | 源码（OpenAI-shaped；**已实现**，MockVerified） | 源码（**已实现**，MockVerified） | 原生（`/v2/chat/completions`；**已实现**） | 需转换（**已实现**） | —（growth/checkin 不是 Agent Task API） | experimental（M14-B/M14-C；MockVerified） |
| WorkBuddy Intl | 源码（独立桌面登录快照；Token 不与 CN 互换） | 源码（`/v3/config`，需桌面 UA；**已实现解析**） | 源码（独立 credits；**已实现**，MockVerified） | 部分（同上） | 源码（**已实现**） | 源码（**已实现**） | 原生（**已实现**） | 需转换（**已实现**） | — | unavailable（首条 system/会话头未适配，未实测） |
| CodeBuddy CN | 已移除 | — | — | — | — | — | — | — | — | excluded |
| CodeBuddy Intl | 已移除 | — | — | — | — | — | — | — | — | excluded |

> ZCode 已明确排除（2026-10-29），不再评估、不实现。
>
> M14-B/M14-C（2026-10-29）：WorkBuddy 适配器核心、刷新、设备码、额度与 CN 签到已实现并通过 Mock 测试（MockVerified）；Intl 签到 Unsupported。
>
> 2026-10-30 实测（WorkBuddy CN，真实账号经管理台授权）：provider 需在配置里显式启用
> `kind = "workbuddy" region = "cn"`，否则管理台会创建出“未注册 Provider”的账号，
> 额度/签到按钮返回 `provider is not registered`。启用后启动时发现 15 个模型，
> `POST /v1/chat/completions`（`workbuddy-cn::<model>`）非流式与 SSE 均返回 200 并带正确 content。
> 额度刷新与签到未由自动化触发，仍按 MockVerified 记录。
> 表格中“源码”表示参考实现的协议依据，“已实现”表示 PolyGate 的 Rust 适配器；
> 两者都不等于真实上游可用。诚实头能否通过上游校验仍未验证，见
> `docs/WORKBUDDY_MIGRATION.md` §14 与 `PROGRESS.md` §36.5。
>
> M14-D（2026-10-29）：TRAE 核心（凭据、区域隔离、模型发现、聊天/SSE、工具、错误、超时）
> 已实现并通过 Mock 测试。M14-E（2026-10-29）补齐回调授权、`ExchangeToken` 刷新、
> 额度与 CN 签到，并接入管理台；全部 MockVerified，未发真实请求。TRAE 的 IDE 头族与
> 生成设备号按合规边界 **不迁移**，设备 ID 由运维提供；诚实头是否被上游接受仍未验证，
> 见 `docs/TRAE_MIGRATION.md` §13/§18 与 `PROGRESS.md` §38/§39。

## 13.3 已验证 / 未验证清单

**已验证（真实账号端到端）**：无。任何 Provider 的任何一项能力都不得标 `verified`。

**有源码依据但未实测**：上表所有标 `源码` 的单元格。它们的共同前提是“参考实现在其作者的环境里可能工作过”，不等于本项目的 Rust 实现可用，也不等于上游当前仍然接受同样的头与协议。

**明确未实现/未发现**：CodeArts 与 Qoder/TRAE/WorkBuddy 的 Agent Task API、TRAE 的原生会话头、Qoder 的原生会话恢复证明。ZCode 已整体排除，不属于“未实现”范畴。

**跨 Provider 不得混用**：CN 与 International 的 token、额度、账号池、区域代码（WorkBuddy `cn/global`、TRAE `cn/ai`、Qoder `cn/global`）均独立；CodeBuddy 不是第三个独立上游。

## 13.4 状态变更规则

1. 只有完成“授权 → 模型发现 → 普通请求 → 流式 → 工具调用 → 额度 → 错误与取消”的实测，才允许从 `experimental` 升为 `verified`。
2. 缺凭据或区域不可访问时保持 `unavailable`，不得用 Mock 结果替代。
3. 上游拒绝设备/客户端头时，记录为限制，不通过伪造头解决。
4. 状态变更必须同时更新本节、§9 能力表与 `PROGRESS.md`，并附审计日期与证据来源。