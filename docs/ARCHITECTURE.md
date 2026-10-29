# PolyGate 架构设计

> 纯 Rust 多 Provider Coding Agent Gateway + 账号池。
> 本文档记录设计基线与已落地模块；逐阶段实现状态与验证结果以 `PROGRESS.md` 为准。

## 1. 目标与非目标

**目标**

- 单一 Rust 二进制（含嵌入式 Web 管理台），Linux x86_64 / aarch64。
- 统一管理 CodeArts、Qoder、TRAE、WorkBuddy 的账号、模型、会话、额度；CodeBuddy 与 ZCode 均明确排除。
- 对下游提供 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 兼容 API，含 SSE、工具调用、取消、错误映射。
- 默认“新会话公平分配 + 同会话固定账号”，支持 SWRR / RR / Least Connections / Quota-aware。
- 低内存：空闲目标 < 30 MB，10 账号 < 50 MB；真实测量，不承诺。
- 公开 GitHub 发布：文档、CI、Docker、systemd、最小权限运行。

**非目标**

- 不实现 OpenCode Zen 上游。
- 不做验证码破解、批量注册、设备指纹伪造、封禁后自动换号、风控规避。
- 不保证所有 Agent 接口都能伪装成普通 LLM API；不支持的能力明确返回 `Unsupported`。
- 不内置浏览器/Chromium/Node/Python 运行时。

## 2. 模块布局

```text
src/
├── main.rs            # 薄入口：调用 cli::run()，返回进程退出码
├── lib.rs             # 可测试的库入口
├── cli/               # clap：serve / doctor / config-example、日志初始化、关闭信号
├── runtime/           # Gateway::bind / serve：配置 → 安全 → SQLite → Registry → Router → Listener
├── config/            # TOML + POLYGATE_* 环境变量
├── api/
│   ├── mod.rs          # 路由、API Key 中间件、解析 → dispatch 主链路
│   ├── openai.rs       # /v1/chat/completions 的 wire DTO 与转换
│   ├── responses.rs    # /v1/responses 的 wire DTO、事件状态机
│   ├── anthropic.rs    # /v1/messages 的 wire DTO、事件状态机
│   ├── wire.rs         # WireRequest trait（三接口共用）
│   ├── sse.rs          # SseTranslator + 通用 SSE 驱动
│   ├── logging.rs      # 请求元数据日志（不含内容）
│   └── catalog.rs      # 启动时模型目录
├── admin/             # 嵌入式 Web 管理台（M13-C1）
│   ├── mod.rs         # 资源嵌入、AdminState/Settings、路由、安全头
│   ├── auth/          # 管理员设置/登录/恢复码/会话/CSRF/Origin/限速
│   ├── account/       # 授权结果落库、账号视图脱敏
│   └── api/           # Dashboard/账号/模型/签到/用量/授权端点
├── model_service/     # 模型别名、多目标、能力覆盖与解析（M15）
├── scheduler/         # 账号轮询/加权/最少连接、额度与冷却过滤（M15）
├── providers/
│   ├── mod.rs         # Provider trait、能力、错误
│   ├── registry.rs    # ProviderRegistry（按 kind+region 实例）
│   ├── mock.rs
│   ├── codearts/      # CodeArts adapter（M13-B/C1）
│   ├── qoder/         # Qoder adapter（M13-D2）
│   ├── workbuddy/     # WorkBuddy adapter（M14-B/C）
│   └── trae/          # TRAE adapter（M14-D/E）
├── protocol/          # 规范化的请求/响应/流事件（与上游解耦）
├── accounts/          # Account / Credential / AuthStatus（领域类型，不依赖 storage）
├── account_service/   # AccountService / CredentialService（生命周期、事务、CAS 更新）
├── checkin/           # 签到引擎 + CodeArts/Qoder/WorkBuddy/TRAE 执行器
├── sessions/          # 会话绑定、粘性、TTL、持久化
├── session_service/   # 会话绑定策略 + SessionRouter：账号/凭据/上游会话租约（M13）
├── quota/             # Quota Group / Snapshot / Unknown 语义
├── models/            # 启动时模型目录（Provider 发现结果）
├── storage/           # SQLx + SQLite，迁移、批量写入
├── security/          # AES-256-GCM、token hash、脱敏
├── metrics/           # 计数、RSS/CPU、用量聚合
├── test_support.rs    # 测试用本地 HTTP 服务器
└── error/             # ErrorKind、OpenAI/Anthropic 错误体
web/                   # 原生 HTML/CSS/JS，rust-embed 嵌入二进制（无 Node 运行时、无 CDN）
migrations/            # 0001–0008（init / credential revision / checkin / session revision /
                       #   admin / refresh lease / model routes / request logs）
deploy/                # systemd 单元、Dockerfile、容器配置
tests/                 # 调度、协议、API 集成测试
```

### 2.1 模块职责与依赖

