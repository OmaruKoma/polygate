# 参考仓库与协议研究

> 阶段：工作区检查 / 研究初始化（2026-10-28）
> 结论只区分三类：**已观察事实**（附源码位置）、**推断**（未验证）、**未知**。未做真实账号联调。

## 1. 工作区检查结果

| 项目 | 结果 |
| --- | --- |
| 当前工作目录 | `<repo>` |
| Git | 本目录已初始化为独立仓库（`main`）；上级目录另有一个 Git 仓库 |
| 原有项目文件 | 本目录最初为空；当前存在的 `Cargo.toml`、`migrations/`、`src/` 骨架是上一轮被中断的开发尝试所创建，不属于用户原有文件 |
| Rust 工具链 | rustc / cargo `1.98.1`，stable `x86_64-pc-windows-msvc`（2026-09-01） |
| 前端工具链 | Node `v26.3.0` / npm `11.16.0`（仅用于构建静态资源，不作为生产运行时） |
| 参考仓库 | 6 个参考仓库已存在于 `.refs/`（在本阶段限制下达前克隆；本阶段未再下载任何仓库，`.refs/` 已加入 `.gitignore`） |
| 真实 Provider 测试 | **未执行**。无凭据、未调用任何上游接口 |

## 2. 本地已有源码清单

| 路径 | 语言 | 许可证 | 与本项目关系 |
| --- | --- | --- | --- |
| `~/codearts2api` | Go（主）+ 旧 Python FastAPI 副本 | **无 LICENSE 文件；操作者本人作品**（2026-10-29 确认） | CodeArts 协议事实来源；代码可复用（作者授权），复制时保留版权声明并在 PolyGate `LICENSE`/`NOTICE` 标注来源 |
| `~/codearttray` | Go（fyne systray） | **无 LICENSE 文件；操作者本人作品**（2026-10-29 确认） | **`codearts2api` 的 Windows 系统托盘管理程序，与 `~/codearts2api` 属于同一套 CodeArts 工程，不是独立 Provider**。它通过网关 `/api/ui/*` 做账号列表/删除、额度与签到展示；可复用（作者授权），但托盘/自启/桌面 UI 属于设计取舍，默认不迁移 |
| `~/workbuddy2api` | Python | MIT（`LICENSE`，Copyright (c) 2026 HanHan666666） | WorkBuddy/CodeBuddy 登录态反代，协议事实与错误分类参考。其工作区有 2 个未提交补丁（加密凭据只读、空 finish_reason→null） |
| `~/codearts2api.exe`（`~/tools/codearts2api.cmd`） | 二进制 | 操作者本人作品；无 LICENSE 文件 | 已构建的 CodeArts 网关，未用于本阶段测试 |

其他目录（`oc2apiWorkers`、`spamheart2api`、`rustcode` 等）与本任务无直接关系，未纳入研究。

## 3. 参考仓库矩阵

| 仓库 | 语言 | 许可证 | 可借鉴内容 | 使用边界 |
| --- | --- | --- | --- | --- |
| RobbsLuo/Coding2API | Python / FastAPI | MIT | 多 Provider 协议、OpenAI Chat/Responses/Anthropic 适配、调度与冷却、SQLite 设计 | 独立实现；不整文件搬运 |
| chengzhi-c/dsh-wtq-pool | TypeScript / Electron | MIT | TRAE / WorkBuddy / Qoder 协议、UI 设计语言 | 只重绘 UI，不复制 CSS/JS/图标/文案 |
| Johnsheng1/codearts2api | Python（单文件） | MIT | CodeArts 模型、流式、额度接口 | 独立实现 |
| liu5269/zcode2api | Python + Node | **AGPL-3.0** | ZCode Anthropic 协议、SQLite 账号管理 | **只提取协议事实，不复制代码**；验证码/指纹逻辑不迁移 |
| yetone/magpie | Go | MIT | 路由策略、额度预留、会话粘性思路 | 重新设计，不转写 |

许可证结论：本项目可以采用 MIT（或 Apache-2.0，发布前二次确认）；不得把 AGPL/GPL 仓库的代码或衍生表达并入。协议常量（URL、Header 名、错误码、事件名）属于事实，可独立重新表达。

## 4. 能力矩阵（研究结论，不代表已实现）

| Provider | 模型推理 | SSE | 工具调用 | 原生会话 | 额度查询 | Agent 任务 | 认证 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CodeArts CN | 是 | 是 | 是（OpenAI 形状） | 会话头 + chat_id | 有，语义随端点不同 | 未见独立任务 API | STS 临时 AK/SK + DPoP |
| Qoder CN/Intl | 是 | 是（COSY 信封） | 是 | session_id/chat_record_id | 有（OpenAPI） | 否 | COSY 签名 |
| TRAE CN/Intl | 是 | 是（命名事件） | 是 | 客户端历史，无稳定会话头 | 区域不同 | 否 | Cloud-IDE-JWT |
| WorkBuddy CN/Intl | 是 | 是（OpenAI 形状） | 是 | 会话头族 | 有（积分） | 否 | Bearer + 企业/域头 |
| CodeBuddy CN/Intl | 是 | 是 | 是 | 会话头族 | 有 | 否 | Bearer + 企业/域头 |
| ZCode | 是 | 是（Anthropic SSE） | 是 | 上游机制未完全验证 | 有轮询 | 否 | 平台 Token |

所有真实 Provider 当前的验证状态均为 **Experimental / Unavailable**（未实测），不得写成 Verified。

## 5. 协议要点（已观察事实）

### CodeArts
- 端点：CN `https://snap-access.cn-north-4.myhuaweicloud.com`，国际站 `snap-access.ap-southeast-1...`；聊天 `POST /api/v2/chat/completions`；模型 `GET /v1/model/builtin`（`Agent-Type: PromptCenter`）；额度 `GET /snap-manager/v1/statistics/plugin`（必须带 `/snap-manager` 前缀）。
- 认证：STS 临时 `access_key_id/secret_access_key/security_token/expiration` + `refresh_token` + `client_id` + P-256 DPoP 私钥 + `domain_id/uid`；请求签名 `SDK-HMAC-SHA256`，`X-Security-Token`。refresh_token 一次性。
- 会话：`x-ot-session-id` / `user-session-id` 头；本地 Go 实现使用 3 slot 上限与 `CODEARTS_SESSION_SLOTS`。
- 错误：HTTP 402 / `14018` 额度；`1005` 套餐；`11102` 模型不可用；`APIG.0602` 软限流；401/403 凭据失效；`TM.00001041`/并发文本 并发超限。
- 未知：额度单位（点数 vs token）两种实现不一致，必须保持 Unknown/按端点标注。

### Qoder
- 端点：CN `gateway.qoder.com.cn` / `openapi.qoder.com.cn`；Intl `api3.qoder.sh` / `openapi.qoder.sh`（api1/api2 备选）。
- 认证：私有 COSY：随机 AES-128 密钥加密身份 JSON，RSA 包装密钥，`Authorization: Bearer COSY.<payload>.<sig>`；另有 `Cosy-*` 头族。自定义 base64/字符置换编码。
- 聊天：`POST /algo/api/v2/service/pro/sse/agent_chat_generation?...`；SSE 为普通 `data:` JSON（无 event 名），可能带内错误。
- 额度：`GET /sash/api/v2/me/usage`（备选 `/api/v2/quota/usage`），`user_quota` + `add_on_quota`。
- 未知：服务端对设备/版本头的校验强度；国际站活动可用性。

### TRAE
- 端点：CN chat `https://trae-api-cn.mchost.guru`，Intl chat `https://coresg-normal.trae.ai`；模型 `/api/ide/v1/get_detail_param`；聊天 `/api/agent/v3/llm_utils_chat`。
- 认证：`Authorization: Cloud-IDE-JWT <token>` + `X-Ide-Token`/`X-Cloudide-Token`，设备/版本头族，`x-app-id`。
- 流式：命名事件 `request_wait_in_queue/progress_notice/token_usage/done`，data 内含 `response`/`reasoning_content`/`tool_calls`。
- 错误：402→额度，429→软限流，`4290-4299`→429，`≥5000`→502；`4022` 上下文超限。
- 区域额度接口不同（CN 签到、Intl pay-status）。

### WorkBuddy / CodeBuddy
- 端点：CN `https://copilot.tencent.com`（计费 `www.codebuddy.cn`）；Intl `https://www.workbuddy.ai` / `www.codebuddy.ai`（Token 不互通）。聊天 `POST /v2/chat/completions`；CN 模型 `/v2/enterprises/{personal|eid}/models`；Intl `/v3/config`；额度 `/v2/billing/meter/get-user-resource`。
- 认证：`Authorization: Bearer` + `X-User-Id`/`X-Enterprise-Id`/`X-Tenant-Id`/`X-Domain`；客户端 UA/`X-Product: SaaS`/会话头族。
- 错误分类必须分开：会话失效（12153/3004/401/402）、账号/授权（11140/11142/14015-14017）、额度（14001/14012-14014/14018/14019）、限流（6000-6007/6020-6022/14003）、渠道限制（11128）。**不得把授权拒绝误判为普通 429**。
- 限制：`X-Device-Token` 由腾讯 Turing Shield SDK 生成，本项目**不实现**该生成/指纹逻辑；仅允许运维显式配置静态头，缺失时按 Experimental 降级。

### ZCode（AGPL，仅事实）
- Anthropic Messages 直通：`POST /v1/messages`、`GET /v1/models`；`anthropic-version: 2023-06-01`、`X-ZCode-App-Version`、`X-ZCode-Agent`。
- SQLite 账号、额度轮询、按列表轮换。
- **禁止迁移**：阿里云验证码求解、`X-Aliyun-Captcha-Verify-Param` 设备指纹流程。

## 6. 下游兼容与明确排除

- 下游目标客户端：OpenCode、Claude Code、Cline、Codex（Responses）。需要 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三套入口与 SSE。
- **OpenCode Zen 不作为上游 Provider**：不实现其账号、模型、额度、路由、认证。OpenCode 仅作为下游客户端。
- 不实现：验证码破解、批量注册、设备指纹伪造、客户端身份冒充、封禁后自动换号、绕过平台配额/风控、未授权凭据共享。

## 7. 尚未研究 / 未知项

- Magpie 的预算预留与 steady-state 调度只做了摘要，未细读全部实现。
- 参考仓库的测试未运行；无真实账号，全部 Provider 协议只做了源码级核对。
- CodeArts 国际站是否真实可用、Qoder/TRAE 国际站是否与 CN 共用账号体系：未知。
- WorkBuddy/CodeBuddy 积分是否真正共享：未知，Quota Group 只能配置，不能假定。

## 8. 下一步

1. 冻结技术选型与许可证（建议 MIT）。
2. 建立 Provider Trait 与 Mock Provider，先打通协议与调度测试。
3. 调度器测试（公平性/粘性/并发/持久化）优先于真实 Provider。
4. 逐个 Provider 实现，状态初始为 Experimental，只有真实账号测试通过后才标记 Verified。

---

# 9. Coding2API 专项源码审计（2026-10-28）

> 仓库：`.refs/RobbsLuo_Coding2API`，版本 `0.10.0`，Python ≥3.12。
> 许可证：MIT（`LICENSE`）；`NOTICE` 声明从零实现并参考了 codebuddy2api、trae2api-web、workbuddy-auto-signin、workbuddy2api 等 MIT 项目。
> 规模：`src/` 约 41,868 行 Python；`tests/` 273 个测试文件（**本次未运行**）。
> 本审计只读源码，未复制代码，未调用任何上游接口。

## 9.1 技术栈与运行形态

| 项 | 内容 |
| --- | --- |
| Web 框架 | FastAPI + Uvicorn |
| HTTP 客户端 | httpx（含 SOCKS 代理） |
| 配置 | pydantic-settings，环境变量 + `runtime_settings` 热更表 |
| 存储 | SQLite（WAL），手写 SQL + 轻量迁移 |
| 凭据加密 | `cryptography` Fernet（AES-128-CBC + HMAC），密钥由 `APP_SECRET` SHA-256 派生 |
| 前端 | `web/` Vite（独立构建） |
| 上游范围 | codebuddy、codebuddy_intl、trae、qoder、codearts、officeace、zen、kilo |
| Agent 能力 | **无独立 Agent Task API**；只有 chat/completions 形状的推理。OfficeAce 等也走 chat。 |

## 9.2 十项功能审计矩阵

| # | 功能 | 主要源码 | 实现方式 | 外部依赖 | Rust 迁移难度 | 是否值得迁移 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Provider 注册/初始化 | `src/main.py:96-117, 360-476`；`src/provider/base.py:707-733` | 硬编码 dict + `Provider` Protocol；启动时一次装配，无插件发现 | FastAPI/httpx | 中 | 是（改为注册表 + trait object） |
| 2 | Provider 配置 | `src/config.py:18-120, 240-276` | 全局扁平 Settings；每渠道 endpoint/proxy/节流独立字段 | pydantic-settings | 低 | 是（改为每实例配置 + 校验） |
| 3 | 账号管理 | `src/db/repo.py:206-810`；`src/db/schema.sql:42-90` | SQLite `credentials` 表；启用/硬禁用/pin/健康/冷却/额度列；凭证级限额表 | SQLite | 中 | 是 |
| 4 | 授权凭据 | `src/db/crypto.py`；`src/provider/*/credential.py`；`refresh.py` | Fernet 加密；各 Provider 自解析/自刷新；弱密钥拒绝启动 | cryptography | 低-中 | 是（换 AES-256-GCM） |
| 5 | 模型列表/映射 | `src/engine/model_resolver.py`；`src/provider/naming.py`；`src/api/models.py`；`src/api/model_merge.py` | `model@provider` 解析；归一键/展示名；运行时别名表；动态目录合并 | 无额外 | 中 | 是（保留 raw_id + alias） |
| 6 | OpenAI/Anthropic 兼容 | `src/compat/openai/*`；`src/compat/anthropic/*`；`src/compat/responses/*` | 入站转内部 ChatRequest；出站翻译三种协议 | 无额外 | 中-高 | 是（协议层核心） |
| 7 | SSE | `src/engine/sse.py`；`src/api/streaming.py`；各 `*/events.py` | 增量 UTF-8 解码 + 帧状态机；15s 保活；三方终止语义 | 无额外 | 中 | 是 |
| 8 | Tool Calling | `src/compat/openai/request.py`；`openai_chunk.py`；`dsml.py`；Anthropic/Responses 解析器 | 工具定义转换、增量参数、坏 JSON fail-closed、DSML 回读 | 无额外 | 高 | 是（兼容性关键） |
| 9 | 错误处理 | `src/provider/base.py:26-120`；`src/engine/scheduler.py:1-120, 300-430`；各 `events.py` | 中立 `ErrKind`；账号级/模型级冷却分离；半开、排队、轮换预算 | 无额外 | 高 | 是（最有价值的部分） |
| 10 | 数据存储 | `src/db/schema.sql`；`src/db/repo.py:1539 行`；`src/db/migrate.py` | SQLite WAL；凭据/用量/小时聚合/冷却/审计/运行时设置 | SQLite | 中 | 是（精简为网关所需表） |

## 9.3 Provider 注册与初始化

- 事实：没有自动发现或 entry point。`build_app()` 内直接构造 8 个 Provider 实例放入 dict（`src/main.py:430-476`）。每个 Provider 按需注入 `httpx` client、出站代理和 pacer（渠道级节流）。
- 国内/国际站是**两个独立 Provider id**：`codebuddy` 与 `codebuddy_intl` 共用一个客户端实现，但 endpoint 与装配参数不同；`codebuddy_intl` 额外开启 `require_leading_system=True`（`src/main.py:439-446`）。
- Provider 协议是 Python `Protocol`（结构化鸭子类型），方法分三类：凭证生命周期、额度探测、执行与分类（`base.py:707-733`）。没有显式能力位；能力由“方法是否存在/是否返回 None”隐式表达（如 Intl 把 `growth/checkin` 置 `None`）。
- `aclose()` 统一关闭连接池；`LazyClients` 延迟创建长/短连接（`src/provider/lazy_clients.py`）。
- **Rust 迁移**：用 `Arc<dyn Provider>` 注册表 + 显式 `Capabilities`，比 Protocol 更可检查；不要把“方法存在”当能力。难度中，收益高。

## 9.4 Provider 配置结构

- 事实：单一 `Settings` 类（`src/config.py:18`），每渠道的 endpoint、allowed_endpoints、代理、聊天最小间隔、并发上限拆成独立字段（如 `codebuddy_api_endpoint`、`qoder_gateway_endpoint`、`codearts_max_concurrency`、`codebuddy_chat_min_interval`）。
- 渠道级 `allowed_endpoints` 用于防误配；`provider_proxies` 是启动期解析的字符串（`src/provider/proxy.py`）。
- 运行期可热更的是行为参数（TTL、窗口、节流），不是 endpoint/连接池。
- **缺口**：没有“每个 Provider 可独立启用/禁用”的顶层开关；启用由渠道账号是否存在决定。没有 Region 维度，CN/Intl 通过不同 Provider id 表达。
- **Rust 迁移**：低难度。建议 `[[provider]]` 数组 + `kind/region/id/enabled`，每实例独立 endpoint/options/quota_group；启动时校验唯一 id 与 URL。

## 9.5 账号管理

- 事实：账号 = `credentials` 行（`schema.sql:42-69`）。字段包括：provider、昵称、加密 data、软开关 `enabled`、硬禁用 `disabled` + 原因、`pinned`、健康度、冷却截止、错误计数、额度字段、token 到期、标签。
- 候选过滤在 `CredentialRepository.candidates()`（`repo.py:566-605`）：启用且未硬禁用、未冷却、凭证级护栏未触顶。
- 调度选号在 `Scheduler.select()`（`scheduler.py:265-310`）：免费优先 → pin → 在途最少 → 36h 内到期额度 → 7d 内到期额度 → 健康度 → 余额 → id。**不是**轮询，而是多指标排序；并发靠“在途最少”分摊。
- 凭证级护栏独立表 `credential_limits`（keep_credits、daily_tokens、daily_credits、per_model_tokens）。
- 模型级冷却独立表 `credential_model_cooldowns`（6004/11102）。
- **缺口（相对本项目）**：没有 Quota Group、没有会话表、没有 Provider 级并发门；会话粘性仅进程内。
- **Rust 迁移**：中难度。SQLite 表可直接重新设计；调度需要补 SWRR、会话粘性、Provider 并发与配额组。

## 9.6 授权凭据处理

- 事实：`CredentialCipher` 用 `APP_SECRET` 做无盐 SHA-256，再交给 Fernet；拒绝短于 16 字符或低熵的密钥（`src/db/crypto.py:25-76`）。密钥丢失 = 凭证不可解，无轮换。
- 各 Provider 自管凭据：`credential.py` 归一字段；`refresh.py` 刷新；CodeBuddy 手动 bearer 不刷新，仅 OAuth 刷新（`codebuddy/credential.py:66-88`）。
- 刷新旋转（refresh token rotation）要求先持久化新 token 再继续；否则并发刷新会丢代次。
- **Rust 迁移**：低-中。改用 AES-256-GCM + 随机 nonce + 主密钥文件/环境变量；刷新状态机沿用“先落库后使用”。

## 9.7 模型列表与模型映射

- 事实：模型名解析支持扁平名与 `model@provider` 强制路由（`model_resolver.py:63-86`）；未知 provider 400。
- 三字段口径（`provider/naming.py:1-100`）：`raw_id` 永不改；`normalize_model_key` 去噪声/厂商前缀做合并键；`display_name` 做人读名；显式 `MODEL_SYNONYMS` 处理同一模型的异名（如 `deepseek-flash` → `deepseek-v4.1-flash`）。
- 运行时 `model_aliases: provider -> {小写统一名: 上游 raw id}`，由 `/v1/models` 拉取后更新（`main.py:480-485`；`api/models.py:190-225`）。隐藏/探活判死的模型也会补登记别名，避免请求扇出到不认该模型的上游。
- `/v1/models` 合并多 Provider，带能力/倍率等元数据；成本价表来自 OpenRouter 刊例价快照（非必须迁移）。
- **Rust 迁移**：中难度。需独立 `ModelRegistry`（统一 id、aliases、raw_id、capabilities）与 Provider 发现结果合并；不要沿用“运行时共享可变 dict”的隐式耦合。

## 9.8 OpenAI / Anthropic / Responses 兼容

- 三个入口共用同一执行器，只换入站解析与出站翻译（`api/chat.py`、`api/messages.py`、`api/responses.py`）。
- OpenAI Chat：只允许 `n=1`；校验 temperature/top_p 等边界；未知/不支持字段显式 400，而不是静默透传（`compat/openai/request.py`）。
- Anthropic：`system`（字符串或 text 块）→ system 消息；`tool_use` → assistant `tool_calls`；`tool_result` → `role:"tool"`；`thinking` → reasoning；`cache_control` 结构校验并透传；`/v1/messages/count_tokens` 本地估算（`compat/anthropic/request.py`；`api/messages.py`）。
- Responses：`instructions` → system；`input` 支持 message/function_call/function_call_output/reasoning；仅 function 工具；`store=true`、`previous_response_id`、`background=true`、`input_file`、未知 include 显式 400（`compat/responses/request.py`）。
- 出站事件序列与官方一致；各有终止语义：Chat 用 `[DONE]`，Anthropic 用 `message_stop`，Responses 用 `response.completed/incomplete/failed`（`compat/*/response.py`）。
- **Rust 迁移**：中-高。结构清晰，但字段与事件细节多；必须用抓包/客户端实测补齐，不能只测纯文本。

## 9.9 SSE 流式实现

- 事实：`engine/sse.py` 是跨 Provider 的帧解析器：
  - 增量 UTF-8 解码，避免多字节字符被块边界切断；
  - 支持 `event:`、多行 `data:`、注释心跳、空行成帧、末尾无空行 flush；
  - 单行/单帧 8 MiB 上限，超限抛错终止，不静默截断。