| 模块 | 位置 | 职责 | 允许依赖 | 禁止 |
| --- | --- | --- | --- | --- |
| Provider Trait | `providers/mod.rs` | 上游契约：`info`/`capabilities`/`auth_status`/`list_models`/`quota`/`chat`/`chat_stream`/`agent_task`/`cancel`；`ProviderContext` 与 `CredentialStore`（M13-B；M14-C 增加跨进程刷新租约与 `quota_with_context`） | `protocol`、`error`、只读 `accounts::Credential` | 依赖 `storage`、`scheduler`、`api` |
| Provider Registry | `providers/registry.rs` | `(kind, region)` → `Arc<dyn Provider>`；启停、能力快照、验证状态 | `config`、`providers` | 持有凭据、做健康判断、决定选号 |
| Account Manager | `account_service/` | 账号 CRUD、启停、认证状态机、授权上下文；凭据加解密、修订号 CAS、失效标记 | `storage`、`security`、`accounts` 领域类型 | 决定“本次请求用哪个账号”、调用真实 Provider、知道签到协议 |
| Check-in Engine | `checkin/` | 签到状态机、批量/全部、定时策略、持久化去重与崩溃恢复；`RoutingCheckinExecutor` 按 (kind, region) 分派；CodeArts `/v1/ops/*` 执行器（M13-B） | `account_service`、`storage`、`error`、`providers::codearts`（签名传输） | 绕过验证码、改 Provider Registry、写 Web UI |
| Scheduler | `scheduler/` | 候选过滤、选号策略、并发租约、冷却/半开 | `account_service`（只读接口）、`quota`（只读快照）、`sessions`、`models` | 直接发起 HTTP、写凭据 |
| Session Manager | `sessions/` | 解析会话键、绑定/迁移、TTL、持久化节流 | `storage` | 持有凭据、选择账号 |
| Session Binding | `session_service/` | 绑定粘性、CAS 改绑、显式失效/恢复、账号/会话级并发；`SessionRouter` 解析账号与凭据并把租约交给 API 层 | `sessions`（领域）、`storage`、`account_service`、`error`、`providers::ProviderContext` | 静默改绑、删除失效绑定、无界等待、完整调度策略 |
| Quota Manager | `quota/` | 快照状态机、组聚合、历史、Unknown 语义 | `storage`、`providers::quota` | 把未知额度折算为 0 或 ∞ |
| API Layer | `api/` | 三协议适配、下游鉴权、错误映射、SSE 输出；通过 `SessionRouter` 获取账号/凭据/租约 | `protocol`、`scheduler`、`sessions`、`models`、`session_service`、`error`、`metrics` | 直接访问具体 Provider 实现 |
| Web Admin | `admin/` | 嵌入式管理台：管理员认证、会话/CSRF、CodeArts 授权流程编排、账号视图与管理端点 | `account_service`、`storage::AdminRepo`、`security`、`api::ModelCatalog`、`checkin`、`providers::codearts::login` | 直接写 SQL、直接请求具体 Provider（登录协议除外）、依赖 `/v1` API Key |
| Storage | `storage/` | SQLx/SQLite、迁移、事务、批量写、备份接口 | `error`、`security`、`accounts`/`sessions`/`quota` 领域结构 | 依赖 `api`、`providers`、`scheduler` |
| Security | `security/` | `SecretBox`（AES-256-GCM）、token hash、脱敏、审计密钥 | 无（纯工具） | 依赖业务模块 |
| Protocol | `protocol/` | 规范化 `ChatRequest`/`StreamEvent`，与上游解耦 | `error` | 依赖 `storage`、`providers` 实现 |
| Metrics | `metrics/` | 请求/会话/token 三类计数、RSS/CPU、用量聚合 | `storage` | 复用会话计数冒充请求数 |

**依赖方向（单向）**

```text
config ─┬─> providers ──> protocol, error
        ├─> storage ──> security
        ├─> security
        └─> api

api ──> protocol, scheduler, sessions, models, error, metrics
admin ──> account_service, storage(AdminRepo), security, api::ModelCatalog, checkin, providers::codearts::login
scheduler ──> account_service, quota, sessions, models
checkin ──> account_service, storage, error, providers::codearts(transport)
session_service ──> sessions(domain), storage, account_service, error, providers::ProviderContext
providers::codearts ──> protocol, error, accounts(read-only), quota, hmac/p256
checkin(model) ──> config, error
storage ──> checkin(model)            # 只复用记录/策略类型，不反向调用服务
account_service ──> storage, security, accounts(domain)
accounts(domain) ──> config
quota ──> storage, providers(quota)
sessions ──> storage
```

`providers` 不得反向依赖 `scheduler` 或 `storage`；`scheduler` 不得依赖 `api`；`api` 只依赖 scheduler 的抽象结果，不碰具体 Provider。

Storage 仍直接复用 `accounts`/`sessions`/`quota` 的领域结构（Repository 返回领域对象），这是 M4 的已知偏差。M11 把账号/凭据生命周期放进独立的 `account_service`，没有让 `accounts` 领域模块反向依赖 `storage`；后续如需彻底消除偏差，可为 storage 引入自有 row 类型 + 显式转换。