- `api/streaming.py` 的保活设计值得迁移：
  - 15s 无输出插入 SSE 注释帧（Anthropic 用具名 `ping`）；
  - 取下一帧用**可复用 Task** 与定时器竞争，超时只发心跳，不取消上游读取（避免“保活把流关了”）；
  - 非流式慢请求先等宽限期，超时才提交 200 + chunked，并允许前导空白保活；
  - 200 已提交后上游失败，写一个可解析的 JSON 错误体，而不是截断。
- CodeBuddy 强制上游 `stream=true`，非流式由网关聚合；`stream_options.include_usage=true`。
- **Rust 迁移**：中难度。用 `bytes::Bytes` + 手写增量解码/行缓冲，禁止假设“一次 chunk = 一帧”；保活用 `tokio::select!` 与 kept future，不要取消上游流。

## 9.10 Tool Calling

- OpenAI：工具定义从 `{type:function, function:{...}}` 进入内部；历史工具调用与结果校验；工具参数增量按 index 合并；缺失 index 时补稳定 index，不重生成已有 id（`compat/openai/response.py:60-120`；`provider/openai_chunk.py`）。
- DSML 回读：部分上游把工具调用写成文本标记，`compat/openai/dsml.py` 在流结束后回读并改写 `finish_reason=stop → tool_calls`；这是兼容性补丁，需明确是否迁移。
- 坏参数 fail-closed：`InvalidToolArguments` 不把半截 JSON 交给客户端，也不冷却账号（`compat/openai/errors.py:1-30`）。
- Anthropic：`tool_use` 块 + `input_json_delta`；`tool_result` 转 tool 消息；thinking 块映射。
- Responses：`function_call` / `function_call_output` 双向映射；流式 `response.function_call_arguments.delta/done`。
- **Rust 迁移**：高难度。必须为增量参数、乱序/缺失 index、坏 JSON、工具调用跨轮回放写专门测试。

## 9.11 错误处理（最有迁移价值）

- `ErrKind` 不是简单 HTTP 映射，而是**冷却作用域**（`base.py:26-120`）：
  - 账号级：PLAN（12h）、CREDIT（到次日 04:00）、SOFT（60s）、DEAD（硬禁用）、RESTRICTED（2h，且当前请求不换号）、OTHER（3 次→10m）。
  - 模型级：MODEL（600s 起翻倍，封顶 2h）、BLOCKED（6h 起，封顶 24h）、CONCURRENCY（固定 60s）。
  - 请求级：INVALID（不罚号）、REQUEST（不罚号但换号）、QUEUE（原号等待，不占轮换预算）。
- 账号级冷却出现时清空模型级条目，防止“换模型绕过冷却”。
- `Retry-After` 支持整数秒、HTTP-date、文本（`Try again in 17h 59m`），并双端钳制（不因坏头瞬间打爆全部凭证，也不接受数月冷却）。
- 轮换预算按“实际尝试的凭证数”计算，而不是 `tried` 集合大小；INVALID 会跳过整个 Provider 但不占预算。
- 流内业务错误（HTTP 200 里的 error）也参与分类与轮换（CodeArts 正是这种）。
- **Rust 迁移**：高价值、高难度。建议照搬“分类 → 作用域 → 冷却”的思想，不照搬常量；必须区分授权拒绝、渠道限制、额度、限流、模型级与请求级。

## 9.12 数据存储

- SQLite WAL；表：`api_keys`、`credentials`、`credential_limits`、`credential_model_cooldowns`、`model_probes`、`usage_events`、`usage_hourly`、`growth_events`、`credit_events`、`runtime_settings`、`users`、审计表。
- 凭据密文 BLOB；额度包明细/到期阶梯用 JSON 文本存列；小时聚合表预聚合请求/token/credit/成本/延迟。
- `api_keys` 存 SHA-256 digest + preview；支持 provider 绑定、IP/模型白名单、到期、Key 级配额 JSON。
- 管理端会话是无状态 HMAC Cookie + `session_epoch` 吊销；用户/角色在 DB。
- **Rust 迁移**：中难度。建议保留：账号、凭据、会话、额度快照、用量明细/小时聚合、API Key、审计、设置；删除/推迟：成长中心、签到、成本价表、模型探活。

## 9.13 【CodeBuddy】专项（历史研究，已移除）

> **2026-10-29：CodeBuddy 已从本项目范围移除。** 本节仅保留历史审计，不构成实现计划。

**文件**：`src/provider/codebuddy/` 共 10 个模块（client 910 行、credential 137、headers 143、events 143、intl 84、oauth 211、refresh 236、activity/checkin/growth 若干）。

**注册**：`main.py:434-446` 注册 `codebuddy` 与 `codebuddy_intl` 两个 Provider；同一客户端实现，不同 endpoint；国际站 `require_leading_system=True`。

**端点**（`headers.py:25-36`）：

| 用途 | 路径 |
| --- | --- |
| 聊天 | `POST /v2/chat/completions` |
| 模型配置 | `GET /v3/config` |
| 个人额度 | `POST /v2/billing/meter/get-user-resource` |
| 企业额度 | `GET /v2/billing/meter/get-enterprise-user-usage` |
| Token 刷新 | `POST /v2/plugin/auth/token/refresh` |
| OAuth | `/v2/plugin/auth/state`、`/v2/plugin/auth/token`、`/v2/plugin/login/account` |

**凭据**（`credential.py`）：`bearer_token`、`user_id`、`account_uid`、`domain`、`enterprise_id`、`department_full_name`、`refresh_token`、`expires_at`、`auth_source(manual|oauth)`、`quota_probe_mode(personal|enterprise)`。手动 bearer 永不刷新；OAuth 才刷新。domain 必须是合法主机名（会进入 Host/X-Domain 头）。到期时间缺失时回落到 JWT `exp`。

**请求头**（`headers.py:67-143`）：`Authorization: Bearer`、`X-User-Id`（account_uid 优先）、`X-Enterprise-Id` + `X-Tenant-Id`、`X-Domain` 与 `Host` 必须由同一 endpoint 派生、部门名 UTF-8 百分号编码、`User-Agent: CLI/2.107.0 CodeBuddy/2.107.0`、`X-Product: SaaS`、`X-Requested-With`、`X-CodeBuddy-Request`、会话头族 `X-Conversation-ID/Request-ID/Message-ID`、`X-Agent-*`。另有 `x-stainless-*` 伪装 OpenAI JS SDK 的头 **属于客户端身份伪装，本项目不迁移**。

**请求改写**（`client.py:232-290`）：
- 强制 `stream=true`，`stream_options.include_usage=true`；
- `developer` 角色 → `system`（上游不认 developer，会触发 11128）；
- 国际站强制首条 system（中性最小句，不覆盖客户端已有 system）；
- 缺省 `enable_thinking=true`、`reasoning_effort=medium`（客户端显式值不覆盖）；
- 清理孤儿 `tool_calls` / `role:tool`（避免上游拒绝）；
- 中和正文里的“伪装客户端指纹”以防 11128 内容风控（**本项目不迁移该逃避风控逻辑**）。

**模型**（`client.py:371-440`）：`/v3/config` 动态拉取，10 分钟缓存，失败回退 `glm-5.2`/`deepseek-v4-pro`；国际站额外按套餐过滤（`filter_available_models`，探针失败不误删）。

**额度**（`client.py:294-370`）：个人额度逐套餐累计 `CycleCapacitySize/CycleCapacityRemain`（Precise 优先），不使用 `TotalDosage`（上周期口径，实时性错误）；企业额度用 `credit/limitNum`。到期阶梯与套餐明细分开存。单位是**积分**，不是 token。

**错误分类**（`events.py:25-143`）：

| 上游信号 | 分类 | 作用域/动作 |
| --- | --- | --- |
| 402 / 14018 | CREDIT | 冷却到次日 04:00 |
| 1005 | PLAN | 12h |
| 400/404 + 11102 | BLOCKED | (账号, 模型) 负缓存 |
| 11140（账号异常/受限） | RESTRICTED | 账号级 2h，**当前请求不再换号** |
| 401/403 | DEAD | 硬禁用 |
| 404 | SOFT | 60s，不累计 |
| 429 + 6004/14003 | MODEL | 只冷却该模型 |
| 429 其他 | SOFT | 账号级 60s |
| 400 + 11101/11115/11128/11135 | REQUEST | 不罚号，但换号 |
| 400 其他 | INVALID | 不冷却，直接 400 |

> **关键洞察**：11128（“Illegal API invocation from an unapproved channel”）被 Coding2API 归为**请求级**（窗口型瞬时拦截，换号无效，不惩罚账号），不是 429、也不是账号失效。11140 才是账号受限。本项目必须保留这种区分，避免“授权拒绝 / 渠道限制 / 429”互相误判。

**会话**：CodeBuddy 每次请求生成新的 `X-Conversation-*`（源码注释明确 stable id 不需要）；会话粘性完全由网关的 `ConversationAffinity` 负责，不做上游原生会话恢复。因此本项目不应宣称 CodeBuddy 具备原生会话恢复。

**迁移结论（CodeBuddy）**：协议端点、字段、错误分类、额度口径值得迁移；OAuth 登录、签到/成长中心可推迟（不属于网关核心）；设备指纹/Turing Token/伪装 SDK 头**不迁移**。难度中-高，主要成本在请求头兼容与流式/错误细节，而非业务逻辑。

## 9.14 迁移清单（Coding2API → Rust）

**建议迁移（独立实现）**

1. 三协议入站/出站映射与终止语义（Chat/Responses/Anthropic）。
2. SSE 帧状态机、增量 UTF-8、帧大小上限、保活不取消上游、迟到失败错误体。
3. `ErrKind` 的分类与作用域思想（账号级/模型级/请求级/排队），轮换预算按实际尝试数。
4. 会话粘性优先级：body 显式键 > 会话头 > 消息前缀指纹；用户名参与哈希；TTL + LRU。
5. 模型三字段口径（raw_id / normalized / display）与别名表。
6. CodeBuddy 端点、凭据字段、错误码、额度逐包口径。
7. SQLite 账号/用量/小时聚合/审计表结构（可精简）。
8. API Key SHA-256 + preview + 最后使用节流；管理会话 HMAC + epoch 吊销。

**不迁移或需改造**

- OpenCode Zen / Kilo：本项目明确排除 Zen；Kilo 不在 Provider 范围。
- 签到、成长中心、抽奖、活动上报：非网关核心，涉及不可逆权益操作。
- 成本价表/汇率：可选，不作为额度真值。
- `x-stainless-*`、UA/IDE 伪装、Turing/设备令牌、验证码：违反“客户端身份冒充/设备指纹伪造”限制。
- 无盐 SHA-256 派生 Fernet 密钥：改为 AES-256-GCM + 主密钥文件/环境变量。
- 进程内 affinity、无会话表：改为 SQLite 持久化 + 重启恢复。
- 扁平全局配置：改为 per-provider 配置 + 独立启停 + Region。

**风险**

- Coding2API 为 MIT，但仍应独立实现；仅协议常量与行为事实可用。
- 其 `NOTICE` 还引用多个 MIT 项目，若未来复制这些项目的表达，需要保留版权声明。
- 本次未运行其测试，不能引用其覆盖率作为本项目的验证。

## 10. CodeArts 账号池专项审计（2026-10-28）

### 10.0 审计对象与结论摘要

本节区分源码实际行为、按源码推演的行为和未验证事项。本次只做静态源码审计：未运行两个工程的测试，未启动服务，未读取本地 Go 工程的 `.env`，未调用真实上游。

| 对象 | 状态 | 许可证/提交状态 | 审计作用 |
| --- | --- | --- | --- |
| `.refs/Johnsheng1_codearts2api` | Python Flask 单进程代理 | MIT；浅克隆 HEAD `d813ca0`，2026-10-22 | 签名、模型同步、Benefit 额度、OpenAI SSE 参考；**没有账号池** |
| `~/codearts2api` | Go 多账号网关 | 无 LICENSE 文件；**操作者本人作品**（2026-10-29 确认）；HEAD `f9cb52c`，2026-10-15 | 本地账号池实现对照；代码可复用（作者授权） |

结论：Johnsheng1 版本只从环境变量读取一组 `CODEARTS_AK` / `CODEARTS_SK`，没有多账号配置、账号选择、会话绑定、账号健康状态或跨账号故障转移。本地 Go 版本实现了多账号文件存储、`round_robin` / `random` / `weighted` 路由、账号级与模型级冷却、请求重试、流式首字节前故障转移、会话元数据持久化和 keepalive 探测；但它仍不是本项目目标所需的完整公平调度器：并发租约主要是进程内状态，会话槽是全局而非每账号，额度只展示不参与路由，没有会话 TTL/清理，并发突发时没有真正的每账号排队，JSON 文件读改写可能丢失统计更新。

另一个重要差异：Go README 声称同一会话复用稳定的 CodeArts `chat_id`；当前代码的 `streamSingle` 每次从全局 `sessionSlots` 取槽 ID 并覆盖请求中的 `ChatID`。因此实际稳定的是账号偏好和网关会话元数据，不是每个下游会话独占的上游会话 ID。

### 10.1 核心源码路径

#### Johnsheng1 Python 版本

| 功能 | 源码路径 | 实际内容 |
| --- | --- | --- |
| 单账号凭据 | `.refs/Johnsheng1_codearts2api/server.py:30-33` | 启动时读取一组 AK/SK；缺失即退出 |
| SDK-HMAC 签名 | `server.py:73-161` | 使用全局 AK/SK 对请求签名，没有按账号传入凭据的抽象 |
| 模型发现与缓存 | `server.py:164-349` | AgentCenter、opengw Benefit 模型、JSON 模型缓存和兜底列表 |
| OpenAI Chat | `server.py:536-703` | 单账号上游请求，没有候选账号列表 |
| SSE | `server.py:359-441`、`606-688` | 聚合 SSE 为非流式响应，或转发并补齐部分 OpenAI SSE 结束语义 |
| 业务错误 | `server.py:390-398`、`588-604` | 识别部分 HTTP 200 业务错误；HTTP 非 200 转成通用 502 |
| Benefit 额度/签到 | `benefit.py:20-47`、`80-104` | 查询免费额度余额、签到状态、模型；独立脚本，不是池调度器 |

#### 本地 Go 版本

| 功能 | 源码路径 | 实际内容 |
| --- | --- | --- |
| 账号池与选择 | `internal/auth/pool.go:18-28`、`55-203` | 路由策略、候选账号过滤、并发计数、preferred account、权重 |
| 账号结构与存储 | `internal/auth/manager.go:59-103`、`438-466`、`746-864` | 账号记录、秘密字段保护、单账号文件读写、列表和排序 |
| 凭据刷新 | `internal/auth/manager.go:866-941` | 按每个账号独立刷新临期 OAuth 凭据 |
| 调度、重试、上游 SSE | `internal/codearts/client.go:80-102`、`143-258`、`307-513` | 槽位、候选账号尝试、SDK 请求和 SSE 解析 |
| 上游会话槽 | `internal/codearts/client.go:39-43`、`96-102`、`525-548` | 全局固定数量的确定性槽 ID；识别 TM.00001041 |
| 网关会话 | `internal/conversation/store.go:17-89`、`91-169` | 会话元数据持久化、显式 ID 和历史哈希恢复 |
| API 会话/流式 | `internal/server/server.go:612-773` | 请求解析、账号偏好、Chat/SSE 响应和会话保存 |
| 模型目录 | `internal/catalog/catalog.go:17-27`、`58-204` | 按账号发现、跨账号合并和短期缓存 |
| 额度目录 | `internal/subscription/subscription.go:15-161` | 查询套餐用量、模型指标、跨账号汇总和缓存 |
| 健康探测 | `internal/keepalive/service.go:29-197` | 定期探测、OAuth 刷新尝试和健康状态记录 |
| 配置默认值 | `internal/config/config.go:95-159` | CN/Intl profile、路由、并发、重试、冷却、缓存配置 |
| 路由/状态 API | `internal/server/server.go:424-555`、`837-847` | 手动探测、路由策略读写及下游状态映射 |

### 10.2 CodeArts 账号数据结构与多账号配置

**Go 版本实际支持。** `AccountRecord` 包含 `ID`、`Environment`、AK/SK、`SecurityToken`、到期时间、`RefreshToken`、`DomainID`、用户/主体信息，以及 `Stats`、账号级 `Health`、`ModelHealth` 和 `UpdatedAt`（`internal/auth/manager.go:59-77`）。账号以独立 JSON 文件保存；可保护的秘密字段放入 `Protected`，公开元数据、统计和健康状态留在文件外层（`manager.go:88-103,438-466`）。Windows 使用当前用户 DPAPI，Linux 使用 AES-GCM（实现位于 `internal/securestore/`）。账号按环境、域和用户身份去重；每个账号单独刷新凭据。默认账号目录位于平台数据目录下，支持读取旧数据目录（`manager.go:560-582,746-864`）。

多账号不是一个逗号分隔的 AK/SK 配置项，而是 OAuth/凭据导入产生的多个账号文件。每条记录的 `Environment` 决定 CN 或 International endpoint（`internal/config/config.go:19-31,173-186`）。列表按 `UpdatedAt` 新到旧排序（`manager.go:823-864`）。

**Johnsheng1 Python 版不支持。** `server.py:30-33` 将 AK/SK 读入全局变量；无账号列表、账号 ID、独立凭据存储或多区域 per-account 配置。其 `.env.example` 也只有单组 AK/SK。

**存储限制。** Go 版本虽有原子文件替换，但统计/健康更新是读取记录、修改 map、再保存整文件；跨请求没有每账号文件锁或数据库事务，因此并发更新可能互相覆盖。原子 rename 防止半写文件，不等于读改写事务。

### 10.3 账号选择算法与负载均衡

Go 版本 `Pool.Candidates` 的实际顺序是：

1. 读取账号，要求凭据存在、账号未冷却、未标记 `needs_relogin`、未达到全局 `AccountMaxConcurrency`，且请求模型未处于模型级冷却（`pool.go:111-203`）。
2. 按账号 `UpdatedAt` 从新到旧稳定排序。
3. 若存在会话 preferred account，将其移到候选列表最前；但只有该账号仍可用时才保留。
4. 选择一种路由顺序：`round_robin` 旋转候选列表游标；`random` 随机打乱；`weighted` 按当前权重随机抽取一个顺序（无放回），见 `pool.go:55-80,129-185,408-466`。
5. 请求最多取前 `RoutingMaxAttempts` 个候选（默认 3）；默认值见 `config/config.go:137-145`。

`weighted` 不是用户配置的账号权重，也不是确定性公平队列。每个账号的抽样权重由以下因素相乘得到：并发数、连续失败数、模型连续失败数、历史失败率、平均延迟的倒数惩罚，以及最长约 50% 的空闲奖励；最低权重钳制为 0.001（`pool.go:433-466`）。

默认 `round_robin` 的游标保存在进程内，启动后从 0 开始；候选列表又会因 `UpdatedAt` 排序，而每个请求的统计/健康写入会更新该时间戳。因此它不是独立于账号状态的严格轮询计数器。明确传 `X-CodeArts-Account` 时只选指定账号，不做跨账号故障转移（`server.go:649`、`client.go:242-244`）。

### 10.4 两个账号的 100 个新会话分配

这里的“新会话”指首次请求时没有显式会话 ID、也没有可匹配历史哈希的请求；账号是在首次请求路由时选定，成功后才写入会话上下文。

**顺序请求，默认 `round_robin`：**按代码推演，假定 A/B 都始终健康、请求均成功、每次更新 `UpdatedAt`。若初始 A 更新，候选排序与游标交互会出现近似 `A, B, B, A, A, B, B, A...` 的成对突发；100 次结束时可为 50:50，但它不是严格的 A/B 交替或对任意前缀都公平。初始排序相反时模式镜像。该分配是源码推演，不是运行测试结果；现有测试没有 100 会话分布验收。

**100 个并发新请求：**默认每账号 `AccountMaxConcurrency=4`，所以 A/B 最多各持有 4 个并发请求租约；超过两账号容量的请求不会在账号租约处排队。另有默认 `SessionSlots=2` 的全局槽通道，只有 2 个请求能同时进入上游流式调用，其余已获账号租约的请求会等槽位。初始突发时最多约 8 个请求可持有账号租约（其中 2 个运行、最多 6 个等全局槽）；后续请求在 `Begin` 失败后尝试下一账号，全部满时通常以 `retry_budget`/503 失败，而不是进入账号级等待队列。精确并发结果受到达时序影响。

因此，Go 版本对**顺序 100 次**有近似均分的轮询效果，但对**100 路并发**既没有可证明的公平分配，也没有目标系统所需的容量排队保证。`weighted` 模式是随机权重抽样，不保证 50:50。Johnsheng1 Python 版没有第二个账号，100 个会话全都使用唯一一组 AK/SK，没有分配行为。

### 10.5 会话与账号绑定

Go 版 `conversation.Store.Resolve` 的顺序是：请求头 `X-CodeArts-Conversation` 或 body `conversation_id` 显式 ID；否则去掉最后一条 user 消息后计算此前消息的 SHA-256，并扫描会话文件寻找相同 `HistoryHash`；找不到则创建随机 ID（`internal/conversation/store.go:34-67,107-131,162-169`）。成功后保存 `AccountID`、环境、`ChatID`、轮数、更新时间和含 assistant 回复的历史哈希；不保存提示词/响应正文（`store.go:69-89`）。

后续请求把已保存 `AccountID` 作为 `PreferredAccountID`。账号可用时优先回到原账号；若账号冷却、凭据失效、模型冷却或达到并发上限，则它会从可用候选中消失，其他账号可能接手。成功后会话记录改绑到实际成功的账号。此切换在返回结果中可从 `X-CodeArts-Account` / `codearts_account` 观察，但没有专门的“会话已迁移”事件，也没有为 sticky session 提供等待原账号槽位的队列。