`providers` 另外直接使用 `quota::QuotaSnapshot` 作为 `quota()` 返回类型（M6 偏差）。理由：额度结构已在 `quota` 中定型，重复定义会制造两套 `unknown` 语义。约束不变：只读复用，`providers` 不得写数据库、不得调用调度器。

### 2.2 Provider Registry

- **键**：`ProviderKey { kind, region }`，区域枚举按上游事实区分：WorkBuddy/Qoder 用 `cn|global`，TRAE 用 `cn|ai`，CodeArts 用 `cn|intl`。不共用一套区域名，避免 TRAE 的 `ai` 被误写成 `global`。
- **注册时机**：启动时按配置实例化，只有 `enabled=true` 且凭据可解析的 provider-region 才进入注册表；未启用者不发布模型、不出现在 `/v1/models`。
- **验证状态**：`VerificationState::{Verified, Experimental, Unsupported, Unavailable}`（M6 实现）。只有真实账号端到端验证后才允许 `Verified`；`config::Verification::Mock` 映射为 `Experimental`，mock 身份由 `ProviderKind::Mock` 表达，Mock 永远不得成为 `Verified`（见 `PROVIDERS.md` §13.4）。
- **能力快照**：注册时缓存 `Capabilities`；运行时能力变化（如目录拉取失败）只影响目录来源标记 `live|cache|fallback`，不改写能力位。能力位默认 `false`，`ProviderClass::{Model, Agent, Hybrid}` 由 `chat`/`agent_task` 派生；trait 未实现的操作统一返回 `Unsupported`。
- **启停语义**：禁用 Provider 只停止新请求的候选资格；已绑定的会话在下次请求时返回可解释错误，不静默迁移到别家。
- **不持有凭据**：Registry 只存 `Arc<dyn Provider>` 与元数据；凭据在每次调用时由 Account Manager 解密后传入，避免长期驻留内存。

### 2.3 账户、调度、会话、额度的协作边界

```text
请求 ─> API(下游协议) ─> Session(解析/绑定) ─> Scheduler(候选+租约)
                                                    │
                          ┌─────────────────────────┼─────────────────────┐
                          ▼                         ▼                     ▼
                    Account(凭据/状态)        Quota(快照/可路由性)    Models(统一名→raw id)
                          │
                          ▼
                    Provider(上游调用) ─> Protocol(规范化事件) ─> API(下游 SSE/JSON)
```

- Scheduler 只读账号状态；状态写入由 Account Manager 在调用结果返回后完成，避免调度器直接改凭据状态。
- 额度是否参与候选资格由 Quota Manager 回答；`unknown` 不排除账号，`exhausted`（已知为 0）才排除。
- 会话绑定失效（账号禁用/凭据失效/额度耗尽）时返回 `SessionBindingInvalid`，由客户端决定是否开新会话；不自动换号续跑非幂等任务。

### 2.4 签到引擎（M12）

```text
CheckinScheduler ──> CheckinService ──> RoutingCheckinExecutor ──> Mock / CodeArts (M13-B)
                          │                    ▲
                          │                    └── M13/M14 真实 adapter（尚未实现）
                          ├──> AccountService（候选账号、授权上下文、认证失败回写）
                          ├──> Storage::checkin（runs/schedules，claim/complete）
                          └──> QuotaRefreshHook（成功后请求刷新，失败隔离）
```

- **状态机**：`Pending → Running → Succeeded | AlreadyCheckedIn | Failed | Unknown | Unsupported`；`Failed → Running` 仅在有界重试策略允许时发生。`Unknown` 表示请求可能已到达上游但结果不可确认，**不得自动重试**，只能靠上游状态查询确认。
- **持久化去重**：`checkin_runs` 的 `UNIQUE(account_id, business_date)` 保证本地每个账号每天只有一行；`pending|failed → running` 的条件 UPDATE 加租约（`lease_until`）保证多 Worker 只有一个执行者。本地唯一约束**不能**保证远端副作用恰好一次。
- **业务日期**：`business_date` 按 schedule 的 IANA 时区计算（无 schedule 时用 Provider 默认：CN=`Asia/Shanghai`、Intl=`UTC`），从不使用 VPS 本地时区。抖动按 `(account_id, business_date)` 确定性生成，重启不会改变当天时点。
- **自动循环**：只读 DB 中的 schedule；每轮重新计算今天/昨天的 occurrence 与补签窗口，不在内存保存“今天已经做过”的集合。启动时先恢复中断运行，再进入 sleep/tick 循环；shutdown 后不再领取新任务。
- **崩溃恢复**：`Running` 且租约过期的行先查上游状态；能确认成功/已签/失败就落对应状态，否则落 `Unknown`，绝不重新执行签到。`Pending` 行可直接被下一次 claim 领取。
- **执行器边界**：签到逻辑不进入 Provider Registry；`CheckinExecutor` 与 `Provider` trait 解耦，M13/M14 再决定集成方式。不支持的 Provider 明确返回 `Unsupported`，且不读取凭据。
- **额度刷新**：成功后调用 `QuotaRefreshHook`；刷新失败只写在结果里，不回滚签到状态。