上游 `chat_id` 与下游会话不是一对一关系：`streamSingle` 在 `sessionSlots != nil` 时总是取得一个全局槽 ID 并覆盖 `request.ChatID`（`client.go:343-357`）；客户端初始化总会创建该通道，非正值还会回退为 2（`client.go:80-94`）。因此不同下游会话可能复用同一组槽 ID，某个会话的 `ChatID` 不保证跨请求原样延续。代码每次向上游发送完整消息历史（`client.go:364-369`），当前会话恢复依赖完整历史，而不是依赖上游服务端保存的对话内容。

会话历史哈希查找会遍历整个目录，没有索引；当前实现未见 TTL、过期清理或 LRU。显式 ID 则可直接读文件。Johnsheng1 Python 版没有会话存储或账号粘性。

### 10.6 健康检查、失效与错误恢复

**可用性过滤。** `usable` 排除缺 AK/SK、账号冷却中、`needs_relogin`、账号并发已满和当前模型冷却中的记录（`pool.go:187-203`）。账号级与模型级健康分开保存。

**失败冷却。** `MarkFailure` 将 401/403、`auth`、凭据刷新失败等归为账号级；其他带模型的失败通常归为该模型级。冷却按指数退避增长并封顶；默认 base 15 秒、max 300 秒，认证冷却 120 秒，429 冷却 60 秒，`Retry-After` 可抬高等待时间（`pool.go:338-406`；默认值 `config.go:146-151`）。401/403 还设置 `needs_relogin=true`，因此仅等冷却结束并不足以重新入池。`benefit_cap` 被特判为不冷却模型。

**健康探测。** keepalive 默认启用，后台每 300 秒轮询一次，但单账号仅在从未成功探测或距上次成功探测达到默认 24 小时时才 due；请求 `/v1/model/builtin`，网络错误最多重试一次。遇到 OAuth 认证错误会尝试刷新并再次探测；探测不创建聊天请求，也不消耗模型 token（`keepalive/service.go:29-56,69-116,118-197`）。这是周期性探测，不是连续健康采样或自动半开熔断器。

**重试与故障转移。** 全局有尝试次数/时间预算、每账号网络/超时/5xx 重试、指数退避和 jitter。对未指定账号的请求，符合 retryable 的失败可继续尝试下一个候选；显式账号请求不跨账号。流式请求只允许在首个 delta 发出前换账号；已输出后错误会作为 SSE error 结束，不重放部分内容（`client.go:151-258`；`server.go:712-773`）。`TM.00001041`/“并发会话数已达上限”识别为 `benefit_cap`，同账号等待约 3 秒并有独立重试上限，避免消耗普通失败预算（`client.go:218-230,420-427,525-548`）。

**边界。** 账号认证错误对下游通常映射成 502，而不是原样 401/403；只有 rate limit / benefit cap 映射 429、无账号/预算耗尽映射 503（`server.go:837-847`）。没有通用 half-open/circuit-breaker 状态机；模型/账号冷却的解除主要依赖时间到期、成功请求或探测/重新登录。Johnsheng1 Python 版对非 200 统一返回 502，没有账号级恢复。

### 10.7 账号并发限制

Go 版本的 `Pool.Begin/Finish` 用互斥锁维护进程内 `inFlight` map；默认每账号最多 4 个（`pool.go:83-109`、`config.go:138`）。达到上限时该账号从候选中排除；`Begin` 失败会尝试下一候选，但没有按账号排队、排队超时或公平唤醒机制。正常返回、错误和上下文取消都会从 `streamSingle` 返回后调用 `Finish`；进程崩溃时计数随进程消失。租约没有持久化或 TTL 兜底。

此外，`Client.sessionSlots` 是**整个 Client 共用**的通道，默认 2、最大 3，而代码注释描述的 CodeArts 上游上限是每账号 3 个并发会话。多账号时全局槽会把所有账号合计压到 2–3 个活动 chat 请求，不能达到“每账号最多 3 个”的利用率。槽位等待受请求 context 约束，但没有独立队列超时。

### 10.8 额度查询与路由关系

Go 版 `subscription.Catalog` 按账号调用 `/snap-manager/v1/statistics/plugin`，解析套餐的已用/总 token、套餐名/状态、起止时间和 `model_metrics`；结果按账号缓存，默认 60 秒，可强制刷新（`subscription.go:32-101,103-161`）。汇总时对 available 账号的用量和额度求和。

额度**不参与账号可用性或路由选择**：`Pool.usable` 不读取额度；README 也明确说明套餐用量不作为 weighted 路由因子。因此“额度耗尽”实际由上游响应决定，系统没有在发送请求前排除余额耗尽的账号。额度查询失败时单账号显示 `available:false`；聚合不可用对象会将数字字段置 0，前端必须同时检查 `available`，否则会把未知误显示为 0。Rust 版本应使用 `unknown/error/known` 状态而非用零代替未知。

Python 版 `benefit.py` 查询的是 opengw 免费额度 `total_quota`、`total_balance`、`used_amount`，并可单独执行签到；它不关联多个账号、不为 Chat 请求做余额门控，也不参与路由。README 宣称的“插队”不是账号池能力，不应转化为绕过上游排队/配额控制的实现目标。

### 10.9 模型目录与映射

Go 版 `catalog.List` 遍历当前可用账号，逐账号读取 builtin 模型、最多 6 个 AgentCenter agent detail，以及第三方模型列表；模型按原始 ID 合并，并保留来源和环境列表。每账号结果默认缓存 300 秒；全部发现失败时回退到配置的默认模型/模型列表（`catalog.go:58-164`）。模型元数据包括 display name、context/input/output 长度和 thinking type（`catalog.go:17-27,166-204`）。

这不是规范化 alias 映射表：没有显式 `alias -> raw_id -> provider` 层。`/v1/models` 是跨可用账号的并集，但 Chat handler 只应用默认模型，不检查所选账号是否拥有该模型（`server.go:585-610,618-649`）。因此“模型只在 B 可用，但路由到 A”可能直接得到上游 client error；此类不可重试错误通常不会自动试 B。Rust 版应按账号/区域记录模型可用性、显式 alias 和 capability，并在调度前决定候选资格。

Python 版同时维护 AgentCenter 与 Benefit 模型列表，并通过 `channel` 区分签名/请求头；模型 ID 是直接查表，不是多账号映射。两版都不能证明动态模型能力真实可用，Rust 项目仍需将模型能力标为未知直到实测。

### 10.10 SSE 流式与 Tool Calling

Go 版上游请求固定使用 `stream=true`。解析器逐行读取 SSE，忽略空行、注释和 `event:` 行，接受 `data:` JSON 与 `[DONE]`，扫描器单行上限 4 MiB；只处理第一个 choice，累计 content/reasoning，记录 usage/chat_id，并按 tool index 合并工具名与 arguments 增量（`client.go:439-513,678-731`）。上游 event 错误可转为内部错误；网络断流/JSON 解析错误则终止当前请求（`client.go:735-766`）。

网关流式响应先发 role chunk，随后逐 delta 转发；若缺 finish chunk 则补齐，可按请求选项输出 usage chunk，最后输出 `[DONE]`（`server.go:712-773`）。首个 delta 之后不跨账号重试。非流式接口仍通过同一上游流式过程聚合为完整结果。现有测试覆盖 SSE 解析、工具片段合并和一个两账号故障转移，但没有 100 会话分布、并发公平队列或重启恢复验收（`internal/codearts/client_test.go`、`delta_test.go`、`failover_test.go`、`internal/conversation/store_test.go`）。

Python 版在客户端请求 `stream=true` 时逐帧转发并规范 `finish_reason`、补 `[DONE]`；遇到非流式上游响应则包装为 SSE。非流式客户端请求会将上游 SSE 聚合成 JSON。它没有账号切换，工具调用依赖转发/聚合的 OpenAI-shaped 数据。

### 10.11 服务重启后的恢复与持久化

Go 版以下状态落盘：账号凭据与健康/统计 JSON、conversation JSON、路由策略 JSON、usage/request JSONL、设置 JSON。会话文件保存 AccountID 等路由元数据和历史哈希，不保存消息正文。重启后可由显式会话 ID 或历史哈希找到原账号偏好；账号冷却/needs_relogin 也在账号文件内。

重启后**不会恢复**进程内 `rrIndex`、`inFlight` 计数或正在执行的请求。`sessionSlots` 通道重新初始化；槽 ID 按槽序号确定性生成，但它们是跨会话共享槽，并非恢复某个会话的独占 upstream session。会话没有 TTL/自动清理，历史哈希恢复需目录扫描。不存在非幂等任务的中断恢复或续跑机制。Johnsheng1 版只有模型缓存文件，没有会话/账号池恢复。

### 10.12 Account A / Account B 场景核对

| 场景 | Go 原项目实际行为 | Johnsheng1 Python 版 | 缺口/说明 |
| --- | --- | --- | --- |
| 100 个顺序新会话 | 默认 RR 按推演近似 50:50，但 `UpdatedAt` 排序导致成对突发；无分布测试 | 全部使用唯一一组 AK/SK | Go 不是严格 SWRR/公平队列 |
| 100 个并发新会话 | 每账号默认最多 4 个租约；全局默认 2 个 chat 槽；租约容量用尽的请求无账号级排队，常见 503 | 单账号，无池并发分配 | 不满足 100 并发稳定接入；实际结果受时序影响 |
| 同一会话连续请求 | 持久化 preferred AccountID；可用时优先原账号；不可用/忙时可能静默切到另一账号；上游 chat_id 实为共享槽 | 无会话绑定 | 账号亲和存在，上游会话亲和不成立；需明确迁移语义 |
| A 凭据失效 | 401/403 标记账号冷却与 `needs_relogin`；未 pin 时可在预算内换 B；OAuth 可由探测/凭据路径尝试刷新 | 返回通用 502，无备用账号 | 需区分凭据失效、暂时错误和封禁；不能把轮换用于绕过限制 |
| A 达到并发上限 | A 被排除，尝试 B；B 也满则失败，不在 A 上等待 | 无账号池并发控制 | sticky 会话也可能切换；无队列超时/公平唤醒 |
| A 额度耗尽 | 额度仅查询展示，不提前排除 A；依上游错误决定是否重试/冷却 | Benefit 余额独立查询，不与请求路由联动 | Rust 需把已知耗尽、未知和查询错误分开；是否跨账号重试需按错误语义决定 |
| 服务重启 | 会话 AccountID/健康状态等文件仍在；in-flight/游标/执行中的请求不恢复 | 无会话恢复 | 无 TTL 清理、任务恢复；历史哈希目录扫描 |

### 10.13 可复用设计（概念层）

- 账号秘密与公开状态分离；每账号独立刷新并持久化凭据轮换结果。
- 将账号级认证冷却与模型级临时故障分开；采用有上限退避、`Retry-After` 和重试预算。
- 会话元数据记录 preferred account；OpenAI 客户端不提供显式 ID 时可用历史前缀哈希恢复，但应加索引、TTL 和冲突策略。
- 流式响应开始前可以故障转移；首个 delta 之后不重放已输出文本。
- 账号级模型目录合并来源/区域信息；额度按账号保留套餐与模型指标。
- 先做凭据刷新/候选预检再建立 SSE 响应，避免响应头已发送后才发现无可用账号。
- `TM.00001041` 作为独立并发会话上限错误处理，而不是笼统归为认证失败。

### 10.14 原项目缺失、不可当成已有能力的部分

- Johnsheng1 参考仓库没有账号池及其全部调度行为；不能把本地 Go 的实现归到该 GitHub 项目名下。
- Go 版没有真实的每账号公平排队、SWRR 保证、持久化并发租约/租约 TTL、崩溃后 in-flight 恢复。
- `sessionSlots` 是全局 2–3 个，而不是每个账号各 2–3 个；README 的 per-conversation 稳定 upstream `chat_id` 与代码不符。
- 额度没有进入路由资格或权重；未知额度可能在 UI 聚合中表现为 0。
- 没有会话 TTL/LRU/清理、上游 session ID 与下游 session 的明确独立模型、非幂等任务恢复、sticky 会话等待原账号的队列。
- 没有显式模型 alias/capability 注册或所选账号的模型可用性验证；跨账号模型并集可能误导请求路由。
- 健康探测是低频 keepalive，不是完整的熔断器/半开状态机；错误到下游的 HTTP 映射较粗。
- 多账号统计/健康文件更新缺少事务或每账号锁；并发 RMW 可能丢更新。

### 10.15 Rust 迁移建议

1. **accounts/storage**：沿用领域字段，不沿用 JSON 文件实现；SQLite 事务保存账户状态、加密凭据、健康和统计，凭据轮换原子提交。公开视图永不输出秘密。
2. **scheduler**：默认 SWRR 或等权公平分配，游标/权重与 `UpdatedAt` 解耦；支持显式权重、会话亲和、模型可用性、冷却和并发准入。租约用 RAII 释放，并有持久化/TTL 兜底；明确排队、超时和拒绝语义。
3. **sessions**：将下游会话、绑定账号和可选 `upstream_session_id` 分字段持久化；显式 ID > 会话头 > 增量历史哈希；增加索引、TTL 清理和重启恢复。槽位必须按账号隔离，不能把共享槽误当成会话绑定。
4. **health/error**：建立请求级、模型级、账号级错误分类；认证失效不等同于普通 5xx；区分 `TM.00001041`、429、额度耗尽、模型不可用。支持冷却、探测、半开和人工禁用，但不以换号绕过上游限制。
5. **quota/models**：额度状态采用 `known/unknown/error/partial`，未知不填零；默认额度不直接等价于 token，只有上游明确给出 token 单位时才记录。模型按 raw ID、alias、区域、来源、能力和账号可用性建索引；路由前验证。
6. **providers/protocol**：独立重写 CodeArts SDK-HMAC/STS/DPoP、CN/Intl profile、SSE 解码和工具增量合并；SSE 首字节后禁止自动重放。Benefit 作为单独能力做实测，不实现绕过高峰排队/配额的逻辑。
7. **测试**：新增 A/B 两账号 100 个顺序会话与并发会话分布、sticky、pin 行为、账号失效/恢复、账号满载排队、额度 unknown/耗尽、重启恢复和 SSE 中途断流测试。现有 Go 测试只可作行为线索，不能视为 Rust 测试通过。
8. **许可边界**：本地 Go 工程未发现 LICENSE，仅使用协议和行为事实，不复制其代码或表达；MIT Python 参考也保持独立实现。

### 10.16 与 Johnsheng1 Python 版本的能力对照

| 领域 | Johnsheng1 Python | 本地 Go 工程 | Rust 版本需新增/改造 |
| --- | --- | --- | --- |
| 账号数/配置 | 单组 env AK/SK | 多账号独立文件，CN/Intl profile | Provider/Region/Account 独立领域模型与加密 DB |
| 路由/负载 | 无 | RR、random、health-aware weighted | 可验证公平性的 SWRR、持久化游标与租约 |
| 会话绑定 | 无 | 下游会话元数据绑定账号；上游 chat_id 使用全局槽 | 明确持久化 affinity 与 upstream session，补 TTL/队列 |
| 健康/失效 | 通用 502 | 账号/模型冷却、keepalive、部分 failover | 细分错误状态、探测熔断和可解释恢复 |
| 模型 | AgentCenter + Benefit 列表与缓存 | 多账号目录合并 | alias/capability/账号资格与未知状态 |
| 额度 | Benefit 独立余额/签到脚本 | 套餐和模型额度汇总展示 | Unknown-safe、额度门控规则和共享组语义 |
| SSE/工具 | OpenAI SSE 转发/聚合 | SSE 解析重组、工具增量合并 | 统一事件层并覆盖 Chat/Responses/Anthropic |
| 持久化 | 模型缓存文件 | 账号/会话 JSON，使用与请求 JSONL | SQLite 事务、迁移、TTL 与并发一致性 |

**本节审计结论**：账号池与调度细节来自本地 Go 工程，不来自用户给出的 Johnsheng1 仓库。静态分析发现 Go 版本具备有价值的故障分类、账号亲和和重试思路，但其“100 路并发”“额度感知”“每会话 upstream session”“公平调度”均不能直接当成已实现能力。未执行测试或真实上游验证，以上运行场景属于基于源码的行为判断。

### 10.17 codearttray 与 codearts2api 的调用关系（同一工程）

**定位**：`~/codearttray` 与 `~/codearts2api` 是同一套 CodeArts 工程的 Windows 前端与核心服务，**不是两个 Provider**。托盘只做桌面壳，不接触华为凭据、不签名、不持账号池。

**调用方式（源码证据）**

| 项 | 事实 | 位置 |
| --- | --- | --- |
| 包注释 | 明确“tray 与本地网关通信，网关持有 OAuth 登录态与请求签名；tray 不碰华为凭据” | `api/api.go:1-4` |
| 网关地址 | 默认 `http://127.0.0.1:8000`，另有 `gateway_key`、`gateway_exe`、`refresh_minutes=10`、`auto_checkin=true` | `config.go:13-21` |
| 鉴权 | `req.SetBasicAuth("admin", key)`；账号维度用 `X-CodeArts-Account` 头 | `api/api.go:89-92` |
| 账号列表 | `GET /api/ui/accounts` → `{accounts:[{id,user_id,user_name,authenticated,environment}]}` | `api/api.go:58-64,128-143` |
| 移除账号 | `DELETE /api/ui/accounts/{id}` | `api/api.go:160-169` |
| 上游代理 | `GET/POST /api/ui/codearts/proxy?path=<path>&agent=PromptCenter`，带账号头 | `api/api.go:171-195` |
| 额度 | 代理 `GET /snap-manager/v1/statistics/plugin`，取 metric `usageTotalPackageCredit` 的 `package_credit_amount` / `package_credit_used` | `api/api.go:256-284` |
| 签到状态 | 代理 `GET /v1/ops/delivery?channel=WEB`，按 `type=USER_LOGIN` 找当日活动；状态 `CLAIMED/CONFIRMED/CONSUMED` 视为已签 | `api/api.go:20-35,286-343` |
| 签到执行 | 代理 `POST /v1/ops/claim`（`campaignId`、`idempotentKey`、`channel=WEB`）成功后 `POST /v1/ops/confirm`；返回码 `0` 成功、`40001` 已完成、`40002` 已领取 | `api/api.go:197-245` |
| 自动签到 | 每轮刷新（默认 10 分钟）对未签到账号逐个补签，可一键全部签到 | `tray.go:154-168,558-597` |
| 进程托管 | 网关未达 `/health` 时以隐藏窗口方式启动 `codearts2api.exe`，等待 20 秒就绪 | `gateway_windows.go:19-62` |
| 后端路由 | 网关实际实现 `/api/ui/meta|access|accounts(含 refresh/probe)|routing|usage|codearts/proxy|logs|credentials/*` | `internal/server/server.go:108-126` |

托盘只使用了其中的 accounts / delete / proxy 三个入口；`refresh`、`probe`、`routing`、`usage`、`logs`、`credentials` 等属于 Dashboard 路由，未被托盘调用，可作为 Rust 管理 API 的功能清单参考。

**迁移结论**

| 托盘功能 | 迁移到 Rust Web 管理 API | 说明 |
| --- | --- | --- |
| 账号列表与身份 | 是 | `GET /admin/accounts`，只返回公开字段，不含凭据 |
| 移除账号 | 是 | 需二次确认 + 审计日志；对应 `DELETE` |
| 额度展示 | 是，但不经代理 | Rust 由 Provider 的 `quota()` 直接读取，保留 `known/unknown/error` 状态 |
| 签到状态展示 | 是（只读） | 读取活动/领取状态，仅展示，不代表已实现领取动作 |
| 签到领取（claim/confirm） | **默认否** | 仅当确认上游正式支持且允许自动化调用时才作为可选模块；见下方门控 |
| 定时刷新 | 是 | 作为可配置的额度刷新任务，不含自动领取 |
| 托盘菜单/通知/剪贴板 | 否 | 桌面交互，服务器无对应需求 |
| 开机自启（注册表 Run） | 否 | 服务器用 systemd/Docker 重启策略 |
| 网关进程托管（隐藏窗口启动） | 否 | 服务器由 systemd/容器编排负责 |
| Basic `admin:<key>` 鉴权 | 否（改用 Bearer） | 观察到的旧式认证不作为 Rust 设计；管理面统一用高熵 Token + 常量时间比较 |

**签到能力门控**：`/v1/ops/claim` 与 `/v1/ops/confirm` 属于权益领取动作，源码只能证明“可调用”，不能证明“上游正式允许自动化”。本项目默认不实现签到；只有同时满足（1）上游文档或官方说明明确支持该用法、（2）运维书面确认允许自动化、（3）在真实账号上验证不触发风控 三个条件，才允许作为默认关闭的可选模块接入，并在 UI 中标注来源与限制。

## 11. dsh-wtq-pool 源码审计（2026-10-28）

### 11.1 对象、版本与许可证

- 对象：`.refs/chengzhi-c_dsh-wtq-pool`，Git `f7888dc5759bb1e5a4360ac0c18ccabf28a88ca8`，提交标题 `WTQ-1.2.0`。
- `package.json` 声明版本 `1.2.0`、MIT；仓库含 `LICENSE`，版权为 `chengzhi-c`。本项目只记录协议与行为事实，不复制其源码、CSS、图标或文案；若借鉴 UI，只做独立重绘。
- 技术形态是 Node/TypeScript ESM 插件，运行时依赖 DeepSeek Harness `0.2.0` 线；Provider 逻辑在 `src/`，面板发布物在 `renderer/`，宿主卡片/输入框扩展在 `src/client/`。
- 本次为静态审计：未安装依赖、未运行测试、未启动 dsh、未读取真实桌面登录态、未请求任何上游；README 的能力声明不能替代真实验证。

### 11.2 Provider 实际实现矩阵