### 2.5 会话绑定（M13）

```text
SessionBindingService
  ├── 唯一键 (client_session_id, provider_kind, region)
  ├── account_id → 同一会话永久粘性（除非显式 rebind）
  ├── upstream_session_id → 只在 account + binding_revision 匹配时写入
  ├── invalidate() 保留账号与上游 ID，acquire() 明确失败
  └── reset_upstream()/rebind() 是仅有的恢复动作
```

- 账号级在途上限、启动窗口和会话串行化都在 `session_service/limits.rs`；等待有统一超时，超时返回 `NoCapacity`，不做无界重试。
- CodeArts 的保守默认是 3 在途、3 次启动/60s、75s 队列超时；来源与证据等级见 `docs/CODEARTS_SESSION.md`。
- 绑定持久化在 `sessions` 表（迁移 0004 增加 `binding_revision` / `invalid_reason`）；并发租约是进程内状态，多实例需等后续 Scheduler 阶段改为数据库租约。
- **M13-B `SessionRouter`**：Chat 请求先用绑定查找账号；不存在时选第一个可用账号并创建绑定，
  读取凭据后返回 `RouteLease`（会话锁 + 账号槽 + 上游会话 ID）。API 层持有租约直到 SSE 结束或
  连接断开；错误按 `classify_session_failure` 触发 `reset_upstream` 或写入账号认证状态。
  完整公平选号/权重/冷却留给 M19。
- **M13-D2-E 租约语义（已测试，`PROGRESS.md` §33）**：SSE body 被客户端丢弃时租约随 stream
  一起 drop，立即释放会话锁与账号槽（不调用 `finish()`，不计 usage/不延长 TTL）；正常结束、
  空闲超时、流内额度错误都走 `report_failure` + `finish()`；同一会话的请求由 per-session 锁
  串行化，不同会话不互相阻塞；会话失效只重置同一账号的 upstream id，从不静默换号。

## 3. 领域对象（严格分离）

| 对象 | 职责 | 关键约束 |
| --- | --- | --- |
| Provider | 上游平台实例（kind + region + 配置） | 可独立启停；CN/Intl 分开 |
| Region | `cn` / `intl` | 不因换 Base URL 就宣称支持 |
| Account | 一个授权账号 | 健康/权重/并发/冷却/额度组 |
| Credential | 加密的上游秘密 | 只经 `SecretBox` 解密；日志脱敏 |
| Quota Group | 余额池 | 独立或共享；未知不显示 0 |
| Model | 统一定义 + 上游 raw_id + 能力 | 一个统一名可被多 Provider 提供 |
| Session | 下游会话绑定 | 稳定绑定账号；重启可恢复 |
| Request | 一次 API 调用生命周期 | 并发计数、取消、计量分开 |
| Scheduler | 账号与路由决策 | 不混同会话/请求/Token 统计 |

**统计口径**：会话分配次数、HTTP 请求数、Token 用量分别计数，互不替代。

## 4. Provider Trait（能力分级）

```rust
#[async_trait]
trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;          // id / kind / region / verification
    fn capabilities(&self) -> Capabilities;  // 见下

    async fn auth_status(&self, c: &Credential) -> Result<AuthStatus>;
    async fn list_models(&self, c: &Credential) -> Result<Vec<UpstreamModel>>;
    async fn quota(&self, c: &Credential) -> Result<QuotaSnapshot>;

    async fn chat(&self, c: &Credential, req: ChatRequest)
        -> Result<ChatResponse, GatewayError>;
    async fn chat_stream(&self, c: &Credential, req: ChatRequest)
        -> Result<ChatStream, GatewayError>;

    // 默认返回 Unsupported，由真正支持的 Provider 覆盖
    async fn agent_task(&self, _: AgentTaskRequest) -> Result<AgentTask, GatewayError>;
    async fn cancel(&self, _: CancelRequest) -> Result<(), GatewayError>;
    async fn resume_session(&self, _: &UpstreamSession) -> Result<(), GatewayError>;
}
```

`Capabilities` 至少包含：`model_inference`、`agent_tasks`、`streaming`、`tools`、`list_models`、`quota`、`native_session`、`cancel`。
**Model Provider** 与 **Agent Provider** 分开声明；不得把 Agent 任务结果硬转为普通 Chat 语义。

## 5. 调度器

### 5.1 选择流程

1. 解析统一模型 → 候选 Provider（按配置顺序/优先级）。
2. 若请求带 `session_id` 且存在有效绑定：**只使用绑定账号**，不重新选号；绑定失效返回可解释错误。
3. 新会话：过滤不可用账号（禁用、认证失效、冷却、额度耗尽、达到并发上限）。
4. 按策略选择：默认 Smooth Weighted Round Robin；可选 RR / Least Connections。
5. 获取 Provider 级与账号级并发租约（有界等待，超时返回 `NoCapacity`）。
6. 绑定会话并持久化；请求结束释放租约。