| Provider/区域 | 授权与账号来源 | 模型发现 | 聊天/流式 | 额度与账号状态 | 区域差异与限制 |
| --- | --- | --- | --- | --- | --- |
| WorkBuddy CN | 扫描桌面 CodeBuddy/WorkBuddy auth 目录及插件自己的 `*.info`；凭据含 access/refresh token、billing identity、region，见 `src/accounts.ts` | `GET /v2/enterprises/personal/models`，`src/upstream.ts:211-214`；`src/catalog.ts` 有 live 失败时的静态 fallback，不能当实时目录 | `POST /v2/chat/completions`，上游强制流式，见 `src/upstream.ts:1389`；网关输出 OpenAI-shaped SSE | 个人/套餐 credits、token 续期、per-model cooldown、session-dead/restricted hold，详见 `src/accounts.ts` 与 `src/upstream.ts`；额度保底会影响候选资格 | CN chat 为 `copilot.tencent.com`，计费为 `www.codebuddy.cn`；客户端版本/头与国际站分开 |
| WorkBuddy International | 国际桌面登录文件可扫描/归档；Token 与 CN 不互换 | `GET /v3/config`，国际目录需要桌面 UA，见 `src/upstream.ts:211-214` | 同一 `/v2/chat/completions` 族；国际请求使用独立 UA/Host 组合 | credits/refresh/hold 机制独立，不能与 CN 余额合并 | chat/登录基址为 `www.workbuddy.ai`；国际只维护一个桌面登录快照，重登会覆盖，插件通过归档保留旧账号 |
| TRAE CN | `src/trae-discovery.ts` 扫描本地桌面/CLI 凭据；区域为 `cn`，身份包含 machine/device 信息 | `GET /api/ide/v1/get_detail_param`，函数目录决定可调用 config，见 `src/trae-upstream.ts:46-74` | `POST /api/agent/v3/llm_utils_chat`；命名 SSE 事件由 `src/trae-sse.ts` 解析，包含排队/进度/token usage/done 语义 | credits/usage 读 `POST /trae/api/v2/pay/web_user_ent_usage`；模型不可用、限流、认证和网络错误分开，source pool 对失败做 hold/轮换 | chat `https://trae-api-cn.mchost.guru`，remote `https://solo.trae.cn/api/remote/v1`，pay `https://api.trae.cn`；CN 有签到/额度路径 |
| TRAE International | 本地凭据与 CN 分池；区域代码为 `ai`，不混用 CN 身份 | 同一目录路径但函数和远端基址不同 | 同一 `llm_utils_chat` 家族与命名 SSE | usage/pay 状态路径独立；源码对 CN-only usage 能力有明确限制，未知项不得填零 | chat/remote 为 `coresg-normal.trae.ai`，pay 为 `growsg-normal.trae.ai`；区域代码不是 WorkBuddy 的 `global` |
| Qoder CN | `src/qoder-discovery.ts` 扫描 Qoder 本地 `Local State`/`auth.v1.dat`；COSY credential 含 user/token/machine id | `GET /algo/api/v2/model/list?Encode=1`，只取可调用 `chat` 组，见 `src/qoder-client.ts:83-88` | `POST /algo/api/v2/service/pro/sse/agent_chat_generation?...`；`src/qoder-sse.ts` 读取普通 `data:` 行，源码注明没有 event/id/retry 字段 | OpenAPI `https://openapi.qoder.com.cn`，优先 `/sash/api/v2/me/usage`，回退 `/api/v2/quota/usage`；user/add-on/dedicated packages 分开，失败分类为 auth/credit/rate/model/server/client | gateway `https://gateway.qoder.com.cn/`；请求体使用 Encode=1 自定义编码，COSY 使用 AES/RSA 包装，见 `src/qoder-upstream.ts` |
| Qoder International | 独立 `global` discovery 与 credential，不能假定 CN token 互通 | 同一模型路径、独立目录与账号 | 同一 SSE agent chat 路径 | OpenAPI usage 为 `https://openapi.qoder.sh`；额度/活动状态独立 | gateway `https://api3.qoder.sh/`；源码使用 `global` 而非 `intl` 命名，不能据此宣称与 CN 共享账号/额度 |

### 11.3 授权、轮换、会话和错误边界

- 三家 Provider 都是“读取用户已有桌面登录态 + 插件自己的副本/归档”，不是账号注册器。源码没有把验证码破解、设备指纹生成或批量注册作为能力；本项目不得迁移任何身份伪装/风控规避逻辑。
- WorkBuddy 有独立账号池；`src/accounts.ts` 记录账号级与模型级冷却，支持 priority、round-robin、expiry 和 conversation sticky。TRAE/Qoder 由 `src/source-pool.ts` 按 preferred、分发模式、token 到期和失败 hold 排序。README 所称“限流/额度耗尽后换号”是实现行为，但仍不能解释为绕过平台封禁或配额。
- 失败分类实际存在：认证、额度、限流、模型不可用、客户端错误、服务端错误、网络/取消等；模型级错误只应禁用该账号该模型，授权拒绝不应伪装为普通 429。具体上游代码仍需真实账号验证其当前码值。
- WorkBuddy 对话请求有会话头和消息历史处理，但 dsh pool 的稳定性主要是网关侧账号选择；TRAE 与 Qoder 源码未证明可恢复一个官方原生会话 ID。Rust 侧应把 `gateway_affinity` 与 `native_session` 分开建模，不能把失败换号称为原生会话恢复。
- 三家都将上游 SSE 转成下游 `chat.completion.chunk`；Qoder 是普通 `data:` JSON，TRAE 是命名事件，WorkBuddy 是 OpenAI-shaped 流。必须按帧/事件解析，不能按一次网络 read 等于一帧。

### 11.4 dsh renderer/UI 设计参考（只做独立重绘）

- `renderer/index.html` 是原生 HTML 骨架，不是 React/Vue/Svelte；`renderer/panel.js` 使用 DOM helper 组装账号卡、额度折叠区、模型编辑行、连接检查和成长中心；`renderer/panel.css` 自带浅色/暗色变量、卡片、徽标、tab 和表单样式，无第三方 UI 组件库。
- 导航为两级：一级 WorkBuddy/Trae/Qoder 来源 tabs，二级 pool/models/check/growth 视图 tabs；区域再用 CN/Global 或 TRAE 的 CN/AI 子 tabs。成长中心和 token keep-alive 仅适用于相应 Provider，不应渲染成所有 Provider 都有。
- 账号池页面实际展示账号标签、区域、token 到期、credits/package 明细、cooldown、preferred、失败/限制状态，并支持启停、移除/恢复、二次确认删除、设置首选账号和额度 reserve。额度 reserve 会进入候选过滤，不只是视觉字段。
- 模型页支持 live catalog、搜索、选择发布、display name、context/output 上限、thinking、图片能力和每区域配置；WorkBuddy 的静态 fallback 必须在产品中标注“缓存/兜底”，不能冒充实时发现。
- 暗色模式由 `src/client/index.tsx` 从宿主 `data-base`/系统偏好计算，iframe 通过 `base=dark|light` 加载；`renderer/panel.css` 同时实现 CSS 变量与 `prefers-color-scheme` fallback。可借鉴色彩层次和信息密度，不复制其 CSS。
- 状态管理是模块级变量 + `pluginBridge.invoke()` + 定时刷新，不是 Redux/Zustand；真实账号、Provider、上游状态在宿主 `src/host.ts`/`src/web-panel.ts`，renderer 仅持有页面草稿和快照。动态 channel 白名单、loopback peer/Host/origin 校验和 CSP nonce 是其安全边界。
- `src/client/index.tsx` 仅用 React 做宿主插件卡片/iframe 高度测量，`src/client/composer.tsx` 做当前模型对应账号 credits chip；不应据此把整个 renderer 归类为 React 应用。

### 11.5 对本项目的可复用结论与明确缺口

**可借鉴（概念/事实，独立实现）**

1. Provider/Region 独立注册；没有账号或区域禁用时不发布模型组。
2. 账号状态、模型状态、额度快照和会话亲和分离；模型级限流不应禁用整个账号。
3. 额度按套餐/资源包保留明细，Unknown/读取失败不可显示为 0；credits 不自动当 token。
4. 连接检查应报告监听、目录、注册地址漂移和宿主是否接受 Provider，而不是只显示一个绿色状态。
5. UI 设计可采用来源→区域→视图的层级，但 Rust 管理台必须把后端状态作为唯一真值，页面草稿与已保存配置分开。

**不能当成已实现或已验证**

- dsh 项目没有 Qoder/WorkBuddy/Trae 的真实联调证据；本次没有凭据、没有启动服务、没有运行其测试。
- README 的“自动换号”不等于本项目可以在账号封禁、风控或配额耗尽时无条件继续；错误作用域、冷却和人工重新授权必须保留。
- Qoder COSY、TRAE Cloud/desktop identity、WorkBuddy 设备/客户端头的可用性与风控接受度仍需授权环境实测；不通过伪造头解决。
- dsh renderer 是可借鉴的 MIT 设计参考，不是本项目的前端实现；不得复制源代码、静态资源或表达性文案。

**本节结论**：dsh-wtq-pool 实际覆盖 WorkBuddy、TRAE、Qoder 的 CN/国际分组，并有 Provider、账号池、模型目录、额度读取、SSE、面板和故障状态代码；但“已实现源码”不等于“上游可用性已验证”。本项目仍应将六个目标区域初始标为 `experimental/unavailable`，完成真实授权测试后再改变状态。

## 12. ZCode（liu5269/zcode2api）源码审计（2026-10-28）

> **状态变更（2026-10-29）**：ZCode 已从 PolyGate 支持范围中**明确排除**。本节仅作为
> 历史审计记录保留，不得据此实现 `ZCodeProvider`、不得迁移 `zcode2api` 的任何功能，
> 也不得因参考实现支持而将其重新加入范围。

> 审计对象：`.refs/liu5269_zcode2api`。本节只记录该仓库，不重复已完成的 Coding2API、CodeArts、dsh-wtq-pool 审计。未安装依赖、未运行测试、未启动服务、未读取真实凭据、未调用真实上游。

### 12.1 许可证、语言与核心架构

- 根仓库许可证为 **AGPL-3.0**（`LICENSE`；`README.md` 亦声明）。本项目不复制其源码，也不把其衍生代码并入 MIT/Apache 目标；Node 子项目依赖另有各自的第三方许可证。
- Python 侧由 `requirements.txt` 管理，无 lockfile：FastAPI、Uvicorn、httpx、python-dotenv。验证码子项目 `captcha_node/` 只依赖 `jsdom`（`package.json`），Node 版本要求由 `package-lock.json` 与 `Dockerfile` 锁定。
- 实际模块：`app/main.py` 应用工厂与生命周期，`main.py` CLI，`app/routes/gateway.py`（`/v1/messages`、`/v1/models`）、`app/routes/admin_api.py`、`app/routes/pages.py`、`app/models.py`、`app/store.py`、`app/agent.py`、`app/quota.py`、`app/oauth.py`、`app/auth_admin.py`。
- 请求链路：`/v1/messages` 解析 JSON → 按前缀/头判断 provider → 规范化 model 与 content → `Store.select()` 取账号 → 构造上游 URL/headers → `httpx.AsyncClient.stream()` 转发 → 按状态码决定换号或直接返回。