### 5.2 状态与限制

- 权重（默认 1）、单账号最大并发、Provider 最大并发。
- 连续失败计数 + 指数冷却（有上限）；认证类错误直接标记账号不可用。
- 模型级冷却（WorkBuddy `6004/6008` 等）不污染账号健康。
- 半开探测：冷却结束后只放行一个探测请求。
- 并发租约 RAII 释放；断连/取消也必须释放。
- SQLite 持久化会话、账号状态、用量；运行态计数在内存中，避免每请求写库。

### 5.3 公平性验收

- 两等权账号、100 个新会话 → 接近 50:50。
- 权重 2:1 → 长期比例接近 2:1。
- 同会话连续请求账号不变。
- 超并发 → 排队至超时或明确错误，不超发。
- 重启 → 有效会话映射恢复。
- 高并发无竞态；取消后计数归零。

## 6. 会话模型

```text
client_session_id  ->  sessions.id
provider / region  ->  provider + region
account_id         ->  稳定绑定
upstream_session_id->  上游原生会话（可空）
state              ->  active / expired / invalid
created_at / last_used_at / expires_at / request_count
```

- 客户端会话 ID 来源优先级：显式头（`x-session-id` 等）→ 请求内 `metadata.session_id` → 由消息前缀派生的确定性指纹。
- 未显式提供时，**不能保证同一窗口对应同一上游会话**；只保证网关侧粘性可复现。
- 非幂等 Agent 任务进行中禁止迁移账号。
- 过期清理由后台任务执行；`last_used_at` 写入做节流（例如 ≥30s 才落库）。

## 7. 额度

- `QuotaSnapshot`：`unit ∈ {credits, requests, tokens, unknown}`，`status ∈ {known, unknown, error, partial}`，数值可为 `null`。
- 共享组取最新 Known 快照（不求和）；独立组对 Known 成员求和；未知成员不按 0 计。
- 平台积分与 token 不自动换算；如需换算必须有运维配置的已验证比率。
- 额度历史写库，供 Usage 页面趋势展示；查询失败保留错误快照。

## 8. 协议层

内部规范化事件：

```text
Start -> TextDelta* / ReasoningDelta* / ToolCallStart*
      -> ToolCallArgsDelta* -> Usage -> Finish(reason) | Error
```

- 内部类型位于 `src/protocol/`：`ChatRequest`/`ChatResponse`/`ChatMessage`/`ContentBlock`/`ToolCall`/`ToolResult`/`Usage`/`FinishReason`/`StreamEvent`。不支持的内容用 `ContentBlock::Unsupported` 显式表达，未知 `finish_reason` 用 `FinishReason::Other` 保留，不得静默丢弃。
- 顺序由 `StreamEventValidator` 显式状态机强制：`Start` 先行；`Usage` 最多一次，其后只允许 `Finish`/`Error`；终止后拒绝任何事件；`ToolCallArgsDelta` 必须匹配已开始的 call id，重复 id 拒绝。
- Serde：可选字段缺省即省略；内部类型忽略未知 JSON 字段以保持前向兼容；外部协议 DTO 的严格校验由 API 适配层负责。

适配器：

| 下游 | 非流式 | 流式 |
| --- | --- | --- |
| OpenAI Chat | `chat.completion` | `chat.completion.chunk` + `[DONE]` |
| OpenAI Responses | `response` 对象 | `response.created/output_item.*/output_text.delta/function_call_arguments.delta/response.completed`，无 `[DONE]` |
| Anthropic Messages | `message` + `content[]` | `message_start/content_block_start/delta/stop/message_delta/message_stop/ping/error` |

要求：

- SSE 按块解析，不假设一次 TCP 读取等于一个事件；处理 `[DONE]`、多行 `data:`、命名事件、空行分隔。
- 工具参数以增量转发，最终拼接合法 JSON；非法 JSON 产生可解释错误。
- `finish_reason` 规范化：`stop/length/tool_calls/content_filter`；上游空串不得原样泄漏给严格客户端。
- 客户端断连 → 取消上游请求、释放租约、记录 `cancelled`（不记为成功）。
- 错误映射：OpenAI `{"error":{...}}`，Anthropic `{"type":"error",...}`，HTTP 状态按 `ErrorKind`。

## 9. 管理 API 与 Web UI（M13-C1 已实现）

详细设计、路由表与安全清单见 `docs/WEB_ADMIN.md`；本节只记录架构约束。

- **两类接口彻底隔离**：`/v1/*` 使用 Gateway API Key；`/admin/*` 使用管理员密码 + 会话 Cookie + CSRF。
  API Key 永远不授予管理权限，管理会话也不用于推理请求。
- **处理器不碰 SQLite**：账号/凭据操作经 `AccountService`/`CredentialService`；存储只通过
  `Storage::AdminRepo` 读管理员/会话/流程行。
- **协议在适配器**：CodeArts 授权 URL、PKCE、DPoP、token 交换、身份查询全部位于
  `providers::codearts::login`；`admin` 只编排 `start → callback → complete` 状态机。