### 12.2 Provider 接口

- 没有 Provider trait/基类/注册表/插件机制。provider 是 `app/models.py` 的字符串常量（`zai`、`bigmodel`），分发写死在 `app/agent.py:31-48` 与 `app/routes/gateway.py:46-50`。
- Z.AI 支持两种模式：Coding Plan JWT（含两点结构判定，`app/models.py:56-67`）与 API Key（fallback URL + `x-api-key`）。BigModel 仅 API Key 转发，可用模型前缀 `bigmodel/` 或头 `x-provider: bigmodel` 选择。
- OAuth 只覆盖 Z.AI（`app/oauth.py`），flow 存于进程内字典（`app/routes/admin_api.py:18-19`），重启丢失、无过期清理；BigModel 无 OAuth。

### 12.3 账号管理与 SQLite

- `Account`（`app/models.py:30-67`）含 provider、mode、jwt_token、api_key、enabled、status、quota、plan、usage、use_count、fail_count；状态为 `active/exhausted/cooling/invalid/disabled`。
- Schema 只有 `meta` 与 `accounts` 两张表（`app/store.py:45-79`），账号整体以 JSON 存在 `accounts.data`；启用 WAL、`synchronous=NORMAL`、`busy_timeout=5000`。
- **没有 schema 版本表或迁移脚本**；新增字段靠 `Account.from_dict()` 忽略未知键，不能处理表结构变更。导出 JSON 的 `version` 不是数据库版本。
- 凭据以明文 JSON 落库，`/admin/api/export` 与设置 API 可返回明文 secret。前端 localStorage 的 AES 仅包装后台 key，且派生材料硬编码、含固定 XOR fallback —— 不构成凭据加密存储。
- 轮询游标在内存（`app/store.py:29-33`），重启丢失，无跨实例协调。

### 12.4 会话与路由

- 无 conversation/session 表、无会话 ID、无历史保存；每次请求只用客户端 `messages`（`app/routes/gateway.py:93-104` 仅提取末条用户文本做日志）。**无会话粘性**，与 Coding2API/CodeArts 的 affinity 设计不同。
- `Store.select()` 过滤不可选账号后按每 provider 游标轮询（`app/store.py:224-238`）；单请求最多尝试 5 个账号，`skip_ids` 防重试同一账号。这是纯轮询，无权重、无公平队列、无并发租约。

### 12.5 模型、Anthropic Messages 与 SSE

- `/v1/models` 返回固定两项（`GLM-5.2`、`GLM-5-Turbo`），无动态发现；请求侧另有 `glm-5.1`、`glm-4.7` 等映射，导致公开列表与实际可路由名称不一致。
- `/v1/messages` 仅做有限规范化：去模型前缀、字符串 content 转单个 text block，其余原样透传。未实现 tool use/tool result、extended thinking、prompt caching、stop reason/usage 归一、非流式 envelope 与参数校验。
- SSE 为 `aiter_bytes()` 字节级透传（`app/routes/gateway.py:238-251`）：不解析事件、不重写、不校验 `message_start`/`content_block_delta`/`[DONE]` 序列。上游 read timeout 设为 `None` 以支持长连接。

### 12.6 额度管理与错误处理

- `app/quota.py:27-109` 并发请求 `/billing/current`、`/billing/balance`、`/usage`，按 `plans/balances/total_units/used_units/remaining_units/expires_at` 做启发式解析；默认 60 秒刷新、最多 8 并发；只监控 Z.AI JWT 账号，API Key 与 BigModel 不刷新。
- 耗尽判定为“所有模型剩余 ≤ 0”加上转发路径中的 402/关键词命中，属启发式而非稳定错误码协议。
- 错误映射：402/耗尽→`exhausted`，429/连接失败→`cooling`，401/403→`invalid`（验证码例外），其他→增加 `fail_count` 并原样返回；400/无账号→503，验证码失败→500。冷却到期只影响可选择性，状态字段不自动回写 `active`。
- 流式中断只记日志，不向客户端重构 Anthropic 错误事件；`fail_count` 仅在部分分支累加。

### 12.7 可迁移设计与不适合迁移项

**可独立重写的事实与思路**

1. 账号状态机与候选过滤分层（enabled / status / cooling 与动态可选性分离）。
2. 每 provider 独立游标 + 有限尝试预算 + `skip_ids` 去重。
3. 额度刷新与请求路径解耦，后台周期刷新、并发上限、失败保留旧快照。
4. 网关 key 与管理 key 分开校验，采用常量时间比较。
5. Anthropic 入站 → 内部统一事件 → 多上游适配的分层方向（本项目必须补齐 ZCode 缺失的协议语义）。

**不适合迁移**

- AGPL 源码与表达性实现；明文凭据入库、硬编码派生的前端包装、无版本 SQLite schema。
- 字符串分支式 Provider 扩展、固定模型列表冒充动态发现、字节级 SSE 透传冒充完整 Anthropic 协议。
- 无会话表却宣称的会话能力（该项目本就没有，不应在别处照抄这种缺口）。
- `captcha_node/solver.js`、`app/captcha.py`、`X-Aliyun-Captcha-Verify-Param` 全链路：含 jsdom 浏览器 API/指纹桩、第三方验证码求解与自动重试，属明确不迁移项；AGPL 不授予第三方 SDK 或上游反滥用机制的额外权利。

**审计结论**：ZCode2API 是具备多账号轮询与流式透传的 FastAPI 网关，但缺少可复用 Provider 抽象、完整 Anthropic 协议层、动态模型发现、会话粘性和安全凭据存储。ZCode 与 BigModel 在本项目继续标 `experimental` / `unavailable`，不得据此标 `verified`。

## 13. 许可证审计与仓库清单（2026-10-28 核对）

本节只记录对**实际 LICENSE 文件**的核对结果，不依据 README 或记忆推断。`Unknown` 表示仓库内未发现可确认的许可文件，按“保留所有权利”处理。本表不构成对最终项目许可证的选择。

| 仓库/工程 | 路径 | 实际许可证证据 | 风险与使用边界 |
| --- | --- | --- | --- |
| RobbsLuo/Coding2API | `.refs/RobbsLuo_Coding2API` | MIT，`LICENSE`，Copyright (c) 2026 Hongsheng Luo (Robbs)；HEAD `df99362` | 可参考；仍独立重写，不整文件搬运 |
| chengzhi-c/dsh-wtq-pool | `.refs/chengzhi-c_dsh-wtq-pool` | MIT，`LICENSE`，Copyright (c) 2026 chengzhi-c；HEAD `f7888dc` | 可参考；不复制 CSS/JS/图标/文案，UI 只独立重绘 |
| Johnsheng1/codearts2api | `.refs/Johnsheng1_codearts2api` | MIT，`LICENSE`，Copyright (c) 2026 JohnSheng；HEAD `d813ca0` | 可参考；单账号实现，无账号池 |
| liu5269/zcode2api | `.refs/liu5269_zcode2api` | **AGPL-3.0**，`LICENSE` 为 GNU AGPL v3 全文；HEAD `925e929` | **只提取协议事实，不复制代码**；不并入 MIT/Apache 目标；验证码/指纹链路不迁移 |
| yetone/magpie | `.refs/yetone_magpie` | MIT，`LICENSE`，Copyright (c) 2026 yetone；HEAD `27bf147` | 可参考路由/额度/粘性思路；重新设计，不转写 |
| 本地 CodeArts Go 工程 | `~/codearts2api` | **无 LICENSE；操作者本人作品**（2026-10-29 确认） | 代码可复用（作者授权）；复制时保留版权声明并标注来源；依赖库仍需单独核对 |
| workbuddy2api | `~/workbuddy2api` | MIT，`LICENSE`，Copyright (c) 2026 HanHan666666 | 可参考；2 个本地未提交补丁需注意 |
| codearttray | `~/codearttray` | **无 LICENSE；操作者本人作品**（2026-10-29 确认） | `codearts2api` 同一工程的 Windows 托盘前端，非独立 Provider；代码可复用（作者授权）；托盘/自启/桌面 UI 默认不迁移（设计取舍） |
| BlueSkyXN/OpenZWork | 已移除（原 `.refs/BlueSkyXN_OpenZWork`，GPL-3.0） | 2026-10-28 按要求彻底删除 | 不再研究、不再参考其 Agent Runtime |

**许可证结论**

1. 参考仓库中 3 个 MIT（Coding2API、dsh、Johnsheng1）+ 1 个 MIT（magpie）+ 1 个 AGPL（zcode2api）；两个本地工程（`codearts2api`、`codearttray`）无 LICENSE 文件，但均为**操作者本人作品**（2026-10-29 确认），代码可复用。
2. AGPL 内容一律不进入本项目代码；第三方无 LICENSE 项目同样禁止复制；只允许独立重新表达的协议常量（URL、头名、错误码、事件名）。操作者本人作品的复用不受此限，但需保留版权声明并在 PolyGate `LICENSE`/`NOTICE` 标注来源。
3. **最终项目许可证尚未决定**：`docs/ARCHITECTURE.md` 中的“MIT（建议）”只是待确认项，本阶段不锁定。选择 MIT 或 Apache-2.0 都需要在发布前由用户确认，并完成依赖许可证清点。
4. 第三方依赖（如 zcode2api 的 `jsdom`、各仓库的传递依赖）有各自许可证，即使参考仓库为 MIT/AGPL，也不代表其依赖可被忽略；操作者本人工程的依赖同样需要核对。

## 14. 研究结论索引

| 主题 | 所在章节 | 结论摘要 |
| --- | --- | --- |
| 多 Provider 架构与三协议适配 | §9（Coding2API） | 显式注册 + ErrKind 分类 + 会话亲和最值得迁移 |
| CodeArts 账号池与调度 | §10 | Go 工程有 RR/weighted、冷却、会话绑定；无公平队列与持久化租约 |
| 托盘↔网关的管理面调用 | §10.17 | `/api/ui/*` 的账号/额度/签到代理；签到默认不实现，桌面壳不迁移 |
| WorkBuddy/TRAE/Qoder 协议与 UI | §11、`PROVIDERS.md` §12 | 有源码实现；原生会话恢复未证明；额度必须 Unknown-safe |
| ZCode Provider | §12 | 部分 Anthropic 透传、无抽象、明文凭据、AGPL |
| 许可证与使用边界 | §13 | 1 个 AGPL；其余参考仓库 MIT；本地 `codearts2api`/`codearttray` 为操作者本人作品（可复用）；最终许可证待定 |
| Magpie 路由/额度/粘性 | 未完成 | 仅摘要级阅读，尚需阶段 02 前补做或明确放弃 |

**全局结论**：所有上游 Provider 的真实能力验证数为 **0**。现有材料只能支撑“协议与实现路径有源码依据”，不能支撑任何 `verified` 声明。