- **流程状态机**：`pending → exchanging → succeeded | failed | cancelled | expired`。回调不携带
  `state`，因此一个实例只允许一个活动流程（唯一索引 + 条件 UPDATE），并发发起返回 409。
- **凭据边界**：目标账号的凭据只以 `SecretBox` 密文入库；流程秘密（PKCE verifier、DPoP 私钥）
  单独加密，AAD 绑定 flow id。任何管理 JSON/日志/前端存储都不包含凭据。
- **身份去重**：`accounts.external_id` + 部分唯一索引 `(provider_kind, region, external_id)`；
  重复授权更新同一行凭据（CAS），CN/Intl 互不影响。
- **默认本地**：只监听 `127.0.0.1`；首次设置要求 loopback 对端且 loopback Host；反向代理部署需
  显式配置 `server.public_url` / `[admin].redirect_url` 与 HTTPS。

页面：Dashboard、Accounts（CodeArts 可用，其他 Provider 标注“待接入”）、Models、Check-ins、Usage、
Settings。前端为 `web/` 下的原生 HTML/CSS/JS，由 `rust-embed` 打包；无 Node 运行时、无外部 CDN、
无内联脚本（CSP `default-src 'self'`）。视觉仍采用 dsh-wtq-pool 的 quiet instrument 语言：深色背景、
细边线、青绿强调、左侧导航、状态 pill、深浅主题切换、响应式布局。

空状态与未知语义：没有账号/请求/签到时显示明确空状态；额度未知显示“未知”，不显示 0 或 ∞；
本地认证状态与实时探测结果分开标注，不把本地记录当作上游验证。

### 9.1 从 codearttray 借鉴的管理面功能

`codearttray` 是 `codearts2api` 同一工程的 Windows 托盘前端（见 `RESEARCH.md` §10.17），**不是独立 Provider，也不进入 Provider 列表**。它调用的 `/api/ui/*` 给出了一份真实的管理面需求清单，Rust 版将其作为 Web 管理 API 的功能输入，而不是桌面壳移植。

| 能力 | 观察到的接口 | Rust Admin API 对应 |
| --- | --- | --- |
| 账号列表/身份 | `GET /api/ui/accounts` | `GET /admin/accounts`（公开字段，无凭据） |
| 移除账号 | `DELETE /api/ui/accounts/{id}` | `DELETE /admin/accounts/{id}`（二次确认 + 审计） |
| 单账号刷新/探测 | `POST /api/ui/accounts/{id}/refresh`、`/probe` | `POST /admin/accounts/{id}/refresh|probe` |
| 路由策略读写 | `GET/POST /api/ui/routing` | `GET/PUT /admin/routing` |
| 用量与日志 | `GET /api/ui/usage`、`/api/ui/logs` | `GET /admin/usage`、`/admin/logs` |
| 凭据导入/导出 | `/api/ui/credentials/*` | 仅加密导出；默认关闭，需显式开启 |
| 额度与签到状态 | 代理上游 `statistics`、`ops/delivery` | 由 Provider `quota()` 与只读活动状态提供 |

**不迁移**：系统托盘、注册表开机自启、桌面通知、剪贴板、隐藏窗口托管网关进程、`Basic admin:<key>` 认证（Rust 统一 Bearer + 常量时间比较）。

> M13-C1 只实现上表中的账号、额度、签到、用量视图。`routing` 读写、`logs` 流、凭据导入/导出仍为
> 后续阶段规划，管理台当前不提供入口。

**签到门控**：`/v1/ops/claim` + `/v1/ops/confirm` 属于权益领取。默认不实现；仅当上游明确允许自动化、且运维书面确认后，才作为默认关闭的可选模块，并记录来源与限制。

## 10. 安全

- 默认绑定 `127.0.0.1`；公网部署依赖反向代理 + TLS。
- 管理认证：Argon2id 密码哈希、恢复码/会话 token 仅存 SHA-256、HttpOnly + SameSite=Strict 会话
  Cookie、CSRF 头、Origin/Host 精确校验、登录限速、首次设置仅 loopback。无默认密码。
- Gateway API Key / 管理 Token 仅存 SHA-256；凭据 AES-256-GCM 加密，AAD 绑定 account id（`polygate:credential:v1:<account_id>`，跨账号解密必然失败），主密钥来自环境变量或 0600 文件，解码后的密钥缓冲区即时零化。
- 敏感字段脱敏；日志不打印 Authorization/Cookie/token；管理 API 不返回凭据。
- 管理会话 TTL；修改类操作要求 CSRF 与二次确认；M13-C1 未实现独立审计日志视图。
- 优雅退出：停止接收新请求、等待在途请求、flush 用量、关闭连接池。
- DB 备份：提供 `VACUUM INTO`/文件快照说明，不实现自动外传。
- 环境变量覆盖配置；`.env.example` 不含真实密钥。

## 11. 部署与性能测量

- 已提供 `deploy/Dockerfile`（多阶段、非 root、data 卷）与
  `deploy/polygate.service`（`NoNewPrivileges`、`ProtectSystem=strict`、`PrivateTmp`、
  `MemoryDenyWriteExecute` 等）。详细步骤见 `docs/DEPLOYMENT.md`。
- 2026-10-29 实测（Windows 笔记本、release、Mock 上游、16 连接 × 100 请求、
  每请求写请求日志）：约 1878 req/s，p50 4.13 ms，p95 21.06 ms，工作集 16.7 MB。
  该数字只衡量网关自身开销；Linux/VPS 与真实上游尚未测量。
- 测量方法：Linux VPS 上 `/proc/<pid>/status` VmRSS、`/proc/<pid>/smaps_rollup` PSS、`/proc/<pid>/stat` CPU、请求延迟直方图、启动到 `/health` 可达时间。
- 场景：空闲、10/100 账号、持续请求、10 并发 SSE、断连。
- 目标仅为预算，发布时附真实测量表；未测则写“未测量”。

## 12. 关键决策与待定项

| 项 | 当前决策 | 待确认 |
| --- | --- | --- |
| 许可证 | **未决**：候选 MIT 或 Apache-2.0，尚未选择 | 发布前由用户确认，并完成依赖许可证清点（见 `RESEARCH.md` §13） |
| 管理认证 | Argon2id 密码 + HttpOnly 会话 Cookie + CSRF；恢复码重置；API Key 不授予管理权限 | 是否增加多管理员/2FA（后续） |
| 模型路由 | 持久化别名 + 多目标 + `provider::model`；别名不得覆盖原生 ID（M15） | 是否需要按请求参数（上下文长度/成本）动态选目标 |
| 调度 | 轮询/加权/最少连接 + 额度/冷却过滤；不确定失败绝不重发（M15） | 是否加入跨 Provider 的显式 failover 策略 |
| 配置存储 | TOML + 环境变量；运行设置入库 | 是否支持热重载非敏感项 |
| 前端 | 原生 HTML/CSS/JS，`rust-embed` 嵌入（M13-C1） | 是否提供无 JS 降级 |
| 真实 Provider 验证 | 需运维提供凭据 | 验证前不得标 Verified |
| CodeArts 国际站 | 预留配置 | 是否真实可用未知 |
| WorkBuddy 设备令牌 | 不生成，只接受显式静态配置 | 上游是否强制未知 |

## 13. 迁移优先级

顺序即依赖顺序：后一项建立在前面已通过验收的模块之上。**不得跳过 1–4 直接实现任何真实 Provider。**

| 序 | 阶段 | 目标 | 退出条件（可验证） |
| --- | --- | --- | --- |
| 1 | Rust Core | `config`、`error`、`security`、`storage` + `migrations/0001`、`protocol` 骨架、`Provider` trait + Registry、Mock Provider、`/health` | `cargo test` 全绿；Mock 走通三协议各一条请求；密钥加密与脱敏有单测 |
| 2 | CodeArts 账号池 | Account Manager（凭据加密、刷新、状态机）、Scheduler v1（SWRR/RR/Least-Conn）、冷却、并发租约 | 两账号 Mock 100 会话 50:50；权重 2:1；超并发返回 `NoCapacity`；取消后租约归零 |
| 3 | 会话粘性 | Session Manager：显式 ID → metadata → 前缀指纹；绑定持久化、TTL、失效语义 | 同会话连续请求账号不变；重启后绑定恢复；绑定失效返回可解释错误不静默迁移 |
| 4 | API 兼容 | OpenAI Chat、Anthropic Messages、OpenAI Responses 适配器；SSE 状态机、工具增量、取消、错误映射 | 三协议对 Mock 的流式/非流式、工具调用、取消、断连测试全部通过 |
| 5 | CodeArts Provider | 在 trait 后实现真实上游：STS/HMAC/DPoP、模型目录、会话头、额度、错误分类 | 真实凭据联调通过后才 `Verified`；无凭据时保持 `experimental` |
| 6 | Web UI | 嵌入式管理台（独立实现）：Dashboard、Accounts、Models、Check-ins、Usage、Settings；账号通过浏览器授权接入 | 页面数据全部来自真实服务；无 Mock 冒充健康；暗色主题可用；M13-C1 已完成，真实 CodeArts 登录待验证 |
| 7 | 其他 Provider | Qoder CN/Intl、TRAE CN/Intl、WorkBuddy CN/Intl（CodeBuddy、ZCode 排除）；按可验证性排序 | 每个 Provider 独立完成 §13.2 能力核对与实测，各自维护状态 |

> CodeArts 是一个整体工程：`codearts2api` 为核心服务，`codearttray` 只是同一工程的 Windows 托盘前端。第 2、5 级只针对服务端核心；托盘仅提供管理面参考（§9.1），**不计入 Provider 数量**。

**排序依据**

1. CodeArts 的账号池与调度在本地 Go 工程有最完整的参考实现（§10），风险最低，最适合验证调度器设计。
2. Qoder（COSY）、TRAE（设备身份）、WorkBuddy（设备令牌/风控）依次递增协议与合规风险，放后。
3. ZCode 已明确排除：AGPL-3.0、依赖第三方验证码参数，且不在必需 Provider 列表内；仅保留历史审计记录，不实现。
4. Web UI 依赖前三阶段稳定的真实 API，提前实现只能对着 Mock 画界面，无法验证状态真实性。

## 14. 运行时、配置与 CLI（M9 已实现）

### 14.1 启动链路

`runtime::Gateway::bind` 完成所有可失败的启动步骤后才绑定端口；`Gateway::serve` 才真正接收请求。

| 序 | 步骤 | 失败行为 |
| --- | --- | --- |
| 1 | `Config::load` / `load_with_env`（TOML + 环境变量） | 解析错误、未知字段、非法监听地址、非法日志级别、空/过短/示例 API Key → 拒绝启动 |
| 2 | 日志初始化（`EnvFilter`） | 级别在配置校验阶段已拒绝；`try_init` 避免测试重复安装 |
| 3 | 网关 API Key（`Config::gateway_keys`） | 为空 → `Config` 错误；不打印任何 key |
| 4 | 主密钥（`SecretBox::from_env_or_file`） | 无效 → 拒绝启动；错误信息不含密钥内容 |
| 5 | SQLite（`Storage::open_url` + 迁移） | 打开/迁移失败 → 拒绝启动 |
| 5.5 | `AccountService`（复用同一 `Storage` + `SecretBox`） | 纯构造；无账号时不写任何行 |
| 5.6 | `CheckinService` + `CheckinScheduler`（Mock executor；无默认 schedule） | 纯构造；循环启动后先恢复中断运行，再按 DB 中的 schedule 工作 |
| 5.7 | `SessionBindingService`（`LimitPolicy::codearts()`；不自动绑定） | 纯构造；无绑定时不写行 |
| 5.8 | `AdminState`（`[admin].enabled`；使用已绑定端口的回调地址） | 纯构造；`admin_users` 为空时进入首次设置 |
| 6 | Provider Registry（仅 `enabled=true`；Mock + CodeArts） | 未实现 kind 或无启用 Provider → 拒绝启动 |
| 7 | 模型目录（`ModelCatalog::discover_with_accounts`） | 使用每个 Provider 第一个可用账号的凭据；发现失败/无账号只记录并跳过，不伪造列表 |
| 8 | `ApiState` + `api::router`；合并 `admin::router` | 无（纯构造） |
| 9 | `TcpListener::bind`（默认 `127.0.0.1:8787`） | 地址非法/被占用 → 拒绝启动 |
| 10 | `axum::serve(...).with_graceful_shutdown(...)` + `ConnectInfo<SocketAddr>`，结束后 `Storage::close` | 服务器错误 → `Internal`；连接池关闭后 `is_closed()` 为真 |

### 14.2 配置优先级

1. 显式 `--config` / `--data-dir`；
2. `POLYGATE_*` 环境变量（`POLYGATE_CONFIG` 指定配置文件）；
3. `<data-dir>/config.toml`；
4. 内置默认值（`127.0.0.1:8787`、`data/polygate.db`、零配置 Mock）。

环境变量覆盖 TOML。`Config::load_with_env` 允许测试注入环境查询，不修改进程环境。

**密钥规则**：网关 API Key 至少 16 字符、拒绝空串与示例占位值（`change-me` 等）；主密钥只来自 `POLYGATE_MASTER_KEY` 或 0600 的 `master.key`；配置结构使用 `deny_unknown_fields`，在 TOML 中写 `master_key` 会直接报错。

### 14.3 CLI

```text
polygate serve            # 启动 HTTP 网关（Ctrl+C / SIGTERM 优雅关闭）
polygate doctor           # 配置、API Key、主密钥、SQLite+迁移、Provider、监听地址
polygate config-example   # 输出 config.example.toml
```

`doctor` 只读检查：打开数据库并应用待执行迁移后立即关闭，不删除/不改写已有数据；不打印 key 或主密钥值。`doctor` 在任一检查失败时以非零码退出。

### 14.4 M9/M13-C1 边界

- Mock 与 CodeArts 已接入 Chat；CodeArts 通过 `SessionRouter` 做账号粘性/并发；完整调度器与额度聚合仍未接入。
- 管理台已可完成首次设置、登录、CodeArts 浏览器授权、账号管理与只读概览；真实 CN 账号已跑通
  授权、模型、额度、Chat、SSE、工具调用、多轮与会话粘性，刷新/签到/Intl 仍未验证。
- 请求使用占位 `Credential`（Mock 忽略）；真实凭据通过管理台授权后由 `SecretBox` 加密入库。
- 模型目录在启动时发现一次；运行时启停只影响可见性与可路由性。
- 信号处理：Unix 监听 SIGTERM 与 Ctrl+C，Windows 仅 Ctrl+C；测试通过同一 shutdown future 验证关闭路径。
- 无 TLS、Docker、systemd（第 7 级），部署时置于反向代理之后。