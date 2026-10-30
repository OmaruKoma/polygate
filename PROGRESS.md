# PROGRESS

> 最后更新：2026-10-29
> 当前阶段：**M15 完成**（§40）：跨 Provider 回归审计、持久化模型别名/多目标路由、
> 账号调度（轮询/加权/最少连接/额度/冷却）、三种兼容 API（OpenAI Chat、OpenAI
> Responses、Anthropic Messages）、管理台模型与请求日志、Linux 部署物与性能基线。
> M14-E（TRAE 认证/额度/签到）仍为 **MockVerified**，无真实账号。
> ZCode 与 CodeBuddy 均明确排除，不实现、不注册、不出现在 UI。
> 真实验证范围：CodeArts CN（§26）、Qoder CN（§28–§31）已 LiveVerified；
> WorkBuddy、TRAE 仅 MockVerified（§36–§39）。
> 下一步：真实 TRAE/WorkBuddy 上游只读探针需凭据与显式授权（等待指令）；
> Release 发布与 `LICENSE` 仍待来源清点完成。
> §43 新增可选客户端身份伪装（`client.identity = "impersonate"`），默认仍为 honest；
> 三个 Provider 均已接入，Qoder CN 已真实对照验证，WorkBuddy/TRAE 未联调。
> 注：本项目根目录已于 2026-10-28 创建精简版 `AGENTS.md`（此前不存在）。

## 1. 当前项目状态

| 项 | 状态 |
| --- | --- |
| 工作目录 | `<repo>`（独立 Git 仓库，`main`，尚无提交） |
| 参考仓库 | 5 个，位于 `.refs/`（本阶段限制下达前已克隆；本阶段**未再下载**）。OpenZWork 已于 2026-10-28 按要求彻底移除。`.refs/liu5269_zcode2api` 仅作历史审计保留，ZCode 已排除、不得据此实现 |
| Provider 范围 | **必须支持**：CodeArts（保留）、WorkBuddy（优先）、TRAE、Qoder。**ZCode 与 CodeBuddy：显式排除**（CodeBuddy 于 2026-10-29 移除），不实现、不注册、不出现在模型列表与 UI |
| 研究文档 | `docs/RESEARCH.md`（§9 Coding2API、§10 CodeArts 账号池、§11 dsh-wtq-pool、§12 ZCode（历史审计，已排除）、§13 许可证审计、§14 结论索引）、`docs/PROVIDERS.md`（§10 CodeBuddy、§11 对照、§12 dsh 审计、§13 Provider 能力矩阵）、`docs/ARCHITECTURE.md`（含 §2.1–2.3 模块职责/Registry/协作边界、§13 迁移优先级、§14 运行时/配置/CLI）；**M10 新增** `docs/PROVIDER_MIGRATION.md`、`docs/CHECKIN_RESEARCH.md`、`docs/MIGRATION_MATRIX.md`；**M13 补充新增** `docs/CODEARTS_SESSION.md`；**M13-D1 新增** `docs/QODER_PROVIDER_RESEARCH.md`；**M14-A 新增** `docs/WORKBUDDY_MIGRATION.md`；**M14-D 新增** `docs/TRAE_MIGRATION.md` |
| 后端实现 | **M1–M13-D2-E 完成**：配置、错误、安全、存储、协议、Provider 契约/Registry、Mock Provider、OpenAI Chat API（JSON+SSE）、运行时与 CLI；M11 AccountService/CredentialService；M12 签到引擎；M13-A 会话绑定服务；M13-B CodeArts Provider 与签到执行器；M13-C1 管理台与 CodeArts 浏览器授权；M13-D2-A/B/C Qoder 协议、设备码授权、额度、签到、模型发现与推理 Adapter；M13-D2-D 错误/流式稳定性；M13-D2-E 会话租约生命周期测试；**M14-A WorkBuddy 协议与架构适配研究（仅文档，未改代码、未发真实请求）**；**M14 范围调整：移除 `ProviderKind::Zcode` 与 UI 占位（2026-10-29）**；**M14-B WorkBuddy 核心 Provider（credential/error/models/convert/events/http/provider，2026-10-29）**；**M14-C WorkBuddy 刷新/设备码/额度/签到与跨进程租约（2026-10-29）**；**M14-D TRAE 协议核心（credential/region/error/models/convert/events/http/provider，2026-10-29）**；**M14-E TRAE 认证生命周期/额度/签到（auth/quota/checkin、admin 授权流、运行时与 UI 接入，2026-10-29）** |
| 账号服务 | `src/account_service/`：账号 CRUD/启停/认证状态机、授权上下文、凭据加密保存/修订号 CAS 更新/失效标记；已挂到 `Gateway::accounts()` |
| 会话绑定 | `src/session_service/`：持久化粘性、CAS 改绑、显式失效/恢复、账号级/会话级有界并发；`SessionRouter` 已接入 `/v1/chat/completions`（账号选择、凭据读取、SSE 全生命周期持有租约）；M13-D2-E 增加断连/空闲超时/同会话并发/额度耗尽/失效/重启的端到端租约测试（§33） |
| 签到引擎 | `src/checkin/`：签到状态机、批量/全部、持久化去重、定时策略、崩溃恢复；`RoutingCheckinExecutor` 按 (kind, region) 分派 Mock / CodeArts；`LiveQuotaRefresh` 在成功后写入真实额度快照（CodeArts）；管理台只读展示记录 |
| Provider 实现 | Trait + Registry + Mock；**CodeArts Provider**（签名/模型/额度/Chat/SSE/工具/凭据刷新，标记 `experimental`，CN 基础能力已真实登录验证，见 §26）；**Qoder Provider**（认证/模型/额度/Chat/SSE/工具/签到全部实现；CN 已真实验证授权、模型、额度、签到、Chat、SSE、工具与多轮；见 §28–§31）；**WorkBuddy**（M14-B 核心 Provider + M14-C 刷新生命周期/设备码/额度/CN 签到；状态 **MockVerified**，未真实联调，见 §36/§37）；**TRAE**（M14-D 协议核心 + M14-E 认证刷新/额度/CN 签到/管理台授权；状态 **MockVerified**，无真实账号，见 §38/§39）；**ZCode：明确排除，无实现** |
| HTTP API | `GET /health`、`GET /v1/models`、`POST /v1/chat/completions`（JSON + SSE）、**`POST /v1/responses`**、**`POST /v1/messages`**（Anthropic），API Key 鉴权；三接口共享模型别名 → 调度 → Provider 链路（§40.6）；**管理 API** `/admin/api/*`（会话 + CSRF），新增模型路由、调度快照与请求日志（§40.3/§40.4） |
| Web UI | **M13-C1 完成**：嵌入式管理台（Dashboard/Accounts/Models/Check-ins/Usage/Settings）、首次设置/登录/恢复码、CodeArts 浏览器授权、账号管理与只读概览；**M13-D2-B** 增加 Qoder 子导航与设备码授权弹窗（浏览器实测，见 §29.5）；**M14-C/M14-E** 增加 WorkBuddy/TRAE 子导航、授权弹窗（TRAE 为设备 ID + 粘贴回调）与 CN 签到；**2026-10-29 移除 CodeBuddy 子导航**，现在只保留 CodeArts/Qoder/TRAE/WorkBuddy |
| 测试 | `cargo test --workspace`：**646 个测试通过**（2026-10-29 实测，见 §43.5）；§43 新增 17 个（身份档案单测 5、WorkBuddy 4、Qoder 3、TRAE 3、配置解析 2）。全部离线运行 |
| 真实 Provider 验证 | **CodeArts 1 个 CN 账号**：认证、模型、额度、Chat、SSE、Tool Calling、多轮、会话粘性已 Live 验证；刷新/签到/Intl 未验证（见 §26）。**Qoder 1 个 CN 账号**：设备码授权、userinfo、落库、额度查询、签到领取（+100 credits，额度 100→200）、模型发现、Chat、SSE、Tool Calling、多轮工具结果已 Live 验证；刷新/Intl/并发仍未验证（见 §29.6、§30.4、§31.4）。**WorkBuddy：无真实账号，未发起任何真实请求（MockVerified only）**。**TRAE：无真实账号，未发起任何真实请求（MockVerified only）**；授权/刷新/额度/签到均未真实联调 |
| 发布物 | 新增 `README.md`、`docs/API.md`、`docs/ROUTING.md`、`docs/DEPLOYMENT.md`、`deploy/polygate.service`、`deploy/Dockerfile`、`deploy/container-config.toml`；release 构建已实测（§40.7）。`LICENSE` 仍暂不生成（暂定 MIT，来源清点未完成） |

## 2. 工作区中已存在的文件（如实记录）

上一轮任务中断前创建的**脚手架**已在 M1 中保留并改造，当前状态如下：

```text
Cargo.toml                      # 依赖清单（已锁定，见 §10）
Cargo.lock                      # 新生成：依赖锁定
rust-toolchain.toml             # 新生成：固定 Rust 1.98.1
AGENTS.md                       # 新生成：精简开发规范
config.example.toml             # 新生成：示例配置（无密钥）
src/lib.rs                      # 新生成：库入口，声明已有模块
src/main.rs                     # 新生成：最小二进制入口（仅打印版本）
migrations/0001_init.sql        # SQLite schema v1（M4 已执行/验证）
src/config/mod.rs               # 配置结构 + 环境变量覆盖草案（部分属 M9）
src/error/mod.rs                # 统一错误分类 / HTTP 状态 / 错误体 / 脱敏（M2 已完成）
src/security/mod.rs             # AES-256-GCM（AAD 绑定 account id）/ 主密钥 / token SHA-256 / 脱敏（M3 已完成）
src/accounts/mod.rs             # Account / Credential（M4 增补 NewAccount.kind）
src/models/mod.rs               # 模型注册表草案
src/quota/mod.rs                # QuotaSnapshot / Group 草案
src/sessions/mod.rs             # Session 草案 + client_session_id / kind（M4）
src/storage/mod.rs              # Storage 连接池 / 迁移 / 关闭 / 事务建号（M4 已完成）
src/storage/{accounts,credentials,sessions,quota,usage,settings}.rs  # Repository（M4）
src/storage/tests.rs            # 存储层单测（M4）
src/protocol/mod.rs             # 内部协议入口 / 序列化策略（M5 已完成）
src/protocol/{message,request,response,stream,validate}.rs  # 消息、请求、响应/Usage、流事件、顺序校验器（M5）
src/protocol/tests.rs           # 协议与状态机单测（M5）
src/providers/mod.rs            # Provider trait / Capabilities / ProviderInfo / VerificationState / 能力分级（M6）
src/providers/registry.rs       # (kind, region) Registry：注册、查询、启停、能力/验证状态（M6）
src/providers/tests.rs          # Registry 与默认 Unsupported 单测（M6）
src/providers/mock.rs           # 确定性 Mock Provider：非流式/流式、故障注入、可取消流（M7）
src/providers/mock_tests.rs     # Mock Provider 行为单测（M7）
src/api/mod.rs                  # Axum Router、API Key 鉴权中间件、/health、/v1/models、/v1/chat/completions（M8）
src/api/openai.rs               # OpenAI Chat Completions DTO 与请求/响应转换（M8）
src/api/catalog.rs              # 从已启用 Provider 发现模型并解析路由（M8）
src/api/sse.rs                  # StreamEvent → OpenAI chat.completion.chunk SSE（M8）
src/api/tests.rs                # Router 级集成测试（M8）
src/cli/mod.rs                  # clap serve/doctor/config-example、日志、信号、doctor 检查（M9）
src/cli/tests.rs                # CLI/配置优先级/doctor 测试（M9）
src/runtime/mod.rs              # Gateway::bind/serve：配置→安全→SQLite→Registry→Router→Listener（M9）
src/runtime/tests.rs            # 真实 TCP 端到端、关闭、持久化、启动失败测试（M9）
src/admin/mod.rs                # 嵌入式管理台：AdminState/Settings、路由、安全头、错误 JSON（M13-C1）
src/admin/auth/{mod,flow}.rs    # 管理员设置/登录/恢复码/会话/CSRF/Origin/限速 + 授权流程状态机（M13-C1）
src/admin/account/mod.rs        # 授权结果落库、身份去重、账号视图脱敏（M13-C1）
src/admin/api/mod.rs            # Dashboard/账号/模型/签到/用量/授权端点、OAuth 回调页（M13-C1）
src/admin/tests.rs              # Mock STS + 真实 Axum Router 集成测试（M13-C1）
src/providers/codearts/login.rs # CodeArts 浏览器授权：PKCE/DPoP/token 交换/身份查询（M13-C1）
src/providers/qoder/{mod,region,cosy,error,convert,events}.rs  # Qoder 协议基础层（M13-D2-A）
src/providers/qoder/{auth,credential}.rs  # Qoder 设备码授权/刷新/凭据（M13-D2-B）
src/admin/auth/qoder.rs         # Qoder 授权流程生命周期与秘密解密（M13-D2-B）
src/providers/qoder/fixtures/ # Qoder 离线 SSE/错误夹具（M13-D2-A）
web/{index.html,app.css,app.js,oauth-callback.js}  # 原生静态前端，rust-embed 嵌入（M13-C1）
migrations/0005_admin.sql       # external_id、管理员/会话/授权流程表（M13-C1）
src/{scheduler,metrics}/        # 空目录占位（未声明模块）
tests/ deploy/ .github/workflows/                       # 空目录占位
```

**注意事项**

- 以上模块已通过 `cargo build`，但只代表“可编译”；除 §10–§12、§15–§18 所列测试覆盖的行为外，**不代表功能已验证**。
- 空目录仍为后续里程碑占位，未在 `src/lib.rs` 中声明，因此不参与编译。
- 当前可运行 Gateway（Mock Provider + OpenAI Chat API），但无调度器、无 Web UI、无真实 Provider。

## 3. 已发现的源码清单

| 路径 | 语言 | 许可证 | 用途 |
| --- | --- | --- | --- |
| `~/codearts2api` | Go（主）+ 旧 Python | **无 LICENSE 文件；操作者本人作品**（2026-10-29 确认） | CodeArts 协议事实；代码可复用（作者授权），复制时保留版权声明并标注来源 |
| `~/codearttray` | Go | **无 LICENSE 文件；操作者本人作品**（2026-10-29 确认） | **`codearts2api` 同一工程的 Windows 系统托盘前端，不是独立 Provider**；通过网关 `/api/ui/*` 做账号列表/删除、额度与签到；代码可复用（作者授权）；托盘/自启/桌面 UI 默认不迁移（设计取舍） |
| `~/workbuddy2api` | Python | MIT | WorkBuddy/CodeBuddy 协议与错误分类参考 |
| `.refs/RobbsLuo_Coding2API` | Python | MIT | 多 Provider、三协议适配、调度参考 |
| `.refs/chengzhi-c_dsh-wtq-pool` | TypeScript | MIT | UI 视觉语言；TRAE/WorkBuddy/Qoder 协议 |
| `.refs/Johnsheng1_codearts2api` | Python | MIT | CodeArts 额度/模型/流式 |
| `.refs/liu5269_zcode2api` | Python/Node | **AGPL-3.0** | **ZCode 已排除**；仅保留历史审计，不实现、不复制、不迁移验证码/指纹 |
| `.refs/yetone_magpie` | Go | MIT | 路由/额度/会话粘性设计思路 |

补充事实：本地 CodeArts 工程的 Git 远端为 `github.com/handsomedogx/codearts2api`，无 LICENSE 文件，但为操作者本人作品（2026-10-29 确认），代码可复用；其 Go 源码完整（`app/`、`cmd/`、`internal/`），旧 Python FastAPI 副本仍并存。`workbuddy2api` 有 2 个未提交补丁（加密凭据只读、空 `finish_reason` 归一为 `null`）。

## 4. 尚未研究的仓库 / 内容

- **Magpie**：仅摘要级阅读；路由算法、预算预留、steady-state 调度、会话粘性细节**未细读**。这是唯一尚未深入源码的参考仓库。
- **Coding2API 测试套件**：源码静态审计已完成（见 §8），但**测试未运行**（Python 环境与依赖未准备）。
- **CodeArts 账号池实现**：本地 Go 工程关键路径静态审计已完成（见 §10）；Johnsheng1 Python 参考为单账号实现。未运行测试，未逐行审计无关模块。
- **真实上游**：CodeArts CN 已用 1 个真实账号验证授权/模型/额度/Chat/SSE/工具/多轮/会话粘性（§26）；CodeArts 刷新/签到/Intl、Qoder/TRAE Intl、WorkBuddy/CodeBuddy 共享积分均未验证。Qoder 只有源码级研究（§27），无真实请求。
- 各参考仓库的依赖许可证未逐一清点（仅 root LICENSE 已核对，见 `docs/RESEARCH.md` §13）。
- **CodeArts 只有一个 Provider**：核心是 `codearts2api` 服务，`codearttray` 是同一工程的 Windows 托盘前端。托盘的后端调用已记录在 `docs/RESEARCH.md` §10.17，仅用于管理 API 需求参考。
- **签到未确认合规**：`/v1/ops/claim` + `/v1/ops/confirm` 可调用 ≠ 上游允许自动化。默认不实现，门控条件见 `docs/RESEARCH.md` §10.17。

## 5. 已知风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| 许可证污染 | 无法以 MIT 发布 | GPL/AGPL 仅读事实；第三方无 LICENSE 代码不复制；本地 `codearts2api`/`codearttray` 为操作者本人作品，可按授权复用 |
| 协议未实测 | Provider 可能不可用 | 初始标 `experimental/unavailable`；实测后才 `verified` |
| 私有协议复杂（Qoder COSY） | 实现成本高、易失效 | 先完成 Mock + 调度 + 协议层，再实现 Provider |
| 上游风控/设备令牌 | WorkBuddy 可能拒绝非官方客户端 | 不伪造指纹；接受显式静态头；文档声明限制 |
| 额度语义不一致 | 误报余额 | Unknown 优先；共享组去重；不换算 token |
| 会话粘性与上游会话混淆 | 错误恢复 | 区分下游绑定与上游原生会话；非幂等任务禁止迁移 |
| 低内存目标 | 可能达不到 30 MB | 实测；不用重型运行时/浏览器；连接池与缓存有界 |
| 脚手架仅“可编译” | 可能被误读为功能完成 | §2 与 §10 明确区分“编译通过”与“已验证”；未测项逐条列出 |
| 签到/权益领取合规 | 可能违反上游条款 | 默认不实现；仅在上游明确允许自动化 + 运维书面确认 + 真实账号验证后，作为默认关闭的可选项 |
| 本地工程许可证状态 | 影响复用判断 | `codearts2api`、`codearttray` 无 LICENSE 文件，但均为操作者本人作品（2026-10-29 确认），代码可复用；复制时保留版权声明并在 PolyGate `LICENSE`/`NOTICE` 标注来源；第三方依赖仍需核对 |

## 6. 下一步任务

前置决策已于 2026-10-28 确认：暂定 MIT（暂不生成 LICENSE）、保留并改造脚手架、不复制 AGPL/第三方无许可证代码（本地 `codearts2api`/`codearttray` 为操作者本人作品，2026-10-29 确认可按授权复用）、OpenZWork 已移除。

1. **M1 — 已完成**（见 §10）。
2. **M2 — 已完成**（见 §11）。
3. **M3 — 已完成**（见 §12）。
4. **M4 — 已完成**（见 §13）。
5. **M5 — 已完成**（见 §14）。
6. **M6 — 已完成**（见 §15）。
7. **M7 — 已完成**（见 §16）。
8. **M8 — 已完成**（见 §17）。
9. **M9 — 已完成**（见 §18）。
10. **M10 — 已完成**（源码审计与迁移规划，见 §19；只产出文档，未改代码/Schema）。
11. **M11 — 已完成**（账户与凭据生命周期，见 §20；新增 `account_service` 与迁移 0002）。
12. **M12 — 已完成**（持久化签到任务引擎，见 §21；新增 `checkin`、迁移 0003；仅 Mock）。
13. **M13-A 补充要求 — 已完成**（CodeArts 会话墙与本地源码差异、会话绑定最小机制，见 §22；新增 `session_service`、迁移 0004）。
14. **M13-B — 已完成**（CodeArts Provider Adapter、会话路由接入、CodeArts 签到执行器，见 §23）。
15. **M13-C — 已阻塞关闭**（无授权凭据进入 PolyGate，未发出真实请求；见 §24 与 `docs/CODEARTS_LIVE_VERIFICATION.md`）。
16. 下一里程碑为 **M14 安全账号导入/最小管理面**，之后再重跑 CodeArts 真实联调（签名/STS 刷新/会话墙/签到）；见 §24.6 与 `docs/MIGRATION_MATRIX.md` §5。
16. 阶段级依赖以 `docs/ARCHITECTURE.md` §13 为准；更细的 M11+ 顺序与退出条件以 M10 的 `docs/MIGRATION_MATRIX.md` §5 为准。

> 本文件早期版本用“阶段 1–6”描述后续计划，与 `ARCHITECTURE.md` §13 的七级顺序存在编号差异；现统一以 §13 为准，“阶段 02”指其中的第 1 级（Rust Core）。

## 7. 本阶段执行记录

- 2026-10-28：检查工作目录、Git、Rust/Node 工具链、本地相关工程。
- 2026-10-28：完成 6 个参考仓库的许可证与协议审计（只读），形成能力矩阵。
- 2026-10-28：创建 `docs/RESEARCH.md`、`docs/PROVIDERS.md`、`docs/ARCHITECTURE.md`、`PROGRESS.md`。
- 2026-10-28：**未实现、未修改**任何 Provider / Gateway / UI / 用户业务代码；未调用真实上游接口。
- 验证记录：无真实 Provider 测试；无测试通过记录；无性能数据。
- 2026-10-28：读取本文件后，对 `.refs/RobbsLuo_Coding2API`（MIT，0.10.0，浅克隆 HEAD `df99362`）做 10 项功能静态审计；审计结果写入 `docs/RESEARCH.md` §9、`docs/PROVIDERS.md` §10/§11。
- 2026-10-28：审计阶段**未编写 Rust Provider、未复制原项目源码、未调用真实上游**。
- 2026-10-28：完成 CodeArts 账号池静态审计，新增 `docs/RESEARCH.md` §10；对照 `.refs/Johnsheng1_codearts2api` 单账号 Python 实现与本地 `~/codearts2api` Go 账号池。未启动服务、未运行测试或调用上游；未读取 Go 工程 `.env`。
- 2026-10-28：A/B 场景的 100 会话分配为源码推演，非运行测量；区分了实际实现、README 声明和 Rust 需新增能力。
- 2026-10-28：完成 `.refs/chengzhi-c_dsh-wtq-pool` 静态源码审计（版本 `1.2.0`，提交 `f7888dc5759bb1e5a4360ac0c18ccabf28a88ca8`，MIT）。覆盖 Qoder CN/International、TRAE CN/International、WorkBuddy、CodeBuddy 的凭据来源、端点、模型目录、额度、账号池、会话生命周期、SSE、区域差异和未实现边界；结果写入 `docs/RESEARCH.md` §11 与 `docs/PROVIDERS.md` §12。
- 2026-10-28：完成 dsh renderer/UI 静态审计；确认 renderer 为原生 HTML/CSS/JavaScript，React 仅用于宿主卡片和 composer 额度组件，动态操作经 loopback panel bridge，未编写前端或 Rust 代码。
- 2026-10-28：dsh 审计期间未安装依赖、未运行测试、未启动服务、未读取真实凭据、未调用真实上游；所有相关 Provider 继续保持 `experimental` / `unavailable`，没有新增 `verified` 声明。
- 2026-10-28：完成 `.refs/liu5269_zcode2api` 静态源码审计（AGPL-3.0），覆盖 Provider 分支、账号状态机、SQLite schema、认证/凭据存储、模型列表、Anthropic Messages 透传、字节级 SSE、额度轮询与错误分类；结果写入 `docs/RESEARCH.md` §12。未运行其 Node 验证码子项目，未调用真实上游。
- 2026-10-28：按用户指令彻底移除 `.refs/BlueSkyXN_OpenZWork`（GPL-3.0，289 MB）及 `docs/RESEARCH.md`、`PROGRESS.md` 中全部 OpenZWork 引用；不再研究或参考其 Agent Runtime。其余参考仓库与项目目标不变。
- 2026-10-28：完成研究汇总。新增 `docs/PROVIDERS.md` §13（Provider 能力矩阵、已验证=0、状态变更规则）、`docs/RESEARCH.md` §13（逐个 LICENSE 文件核对）与 §14（结论索引）、`docs/ARCHITECTURE.md` §2.1–2.3（模块职责与依赖、Provider Registry、账户/调度/会话/额度协作边界）与 §13（迁移优先级）；`PROGRESS.md` 新增 §9 阶段 02 任务清单。
- 2026-10-28：许可证核对更正：`~/codearttray` 无 LICENSE（此前记为 MIT），`~/codearts2api` 无 LICENSE；其余 `.refs/` 五个仓库 root LICENSE 与已知一致（4 MIT + 1 AGPL）。
- 2026-10-29：操作者确认 `~/codearts2api` 与 `~/codearttray` 均为本人作品。此前仅按“无 LICENSE”推断“禁止复制”的结论不再适用：两工程代码可按作者授权复用；复制时保留原版权声明，并在 PolyGate `LICENSE`/`NOTICE` 中标注来源；其第三方依赖仍需单独核对。
- 2026-10-28：本次汇总未重新下载或完整重读任何仓库，仅在许可证核对时读取 LICENSE 文件并检查了项目根目录是否存在 `AGENTS.md`（不存在）；未编写实现代码，未调用真实上游。
- 2026-10-28：修正本地工程定位。`codearttray` 是 `codearts2api` 的 Windows 托盘前端，两者同属一个 CodeArts 工程；它不是独立 Provider，不计入 Provider 数。依据：`codearttray/api/api.go:1-4`（托盘不接触华为凭据）、`api.go:89-92`（Basic admin:key + `X-CodeArts-Account`）、`api.go:129/161/175-195`（`/api/ui/accounts`、删除、`/api/ui/codearts/proxy`）。
- 2026-10-28：新增 `docs/RESEARCH.md` §10.17（托盘↔网关调用关系、迁移/不迁移清单、签到门控）、`docs/ARCHITECTURE.md` §9.1（管理面功能输入）与 §13 说明（CodeArts 是一个工程）；未修改任何实现代码。
- 2026-10-28：签到相关接口 `/v1/ops/claim`、`/v1/ops/confirm`、`/v1/ops/delivery` 仅记录为事实；默认不实现，未调用。
- 2026-10-28：**阶段 02 / M1 执行**。新增 `src/lib.rs`、`src/main.rs`、`config.example.toml`、`rust-toolchain.toml`（固定 1.98.1）、`AGENTS.md`；生成 `Cargo.lock`。修复脚手架编译错误（`QuotaUnit` 缺 `Hash`）、clippy 告警（4 处 `from_str` 改名 `from_db_str`、`SchedulerStrategy` 改为派生 `Default`、`security` 的 `let_and_return`），并执行 `cargo fmt`。新增 1 个配置示例解析测试。
- 2026-10-28：M1 验证结果：`cargo build` 成功；`cargo test` 5 passed / 0 failed；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo fmt --check` 通过。**未执行 M2–M9**，未接入真实 Provider，未调用上游，未实现调度器或 Web UI。
- 2026-10-28：**阶段 02 / M2 执行**。重写 `src/error/mod.rs`：`ErrorKind` 重命名对齐 §9（`ModelUnavailable`/`Transport`/`UpstreamServer`/`SessionBindingInvalid`），新增 `Config`、`Storage`，保留 `AccountDisabled`/`Timeout`/`Internal`；新增 `ErrorCategory` 分类、`ErrorKind::ALL`、`GatewayError::public_view`/`log`、`redact_secrets` 脱敏，重写 OpenAI/Anthropic 错误体映射；`sqlx::Error` → `Storage`、`reqwest::Error` → `Timeout`/`Transport`/`UpstreamServer`。`config`、`security` 中 12 处配置类错误改标 `Config`。新增 14 个错误层单测。未新增依赖，未接入真实 Provider。
- 2026-10-28：M2 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 19 passed / 0 failed；`cargo build` 成功（详见 §11.4）。
- 2026-10-28：**阶段 02 / M3 执行**。`src/security/mod.rs` 增加 AES-256-GCM AAD：`account_aad`/`encrypt_with_aad`/`decrypt_with_aad`/`encrypt_for_account`/`decrypt_for_account`，保留原 `encrypt`/`decrypt` 签名（以空 AAD 委托）；主密钥解码后 `zeroize`；密钥文件加载时检查并告警组/其他可读权限。新增 8 个安全层单测（AAD 绑定、错误主密钥、篡改、密钥文件创建/重载/非法内容、token hash、日志脱敏、credential hint）。未新增依赖，未接入真实 Provider。
- 2026-10-28：M3 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 27 passed / 0 failed；`cargo build` 成功（详见 §12.4）。
- 2026-10-28：**阶段 02 / M4 执行**。重写 `migrations/0001_init.sql`（7 张表 + 索引 + 外键；移除尚未使用的草案表）；新增 `src/storage/`（连接池、WAL、busy_timeout、外键、迁移、关闭、6 个 Repository、账号+凭据事务创建）；`sqlx` 启用 `macros`/`migrate` feature（引入 `sqlx-macros` 等构建期传递依赖）；`DatabaseConfig.max_connections` 默认 8 → 4；`accounts::NewAccount` 增加 `kind`，`sessions::Session` 增加 `client_session_id`/`kind`；`security` 新增 `from_master_key` 与 `ENCRYPTION_VERSION`。新增 21 个存储层单测。未新增直接依赖，未接入真实 Provider。
- 2026-10-28：M4 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 48 passed / 0 failed；`cargo build` 成功（详见 §13.5）。
- 2026-10-28：**阶段 02 / M5 执行**。新增 `src/protocol/`（消息/内容块/请求/响应/Usage/FinishReason/8 类流事件 + `StreamEventValidator` 状态机）；`src/lib.rs` 声明 `protocol`；新增 22 个协议与状态机单测。未新增依赖，未修改 schema、Provider、调度器、API 或 UI。
- 2026-10-28：M5 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 70 passed / 0 failed；`cargo build` 成功（详见 §14.5）。
- 2026-10-29：**阶段 02 / M6 执行**。新增 `src/providers/mod.rs`（`Provider` trait：`info`/`capabilities` 必需，其余默认 `Unsupported`；`Capability`/`Capabilities`/`ProviderClass`/`ProviderInfo`/`ProviderKey`/`VerificationState`/`UpstreamModel`/`AgentTask*`/`CancelRequest`/`ChatStream`）、`src/providers/registry.rs`（`(kind, region)` 键、`RwLock` 并发安全、注册/查询/启停/能力/验证状态）、`src/providers/tests.rs`（12 个单测）；`src/lib.rs` 声明 `providers`。未新增依赖，未实现具体 Provider，未修改 schema、调度器、API 或 UI。
- 2026-10-29：M6 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 82 passed / 0 failed；`cargo build` 成功（详见 §15.5）。
- 2026-10-29：**阶段 02 / M7 执行**。新增 `src/providers/mock.rs`（确定性 Mock Provider：非流式/流式、模型列表、能力声明、可配置回复/推理/工具调用/分块/延迟、8 类故障注入、可取消流）与 `src/providers/mock_tests.rs`（21 个单测）；`src/providers/mod.rs` 声明 `mock`/`mock_tests` 并 re-export。未新增依赖，未接真实上游，未修改 schema、调度器、HTTP API 或 UI。
- 2026-10-29：M7 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 103 passed / 0 failed；`cargo build` 成功（详见 §16.7）。
- 2026-10-29：**阶段 02 / M8 执行**。新增 `src/api/`（Axum Router、常量时间 API Key 鉴权中间件、`/health`、`/v1/models`、`/v1/chat/completions` 的 JSON 与 SSE 输出、OpenAI DTO 转换、模型目录发现、SSE 事件映射）与 `src/api/tests.rs`（19 个 Router 级集成测试）；`src/lib.rs` 声明 `api`；`MockProvider` 增加 `request_count()` 供测试断言“未触发上游调用”。未新增依赖，未接真实上游，未修改 schema、调度器、Web UI，未实现 Anthropic/Responses API。
- 2026-10-29：M8 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 122 passed / 0 failed；`cargo build` 成功（详见 §17.8）。
- 2026-10-29：**阶段 02 / M9 执行**。新增 `src/runtime/`（`Gateway::bind`/`serve`：配置→主密钥→SQLite 迁移→Registry→模型目录→ApiState→Router→TcpListener→优雅关闭并关闭连接池）与 `src/cli/`（clap `serve`/`doctor`/`config-example`、日志初始化、Ctrl+C/SIGTERM、doctor 六项检查）；`main.rs` 改为薄入口；配置新增 `logging.level`、`auth.api_keys`、`POLYGATE_LOG_LEVEL`、`POLYGATE_MOCK_ENABLED`、`Config::load_with_env`、监听地址/日志级别/API Key 校验、`deny_unknown_fields`；`Storage::close` 改为 `&self` 并新增 `is_closed`；`config.example.toml` 同步。未新增依赖，未接真实上游，未实现调度器/Web UI/Anthropic/Responses API。
- 2026-10-29：M9 验证结果：`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 无告警；`cargo test` 144 passed / 0 failed；`cargo build` 成功；并以真实 TCP 启动 Gateway，用 curl 验证 `/health`、`/v1/models`、非流式、SSE、401 与 `doctor`/`config-example`（见 §18.8）。

## 8. Coding2API 源码审计记录（2026-10-28）

**对象与状态**

- 路径：`.refs/RobbsLuo_Coding2API`
- 版本：`pyproject.toml` → `coding2api 0.10.0`；许可证 MIT；浅克隆，HEAD `df993624`（提交时间 2026-10-28）。
- 规模：`src/` 约 4.19 万行 Python；`tests/` 273 个测试文件（**未运行**）。
- 方法：先看目录与 LICENSE，再按功能点检索关键模块，分模块精读；未整仓通读。

**十项功能面（路径与迁移结论详见 `docs/RESEARCH.md` §9.2 矩阵）**

| # | 功能 | 关键源码 | 迁移结论 |
| --- | --- | --- | --- |
| 1 | Provider 注册/初始化 | `src/provider/base.py`（Protocol、ErrKind）、`src/provider/<name>/client.py`、`src/main.py` | 借鉴显式注册与环境装配；改为 Rust enum + trait，按配置开关 |
| 2 | Provider 配置结构 | `src/config.py` | 借鉴分 provider 配置分段；Rust 用 serde/toml，凭据走加密存储 |
| 3 | 账号管理 | `src/db/repo.py`、管理 API | 借鉴状态/权重/并发字段；Rust 侧另加租约与冷却持久化 |
| 4 | 凭据处理 | `src/db/crypto.py`、各 provider 的 `credential.py`/`auth.py` | 仅借鉴字段与刷新流程；加密改 AES-256-GCM（不复用 Fernet 无盐派生） |
| 5 | 模型列表/映射 | `src/engine/model_resolver.py`、`src/engine/naming.py` | 迁移“别名→raw_id→provider”解析顺序；模型能力以实测为准 |
| 6 | 三协议兼容 | `src/compat/{openai,anthropic,responses}/`、`src/api/*` | 迁移转换思路与字段映射；在 Rust 内建统一事件层后重写 |
| 7 | SSE | `src/engine/sse.py`、`src/api/streaming.py` | 迁移事件切分/缓冲/错误事件语义；用 Rust 流 + 自动测试重写 |
| 8 | Tool Calling | `src/compat/*/request.py`、`response.py` | 迁移参数增量与 finish_reason 归一；需覆盖并行工具调用 |
| 9 | 错误处理 | `src/provider/base.py` ErrKind、各 client 错误分支 | **最值得迁移**；采用同类冷却/冷却范围/是否换号语义，但用 Rust 枚举重构 |
| 10 | 数据存储 | `src/db/schema.sql`、`repo.py` | 借鉴表结构意图；Rust 侧已起草 SQLite schema，需按租约/额度重排 |

**CodeBuddy 专项（`docs/PROVIDERS.md` §10）**

- 双区端点（CN / Intl）独立配置；凭据字段与必填头已记录。
- 错误码语义：`11140` 账号受限（当前请求**不换号**）、`11128` 渠道瞬时拦截（非 429/非鉴权）、`6004`/`1005`/`14018` 等分类；额度按套餐/包统计。
- 上游无原生会话 ID；仅下游会话粘性。
- **不迁移**：`x-stainless-*` 客户端身份伪装、设备令牌、渠道标记中和、验证码/指纹相关逻辑。

**有意排除**

- OpenCode Zen / Kilo 作为上游（与总目标一致）；增长、签到、邀请类接口。
- 任何绕过配额/风控、封禁后自动换号继续调用的逻辑。
- 不复制 Coding2API 源码；只吸收协议事实与设计结论。

**本阶段交付边界**

- 未编写 Rust Provider，未新增 Gateway/UI 代码，未修改用户既有业务工程。
- 未调用任何真实上游，无额度/成功率/性能实测数据。

## 9. 阶段 02 最小实现任务清单

> 对应 `docs/ARCHITECTURE.md` §13 第 1 级。前置决策已于 2026-10-28 确认：**暂定 MIT（暂不生成 LICENSE）**与**保留改造现有脚手架**。
> 本阶段只交付“可编译、可测试、可跑通 Mock 的 Rust Core”，**不实现完整 Gateway，不实现任何真实 Provider**。
> 执行方式：每次只做一个里程碑；M1–M9 已完成（§10–§18）。

| # | 任务 | 关键产物 | 验收（必须可执行） |
| --- | --- | --- | --- |
| M1 | 工程基线 | 确定 workspace/单 crate、toolchain 固定、依赖清单；清理或保留脚手架并注明 | `cargo build` 成功；`cargo test` 在零测试或骨架测试下通过 **（2026-10-28 完成，见 §10）** |
| M2 | 错误层 | `error::ErrorKind`（AuthInvalid、AuthExpired、QuotaExhausted、RateLimited、ModelUnavailable、ChannelRestricted、BadRequest、UpstreamServer、Transport、Cancelled、NoCapacity、SessionBindingInvalid、Unsupported）+ OpenAI/Anthropic 错误体序列化 | 表驱动单测：每个 kind → HTTP 状态 + 两种错误体 JSON **（2026-10-28 完成，见 §11）** |
| M3 | 安全层 | `SecretBox`（AES-256-GCM、随机 nonce、AAD 绑定 account id）、主密钥来自环境变量或 0600 文件、token SHA-256、脱敏函数 | 加解密往返；错误密钥解密失败；日志脱敏用例覆盖 Authorization/Cookie/token **（2026-10-28 完成，见 §12）** |
| M4 | 存储层 | SQLx + SQLite；`migrations/0001`（schema_version、accounts、credentials、sessions、quota_snapshots、usage_events、settings）；WAL/busy_timeout/外键 | 空库迁移成功；账号 CRUD；凭据以密文落库；并发写入不丢更新（事务测试） **（2026-10-28 完成，见 §13）** |
| M5 | 协议骨架 | `protocol`：`ChatRequest`/`ChatResponse`/`StreamEvent`（Start、TextDelta、ReasoningDelta、ToolCallStart、ToolCallArgsDelta、Usage、Finish、Error）；角色与内容块 | serde 往返；事件顺序校验器拒绝乱序（如 ArgsDelta 先于 ToolCallStart） **（2026-10-28 完成，见 §14）** |
| M6 | Provider Trait + Registry | trait（`info`/`capabilities`/`auth_status`/`list_models`/`quota`/`chat`/`chat_stream`/可选 `agent_task`）、`Capabilities`、`ProviderInfo`、`VerificationState`、`(kind, region)` Registry | 注册两个 Mock；未启用 provider 不出现在列表；能力位默认 false；`agent_task` 默认返回 `Unsupported` **（2026-10-29 完成，见 §15）** |
| M7 | Mock Provider | 非流式 + 流式（含工具调用增量、usage、finish）、可注入 401/402/429/500/超时/取消 | 集成测试覆盖：正常、每个错误码、客户端取消后上游被中断 **（2026-10-29 完成，见 §16）** |
| M8 | 最小 API | `POST /v1/chat/completions`（流式/非流式）、`GET /v1/models`、`GET /health`、gateway key 鉴权（Bearer 或 `x-api-key`，常量时间比较） | 对 Mock 跑通 OpenAI 形状；无 key 返回 401；未知模型返回 404 且不产生上游调用 **（2026-10-29 完成，见 §17）** |
| M9 | 配置与 CLI | TOML + `POLYGATE_*` 环境变量；`serve` / `doctor` / `config-example`；默认仅监听 `127.0.0.1` | 空配置启动给出可读错误；`doctor` 输出配置、数据库、主密钥、端口四项检查结果 **（2026-10-29 完成，见 §18）** |

**本阶段明确排除**

- 真实 Provider（CodeArts/Qoder/TRAE/WorkBuddy）与其凭据逻辑；CodeBuddy 与 ZCode 明确排除。
- 账号池调度、并发租约、冷却、会话持久化与粘性（属第 2–3 级）。
- Anthropic Messages 与 OpenAI Responses 适配器（第 4 级；本阶段只要求 OpenAI Chat）。
- Web UI、Docker、systemd、CI 与性能测量（第 6–7 级）。
- 任何真实上游调用、任何 `verified` 状态声明。
- 签到/权益领取（`/v1/ops/claim`、`/v1/ops/confirm`）及任何托盘/桌面 UI。

**建议依赖（待 M1 确认，不作承诺）**：tokio、axum（或同等级 HTTP 框架）、serde/serde_json、sqlx（sqlite）、clap、tracing、aes-gcm、sha2、zeroize、thiserror。版本与 feature 在 M1 锁定并记录。

**完成判据**：`cargo test` 全绿；Mock 上 OpenAI Chat 流式与非流式各一条端到端测试；安全与存储单测覆盖上述验收点；`PROGRESS.md` 记录实际测试输出，未测项如实标注。
## 10. M1 执行记录（2026-10-28）

**目标（来自 §9 M1）**：确定 workspace/单 crate、toolchain 固定、依赖清单；清理或保留脚手架并注明。
**验收**：`cargo build` 成功；`cargo test` 在零测试或骨架测试下通过。

### 10.1 工程决策

| 项 | 结果 |
| --- | --- |
| 工程形态 | **单 crate**（`polygate` 0.1.0），库入口 `src/lib.rs`，二进制入口 `src/main.rs` |
| edition / MSRV | edition 2021 / `rust-version = "1.85"` |
| Toolchain | `rust-toolchain.toml` 固定 **1.98.1**（`profile = minimal`，含 rustfmt、clippy）；已通过 rustup 安装该版本 |
| 依赖锁定 | `Cargo.lock` 已生成；依赖直接取自现有 `Cargo.toml`，未新增或删除任何依赖 |
| 脚手架 | **保留并改造**（按用户前置决策）；空目录继续作为后续里程碑占位 |
| 许可证 | 暂定 MIT；`Cargo.toml` 已声明 `license = "MIT"`，但**未生成 LICENSE 文件**，发布前需完成来源与依赖清点 |

**锁定的顶层依赖版本（2026-10-28 实测 `cargo tree --depth 1`）**

aes-gcm 0.10.3、axum 0.8.9、chrono 0.4.45、clap 4.6.7、reqwest 0.12.28、serde 1.0.229、
serde_json 1.0.154、sha2 0.10.9、sqlx 0.8.6、thiserror 2.0.21、tokio 1.54.1、toml 0.8.23、
tower 0.5.3、tower-http 0.6.11、tracing 0.1.44、uuid 1.29.0、zeroize 1.9.1。
（完整清单以 `Cargo.lock` 为准。）

### 10.2 实际修改

| 文件 | 变更 |
| --- | --- |
| `src/lib.rs` | 新增。`#![forbid(unsafe_code)]`；声明 `accounts`、`config`、`error`、`models`、`quota`、`security`、`sessions`；空目录不声明 |
| `src/main.rs` | 新增。最小入口，仅打印 `polygate <version>`；CLI 子命令留给 M9 |
| `config.example.toml` | 新增。示例配置（loopback 监听、SQLite 路径、调度默认值、仅 mock provider，无密钥） |
| `rust-toolchain.toml` | 新增。固定 1.98.1 与 rustfmt/clippy |
| `AGENTS.md` | 新增。精简开发规范：许可证/来源、安全红线、阶段纪律、依赖方向、构建验证、文档更新 |
| `Cargo.lock` | 新增（构建生成） |
| `src/quota/mod.rs` | `QuotaUnit` 增加 `Hash`（编译错误修复）；`from_str` → `from_db_str`（clippy） |
| `src/accounts/mod.rs`、`src/sessions/mod.rs` | `from_str` → `from_db_str`（clippy） |
| `src/config/mod.rs` | `SchedulerStrategy` 改为派生 `Default`（`#[default]` 标注 SWRR）；新增 `example_config_parses` 测试 |
| `src/security/mod.rs` | 消除 `let_and_return`（clippy）；行为不变 |
| 全部 `src/*.rs` | 执行 `cargo fmt`（仅格式） |

**未修改**：`migrations/0001_init.sql`、`docs/*`、空目录、`.refs/`；未新增 Provider、API、调度器、存储实现。

### 10.3 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.07s
exit=0

$ cargo test
running 5 tests
test quota::tests::shared_group_does_not_double_count ... ok
test config::tests::example_config_parses ... ok
test quota::tests::unknown_never_becomes_zero ... ok
test security::tests::mask_never_reveals_middle ... ok
test security::tests::encrypt_roundtrip_and_tamper_detection ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile ... exit=0

$ cargo fmt --check
exit=0
```

### 10.4 剩余问题（M1 范围内 / 留给后续里程碑）

1. **未验证**：5 个测试只覆盖配置示例解析、额度未知语义、共享组去重、AES-GCM 往返与篡改检测、脱敏；**安全层尚无 AAD 绑定 account id、错误密钥解密、日志脱敏完整用例**（M3）。**（已于 M3 解决，见 §12）**
2. **Schema 未执行**：`migrations/0001_init.sql` 存在但从未连接 SQLite，未验证语法、WAL、外键与并发（M4）。**（已于 M4 解决，见 §13）**
3. **错误枚举与 §9 命名不一致**：现有 `ModelNotFound`/`Network`/`Upstream`/`SessionInvalid` 与 M2 要求的
   `ModelUnavailable`/`Transport`/`UpstreamServer`/`SessionBindingInvalid` 存在差异，且现有额外包含 `AccountDisabled`、`Timeout`、`Internal`。M2 需一次性对齐并给出映射表。**（已于 M2 解决，见 §11.1）**
4. **配置模块超前于 M1**：`src/config/mod.rs` 已包含 `POLYGATE_*` 覆盖与 `example_config()`，属 M9 范围；本阶段仅保留脚手架，**未实现 CLI 子命令**。
5. **无实际服务**：无 `/health`、无 HTTP 监听、无具体 Provider（协议层 M5、Provider Registry M6 已完成；Mock Provider 属 M7，API 属 M8）。
6. **许可证**：`LICENSE` 未生成；依赖许可证未逐项清点，发布前必须完成（见 `docs/RESEARCH.md` §13）。
7. **Git**：仓库仍无提交；所有文件均为未跟踪状态，本阶段未创建 commit。
8. **未测项**：无真实 Provider、无真实上游调用、无性能/内存测量、无 Linux/Docker 部署验证。

## 11. M2 执行记录（2026-10-28）

**目标（来自 §9 M2）**：`error::ErrorKind` 对齐 + OpenAI/Anthropic 错误体序列化。
**验收**：表驱动单测，每个 kind → HTTP 状态 + 两种错误体 JSON。

### 11.1 错误分类（最终枚举）

旧名 → 新名（§10.4 遗留项 3 关闭）：

| 旧名 | 新名 |
| --- | --- |
| `ModelNotFound` | `ModelUnavailable` |
| `Network` | `Transport` |
| `Upstream` | `UpstreamServer` |
| `SessionInvalid` | `SessionBindingInvalid` |

保留 `AccountDisabled`、`Timeout`、`Internal`（既有且语义独立）；新增 `Config`、`Storage`，用于满足“配置错误、数据库错误、超时需可区分”的要求。最终 18 个 kind：

| ErrorKind | ErrorCategory | HTTP | code（稳定） |
| --- | --- | --- | --- |
| BadRequest | Request | 400 | `invalid_request_error` |
| AuthInvalid | Auth | 401 | `invalid_api_key` |
| AuthExpired | Auth | 401 | `credential_expired` |
| AccountDisabled | Auth | 403 | `account_disabled` |
| ChannelRestricted | Auth | 403 | `channel_restricted` |
| QuotaExhausted | Quota | 402 | `insufficient_quota` |
| RateLimited | Quota | 429 | `rate_limit_exceeded` |
| NoCapacity | Routing | 503 | `no_capacity` |
| ModelUnavailable | Routing | 404 | `model_unavailable` |
| SessionBindingInvalid | Routing | 409 | `session_binding_invalid` |
| Unsupported | Unsupported | 501 | `unsupported_capability` |
| UpstreamServer | Provider | 502 | `upstream_error` |
| Transport | Provider | 502 | `transport_error` |
| Timeout | Timeout | 504 | `timeout` |
| Cancelled | Cancelled | 499 | `request_cancelled` |
| Config | Config | 500 | `configuration_error` |
| Storage | Database | 500 | `storage_error` |
| Internal | Internal | 500 | `internal_error` |

**错误体类型映射**

- OpenAI：`BadRequest`/`ModelUnavailable`/`SessionBindingInvalid`/`Unsupported` → `invalid_request_error`；`AuthInvalid`/`AuthExpired` → `authentication_error`；`AccountDisabled`/`ChannelRestricted` → `permission_error`；`QuotaExhausted` → `insufficient_quota`；`RateLimited` → `rate_limit_error`；其余 → `server_error`。
- Anthropic：`BadRequest`/`SessionBindingInvalid`/`Unsupported` → `invalid_request_error`；`ModelUnavailable` → `not_found_error`；认证类 → `authentication_error`；权限类 → `permission_error`；额度/限流 → `rate_limit_error`；`NoCapacity` → `overloaded_error`；其余 → `api_error`。
- 两种错误体都只含 `message`/`type`/`code`（OpenAI 另含 `param: null`），**不含** `detail`、`upstream_status` 或任何头信息。

### 11.2 实际修改

| 文件 | 变更 |
| --- | --- |
| `src/error/mod.rs` | 重写。`ErrorKind` 重命名/新增；`ErrorKind::ALL`、`category()`、`http_status()`、`code()`、`blames_account()`、`retryable()`；`ErrorCategory`；`GatewayError` 构造时脱敏，新增 `config`/`storage`/`auth_invalid`/`model_unavailable`/`session_binding_invalid`/`upstream_server`/`transport`/`timeout` 构造器、`public_view()`、`log(operation)`；`redact_secrets()`；重写 `openai_error_body`/`anthropic_error_body`；`From<sqlx::Error>` → `Storage`，`From<reqwest::Error>` → `Timeout`/`Transport`/`UpstreamServer` 且 detail 脱敏 |
| `src/config/mod.rs` | 6 处配置类错误（监听地址、配置文件读取/解析、调度策略、重复 provider id、数据目录）由 `internal` 改标 `config` |
| `src/security/mod.rs` | 6 处主密钥/密钥文件类错误（环境变量格式、密钥长度、读取/写入/chmod、数据目录）由 `internal` 改标 `config`；加解密失败仍为 `internal` |
| `PROGRESS.md` | 本记录与状态更新 |

**未修改**：`accounts`、`models`、`quota`、`sessions`、`migrations/`、`docs/`、空目录、`.refs/`；未新增依赖（`Cargo.toml`/`Cargo.lock` 未变）；未新增 HTTP、Provider、调度器、Web UI 代码。

### 11.3 已实现功能（对照任务要求）

1. 复用现有 `ErrorKind`/`GatewayError`，仅重命名与补 2 个 kind。
2. 所有内部失败仍统一为 `GatewayError`；`sqlx`/`reqwest` 错误经 `From` 归一。
3. `ErrorCategory` 覆盖 Request/Auth/Quota/Routing/Provider/Timeout/Unsupported/Cancelled/Config/Database/Internal，配置、认证、Provider、请求、数据库、超时、不支持能力均可区分。
4. 分类方式稳定：kind 枚举 + `code()` + serde snake_case 名 + `ErrorCategory`，全部由表驱动单测锁定。
5. `http_status()` 提供 400/401/402/403/404/409/429/499/500/501/502/503/504 映射。
6. 客户端信息：`public_view()` 与两种错误体；`detail` 永不序列化。
7. 日志：`GatewayError::log(operation)` 输出 kind/category/upstream_status/retry_after/脱敏 detail；`From<sqlx::Error>` 与 `From<reqwest::Error>` 自动记录。
8. 脱敏：`redact_secrets()` 处理 `Authorization`/`Cookie`/`Set-Cookie`/`X-Api-Key` 等头（整行），以及 `token=`/`api_key=`/JSON `"refresh_token":"…"` 等参数（仅值）；消息与 detail 在构造期即脱敏，输出侧再次脱敏。
9. 未新增依赖。
10. 现有调用接口保留：`GatewayError::new/with_detail/with_upstream_status/retry_after/bad_request/unsupported/internal/no_capacity` 签名不变。

### 11.4 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.35s
exit=0

$ cargo test
running 19 tests
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.26s
exit=0
```

新增测试（14 个，均在 `src/error/mod.rs`）：`tables_cover_every_kind`、`kind_maps_to_status_code_and_category`、`openai_body_is_stable_per_kind`、`anthropic_body_is_stable_per_kind`、`serde_names_are_stable`、`unsupported_maps_to_501`、`timeout_is_504_provider_retryable`、`provider_auth_failures_blame_account`、`client_errors_do_not_blame_account`、`public_view_hides_detail_and_upstream_status`、`redacts_authorization_cookie_and_api_key_headers`、`redacts_query_and_json_credentials_only`、`redaction_leaves_ordinary_text_intact`、`message_and_detail_are_redacted_at_construction`。

首轮测试有 3 个脱敏用例失败（JSON 键后的引号未识别、分隔符后的空白未保留、开引号被丢弃），已修正匹配逻辑后复跑通过；上述输出为修正后的结果。

### 11.5 已知问题

1. **无 HTTP 层**：状态码与错误体尚未挂到路由上（M8 才提供 `/v1/chat/completions` 等）；`retry_after` 也尚未输出为响应头。
2. **`AuthInvalid` 双重语义**：既表示上游凭据被拒，也将在 M8 表示下游 gateway key 无效，两者都是 401；若需区分需在 M8 前拆分或加子类型。
3. **`AccountDisabled` 状态码变更**：401 → 403（语义修正）；当前无调用方依赖旧值。
4. **脱敏边界**：`redact_secrets` 依赖“键名 + `:`/`=`”模式，不识别无键名的裸 token（如日志中直接打印 `sk-...`）；日志路径仍须避免打印完整凭据（M3 继续加固）。
5. **`detail` 仍为公开字段**：直接赋值可绕过构造期脱敏，但输出侧（`public_view`/错误体/`log`）会再次脱敏。
6. **未验证**：未连接真实上游，`reqwest`/`sqlx` 转换分支未被真实错误触发（仅编译覆盖）。
7. **Git**：仓库仍无提交。
8. 未新增依赖；未修改 `Cargo.toml`/`Cargo.lock`。

### 11.6 下一步 M3 入口

M3 安全层：`SecretBox` 增加 AAD 绑定 account id、错误密钥解密失败用例、日志脱敏用例（复用 `error::redact_secrets`，`security` → `error` 依赖方向已存在，无循环）。等待用户下达任务。

## 12. M3 执行记录（2026-10-28）

**目标（来自 §9 M3）**：`SecretBox`（AES-256-GCM、随机 nonce、AAD 绑定 account id）、主密钥来自环境变量或 0600 文件、token SHA-256、脱敏函数。
**验收**：加解密往返；错误密钥解密失败；日志脱敏用例覆盖 Authorization/Cookie/token。

### 12.1 设计

- **AAD 域分隔**：`account_aad(account_id)` 返回 `polygate:credential:v1:<account_id>`。域前缀固定，避免凭据密文被当作其他类型的加密数据复用；换 account id 或换主密钥都会导致 AEAD 校验失败。
- **API 兼容**：保留 M1 的 `encrypt(plaintext)` / `decrypt(encoded)` 签名，内部以空 AAD 委托给新方法；凭据路径使用 `encrypt_for_account` / `decrypt_for_account`。
- **密文格式不变**：`v1:` + base64(nonce‖ciphertext‖tag)，12 字节随机 nonce 由 `OsRng` 生成。
- **主密钥**：仍由 `POLYGATE_MASTER_KEY`（hex/base64，32 字节）或数据目录下 `master.key` 提供；解码后的密钥缓冲区在构造 cipher 后 `zeroize`。Unix 下新建密钥文件强制 0600；读取既有文件时若组/其他可读，记录告警但不静默修改权限。Windows 无 0600 语义（见 §12.5）。
- **token**：沿用 `generate_token`（32 字节 CSPRNG + URL-safe base64）与 `hash_token`/`verify_token`（SHA-256 + 常量时间比较）。
- **日志脱敏**：不新增第二套实现，用例直接覆盖 `error::redact_secrets`（M2 实现），验证 Authorization、Cookie、`token=` 三类凭据不会出现在日志文本中；`credential_hint` 仅输出键名。

### 12.2 实际修改

| 文件 | 变更 |
| --- | --- |
| `src/security/mod.rs` | 新增 `AAD_DOMAIN`、`account_aad`、`encrypt_with_aad`、`decrypt_with_aad`、`encrypt_for_account`、`decrypt_for_account`；`encrypt`/`decrypt` 改为空 AAD 委托（签名不变）；`from_env_or_file` 对密钥缓冲区 `zeroize`；新增 `warn_if_permissive`（Unix 检查 0600，失败仅告警）；新增 8 个单测 |
| `docs/ARCHITECTURE.md` | §10 安全条目补充 AAD 绑定 account id 与密钥零化事实 |
| `PROGRESS.md` | 状态、任务清单、执行记录与本 §12 |

**未修改**：`error`、`config`、`accounts`、`models`、`quota`、`sessions`、`lib.rs`、`main.rs`、`migrations/`、空目录、`.refs/`；未新增依赖（`Cargo.toml`/`Cargo.lock` 未变）；未新增 HTTP、Provider、调度器、Web UI 代码。

### 12.3 已实现功能（对照验收）

1. **加解密往返**：`encrypt_roundtrip_and_tamper_detection`（空 AAD）与 `aad_roundtrip_binds_ciphertext_to_account`（account AAD）。
2. **AAD 绑定 account id**：同一密文用其他 account id 解密返回 `ErrorKind::Internal`；用空 AAD 也失败。
3. **错误主密钥解密失败**：`wrong_master_key_and_tampered_aad_blob_are_rejected` 用两把不同密钥验证，并覆盖密文篡改。
4. **主密钥文件**：`master_key_file_is_created_once_and_reloaded` 验证首次创建、32 字节、重载一致；Unix 下额外断言 0600。`invalid_master_key_file_is_a_config_error` 验证非法内容返回 `Config` 且 message 不含文件内容。
5. **token SHA-256**：`token_hash_is_deterministic_and_verifiable`（64 位 hex、确定性、`verify_token` 正反例）；`generated_tokens_are_unique_and_prefixed`。
6. **日志脱敏**：`log_redaction_covers_authorization_cookie_and_token` 覆盖 `Authorization: Bearer …`、`Cookie: session=…`、`token=…`；`credential_hint_never_contains_values` 验证提示只含键名。
7. 保留原有 `mask_secret` 行为与 M1/M2 全部测试；未改调用接口，未新增依赖。

### 12.4 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.94s
exit=0

$ cargo test
running 27 tests
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
（另有 main 与 doc-tests 各 0 个）
exit=0

$ cargo build
   Compiling polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.80s
exit=0
```

新增 8 个安全层测试：`aad_roundtrip_binds_ciphertext_to_account`、`wrong_master_key_and_tampered_aad_blob_are_rejected`、`master_key_file_is_created_once_and_reloaded`、`invalid_master_key_file_is_a_config_error`、`token_hash_is_deterministic_and_verifiable`、`generated_tokens_are_unique_and_prefixed`、`log_redaction_covers_authorization_cookie_and_token`、`credential_hint_never_contains_values`。测试总数 19 → 27，M1/M2 测试名称与断言未变。

### 12.5 已知问题

1. **明文返回未受管**：`decrypt_*` 返回 `Vec<u8>`，调用方负责尽快解析并丢弃；内部临时缓冲已 `zeroize`，但返回副本的生命周期不受 `SecretBox` 控制。后续可换成 `Zeroizing<Vec<u8>>`（需调用方配合，M4 决定）。
2. **Windows 无 0600**：`restrict_permissions` 在非 Unix 为空实现；Windows 下密钥文件权限依赖目录 ACL，安全强度以 Linux 部署为准（`cfg(unix)` 已测试）。
3. **既有密钥文件不自动收紧**：Unix 加载时只对组/其他可读权限记录 `warn`，不改写权限，避免在只读根文件系统或非属主场景失败。
4. **account id 不可变**：AAD 直接包含 account id；重命名账号会导致旧密文不可解密。M4 存储层必须保证 account id 稳定，或在改名时重新加密。
5. **环境变量优先路径未测**：`POLYGATE_MASTER_KEY` 为进程级全局状态，单元测试并发下不可靠，因此只测文件路径；env 分支仅编译覆盖。
6. **token hash 无盐**：SHA-256 直接哈希适用于高熵随机 token（设计如此），不适用于人类密码；argon2 依赖已存在但尚无调用点。
7. **日志脱敏边界**：仍依赖“键名 + `:`/`=`”模式，裸 token（如单独打印 `sk-...`）不会被识别；日志规范仍要求不打印完整凭据。
8. **未接线**：凭据加解密尚无调用方（M4 存储层接入）；无真实上游、无性能/内存数据；Git 仍无提交。

### 12.6 下一步 M4 入口

M4 存储层：SQLx + SQLite、执行并验证 `migrations/0001_init.sql`（schema_version/accounts/credentials/sessions/quota_snapshots/usage_events/settings）、WAL/`busy_timeout`/外键、账号 CRUD。写凭据时使用 `SecretBox::encrypt_for_account(account_id, …)`，读取时 `decrypt_for_account`；落库前确认列中只存 `v1:` 密文。等待用户下达任务。

## 13. M4 执行记录（2026-10-28）

**目标（来自 §9 M4）**：SQLx + SQLite、`migrations/0001`、WAL/busy_timeout/外键、账号 CRUD、凭据密文落库、并发不丢更新。
**验收**：空库迁移成功；账号 CRUD；凭据以密文落库；并发写入不丢更新（事务测试）。

### 13.1 Schema v1（`migrations/0001_init.sql`）

`migrations/0001_init.sql` 是**首次真正执行**的 schema（此前从未连接数据库，无现存数据可覆盖）。相对脚手架草案的变更：

- `credentials` 从 `accounts.credential_enc` 拆出为独立表，含 `encryption_version`；账号表不再持有任何密文。
- `accounts.kind` 更名为 `provider_kind`（与 `ProviderKind` 对齐），`name` → `display_name`。
- `sessions` 增加 `client_session_id` 与 `provider_kind`，绑定唯一键为 `(client_session_id, provider_kind, region)`；`account_id` 外键 `ON DELETE CASCADE`。
- `quota_snapshots` 用可空 `used` / `limit_value` 表达额度，未知时两列均为 `NULL`，不使用 0。
- 移除尚未使用的草案表（`quota_groups`、`models`、`api_keys`、`admin_sessions`、`agent_tasks`、`audit_log`）；它们由实际使用它们的里程碑再建新 migration，避免空表被误读为已实现功能。
- `PRAGMA journal_mode` 不再写进 migration（sqlx 在事务中执行迁移，SQLite 不允许在事务内切换 journal 模式）；WAL、`busy_timeout`、`foreign_keys` 由连接选项逐连接设置。

| 表 | 关键列 | 约束 / 索引 |
| --- | --- | --- |
| `schema_version` | version, applied_at | version 主键；迁移末尾写入 1 |
| `accounts` | id, provider, provider_kind, region, display_name, enabled, weight, max_concurrency, quota_group, auth_status, auth_checked_at, last_error, credential_hint, metadata, created_at, updated_at | 主键 id；`CHECK` enabled/weight/max_concurrency；`(provider_kind, region)`、`quota_group` 索引 |
| `credentials` | account_id, encrypted_payload, encryption_version, created_at, updated_at | `account_id` 主键 + 外键 → accounts `ON DELETE CASCADE` |
| `sessions` | id, client_session_id, provider, provider_kind, region, account_id, upstream_session_id, state, created_at, last_used_at, expires_at, request_count, metadata | `UNIQUE(client_session_id, provider_kind, region)`；外键 → accounts；account、expires_at 索引 |
| `quota_snapshots` | account_id, quota_group_id, model, quota_unit, used, limit_value, status, error, refreshed_at, raw | 外键 → accounts；`(account_id, refreshed_at)`、`(quota_group_id, refreshed_at)` 索引 |
| `usage_events` | request_id, provider_kind, account_id, model, input_tokens, output_tokens, status, created_at | `request_id UNIQUE`；`account_id` 外键 → accounts `ON DELETE SET NULL`；created_at、account 索引 |
| `settings` | key, value, updated_at | key 主键；Repository 拒绝敏感键名 |

### 13.2 连接、迁移与关闭

- **连接池**：`SqlitePoolOptions`，`min_connections = 1`，`max_connections` 由调用方传入并限制在 `1..=8`；`acquire_timeout = 5s`。配置默认值由 8 改为 4（`config.example.toml` 同步），适合低内存 VPS。
- **逐连接 PRAGMA**：`journal_mode = WAL`、`synchronous = NORMAL`、`busy_timeout = 5000ms`、`foreign_keys = ON`、`create_if_missing = true`（文件模式）。
- **迁移**：`sqlx::migrate!("./migrations")` 编译期嵌入，`Storage::open_file/open_url/open_memory` 打开后自动执行；`Storage::migrate()` 可重复调用，已应用版本记录在 `_sqlx_migrations` + `schema_version`。
- **错误**：打开/迁移失败归为 `ErrorKind::Storage`（URL 形态错误归 `Config`）；`storage_error` 将唯一键冲突映射为 `duplicate key`、外键冲突为 `foreign key constraint failed`、SQLITE_BUSY/`database is locked` 映射为 `database is busy`，detail 只含 SQLite 约束信息。
- **优雅关闭**：`Storage::close(self)` 调用 `pool.close().await`；重新打开文件的持久化已测。
- **入口**：`Storage::open_file(path, max_connections)`、`Storage::open_url(url, max_connections)`、`Storage::open_memory()`（单连接，仅测试）。

### 13.3 Repository 接口

| Repository | 方法 |
| --- | --- |
| `AccountRepo` | `insert`、`get`、`list`、`update`、`update_auth`、`set_enabled`、`delete` |
| `CredentialRepo` | `save`（加密 upsert）、`load`（按需解密）、`delete` |
| `SessionRepo` | `save`（按绑定键 upsert）、`get`、`get_by_id`、`list_by_account`、`touch`（原子 `request_count + 1`）、`mark_state`、`count_active_by_account`、`delete_expired` |
| `QuotaRepo` | `save`、`latest_for_account`、`latest_for_group`（每账号最新一条，不求和）、`history_for_account` |
| `UsageRepo` | `record`、`summary`（请求数/输入/输出 token 分列）、`list_recent` |
| `SettingsRepo` | `get`、`set`（拒绝敏感键）、`delete`、`list` |

- `Storage::create_account(&SecretBox, &NewAccount)`：同一事务内写入 accounts + credentials，任一步失败则整体回滚。
- Repository 不依赖 HTTP、Provider、Scheduler；`storage` 只依赖 `error`、`security` 与领域数据结构。

### 13.4 实际修改

| 文件 | 变更 |
| --- | --- |
| `migrations/0001_init.sql` | 重写为 7 表 schema v1（见 §13.1） |
| `src/storage/mod.rs` | 新增：`Storage`、连接选项/池、迁移、`close`、6 个 Repository 构造函数、`create_account` 事务、`storage_error`、时间戳与 limit 辅助 |
| `src/storage/accounts.rs` | 新增：账号 CRUD、原子状态更新、可在事务内复用的 `insert_row` |
| `src/storage/credentials.rs` | 新增：加密 upsert、按 account 解密读取、删除、`upsert_payload`（事务复用） |
| `src/storage/sessions.rs` | 新增：绑定 upsert、查询、原子 touch、状态、计数、过期清理 |
| `src/storage/quota.rs` | 新增：快照保存/最新/组内最新/历史；未知额度保持 NULL |
| `src/storage/usage.rs` | 新增：事件写入、请求/token 汇总、最近列表 |
| `src/storage/settings.rs` | 新增：非敏感 settings CRUD + 敏感键拒绝 |
| `src/storage/tests.rs` | 新增 21 个存储层单测 |
| `src/lib.rs` | 声明 `pub mod storage` |
| `Cargo.toml` | `sqlx` features 增加 `macros`、`migrate`（无新增直接依赖；`Cargo.lock` 增加 `sqlx-macros`、`dotenvy` 等构建期传递依赖） |
| `src/config/mod.rs` | `DatabaseConfig.max_connections` 默认 8 → 4；`Region`/`ProviderKind` 增加 `from_db_str` |
| `config.example.toml` | `max_connections` 改 4 并注明上限 |
| `src/accounts/mod.rs` | `NewAccount` 增加必填 `kind: ProviderKind` |
| `src/sessions/mod.rs` | `Session` 增加 `client_session_id`、`kind` |
| `src/security/mod.rs` | 新增 `SecretBox::from_master_key`（测试/doctor 用）与 `ENCRYPTION_VERSION` |
| `PROGRESS.md` | 本记录与状态更新 |

**未修改**：`src/error`、`src/quota` 聚合逻辑、`src/models`、`src/main.rs`、`docs/*`、`.refs/`；未新增直接依赖（`Cargo.lock` 因 sqlx `macros`/`migrate` feature 展开新增构建期传递依赖）；未新增 Provider、HTTP 路由、调度器或 Web UI。

### 13.5 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.04s
exit=0

$ cargo test
running 48 tests
test result: ok. 48 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
（另有 main 与 doc-tests 各 0 个）
exit=0

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.28s
exit=0
```

新增 21 个测试（均在 `src/storage/tests.rs`）：`empty_database_migrates_and_creates_required_tables`、`corrupt_database_file_fails_initialization`、`migration_is_idempotent`、`account_crud_roundtrip`、`credential_is_encrypted_at_rest_and_reads_back`、`wrong_master_key_cannot_decrypt_credentials`、`credential_for_unknown_account_is_rejected_by_foreign_key`、`session_requires_existing_account`、`session_binding_is_upserted_by_client_provider_region`、`quota_unknown_stays_unknown`、`quota_known_snapshot_roundtrips_used_and_limit`、`shared_group_returns_latest_row_per_account_without_summing`、`usage_events_count_requests_and_tokens_separately`、`duplicate_request_id_is_a_storage_error`、`settings_roundtrip_and_sensitive_keys_are_rejected`、`account_and_credential_are_created_in_one_transaction`、`concurrent_transactional_creations_commit_complete_rows`、`data_survives_reopen`、`concurrent_session_updates_are_atomic`、`concurrent_setting_upserts_leave_one_consistent_value`、`deleting_account_cascades_credentials_sessions_and_quota`。测试总数 27 → 48；M1–M3 测试名称与断言未变。

并发测试验证最终数据：8 个任务 × 10 次 `touch` 后 `request_count` 精确等于 80；8 个并发事务建号后账号数与可解密凭据数均为 8。

### 13.6 已知问题

1. **依赖方向偏差**：`storage` 直接使用 `accounts`/`sessions`/`quota` 的领域结构，而 `docs/ARCHITECTURE.md` §2.1 只把 `security` 列为 Storage 允许依赖。当前不存在与 Provider/Scheduler/API 的耦合；阶段 2/3 引入 Account/Session Manager 时可改为 storage 自有 row 类型 + 显式转换。
2. **Busy 未真实触发**：`database is busy` 分类已实现，但 `busy_timeout = 5s` 下测试会等待而非报错，未构造锁竞争失败用例。
3. **URL 形态未测**：`open_url`（`sqlite://...`，含 Windows 盘符路径）与 `open_memory` 仅编译覆盖；测试使用 `open_file`。
4. **回滚路径未故障注入**：事务成功路径与并发提交已测；SQL 失败时的回滚由 sqlx `Transaction` drop 执行，未构造强制失败用例。
5. **并发规模有限**：8 任务 × 10 次、8 个事务不构成压力/性能证明；无内存/RSS 数据。
6. **无升级迁移**：v1 为首次执行，缺 v2 的升级路径测试（新增 migration 后才可验证）。
7. **时间戳格式约束**：以 RFC3339 TEXT 存储，`encode_ts` 统一为毫秒 UTC；若外部写入其他格式，`latest_*` 的字符串排序会不准确。
8. **`Session.region` 仍是 String**：保存时校验 `cn`/`intl`（未知返回 `BadRequest`）；后续可改为 `Region` 枚举。
9. **settings 保密仅按键名**：敏感键按名称片段拒绝，值内容未扫描；不得用它存凭据/主密钥（主密钥只来自环境变量或 `master.key`）。
10. **未接线**：存储尚无调用方（M7–M9 才接 Provider/API）；`usage_flush_secs` 未使用；无真实上游、无性能数据；Git 仍无提交。

### 13.7 下一步 M5 入口

M5 协议骨架：新增 `src/protocol/`，定义 `ChatRequest` / `ChatResponse` / `StreamEvent`（Start、TextDelta、ReasoningDelta、ToolCallStart、ToolCallArgsDelta、Usage、Finish、Error）与角色/内容块；提供事件顺序校验器（拒绝 `ToolCallArgsDelta` 先于 `ToolCallStart` 等乱序）与 serde 往返测试。M5 不访问数据库、不实现 Provider、不改动 `storage` 接口。等待用户下达任务。

## 14. M5 执行记录（2026-10-28）

**目标（来自 §9 M5）**：`protocol` 规范化请求/响应/流事件与显式状态机校验；与具体 Provider、外部 HTTP 格式解耦。
**验收**：serde 往返；乱序事件（ArgsDelta 先于 ToolCallStart、终止后继续、重复终止等）被拒绝。

### 14.1 新增协议类型

| 类型 | 位置 | 说明 |
| --- | --- | --- |
| `MessageRole` | `src/protocol/message.rs` | `System`/`Developer`/`User`/`Assistant`/`Tool`；`is_instruction()` 区分指令角色 |
| `ContentBlock` | `src/protocol/message.rs` | 内部标签 `type`：`text`、`reasoning`、`tool_call`、`tool_result`、`unsupported`；未知内容进入 `Unsupported { media_type, data }` 而非丢弃 |
| `ToolCall` | `src/protocol/message.rs` | 稳定 `id`、`name`、原始 JSON `arguments`；`parse_arguments()` 对空串返回 `{}`，非法 JSON 返回 `UpstreamServer` 且不泄漏原始参数 |
| `ToolResult` | `src/protocol/message.rs` | `tool_call_id`、`content`、`is_error` |
| `ToolDefinition` | `src/protocol/message.rs` | 工具名、可选描述、原样 JSON Schema |
| `ChatMessage` | `src/protocol/message.rs` | `role` + `content: Vec<ContentBlock>` + 可选 `name`；提供 `text_content()` / `tool_calls()` / `tool_results()` / `unsupported_blocks()` |
| `ChatRequest` | `src/protocol/request.rs` | `request_id`、`model`、`messages`、`tools`、`tool_choice`、`max_output_tokens`、`temperature`、`top_p`、`stop`、`stream`、`metadata`；未设置的选项保持 `None` |
| `ToolChoice` | `src/protocol/request.rs` | `Auto`/`None`/`Required`/`Tool(name)` |
| `ChatResponse` | `src/protocol/response.rs` | 响应 `id`、可选 `request_id`、`model`、最终 `message`、`finish_reason`、可选 `usage`、`created_at` |
| `Usage` | `src/protocol/response.rs` | `input_tokens`/`output_tokens` + 可选 `reasoning_tokens`/`cached_input_tokens`/`total_tokens`；`total()` 在未报告总和时求和 |
| `FinishReason` | `src/protocol/response.rs` | `Stop`/`Length`/`ToolCalls`/`ContentFilter`/`Cancelled`/`Error`/`Unknown`/`Other(String)`；`from_upstream()` 归一空串与常见别名，未知非空值保留为 `Other` |
| `StreamEvent` | `src/protocol/stream.rs` | 内部标签 `event`：`start`、`text_delta`、`reasoning_delta`、`tool_call_start`、`tool_call_args_delta`、`usage`、`finish`、`error` |
| `StreamError` | `src/protocol/stream.rs` | 客户端安全投影：`kind` + 脱敏 `message` + 可选 `retry_after`；不包含 `detail`、上游状态或头部 |
| `StreamEventValidator` / `StreamState` | `src/protocol/validate.rs` | 显式状态机，见 §14.2 |

### 14.2 StreamEventValidator 状态机

状态：`Fresh`（尚未收到事件）→ `Streaming`（已收 `Start`）→ `AfterUsage`（已收唯一 `Usage`）→ `Finished`（已收 `Finish`）；`Failed`（已收 `Error`）。`Finished`/`Failed` 为终止态，之后任何事件都被拒绝。

| 规则 | 实现 |
| --- | --- |
| `Start` 必须先于内容事件 | `TextDelta`/`ReasoningDelta`/`ToolCallStart`/`ToolCallArgsDelta`/`Usage` 在 `Fresh` 状态被拒绝；`Start` 仅允许在 `Fresh`，重复 `Start` 拒绝 |
| `Finish` 后不能继续 | 终止态先于事件分类检查，任何事件返回 “after the stream finished” |
| `Error` 后不能继续 | 同上，返回 “after the stream failed”；`Error` 允许在任何非终止态出现（含 `Fresh`，表示连接前失败） |
| `ArgsDelta` 必须对应已开始的调用 | 维护 `HashSet<String>`；未知 `id` 拒绝 |
| 不允许冲突的 ToolCall ID | 空 `id` 拒绝；重复 `ToolCallStart` 同一 `id` 拒绝 |
| 不允许重复终止 | 终止态拒绝后续 `Finish`/`Error`；`Finish` 在 `Fresh` 拒绝（“finish before start”） |
| `Usage` 位置 | 仅 `Streaming` 状态允许，最多一次；`Usage` 后只允许 `Finish`/`Error`，内容事件与重复 `Usage` 拒绝 |
| 流未终止 | `ensure_terminated()` 对 `Fresh`/`Streaming`/`AfterUsage` 返回错误 |
| 拒绝不推进状态 | 校验失败时不写入 ID、不改变状态；调用方可直接终止并记录违规事件 |

违规统一返回 `ErrorKind::UpstreamServer`（消息前缀 `invalid stream event sequence:`），因为流由上游适配器产生，网关自身不得输出非法序列。

### 14.3 序列化策略

- 所有类型使用 Serde 派生；请求、响应、工具调用、8 类流事件均有 JSON 往返测试。
- 可选字段使用 `skip_serializing_if`：未设置时不出现（如 `request_id`、`usage`、`tool_choice`、`temperature`）。
- 未知字段在反序列化时忽略，用于网关内部类型的前向兼容；外部 OpenAI/Anthropic DTO 的严格校验属于后续 API 适配层，不在此模块。
- `ContentBlock` 与 `StreamEvent` 使用内部标签（`type` / `event`），下游适配器不得直接透传该 JSON。

### 14.4 修改文件

| 文件 | 变更 |
| --- | --- |
| `src/protocol/mod.rs` | 新增。模块入口、序列化策略说明、统一 re-export |
| `src/protocol/message.rs` | 新增。角色、内容块、ToolCall/ToolResult/ToolDefinition、ChatMessage |
| `src/protocol/request.rs` | 新增。ChatRequest、ToolChoice |
| `src/protocol/response.rs` | 新增。ChatResponse、Usage、FinishReason |
| `src/protocol/stream.rs` | 新增。StreamEvent、StreamError 与构造函数；终止/内容事件判定 |
| `src/protocol/validate.rs` | 新增。StreamState、StreamEventValidator |
| `src/protocol/tests.rs` | 新增。22 个协议/状态机测试 |
| `src/lib.rs` | 声明 `pub mod protocol`；注释同步（空目录只剩 `api`/`providers`/`scheduler`/`metrics`） |
| `PROGRESS.md` | 本记录与状态更新 |

**未修改**：`migrations/0001_init.sql`、`src/storage/*`、`src/accounts`、`src/sessions`、`src/quota`、`src/error`、`src/config`、`src/security`、`Cargo.toml`/`Cargo.lock`、`docs/*`（除 §8 说明性补充）；未新增依赖、Provider、HTTP 路由、调度器或 UI。

### 14.5 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.81s
exit=0

$ cargo test
test result: ok. 70 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s
（另有 main 与 doc-tests 各 0 个）
exit=0

$ cargo build
   Compiling polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.84s
exit=0
```

新增 22 个测试（均在 `src/protocol/tests.rs`）：`chat_request_roundtrips`、`minimal_request_omits_absent_optional_fields`、`chat_response_roundtrips`、`minimal_response_omits_absent_optional_fields`、`multi_turn_conversation_preserves_roles_and_content`、`tool_call_and_result_roundtrip`、`valid_stream_start_text_finish`、`args_delta_before_tool_call_start_is_rejected`、`events_after_finish_are_rejected`、`events_after_error_are_rejected`、`error_before_start_is_allowed_as_immediate_failure`、`interleaved_tool_calls_keep_fragments_by_id`、`duplicate_tool_call_id_is_rejected`、`duplicate_finish_and_finish_before_start_are_rejected`、`usage_ordering_is_enforced`、`duplicate_start_is_rejected`、`stream_events_roundtrip_through_json`、`unknown_fields_are_ignored`、`stream_end_without_terminator_is_rejected`、`finish_reason_normalisation`、`stream_error_carries_no_diagnostic_detail`、`usage_totals_fall_back_to_sum`。测试总数 48 → 70；M1–M4 测试名称与断言未变。

### 14.6 已知问题

1. **违规分类单一**：校验失败统一为 `UpstreamServer`；若后续用它校验网关自身生成的流，应区分 `Internal`，当前未引入独立 `ProtocolViolation`。
2. **Finish 与 Usage 分离**：`Finish` 不携带 usage，流式用量必须通过独立 `Usage` 事件；Anthropic `message_delta` 中的 usage 需适配器拆分。
3. **未校验工具名字**：未检查 `ToolCallStart.name` 是否出现在请求 `tools` 中；该能力属于 Provider/API 层。
4. **文本与工具可交错**：协议允许文本增量与工具参数并行；仅当 `Usage` 之后禁止内容，未强制“文本必须在工具参数之前结束”。
5. **无流标识关联**：事件自身不携带请求/响应 id（仅 `Start` 有）；无法检测跨连接拼接或重复流，需上游适配器保证。
6. **未知字段忽略**：内部类型前向兼容；外部 API DTO 的严格校验尚未实现（M8）。
7. **Unsupported 数据无上限**：`ContentBlock::Unsupported.data` 原样保留，未做大小或敏感内容检查；转发/拒绝策略留给 Provider。
8. **测试为单元级**：无并发流、无真实 SSE 解析、无性能数据；M7 在 Mock 上覆盖流式错误与取消。

### 14.7 下一步 M6 入口

M6 Provider Trait + Registry：新增 `src/providers/`，定义 `Provider` trait（`info`/`capabilities`/`auth_status`/`list_models`/`quota`/`chat`/`chat_stream`，`agent_task` 默认 `Unsupported`）、`ProviderInfo`、`Capabilities`、`VerificationState` 与 `(kind, region)` Registry；注册两个 Mock；未启用 Provider 不出现在列表；能力位默认 `false`。M6 复用 M5 协议类型，不实现 HTTP 路由、不接真实上游、不改动 `protocol` 的对外形状；如发现协议缺口，记录后单独处理。等待用户下达任务。

## 15. M6 执行记录（2026-10-29）

**目标（来自 §9 M6）**：定义 Provider Trait、`Capabilities`、`ProviderInfo`、`VerificationState` 与 `(kind, region)` Registry。
**验收**：注册两个 Mock；未启用 Provider 不出现在默认列表；能力位默认 `false`；`agent_task` 默认 `Unsupported`。

### 15.1 Provider Trait 设计

| 项 | 实现 |
| --- | --- |
| Trait Object 可用 | `Provider: Send + Sync` + `#[async_trait]`；`pub type ProviderRef = Arc<dyn Provider>`；Registry 只存 `ProviderRef`，无泛型参数 |
| 必需方法 | `info() -> ProviderInfo`、`capabilities() -> Capabilities` |
| 默认方法 | `auth_status`、`list_models`、`quota`、`chat`、`chat_stream`、`agent_task`、`cancel` 全部默认返回 `GatewayError::unsupported(...)`（`ErrorKind::Unsupported`，HTTP 501），消息含实例 id、`kind-region` 与能力名 |
| 异步抽象 | 沿用项目既有 `tokio` + `async-trait`；流式返回 `ChatStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, GatewayError>> + Send>>`（`tokio_stream::Stream`），事件类型复用 M5 `protocol::StreamEvent` |
| 凭据 | 每次调用传入 `&accounts::Credential`；Trait 不持有凭据，Registry 也不持有 |
| 领域类型 | 返回 `accounts::AuthStatus`、`quota::QuotaSnapshot`、`protocol::{ChatRequest, ChatResponse}`；`UpstreamModel`、`AgentTaskRequest`、`AgentTask`、`CancelRequest` 定义在 `providers` |

**能力分级（§9 第 3 条）**：`ProviderClass::{Model, Agent, Hybrid}` 由 `Capabilities::class()` 派生——`chat` 与 `agent_task` 的组合决定；两者都为 `false` 时返回 `None`（不可路由）。Agent 任务不会降级成 Chat 语义，反之亦然。

**能力位（`Capabilities`）**：`authentication`、`model_discovery`、`quota`、`chat`、`streaming`、`tools`、`native_session`、`agent_task`、`cancellation`，全部 `Default = false`；`Capability` 枚举（`snake_case`）与 `get`/`set`/`declared` 一一对应。

**`ProviderInfo`**：`id`（实例 id，如 `qoder-cn`）、`kind`（`ProviderKind`）、`region`（`Region`）、`display_name`、`version`、`verification`、`enabled`。`kind` 与 `region` 分离，因此 `qoder-cn` 与 `qoder-intl` 是两个独立实例；`is_mock()` 由 `kind == Mock` 判定。

**`VerificationState`**：`Verified` / `Experimental` / `Unsupported` / `Unavailable`（`snake_case`）；`is_verified()` 仅 `Verified` 为真，`routable()` 允许 `Verified`/`Experimental`。`From<config::Verification>`：`Mock` 与 `Experimental` 都映射到 `Experimental`——**Mock 永远不会成为 `Verified`**，mock 身份由 `ProviderKind::Mock` 与 `is_mock()` 表达。当前项目无任何 `Verified` 声明（与 `docs/PROVIDERS.md` §13.3 一致）。

### 15.2 Registry 实现

| 需求 | 实现 |
| --- | --- |
| 注册键 | `ProviderKey { kind, region }`（`Hash` + `Copy` + `Display` 为 `<kind>-<region>`） |
| 并发安全 | `parking_lot::RwLock<HashMap<ProviderKey, RegistryEntry>>`；读路径（`get`/`require`/`list*`/`capabilities`/`verification`）只持共享锁，注册与启停持独占锁 |
| 注册 / 防重复 | `register` 拒绝重复 `ProviderKey`、重复实例 `id`（跨 region）与空 id，均返回 `ErrorKind::Config`；`register_all` 顺序注册，遇冲突即返回已注册数量前的错误 |
| 查询 | `get`（含禁用，管理面用）、`require`（仅启用）、`require_capability`（启用 + 能力位） |
| 列表 | `list_enabled()` 为默认列表（禁用者不出现）；`list_all()` 供管理面；均按 `id` 排序，`enabled` 由 Registry 覆盖 Provider 的上报值 |
| 启停 | `set_enabled`；禁用只取消新请求候选资格，凭据与账号记录不受影响；未注册返回 `ModelUnavailable` |
| 能力 / 验证状态 | `capabilities(key)`、`verification(key)` 返回 `Option`（禁用仍可查询，便于管理面展示）；`require_capability` 对未声明能力返回 `Unsupported` |
| 未注册 / 禁用错误 | 未注册 → `ModelUnavailable`（404）；禁用 → `NoCapacity`（503）；能力缺失 → `Unsupported`（501） |
| 明确不做 | 不持有凭据、不判断账号健康、不做负载均衡与选号、不访问数据库或 HTTP——这些属于 M7 之后的 Provider 实现与调度器 |

### 15.3 新增测试（12 个，`src/providers/tests.rs`）

| 测试 | 覆盖 |
| --- | --- |
| `registers_two_mock_providers` | 注册两个 Mock（`mock-cn`/`mock-intl`）；默认列表按 id 排序；`len == 2` |
| `lookup_by_kind_and_region` | `get`/`require` 返回同一 `Arc`；另一 region 查不到 |
| `duplicate_registration_is_rejected` | 重复 key 与重复 id 均被拒绝（`Config`），注册表长度不变 |
| `registration_requires_a_non_empty_id` | 空 id 拒绝 |
| `disabling_removes_a_provider_from_request_routing` | 禁用后 `require` 返回 `NoCapacity`、默认列表为空，`get`/`list_all` 仍可见；可重新启用；未注册启停报错 |
| `capabilities_default_to_false` | 9 个能力位默认 `false`、`declared()` 为空、JSON 全 false、未覆盖的 Provider 上报默认值 |
| `capability_bits_are_wired_to_names` | `get`/`set` 双向一致；`Model`/`Agent`/`Hybrid` 分类与 `declared()` 顺序 |
| `default_operations_return_unsupported` | 7 个默认方法均返回 `Unsupported`，`agent_task` 消息含实例 id 与能力名，HTTP 501 |
| `cn_and_international_are_independent_providers` | Qoder CN / INTL 独立注册、独立键与 id；禁用 CN 不影响 INTL |
| `concurrent_readers_see_a_consistent_registry` | 8 个读线程 × 500 次 + 1 个写线程 × 500 次启停，全程无 panic 且不变量成立 |
| `verification_state_serialises_and_maps_from_config` | 4 个状态 `snake_case` 序列化；`is_verified`/`routable`；`Mock → Experimental`；`ProviderInfo` JSON 字段与反序列化 |
| `unsupported_capability_lookup_returns_a_classified_error` | 声明 `chat`/`streaming` 的 Provider：查询未声明的 `agent_task` → `Unsupported`；未注册 → `ModelUnavailable`；禁用 → `NoCapacity` |

### 15.4 修改文件

| 文件 | 变更 |
| --- | --- |
| `src/providers/mod.rs` | 新增。trait、`ProviderRef`/`ChatStream`/`StreamItem`、`ProviderKey`、`Capability`/`Capabilities`、`ProviderClass`、`VerificationState`、`ProviderInfo`、`UpstreamModel`、`AgentTaskRequest`/`AgentTask`/`AgentTaskState`/`CancelRequest`、`unsupported_capability`、`Debug for dyn Provider` |
| `src/providers/registry.rs` | 新增。`ProviderRegistry`（注册/查询/启停/能力/验证状态/列表/keys/len） |
| `src/providers/tests.rs` | 新增。轻量 `MockProvider`（仅 `info`/`capabilities`，其余走默认实现）+ 12 个测试 |
| `src/lib.rs` | 声明 `pub mod providers`；注释同步（占位目录只剩 `api`/`scheduler`/`metrics`） |
| `docs/ARCHITECTURE.md` | §2.2 验证状态条目改为已实现的 4 状态模型；补 Provider 依赖说明 |
| `PROGRESS.md` | 本记录与状态更新 |

**未修改**：`src/protocol/*`、`src/error/*`、`src/storage/*`、`src/accounts`、`src/sessions`、`src/quota`、`src/config`（仅复用 `ProviderKind`/`Region`/`Verification`）、`migrations/0001_init.sql`、`Cargo.toml`/`Cargo.lock`、`src/main.rs`。未新增依赖，未实现 Mock Provider（M7）、HTTP 路由（M8）、CLI（M9）、调度器或 UI。

### 15.5 实际验证输出

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.87s
exit=0

$ cargo test
test result: ok. 82 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
（另有 main 与 doc-tests 各 0 个）
exit=0

$ cargo build
   Compiling polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.76s
exit=0
```

测试总数 70 → 82；M1–M5 的测试名称与断言未变。

### 15.6 已知问题

1. **无具体 Provider**：只有 trait 与 Registry，`MockProvider` 仅存在于测试中；M7 才提供可注入错误的 Mock Provider。
2. **能力位与实现可能脱节**：能力是声明值，Registry 不校验“声明 `chat` 但方法仍走默认 `Unsupported`”的不一致；该一致性只能由测试或真实调用暴露（当前测试里刻意存在这种 Provider）。
3. **`Unsupported` 先于能力检查**：调用方直接调 `Provider::chat` 时不会预检能力位，由 trait 默认实现报错；`require_capability` 是路由侧的显式预检，两者都需要调用方选用。
4. **`Verification::Mock` 的映射**：mock 在 `VerificationState` 中表现为 `Experimental`，区分 mock 依赖 `ProviderKind::Mock`；若将来出现非 mock 但仅本地可用的 Provider，需要新的状态表达。
5. **依赖偏差**：`providers` 直接使用 `quota::QuotaSnapshot` 作为返回类型（`ARCHITECTURE` §2.1 允许列表未列 `quota`），以避免重复定义额度结构；属于只读领域结构复用，已在 §2.2 注明。
6. **`enabled` 双来源**：`ProviderInfo.enabled` 为 Provider 上报的初始值，运行时以 Registry 覆盖为准；直接调用 `provider.info()` 的代码可能看到过期值。
7. **Agent Task 类型为占位**：`AgentTaskRequest`/`AgentTask`/`AgentTaskState` 只覆盖提交与状态，无轮询、事件流、工作区文件语义；实现 Agent Provider 前需要扩充。
8. **未测项**：无请求级路由（无 API）、无真实流式取消、无多线程写注册（只测启停并发）、无性能与内存数据；Test 中的并发测试为 8 读 1 写 × 500 次，不构成压测。

### 15.7 下一步 M7 入口

M7 Mock Provider：在 `src/providers/mock.rs` 实现确定性 Provider（非流式 `chat` + 流式 `chat_stream`，含 `ToolCallStart`/`ToolCallArgsDelta` 增量、`Usage`、`Finish`），通过 `Capabilities` 声明实际实现的能力位，并支持注入 `AuthInvalid`(401)、`QuotaExhausted`(402)、`RateLimited`(429)、`UpstreamServer`(500)、`Timeout` 与取消；集成测试覆盖正常、每个错误码、客户端取消后上游被中断。M7 复用 M6 Registry 与 M5 协议/校验器，不实现 HTTP 路由、不接真实上游、不改动 Schema。等待用户下达任务。

## 16. M7 执行记录（2026-10-29）

**目标（来自 §9 M7）**：确定性 Mock Provider —— 非流式 + 流式（工具调用增量、usage、finish），可注入 401/402/429/500/超时/取消。
**验收**：测试覆盖正常、每个错误码、客户端取消后停止产生事件，且验证实际事件内容与顺序。

### 16.1 新增模块

| 文件 | 内容 |
| --- | --- |
| `src/providers/mock.rs` | `MockProvider` + `MockConfig` + `MockReply` + `MockToolCall` + `FaultMode`/`FaultScope`/`FaultInjection` + 流式状态机 `MockStreamState` + 每实例取消注册表 |
| `src/providers/mock_tests.rs` | 21 个行为测试（见 §16.5） |
| `src/providers/mod.rs` | 声明 `mod mock;` / `#[cfg(test)] mod mock_tests;`，re-export `MockProvider`、`MockConfig`、`MockReply`、`MockToolCall`、`FaultMode`、`FaultScope`、`FaultInjection` |

复用 M5 `protocol::{ChatRequest, ChatResponse, StreamEvent, StreamEventValidator…}` 与 M6 `Provider` trait / `Capabilities` / ProviderKey / Registry，未新增任何协议或 Provider 抽象。

### 16.2 Mock Provider 能力

| 需求 | 实现 |
| --- | --- |
| 实现现有 Trait | `#[async_trait] impl Provider for MockProvider`；实现 `info`/`capabilities`/`list_models`/`chat`/`chat_stream`/`cancel`；`agent_task`、`auth_status`、`quota` 保留 trait 默认 `Unsupported` |
| 非流式 Chat | `chat` 返回 `ChatResponse`：内容由配置生成，`usage` 始终存在，`finish_reason` 为 `Stop`（无工具调用）或 `ToolCalls` |
| 流式 Chat | `chat_stream` 返回 `ChatStream`，事件由状态机逐条生成：`Start → ReasoningDelta* → TextDelta* → ToolCallStart* → ToolCallArgsDelta*（多调用轮转交错）→ Usage → Finish` |
| 模型列表 | `list_models` 返回 `MockConfig.models`（默认 `mock-chat`、`mock-reasoner`）；请求未配置的模型返回 `ModelUnavailable`（404） |
| 能力声明 | `model_discovery`、`chat`、`streaming`、`tools`、`cancellation` = `true`；其余（含 `agent_task`、`native_session`、`authentication`、`quota`）= `false` |
| 可配置响应内容 | `MockReply::Fixed(text)` 或 `MockReply::Echo(prefix)`；可选 `reasoning`；可选 `tool_calls: Vec<MockToolCall>`（id/name/JSON 参数） |
| 可控延迟 | `response_delay`（非流式）、`event_delay`（流事件间隔）、`chunk_size`（每次增量的字符数） |
| 故障注入 | 8 种模式，见 §16.3 |
| 请求取消 | `Provider::cancel` + 流内取消注册；见 §16.4 |
| 注册到 Registry | `MockProvider::shared(config)` 返回 `ProviderRef`，可直接 `ProviderRegistry::register` |
| VerificationState | 固定 `Experimental`；`info.is_mock()` 为真；**永远不会是 `Verified`** |

**确定性**：内容只由请求与配置决定；usage 用字符数估算（`ceil(chars/4)`，输入取全部消息文本，输出取文本 + 推理 + 工具参数）；请求序号是每实例 `AtomicU64` 计数器，仅用于响应 id（`<instance>-<ordinal>`）与故障定位。两个独立实例在相同输入下产生相同文本、usage、finish reason 与响应 id。

### 16.3 故障注入设计

```text
FaultInjection { mode, scope, at, after_events, timeout }
```

- `mode`：`AuthInvalid`(401)、`QuotaExhausted`(402)、`RateLimited`(429，`retry_after = 1`)、`UpstreamServer`(上游 500，网关对外 502)、`Timeout`(504)、`Cancelled`(499)、`Transport`(502)、`StreamInterrupted`。
- `scope`：`Both`（默认）/ `Chat` / `Stream`，用于只影响一种调用方式。
- `at`：`None` = 每次请求都注入；`Some(n)` = 仅第 n 个请求（0 起，按实例的请求序号）。
- `after_events`：流式调用在发出 N 个事件后失败；`0` 表示在 `Start` 之前失败（此时 `chat_stream` 直接返回 `Err`，不产生流）。
- `timeout`：`Timeout` 模式在报错前模拟的等待时间（默认 10 ms）。
- `StreamInterrupted`：**不**发 `Error`，而是直接结束流（无终止事件），用于验证 M5 校验器的 `ensure_terminated()` 会拒绝被截断的流；非流式调用无截断表达，映射为 `UpstreamServer`。

`UpstreamServer` 的网关状态码沿用 M2 既定映射（502）；注入的“上游 500”记录在 `GatewayError.upstream_status = Some(500)`，不伪造 500 响应。

### 16.4 流式取消机制

- **无后台任务**：`chat_stream` 用 `futures_util::stream::unfold` 把 `MockStreamState` 直接包成 `ChatStream`；事件在每次 `poll` 时按阶段生成，客户端丢弃流即释放生成器，不存在需要 abort 的 Tokio Task。
- **延迟可被打断**：每个事件前的 `event_delay` 通过 `tokio::select!` 与取消通知竞争，取消后立即返回 `Error(Cancelled)` 并结束流；取消检查发生在生成事件之前，因此取消后不会再产生内容事件。
- **取消注册表**：`CancelRegistry`（每实例一个 `Mutex<HashMap<String, Arc<CancelSlot>>>`，不是全局静态）；`chat_stream` 以 `request_id`（缺省 `mock-stream-<instance>-<ordinal>`）注册，`Provider::cancel` 置位 `AtomicBool` 并用 `Notify` 唤醒等待。
- **生命周期释放**：流状态持有 `CancelRegistration`，其 `Drop` 在流结束或丢弃时按 `Arc::ptr_eq` 移除对应条目；`in_flight_requests()` 可观察残留，测试断言结束时为 0。
- **互不影响**：每个流持有独立的 `MockStreamState` 与取消槽；取消一个请求不会触碰其他请求（测试中同时开启两个流验证）。
- `cancel` 未知/已结束的 `request_id` 返回 `BadRequest`（400），避免静默成功。

### 16.5 新增测试（21 个，`src/providers/mock_tests.rs`）

| 测试 | 覆盖 |
| --- | --- |
| `mock_provider_registers_with_the_registry` | 注册到 Registry；`is_mock`、`Experimental`（非 `Verified`）；`chat`/`streaming`/`tools`/`model_discovery`/`cancellation` 可用，`agent_task`/`native_session` 为 `Unsupported` |
| `list_models_returns_the_configured_catalogue` | 模型列表返回配置值 |
| `unknown_model_is_rejected` | 未配置模型 → `ModelUnavailable`（404） |
| `non_streaming_chat_is_deterministic` | 两个独立实例同输入 → 相同 id/内容/finish/usage；精确断言 token 估算 |
| `non_streaming_echoes_the_last_user_turn` | 多轮消息（system/user/assistant/user）输入，回复最后一条 user；input token 精确 |
| `non_streaming_tool_calls` | 非流式工具调用：id/name/参数 JSON、`finish_reason = tool_calls`、文本为空 |
| `streaming_text_events_are_ordered_and_validated` | 文本分块内容与顺序；`Start`/`Usage`/`Finish` 位置；校验器通过并 `Finished` |
| `streaming_tool_calls_interleave_argument_fragments` | 两个工具调用：`ToolCallStart` 顺序与 `index`；参数增量按 `a,b,a,b…` 交错；按 id 重组 JSON 与配置一致 |
| `streaming_usage_and_finish_are_terminal` | `ReasoningDelta` 先于文本；`Usage` 唯一且位于 `Finish` 之前；`reasoning_tokens` 与 `output_tokens` 精确 |
| `auth_invalid_fault_returns_401` | 401（非流式与流式）；`blames_account`；`Chat` 作用域不影响流式 |
| `quota_exhausted_fault_returns_402` | 402；不可重试 |
| `rate_limited_fault_returns_429` | 429 + `retry_after`；可重试 |
| `upstream_server_fault_records_upstream_status_500` | `UpstreamServer`、网关 502、`upstream_status = 500` |
| `timeout_fault_returns_504` | 超时分类与 504 |
| `transport_fault_is_classified` | `Transport` 与 502 |
| `cancelled_fault_reports_499` | `Cancelled` 与 499 |
| `faults_target_a_single_request_by_ordinal` | `at = Some(1)` 只影响第 2 个请求，前后请求正常 |
| `mid_stream_fault_emits_a_terminal_error` | 3 个事件后发 `Error(UpstreamServer)`；校验器 `Failed`；注册表清空 |
| `stream_interruption_truncates_without_a_terminator` | 流在 2 个 delta 后直接结束、无终止事件；校验器拒绝；注册表清空；非流式映射 `UpstreamServer` |
| `cancelling_a_stream_stops_generation_and_releases_the_registration` | 取消后只收到 `Error(Cancelled)` 且无后续事件；另一并发流正常完成；结束后 `cancel` 返回错误；`in_flight_requests` 归零 |
| `concurrent_streams_do_not_interfere` | 两个并发流各得各的文本、均可通过校验器、注册表归零 |

### 16.6 修改文件

| 文件 | 变更 |
| --- | --- |
| `src/providers/mock.rs` | 新增（§16.1–§16.4） |
| `src/providers/mock_tests.rs` | 新增 21 个测试 |
| `src/providers/mod.rs` | 声明 `mock`/`mock_tests`，re-export Mock 类型 |
| `PROGRESS.md` | 本记录、状态、任务清单与下一步 |

**未修改**：`src/protocol/*`、`src/error/*`、`src/storage/*`、`src/accounts`、`src/sessions`、`src/quota`、`src/config`、`src/security`、`src/lib.rs`、`src/main.rs`、`migrations/0001_init.sql`、`Cargo.toml`/`Cargo.lock`、`docs/*`。未新增依赖，未实现 HTTP 路由（M8）、CLI（M9）、调度器或 UI，未调用真实上游。

### 16.7 实际验证结果

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.54s
exit=0

$ cargo test
test result: ok. 103 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
（main 与 doc-tests 各 0 个）
exit=0

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s
exit=0
```

测试总数 82 → 103（新增 21）；M1–M6 的测试名称与断言未变。

### 16.8 已知问题

1. **网关状态码与注入状态**：`UpstreamServer` 对外仍是 502（M2 映射），注入的上游 500 只体现在 `upstream_status`；若需字面 500，应在 API 层另行决定，而不是改错误分类。
2. **故障定位依赖调用顺序**：`at = Some(n)` 按实例的请求序号匹配；并发调用时“第 n 个请求”由调用到达顺序决定，不再可预测。并发场景应使用 `scope` 或无故障配置。
3. **工具调用不校验请求**：配置了 `tool_calls` 就会返回，不检查请求的 `tools`、`tool_choice` 或工具名是否存在；`ToolChoice::None` 也照样触发。
4. **取消只覆盖流式**：非流式 `chat` 没有可取消句柄；`response_delay` 期间无法取消（M7 不引入后台任务）。
5. **截断流是“非法”流**：`StreamInterrupted` 故意不终止，调用方必须把 `ensure_terminated()` 失败映射为下游错误；M8 需要明确 SSE 层的处理。
6. **usage 是估算**：按字符数 `ceil(chars/4)` 计算，不是 tokenizer，不能用于计费比对。
7. **时长依赖真实时钟**：延迟与超时使用 `tokio::time::sleep`；测试只验证分类与顺序，不验证精确时延，也没有性能/内存数据。
8. **取消注册表为进程内状态**：按实例保存在内存，重启即丢失；不跨进程、不持久化。
9. **响应 id 依赖序号**：`<instance>-<ordinal>` 在同一实例内唯一但不跨重启稳定；下游若需要幂等键应使用请求自带的 `request_id`。
10. **未覆盖的 Trait 能力**：`auth_status`、`quota`、`agent_task` 仍走默认 `Unsupported`，Mock 无法用于验证这三条路径。
11. **仍无 HTTP/SSE 序列化测试**：Mock 只到协议层；OpenAI/Anthropic 线格式与 SSE 分帧属 M8。
12. **Git**：仓库仍无提交；`LICENSE` 仍未生成。

### 16.9 下一步 M8 入口

M8 最小 API：`POST /v1/chat/completions`（流式 SSE + 非流式）、`GET /v1/models`、`GET /health`，gateway key 鉴权（Bearer 或 `x-api-key`，常量时间比较）。路由通过 `ProviderRegistry::require_capability` 解析 Mock Provider；`StreamEvent` 映射为 OpenAI chunk 序列，客户端断开时丢弃流即可触发 Mock 的取消路径；未知模型在调用 Provider 之前返回 404。M8 不接真实上游、不改 Schema、不做 Web UI/调度器。等待用户下达任务。

## 17. M8 执行记录（2026-10-29）

**目标（来自 §9 M8）**：`POST /v1/chat/completions`（流式/非流式）、`GET /v1/models`、`GET /health`，gateway key 鉴权（Bearer 或 `x-api-key`，常量时间比较）。
**验收**：对 Mock 跑通 OpenAI 形状；无 key 返回 401；未知模型返回 404 且不产生上游调用。

### 17.1 路由与状态

| 项 | 实现 |
| --- | --- |
| 框架 | Axum 0.8（既有依赖），`axum::Router`；未新增依赖 |
| 路由 | `GET /health`（公开）、`GET /v1/models`、`POST /v1/chat/completions`（均要求 API Key） |
| 状态 | `ApiState { registry: Arc<ProviderRegistry>, catalog: Arc<ModelCatalog>, auth: Arc<ApiKeyAuth> }`，`Clone` 共享 |
| 构造 | `ApiState::discover(registry, keys)`：从已启用 Provider 发现模型 + 构建鉴权；`api::router(state)` 返回 `Router` |
| 请求链路 | `require_api_key` 中间件 → `Json<ChatCompletionRequest>` 解析 → `validate()` → `catalog.resolve()` → `registry.require_capability()` → `Provider::chat`/`chat_stream` → `ChatResponse`/`StreamEvent` → OpenAI JSON/SSE |
| 错误 | 统一 `error_response(&GatewayError)`：`ErrorKind::http_status()` + `openai_error_body()`；`retry_after` 写入 `Retry-After` 头 |

### 17.2 请求转换（`src/api/openai.rs`）

- `ChatCompletionRequest` 使用 `#[serde(deny_unknown_fields)]`：`frequency_penalty`、`presence_penalty`、`seed`、`response_format`、`logprobs`、`parallel_tool_calls` 等未实现参数**返回 400**，不静默忽略。
- 字段：`model`、`messages`、`stream`、`stream_options.include_usage`、`max_tokens`/`max_completion_tokens`（二选一）、`temperature`、`top_p`、`stop`（字符串或数组）、`tools`、`tool_choice`、`n`、`user`。
- 校验：model 非空、messages 非空、`n == 1`、token 上限 > 0、二者不同时设置、`temperature ∈ [0,2]`、`top_p ∈ [0,1]`、tool 类型必须为 `function`、`tool_choice` 指定函数时必须提供 `tools`。
- 消息：`system`/`developer`/`user`/`assistant`/`tool`；字符串或 content parts（`text` → 文本块，其他类型 → `ContentBlock::Unsupported` 保留）；assistant 的 `tool_calls` 转 `ContentBlock::ToolCall`；`tool` 消息必须有 `tool_call_id` 并转 `ContentBlock::ToolResult`。
- `user` 写入内部 `metadata`；`max_completion_tokens` 优先于 `max_tokens`；请求 id 由 `Uuid::new_v4()` 生成。
- 响应：`chat.completion` + `choices[0].message`（`content` 为空时为 `null`，可带 `reasoning_content`/`tool_calls`）+ `finish_reason` + `usage`（`prompt_tokens`/`completion_tokens`/`total_tokens`）。

### 17.3 SSE 映射（`src/api/sse.rs`）

内部事件**不直接透传**，逐条转换为 `chat.completion.chunk`：

| 内部事件 | SSE chunk |
| --- | --- |
| `Start` | `delta: {"role":"assistant","content":""}` |
| `TextDelta` | `delta: {"content": ...}` |
| `ReasoningDelta` | `delta: {"reasoning_content": ...}` |
| `ToolCallStart` | `delta.tool_calls[0] = {index, id, type: "function", function: {name, arguments: ""}}` |
| `ToolCallArgsDelta` | `delta.tool_calls[0] = {index, function: {arguments: ...}}` |
| `Usage` | 缓存；`include_usage=true` 时在 `finish` 后输出 `choices: []` + `usage` 的独立 chunk |
| `Finish` | `delta: {}` + `finish_reason`，随后（可选 usage chunk）+ `data: [DONE]` |
| `Error` | `data: {"error": {...}}` + `data: [DONE]` |
| 流未终止 | 补发 `upstream_error` 帧 + `[DONE]`（不把截断当成功） |

- 工具调用 index 由 `SseState` 分配：优先使用事件里的 `index`，缺失时按首次出现顺序分配；参数增量按 id 映射回同一 index，支持多工具交错。
- `finish_reason` 映射：`stop/length/tool_calls/content_filter`；`Cancelled`/`Error`/`Unknown` → `stop`；上游 `Other(s)` 非空时原样透传。
- 响应体流直接持有 Provider 流：客户端断开 → 丢弃 `Body` → 丢弃 Mock 流 → 取消注册被 `Drop` 清理；没有后台任务需要 abort。

### 17.4 鉴权

- `ApiKeyAuth` 保存 SHA-256 哈希（复用 `security::hash_token`），`verify` 对每个 key 调用常量时间 `verify_token` 并 `|` 聚合，不短路、不比较明文。
- 提取顺序：`Authorization: Bearer <key>` → `x-api-key`。`Authorization` 存在但不是 Bearer 时**不**回退到 `x-api-key`。
- 缺少、错误、空字符串、空 `Bearer` 均返回 401 + OpenAI 错误体（`code = invalid_api_key`）；不记录密钥，错误消息不含 key。
- `/health` 不鉴权，只返回 `{"status":"ok","version":...}`；构造 `ApiKeyAuth` 时拒绝空 key 或空列表（`ErrorKind::Config`）。

### 17.5 模型目录（`src/api/catalog.rs`）

- `ModelCatalog::discover(registry, credential)`：遍历 `registry.list_enabled()`，仅对声明 `model_discovery` 的 Provider 调用 `list_models()`；发现失败记录日志并跳过，不影响其他 Provider。
- 条目记录公开 id、上游 id、`ProviderKey`、显示名与上下文窗口；按 id + provider 排序。
- `GET /v1/models` 每次按 Registry 当前启停状态过滤：禁用 Provider 的模型立即消失，也不可路由。
- Mock 模型带 `polygate: {mock: true, test_model: true, verification: "experimental", kind: "mock", region: "cn"}`，`owned_by` 为 `mock-cn`，明确标注为测试模型。
- `resolve()` 在 Provider 调用之前执行；未命中 → `ModelUnavailable`（404），测试断言 Mock 的 `request_count()` 保持 0。

### 17.6 新增测试（19 个，`src/api/tests.rs`）

| 测试 | 覆盖 |
| --- | --- |
| `health_is_public_and_minimal` | `/health` 无鉴权、200、只有 status/version、不泄漏 key 或 provider 信息 |
| `valid_bearer_key_is_accepted` | Bearer 正确 → 200 |
| `x_api_key_header_is_accepted` | `x-api-key` 正确 → 200 |
| `missing_and_wrong_keys_are_rejected` | 无 key/错误 key 的 models 与 chat 均 401 + `invalid_api_key`；上游未被调用 |
| `empty_keys_are_rejected` | 空 `Bearer`、空 `x-api-key`、非 Bearer 的 Authorization（即使同时带有效 x-api-key）→ 401 |
| `models_list_mock_models_as_test_models` | 列表含 `mock-chat`/`mock-reasoner`，`owned_by=mock-cn`，`polygate.mock/test_model=true`，`verification=experimental` |
| `disabled_providers_are_hidden_and_not_routable` | 禁用后列表为空、聊天 404、上游未调用 |
| `unknown_model_is_404_without_upstream_call` | 未知模型 404 + `model_unavailable`，`request_count == 0` |
| `non_streaming_chat_returns_openai_response` | `chat.completion` 形状、`chatcmpl-` id、content、`finish_reason=stop`、usage 精确 |
| `non_streaming_tool_calls_are_preserved` | 多轮输入（含 assistant tool_calls 与 tool 结果）、tools/tool_choice 解析、响应 tool_calls 与 `finish_reason=tool_calls` |
| `sse_streams_text_chunks_and_done` | `text/event-stream`、role delta、文本分块拼接、finish_reason、`[DONE]` 结尾 |
| `sse_streams_tool_call_deltas_with_indices` | 工具 start（index/id/type/name）、参数增量拼接为合法 JSON、`finish_reason=tool_calls` |
| `sse_interleaves_multiple_tool_calls` | 两个工具 index 0/1；参数增量按 `0,1,0,1…` 交错，按 index 重组正确 |
| `sse_reports_usage_when_requested` | `include_usage=true` 时 usage chunk（`choices: []`）位于 finish 之后、`[DONE]` 之前 |
| `sse_mid_stream_fault_is_reported_as_an_error_frame` | 中途故障 → 200 + `upstream_error` 帧 + `[DONE]`，注册表归零 |
| `provider_faults_map_to_openai_errors` | `AuthInvalid`→401、`QuotaExhausted`→402、`RateLimited`→429（含 `Retry-After`）、`UpstreamServer`→502；流式请求在 body 前失败 |
| `invalid_requests_are_rejected` | 12 种非法请求（空 model、空 messages、n=2、未知字段、缺 content、tool 缺 id、tool_choice 无 tools、越界参数、双 token 上限、未知角色）与非法 JSON → 400 |
| `client_disconnect_cancels_the_provider_stream` | 读取一帧后丢弃 body → `in_flight_requests()` 1 → 0 |
| `concurrent_requests_do_not_interfere` | 两个并发请求各自返回自己的文本，请求计数为 2 |

### 17.7 修改文件

| 文件 | 变更 |
| --- | --- |
| `src/api/mod.rs` | 新增：Router、`ApiState`、`ApiKeyAuth`、鉴权中间件、三个 handler、错误响应 |
| `src/api/openai.rs` | 新增：严格请求 DTO、校验、内部转换、OpenAI 响应/usage/finish_reason 映射 |
| `src/api/catalog.rs` | 新增：`ModelCatalog`/`ModelEntry`，模型发现与路由解析 |
| `src/api/sse.rs` | 新增：SSE 会话状态机与 `to_openai_sse` 适配器 |
| `src/api/tests.rs` | 新增：19 个 Router 级集成测试 |
| `src/lib.rs` | 声明 `pub mod api`；空占位只剩 `scheduler`/`metrics` |
| `src/providers/mock.rs` | 新增只读方法 `request_count()`（测试用） |
| `PROGRESS.md` | 本记录、状态、任务清单与下一步 |

**未修改**：`src/protocol/*`、`src/error/*`、`src/storage/*`、`src/accounts`、`src/sessions`、`src/quota`、`src/config`、`src/security`、`src/main.rs`、`migrations/0001_init.sql`、`Cargo.toml`/`Cargo.lock`、`docs/*`。未新增依赖，未实现 Anthropic Messages/Responses API、调度器、账号池、Web UI 或 CLI。

### 17.8 实际验证结果

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.67s
exit=0

$ cargo test
test result: ok. 122 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
（main 与 doc-tests 各 0 个）
exit=0

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.15s
exit=0
```

测试总数 103 → 122（新增 19）；M1–M7 的测试名称与断言未变。

### 17.9 已知问题

1. **二进制未监听**：`src/main.rs` 仍只打印版本；`serve`/`doctor`/端口绑定属 M9，Router 目前只在测试中构造。
2. **无账号池**：调用使用空占位 `Credential`；Mock 忽略凭据。真实 Provider 需要账号/凭据接入（阶段 3），当前不涉及。
3. **目录只发现一次**：`ApiState::discover` 在构造时调用；Provider 后续目录变化不会刷新。启停过滤是实时的（禁用立即不可见、不可路由）。
4. **同名模型去重**：`resolve` 返回按 (id, provider) 排序后的第一个，没有优先级/权重；跨 Provider 的模型路由留给调度器。
5. **上游 5xx 映射为 502**：沿用 M2 分类；若要字面 500 需在 API 层另行决定。
6. **SSE 错误使用 200**：中途故障以 200 + error 帧 + `[DONE]` 结束（OpenAI 兼容做法），客户端不能依赖 HTTP 状态发现中途失败。
7. **截断流不可区分**：Provider 流没有终止事件时补发 `upstream_error`，与显式中途错误使用相同帧；没有单独的 “interrupted” 代码。
8. **SSE 默认无 usage**：与 OpenAI 一致，只有 `stream_options.include_usage=true` 才发送 usage chunk；chunk 内 `usage` 为 null。
9. **严格解析比 OpenAI 更严**：`deny_unknown_fields` 会拒绝常见但未实现的参数（如 `frequency_penalty`、`seed`、`response_format`、`logprobs`），返回 400 而非忽略。
10. **仅 `n=1`**：`n>1`、`stream_options` 的其他字段、`parallel_tool_calls` 均不支持（400）。
11. **多模态未转换**：非文本 content part 作为 `ContentBlock::Unsupported` 传入 Provider；Mock 不处理也不报错，真实 Provider 的拒绝/转发策略未实现。
12. **鉴权范围有限**：静态 key 列表，不读数据库、不支持轮换/过期；admin token 与 `api_keys` 表属后续阶段。
13. **无 keep-alive/限流/metrics/TLS**：SSE 无注释帧，长静默可能被代理断开；无速率限制、无请求日志、无指标；TLS 依赖反代（M9/部署阶段）。
14. **未验证**：无真实 Provider、无 Linux/Docker 部署、无并发压力与内存数据；19 个测试均为 Mock + Router 级。
15. **Git**：仓库仍无提交；`LICENSE` 仍未生成。

### 17.10 下一步 M9 入口

M9 配置与 CLI：`serve`（读取 TOML + `POLYGATE_*`，构建 `SecretBox`、`Storage`、`ProviderRegistry`（默认 Mock），调用 `ApiState::discover`，在 `server.listen`（默认 `127.0.0.1:8787`）上监听）、`doctor`（配置、数据库、主密钥、端口四项检查）、`config-example`；空配置给出可读错误。M9 不接真实上游、不做调度器/Web UI。等待用户下达任务。

## 18. M9 执行记录（2026-10-29）

**目标（来自 §9 M9）**：TOML + `POLYGATE_*` 环境变量；`serve` / `doctor` / `config-example`；默认仅监听 `127.0.0.1`。
**验收**：空配置启动给出可读错误；`doctor` 输出配置、数据库、主密钥、端口检查；实际启动并通过 curl 验证。

### 18.1 启动链路（`src/runtime/mod.rs`）

`Gateway::bind` 完成全部可失败步骤后才绑定端口，`Gateway::serve` 才接收请求：

| 序 | 步骤 | 失败行为 |
| --- | --- | --- |
| 1 | `Config::load`（TOML + 环境变量 + 校验） | 解析错误、未知字段、非法监听、非法日志级别、空/过短/占位 Key → 拒绝启动 |
| 2 | 日志初始化（`EnvFilter`，`try_init`） | 级别已在配置校验阶段拒绝 |
| 3 | 网关 API Key（`Config::gateway_keys`） | 空 → `Config` 错误，不打印 key |
| 4 | 主密钥（`SecretBox::from_env_or_file`） | 无效 → 拒绝启动，错误不含密钥内容 |
| 5 | SQLite（`Storage::open_url` + 迁移） | 打开/迁移失败 → 拒绝启动 |
| 6 | Registry（仅 `enabled=true`；仅 Mock） | 未实现 kind 或无启用 Provider → 拒绝启动 |
| 7 | 模型目录（`list_models` 发现） | 单 Provider 失败只记日志并跳过 |
| 8 | `ApiState` + `api::router` | — |
| 9 | `TcpListener::bind` | 地址非法/被占用 → `Config` 错误 |
| 10 | `axum::serve(...).with_graceful_shutdown(...)` → `Storage::close` | 服务器错误 → `Internal`；关闭后 `is_closed()` 为真 |

### 18.2 配置结构与优先级

**优先级（高 → 低）**：`--config`/`--data-dir` → `POLYGATE_*` 环境变量（含 `POLYGATE_CONFIG` 指定文件） → `<data-dir>/config.toml` → 内置默认。
环境变量覆盖 TOML；`Config::load_with_env` 允许测试注入环境查询而不修改进程环境。

| 配置 | 字段 / 环境变量 | 默认 |
| --- | --- | --- |
| 监听地址 | `server.listen` / `POLYGATE_LISTEN` | `127.0.0.1:8787`（校验可解析） |
| SQLite | `database.url` / `POLYGATE_DATABASE_URL` | `sqlite://data/polygate.db?mode=rwc`（相对数据目录解析） |
| 网关 Key | `auth.api_keys`、`auth.bootstrap_api_key` / `POLYGATE_API_KEY` | 无（serve 必须配置） |
| 主密钥来源 | `POLYGATE_MASTER_KEY` 或 `<data-dir>/master.key` | 首次 serve 生成 32 字节密钥（Unix 0600） |
| Mock 开关 | `providers[].enabled` / `POLYGATE_MOCK_ENABLED` | 零配置默认启用 |
| 日志级别 | `logging.level` / `POLYGATE_LOG_LEVEL` | `info`（`EnvFilter` 指令） |

**校验规则**：监听地址必须可解析；日志级别必须是非空合法 `EnvFilter` 指令；API Key 禁止空串、短于 16 字符、示例占位值（`change-me`、`polygate-dev-key`、`test-key` 等）；配置结构 `deny_unknown_fields`，在 `[auth]` 写 `master_key` 直接报错（主密钥不得进普通配置）。所有校验失败都会阻止 serve/doctor 启动。

### 18.3 CLI（`src/cli/mod.rs`）

```text
polygate [--config PATH] [--data-dir DIR] serve
polygate [--config PATH] [--data-dir DIR] doctor
polygate config-example
```

- `serve`：加载并校验配置 → 初始化日志 → `Gateway::bind` → 打印实际监听地址 → `axum::serve` + 关闭信号。
- `doctor`：六项检查 `config`、`api_key`、`master_key`、`database`（打开+迁移+关闭）、`providers`、`listen`（临时 bind 后释放）；任一失败以非零码退出；不打印 key/主密钥值；不删除已有数据。
- `config-example`：输出 `config.example.toml`（无真实 key，`api_keys` 为注释）。
- 关闭信号：Unix `SIGTERM` + Ctrl+C，其他平台仅 Ctrl+C；两者共用同一 shutdown future。

### 18.4 Mock Provider 注册

- 注册发生在 `runtime::build_registry`：遍历配置中 `enabled=true` 的 Provider，仅允许 `kind = "mock"`；HTTP Handler 只通过 Registry `require_capability` 取句柄，未直接实例化 Mock。
- Mock 行为由配置驱动：`options["mock.reply"]`（固定回复）、`options["mock.tool_calls"]`（JSON 数组，含 id/name/arguments）；模型取 `providers.models[].id`（`raw_id`/别名映射尚未实现）。
- 未实现的非 Mock Provider 且启用时拒绝启动，不会静默降级；禁用则不注册。

### 18.5 优雅关闭机制

- `Gateway::serve` 把 shutdown future 交给 `axum::serve` 的 `with_graceful_shutdown`；信号产生后停止接收新连接、等待在途请求结束。
- 服务器退出后调用 `Storage::close()`（改为 `&self`）关闭连接池；`Storage::is_closed()` 供测试/诊断断言。
- 无后台任务需要 abort：SSE body 随连接释放并触发 Mock 的取消路径。

### 18.6 新增测试（22 个）

| 文件 | 测试 | 覆盖 |
| --- | --- | --- |
| `src/config/mod.rs`（7） | `environment_overrides_toml` | 环境变量覆盖 TOML；key 合并去重 |
| | `placeholder_and_short_keys_are_rejected` | 占位 key / 过短 key → `Config` 错误 |
| | `empty_api_key_entries_are_rejected` | 空 `api_keys` 项与空 bootstrap key 被拒绝 |
| | `master_key_cannot_be_stored_in_config` | `[auth] master_key` → unknown field 错误 |
| | `invalid_listen_and_empty_log_level_are_rejected` | 非法监听 / 空日志级别 |
| | `mock_provider_can_be_disabled_by_environment` | `POLYGATE_MOCK_ENABLED` 真/假/非法值 |
| | `process_environment_is_read_by_load` | `Config::load` 读取真实进程环境（带清理 guard） |
| `src/runtime/tests.rs`（7） | `serves_openai_over_a_real_tcp_listener_and_closes_the_pool` | **真实 TCP**：health、401（且不泄漏 key）、models、非流式、SSE（含 usage 与 `[DONE]`）、关闭后连接池释放 |
| | `restart_keeps_database_state_and_serves_tool_calls` | 关闭→重启后 settings 持久化；TCP 工具调用响应与参数 JSON |
| | `bind_fails_without_a_gateway_key` | 缺 API Key 拒绝启动 |
| | `bind_fails_on_an_unusable_master_key` | 非法 master.key 拒绝启动，错误不含密钥内容 |
| | `bind_rejects_unimplemented_provider_kinds` | 启用 qoder → 拒绝启动 |
| | `bind_fails_when_the_address_is_already_in_use` | 端口占用 → 拒绝启动 |
| | `registry_contains_every_enabled_provider` | 仅注册启用 Provider，禁用者不注册 |
| `src/cli/tests.rs`（8） | `help_lists_every_command` | `--help` 列出三个命令 |
| | `parses_subcommands_and_global_options` | 子命令与全局参数解析 |
| | `config_example_is_complete_and_contains_no_key` | 示例可解析、无 key、命令可执行 |
| | `doctor_passes_and_never_prints_the_key` | 六项检查通过；输出不含 key |
| | `doctor_reports_a_missing_api_key` | 缺 key 检查失败，其余检查仍执行 |
| | `doctor_reports_an_unusable_master_key` | 非法 master.key 检查失败且不泄漏内容 |
| | `doctor_reports_a_busy_listen_address` | 端口占用 → listen 检查失败 |
| | `serve_command_rejects_invalid_configuration_before_starting` | 非法监听在启动前被拒绝 |

要求 1–18 映射：1 CLI 帮助、2 config-example、3 TOML 加载、4 环境覆盖、5 缺 API Key、6 非法主密钥、7 非法监听、8 SQLite 初始化、9 Registry 初始化、10–14（health/models/非流式/SSE/工具调用，真实 TCP）、16 连接池释放、17 重启持久化、18 不泄漏密钥，均有对应测试。**15（SIGTERM/Ctrl+C）**：关闭路径由同一 shutdown future 在真实 TCP 测试中验证；OS 信号投递本身未在 Windows 测试进程内触发（见 §18.9）。

### 18.7 修改文件

| 文件 | 变更 |
| --- | --- |
| `src/runtime/mod.rs` | 新增：`Gateway::bind`/`serve`、Registry 构建、Mock 配置映射、Debug |
| `src/runtime/tests.rs` | 新增：真实 TCP 端到端、启动失败、关闭、持久化测试 |
| `src/cli/mod.rs` | 新增：clap CLI、`execute`、日志初始化、`shutdown_signal`、doctor 检查与报告 |
| `src/cli/tests.rs` | 新增：CLI/配置/doctor 测试 |
| `src/main.rs` | 改为薄入口，调用 `cli::run()` 并返回退出码 |
| `src/lib.rs` | 声明 `cli`、`runtime` |
| `src/config/mod.rs` | 新增 `logging`、`auth.api_keys`、`load_with_env`、`gateway_keys`、key/监听/日志校验、`POLYGATE_LOG_LEVEL`/`POLYGATE_MOCK_ENABLED`、`deny_unknown_fields`、7 个测试 |
| `src/storage/mod.rs` | `close` 改为 `&self`；新增 `is_closed` |
| `config.example.toml` | 新增 `[logging]`、`auth.api_keys` 说明、mock 选项说明 |
| `docs/ARCHITECTURE.md` | 模块树加入 `cli`/`runtime`；新增 §14（启动链路、配置优先级、CLI、边界） |
| `PROGRESS.md` | 本记录、状态、任务清单与下一步 |

**未修改**：`src/protocol/*`、`src/error/*`、`src/api/*`（M8 的 Router 与 SSE 未改）、`src/providers/*`（未改 Mock 行为）、`src/accounts`、`src/sessions`、`src/quota`、`src/security/*`、`migrations/0001_init.sql`、`Cargo.toml`/`Cargo.lock`。未新增依赖；未接真实 Provider、未实现调度器/Web UI/Anthropic/Responses/Docker/systemd。

### 18.8 实际验证结果

执行环境：Windows，`rustc 1.98.1 (48a229cea 2026-09-01)`。

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
    Checking polygate v0.1.0 (<repo>)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.93s
exit=0

$ cargo test
test result: ok. 144 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
（main 与 doc-tests 各 0 个）
exit=0

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.45s
exit=0
```

测试总数 122 → 144（新增 22）；M1–M8 的测试名称与断言未变。

**真实进程 + curl（`./target/debug/polygate.exe serve --config target/m9-check/config.toml --data-dir target/m9-check`，监听 `127.0.0.1:8799`）**

| 请求 | 状态 | 关键结构 |
| --- | --- | --- |
| `GET /health` | 200 | `{"status":"ok","version":"0.1.0"}` |
| `GET /v1/models`（无 key） | 401 | OpenAI 错误体，响应不含 key |
| `GET /v1/models`（Bearer） | 200 | `object=list`；`mock-chat`/`mock-reasoner`；`polygate.mock=true`、`test_model=true`、`verification=experimental` |
| `POST /v1/chat/completions`（非流式） | 200 | `object=chat.completion`；`content="M9 runtime check"`；`finish_reason=stop`；`usage={prompt:2,completion:4,total:6}` |
| `POST /v1/chat/completions`（`stream=true` + `include_usage`） | 200 | `content-type: text/event-stream`；role chunk → 2 个 content chunk → finish(`stop`) → usage chunk(`choices:[]`) → `data: [DONE]` |

```text
$ polygate --config ... doctor
[ok] config: loaded 1 provider entries, listen 127.0.0.1:8799
[ok] api_key: 1 key(s) configured
[ok] master_key: usable (master.key file, 32 bytes)
[ok] database: reachable; migrations applied; pool closed
[ok] providers: mock-cn:mock
[FAIL] listen: cannot bind 127.0.0.1:8799: ... (os error 10048)   # 服务仍在运行时
exit=1
（停止服务后复跑：listen -> [ok] ... is bindable，exit=0）
```

`polygate --help` 列出 `serve` / `doctor` / `config-example`；`polygate config-example` 输出完整 TOML（`api_keys` 为注释，无真实密钥）。

### 18.9 已知问题

1. **信号未被测试进程投递**：优雅关闭路径由 shutdown future 在真实 TCP 测试中覆盖；`SIGTERM` 处理仅在 Unix 编译，Windows 无 SIGTERM，未做 OS 信号级自动化测试。
2. **没有自动生成网关 Key**：未配置 Key 时 serve 拒绝启动（有意为之）。零配置开发也必须提供 `POLYGATE_API_KEY`。
3. **doctor 有轻微副作用**：打开数据库会创建文件并应用待执行迁移（不删除/不改写数据）；`doctor` 不生成主密钥，仅在 serve 首次启动时生成。
4. **主密钥来源优先级**：`POLYGATE_MASTER_KEY` 优先于 `master.key`；两者不一致会导致既有凭据无法解密（当前尚无凭据写入）。
5. **关闭无超时**：`with_graceful_shutdown` 会等待在途请求；长时间 SSE 可能延迟进程退出，尚无 drain 超时配置。
6. **监听检查存在 TOCTOU**：doctor 先 bind 再释放，真正 serve 时地址可能已被占用。
7. **仅 Mock**：启用任意非 mock Provider 直接拒绝启动；模型目录启动时发现一次，无热刷新；`raw_id`/别名未映射。
8. **无账号池**：HTTP 调用仍使用占位 `Credential`；额度、会话、调度均未接入。
9. **日志**：只支持级别指令，无 JSON/文件输出配置；`try_init` 使测试不会重复安装订阅器，但也无法断言日志内容。
10. **传输安全**：无 TLS、限流、metrics；默认只监听 loopback，对外部署需反代（第 7 级）。
11. **测试环境依赖 loopback**：真实 TCP 测试需要本机回环网络；受限沙箱中可能失败。
12. **发布准备未完成**：`LICENSE` 未生成（暂定 MIT，来源清点未完成），Git 仍无提交，无 CI/Docker/systemd。

### 18.10 下一阶段开发建议（阶段 03：CodeArts 账号池 + 调度器）

> M10 已按真实源码依赖重排 M11+：先账户管理、再通用签到框架、再真实 Provider 与签到，
> 调度器/会话粘性放到 M19。本节保留为 M9 当时的建议，最新顺序见 §19.6 与
> `docs/MIGRATION_MATRIX.md` §5。

1. **Account Manager**：账号 CRUD、凭据经 `SecretBox` 入库（已有 `Storage::create_account`）、认证状态机与刷新；为调度器提供只读视图。
2. **Scheduler v1**：候选过滤、SWRR/RR/Least-Conn、并发租约、冷却/半开、`NoCapacity`；把 `/v1/chat/completions` 从占位凭据切换到“会话绑定 + 选号 + 解密凭据”。
3. **Session Manager**：显式会话 ID → metadata → 前缀指纹；绑定持久化与 TTL；绑定失效返回 `SessionBindingInvalid`。
4. **Management API**：账号/额度/会话的只读接口优先，写接口其次；所有响应不得包含凭据。
5. **验收**：两账号 Mock 100 会话 50:50、权重 2:1、超并发 `NoCapacity`、取消后租约归零、重启不丢绑定；真实 CodeArts 联调仍留到第 5 级。
6. **发布准备**：完成依赖/来源许可证清点后生成 `LICENSE`；首次 Git 提交与 CI（fmt/clippy/test）。

## 19. M10 源码审计与迁移矩阵（2026-10-29）

### 19.1 目标与边界

以 `https://github.com/RobbsLuo/Coding2API` 为主要功能参考，研究多渠道、账号管理、
签到、额度、调度和 API 功能如何迁移到 PolyGate 的 Rust 架构；辅助参考
`.refs/chengzhi-c_dsh-wtq-pool`、`.refs/Johnsheng1_codearts2api`、
`.refs/liu5269_zcode2api`。明确排除 OpenCode Zen 上游。

**本里程碑只做研究**：未修改数据库 Schema、未开发真实 Provider、未实现签到请求、
未开发 Web UI、未提前执行 M11。

### 19.2 实际读取的源码

Coding2API：

- 任务层：`src/tasks/checkin.py`、`src/tasks/runner.py`、`src/tasks/pacer.py`、
  `src/tasks/status.py`、`src/tasks/__init__.py`。
- 管理 API：`src/api/admin_credentials.py`（单条/批量/状态）。
- 存储：`src/db/schema.sql`（明确不建 `checkins` 表）。
- 调度/粘性：`src/engine/scheduler.py`、`src/engine/affinity.py`、
  `src/engine/limits.py`、`src/engine/executor.py`（选号/轮换/粘性调用点）。
- Provider：`src/provider/base.py`、`codearts/client.py`、`qoder/client.py`、
  `trae/client.py`、`codebuddy/{client,checkin,intl,headers}.py`、
  `officeace/{client,events}.py`、`kilo/client.py`、`zen/client.py`。
- 前端：`web/src/pages/CredentialsPage.tsx`、`web/src/api/client.ts`。
- 测试：`tests/tasks/test_checkin*.py`、`tests/provider/*/test_checkin.py`、
  `tests/engine/test_scheduler.py`。

其他参考：

- dsh：`src/checkin-time.ts`、`src/host.ts`（`runCheckinPass`、`qoderCheckinAll`、
  `traeCheckinAll`、`startAutoCheckin`、`checkin-slots.json`）、`src/source-pool.ts`、
  `src/accounts.ts`。
- Johnsheng1：`benefit.py`（`/api/v1/benefit/claim`）。
- 本地 `codearts2api`：未发现签到代码；作为账号池/调度参考（操作者本人作品，可复用）。
- zcode2api：`app/captcha.py`、`captcha_node/solver.js`（验证码求解，明确排除）。

### 19.3 关键结论

1. Coding2API 注册 8 个 Provider：`trae`、`codebuddy`、`codebuddy_intl`、`zen`、
   `kilo`、`qoder`、`codearts`、`officeace`；Zen 永久排除。
2. 签到覆盖 5 个渠道：CodeArts、Qoder、CodeBuddy CN、TRAE、OfficeAce；
   CodeBuddy Intl 显式 `checkin = None`（活动不可用）；Kilo/Zen 无签到。
3. 签到是“任务层通用逻辑 + Provider 协议状态机”两层；单账号 `CheckinTask.run_once`、
   一键 `POST /api/credentials/batch`（action=checkin，上限 200、并发 4）、自动
   `TaskRunner` 60s/600s 循环、状态查询 `GET /api/credentials/{id}/checkin`。
4. 签到结果**不落库**：`_done_scopes`/`_fired_slots` 在内存；`schema.sql:9` 明确
   不建签到表，靠上游幂等。重启后无法回答“今天签没签”，且不幂等的上游有重复领取风险。
5. 自动签到成功后**不会立即刷新额度**（`CheckinTask` 未装配 `on_success`）；手动/批量
   会 `schedule_probe`。`credit_events` 只记净变化，不归因“签到 +N”。
6. 时点窗口 `[时点-抖动, 时点]` **不补签**；dsh 的 `dueCheckinSlot` 会补做当天已过
   未消费的槽，并把槽位持久化到 `checkin-slots.json`（保留 3 天），还支持 Qoder
   10:00 开放时刻的延迟重试。这是迁移时最值得吸收的两点。
7. 调度排序（`engine/scheduler.py:258-309`）为“免费优先 → pin → 在途最少 →
   36h 到期积分 → 7d 到期积分 → health → 剩余积分 → credential_id”。两个静态指标
   相同的账号会退化到 `credential_id`，顺序请求不会 50:50；PolyGate 必须用显式
   SWRR/RR 或 least-inflight 修正。
8. 许可证：Coding2API/dsh/Johnsheng1/magpie 为 MIT；zcode2api 为 AGPL-3.0（禁止复制）；
   本地 `codearts2api`/`codearttray` 无 LICENSE 文件但为操作者本人作品（2026-10-29 确认），
   代码可复用（作者授权，保留声明并标注来源）。PolyGate 自身仍无 LICENSE，
   发布前必须补齐并完成依赖清点。
9. PolyGate `Provider` trait 目前没有 refresh/check-in 钩子，`Capability` 没有
   Checkin；`ProviderKind` 没有 OfficeAce/Kilo。M11/M12 需要扩展契约；不能照搬
   Python 的 `getattr` 能力探测。

### 19.4 交付文档

- `docs/PROVIDER_MIGRATION.md`：8 个 Provider 的认证/模型/推理/流式/工具/会话/额度/
  签到/源码/验证状态总表与逐项迁移动作；Zen 排除、Kilo 待定、WorkBuddy/CodeBuddy
  关系澄清。
- `docs/CHECKIN_RESEARCH.md`：签到专项审计（单账号、一键、自动、状态、额度刷新、
  失败/过期、定时实现、结果存储、dsh 对照、分类结论、PolyGate 设计建议、未决问题）。
- `docs/MIGRATION_MATRIX.md`：与 M1–M9 的逐子系统矩阵、账号池/两账号公平分析、
  许可证审计、M11+ 里程碑重排与退出条件、明确排除项、证据缺口。
- 本文件 `PROGRESS.md`：新增本节，更新顶部状态与下一步。

### 19.5 未做事项（按要求）

- 未修改任何 Rust 源码、`Cargo.toml`、`migrations/` 或 `config.example.toml`。
- 未实现真实 Provider、账号池、调度器、签到任务、管理 API 或 Web UI。
- 未调用任何真实上游、未使用任何真实凭据。
- 未提前执行 M11。

### 19.6 下一步（M11+，详见 `docs/MIGRATION_MATRIX.md` §5）

按“账号认证与管理 → 签到基础设施 → 真实 Provider → 自动/一键签到 → 负载均衡与
会话粘性 → Web UI”的用户优先级，并满足“真实签到依赖已验证 Provider”：

1. **M11** 账户与凭据管理（AccountService/Admin API、手工导入、AuthStatus、RBAC 最小集）。
2. **M12** Provider 契约扩展 + 通用签到框架（Mock）：`Capability::Checkin`、
   `CheckinOutcome` 三态、任务 runner/Pacer/时点抖动、scope 去重、`checkin_runs` 迁移。
3. **M13** CodeArts Provider 实测（auth/refresh 单写者/模型/额度/聊天/SSE/工具/错误）。
4. **M14** CodeArts 签到（单账号 → 批量 → 自动；默认关闭）。
5. **M15–M18** CodeBuddy CN / Qoder / TRAE（OfficeAce 为可选项，需先确认是否纳入
   现有 Provider 范围）：每个 Provider 单独验收，再接各自签到；Intl 活动不可用要显式标注。
6. **M19** 调度器与会话粘性（SWRR/RR/Least-Conn、权重、并发租约、冷却、健康、
   额度感知；两账号 50:50、权重 2:1、重启绑定恢复）。
7. **M20** Web UI；**M21** 部署、LICENSE/NOTICE、依赖许可证清点与发布。

说明：M19 与 M12–M18 无强依赖，可提前到 M14 之后；本顺序按用户给定优先级排列。

### 19.7 验证

- 本里程碑只写 Markdown，未改代码，因此未重新执行 `cargo build/test/clippy/fmt`；
  M9 的 **144 passed / 0 failed** 仍是当前代码基线。
- 文档中的源码路径、函数名、行号均来自本机 `.refs/` 读取；未使用 README 作为
  能力证据（README 只用于定位）。
- 未验证项已逐条标注“参考实现单测/作者实测/本项目未实测”，不得据此声明
  Provider 为 `verified`。

## 20. M11 执行记录（2026-10-29）

### 20.1 目标与边界

实现账户与凭据生命周期服务，为后续 Scheduler / CheckinTask / Provider Adapter 提供
稳定接口。**未实现**：真实 Provider、真实上游调用、额度查询、签到任务、调度器、
HTTP 管理端点、RBAC 或 Web UI；未修改 `0001_init.sql`。

### 20.2 新增模块与文件

| 文件 | 内容 |
| --- | --- |
| `src/account_service/mod.rs` | 独立服务模块；依赖方向 `account_service → storage/security/accounts(domain)` |
| `src/account_service/service.rs` | `AccountService`、`CreateAccount`、`AccountUpdate`、`AuthorizationContext` |
| `src/account_service/credential.rs` | `CredentialService`、`CredentialStatus` |
| `src/account_service/tests.rs` | 20 个服务层测试（19 个 async + 1 个状态映射） |
| `src/accounts/mod.rs` | `AuthStatus` 重构为 `Unknown/Valid/Expired/Invalid/RefreshRequired/Restricted`；`Credential` 增加 `Drop` 递归 zeroize；`NewAccount` Debug 脱敏 |
| `src/storage/credentials.rs` | 新增 `CredentialInfo`、`insert_if_absent`、`update_if_revision`、`info`、`exists`；upsert 时递增 revision |
| `src/storage/mod.rs` | re-export `CredentialInfo`；`Storage::create_account` 继续作为账号+初始凭据事务入口 |
| `migrations/0002_credential_revision.sql` | 为 `credentials` 增加 `revision` 乐观锁列；写入 `schema_version=2` |
| `src/runtime/mod.rs` | `Gateway` 持有 `AccountService`，新增 `Gateway::accounts()`；启动时复用同一个 `SecretBox` |
| `docs/ARCHITECTURE.md` | 模块树、依赖方向、启动链更新 |

未新增依赖。

### 20.3 账号与凭据生命周期设计

**账号身份**：

- 账号 ID 是 `uuid v4`（`Storage::create_account` / `AccountRepo::insert` 生成）。
- `provider` 是 provider 实例 id（默认 `<kind>-<region>`）；`kind` + `region` 在创建后
  不可修改，`AccountUpdate` 刻意不包含它们。
- 允许同一 `ProviderKind` 下多个账号；允许重复的 display name——名称不是身份，
  不做手机号/名称唯一键。重复提交需要由调用方幂等键解决。
- `account_service` 不依赖任何具体 Provider Adapter，也不知道签到协议。

**创建**：

- `CreateAccount { provider, kind, region, name, credential: Option<Value>, ... }`。
- 有初始凭据 → `Storage::create_account` 在同一事务写 `accounts` + `credentials`；
  加密使用 `SecretBox::encrypt_for_account(account_id)`，AAD 绑定账号 ID。
- 无初始凭据 → 只写 `accounts` 行；之后用 `CredentialService::save` 添加。
- 校验：provider/name 非空、weight/max_concurrency ≥1、凭据必须是 JSON 对象。

**查询/修改/删除**：

- `get`/`list`/`list_by_provider`/`list_available`。
- `list_available(kind, region)` 只返回：`enabled`、`AuthStatus::is_dispatchable()`
  （Unknown/Valid）、且凭据存在的账号；CN/Intl 通过 kind+region 精确过滤。
- `update` 只改 name/weight/max_concurrency/quota_group/metadata；`set_enabled`
  单独控制本地启停；`delete` 依赖现有外键级联 credentials/sessions/quota。
- 禁用账号不删除凭据、不改认证状态。

**凭据**：

- `CredentialService::save` 是 create-only；已存在则 `BadRequest`，避免两个管理会话
  静默覆盖。
- `update(account_id, credential, expected_revision)` 使用 CAS：SQL
  `WHERE account_id=? AND revision=?`；冲突返回 `BadRequest`，调用方需重新读取。
- `status` 返回 `present/revision/created_at/updated_at/encryption_version/auth_status`，
  不返回明文。
- `invalidate` 把账号认证状态标为 `Invalid`，保留凭据与 enabled；`mark_refresh_required`
  标为 `RefreshRequired`。
- 解密只按 account_id：`CredentialService::load` 无“列出全部凭据”接口；AAD 防止
  把 A 账号的密文用 B 账号解密。
- 明文缓冲：`serde_json::to_vec` 后立即 `zeroize`；`Credential` 的 `Drop` 递归
  zeroize 字符串值；`Debug` 全部脱敏。

### 20.4 认证状态与转换

`AuthStatus`：`Unknown` / `Valid` / `Expired` / `Invalid` / `RefreshRequired` /
`Restricted`。与 `Account.enabled` 独立。

转换规则（`src/accounts/mod.rs` `can_transition_to`）：

- 任意状态 → `Unknown`（显式重置）；同状态幂等。
- `Unknown` → 任意状态。
- `Valid` → `Expired` / `Invalid` / `RefreshRequired` / `Restricted`。
- `Expired` → `Valid` / `Invalid` / `RefreshRequired` / `Restricted`。
- `RefreshRequired` → `Valid` / `Expired` / `Invalid` / `Restricted`。
- `Invalid` → 仅 `Valid`（重新认证）或 `Unknown`。
- `Restricted` → `Valid` / `Expired` / `Invalid` / `RefreshRequired`。

失败分类（`AccountService::mark_auth_failure`）：

| ErrorKind | 目标状态 |
| --- | --- |
| `AuthInvalid` | `Invalid` |
| `AuthExpired` | `Expired` |
| `AccountDisabled` / `ChannelRestricted` | `Restricted` |
| `Timeout` / `Transport` / `UpstreamServer` / `RateLimited` / `QuotaExhausted` / 其他 | **不变** |

因此一次网络超时不会把凭据标成 `Invalid`；测试覆盖。

### 20.5 数据库迁移

- `0001_init.sql` **未修改**。
- 新增 `migrations/0002_credential_revision.sql`：
  `ALTER TABLE credentials ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;`
  并写入 `schema_version=2`。
- 理由：`updated_at`（毫秒）不足以做并发 CAS，两个同毫秒写入会一起匹配；显式
  `revision` 是避免并发覆盖的最小改动。
- `Storage::create_account` 仍然是一个事务；迁移对已有库幂等（sqlx 版本记录）。
- 存储测试的版本/迁移计数从 1 更新为 2。

### 20.6 测试与验证

新增 20 个 `account_service::tests`，覆盖任务要求的 17 项并额外覆盖失效标记、
Region 越界、缺失账号、元数据更新、遗留状态映射：

1. 单账号创建（含凭据）。
2. 同 Provider 两账号。
3. CN/Intl 独立与越界拒绝。
4. 启用/禁用且保留凭据。
5. 凭据密文落盘（不含明文）。
6. 正确主密钥解密。
7. 错误主密钥解密失败且不泄漏。
8. 跨账号 AAD 解密失败。
9. 凭据更新 + revision 递增 + 旧 revision 冲突。
10. 凭据删除（账号保留，可重新 save）。
11. 账号删除级联 credentials/sessions。
12. 认证状态转换、非法转换拒绝、超时不改状态、AuthInvalid 标 Invalid。
13. 并发凭据更新恰好一个成功。
14. 文件数据库关闭重开后账号/凭据仍可读。
15. Debug/错误/密文不泄漏明文。
16. 无凭据账号：可见、不可调度、之后可添加凭据。
17. 重复 display name 生成不同 ID。
18. 显式 `invalidate` 保留密文与 enabled。
19. 缺失账号/非法输入返回受控错误。
20. 遗留 `ok/disabled` 字符串映射。

验证结果（Windows，`rustc 1.98.1`）：

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0

$ cargo test
running 164 tests
test result: ok. 164 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0
```

M9 的 144 个测试全部保留；新增 20 个。

### 20.7 已知问题与 M12 入口

已知问题：

1. **没有 HTTP 管理端点 / RBAC**：M11 只实现服务层；账号管理 API 留给后续管理面。
2. **认证状态没有真实探测来源**：`auth_status` 目前由调用方显式设置或测试驱动；
   真实 Provider 的 `auth_status`/额度探测在 M13 才接入。
3. **`CredentialRepo::save` 仍是无条件 upsert**：供存储层测试/内部使用；生命周期
   路径必须走 `CredentialService::save/update`。后续可把 repo 方法收窄为 `pub(crate)`。
4. **`list_available` 是 N+1 查询**：每个账号查一次凭据状态；账号池规模小，暂可接受，
   调度器阶段可改为一条 JOIN。
5. **账号元数据更新没有 CAS**：只有凭据有 revision；账号字段 last-wins，后续如需
   并发编辑再加版本列。
6. **`CreateAccount`/`NewAccount` 的原始 `Value` 不实现 Drop zeroize**：真正进入
   加密路径的序列化缓冲会 zeroize，`Credential` 自身 Drop 也会清字符串；调用方
   持有的请求 JSON 仍是普通 Value。
7. **未接真实 Provider**：不得把本阶段的服务层测试当成 Provider 验证。

**M12 入口**：

- 复用 `AccountService::list_available(kind, region)` 作为签到候选来源。
- 复用 `authorization_context_for(account_id, kind, region)` 获取解密凭据，并在
  请求后丢弃。
- 用 `CredentialService::mark_refresh_required` / `invalidate` 记录签到/刷新失败；
  用 `mark_auth_failure` 做错误分类，禁止把超时标成 Invalid。
- 新增 `Capability::Checkin`、`CheckinOutcome` 三态（Done/Already/Unavailable/Failed/
  Unsupported）、任务 runner、Pacer、时点/抖动与 `checkin_runs` 迁移；**只接 Mock**。
- 不实现真实签到、不查真实额度、不做调度器、不开发 Web UI。
- **实际 M12 调整（2026-10-29）**：签到改为独立的 `CheckinExecutor` 抽象，**未扩展
  `Provider` trait**、未加 `Capability::Checkin`；Pacer/全局节流未实现。理由与现状见 §21。

## 21. M12 执行记录（2026-10-29）

### 21.1 目标与边界

实现持久化的签到任务引擎：领域状态机、SQLite 去重、单账号/批量/全部签到、按 Provider
时区与补签窗口的自动调度、Mock 执行器、崩溃恢复与优雅关闭。

**未实现**：真实签到协议、真实 Provider、验证码相关能力、真实额度查询、账号负载均衡、
管理 HTTP API、Web UI、Pacer/全局节流；未修改 `0001_init.sql` 与 `0002_credential_revision.sql`，
未扩展 `Provider` trait 或 `Capability`。

### 21.2 新增模块与文件

| 文件 | 内容 |
| --- | --- |
| `src/checkin/mod.rs` | 独立模块与依赖方向说明；re-export 全部公开类型 |
| `src/checkin/model.rs` | `CheckinStatus`、`CheckinErrorCategory`、`CheckinRequest`、`CheckinOutcome`、`CheckinResult`、`CheckinRecord`、`CheckinPolicy`、`CheckinSchedule`、`Clock`、时区/业务日期/抖动/补签窗口函数、错误分类 |
| `src/checkin/executor.rs` | `CheckinExecutor` trait（`supports` / `check_status` / `checkin`）、`CheckinProbe`、`MockCheckinExecutor`（按账号脚本、调用计数） |
| `src/checkin/quota.rs` | `QuotaRefreshHook` trait、`QuotaRefreshRequest`、`MockQuotaRefreshHook` |
| `src/checkin/service.rs` | `CheckinService`：单账号/批量/全部、schedule CRUD、`run_due`、`recover_interrupted`、结果脱敏 |
| `src/checkin/scheduler.rs` | `CheckinScheduler`、`Sleeper`/`TokioSleeper`；启动恢复 + sleep/tick 循环 + shutdown |
| `src/checkin/tests.rs` | 23 个服务层测试（含崩溃/并发/时区/脱敏/优雅关闭） |
| `src/storage/checkin.rs` | `CheckinRepo`：runs 的 claim/complete/list、schedule CRUD；`RunClaim` 四态 |
| `src/storage/mod.rs` | 声明 `checkin` 模块、re-export `CheckinRepo`/`RunClaim`、`Storage::checkin()` |
| `migrations/0003_checkin.sql` | `checkin_runs` + `checkin_schedules`；`schema_version=3` |
| `src/runtime/mod.rs` | `Gateway` 持有 `CheckinService`/`CheckinScheduler`，`Gateway::checkin()`；`serve` 启动/停止调度循环 |
| `Cargo.toml` / `Cargo.lock` | 新增 `chrono-tz 0.10.4`（MIT/Apache-2.0，含 `phf`/`siphasher` MIT/Apache-2.0） |
| `docs/ARCHITECTURE.md` | 模块树、依赖方向、§2.4 签到引擎、启动链 5.6 |
| `docs/MIGRATION_MATRIX.md` | M12 行标注实际实现与偏差 |

### 21.3 签到状态机

```
Pending ──claim──> Running ──> Succeeded | AlreadyCheckedIn | Failed | Unknown | Unsupported
                      ▲
Failed ──有界重试──────┘
```

- `Pending`：行已创建，尚无 worker 持有。仅 `insert_pending`（测试）或 claim 前的插入产生。
- `Running`：持有 `lease_until`。租约有效时其它 worker 得到 `Busy`；过期后得到
  `Interrupted`，只能通过状态查询确认或落 `Unknown`，**不重新执行**。
- `Succeeded` / `AlreadyCheckedIn`：成功；触发额度刷新，并把账号认证状态尽量标为 `Valid`。
- `Failed`：上游/本地明确拒绝（鉴权、限流、额度、参数、无凭据）。自动重试仅限：
  `status=failed` 且 `error_category=rate_limit` 且 `attempt_count < max_attempts`。
- `Unknown`：请求可能已到达上游但结果不可确认（超时、传输错误、上游 5xx、
  本地保存失败、进程在 Running 时崩溃且无法查询状态）。**从不重新执行签到**：
  自动循环直接跳过；手动再次调用只会向上游查询状态，确认成功/已签则补记，否则保持
  `Unknown`。不得把它当作 `Failed`。
- `Unsupported`：`CheckinExecutor::supports(kind) == false`，或执行器明确返回不支持；
  不读取凭据、不调用上游。

`CheckinErrorCategory`：`timeout / transport / upstream / auth / rate_limit / quota /
unsupported / storage / other`。只有 `rate_limit` 被标记为可自动重试；超时/传输/上游 5xx
映射为 `Unknown`，不进入重试。

### 21.4 数据库迁移

新增 `migrations/0003_checkin.sql`（`0001`/`0002` 未修改）：

- `checkin_runs`：`account_id`（FK → `accounts`，`ON DELETE CASCADE`）、`provider_kind`、
  `region`、`business_date`、`timezone`、`status`、`attempt_count`、`started_at`、
  `completed_at`、`last_error_category`、`result_summary`、`lease_until`、时间戳；
  **`UNIQUE(account_id, business_date)`** 是本地去重机制。
- `checkin_schedules`：`account_id` 主键（FK 级联）、`enabled`、`timezone`、`local_time`、
  `jitter_secs`、`catch_up_secs`、`max_attempts`、`updated_at`。
- 写入 `schema_version=3`；存储测试的版本/迁移计数同步更新为 3，并断言新表存在。

**边界**：本地唯一约束只保证“本机每个账号每个业务日只有一行/一个执行者”，不能保证远端
副作用恰好一次；因此引入了 `Unknown` 与“先查状态再决定”的恢复策略。

### 21.5 定时调度设计

- **时区**：`CheckinPolicy.timezone` 是 IANA 名称，业务日期 = `now` 在该时区的日期。
  没有 schedule 时用 Provider 默认：CN=`Asia/Shanghai`，Intl=`UTC`（占位，M13 由 Provider
  声明替换）；从不使用 VPS 本地时区。
- **时点**：`local_time = HH:MM`；DST 歧义取较早时刻，DST 跳变向前顺延到有效时刻。
- **抖动**：`jitter_offset(account_id, business_date, jitter_secs)` 用 FNV-1a 确定性计算，
  同一天重启不会换时点；抖动上限 24h。
- **补签**：occurrence 之后 `catch_up_secs` 内仍可执行；同时检查“今天”和“昨天”的本地
  日期，允许窗口跨午夜。超过窗口不自动补。
- **去重/领取**：每轮从 DB 读 schedule，重新计算 due；claim 使用条件 UPDATE + 租约。
  多个调度循环（滚动重启、两个进程）只有一个能领取同一 (账号, 日期)。
- **重启恢复**：`CheckinScheduler::run` 启动时先 `recover_interrupted()`；`Pending` 行由
  下一轮 claim 接管；`Running` 行按 §21.6 处理。
- **优雅关闭**：`run(shutdown)` 在 sleep 与 shutdown 间 `select!`；关闭后不再领取新任务，
  正在执行的批次完成后返回，随后 `Gateway::serve` 才关闭数据库连接池。
- **启用/禁用**：`checkin_schedules.enabled` 由 `CheckinService::set_schedule_enabled` 持久化切换；禁用只停止自动执行，不删除历史运行记录。
- **间隔**：默认 60s（`DEFAULT_TICK_SECS`），可通过 `with_interval` 调整（暂无配置文件项）。

### 21.6 幂等与崩溃恢复策略

| 场景 | 处理 |
| --- | --- |
| 创建后进程崩溃（`Pending`） | 下一次 claim 直接领取并执行；远端尚未调用 |
| `Running` 时崩溃 | 租约过期后先 `check_status`：确认成功/已签/失败则落对应状态；无法确认则落 `Unknown`，不重跑 |
| 上游成功但本地保存失败 | 本行仍为 `Running`；重启后只能靠状态查询确认，否则 `Unknown`；不重复执行 |
| 网络超时 | 执行器返回 `Err(Timeout)` → `Unknown`（不是 `Failed`）；同日再次手动触发只查状态，不重跑 |
| 两个 Worker 同时签到 | 条件 UPDATE 决定唯一 owner；另一个得到 `Busy` 并返回既有记录（`replayed=true`） |
| 重启后同一天再次触发 | `UNIQUE(account_id, business_date)` 命中既有行；终态直接返回，`Failed` 仅在自动策略允许时重试 |
| 状态查询不可用 | 落 `Unknown`，不执行签到请求；重复触发仍不执行 |

测试使用文件数据库并主动关闭连接池来模拟“上游成功后本地落库失败”，再用新 `Storage`
重开同一文件验证恢复路径（`upstream_success_with_failed_local_save_stays_unconfirmed`）。

### 21.7 测试与验证

新增 32 个测试（23 个 `checkin::tests` 服务层 + 9 个 `checkin::model` 纯逻辑），覆盖任务
要求的 20 项并补充：批量上限、无凭据账号、上游成功/本地保存失败、状态查询确认。

服务层测试清单（节选任务映射）：

1. 单账号签到；2. 两账号独立；3. 批量按输入顺序返回；4. 批量错误隔离；5. 不支持签到；
6. 同日去重（含批内重复）；7. 跨日再执行；8. 时区边界（schedule 时区优先）；
9. 并发 Worker 仅执行一次；10. 文件库重启后 runs/schedules 仍在；11. Pending 恢复；
12. Running 恢复（查询确认、不重跑）；13. Unknown 不盲目重试（自动跳过；手动只查状态，能确认则补记）；
14. AlreadyCheckedIn；15. 成功后额度刷新（含认证状态 `Valid`）；16. 刷新失败不回滚；
17. 自动签到同日只跑一次、次日再跑、禁用 schedule 后停止；18. 补签窗口内/外；19. 优雅关闭；
20. 敏感信息不进入记录（凭据值回显被替换为 `***`）；
21. 批量 >200 拒绝；22. 无凭据账号不调用上游；23. 上游成功后本地保存失败 → Running → Unknown。

全部使用可控 `TestClock`；唯一使用真实时间的等待是优雅关闭测试的 5 秒超时保护，
正常情况下立即返回。未启动服务、未访问真实上游。

实际验证（Windows，`rustc 1.98.1`）：

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0

$ cargo test
test result: ok. 196 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0
```

M1–M11 的 164 个测试全部保留；新增 32 个。

**网络说明（如实记录）**：首次 `cargo build` 在更新 crates.io index 时因本机 schannel
吊销检查失败（`CRYPT_E_REVOCATION_OFFLINE`，SSL connect error）无法下载 `chrono-tz`。
改用 `CARGO_HTTP_CHECK_REVOKE=false cargo build/test/clippy` 后成功；这是本机网络/证书
吊销检查问题，不是代码问题。CI/Linux 环境未验证。

### 21.8 已知问题与 M13 入口

已知问题：

1. **只有 Mock**：运行时 `MockCheckinExecutor` 仅支持 `ProviderKind::Mock`；真实 Provider
   账号签到返回 `Unsupported`。没有任何真实签到请求。
2. **未扩展 Provider 契约**：`Provider` trait 仍无 `checkin/checkin_status`，
   `Capability` 无 `Checkin`；这是有意推迟到 M13/M14，避免在协议未验证前固化契约。
3. **无管理 HTTP API / Web UI**：schedule 与运行记录只能通过 `CheckinService` 访问；
   无鉴权/RBAC 端点。
4. **无 Pacer / 全局节流**：批量并发固定 4、批次上限 200，但没有跨任务的全局限速；
   真实上游阶段需要补。
5. **认证状态副作用**：签到成功会把账号标为 `Valid`（若状态机允许），属于“成功调用即证据”
   的保守推断；`CheckinOutcome::Failed{Auth}`（执行器已判定）不会自动改认证状态，只有
   `Err(AuthInvalid/AuthExpired/...)` 会走 `mark_auth_failure`。
6. **手动重试无次数上限**：`Failed` 的手动重试不受 `max_attempts` 限制（操作者显式动作），
   自动重试受限；每次仍会递增 `attempt_count` 并留痕。
7. **`checkin_all` / `run_due` 存在 N+1 账号查询**：账号池规模小可接受，后续可改 JOIN。
8. **Intl 默认时区是占位 `UTC`**：待 M13+ Provider 声明真实时区；schedule 可覆盖。
9. **调度间隔不可配置**：固定 60s（可用构造器覆盖，尚无配置项）；抖动/补签已是
   schedule 字段。
10. **本地去重 ≠ 远端恰好一次**：上游没有幂等键时只能依赖状态查询；这是协议限制，
    不是实现缺陷。

**M13 入口**（CodeArts Provider 实测，见 `docs/MIGRATION_MATRIX.md` §5）：

- 先实现真实 Provider 的 auth/refresh/模型/额度/聊天，再决定签到契约：
  推荐让 CodeArts adapter 直接实现 `CheckinExecutor`（或由注册表按 kind 分发一个
  `CheckinExecutor`），**不要**把签到硬塞进聊天 `Provider` 的默认方法。
- 将 `Gateway::bind` 里的 `MockCheckinExecutor::support_only([Mock])` 替换为
  真实 dispatcher；无凭据/未验证的 Provider 保持 `Unsupported`。
- 用真实 `/v1/ops/delivery → claim → confirm` 语义实现 `check_status`（活动/已签/CLAIMED
  补 confirm）与 `checkin`；失败分类遵循 §21.3，超时一律 `Unknown`。
- 成功后接真实 `QuotaRefreshHook`（M13 的额度探测），不要把刷新失败回滚签到。
- 仍不开发管理 HTTP API/Web UI；schedule 管理与运行历史留在服务层。

## 22. M13 补充执行记录（2026-10-29）

### 22.1 目标与边界

按 M13 补充要求完成三件事：

1. 对本地修改版 `~/codearts2api` 相对 HEAD/原版做差异分析；
2. 研究 CodeArts 会话墙（`TM.00001041`）的触发条件与作用范围；
3. 实现会话绑定所需的最小机制：持久化映射、CAS 变更、显式失效/恢复、有界并发。

**未实现**：真实 CodeArts Provider（签名/STS/DPoP/模型/额度/Chat/SSE）、Scheduler 选号、
额度感知/公平负载、管理 HTTP API、Web UI、真实签到；未启动本地 Go 服务，未调用真实上游，
未使用真实凭据。完整证据与设计见 **`docs/CODEARTS_SESSION.md`**。

### 22.2 新增模块与文件

| 文件 | 内容 |
| --- | --- |
| `docs/CODEARTS_SESSION.md` | 差异分析、会话墙证据、映射设计、并发限制、已验证行为、未解决问题 |
| `src/session_service/mod.rs` | 独立策略层与依赖方向；公开类型 |
| `src/session_service/limits.rs` | `LimitPolicy`、`AccountLimiter`、`SessionGate`、有界等待与超时 |
| `src/session_service/service.rs` | `SessionBindingService`、`SessionKey`、`SessionLease`、`SessionRecovery`、失败分类 |
| `src/session_service/tests.rs` | 14 个服务层测试（粘性/并发/CAS/失效/恢复/超时/串行化/重启） |
| `src/sessions/mod.rs` | `Session` 增加 `binding_revision`、`invalid_reason` |
| `src/storage/sessions.rs` | `insert_if_absent`、`set_upstream`、`rebind`、`invalidate`、`reset_upstream`；读写新列 |
| `migrations/0004_session_binding_revision.sql` | 为 `sessions` 增加 CAS 版本与失效原因；`schema_version=4` |
| `src/runtime/mod.rs` | `Gateway` 持有 `SessionBindingService`，新增 `Gateway::sessions()`；不自动绑定 |
| `Cargo.toml` / `Cargo.lock` | dev-dependency 为 tokio 启用 `test-util`（仅测试需要；无新 crate） |

未新增生产依赖；未修改 `0001`–`0003` 迁移。

### 22.3 本地源码差异分析结论

对象：本地工作区（HEAD `f9cb52c` 之上）4 个文件，+298/−3：

- `internal/codearts/client.go`：全局 `sessionSlots`（默认 2、最大 3）固定 32-hex 槽 ID；
  请求前取槽并**覆盖** `request.ChatID`；发送 `x-ot-session-id` / `user-session-id`；
  识别响应体/流内 `TM.00001041` / “并发会话数已达上限”为 `benefit_cap`；cap 重试独立预算
  与等待，不消耗普通重试预算。
- `internal/auth/pool.go`：`benefit_cap` 不写模型冷却。
- `internal/config/config.go`：`CODEARTS_SESSION_SLOTS`（2）、`CODEARTS_SESSION_CAP_RETRY_SECONDS`
  （3）、`CODEARTS_SESSION_CAP_MAX_RETRIES`（8）、Benefit 模型列表。
- `internal/server/server.go`：新增 `/api/ui/codearts/proxy` 与 `PostJSON`，与会话无关；cap 映射 429。

**账号选择未修改**：`Pool.Candidates` / `Begin` / 路由策略与 HEAD 相同；会话偏好仍来自
`conversation.Context.AccountID → PreferredAccountID`。因此本地修改版不能作为 Scheduler 参考。

**会话 ID**：本地修改版没有“每下游会话一个上游会话”的映射；槽 ID 是进程级并发令牌，
不同会话/账号可复用同一 ID，且覆盖后 `conversation.Store` 保存的 `ChatID` 变成槽 ID，
README 宣称的稳定 `chat_id` / prompt cache 亲和实际失效。

**竞态与缺陷**（详见文档 §2.8）：全局槽跨账号共享、固定槽 ID 跨进程复用、槽不按账号隔离、
`Begin` 满时直接换号（无排队）、conversation 文件读改写无锁、无会话级串行化、无会话失效恢复。

### 22.4 会话墙结论

- 触发：同账号上游会话数达到 3；错误 `TM.00001041`，文本
  `并发会话数已达上限(3个)，请关闭部分会话后重试。`
- 形态：HTTP 400（Coding2API 实测注释与测试夹具）、429，以及 HTTP 200 的流内错误事件。
- 作用域：账号级、跨模型；与账号相关，与模型无关。
- 并发关系：不是纯在途上限；会话在流结束后滞留数十秒，Coding2API 使用“每账号 3 次启动/60s”
  滑动窗口。60s 为参考实现实测注释，非官方文档。
- 官方限制：未找到公开文档；错误文本本身给出数值 3。
- 证据等级：错误码/文本/状态码为**已证实**；账号级/跨模型为**强推断**；会话头才是计数键、
  固定槽 ID 可复用为**推测**，需要在 M13 主体用真实账号验证。

### 22.5 会话绑定设计

复用 `sessions` 表与 `SessionRepo`，唯一键 `(client_session_id, provider_kind, region)`；
新增 `binding_revision`（CAS）与 `invalid_reason`。

- `bind_if_absent`：仅插入；已存在时原样返回，**永不**因调用方建议改绑。
- `attach_upstream`：`account_id` + `binding_revision` 双条件 CAS；A 账号的 `chat_id`
  不可能写到 B 账号。
- `acquire`：校验绑定存在、未过期、`active`、账号存在/启用/认证可调度；缺失/失效/过期
  返回 `SessionBindingInvalid`，不创建、不迁移、不删除。
- `finish`：原子递增请求数并延长 TTL；释放会话锁与账号槽。
- `invalidate`：保留账号与上游 ID，记录原因，阻断获取；幂等。
- `reset_upstream`：同账号、清空上游 ID、恢复 active、重设 TTL；适配器必须重发完整历史。
- `rebind`：显式换账号，上游 ID 清空；并发改绑由 CAS 拒绝。
- `recover`：只接受 `ResetUpstream` / `Rebind` 两种显式策略。

不变量：同会话账号粘性；CN/Intl 分离；不同账号的上游 ID 不混用；并发不改错映射；
失效不静默丢失上下文。

### 22.6 并发与限制

`LimitPolicy::codearts()`：`max_in_flight=3`（实际取 `min(3, accounts.max_concurrency)`）、`max_starts=3` / `window=60s`、
`queue_timeout=75s`（略长于约 60s 的会话滞留窗口）、`serialize_session=true`。

- 账号级：in-flight 信号量 + 启动窗口；窗口配额不因流结束退还（对齐会话滞留）。
- 会话级：同一 `(kind, region, client_session_id)` 串行；不同会话互不阻塞。
- 有界等待：统一 deadline；超时 `NoCapacity`（503），不无限等待、不无限重试、不靠新建会话绕过。
- 取消安全：permit / mutex guard 随 future drop 释放。
- 进程内限制：账号槽、窗口、会话锁是进程内状态；多实例需后续改为数据库租约。

错误分类（`classify_session_failure`）：

| ErrorKind | SessionFailure |
| --- | --- |
| `SessionBindingInvalid` | `UpstreamSessionInvalid` |
| `AccountDisabled` / `ChannelRestricted` / `AuthInvalid` / `AuthExpired` | `AccountUnavailable` |
| `RateLimited` / `NoCapacity` | `AccountBusy` |
| 其他 | `Other` |

### 22.7 测试与验证

新增 14 个测试（`src/session_service/tests.rs`）：create-only 粘性、并发 bind 单赢家、
CN/Intl 隔离、attach 的账号/revision 约束、rebind 清空上游 ID、invalidate 保留上下文、
reset 恢复、过期不静默替换、禁用/认证失效阻断、账号槽有界与唤醒、队列超时、会话串行化、
重启持久化、失败分类。

实际验证（Windows，`rustc 1.98.1`）：

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0

$ cargo test
test result: ok. 210 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build
Finished `dev` profile [unoptimized + debuginfo] target(s)
exit=0
```

M1–M12 的 196 个测试全部保留；新增 14 个。存储迁移计数同步更新为 4。

### 22.8 已知问题与 M13 主体入口

已知问题：

1. **会话头语义未证实**：`x-ot-session-id` / `user-session-id` 是否是计数键、是否参与签名、
   与 body `chat_id` 的关系，需要真实账号对照实验。
2. **每会话绑定 vs 3 会话上限**：绑定数超过 3 时必然触发会话墙；Provider 必须选择
   “有限槽映射（牺牲亲和）”或“排队/拒绝（保亲和）”，本阶段未决定。
3. **多实例**：并发租约是进程内状态；两个实例合计会超过上游每账号上限。
4. **会话失效无独立错误码**：只能把上游 4xx/业务码映射为 `SessionBindingInvalid`，可能误判。
5. **无上游会话删除**：本地失效/重置不释放上游名额，只能等其自然过期。
6. **未接 Chat 路由/Provider**：`Gateway::sessions()` 只提供服务层入口。
7. **无管理 HTTP/UI**：绑定查询、失效、重置未暴露。
8. **未接入真实上游**：所有结论来自源码/测试夹具，不是本项目实测。

**M13 主体入口**（CodeArts Provider 实测）：

- 在真实 adapter 中按顺序调用：
  `bind_if_absent`（首次）→ `acquire`（每次请求）→ 上游调用 →
  `attach_upstream`（拿到 `chat_id` 后，account + revision CAS）→ `finish`。
- 上游签名/STS/DPoP/模型/额度/SSE 仍按 `docs/RESEARCH.md` §10 与 `docs/PROVIDER_MIGRATION.md` 实现；
  不得复制 AGPL/第三方无许可证代码（本地 Go 工程为操作者本人作品，可按授权复用并标注）。
- 把 `TM.00001041` / “并发会话数已达上限”映射为 `RateLimited`/`NoCapacity` 或专用会话墙分类；
  不得当作模型不可用或认证失败。
- 用真实账号验证：3 会话窗口、会话头作用、`chat_id` 复用、失效恢复、跨账号隔离。
- 仍不实现 Scheduler 公平选号/额度感知；由会话绑定返回的 `account_id` 作为硬约束交给后续 Scheduler。

## 23. M13-B — CodeArts Provider Adapter（2026-10-29）

### 23.1 目标与边界

在 M13-A 的会话绑定之上实现 CodeArts 的 `Provider` trait，并接入 HTTP Chat 路由与签到
执行器。按任务要求停在 M13-B：不做其他 Provider、不做完整 Scheduler、不做 Web UI、
不改旧迁移、不实现 Anthropic/Responses API。

### 23.2 新增/修改文件

| 文件 | 内容 |
| --- | --- |
| `src/providers/codearts/signer.rs` | `SDK-HMAC-SHA256` 签名（canonical URI 尾 `/`、全头签名、payload hash 头） |
| `src/providers/codearts/auth.rs` | 凭据解析、过期判断、STS refresh + ES256 DPoP（P-256，低 S） |
| `src/providers/codearts/client.rs` | 签名 HTTP 客户端、有界错误体、逐行 SSE 读取、超时/传输分类 |
| `src/providers/codearts/error.rs` | CodeArts 状态码/流内业务码分类（TM.00001041、APIG.0602、APIG.0301 等） |
| `src/providers/codearts/convert.rs` | 内部协议 ↔ v2 Chat 请求体、SSE 增量解析、非流式聚合 |
| `src/providers/codearts/models.rs` | `/v1/model/builtin` → `UpstreamModel` |
| `src/providers/codearts/quota.rs` | `/snap-manager/v1/statistics/plugin` → `QuotaSnapshot`（缺失为 Unknown） |
| `src/providers/codearts/mod.rs` | `CodeArtsProvider`：info/capabilities/auth_status/list_models/quota/chat/chat_stream |
| `src/providers/codearts/tests.rs` | mock-server 测试与解析单元测试 |
| `src/providers/codearts/fixtures/*` | 脱敏 fixture：models / quota / chat SSE / TM.00001041 |
| `src/providers/mod.rs` | `ProviderContext`、`CredentialStore`、`chat_with_context` / `chat_stream_with_context` |
| `src/account_service/store.rs` | `AccountCredentialStore`：把 `CredentialStore` 接到 M11 `CredentialService`（revision CAS） |
| `src/session_service/router.rs` | `SessionRouter`/`RouteLease`：绑定查找/创建、账号选择、凭据读取、失败恢复 |
| `src/checkin/codearts.rs` | `CodeArtsCheckinExecutor`：`/v1/ops/delivery` → `claim` → `confirm` |
| `src/checkin/executor.rs` | `RoutingCheckinExecutor`：按 `(kind, region)` 分派 |
| `src/api/mod.rs` | Chat 路由接入 `SessionRouter`；SSE 包装保持租约；错误触发恢复 |
| `src/api/catalog.rs` | `discover_with_accounts`：用第一个可用账号的凭据发现模型 |
| `src/runtime/mod.rs` | CodeArts 注册、CodeArts 签到执行器、`LiveQuotaRefresh`、账号感知的模型发现 |
| `src/test_support.rs` | 测试专用 HTTP/1.1 mock server（仅 `cfg(test)`） |
| `Cargo.toml` / `Cargo.lock` | 新增 `hmac 0.12`、`p256 0.13`（MIT/Apache-2.0） |

未修改 `0001`–`0004` 迁移；未新增缓存/凭据明文存储。

### 23.3 源码依据（只读参考）

| 事实 | 来源 |
| --- | --- |
| 聊天端点/请求体/`chat_id`/`tool_stream` | 本地 `~/codearts2api\app\codearts\client.py:73-111,304-375`；`.refs/RobbsLuo_Coding2API/src/provider/codearts/client.py:104-149` |
| 聊天请求头（`x-auth-token`、`app-id`、`x-snap-traceid`） | 本地 `client.py:56-82`；Coding2API `client.py:185-204` |
| 签名规则（尾 `/`、全头、payload hash） | 本地 `app/codearts/signer.py:33-150`；Coding2API `signer.py:56-154` |
| 模型端点与 `Agent-Type: PromptCenter` | 本地 `model_catalog.py:150`；Coding2API `client.py:287-291` |
| 额度端点/字段（`usageTotalPackageCredit`） | Coding2API `client.py:309-318,605-663`；本地 `subscription.py:96-103` |
| SSE 形状（OpenAI chunk、200 内嵌错误、`[DONE]`） | Coding2API `events.py:8-20,198-274`；本地 `sse.py:14-63` |
| `TM.00001041`（HTTP 400）与 `APIG.0602`/`APIG.0301` | Coding2API `events.py:84-115,355-397`；测试夹具 |
| OAuth refresh + DPoP（client_id、表单、低 S） | 本地 `auth.py:185-213,329-376`；Coding2API `auth.py:189-207`、`dpop.py:1-30` |
| 签到 `/v1/ops/*`、campaignId=1、ELIGIBLE/CLAIMED/CONFIRMED/CONSUMED | Coding2API `client.py:320-424`、`events.py:400-501` |
| Johnsheng1 原版无签名/会话实现 | `.refs/Johnsheng1_codearts2api/server.py`（仅 OpenAI 形状，不采用） |

本地 `codearts2api` 的“全局固定 32-hex 会话槽”**未复制**；本实现由会话绑定生成每会话
独立上游 ID，跨账号绝不复用。

### 23.4 实现能力

- **认证与签名**：AK/SK + `X-Security-Token`；`CanonicalURI` 每段编码且尾 `/`；
  全部发送头参与 `SignedHeaders`；`X-Sdk-Date`、`X-Sdk-Content-Sha256`。
- **凭据过期/刷新**：本地 `expiration` 先于网络判断；到期前 60s 触发刷新；refresh token +
  原 DPoP 私钥 + PKCE verifier；刷新按账号串行（tokio mutex），刷新结果经
  `CredentialService::update(expected_revision)` 持久化；CAS 冲突只重读一次，不二次消费
  一次性 refresh token。无刷新材料时返回 `AuthExpired`，不回退到匿名调用。
- **Chat**：强制上游 `stream=true`；非流式由本地聚合；多轮 system/developer/user/assistant/tool；
  Tool Calling（`tools`/`tool_choice`/历史 tool_calls/tool 结果）；`max_tokens`、temperature、
  top_p、stop；`Unsupported` 内容块显式拒绝，不静默丢弃。
- **SSE**：逐行解析（不依赖空行分帧）、增量 content/reasoning、tool 参数增量、usage
  （含 cached/reasoning details）、finish_reason、200 内嵌错误、`[DONE]`、EOF 兜底、
  单行上限、UTF-8 按行解码。
- **模型/额度**：只返回上游 `builtinModels`；额度按 `usageTotalPackageCredit`，缺失为
  `Unknown`（remaining/total 为 null），到期日与分项包写入 `raw`；参考实现明确
  `end_date` 是套餐到期日、不是逐笔积分到期/重置时刻，因此不把它标成 reset 时间。
- **会话墙**：`SessionRouter` 接入 Chat；账号级 in-flight 3 + 60s 启动窗口 + 会话串行；
  `TM.00001041` → `RateLimited`（`SessionFailure::AccountBusy`，不换号、不重试）；
  `SessionBindingInvalid` → 同账号 `reset_upstream`；认证类 → 持久化 `AuthStatus`。
- **签到**：`/v1/ops/delivery` 只读探测；`ELIGIBLE+claimable` 才写；`CLAIMED` 只补 confirm；
  `CONFIRMED/CONSUMED` 不写；业务码非 0 为 `Failed(Other)`；网络/5xx 返回 `Err`（M12 记为
  `Unknown`）。成功后由 `LiveQuotaRefresh` 调用 Provider `quota` 并持久化快照。
- **Runtime**：`kind = "codearts"` 可注册（`base_url` + `options.sts_url` 可覆盖）；无账号时
  不产生模型列表，也不伪装为可用；CodeArts 未启用时不注册其签到执行器。

### 23.5 与 Provider Trait 的关系

- 新增 `ProviderContext`（`account_id`、`credential_revision`、`client_session_id`、
  `upstream_session_id`）及 `chat_with_context` / `chat_stream_with_context` 默认方法；
  现有 Provider/Mock 无需改动。
- 上下文只用于网关内部：CodeArts 仅把 `upstream_session_id` 映射为上游 `chat_id` /
  `prompt_cache_key`；不转发账号、凭据或客户端会话 ID。
- `CredentialStore` 定义在 `providers`，实现放在 `account_service`，保持
  `providers` 不依赖 storage/account_service 的方向。

### 23.6 测试与验证

新增 58 个测试（210 → 268）：

- CodeArts Provider 39 个：注册/能力、认证成功/401、过期免请求、模型发现、额度与 Unknown、
  非流式聚合、SSE 增量、工具调用、多轮请求体、`chat_id` 上下文、TM.00001041、429/5xx、
  超时、错误体脱敏、刷新 CAS、CAS 冲突、签名/DPoP 单测、SSE 解析等。
- `SessionRouter` 6 个：粘性、跨账号不复用上游 ID、失效重置、认证失败持久化、无账号 503、
  lease drop 释放 permit（取消语义）。
- CodeArts 签到 9 个：claim→confirm、已签不写、CLAIMED 只 confirm、claim 503 非失败、
  业务拒绝为失败、成功后触发额度 Hook、campaign 解析、结束时间、业务码。
- 其他 4 个：`AccountCredentialStore` revision CAS、API session id、Runtime 注册与无账号
  不产生模型、端到端粘性会话（含真实 SSE、签名头、`chat_id` 复用/隔离）。

实际执行（Windows，`rustc 1.98.1`，`CARGO_HTTP_CHECK_REVOKE=false`）：

```text
$ cargo fmt --check
exit=0

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.17s
exit=0

$ cargo test
test result: ok. 268 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build
Finished `dev` profile [unoptimized + debuginfo] target(s) in 14.73s
exit=0
```

测试只访问 `127.0.0.1` 的 mock server；默认 `cargo test` 不访问真实上游。

### 23.7 真实上游验证状态

**0 项真实上游验证**。本里程碑没有使用任何真实 CodeArts 账号/凭据；以下均未实测：

- AK/SK + STS 签名是否能被华为 APIG 接受；
- refresh token/DPoP 能否成功换票、`expiration` 字段格式；
- `builtinModels` / `statistics/plugin` 的真实字段；
- `chat_id` 复用是否真的形成 prompt cache 亲和；
- 3 会话墙的具体计数键、滞留时间、`TM.00001041` 的触发边界；
- `/v1/ops/*` 签到协议与真实活动状态。

Provider `verification` 保持 `experimental`，能力矩阵不得写成 `verified`。

### 23.8 已知风险与未实现

1. **每会话上游 ID vs 3 会话上限**：超过 3 个活跃下游会话仍可能触发会话墙；当前用
   “3 in-flight + 3 starts/60s + 排队 75s” 缓解，没有做槽位复用（会牺牲亲和）。
2. **会话失效无独立错误码**：没有已证实的“会话过期”业务码；只有适配器显式返回
   `SessionBindingInvalid` 时才自动 `reset_upstream`。
3. **模型目录是启动快照**：账号在启动后新增/刷新不会自动重新发现；`/v1/models` 仍是启动时
   的结果（无管理 API）。M19 调度器阶段应改为按需刷新。
4. **刷新只在 Chat 路径触发**：`list_models`/`quota`/`auth_status` 不主动刷新；启动时凭据
   临近过期可能导致发现失败，需重新导入或先发一次 Chat。
5. **签到执行器不刷新凭据**：签到使用当前凭据；过期时按 `AuthExpired` 失败，不隐式换票。
6. **额度归属**：`Provider::quota` 无账号上下文，返回的 snapshot `account_id` 为空；
   `LiveQuotaRefresh` 在持久化前填入真实账号 ID。
7. **多实例**：账号槽/会话锁/刷新锁都是进程内状态；多实例合计会超过上游每账号限制。
8. **无上游会话删除**：本地 reset/invalidate 不释放上游名额，只能等待自然过期。
9. **无管理 HTTP/UI**：账号导入、绑定查询/失效/重置、签到触发仍未暴露；本里程碑用代码级
   服务与测试覆盖。
10. **签到真实状态未知**：`ELIGIBLE` 但不可领、活动不存在等分支按参考实现映射，未实测。

### 23.9 下一阶段入口（M13-C/M14）

1. 用操作者授权的一个真实 CodeArts 账号做最小验证：`auth_status` → `list_models` →
   `quota` → 非流式 Chat → 流式 Chat → 工具调用 → 连续 3/4 会话触发 `TM.00001041`。
2. 记录真实请求/响应（脱敏）到 `docs/CODEARTS_SESSION.md`，把能与不能证实的行为分开；
   只有通过真实端到端验证的能力才可把 `verification` 改为 `verified`。
3. 若真实响应字段与 fixture 不一致，先改适配器与 fixture，再更新本文档的“源码依据”。
4. 再进入 M14（签到）：在真实账号上验证 `/v1/ops/*` 的 ELIGIBLE/CLAIMED/CONFIRMED 与
   confirm 失败语义；确认前不把签到能力标为 verified。
5. Scheduler（M19）仍不在本阶段：`SessionRouter::select_account` 目前固定选第一个可用账号，
   只作为未来 Scheduler 的硬约束入口。

## 24. M13-C — CodeArts 真实联调验证（2026-10-29，Blocked）

### 24.1 目标与结果

使用一个明确授权的 CodeArts 测试账号验证 M13-B 适配器的真实兼容性，按风险顺序覆盖
认证、模型、额度、Chat、SSE、工具调用、会话粘性、签到（独立授权）与 Token 刷新。

**结果：阻塞关闭，真实上游请求 0 次，签到写操作 0 次。** 当前环境没有可供 PolyGate
使用的授权凭据，也没有把凭据安全导入 PolyGate 的入口。按 M13-C 要求停止真实请求，
未要求用户粘贴明文密钥，未解密任何外部保护存储，未伪造上游结果。

### 24.2 只读盘点（脱敏）

| 检查 | 结果 |
| --- | --- |
| `<repo>/data/` | 不存在：无 `config.toml`、无 `polygate.db`、无 `master.key` |
| 项目配置 | 仅 `config.example.toml`（示例无真实值） |
| 环境变量 | 未发现 `POLYGATE_*` / `CODEARTS_*` / `SNAP_*` / `STS_*`（未输出值） |
| CLI | 仅 `serve` / `doctor` / `config-example`；无账号导入或凭据设置命令 |
| 管理 HTTP API | 不存在（`config.example.toml` 注释提到的 admin API 尚未实现） |
| 本地 `codearts2api` 账号 | 2 条 CN 记录；密钥字段 `protected`，`protection=dpapi-current-user`，无顶层 AK/SK/STS/refresh 明文 |
| 账号元数据 | `expiration` 约 `2026-10-29T04:00:05Z`；检查时距到期约 65 分钟 |

结论：账号必须以 `SecretBox` 加密写入 PolyGate 的 SQLite，但当前没有 CLI/HTTP/配置
入口；外部 `codearts2api` 账号由 DPAPI 保护且应用不提供明文导出，不能直接复用。

### 24.3 能力矩阵

所有真实验证项（认证/签名、错误分类、模型发现、额度、非流式/多轮 Chat、SSE、取消、
工具调用、会话粘性/会话墙、刷新、签到）均为 **Blocked / Unverified**。现有最高证据仍为
Fixture Verified（mock + 脱敏 fixture），`Provider.verification` 保持 `experimental`。
详细矩阵见 `docs/CODEARTS_LIVE_VERIFICATION.md` §4。

### 24.4 安全边界

- 未在聊天、日志或报告中索取或写入 Token、Cookie、AK/SK、Refresh Token、私钥。
- 未读取/解密 `codearts2api` 的 DPAPI 内容；未绕过其保护。
- 未撤销、损坏或强制过期任何有效凭据。
- 未调用 `/v1/ops/claim` / `/v1/ops/confirm`；真实签到保留为独立显式授权测试。
- 未用 Mock/fixture 结果冒充真实响应。

### 24.5 变更与验证

| 文件 | 变更 |
| --- | --- |
| `docs/CODEARTS_LIVE_VERIFICATION.md` | 新增：环境、阻塞证据、能力矩阵、解除阻塞步骤、未验证清单 |
| `PROGRESS.md` | 更新状态、里程碑列表并新增 §24 |

未修改代码、迁移、配置或测试；未新增依赖。为确认基线未漂移，重跑全量测试
（Windows，`rustc 1.98.1`，`CARGO_HTTP_CHECK_REVOKE=false`）：

```text
$ cargo test --quiet
test result: ok. 268 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s
```

### 24.6 下一入口

1. 先实现**安全账号导入 + 最小管理面**（M14）：CLI 或受 admin token 保护的 HTTP 入口，
   凭据只经 `SecretBox` 入库，响应只返回账号 ID/掩码；`doctor` 增加账号存在性检查。
2. 由操作者通过本地文件/环境变量提供一个授权测试账号（不在聊天/日志中粘贴），确认
   CodeArts provider 已注册且模型发现非空。
3. 按 `docs/CODEARTS_LIVE_VERIFICATION.md` §8 的顺序重跑：认证 → 模型 → 额度 → 单轮 Chat
   → 多轮 → SSE/取消 → 工具 → 会话粘性/持久化 → 刷新；会话墙只做 3 并发最小观察。
4. 签到单独授权：先只读查询状态，仅在用户明确允许领取时调用 claim/confirm，不重复执行。
5. 在获得真实成功响应前，文档、UI 与 Provider 状态均不得标 `verified`；真实响应与
   fixture 不一致时，先保存最小脱敏复现，再改适配器并加回归测试。

## 25. M13-C1 — Web 管理台与原生账号接入（2026-10-29）

### 25.1 目标与边界

M13-C 的阻塞点是“没有正常、安全的账号接入入口”。本阶段按最终方向修正：**Web 管理台是账号
添加/授权/管理的正式入口**，CLI 只保留启动、诊断、初始化和必要的离线迁移。

实现：

- 嵌入式静态管理台（`web/`，`rust-embed`，无 Node 运行时、无 CDN）。
- 首次设置、密码登录、恢复码、会话 Cookie、CSRF、Origin/Host 校验、登录限速。
- CodeArts 浏览器授权（PKCE + P-256 DPoP），授权流程状态机与失败回滚。
- 账号列表/详情/改名/启停/删除/本地状态/实时探测/额度刷新/签到记录。
- Dashboard、模型、签到、用量只读视图，数据来自现有服务。

明确未做：CLI 账号 CRUD、旧 codearts2api/DPAPI 导入、跨机加密迁移包、完整多账号负载均衡、
其他真实 Provider、Anthropic/Responses 新协议、M13-C2。

### 25.2 新增/修改文件

| 文件 | 变更 |
| --- | --- |
| `src/admin/mod.rs` | 新增：资源嵌入、`AdminSettings`/`AdminState`、路由合并、安全响应头、32 KiB 请求体上限 |
| `src/admin/auth/mod.rs` | 新增：首次设置、登录、恢复码、会话、Cookie、CSRF、Origin/Host、失败限速 |
| `src/admin/auth/flow.rs` | 新增：`pending → exchanging → succeeded/failed/cancelled/expired` 流程状态机 |
| `src/admin/account/mod.rs` | 新增：按上游身份落库、凭据 CAS 更新、账号视图脱敏 |
| `src/admin/api/mod.rs` | 新增：Dashboard/账号/模型/签到/用量/授权端点、OAuth 回调结果页 |
| `src/admin/tests.rs` | 新增：21 个 Mock STS + 真实 Router 集成测试 |
| `src/providers/codearts/login.rs` | 新增：PKCE、DPoP、token 交换、`caller-identity` 身份查询 |
| `src/providers/codearts/auth.rs` | 增加 `DpopKey::generate()`；`from_json`/`proof` 调整为 `pub(crate)` |
| `src/providers/codearts/mod.rs` | 声明 `login` 模块 |
| `src/storage/admin.rs` | 新增 `AdminRepo`：管理员、会话、授权流程 |
| `src/storage/accounts.rs` | `external_id` 列/映射；`find_by_external` |
| `src/storage/mod.rs` | 挂载 `AdminRepo` |
| `src/accounts/mod.rs` | `Account`/`NewAccount` 增加 `external_id` |
| `src/account_service/service.rs` | 创建账号支持 `external_id` 去重；`find_by_external` |
| `src/api/catalog.rs` | 模型目录记录 `discovered_at` |
| `src/config/mod.rs` | 新增 `[admin]` 配置与校验 |
| `src/runtime/mod.rs` | 先绑定端口再构建管理台；合并 admin Router；`ConnectInfo<SocketAddr>` |
| `src/lib.rs` | 声明 `admin` 模块 |
| `migrations/0005_admin.sql` | 新增：`accounts.external_id` + 唯一索引、管理员/会话/授权流程表 |
| `src/storage/tests.rs` | 表清单与 schema version 断言更新到 5 |
| `web/*` | 新增：单页 HTML、CSS、原生 JS、OAuth 回调页 |
| `config.example.toml` | 新增 `[admin]` 段与 `server.public_url` 注释 |
| `docs/WEB_ADMIN.md` | 新增：设计、协议依据、API、安全、测试、未验证项 |
| `docs/ARCHITECTURE.md` | 更新模块布局、依赖、管理 API、安全、启动链路、迁移列表 |

新增依赖及许可证（均为宽松许可，兼容暂定 MIT）：

| 依赖 | 用途 | 许可证 |
| --- | --- | --- |
| `argon2` | 管理员密码哈希（Argon2id） | MIT / Apache-2.0 |
| `rust-embed` | 静态资源嵌入二进制 | MIT |
| `mime_guess` | 静态资源 Content-Type | MIT |
| `parking_lot` | 登录限速器内存锁 | MIT / Apache-2.0 |

`p256` 为 M13-B 已有依赖，本阶段只增加密钥生成能力，未引入新的加密原语。

### 25.3 移除的中间方案

按要求撤回此前“安全账号导入/CLI 管理”的半成品，避免与 Web 入口重复：

- 删除 `src/migration/`（codearts2api bundle 解析与 `MigrationService`）与
  `migrations/0005_account_migration.sql`。
- 删除 `Storage::import_accounts`、`AccountService::import_accounts` 及
  `ImportedAccount`/`ImportReport`/`ImportAction`/`ImportOutcome`。
- 撤回临时加入的 `hkdf` 依赖与 `SecretBox` 主密钥派生改动；未保留跨机迁移包。
- 保留 `external_id`，用于 Web 授权后的上游身份去重。

### 25.4 CodeArts 授权协议与来源

只读参考本地 `~/codearts2api`（操作者本人作品）：

| 事实 | 来源 |
| --- | --- |
| `/portal/authorize` 参数：`client_id`、`uri_scheme=codearts-snap`、`port`、`code_challenge`、`code_challenge_method=SHA-256`、`ticket_id`、`auth_callback_url`、`plugin-name`、`plugin-version` | `internal/auth/manager.go:160-190` |
| Intl 额外 `quickcompfalg=1` | `internal/auth/manager.go:186` |
| PKCE verifier 64 随机字节 hex；challenge = base64url(SHA-256) | `internal/auth/dpop.go:36` |
| 每流程新建 P-256 密钥，ES256 DPoP JWT | `internal/auth/dpop.go:46-80` |
| `POST {sts}/v1/oauth2/tokens`（form + `DPoP` 头） | `internal/auth/manager.go:196`、`internal/config/config.go:29` |
| `GET {sts}/v5/caller-identity`（SDK-HMAC-SHA256） | `internal/config/config.go:30` |
| 响应含 `access_key_id`/`secret_access_key`/`security_token`/`expiration`/`refresh_token` | 同上 |

实现取舍：**不复用**参考工程的全局 session-slot；回调只带 `code`，因此一个实例同时只允许一个
活动流程（唯一索引 + 条件 UPDATE），并发发起返回 409。流程秘密（PKCE verifier、DPoP 私钥 JWK）
用 `SecretBox` 加密，AAD 为 `polygate:admin-flow:v1:<flow_id>`。

### 25.5 管理 API

公开：`GET /admin/api/bootstrap`、`POST /admin/api/setup|login|recover`、`GET /oauth/callback`。

需会话（修改类另需 `X-PolyGate-CSRF`）：

```text
GET    /admin/api/session | logout(POST)
GET    /admin/api/dashboard | providers | models | checkins | usage
GET    /admin/api/accounts            POST /admin/api/accounts
GET    /admin/api/accounts/{id}       PATCH /admin/api/accounts/{id}
DELETE /admin/api/accounts/{id}?confirm=<id>
GET    /admin/api/accounts/{id}/auth-status   POST 同路径（真实探测）
GET    /admin/api/accounts/{id}/quota         POST 同路径（刷新）
GET    /admin/api/accounts/{id}/checkins
POST   /admin/api/auth/codearts/start
GET    /admin/api/auth/codearts/{flow_id}
POST   /admin/api/auth/codearts/{flow_id}/cancel
```

处理器不直接访问 SQLite；账号/凭据写入经 `AccountService`/`CredentialService`。只有 CodeArts
授权当前可用，其他 Provider 新建返回 501 并在 UI 标注“待接入”。额度未知保持 `unknown`/`null`。

### 25.6 安全模型

- 无默认密码；首次设置仅接受 loopback 对端 **且** loopback Host（防止同机反向代理接管）。
- Argon2id 密码；恢复码与会话 token 只存 SHA-256；恢复码使用后轮换并吊销全部会话。
- Cookie `polygate_admin`：`HttpOnly`、`SameSite=Strict`、`Path=/admin`，HTTPS 下 `Secure`。
- 修改类请求同时校验 Origin 与 CSRF；Origin 精确匹配，不使用通配符。
- 登录失败限速（默认 5 次 / 300 秒），超限 429 + `Retry-After`。
- `/v1` Gateway API Key 不授予管理权限（有测试）。
- CSP `default-src 'self'`、`frame-ancestors 'none'`、`X-Frame-Options: DENY`、`nosniff`、
  `Referrer-Policy: no-referrer`；管理页 `no-store`。
- 凭据只以 `SecretBox` 密文入库；任何管理 JSON/日志/前端存储都不含 AK/SK、STS token、
  refresh token、DPoP 私钥或 PKCE verifier。
- 账号与凭据创建在同一事务；唯一冲突整体回滚（有测试）。

### 25.7 测试与验证

2026-10-29 在 Windows `rustc 1.98.1`（`CARGO_HTTP_CHECK_REVOKE=false`）实测：

```text
$ cargo fmt --check
（无输出，退出码 0）

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.75s

$ cargo test --quiet
running 291 tests
test result: ok. 291 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.04s

$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 46.18s
```

`src/admin/tests.rs` 覆盖：首次设置/Cookie 属性、错误密码与限速、未认证与 API Key 隔离、CSRF、
loopback 与同机代理拒绝、授权成功/失败/取消/超时、重复授权去重、CN/Intl 隔离、账号 CRUD/启停/
删除确认、未知额度、空 Dashboard、无凭据不报已认证、唯一冲突回滚、恢复码轮换、Origin 不匹配、
Secure Cookie、嵌入 UI 与安全头、账号 JSON 无凭据、未实现 Provider 501。

手工验证（本地 `127.0.0.1:18887`，临时数据目录）：首次设置/登录、Dashboard、账号页、
添加账号模态框与取消、深浅主题切换、设置页。**未点击“打开授权页面”**，未发出真实上游请求；
验证后删除临时数据目录并停止服务器。

### 25.8 未验证项与已知问题

- **真实 CodeArts 登录未验证**：无授权账号，仅 Mock STS。门户外参数、token 响应字段、
  `caller-identity` 结构可能与 Mock 不同；真实跑通前不标 `verified`。
- 单实例内存限速器；多实例部署需要外部限速。
- 授权回调只带 `code`，因此单活动流程；若上游引入 `state`，应升级为多流程并发。
- 凭据刷新/重新授权使用修订号 CAS，冲突返回可重试错误，未做自动重试。
- 未实现审计日志页面、日志流、路由策略读写、凭据导入/导出。
- 忘记密码且丢失恢复码时，只能离线删除 `admin_users` 行后重新设置。
- 管理台只在 Mock 授权下验证；未验证真实浏览器在跨设备/HTTPS 反向代理下的回调行为。

### 25.9 下一入口（M13-C2）

使用真实授权账号完成：浏览器授权 → 账号入库 → 模型发现 → 状态探测 → 额度 → 单轮/多轮 Chat
→ SSE/取消 → 工具调用 → 会话粘性 → Token 刷新。签到仍是独立显式授权测试：先查状态，仅在
用户明确允许领取时调用 claim/confirm，不重复执行。真实成功前，文档与 UI 不得标 `verified`。

**M13-C1 到此停止，不自动进入 M13-C2。**

## 26. M13-C2 — 真实 CodeArts 登录与基础调用验证（2026-10-29，部分完成）

### 26.1 目标与结果

使用操作者本人通过 Web 管理台完成的真实 CodeArts CN 授权，验证 M13-C1 的授权链路与
M13-B 适配器。结果：**授权、token 交换、身份查询、模型发现、额度、认证探测、非流式 Chat、
SSE 均真实成功**。未使用任何明文粘贴凭据，未解密 DPAPI，未执行签到写操作。

### 26.2 真实授权方式

1. 本地启动 `polygate serve --data-dir data`，打开 `http://127.0.0.1:8787/admin/`，完成首次设置。
2. 管理台「账号 → 添加账号 → CN」生成 PKCE + P-256 DPoP 授权 URL。
3. 操作者在 Brave 中登录华为云并同意 CodeArts 授权；回调 `127.0.0.1:8787/oauth/callback`。
4. 后端用 `code` 交换 STS 凭据、查询 `caller-identity`，账号与加密凭据原子入库；
   账号 `auth_status=valid`，`external_id=codearts:cn:<principal_id>`。

### 26.3 根因与修复（首次授权失败）

首次回调返回“授权失败”。服务日志显示：

```text
error sending request for url (https://sts.cn-north-4.huaweicloud.com/v1/oauth2/tokens)
<- client error (Connect) <- dns error <- 不知道这样的主机。 (os error 11001)
```

根因：`AdminConfig::default()` 的 `codearts_sts_url` 误写为
`https://sts.cn-north-4.huaweicloud.com`，缺少 `my`。参考工程与 `config.example.toml`
均为 `https://sts.cn-north-4.myhuaweicloud.com`。

修复：
- `src/config/mod.rs` 改正默认值；
- 新增回归测试 `admin_defaults_use_real_huawei_endpoints`；
- 授权传输错误日志保留阶段与错误链（不含凭据）。

修复后同一浏览器授权流程成功。

### 26.4 真实验证结果

| 能力 | 结果 | 证据 |
| --- | --- | --- |
| 浏览器授权/token 交换 | ✅ | 回调成功，账号入库，`auth_status=valid` |
| `caller-identity` | ✅ | 返回 `principal_id`/`domain_id`，用于 `external_id` |
| 认证探测 | ✅ | `POST .../auth-status {"probe":true}` → `valid, persisted=true` |
| 模型发现 | ✅ | `/v1/models` 返回 GLM-5.2、Qwen3-VL-235B、deepseek-v4.1-flash、kimi-k3 等真实模型 |
| 额度 | ✅ | `credits`，总额 15700，剩余 15136.91，`status=known`，含 3 个 package |
| 非流式 Chat | ✅ | `deepseek-v4.1-flash` HTTP 200，约 0.77s，`usage` 正常 |
| SSE | ✅ | 收到 `reasoning_content` 与 `content` 增量 chunk |
| 无效 code 错误分类 | ✅ | STS 返回 `STS5.1805`，映射为 `authorization was rejected` |
| Tool Calling（非流式） | ✅ | `finish_reason=tool_calls`，`get_weather` 参数正确 |
| Tool Calling（流式） | ✅ | 收到 id/name 与参数增量，最终 `finish_reason=tool_calls` |
| 多轮工具结果 | ✅ | assistant tool_calls + `role=tool` 回传，模型返回最终答案 |
| 会话粘性 | ✅ | 同一 `x-session-id` 复用同一 upstream `chat_id`，request_count 递增 |
| 会话墙 | ⚠️ 意外观察 | 第 4 个并发会话触发 `TM.00001041`，正确映射 429；未验证恢复 |
| Token 刷新 | ⏳ | 未等到自然过期，未破坏有效凭据 |
| 签到 | ⏳ | 未查询、未 claim/confirm |
| CodeArts Intl | ⏳ | 未验证 |

Provider 仍标记 `experimental`，但按能力拆分状态：

| 能力 | 状态 |
| --- | --- |
| authentication / model_discovery / quota / chat / streaming | Live Verified |
| tools / multi_turn / session_affinity | Live Verified |
| token_refresh / checkin / intl / session_wall_recovery | Unverified |

### 26.4.1 多轮工具调用的真实修复

首次多轮（assistant `tool_calls` + `role=tool`）返回上游
`inferhub.modelarts.81001.400 invalid argument`。根因：`convert.rs` 把内部 `ToolCall`
（`id/name/arguments`）直接序列化，缺少 OpenAI 的 `type:"function"` 与 `function` 包装。

修复：新增 `tool_call_to_upstream`，输出
`{"id":...,"type":"function","function":{"name":...,"arguments":...}}`；加强
`tool_history_roundtrips` 断言。修复后多轮与流式工具调用均成功。

### 26.5 测试与验证

```text
$ cargo fmt --check
（无输出，退出码 0）

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.89s

$ cargo test --quiet
test result: ok. 292 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.97s
```

本地运行配置在 `data/config.toml`（已 gitignore）：监听 `127.0.0.1:8787`，启用 Mock 与
`codearts` CN Provider；Gateway API Key 仅保存在本地 `data/`，未写入仓库。

### 26.6 未验证项与下一入口

1. Token 自然过期后的 refresh + DPoP + PKCE + CAS；不主动撤销/损坏凭据。当前凭据尚未临近过期。
2. 签到：先只读查询状态，仅在明确允许领取时调用 claim/confirm，不重复执行。
3. 会话墙释放后的恢复观察（本次仅意外触发一次 429，不主动压测）。
4. CodeArts Intl 与多账号隔离/负载均衡。

### 26.7 安全边界

- 未要求或记录任何 Token/Cookie/AK/SK/Refresh Token/私钥；文档只保留脱敏结构。
- 未读取或解密 `codearts2api` 的 DPAPI 内容。
- 未执行签到写操作；未撤销或强制过期有效凭据。
- 未用 Mock 结果替代真实响应；未验证项保持 `Unverified`。

## 27. M13-D1 — Qoder Provider 研究与接入设计（2026-10-29，研究完成）

### 27.1 范围与结果

本阶段只做研究与设计，**未修改任何 Rust 代码、未新增依赖、未触达 Qoder 上游**。

- 新增 `docs/QODER_PROVIDER_RESEARCH.md`：12 节，覆盖参考来源与证据等级、
  PolyGate 复用点、CN/Intl 隔离、设备码 PKCE 认证、Provider 分类与能力矩阵、
  Chat/SSE/多轮/错误、Tool Calling、Agent 工作流、模型与额度、签到、会话/并发、
  Web 管理台接入与 M13-D2 拆分。
- 更新 `docs/MIGRATION_MATRIX.md`：Qoder 行与 M17 退出条件。
- 更新 `docs/PROVIDER_MIGRATION.md` §2.2：补充认证、区域、COSY、SSE、额度、
  签到、会话的源码路径与结论。
- 本文件：记录阶段结论、证据与下一步。

### 27.2 主要结论

| 结论 | 证据等级 |
| --- | --- |
| Qoder 是 Model Provider（OpenAI chat + tools），不是 Agent Provider | Source Evidenced |
| 认证为设备码 + PKCE(S256) 轮询，202/404 为等待，600s TTL，无 redirect_uri | Source Evidenced |
| CN/Intl 的 openapi/gateway/website/domain/UA/nonce 完全独立，必须按 Region 隔离 | Source Evidenced |
| 推理与模型列表必须使用 COSY 签名（自定义 Base64 + AES + RSA + md5） | Source Evidenced |
| 聊天只支持流式；非流式由网关聚合 SSE | Source Evidenced / 设计决定 |
| 工具调用支持 `tool_calls` 与旧式 `function_call`；多轮需 null content 归一、丢弃孤立 tool | Source Evidenced |
| 模型列表返回 `key/display_name/price_factor/max_input_tokens/is_vl/supportsToolCall/is_reasoning` | Source Evidenced |
| 额度为 `userQuota + addOnQuota` 的 credits；字段缺失必须保持 unknown | Source Evidenced |
| 签到为活动制 `/me/campaigns` + `/{id}/claim`，必须 `Cosy-ClientType: 10`；旧接口仅回退 | Source Evidenced |
| 无 upstream session id、无任务 API、无取消 API | Source Evidenced / Unsupported |
| 真实登录、模型、额度、聊天、工具、签到均未执行 | Unverified |

### 27.3 证据与参考

- 参考实现：`.refs/RobbsLuo_Coding2API`，提交
  `df9936240708f99f5a1cd92e52330b4b2016c840`（MIT）。
  关键文件：`src/provider/qoder/{events,auth,client,cosy,credential}.py`；
  测试：`tests/provider/qoder/`。
- 本地官方客户端：`~/.qoder-cn/` 存在（版本观测 1.1.71），只确认了
  `.auth/machine_id`、`.auth/user`、`.models/` 等文件名，未读取凭据内容。
- 参考作者的“实测”注释只作为线索，未升级为 PolyGate 的 `Live Verified`。

### 27.4 PolyGate 复用判断

| 需求 | 复用 | 新增 |
| --- | --- | --- |
| Provider 接入 | `Provider` trait、`Capabilities`、`VerificationState`、Registry | `src/providers/qoder/*` |
| 账号与凭据 | `AccountService`、`CredentialService`、`SecretBox` | `persist_qoder_login` |
| 管理台授权 | Admin 鉴权/CSRF、`AuthFlowRow`、flow 状态机 | Qoder start/status/cancel 方法 |
| 签到 | `CheckinExecutor`、`RoutingCheckinExecutor`、`CheckinService` | `QoderCheckinExecutor` |
| 区域隔离 | `ProviderKind::Qoder` + `Region` 已存在 | Qoder realm 配置 |

不需要新表、不需要新调度器、不需要改 M12/M13-C1 结构。唯一已知限制：
`admin_auth_flows` 全局只允许一个 pending 流程，CodeArts 与 Qoder 不能同时授权。

### 27.5 阻塞项（M13-D2 前必须确认）

1. **COSY 客户端身份字段的合规口径**：参考协议要求
   `cosy-clientip`、UA、`cosy-version`、`login-version` 等官方客户端字段。
   `AGENTS.md` §2 禁止“客户端身份冒充”。本项目设计为只使用协议必需固定字段
   与 uid 派生标识，不读取官方客户端 machine_id、不随机生成设备指纹；但该解释
   需要项目所有者明确确认。未确认前不实现 Qoder 网络请求。
2. **授权账号**：真实登录/模型/额度/聊天/工具/签到验证需要用户提供一次
   设备授权；不得在聊天/日志/文档中粘贴 token。

### 27.6 下一步（未执行）

M13-D2 按 `docs/QODER_PROVIDER_RESEARCH.md` §12.4 拆分：
D2-0 政策确认 → D2-A COSY signer → D2-B 认证/管理台 → D2-C 模型/额度 →
D2-D Chat/SSE/tools → D2-E 会话/并发 → D2-F 签到 → D2-G 真实验收。

**本阶段到此停止，未实现 M13-D2。**

### 27.7 验证说明

本阶段没有 Rust 代码变更，未运行 `cargo build/test/clippy/fmt`。文档中的
源码路径与行号已逐一核对；PolyGate 侧行号以当前工作区为准。所有 Qoder 能力
状态保持 `Unverified`，Provider 尚未实现。

## 28. M13-D2-A — Qoder Protocol Foundation（2026-10-29，完成）

### 28.1 范围与结果

本阶段只实现 Qoder 协议基础层：区域配置、COSY 签名、错误分类、Chat/Tool 转换、
SSE 解析与非流式聚合。**未实现 `Provider`、未注册 Registry、未建立 HTTP 连接、
未发送任何 Qoder 请求、未使用任何凭据。**

新增文件：

| 文件 | 内容 |
| --- | --- |
| `src/providers/qoder/mod.rs` | 模块边界与范围声明 |
| `src/providers/qoder/region.rs` | CN/Intl realm、端点、nonce、网关候选 |
| `src/providers/qoder/cosy.rs` | 自定义 Base64、AES-CBC、RSA 包裹、设备派生、签名与头族 |
| `src/providers/qoder/error.rs` | HTTP/SSE/JSON/设备轮询错误分类 |
| `src/providers/qoder/convert.rs` | Chat 请求体、工具/多轮、非流式聚合 |
| `src/providers/qoder/events.rs` | SSE 帧状态机、信封、内层 chunk、流翻译器 |
| `src/providers/qoder/tests.rs` | 离线集成测试 |
| `src/providers/qoder/fixtures/` | SSE 与错误夹具（脱敏，无凭据） |

`src/providers/mod.rs` 只增加 `pub mod qoder;`；未改动 CodeArts、账号、存储、
管理台、签到或运行时。

### 28.2 区域隔离

- `region::realm(Region)` 对 `Cn`/`Intl` 穷尽匹配，**没有默认回退**。
- CN：`openapi.qoder.com.cn` / `gateway.qoder.com.cn` / `qoder.cn` / `qoder.com.cn`。
- Intl：`openapi.qoder.sh` / `api1.qoder.sh`（api2/api3 回退）/ `qoder.com` / `qoder.com`。
- 两区 client_id 相同（源码事实），但账号、token、端点、额度与签到按区域独立。
- Nonce：CN 带横线 UUID，Intl 无横线 hex。
- 端点常量全部来自 `events.py`；`PLAN_PATH` 标注为“已定义未解析/未验证”。
- 方法：模型 GET、聊天 POST。方法不参与签名（源码事实），已用常量固定。

### 28.3 COSY 签名

实现与参考源码逐项对应：

- `qoder_encode`：标准 Base64 → 尾/中/首旋转 → 自定义字母表 → `=` 换 `$`；
  `qoder_decode` 为逆运算。
- `json_sorted_compact`：键排序、无空白、缺失值为空串（不使用 `null`）。
- `info`：身份 JSON 的 AES-128-CBC 密文（key == iv，PKCS7）。
- `cosy_key`：16 字节临时密钥的 RSA PKCS#1 v1.5 包裹（内嵌公钥）。
- `Authorization: Bearer COSY.<payload>.<md5>`；签名串为
  `payload \n cosy_key \n date \n encoded_body \n path`。
- `path` 去掉 `/algo`，query 不参与；`date` 为 Unix 秒字符串。
- 头族名称、顺序、`cosy-clienttype: 5`、`cosy-version`、`login-version`、
  `x-model-key`/`x-model-source` 与参考一致。
- `Debug` 对 token、identity、签名头脱敏；不打印签名材料。

固定向量（Python/参考算法交叉生成，已写入单测）：

| 项 | 值 |
| --- | --- |
| `qoder_encode(b"{\"a\":1}")` | `ep$$BMn%mYKi` |
| `derive_id("u1","machine")` | `1c77b9ac7fdc6e92cc94595f68a05717` |
| `derive_machine_type("u1")` | `3c4f18a9288004989d` |
| `cosy_signature("p","k","1700000000","BODY","/api/v2/service/pro/sse/agent_chat_generation")` | `8b28c0f19e7491258363d76110c86a8c` |
| `info`（固定身份 + 固定 temp key） | 单测中的固定 base64，与 Python `cryptography` 输出一致 |

RSA 包裹用测试密钥对验证可解密；生产公钥为协议常量，不是秘密。

### 28.4 错误、转换与 SSE

错误分类：

- 402 / `Billing daily count exceeded` → `QuotaExhausted`（不封号）。
- `isQueued` / `RESOURCE_LIMIT`（含嵌套 JSON 字符串）→ `RateLimited`，带 `retry_after`。
- 401 → `AuthInvalid`；403 + `TOKEN_EXPIRE` → `AuthExpired`；
  403 + `12153`/`Offline user session not found` → `AuthInvalid`；
  403 明确权限标记 → `ChannelRestricted`；其他 403 → `UpstreamServer`（不误封）。
- 429 → `RateLimited`；408/504 → `Timeout`；418/5xx → `UpstreamServer`。
- 400/404/422：节点故障 → `UpstreamServer`；模型缺失标记 → `ModelUnavailable`；
  其余 → `BadRequest`。
- 设备授权 `202/404` → `Pending`，`200` → `Ready`，其他 → 分类后的失败；
  不把等待态当推理错误。
- HTTP、SSE 信封、内层 payload、传输错误分别有入口；正文脱敏并截断。

Chat/Tool 转换：

- `stream` 恒为 `true`，`agent_id=agent_common`。
- `max_tokens` 放入 `parameters`；temperature/top_p/stop/tools/tool_choice 按上游形状输出。
- assistant 历史 tool_calls 输出 OpenAI `type:"function"` 形状；
  `content` 绝不输出 `null`（空串替代）。
- 孤立 `role:"tool"` 丢弃；连续 assistant 的声明取并集；连续 tool 共用声明集合。
- 多个 ToolResult 拆成多条 tool 消息。
- `developer` 归一为 `system`；reasoning 作为 assistant `reasoning_content` 保留；
  不可表示的 content block 返回 `Unsupported`，不静默丢弃。
- `ToolResult::is_error` 无上游对应字段，content 原样转发，不伪造字段。

SSE/聚合：

- 增量字节状态机，支持跨块 UTF-8、半帧、多帧同块、CRLF、注释、末行无换行。
- 信封 `statusCodeValue`（数字/字符串）+ 内层 `body` 字符串；`null` 保活帧跳过。
- 内层解析 content/reasoning/tool_calls/function_call/usage/finish_reason；
  参数片段保持原始字符串，不提前解析 JSON。
- 工具调用按 index 合并；缺 index 时用数组位置；参数早于 name 时缓冲。
- `[DONE]` 或 `finish_reason` 正常结束；EOF 两者都没有 → 终止 `Error`，不伪造成功。
- `aggregate` 将事件聚合为 `ChatResponse`；遇到 `Error` 事件返回 `Err`。

### 28.5 验证

```text
$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.12s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.51s

$ cargo test --quiet
test result: ok. 340 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo fmt --all --check
（无输出，退出码 0）
```

Qoder 模块 48 个测试全部离线运行，无 socket、无真实凭据。新增依赖：
`aes`、`cbc`、`md-5`、`rsa`（均为 MIT/Apache-2.0 兼容许可证，已锁定）。

### 28.6 未验证与下一入口

- 所有 COSY 结果仍为 `Source Evidenced` / 固定向量验证，**没有 Qoder `Live Verified`**。
- 未确认：官方 1.1.71 是否接受参考 1.1.64 的头族常量、内嵌 RSA 公钥是否轮换、
  上游是否接受 assistant `reasoning_content` 回传、工具参数对象形状、Intl 差异。
- D2-A 不包含：设备码轮询、token 刷新、模型/额度请求、HTTP 客户端、Provider 实现、
  Registry 注册、管理台流程、签到。
- 下一步为 M13-D2-B（认证与管理台流程），**本阶段不启动**。

## 29. M13-D2-B — Qoder 设备码授权与管理台流程（2026-10-29，完成，CN 真实授权已验证）

### 29.1 范围与结果

本阶段实现 Qoder 的设备码轮询授权、管理员授权流程、加密凭据保存、账号落库与
单写者刷新锁，并接入 Web 管理台。**未实现 `Provider`、未注册 Registry。**
实现阶段只用 Mock 服务器测试；2026-10-29 06:19–06:21 用真实 CN 账号完成了一次
设备码授权（见 §29.6），未调用模型、Chat 或额度接口。

新增文件：

| 文件 | 内容 |
| --- | --- |
| `src/providers/qoder/auth.rs` | 设备授权 URL、轮询、userinfo、刷新、单写者锁 |
| `src/providers/qoder/credential.rs` | `QoderCredential`、过期时间推导、合并刷新结果 |
| `src/admin/auth/qoder.rs` | 流程创建/查询/取消、秘密解密、终态判定 |

修改：

| 文件 | 内容 |
| --- | --- |
| `src/admin/mod.rs` | `AdminState` 增加 `qoder_auth` 与 `qoder_flow_locks`；新增三条路由 |
| `src/admin/auth/flow.rs` | `view` / `flow_aad` 提升为 `pub(super)` 供 Qoder 复用 |
| `src/admin/account/mod.rs` | `persist_qoder_login`：`external_id = qoder:<region>:<uid>`，CAS 保存凭据 |
| `src/admin/api/mod.rs` | `start_qoder_auth` / `qoder_flow_status` / `cancel_qoder_flow` |
| `web/index.html` | Qoder 子导航启用 |
| `web/app.js` | Provider 选择、动态授权端点/文案、Qoder 详情不显示签到与额度刷新 |

### 29.2 授权流程

- `start` 只构造 URL：`{website}/device/selectAccounts?challenge=&challenge_method=S256&nonce=&client_id=&machine_id=`；
  CN 额外带 `redirect_uri`，**不发任何上游请求**。
- PKCE verifier、nonce、machine_id 序列化后经 `SecretBox` 以
  `polygate:admin-flow:v1:<flow_id>` 为 AAD 加密存入 `AuthFlowRow`，浏览器永远拿不到。
- `redirect_uri` 列写空串（设备码无回调），`ticket_id` 存 nonce。
- 同一时间只允许一个活动流程（复用既有唯一索引与 `active_flow` 检查）。
- 状态查询不是纯读：pending 时最多发起一次上游轮询，按 `flow_id` 串行；
  拿到锁后重新读取流程，避免并发重复轮询。
- `202`/`404` → 保持 pending；`200` 且带 token → 拉 userinfo 并落库；
  `200` 但 token 为空 → 仍 pending（授权完成但令牌未就绪，不伪造成功）。
- 终态错误（鉴权、权限、请求形状）→ 流程 `failed`；瞬时错误
  （`Timeout`/`Transport`/`UpstreamServer`/`RateLimited`/`NoCapacity`）→ 保持
  pending，界面可继续重试。
- 取消只在本地把流程标记为 `cancelled`，不调用上游（Qoder 无撤销端点）。
- 轮询请求带 `Go-http-client/2.0` UA、20 s 超时、128 KiB 正文上限；
  错误正文先脱敏再截断。

### 29.3 凭据与账号

- `QoderCredential` 保存 access/refresh token、uid、nickname、realm、expires_at、
  auth_source、user_type、organization_id / organization_name、domain。
- 令牌字段 `Debug` 脱敏，`Drop` 清零；`to_json` 才输出明文，只交给 `SecretBox`。
- 过期时间：`expires_in`（毫秒）优先，其次 `expires_at`（RFC3339），再次 JWT `exp`；
  都没有则记 0（未知），`needs_refresh` 对未知不主动刷新。
- 刷新只发送 `{"refresh_token": ...}`，不带 Authorization；响应必须带新 access token，
  失败按错误分类返回，不覆盖旧凭据。缺少 refresh token 在联网前即拒绝。
- `QoderRefreshLocks` 为每账号一把异步互斥锁；调用方取锁后必须重新读取凭据并
  再判断 `needs_refresh`（已在文档注释中写明），保证单写者。
- 登录落库按 `(qoder, region, qoder:<region>:<uid>)` 查重：已存在则 CAS 更新凭据
  （revision 冲突走 `update`），并记录 `Valid`；`metadata` 只写非敏感字段。
- CN 与 Intl 账号各自独立：区域决定端点，跨区不共享 token，无自动回退。

### 29.4 Web 管理台

- 子导航 CodeArts/Qoder 均可点击，切换后账号列表按 `kind` 与 `region` 过滤。
- 授权弹窗文案、端点、成功提示按 Provider 动态生成；Qoder 提示 Intl 尚未实测。
- Qoder 账号详情只展示昵称、账号类型、组织、授权来源等非敏感元数据，
  不显示“检查认证 / 刷新额度”（Provider 未注册，避免暗示未具备的能力）。
- 签到列对 Qoder 显示“不支持”，不伪造签到记录。

### 29.5 验证

```text
$ cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.76s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.39s

$ cargo test --quiet
test result: ok. 359 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo fmt --all --check
（无输出，退出码 0）
```

Qoder 相关 67 个测试（协议基础 + 授权 + 管理台）全部使用 Mock 服务器，无真实
socket 出网、无真实凭据。

浏览器实测（`127.0.0.1:8799` 临时实例、临时数据目录，已删除）：首次初始化、
CodeArts/Qoder 子导航切换、Qoder 空态、添加账号弹窗文案与 Intl 提示、
弹窗取消均正常；控制台除一次故意的弱口令 400 外无报错。
**未点击“打开授权页面”**，避免向 Qoder 官方站点发起真实请求。

### 29.6 Qoder CN 真实授权验证（2026-10-29）

操作者在管理台完成了 Qoder CN 的设备码授权，结果如下（均为本地观察）：

| 项 | 观察结果 | 证据等级 |
| --- | --- | --- |
| 设备码授权 URL 打开与官方页面确认 | 操作者在 `qoder.cn/device/selectAccounts` 完成登录确认 | Live Verified |
| 轮询 `GET /api/v1/deviceToken/poll` | 首次状态查询返回成功并下发 token（此前为 pending） | Live Verified |
| `GET /api/v1/userinfo` | 返回 uid / 昵称 / user_type / 组织字段 | Live Verified |
| 账号落库 | `external_id=qoder:cn:01a11105…`，`auth_status=valid`，`metadata` 含 `user_type=personal_professional_trial`、`domain=qoder.com.cn` | Live Verified |
| 凭据加密 | `credentials` 行 483 字节密文，`encryption_version=v1`，revision 0 | Live Verified |
| 日志泄漏检查 | `data/serve.log` 中 `access_token`/`refresh_token`/`Bearer` 命中数为 0 | Live Verified |

本次授权期间另观察到：同一时间第二个授权请求被单活动流程保护拒绝
（`another authorization is already in progress`），这是预期行为，不影响已完成的流程。

仍未验证：token 刷新（未主动过期，不破坏有效凭据）、`expires_in` 真实单位、
Intl 区域授权、额度/模型/Chat。

### 29.7 未验证与下一入口

- 未实现：Qoder `Provider`、Registry 注册、模型/额度/聊天/SSE 生产链路、签到。
- 未验证：Intl 授权、token 刷新行为、过期时间真实语义、多账号。
- 下一步为 M13-D2-C（Qoder Provider 与运行时接入），**本阶段不启动**。

## 30. M13-D2-C — Qoder Provider：额度与签到（2026-10-29，部分完成）

### 30.1 范围与结果

新增 Qoder Provider（认证/额度）与签到执行器，并接入运行时与管理台。**模型发现、
Chat、SSE、工具调用未实现**，能力位显式 `false`，因此不会被路由选中，也不会出现在
`/v1/models`。

| 文件 | 内容 |
| --- | --- |
| `src/providers/qoder/http.rs` | OpenAPI Bearer 客户端：参考实现的头族（`X-Machine-ID`/`X-Session-ID` 由 uid 派生）、128 KiB 上限、20 s 超时、错误分类 |
| `src/providers/qoder/quota.rs` | `/api/v2/quota/usage` 解析：基础 + 附加额度求和；缺字段保持 `unknown`/`null` |
| `src/providers/qoder/provider.rs` | `QoderProvider`：`auth_status`（本地过期 + userinfo 探测）、`quota`；其余能力不声明 |
| `src/providers/qoder/credential.rs` | `from_credential`：从已解密凭据还原，realm 只取账号行 |
| `src/checkin/qoder.rs` | `QoderCheckinExecutor`：campaign 查询、`CLAIM_BENEFIT` 过滤、claim 开关、状态映射 |
| `src/runtime/mod.rs` | 注册 `kind="qoder"` Provider 与签到执行器；`checkin.claim_enabled` 选项 |
| `src/admin/api/mod.rs` | `POST /admin/api/accounts/{id}/checkin`（CSRF + Origin） |
| `web/app.js` | 账号详情新增“执行签到”；Qoder 签到列改为可用 |

### 30.2 设计要点

- 额度：`userQuota` 必需；`addOnQuota` 缺失视为无附加包（0），存在但字段非法 → `unknown`；
  任何关键字段缺失都不写 0。
- 签名：本阶段只调用纯 Bearer 的 OpenAPI 端点（userinfo/quota/campaigns），未发 COSY 请求。
- 签到：必须带 `Cosy-ClientType: 10`；只认 `actionType=CLAIM_BENEFIT`；
  `CLAIMED`/`BLOCKED+SAME_PERSON_ALREADY_CLAIMED`/`replayed:true` 归一为“今日已签”；
  其他 `BLOCKED` 为上游失败；404/405/410 → `Unsupported`（settled，不重试）。
- claim 默认关闭；关闭时返回明确的失败说明而不是静默领取。单账号每日最多执行一次
  （`CheckinService` 的持久化 claim），重复点击返回 `replayed=true`。

### 30.3 验证

```text
$ cargo build            Finished `dev` profile ...
$ cargo clippy --all-targets -- -D warnings   Finished ... (0 warnings)
$ cargo test             test result: ok. 380 passed; 0 failed
$ cargo fmt --all --check （无输出）
```

### 30.4 真实账号观察（2026-10-29，CN 账号 `edufx`）

| 请求 | 观察 |
| --- | --- |
| `POST /admin/api/accounts/{id}/quota` | `{"unit":"credits","total":100.0,"remaining":100.0,"status":"known"}`；上游 `userType=personal_standard`，`userQuota` 全 0、`addOnQuota` 100，`expiresAt` 为 9999 年 |
| `POST /admin/api/accounts/{id}/checkin`（claim 关闭时） | 返回 `failed` + “Qoder reward claim is disabled…”，证明 campaign 查询成功且当日存在可领活动；**未发送 claim** |
| `POST /admin/api/accounts/{id}/checkin`（claim 开启，操作者要求执行） | `succeeded`：“checked in; Qoder granted 100 credits”；同一次运行触发的额度刷新把余额从 100 变为 **200/200 credits**（2026-10-29 06:46:42Z） |

真实 claim 只执行了一次（attempt 2），没有重复请求。Intl 查询与 token 刷新仍未执行。

排查记录：中途一次 `Stop-ScheduledTask` 没有结束子进程，网关仍运行旧配置（`claim_enabled=false`）；
改为结束进程后重启，进程启动时间晚于配置修改时间，随后 claim 成功。

### 30.5 未验证与下一入口

- 未实现：模型发现（COSY GET）、Chat/SSE/工具、额度跨区/按模型分组。
- 未验证：真实 claim 结果与奖励到账、签到窗口（10:00 UTC+8）行为、Intl 可用性、
  token 刷新。
- 下一步：Qoder 模型发现与 Chat/SSE（D2-C 续 / D2-D），**本阶段不启动**。
### 30.6 补充：检查认证会刷新过期凭据（2026-10-29）

- `Provider` 新增 `auth_status_with_context`；CodeArts 覆盖它，先走 `ensure_fresh`
  （单写者锁 + revision CAS 持久化）再探测。管理台“检查认证”改为传
  `ProviderContext`，过期账号不再只能重登。
- 单测：过期 + 有 refresh 材料 → 刷新一次并 CAS 写回（revision 3→4）；
  过期但无 refresh 材料 → 返回 `Expired` 而不是传输错误。
- 真实账号（重新授权后的 `hsggga`）：探测返回 `valid`；该凭据未实现过期，
  因此本次没有触发真实刷新。
- CodeArts 签到实测：`/v1/ops/delivery` 中没有 `campaignId=1` 的日常活动，
  执行器按既有规则记 `already_checked_in`（“activity is not available”），未发送
  claim；与“网页已签到”不冲突，也不产生副作用。


## 31. M13-D2-C — Qoder 推理 Adapter：模型发现、Chat/SSE 与工具（2026-10-29，完成，CN 真实联调）

### 31.1 范围与结果

在 §30 的认证/额度/签到之上补齐 COSY 签名的推理面，`QoderProvider` 能力位改为
`authentication / model_discovery / quota / chat / streaming / tools = true`，
`native_session / agent_task / cancellation = false`。模型发现与 Chat 均已在 CN
真实账号上端到端跑通。

| 文件 | 内容 |
| --- | --- |
| `src/providers/qoder/gateway.rs`（新增） | COSY 签名的模型列表（GET）与 `agent_chat_generation`（POST）；单 realm 内仅在传输层失败时切换备用主机；模型字段解析（`key`/`display_name`/`max_input_tokens`，`enable=false` 跳过，空列表视为发现失败） |
| `src/providers/qoder/cosy.rs`（重写签名层） | 与真实上游一致的 COSY 方案：固定顺序的身份 JSON 与 payload 信封、16 字符临时密钥、RSA PKCS#1 v1.5、`md5(payload\nkey\ndate\nbody\npath)`；只发 4 个签名头 + 固定 UA |
| `src/providers/qoder/provider.rs` | 实现 `list_models` / `chat` / `chat_stream`；非流式由网关聚合同一条上游流（`convert::aggregate`），不发明第二个上游端点 |
| `src/providers/qoder/events.rs` | 额外跳过官方客户端同样丢弃的 `[NOTIFICATIONS]#` 遥测帧 |
| `src/providers/qoder/tests/inference.rs`（新增） | 9 个 Mock 服务器测试：签名复算、模型解析、非流式聚合、流式翻译、402 映射、截断判定 |

### 31.2 协议修正（重要）

研究阶段采用的 Coding2API `df99362…`（1.1.64）签名形状在真实 CN 网关上被拒绝：

```text
GET /algo/api/v2/model/list?Encode=1
403 {"code":"101","message":"Signature invalid"}
```

逐一比对后，真实可用方案（与本机官方 Electron 客户端 `lXt`/`iqe` 一致，已实测）：

| 项 | 可用方案 | 被拒绝的旧方案 |
| --- | --- | --- |
| 身份 JSON | `{"uid","aid":"","name","email":"","security_oauth_token"}`，固定顺序 | 键排序，含 `refresh_token`/`organization_*`/`user_type`/`yx_uid` |
| payload | `{"version":"v1","requestId","info","cosyVersion","ideVersion":""}`，固定顺序，`requestId` 为无横线 uuid | 键排序 |
| 临时 AES 密钥 | **16 个十六进制字符**（uuid 去横线截 16） | 任意 16 字节（随机二进制亦被拒） |
| 请求头 | `Cosy-User`/`Cosy-Key`/`Cosy-Date`/`Authorization` + `User-Agent` | 另含 `Cosy-ClientType`/`Cosy-Machine*` 等客户端身份头 |

请求头收敛后不再发送 `cosy-clienttype`/`cosy-machineid`/`cosy-machinetoken`/
`cosy-clientip` 等官方客户端身份字段；身份密文只含 uid/昵称/access token，
**不再把 refresh token 放进请求**。`Cosy-ClientType: 10` 只用于签到活动接口（不变）。

Chat 仍按 `qoder_encode` 编码请求体、按 `path` 去 `/algo` 签名；响应是
`{"statusCodeValue":200,"body":"<chunk JSON>"}` 信封，复用既有翻译器与聚合器。

### 31.3 构建与测试

```text
$ cargo fmt --all --check                  （无输出）
$ cargo clippy --all-targets -- -D warnings Finished ... (0 warnings)
$ cargo test                               test result: ok. 388 passed; 0 failed
$ cargo build                              Finished `dev` profile ...
```

新增/重写的关键测试：`payload_envelope_keeps_the_verified_field_order`、
`info_blob_matches_fixed_aes_vector`（固定密文）、`headers_carry_only_the_signed_fields_*`、
`model_list_is_signed_and_parsed`、`chat_stream_translates_frames_and_sends_the_encoded_body`、
`chat_aggregates_the_stream_into_one_response`、`chat_maps_quota_rejections_*`、
`a_stream_without_done_or_finish_reports_truncation`。

### 31.4 真实账号联调（2026-10-29，CN 账号 `edufx`）

| 能力 | 请求 | 观察 |
| --- | --- | --- |
| model_discovery | 网关启动时 `GET /v1/models` | 发现 11 个模型：`auto`、`fast`、`qmodel_38max`、`qfmodel`、`dmodel`、`dfmodel`、`gmodel`、`gfmodel`、`kmodel_latest`、`kmodel`、`mmodel`（含上下文长度，如 200000/1000000） |
| chat（非流式） | `POST /v1/chat/completions`，`model=auto` | `pong`；`reasoning_content` 保留；`usage {15,157,172}`；`finish_reason=stop`；耗时 3.5 s |
| chat（流式） | 同上 + `stream:true` | 文本增量 → `finish_reason=stop` → `[DONE]`；耗时 3.5 s |
| tools（非流式） | `get_weather` 工具 + 明确要求调用 | `finish_reason=tool_calls`，`id=call_55041f…`，`arguments={"city": "Beijing"}` |
| 多轮工具结果 | assistant `tool_calls` + `role:"tool"` 回传 | `finish_reason=stop`，正文正确使用工具结果；UTF-8 校验通过（☀️/🌡️ 未被破坏） |
| 单模型异常 | `model=qfmodel` | 上游 400 `[FAIL]node:oa_qwen-plus-main … Execution failed` → 归类 `UpstreamServer`（节点瞬时失败），未误判账号；同一账号 `auto` 正常 |

### 31.5 未验证与下一步

- 未验证：token 刷新（凭据未到过期时间）、Intl、每账号并发/限流真实数值、
  `Capabilities::cancellation`（上游无取消接口）、不同模型的实际可用性差异
  （`qfmodel` 曾返回节点失败）、计费/积分换算。
- `[EXCEED_QUOTA]` 等哨兵帧未做专门映射；出现时按“缺少 finish_reason”处理，等真实
  样本后再定语义。
- 下一步（**本阶段不启动**）：D2-E 并发/限流观察与 D2-G 汇总；Qoder token 刷新与 Intl。

## 32. M13-D2-D — Qoder 错误与稳定性收尾（2026-10-29，完成）

### 32.1 范围

只做错误语义与流式稳定性，不新增 Provider 能力、不改变路由结构。

| 文件 | 改动 |
| --- | --- |
| `src/providers/qoder/events.rs` | `[EXCEED_QUOTA]`（顶层与信封 `body` 内）归一为 `402` 信封 → `QuotaExhausted`；`[NOT_EXCEED_QUOTA]`/`[NOTIFICATIONS]#` 跳过 |
| `src/providers/qoder/provider.rs` | 流空闲 120 s 超时：`chunk()` 超时即终止流并返回 `Timeout`，不再无限占用会话/账号租约 |
| `src/providers/qoder/error.rs` | 模型级分类的显式测试：节点故障、模型不存在、429、402，以及“不使用 CodeBuddy 业务码” |
| `src/test_support.rs` | `Reply::sse_stalled`：先发一段再暂停，用于测试中断路径（仅测试代码） |

### 32.2 行为约定

| 场景 | 行为 |
| --- | --- |
| `data: [EXCEED_QUOTA]` / 信封 `body` 以之开头 | 终止流，`QuotaExhausted`（非流式路径返回 Err，不返回半截回答） |
| `data: [NOT_EXCEED_QUOTA]`、`[NOTIFICATIONS]#…` | 忽略（保活/遥测） |
| 流内错误信封（如 402）出现在内容之后 | 终止，返回分类后的错误；下游收到 error 事件 + `[DONE]` |
| EOF 且无 `[DONE]`、无 `finish_reason` | `UpstreamServer` 截断错误（沿用） |
| 有 `finish_reason` 但无 `[DONE]` 就 EOF | 正常结束（补 finish 事件） |
| 请求头已返回但 120 s 无任何字节 | `Timeout`，释放流与会话租约 |
| 400 `[FAIL]node:… Execution failed` | `UpstreamServer`（模型节点瞬时故障，不罚账号） |
| 404/400 + model markers | `ModelUnavailable`（换号无用） |
| 429 | `RateLimited` |
| 402 / `Billing daily count exceeded` | `QuotaExhausted`（不封号） |

说明：`[EXCEED_QUOTA]` 的语义映射依据官方客户端的标记名与忽略逻辑，
**没有真实超额样本**；命中后按额度类错误处理，宁可显式报错也不返回半截内容。
PolyGate 只有账号级失败策略，没有按模型冷却的独立表：模型级错误会返回给调用方，
不会把账号标记为失效。

### 32.3 验证

```text
$ cargo fmt --all --check                  （无输出）
$ cargo clippy --all-targets -- -D warnings Finished ... (0 warnings)
$ cargo test                               test result: ok. 393 passed; 0 failed
$ cargo build                              Finished `dev` profile ...
```

新增测试：`quota_sentinels_end_the_stream_as_a_quota_error`、
`envelope_done_keepalive_and_status_are_decoded`（补充哨兵断言）、
`a_quota_sentinel_after_content_terminates_the_stream`（流式 + 非流式）、
`an_idle_stream_times_out_instead_of_hanging`、`a_finish_reason_without_done_ends_cleanly`、
`envelope_errors_use_the_same_model_level_rules_as_http`。

### 32.4 未验证与下一步

- 未取得真实的 `[EXCEED_QUOTA]`/额度耗尽样本；空闲超时只用 Mock 验证，未真实触发。
- 未做按模型的冷却/熔断；上游无取消接口，客户端断开只释放本地租约。
- 下一步（**本阶段不启动**）：D2-E 并发/限流观察、Qoder token 刷新、Intl。

## 33. M13-D2-E — 会话生命周期与租约测试（2026-10-29，完成）

### 33.1 范围

按任务要求只验证租约与资源生命周期，**不推进双账号负载均衡**，不改路由结构：

| 文件 | 改动 |
| --- | --- |
| `src/api/session_tests.rs`（新） | 5 个端到端测试：真实 Axum Router + `SessionRouter` + `QoderProvider`，上游为本地确定性 Mock HTTP 服务器 |
| `src/session_service/tests.rs` | 3 个测试：失效会话只在同账号恢复、认证失败不静默换号、重启后有效/失效/过期绑定的处理 |
| `src/api/mod.rs` | 注册 `session_tests`（仅测试） |

测试使用的策略：`max_in_flight = 1|2`、`serialize_session = true`、短 `queue_timeout`；
Qoder 流空闲超时在测试中缩短为 150 ms（生产值 120 s）。没有真实网络请求。

### 33.2 结果

| # | 场景 | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | 客户端主动断开 SSE | 丢弃响应 body 即释放会话锁与账号槽；第二个同会话请求立即成功；被中断的请求不记 usage | `client_disconnect_releases_the_session_and_account_lease` |
| 2 | 120 s 流空闲超时 | 空闲超时 → `Timeout`，下游收到 error 事件 + `[DONE]`；`finish()` 执行一次；账号认证状态不变；租约立即复用 | `idle_timeout_ends_the_stream_and_releases_the_lease` |
| 3 | 同一会话并发请求 | 同会话请求在租约存续期间被 per-session 锁串行化；不同会话不受影响；关掉 `serialize_session` 后该测试会失败（已做变异验证） | `concurrent_requests_for_one_session_are_serialized` |
| 4 | 上游额度耗尽 | 流内 `[EXCEED_QUOTA]` → `insufficient_quota` 错误帧 + `[DONE]`；非流式 → 402；两种路径都保留绑定、不标记账号、立即释放租约 | `quota_exhaustion_mid_stream_ends_the_stream_and_frees_the_lease`、`quota_exhaustion_before_the_stream_returns_402_and_frees_the_lease` |
| 5 | 上游会话失效 | `SessionBindingInvalid` → 同一账号重置 upstream id（下一请求生成新 id），**从不静默换号**；认证类失败（`AuthExpired` 等）→ 标记账号并阻塞该会话，不回退到其他账号 | `invalid_upstream_session_recovers_on_the_same_account_only`、`auth_failure_blocks_the_bound_account_instead_of_switching` |
| 6 | 网关重启 | 有效绑定恢复同一账号与同一 upstream id；失效绑定保持失效（`SessionBindingInvalid`）；过期绑定不会自动重建；create-only 绑不复活失效行；真实进程重启后同一 `x-session-id` 仍命中原绑定 | `restart_reopens_valid_bindings_and_keeps_invalid_or_expired_ones_blocked`（文件型 SQLite 关闭后重开）+ 真实进程重启冒烟（§33.3） |

会话语义补充：被客户端中断的流不会调用 `touch()`，因此不计入 `request_count`，也不延长 TTL；
这是既有文档化行为（`SessionLease::finish` 只在正常/错误终止路径调用），本次用测试固定下来。

### 33.3 验证

```text
$ cargo fmt --check                                       （无输出）
$ cargo clippy --all-targets -- -D warnings               Finished ... (0 warnings)
$ cargo test                                              test result: ok. 401 passed; 0 failed
$ cargo build                                             Finished `dev` profile ...
```

新增 8 个测试（393 → 401）；其中 API 端到端 5 个、会话服务 3 个。新测试单独重复运行 5 次均稳定。

重启后 Live 冒烟（真实 CN 账号，未改动协议）：

```text
$ curl /v1/models                      22 models（11 Qoder）
$ curl /v1/chat/completions (fast)     {'finish': 'stop', 'content': 'ok', 'usage': {...}}  0.94 s
```

真实进程重启（`Stop-ScheduledTask` + 结束 `polygate.exe` → 重新启动）后，用同一
`x-session-id: d2e-live-smoke` 再发一次请求：

```text
重启前  sessions: account=9e0913be…, upstream=9bcbf671…, request_count=1, state=active
重启后  sessions: account=9e0913be…, upstream=9bcbf671…, request_count=2, state=active
请求结果 {'finish': 'stop', 'content': 'OK'}
```

即持久化绑定在真实进程重启后恢复原账号与 upstream id，只有进程内的并发租约被重置。

### 33.4 未验证与边界

- **没有适配器会把上游错误映射为 `SessionBindingInvalid`**：Qoder 没有 upstream session；
  Qoder 403 `token_expire` / `12153` 映射为 `AuthExpired`/`AuthInvalid`（账号不可用，不换号），
  CodeArts 并发会话墙映射为 `RateLimited`。因此第 5 项的“会话失效恢复”是**策略层**验证，
  不是真实上游样本验证；若后续发现可恢复的上游会话错误，适配器应显式映射到该错误类别。
- 真实 120 s 空闲超时与真实额度耗尽样本仍未触发（分别为 Mock 缩短值、Mock 哨兵）。
- 双账号负载均衡、权重/冷却、自动重试均**未实现**（按本轮指示暂不推进）。
- 网关重启测试使用同一进程内的 SQLite 重开；未覆盖多实例共享数据库时的租约竞争（进程内租约，等 Scheduler 阶段）。

## 34. M14-A — WorkBuddy Provider 迁移研究（2026-10-29，完成）

### 34.1 范围与产出

按任务要求**只做研究**：不实现 Adapter、不改 Provider/协议代码、不发起真实请求、不读取本机
任何 WorkBuddy 凭据。产出为 `docs/WORKBUDDY_MIGRATION.md`（19 节），本阶段不产生代码差异。

证据来源与等级（详见文档 §0）：三个独立 MIT 实现（本地 `~/workbuddy2api`、
`.refs/RobbsLuo_Coding2API`、`.refs/chengzhi-c_dsh-wtq-pool`）+ PolyGate 现有源码；
均为“源码事实 / 作者实测”，**没有 PolyGate 自己的真实请求**。本机未安装 WorkBuddy 桌面端，
只有 `CodeBuddyExtension` 数据目录，未读取。

### 34.2 主要结论

| 主题 | 结论 |
| --- | --- |
| Provider 类别 | **Model**（OpenAI 兼容 `/v2/chat/completions`），上游**没有**服务端 Agent Task API，工具循环在客户端 |
| 会话 | 上游**没有**原生会话；同一 conversation id 再发一次就是新回合，历史由客户端全量回传。PolyGate 只做账号亲和 |
| 取消 | 无取消端点；`Capabilities::cancellation = false`，断开即停止本地读取 |
| 认证 | 设备码/官方页面 + 轮询，或导入已授权 `.info`；`accessToken`（JWT）+ `refreshToken` + 双过期时间 |
| 刷新 | `POST /v2/plugin/auth/token/refresh`，`X-Refresh-Token` 头，body `{}`；token 会轮换；24 h 上限、临期 5 min 预刷新 |
| CN / Intl | 同一套协议不同域；Intl 需额外 conversation id 与首条 system 消息；token 绑定域，不可跨域使用 |
| 模型 | CN `/v2/enterprises/{id}/models` 取 `agents[name=cli].models`；Intl `/v3/config` |
| 额度 | `/v2/billing/meter/get-user-resource`，单位 credits，分月度包（CapacityType=4）与一次性包 |
| 签到 | 仅 CN；状态查询只读，claim 会改变账号状态，必须独立显式授权；业务码两套实现不一致 |
| 流式 | 上游总是 SSE 且为 OpenAI 行格式；非流式需本地聚合；`finish_reason` 可能为 `content-filter`（连字符） |
| 错误 | 401/12153 → 认证；11140 → 账号受限（**不换号**）；402/14001/14012–14018 → 额度；6004/6008 → 模型日额度；6020–6022 → 排队；11128 → 渠道/内容拦截（语义有三解） |

### 34.3 架构适配结论

现有抽象基本够用：`Provider` / `Capabilities` / `ChatRequest` / `ChatResponse` / `StreamEvent` /
`SessionRouter` / `QuotaSnapshot` 都可直接承接；CodeArts 的 `CodeArtsHttp`、Qoder 的 SSE 行缓冲、
`CredentialService`、`SessionRouter`、`QuotaRefreshHook`、`CheckinExecutor` 可复用，
但**签名、会话墙、COSY 信封、`upstream_session_id` 语义不可照搬**。

需要的**最小**协议扩展（不重写通用协议，详见文档 §15.2）：

1. `ChatRequest` 增加可选 `reasoning_effort`（及 `thinking`/`enable_thinking`），默认 `None`。
2. OpenAI 入站 `WireMessage` 的 `deny_unknown_fields`（`src/api/openai.rs:61-73`）需放行
   assistant 的 `reasoning_content`，否则客户端回传历史会 400。
3. `FinishReason::from_upstream` 增加 `"content-filter"` 拼写。
4. 图片首版走 `ContentBlock::Unsupported` 拒绝并给明确错误。
5. `providers/workbuddy/events.rs` 复用行缓冲思路，不搬 Qoder 信封逻辑。

### 34.4 安全红线（本阶段新增判定）

文档 §14 明确列出**不迁移**的参考实现能力：Turing `X-Device-Token`/机器指纹、官方 UA/
`x-stainless-*`/`X-IDE-Version`/`X-Machine-ID`/`X-Session-ID` 伪造、提示词“中和/折叠”与
脱敏脚本、conversation id 轮换规避 10105、短信登录复刻、11140 后自动换号。
签到 claim 仍按既有规则：先查状态，只有显式授权才调用。

### 34.5 Go/No-Go（M14-B 的第一步）

> 用**诚实头**（Bearer、`X-User-Id`、`X-Enterprise-Id`、`X-Tenant-Id`、`X-Domain`、
> `Content-Type`、`Accept`、PolyGate 自己的 UA；不伪造版本/设备/会话指纹）对 CN 账号做只读探针：
> **额度 → 模型列表 → 一次最小非流式聊天**。
>
> - 三项 `code == 0` 且无 11128 → 按诚实头实现 Provider；单个“闸门头”需逐项确认属于协议必需。
> - 稳定 11128 / 403 → **停止聊天实现**，WorkBuddy 标 `unavailable`，报告“须修改 AGENTS.md 才能继续”，
>   不得偷偷加伪装头。
> - 只读额度/状态若可用，可先作为受限能力；签到仍需显式授权。

### 34.6 未验证项

诚实头能否通过 11128（关键）、11128 的真实触发条件、上游并发阈值、模型日额度阈值、
`10105`/`4013` 的准确语义、签到业务码（1001/1002/1003 vs 10001）、Intl 聊天实测、
以及**全部 PolyGate 侧实现**（本阶段未开始）。这些均已在文档 §16 逐条标注。

### 34.7 M14-B 计划（未执行）

B0 合规探针（先决）→ B1 离线协议层（凭据/错误/模型/额度/SSE/转换，全 Mock）→
B2 Provider 与 runtime 接入 → B3 刷新与凭证生命周期 → B4 签到（可选，显式开关）→
B5 验证与文档。详见文档 §17。

### 34.8 验证

本阶段无代码改动，仍执行 AGENTS.md §5 的四条命令确认基线未破坏：

```text
$ cargo fmt --check                                （无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings        Finished `dev` profile ... (0 warnings)
$ cargo test                                       test result: ok. 401 passed; 0 failed
$ cargo build                                      Finished `dev` profile ...
```

401 与 M13-D2-E 一致（未增删测试）。网关进程（scheduled task，PID 48484）在验证期间保持运行，
构建未触发重新链接，未影响服务。

## 35. M14 范围调整 — ZCode 明确排除（2026-10-29，完成）

### 35.1 范围

本轮**只调整开发范围与相关文档**，未实现 M14-B，也未提前进入后续里程碑。

| 项 | 结论 |
| --- | --- |
| 必须支持 | CodeArts（保留已有实现）、WorkBuddy（优先）、TRAE、Qoder、CodeBuddy CN/Intl |
| ZCode | **Explicitly Excluded**：不实现 `ZCodeProvider`、不迁移 `zcode2api` 任何功能、不注册、不出现在模型列表/配置/管理界面 |
| 开发顺序 | M14-B WorkBuddy 核心 → M14-C WorkBuddy 认证/额度/签到 → M14-D TRAE 核心 → M14-E TRAE 认证/额度/签到 → M14-F 跨 Provider 回归 → 后续 Qoder、CodeBuddy |

### 35.2 代码 / 配置 / UI 变更

| 文件 | 变更 |
| --- | --- |
| `src/config/mod.rs` | 删除 `ProviderKind::Zcode` 及 `as_str` / `from_db_str` / `display_name` 三个分支；仓库中不存在 ZCode Provider 实现，删除后无残留引用 |
| `web/index.html` | 移除账号子导航中的 `ZCode 待接入` 按钮 |
| `web/app.js` | 「接入状态」文案移除 ZCode |
| `.refs/liu5269_zcode2api` | **保留**为历史审计资料，不删除；任何代码不得取自该仓库（AGPL-3.0 且功能已排除） |

行为变化（有意为之）：配置中使用 `kind = "zcode"` 会在反序列化阶段被拒绝；
没有兼容 shim，也不提供“静默忽略”。数据库中的 `provider_id` 若含 `zcode`，
`ProviderKind::from_db_str` 返回 `None`，与其它未知值同样处理。

### 35.3 文档变更

- `PROGRESS.md`：表头、§1 Provider 范围行、参考仓库/研究文档/后端实现/Provider 实现行。
- `docs/PROVIDERS.md`：§6 改为“明确排除”、§7 增加 ZCode 条目、§9 能力表与 §13.2 矩阵移除 ZCode 行并加说明、§13.3 调整措辞。
- `docs/MIGRATION_MATRIX.md`：开头增加范围声明，许可证审计行与 §6 排除清单标注“已排除”。
- `docs/ARCHITECTURE.md`：目标、模块树、迁移优先级、排序依据四处移除 ZCode。
- `docs/RESEARCH.md` §12：增加“历史审计、已排除、不得据此实现”的状态变更说明。
- `docs/PROVIDER_MIGRATION.md`、`docs/CHECKIN_RESEARCH.md`：对应行标注排除。

### 35.4 验证

```text
$ cargo fmt --check                                （无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings        Finished `dev` profile ... (0 warnings)
$ cargo test                                       test result: ok. 401 passed; 0 failed
$ cargo build                                      Finished `dev` profile ... (重新编译)
$ ./target/debug/polygate.exe --help               exit 0，子命令 serve/doctor/config-example 正常
```

测试数量仍为 401：ZCode 在代码中只有枚举占位，无测试、无 Provider、无签到执行器。
`PolyGate-Gateway` 计划任务当前状态为 `Ready`（未运行）；本轮未启动或修改该任务。

### 35.5 边界与后续

- 本轮**没有**新增 WorkBuddy/TRAE 代码，M14-B 仍未开始。
- ZCode 的排除是范围决定，不是“暂未实现”；不得因 Coding2API 支持而在后续阶段自动重新加入。
- 若未来确需 ZCode，必须先显式修改本范围与 `AGENTS.md`，并重新评估 AGPL 与验证码依赖。

## 36. M14-B — WorkBuddy 核心 Provider（2026-10-29，完成，MockVerified）

### 36.1 范围与文件

按 M14-B 要求实现 WorkBuddy 的协议核心；**不要求真实账号**，全部测试走本地
Mock HTTP Server。额度、Token 刷新、签到与完整认证生命周期属 M14-C，本轮未做。

| 文件 | 内容 |
| --- | --- |
| `src/providers/workbuddy/mod.rs` | 模块说明与合规边界 |
| `region.rs` | CN / Intl 基址、默认域、模型路径、域归属校验；无 fallback |
| `credential.rs` | 扁平与桌面 `.info` 两种形状；毫秒/秒过期归一；`uid` 与 JWT `sub` 交叉校验；`Debug` 脱敏 |
| `error.rs` | 业务码优先于 HTTP 状态；认证/额度/受限/限流/模型/请求级分类；`Retry-After` 解析 |
| `http.rs` | 仅协议必需头 + 自有 UA；JSON 与 SSE 两条发送路径；200 错误信封识别 |
| `models.rs` | `{models, agents}` 解析、`agents[name=cli]` 过滤、禁用模型跳过、空目录视为发现失败 |
| `convert.rs` | 请求体构造、`developer→system`、工具配对校验、事件聚合 |
| `events.rs` | 纯 OpenAI SSE 解码、空占位忽略、`content-filter`、截断判定 |
| `provider.rs` | `WorkbuddyProvider`：模型/聊天/流式/本地认证状态；流空闲超时 120 s |
| `tests.rs` | 16 个 Mock 集成测试 |
| `src/runtime/mod.rs` | `ProviderKind::Workbuddy` 注册，支持 `base_url` 覆盖 |
| `src/protocol/response.rs` | `FinishReason::from_upstream` 接受 `content-filter` |

### 36.2 实现行为

- **Provider 类别**：`ProviderClass::Model`；`native_session` / `agent_task` /
  `cancellation` 均为 `false`；上游无取消接口，断开连接只停止本地读取。
- **聊天**：上游只支持 SSE，`stream: true` 固定；非流式请求由网关聚合同一条流，
  不发明第二个端点。`stream_options.include_usage = true`。
- **工具**：标准 `tools` / `tool_choice` / `assistant.tool_calls` / `role:"tool"`；
  工具结果必须对应前序 assistant 声明，否则显式 `BadRequest`（**不静默丢弃历史**）。
- **不修改用户内容**：不做 system 折叠、不做渠道标记中和、不做零宽脱敏；
  assistant 历史中的 `reasoning_content` 不回传（响应侧字段，输入侧无证据）。
- **认证**：仅本地校验——凭据可解析、域属于该区域、JWT 未过期；不调用上游探针。
  过期返回 `Expired`，不被误判为额度或模型问题。
- **区域隔离**：CN 模型路径 `/v2/enterprises/{id|personal}/models`，Intl `/v3/config`；
  `X-Domain` 与基址由账号 region 决定，跨区域凭据直接拒绝。

### 36.3 测试与验证

```text
$ cargo fmt --check                                （无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings        Finished ... (0 warnings)
$ cargo test                                       test result: ok. 445 passed; 0 failed
$ cargo build                                      Finished `dev` profile ...
```

新增 44 个测试（401 → 445）：WorkBuddy 43 个（16 个 Mock 集成 + 27 个单元），
运行时注册 1 个。覆盖：CN/Intl URL 与域、模型发现与 `cli` 过滤、非流式聚合、
SSE 流式、工具分片拼装、工具历史透传、认证过期（本地、零请求）、401/402/403 区分、
429 + `Retry-After`、5xx、请求超时、流空闲超时、截断、200 错误信封、流丢弃后复用、
跨区域凭据拒绝、`quota` 明确 `Unsupported`、凭据 `Debug` 脱敏。

所有测试离线运行；`spawn_server` 只监听 `127.0.0.1:0`。

### 36.4 状态

| 项 | 状态 |
| --- | --- |
| 凭据读取（扁平 / `.info`） | MockVerified |
| 模型发现（CN / Intl） | MockVerified |
| 非流式聊天 | MockVerified |
| SSE 流式聊天 | MockVerified |
| Tool Calling（请求与流式拼装） | MockVerified |
| 错误归一化（含 429/5xx/超时/截断） | MockVerified |
| 本地认证状态 | MockVerified |
| 真实上游（任何能力） | **未验证**：无账号、未发请求 |
| Token 刷新 / Refresh 轮换 / 设备码登录 | 未实现（M14-C） |
| 额度查询 | 未实现，`quota` 报 `Unsupported`（M14-C） |
| 签到 | 未实现（M14-C） |

### 36.5 CompatibilityGap（不实现，如实记录）

1. **官方客户端身份头**：`X-CodeBuddy-Request`、`X-IDE-Name/Type/Version`、
   `X-Product`、`X-Agent-Intent/Purpose/Type`、`X-Conversation-*`、`X-Request-ID`、
   `x-stainless-*` 一律不发送。按 M14-A 判断它们属于客户端身份标记，不属于协议必需；
   上游是否接受诚实头**未验证**，这是 M14-C/真实验证阶段的首要观察项。
2. **设备证明**：`X-Device-Token`（Turing SDK）、`X-Machine-ID`、`X-Session-ID`
   不实现、不派生。
3. **UA**：发送 `polygate/<version>`，不伪装 `WorkBuddy/... CLI/...`。
4. **提示词与历史**：不做 channel marker 中和、system 折叠、零宽脱敏；因此下游
   携带其他客户端 system prompt 时，上游可能直接返回 11128。
5. **Intl 首条 system**：参考实现要求 `messages[0]` 为 system，本适配器不注入占位
   system（属于静默改写用户请求）；Intl 聊天状态标记为需要真实验证。
6. **推理参数**：不补 `reasoning_effort` / `enable_thinking`；上游可能退化为把思考
   写进可见 `content`，且 `reasoning_content` 可能为空。
7. **会话头**：不发送 `X-Conversation-*`；若 Intl 强制要求，聊天将失败。
8. **`.info` 加密信封**：新版桌面端的 `$wbEncrypted` 不解密，只接受明文凭据导入。

### 36.6 M14-C 待办（已由 §37 完成）

- Access Token 刷新：`POST /v2/plugin/auth/token/refresh`、`X-Refresh-Token`、
  refresh 轮换、`CredentialStore` CAS 持久化、单写者约束、24 h 上限与临期预刷新。
- 设备码/官方页面授权与凭据导入（不含验证码自动化）。
- 额度查询：`/v2/billing/meter/get-user-resource`，月度包与一次性包；
  未知额度保持 `null`；单位 credits，不做 token 换算。
- 签到：先只读状态；claim 需显式开关；业务日去重；Intl 不注册。
- 真实上游验证：先做诚实头只读探针（额度 → 模型 → 最小聊天），结果决定
  WorkBuddy 能否从 `experimental` 升级；失败则记录为 `unavailable`，不补伪装头。

## 37. M14-C — WorkBuddy 认证生命周期、额度与签到（2026-10-29，完成，MockVerified）

### 37.1 范围与文件

| 文件 | 变更 |
| --- | --- |
| `src/providers/workbuddy/region.rs` | 新增 auth / refresh / billing / check-in 路径与 `url_at` |
| `src/providers/workbuddy/credential.rs` | refresh token、refresh 过期、轮换合并、`needs_refresh` / `can_refresh` / `to_json`；Debug 与 Drop 同时覆盖两个 token |
| `src/providers/workbuddy/auth.rs` | 设备码授权：`/v2/plugin/auth/state`、`/v2/plugin/auth/token`、`/v2/plugin/login/account`；11217 / 12151 等待码，TTL 600 s，5 s 轮询建议 |
| `src/providers/workbuddy/http.rs` | `refresh()`（`X-Refresh-Token` + `X-Auth-Refresh-Source: plugin` + 诚实头）、billing 基址与 `post_billing_json` / `post_billing_raw` |
| `src/providers/workbuddy/quota.rs` | `get-user-resource` 请求体；月度包（`CapacityType == 4`）与一次性包；`*Precise` 优先；空列表 → Unknown |
| `src/providers/workbuddy/provider.rs` | 临期预刷新、进程内单飞 + SQLite 跨进程租约、revision CAS、写失败不报成功；`quota` / `auth_status_with_context` / `chat_with_context` / `chat_stream_with_context` / `quota_with_context` |
| `src/providers/mod.rs` | `CredentialStore` 新增带默认实现的 `acquire_refresh_lease` / `release_refresh_lease`；`Provider` 新增 `quota_with_context` |
| `src/account_service/{credential,store}.rs` | 刷新租约的 service 包装 |
| `src/storage/credentials.rs`、`migrations/0006_refresh_lease.sql` | `credential_refresh_leases` 表：`account_id` 主键、`owner`、`expires_at` |
| `src/checkin/workbuddy.rs` | CN 签到执行器（状态 + 受开关控制的 claim）；Intl 直接 `Unsupported` |
| `src/checkin/{mod,tests}.rs`、`src/runtime/mod.rs` | 注册执行器；WorkBuddy provider 注入 `CredentialStore`；签到后额度刷新走 `quota_with_context` |
| `src/admin/{mod,auth,api,account}` | 设备码流程（start / status / cancel）、`persist_workbuddy_login`、额度刷新走上下文 |
| `web/{index.html,app.js}` | WorkBuddy 子导航启用；通用授权流增加 WorkBuddy（真实 authUrl、5 s 轮询） |

### 37.2 认证生命周期

- **临期预刷新**：access token 在 300 s 内到期且存在 refresh token 时，先刷新再请求；
  未知过期时间（`0`）不刷新。
- **单写者**：
  1. 同一进程内按 `account_id` 串行（`tokio::Mutex`）；
  2. 跨进程用 SQLite 租约 `credential_refresh_leases`（TTL 60 s，owner = 进程随机 id）。
     拿不到租约时轮询已持久化的凭据（20 × 250 ms）；仍未更新则返回 `Timeout`，
     **不会**用同一张 refresh token 再刷一次。
  3. 写入用 `CredentialStore` 的 revision CAS。
- **轮换**：响应里的 `refreshToken` 覆盖旧值；响应缺失时保留旧值。写入前不向调用方
  暴露“已刷新”。
- **持久化失败**：`Storage` 错误原样返回；其它 CAS 冲突先重读，若已有新值则使用它，
  否则返回 `AuthExpired`，提示重新授权。任何路径都不会在未持久化时继续聊天。
- **失败分类**：`401/403` → `AuthInvalid`（终态，需重新授权）；`5xx`/网络 →
  `UpstreamServer`/`Transport`/`Timeout`（**不**标记 AuthInvalid）；业务码
  `12153` → `AuthExpired`。
- **401 单次重试**：聊天在尚未输出任何内容时收到 `AuthExpired`，强制刷新一次并重试；
  再次失败即返回，不存在重试循环。流式只在建立流之前重试，不重放已发送内容。
- **租约释放**：刷新请求失败或持久化完成后释放；进程崩溃时靠 TTL 过期。

### 37.3 设备码授权

- 流程：`POST /v2/plugin/auth/state?platform=CLI` → 官方 `authUrl` + `state`；
  轮询 `GET /v2/plugin/auth/token?state=`（11217 等待）→ `GET /v2/plugin/login/account?state=`（12151 等待）。
- 上游不提供单独的短“用户码”，因此 UI 只展示真实授权 URL；`state` 以 flow id 为 AAD
  加密存储，不写入普通列。
- 流程 TTL 取 `admin.flow_ttl_secs` 与上游 600 s 的较小值；UI 轮询被服务端节流到
  上游要求的 5 s（更快的请求只返回当前 pending 视图，不增加上游请求）；取消是本地
  决定（上游无取消端点）；过期或取消后不能再轮询。
- 同一时间只允许一个授权流程（沿用现有 admin 约束）；成功后经 `CredentialService`
  加密保存，身份 `workbuddy:<region>:<uid>`，重复授权更新同一行。
- 不实现验证码破解、自动页面操作、设备证明或客户端身份头。

### 37.4 额度

- `POST {billing_base}/v2/billing/meter/get-user-resource`，请求体按参考实现：
  `PageNumber/PageSize/Status=[0,3]/PackageEndTimeRangeBegin/End`，不带 `ProductCode`。
- 解析路径 `data.Response.Data.Accounts[]`（兼容少一层嵌套）；`Status != 0` 跳过。
- `CapacityType == 4` → 月度包：`CycleCapacity*`、`CycleEndTime`；
  其它 → 一次性包：`Capacity*`、`ExpiredTime`；两类都保留对方字段的兜底。
- `*Precise` 字符串优先；`total = monthly + one_off`，`remaining` 为逐包剩余之和并夹在
  `[0, total]`；`used = total - remaining`。
- 单位固定 `credits`，**不做** token 换算；响应级 `TotalDosage` 不参与（参考实现实测
  滞后，计入会误导）。
- 无有效套餐 → `Unknown`（`null`），不写 0、不写无限；业务错误与结构缺失 → 错误，
  不伪装成空余额。明细写入 `quota_snapshots.raw`（复用既有列，无新迁移）。

### 37.5 签到（CN）

- 状态：`POST /v2/billing/meter/checkin-activity-status`（只读）；
  `active=false` → `Unsupported`；`today_checked_in=true` → `AlreadyCheckedIn`。
- 领取：`POST /v2/billing/meter/daily-checkin`；`code == 0` 为成功；
  `1001`（workbuddy2api）或 `10001` / `msg` 含“已签到”（Coding2API，可能 HTTP 400）
  → `AlreadyCheckedIn`；`1002` 无资格、`1003` 活动结束 → `Failed`。
- **默认不领取**：只有 provider 选项 `checkin.claim_enabled = true` 才发送 claim；
  关闭时状态查询仍然可用。`Failed` 的说明直接点名该选项。
- 传输错误 / 5xx → 返回错误，由 `CheckinService` 记为 `Unknown`，不盲目重试；
  成功或已签到后由既有 `QuotaRefreshHook` 写入新额度快照。
- Intl 执行器直接返回 `Unsupported`，**零请求**；不新增调度器、不绕过 M12 去重。
- 后台 `run_due` 跳过 `enabled = false` 的账号（沿用既有逻辑，新增回归测试）。

### 37.6 Provider / 运行时接入

- `capabilities().quota = true`；`auth_status_with_context`、`chat_with_context`、
  `chat_stream_with_context`、`quota_with_context` 都会先执行一次受控刷新。
- runtime 为 WorkBuddy 注入 `AccountCredentialStore`，并在 `RoutingCheckinExecutor`
  注册 CN/Intl 执行器；claim 开关来自 provider 选项。
- 管理台「刷新认证」与「刷新额度」分别调用上下文版本，因此过期 token 会先刷新再探测。
- 不修改 CodeArts 任何行为；CN/Intl 凭据、基址、域、执行器完全隔离。

### 37.7 测试（全部离线，`spawn_server` 仅监听 127.0.0.1）

新增 44 个测试（445 → 489），覆盖 M14-C 要求的 20 类：

| # | 场景 | 测试 |
| --- | --- | --- |
| 1 | access token 有效 | `a_valid_credential_is_used_without_refreshing` |
| 2 | access token 过期且无刷新材料 | `an_expired_credential_without_refresh_material_is_reported` |
| 3 | refresh token 过期 | `an_expired_refresh_token_is_auth_expired_without_a_request` |
| 4 | refresh 轮换并先持久化 | `a_rotated_refresh_token_is_persisted_before_the_chat` |
| 5 | 并发刷新只发生一次 | `concurrent_calls_refresh_once` |
| 6 | CAS 冲突重读胜者 | `a_cas_conflict_reloads_the_winners_credential` |
| 7 | 刷新成功但写库失败 | `a_failed_persist_never_reports_a_successful_refresh` |
| 8 | 跨进程租约 | `another_process_holding_the_lease_is_waited_for_not_duplicated`、`refresh_leases_coordinate_across_connections_and_expire` |
| 9 | 刷新被拒 / 传输失败分类；401 单次重试 | `a_rejected_refresh_is_auth_invalid_and_persists_nothing`、`a_transport_failure_is_not_an_auth_failure`、`a_401_on_chat_triggers_one_forced_refresh_and_one_retry`、`a_persistent_401_is_retried_only_once` |
| 10 | 设备码成功落库 | `workbuddy_device_flow_persists_one_account_without_leaking_tokens` |
| 11 | 设备码过期 | `an_expired_flow_is_rejected_without_a_request` |
| 12 | 设备码取消 / 轮询节流 | `workbuddy_duplicate_flow_is_blocked_and_cancel_is_local`、`workbuddy_poll_throttle_keeps_the_upstream_interval` |
| 13 | 月度包 | `monthly_and_one_off_packages_are_separated` |
| 14 | 一次性包 | 同上 |
| 15 | 额度失败 | `a_quota_failure_is_not_reported_as_zero_balance`、`a_business_error_is_a_failure_not_an_empty_balance` |
| 16 | CN 签到成功 | `claim_succeeds_and_reports_credit`、`a_workbuddy_checkin_refreshes_quota_after_success` |
| 17 | Intl Unsupported | `intl_is_unsupported_without_a_request` |
| 18 | 未知保持 Unknown | `a_transport_failure_stays_an_error`、`an_empty_quota_is_unknown_never_zero` |
| 19 | 签到后刷新额度 | `a_workbuddy_checkin_refreshes_quota_after_success` |
| 20 | 账号禁用 / 脱敏 / 区域隔离 / M14-B 回归 | `a_disabled_account_is_skipped_by_the_background_loop`、`debug_never_prints_the_token`、`a_credential_from_another_region_is_rejected`、M14-B 的 16 个集成测试 |

### 37.8 验证

```text
$ cargo fmt --check
（无输出，exit 0）
$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile ... (0 warnings)
$ cargo test
test result: ok. 489 passed; 0 failed; 0 ignored; ...
$ cargo build
Finished `dev` profile ...
```

### 37.9 状态

| 项 | 状态 |
| --- | --- |
| access/refresh 过期与临期预刷新 | MockVerified |
| 401 单次强制刷新重试 | MockVerified |
| refresh 轮换 + CAS + 跨进程租约 | MockVerified |
| 刷新失败分类（AuthInvalid vs 传输） | MockVerified |
| 设备码授权（CN / Intl 同一流程） | MockVerified |
| 额度（月度包 / 一次性包 / 未知） | MockVerified |
| CN 签到状态 + 受控 claim | MockVerified |
| Intl 签到 | Unsupported（按参考实现，零请求） |
| 签到后额度刷新 | MockVerified |
| 真实上游（任何能力） | **未验证**：无账号、未发真实请求 |
| `$wbEncrypted` 桌面凭据解密 | 明确不实现 |
| 客户端身份/设备证明头 | 明确不实现（CompatibilityGap） |

### 37.10 边界与后续

- 参考实现之间仍有冲突：签到码 `1001/1002/1003`（workbuddy2api）vs `10001`
  （Coding2API）。本项目同时识别，语义以“已签到”优先；真实账号验证前不宣称
  `LiveVerified`。
- 额度字段存在两套词汇（`Cycle*` 与 `Capacity*`），本项目按包类型优先并兜底；
  上游时区未验证，字符串时间按 UTC 解析，原始值保留在 `raw`。
- 审计端点 `/v2/billing/meter/checkin-activity-status` 与签到端点使用
  `/v2/billing/meter/*`（workbuddy2api 实测），而 Coding2API 使用 `/billing/meter/*`；
  若真实账号出现 404，按端点差异记录，不做自动重试猜测。
- 本轮不进入 M14-D（TRAE）；真实上游只读探针仍未执行，需要凭据与授权。

## 38. M14-D — TRAE 核心 Provider（2026-10-29，完成，MockVerified）

### 38.1 范围与文件

| 文件 | 变更 |
| --- | --- |
| `src/providers/trae/region.rs` | CN/Intl realm：chat / pay / oauth 主机、路径、默认域、域与 `apiHost` 白名单 |
| `src/providers/trae/credential.rs` | 扁平 + 嵌套 `{auth,account}` 解析、区域/域/apiHost 校验、JWT 到期回落、uid/sub 交叉校验、Debug/Drop 脱敏 |
| `src/providers/trae/error.rs` | HTTP 状态、信封业务码、流内 `event: error` 分类；消息脱敏与截断 |
| `src/providers/trae/http.rs` | 诚实头（仅 token 头 + 导入的设备号）、模型 POST、聊天流打开、传输错误 |
| `src/providers/trae/models.rs` | `get_detail_param` 请求体与目录解析（`context_window_tokens.{dev,max}`） |
| `src/providers/trae/convert.rs` | `llm_utils_chat` 信封、消息/tools 改写、非流式聚合 |
| `src/providers/trae/events.rs` | 命名事件 SSE 解码、`output`/`token_usage`/`done`/`error` 翻译、工具分片 |
| `src/providers/trae/provider.rs` | `Provider` 实现、请求/流空闲超时、断流与截断 |
| `src/providers/trae/tests.rs` | 本地 Mock HTTP 集成测试 |
| `src/providers/mod.rs` | 声明 `trae` 模块并导出 `TraeProvider` |
| `src/runtime/mod.rs` | `ProviderKind::Trae` 注册（支持 `base_url` 覆盖） |
| `src/runtime/tests.rs` | 新增 TRAE 注册测试；“未实现 kind”改用 `codebuddy` |
| `docs/TRAE_MIGRATION.md` | 协议盘点、实现结构、差异与 M14-E 待办 |

### 38.2 协议盘点（源码证据）

两个参考实现：Coding2API（`src/provider/trae/*`）与 dsh-wtq-pool（`src/trae-*.ts`）。
完整引用见 `docs/TRAE_MIGRATION.md`。

1. **CN/Intl**：CN 网关 `trae-api-cn.mchost.guru` / `api.trae.cn` / `api.trae.com.cn`；
   Intl 只有 dsh 给出的 chat `coresg-normal.trae.ai`、pay `growsg-normal.trae.ai`；
   两个实现都没有可信的 Intl OAuth 主机 → 留空，不猜。
2. **认证**：`Cloud-IDE-JWT` + `X-Cloudide-Token` + `X-Ide-Token`；官方 IDE 还会发
   `X-App-Id`、版本族、设备/OS 头与 `Trae/…` UA，本项目**不迁移**。
3. **刷新**：`ExchangeToken`，`Result.Token/RefreshToken/TokenExpireAt`；M14-E 实现。
4. **模型发现**：`POST get_detail_param`，目录 `config_info_list[]`，上下文取
   `max > dev` 的 `max`；空列表是失败，不回退静态表。
5. **聊天**：`POST llm_utils_chat`，自有信封（`function=solo_work_lite`、
   `config_name`、`model`、`stream=true`）；只流式，非流式本地聚合。
6. **SSE**：命名事件 `output` / `token_usage` / `done` / `error`；其余无下游语义；
   `[DONE]` 与无事件名帧作为兼容兜底。
7. **Tool Calling**：响应 `function_call`，按 index 分片，无 name 的参数续片必须保留；
   请求 `parameters` 必须字符串化。
8. **会话**：无原生会话、无取消端点 → `native_session=false`、`cancellation=false`。
9. **额度**：`ide_user_ent_usage` 逐包求和；M14-E 实现，Unknown 保持 null。
10. **签到**：`checkin_credits/status|claim`；参考实现生成随机数字设备号，属设备指纹
    伪造，默认**不实现**；M14-E 只有运维显式提供设备号并授权时才可评估。
11. **错误**：`402`/`14018`/`4008` 额度；`1005` 权益；`11102` 账号受限；`401` 过期；
    `404` 软限流；`429` 限流（`6004` 模型）；`400` 请求无效；流内错误码同表。
12. **客户端依赖**：IDE 头族、设备号、Referer/timezone、pacer、积分估算均不迁移，
    记录为 CompatibilityGap。

### 38.3 实现行为

- **凭据**：两种形状都接受；`domain` 必须属于区域；`apiHost` 越界直接拒绝；
  `uid` 必须与 JWT `sub` 一致；`expires_at` 只保存上游显式值（JWT 仅在读取时回落）。
- **请求改写**：`developer`→`system`；content 字符串→内容块数组；assistant
  `function`→`function_call`；纯工具轮 content→`null`；`tools.parameters`→JSON 字符串；
  `tool_choice: "none"` 删除 tools；强制函数→函数名；无 tools 时省略 `tool_choice`。
- **有意差异**：孤儿 tool 结果与无 name 的 tool call 返回 `BadRequest`，不静默丢弃
  （会话历史完整性）。
- **流**：解码器支持 CRLF / 跨 chunk UTF-8 / 多行 data；`done` 或 `[DONE]` 立即结束；
  EOF 未见结束标记 → 流内错误；空闲 120 s 断开；错误帧立即终止流。
- **非流式**：聚合同一 SSE 流，保留文本、推理、工具调用、usage 与 finish_reason；
  错误事件不会伪装成成功响应。
- **超时**：短请求 30 s；聊天流不设总超时，只设空闲超时（长上下文不受总时长限制）。
- **过期凭据**：M14-D 无刷新，过期凭据在本地返回 `AuthExpired`，**零请求**。

### 38.4 合规边界

- 只发送 `Authorization: Cloud-IDE-JWT`、`X-Cloudide-Token`、`X-Ide-Token`、
  `X-Uid`，以及运维导入的 `X-Machine-Id` / `X-Device-Id`；缺失就不发，**不生成**。
- 不发送 `X-App-Id`、IDE 版本族、设备类型/品牌/OS、`Request-Traffic-Type`、
  `Trae/…` UA、Referer；UA 为 `polygate/<version>`。
- 不实现验证码、注册、设备指纹、客户端冒充、提示词改写、风控规避。
- 日志与错误消息不包含 token；Debug 与 Drop 同时覆盖 access / refresh token。

### 38.5 测试（全部离线）

新增 63 个测试（489 → 552）：TRAE 模块 62 个 + runtime 注册 1 个。

| # | 场景 | 测试 |
| --- | --- | --- |
| 1 | 扁平 / 嵌套凭据 | `flat_credential_is_loaded`、`nested_ide_shape_is_loaded_with_millisecond_expiry` |
| 2 | 区域 / 域 / apiHost 隔离 | `a_token_from_another_realm_is_rejected`、`an_out_of_realm_api_host_is_rejected`、`api_hosts_are_validated` |
| 3 | uid 与 JWT 一致性、到期回落 | `uid_must_match_the_token_subject`、`jwt_exp_is_the_last_expiry_fallback` |
| 4 | 脱敏 / 往返 | `debug_never_prints_the_token`、`secret_json_round_trips_the_normalized_fields` |
| 5 | 设备号只透传不生成 | `missing_device_identifiers_stay_empty`、`imported_device_identifiers_are_forwarded_not_invented` |
| 6 | 诚实头 / 无冒充 | `honest_headers_never_impersonate_the_ide` |
| 7 | 模型目录 | `catalogue_uses_the_max_window`、`duplicate_ids_are_kept_once`、`models_use_the_directory_envelope` |
| 8 | 目录失败 | `a_missing_list_or_an_empty_list_is_a_failure` |
| 9 | 信封与采样字段 | `envelope_and_content_are_rewritten` |
| 10 | 消息改写 / function_call | `assistant_tool_calls_use_function_call`、`tool_history_is_converted_to_solo_shape` |
| 11 | 历史完整性 | `an_orphan_tool_result_is_a_bad_request`、`an_orphan_tool_result_is_refused_before_any_request`、`an_unnamed_tool_call_is_a_bad_request` |
| 12 | tools / tool_choice | `tool_parameters_are_stringified`、`tool_choice_none_drops_the_tools`、`a_forced_function_becomes_its_name`、`a_tool_policy_without_tools_is_omitted`、`chat_tools_and_choice_reach_the_upstream` |
| 13 | SSE 解码 | `decoder_keeps_event_names_and_split_utf8`、`comment_and_metadata_frames_yield_no_events` |
| 14 | output / reasoning / tools | `output_carries_content_reasoning_and_tools`、`a_tool_continuation_without_a_name_is_kept`、`blank_tool_fragments_are_noise` |
| 15 | usage | `usage_keeps_cache_read_and_zero` |
| 16 | done / [DONE] / 截断 | `done_finishes_immediately_and_eof_is_then_clean`、`done_without_reason_is_stop`、`truncation_without_done_is_an_error`、`a_truncated_stream_is_reported_as_an_error_event` |
| 17 | 流内错误 | `stream_error_frames_are_classified`、`an_in_stream_error_frame_becomes_a_stream_error` |
| 18 | HTTP / 业务码分类 | `auth_and_quota_are_distinct`、`plan_restriction_is_not_a_dead_account`、`model_and_request_failures_keep_their_meaning`、`blocked_account_is_recognised`、`stream_codes_match_the_http_mapping`、`upstream_failures_are_classified` |
| 19 | 流式 / 非流式聚合 | `non_streaming_chat_aggregates_named_events`、`streaming_chat_emits_normalized_events`、`aggregate_keeps_text_reasoning_tools_and_usage` |
| 20 | 超时 / 能力 / 过期 / 注册 | `idle_stream_is_cut_off`、`a_slow_model_request_times_out`、`declared_capabilities_are_honest`、`expired_credentials_are_reported_and_not_sent`、`regions_never_share_credentials_or_hosts`、`bind_accepts_the_trae_provider` |

### 38.6 验证

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

### 38.7 状态

| 项 | 状态 |
| --- | --- |
| 凭据解析 / 区域隔离 / apiHost 白名单 | MockVerified |
| 模型发现 | MockVerified |
| 非流式 / SSE 流式聊天 | MockVerified |
| Tool Calling（请求 / 流式分片 / 历史） | MockVerified |
| Usage / FinishReason / 流内错误 | MockVerified |
| 超时 / 断流 / 截断 | MockVerified |
| 额度查询 | 未实现（M14-E）；`quota = false` |
| Token 刷新 | 未实现（M14-E） |
| 签到 | 未实现；设备号生成被策略禁止（M14-E 评估） |
| 管理台 TRAE 授权 | 未启用（M14-E） |
| 真实上游（任何能力） | **未验证**：无账号、未发真实请求 |

### 38.8 边界与后续

- 参考实现冲突已按“更接近源码证据 + 不臆造”的原则取舍，逐条记录在
  `docs/TRAE_MIGRATION.md` §15。
- Intl 的 OAuth 主机与额度端点没有可信证据，M14-E 不得按 CN 猜测。
- 上游是否接受“诚实的非 IDE 头”仍是最大不确定项；若真实账号被拒，记录为
  CompatibilityGap，不通过伪造头解决。
- 本轮不进入 M14-E（TRAE 认证/额度/签到），也不修改 CodeArts / WorkBuddy / Qoder。

## 39. M14-E — TRAE 认证生命周期、额度与签到（2026-10-29，完成，MockVerified）

### 39.1 范围与文件

| 文件 | 变更 |
| --- | --- |
| `src/providers/trae/auth.rs` | 新增：登录 URL、回调解析、`ExchangeToken`、`GetUserInfo` |
| `src/providers/trae/quota.rs` | 新增：CN 积分包、到期阶梯、Intl 订阅快照 |
| `src/providers/trae/credential.rs` | 新增 `needs_refresh` / `can_refresh` / `merge_refreshed` / `name_hint` |
| `src/providers/trae/provider.rs` | 刷新 + CAS + 租约、额度、`auth_status_with_context`、401 单次重试 |
| `src/checkin/trae.rs` | 新增：CN 签到执行器（状态查询 + 可选领取） |
| `src/checkin/mod.rs`、`src/runtime/mod.rs` | 注册签名与运行时接线（`checkin.device_id`） |
| `src/admin/auth/trae.rs` | 新增：授权流状态、加密设备 ID、粘贴回调完成、单活动流 |
| `src/admin/api/mod.rs`、`src/admin/mod.rs` | 新增 4 条 `/admin/api/auth/trae/*` 路由与处理函数 |
| `src/admin/account/mod.rs` | `persist_trae_login`（外部身份、元数据、CAS 凭据写入） |
| `web/index.html`、`web/app.js` | TRAE 子导航、设备 ID 输入、粘贴回调、CN 签到 |
| `docs/TRAE_MIGRATION.md` | §15/§16 更新，新增 §18 实现记录 |

未修改：CodeArts、Qoder、WorkBuddy 的行为；ZCode 未引入。

### 39.2 认证生命周期

- 授权：`GET https://www.trae.cn/authorization`，参数含 `client_id`（公开常量）、
  `auth_callback_url`、`login_trace_id`（machine+device 后 16 位）与运维提供的
  `machine_id`/`device_id`。Intl 登录 `Unsupported`。
- 设备身份：**不生成**；start 要求两者均为 32 位十六进制，否则 400。设备值随
  flow 加密（AAD 绑定 flow id），成功后写入凭据。
- 回调：带 `refreshToken` 的公开 GET 只返回提示页，不完成授权、不回显 token；
  由已认证面板 `POST .../{flow_id}/complete` 提交完整 URL。`loginTraceID` 不匹配
  时保持 pending。
- 刷新：`ExchangeToken`；提前 300 秒；60 秒跨进程租约；CAS 写回；冲突时加载赢家
  凭据而不重复刷新；写失败为 `Storage`；401/403 拒绝为 `AuthInvalid`。
- 聊天/额度遇 `AuthExpired` 强制刷新一次并重试；第二次失败终止。

### 39.3 额度

- CN：`ide_user_ent_usage`（`{}`），`user_entitlement_pack_list` 汇总，
  `raw.period = per_package` + `expiry_ladder`；响应自带 `usage_summary` 时优先。
- Intl：`ide_user_pay_status`，只报订阅计划；数字全部 `null`/`Unknown`，
  `raw.period = subscription`。
- 单位保持 credits；无包保持 `Unknown`（从不写 0）。

### 39.4 签到（CN，默认不领取）

- 先查 `checkin_credits/status`；仅 `checkin.claim_enabled = true` 时 claim。
- 设备号必须是运维提供的 16 位数字；缺失/非法时不发请求（`Unsupported`/400）。
- 成功判定：回查 `checked_in` 且 credits 增加；`9095` 已领取、`9074` 设备/参与上限。
- 同一账号/自然日由 `CheckinService` 去重，不重复领取。

### 39.5 管理台与 UI

- 新增 TRAE 子导航与授权弹窗：machineId/deviceId 输入（前端校验 hex32）、
  回调链接输入与“完成授权”；Intl 站点在登录弹窗禁用。
- 签到按钮对 TRAE/WorkBuddy Intl 账号隐藏；账号详情不展示 token。
- 设置页接入状态文案更新：TRAE/WorkBuddy 为已接入（MockVerified），
  CodeBuddy 仍待接入。
- 粘贴失败提示使用独立 `#flow-paste-hint` 节点，避免覆盖输入框和“完成授权”按钮
  （浏览器 smoke 中发现并修复）。

### 39.6 验证

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

- 新增 45 个测试（552 → 597），全部离线（Mock 只监听 `127.0.0.1`）。
- 管理台浏览器 smoke（本地 Chrome + 临时配置/数据库）：首次初始化 → TRAE 子导航 →
  授权弹窗（Intl 禁用、设备字段）→ 非法 hex 被前端拦截 → 合法 hex 创建流程并显示
  粘贴区 → 空链接提交只更新提示、粘贴区保留 → 公开回调页不完成授权、不回显 token、
  账号数保持 0。**未**完成真实 TRAE 授权（无账号，未访问上游）。
- 覆盖：授权流（缺设备 ID、hex32、Intl、粘贴完成、token 不外泄、trace 不匹配、
  单活动流、取消）、刷新（轮换、CAS 冲突、租约拒绝、写失败、401 重试、
  有效凭据不刷新、拒绝刷新→Invalid）、额度（CN 已知/未知、Intl 计划）、
  CN 签到（成功、已签到、设备号缺失/非数字、未开启领取、额度未增加、错误码、
  Intl 零请求、传输失败）。

### 39.7 状态

| 项 | 状态 |
| --- | --- |
| 回调授权 / 设备 ID 校验 / 单活动流 | MockVerified |
| ExchangeToken 刷新 / 轮换 / CAS / 租约 | MockVerified |
| 401 单次重试 / 认证状态映射 | MockVerified |
| 额度（CN / Intl） | MockVerified |
| 签到（CN，默认不领取） | MockVerified |
| 管理台授权入口 / Web UI | MockVerified + 浏览器 smoke（Authorization 完成步骤需真实账号，未执行） |
| 真实上游（任何能力） | **未验证**：无账号、未发真实请求 |

### 39.8 边界与后续

- 真实 TRAE 账号仍需凭据与显式授权；额度/签到/刷新全部保持 `experimental`。
- Intl 登录与签到无协议证据，继续 `Unsupported`。
- 上游对“诚实非 IDE 头 + 运维设备 ID”的接受度未知，被拒时记录 CompatibilityGap。
- 本阶段到此为止，不进入 M14-F；ZCode 不重新引入。

## 40. M15 — 完整开发任务：回归审计、模型注册表、调度、管理台、三接口与部署（2026-10-29，完成）

### 40.1 审计结论（阶段 1）

对现有四个 Provider 逐项核对，结论分五级：Implemented / MockVerified / LiveVerified /
Unsupported / CompatibilityGap。**没有因为本阶段任务而重写已完成代码**；唯一的行为改动是
把 Chat 请求收敛到通用 `WireRequest` 链路，并修复下面列出的真实缺陷。

| Provider | 授权 | 模型发现 | Chat/SSE | 工具 | 刷新 | 额度 | 签到 | 结论 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| CodeArts CN | LiveVerified | LiveVerified | LiveVerified | LiveVerified | Implemented，未实测 | LiveVerified | Implemented（`already_checked_in` 实测） | 继续 `experimental` |
| Qoder CN | LiveVerified | LiveVerified | LiveVerified | LiveVerified | Implemented，未实测（等待自然过期） | LiveVerified | LiveVerified（领取 100 credits） | 继续 `experimental` |
| WorkBuddy CN/Intl | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified（CN） | 无真实账号 |
| TRAE CN/Intl | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified | MockVerified（CN） | 无真实账号 |
| CodeBuddy | — | — | — | — | — | — | — | **已移除**（2026-10-29，枚举/UI/文档范围一并移除） |
| ZCode / OpenCode Zen | — | — | — | — | — | — | — | 明确排除 |

审计发现的缺口（均已在本阶段处理或如实记录）：

1. 只有 `/v1/chat/completions`，没有 Responses / Anthropic 接口 → §40.6。
2. 模型只有启动时静态目录，没有别名、多目标、优先级 → §40.3。
3. `Scheduler` 未接入 `SessionRouter`，账号永远选第一个 → §40.4。
4. 没有请求日志/保留策略，管理台也看不到成功率与最近错误 → §40.5。
5. 无 README、部署单元、Dockerfile、VPS 文档 → §40.7。
6. 迁移 `0007/0008` 缺少 `schema_version` 行；存储层 `AVG()` 空表返回 INTEGER 导致
   dashboard 500 → 已在 §40.8 修复。
7. Chat 成功路径此前只写 usage 事件、不写请求日志；解析失败无日志 → 已补。

### 40.2 阶段 1：跨 Provider 集成回归

未新增重复实现，直接复用并补充既有离线测试：

| 回归点 | 证据 |
| --- | --- |
| 路由隔离（Provider/Region 不串号） | `src/session_service/tests.rs` 的账号选择与改绑测试；`src/admin/tests.rs::cn_and_intl_authorizations_stay_isolated` |
| CN/Intl 凭据隔离 | `AccountService` 按 `(kind, region)` 查询；授权测试同上 |
| 模型目录不被覆盖、同名去重 | `src/api/model_routes_tests.rs`：同名原生 ID 返回 400 并提示 `provider::model` |
| SSE 开始/结束/`[DONE]` | `src/api/tests.rs` 流式测试；`wire_tests.rs` 两套新接口事件序列 |
| 流截断（无终止事件） | `SseState::end` / `ResponsesStream::end` / `AnthropicStream::end` 均输出错误帧 |
| 工具参数分片 | `src/api/tests.rs` 多工具交错分片；`wire_tests.rs` function_call/output |
| 错误不映射为成功 | `src/error/mod.rs` 表驱动测试；`wire_tests.rs` 404/400 分支 |
| 客户端取消释放资源 | `src/api/session_tests.rs` 断开 SSE body 后立即释放租约 |
| 刷新不覆盖已更新凭据 | `src/account_service/tests.rs` 修订号 CAS / 单写者 |
| 签到不重复领取 | `src/checkin/tests.rs` 去重；Qoder/TRAE/WorkBuddy 执行器测试 |
| 额度刷新不误禁用账号 | 额度失败只保留 `unknown`，不写 0；scheduler 仅在 `remaining <= 0` 时排除 |
| SQLite 并发 | `src/storage/tests.rs` 并发会话/刷新租约/凭据更新 |
| Provider 故障隔离 | 冷却仅影响失败账号；`repeated_transient_failures_cool_an_account_down` |

### 40.3 阶段 2：统一模型注册表与别名

新增：

| 文件 | 内容 |
| --- | --- |
| `migrations/0007_model_routes.sql` | `model_routes` 表、唯一目标索引、`schema_version = 7` |
| `src/storage/model_routes.rs` | `ModelRouteRow` / `NewModelRoute` / `ModelRoutePatch` / `ModelRouteRepo` |
| `src/model_service/mod.rs` | 解析、别名校验、能力覆盖、CRUD、`AliasView`/`TargetView` |
| `src/api/model_routes_tests.rs` | 6 个路由/歧义/能力/列表测试 |

规则：

- 别名 1–64 字符 `[A-Za-z0-9._-/]`，不能含 `::`，不能与原生 ID 重名。
- 同名上游模型来自多个 Provider 时返回 `400` 并提示 `provider::model` 或建别名；
  `provider::model` 仍可直接调用。
- 目标按 `priority` 降序、`weight` 参与调度；全部禁用 → `model_unavailable`。
- 能力覆盖（streaming/tools/vision/reasoning）优先于 Provider 声明；缺少所需能力
  直接 `400`，不会把工具/视觉请求发给未声明目标。
- 无递归解析；别名目标必须是原生上游模型。
- 管理 API：`GET/POST /admin/api/model-routes`、
  `PATCH/DELETE /admin/api/model-routes/{id}`；管理台“模型管理”页可新建别名、
  编辑优先级/权重/能力、启停与删除单个目标。

### 40.4 阶段 3：多账号调度与故障切换

新增 `src/scheduler/mod.rs`：

- 策略：`round_robin`、`smooth_weighted_round_robin`（默认）、`least_connections`。
- 过滤：已知额度耗尽（`remaining <= 0`）或低于 `quota_min_ratio` 的账号排除；
  **未知额度永不排除**；冷却中的账号跳过。单位不混算（只比较同一快照的 remaining）。
- 冷却：仅 `Transport` / `Timeout` / `UpstreamServer` 计数，指数退避到
  `cooldown_max_secs`；客户端错误、402、429、认证失败不冷却。
- 状态全部在进程内、按账号有界；`parking_lot::Mutex`，无异步大锁、无后台任务。
- `SessionRouter::with_scheduler` 接入：选择账号前走调度器，`open_excluding`
  支持故障切换，`RouteLease::finish` 把失败类别回写给调度器。
- 重试语义（`src/api/mod.rs::dispatch`）：仅认证失败、账号禁用、渠道受限、429、
  402、本地无容量会切换账号/目标；传输错误、超时、5xx 视为结果不确定，**绝不重发**；
  流式一旦开始传输，错误只以流内错误帧结束。失败账号会被排除；没有其他账号时返回
  最初的业务错误（例如 402），不再覆盖成 503。
- 修复：会话冷却测试此前假设首次一定选中 alpha（轮询按 UUID 排序），改为记录实际
  选中的账号；调度配额测试补齐真实账号行以满足外键。

测试：`src/scheduler/mod.rs` 9 个；`src/session_service/tests.rs` 新增 5 个
（轮询分散、粘性优先、改绑不复用上游 id、额度排除、冷却）。

### 40.5 阶段 4：管理台

- 仪表盘：24h 成功率、平均延迟、调度策略、最近错误（只读元数据）。
- 模型管理：别名/目标 CRUD、优先级/权重/能力编辑、启停、删除；原生模型目录。
- 请求日志页：时间、Provider/账号内部 ID、别名与上游模型、API、流式、HTTP 状态、
  错误类别、延迟、token；保留天数清理与清空。
- 新增 `src/storage/logs.rs` + `migrations/0008_request_logs.sql`：
  `NewRequestLog` / `RequestLog` / `RequestLogRepo`（record/list_recent/summary_since/
  list_failures/purge_before/delete_all）。只存元数据，提示词/消息/工具参数/凭据不入表。
- `src/api/logging.rs`：每次尝试生成 `LogContext`，成功、流结束、最终失败各写一条；
  流式 usage 在 EOF 时落库。
- 管理 API：`GET /admin/api/logs`、`POST /admin/api/logs/purge`、
  `GET /admin/api/scheduler`。
- 未向浏览器返回任何明文凭据；日志页只展示账号内部 UUID 前 8 位。

### 40.6 阶段 5：Responses 与 Anthropic API

`src/api/wire.rs` 定义 `WireRequest`（model/streaming/validate/route_needs/api_name/
first_user_text/to_chat_request/translator/render），`src/api/sse.rs` 增加
`SseTranslator` 与通用 `to_sse`。三个接口共用 `handle_wire → resolve_model → dispatch`。

**`POST /v1/responses`**（`src/api/responses.rs`）：

- `input` 字符串或 item 数组（message / function_call / function_call_output）；
  `instructions`、扁平 function tools、`max_output_tokens`、temperature/top_p。
- 非流式返回 `object=response`、`output[]`（message/function_call）、`output_text`、
  `usage`；长度截断/内容过滤返回 `status=incomplete` + `incomplete_details`。
- 流式事件：`response.created`、`output_item.added`、`content_part.added`、
  `output_text.delta/done`、`content_part.done`、`output_item.done`、
  `function_call_arguments.delta/done`、`response.completed` / `response.failed`。
- 明确拒绝：`previous_response_id`、`store: true`、`parallel_tool_calls: false`、
  `strict: true`、非 function 工具、未知字段与非文本/图片部件。

**`POST /v1/messages`**（`src/api/anthropic.rs`）：

- system 字符串/文本块、user/assistant 消息、text/image/tool_use/tool_result、
  Anthropic tools（`input_schema`）、tool_choice（auto/any/tool/none）、stop_sequences。
- 非流式返回 Anthropic message；stop_reason 映射 end_turn/max_tokens/tool_use/refusal；
  tool_use 的 `input` 从内部参数字符串解析，非法 JSON 返回上游错误。
- 流式事件：`message_start`、`content_block_start`、`content_block_delta`
  （`text_delta` / `input_json_delta`）、`content_block_stop`、`message_delta`、
  `message_stop`；错误使用 Anthropic `error` 事件与错误体。
- 明确拒绝：`top_k`、thinking/redacted_thinking、非 user/assistant 角色、未知字段。

CompatibilityGap（如实记录）：

- Anthropic `message_start.usage.input_tokens` 固定 0——上游 usage 只在流末尾到达，
  协议没有后续回填位置；最终 output_tokens 与完整 usage 仍正确落库。
- 推理/thinking 内容在 Responses 与 Anthropic 输出中不重放、不改名成文本。
- Anthropic 无法识别的上游 stop reason 按 `end_turn` 输出。
- 同一 user turn 内文本与 tool_result 混排时，工具结果先转为内部 tool 消息。
- 图片目前只在目标声明 vision 时才进入 Provider 转换；各 Provider 的图片上行适配
  仍需逐项验证（本轮未新增图片上传实现）。

测试：`src/api/wire_tests.rs` 12 个（两个接口的文本/流式/工具/工具结果/鉴权/
404 错误体/top_k 拒绝/vision 闸门）。

### 40.7 阶段 6：部署、性能与文档

新增：

| 文件 | 内容 |
| --- | --- |
| `README.md` | 能力矩阵、快速开始、配置、安全默认值、文档索引 |
| `docs/API.md` | 三种 API、SSE 事件、错误码、重试语义、请求日志 |
| `docs/ROUTING.md` | 名称解析、能力校验、多目标、调度、会话粘性、故障切换 |
| `docs/DEPLOYMENT.md` | Linux/ARM64 构建、目录、systemd、Docker、反代、环境变量、迁移、备份、性能基线、排障 |
| `deploy/polygate.service` | hardened systemd 单元 |
| `deploy/Dockerfile` | 多阶段构建、非 root、数据卷 |
| `deploy/container-config.toml` | 容器内配置样例（不含密钥） |

未做未经验证的“优化”：现有实现已是真实 SSE 流式、共享 `reqwest::Client` 连接池、
SQLite 连接池与应用内嵌静态资源；本轮只修复了有证据的问题（空表 AVG 类型、
请求日志缺失、迁移版本行缺失）。未引入 Node 运行时。

### 40.8 验证（实际命令与输出）

```text
$ cargo fmt --all -- --check
（无输出，exit 0）

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.43s
（0 warnings）

$ cargo test --workspace
running 629 tests
test result: ok. 629 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.07s

$ cargo build --release
Finished `release` profile [optimized] target(s) in 1m 47s
target/release/polygate.exe = 11,820,032 bytes
```

Release 二进制 smoke（Mock Provider，隔离配置 `target/smoke/config.toml`，
`127.0.0.1:18787`，`admin.enabled=false`）：

```text
POST /v1/responses  {"model":"mock-chat","input":"hello"}
→ 200，object=response，status=completed，output_text="mock: hello"，
  usage={input_tokens:2,output_tokens:3,total_tokens:5}，0.004 s

POST /v1/responses  stream=true
→ event: response.created → output_item.added → content_part.added →
  output_text.delta("mock: hi") → … → response.completed

POST /v1/messages   {"model":"mock-chat","max_tokens":32,...}
→ 200，type=message，content[0].type=text，stop_reason=end_turn，
  usage={input_tokens:3,output_tokens:3}，0.0015 s

POST /v1/messages   stream=true
→ message_start → content_block_start → content_block_delta(text_delta) →
  content_block_stop → message_delta(stop_reason=end_turn, output_tokens=2) → message_stop

POST /v1/messages   未知模型
→ 404 {"type":"error","error":{"type":"not_found_error",...}}
```

性能（2026-10-29，Windows 笔记本，release，Mock 上游，16 连接 × 100 请求，
每请求写 request_logs + usage_events，请求体约 1 KB）：

```text
requests=1600 elapsed=0.852s rps=1878
avg=7.27ms p50=4.13ms p95=21.06ms max=284.98ms
工作集 16.7 MB / 私有内存 7.2 MB（压测后）
```

该结果只衡量网关自身开销，**不是**真实上游或 VPS 的容量承诺；未伪造任何基准。

期间发现并修复的真实缺陷：

1. `storage::tests::migration_is_idempotent`：新增迁移后 schema_version 仍为 6；
   为 `0007/0008` 补 `INSERT INTO schema_version`，期望更新为 8，表清单补
   `model_routes`/`request_logs`。
2. 调度器额度测试用虚构账号 ID 写 `quota_snapshots` 触发外键 → 改为先持久化账号。
3. 会话冷却测试依赖轮询初始顺序 → 改为断言“失败账号不再被选中”。
4. `/v1/chat/completions` 402 被后续 `NoCapacity` 覆盖成 503 → 调度器打开失败时
   保留最初的业务错误。
5. 空表 `AVG(latency_ms)` 返回 INTEGER 导致 dashboard 500 → `COALESCE(..., 0.0)`，
   并新增空表 summary 测试。
6. `model_service::create_target` 参数过多（clippy）→ 引入 `NewTarget` 结构体。

### 40.9 未完成 / Unsupported / CompatibilityGap

| 项 | 状态 |
| --- | --- |
| TRAE / WorkBuddy 真实上游 | **未验证**（无账号，未发请求） |
| CodeArts / Qoder 刷新、Intl、多账号并发 | **未验证** |
| Qoder/TRAE/WorkBuddy 多账号实测 | 调度逻辑有 Mock 测试，真实多账号未验证 |
| CodeBuddy | 已移除（不再注册枚举或 UI） |
| ZCode / OpenCode Zen | 明确排除 |
| Responses `previous_response_id` / 服务端存储 | Unsupported（400） |
| Responses 严格 JSON Schema (`strict`) | Unsupported（400） |
| Anthropic `top_k` / extended thinking | Unsupported（400） |
| Anthropic 流式 input_tokens | CompatibilityGap（固定 0） |
| 推理内容在 Responses/Anthropic 输出 | CompatibilityGap（不回放） |
| 未知 stop reason | CompatibilityGap（end_turn） |
| 图片实际转发到各 Provider | 能力闸门已实现；真实图片链路未逐 Provider 验证 |
| `LICENSE` 与发布 | 未完成（来源清点待办） |
| 管理台新页面的浏览器回归 | 见 §40.10 |

### 40.10 边界

- 本阶段没有新增 CodeBuddy Provider、没有接入 ZCode/OpenCode Zen、没有伪造官方
  客户端身份或设备指纹、没有修改任何安全校验提示词、没有复制不兼容许可证代码。
- 新增依赖：无。
- 已发布迁移 `0001`–`0006` 未改动；`0007`/`0008` 为本阶段新增，补齐了
  `schema_version` 行。
- 管理台新页面已完成浏览器 smoke（本地 Chrome + 隔离实例 `127.0.0.1:18788`，
  临时数据库）：首次初始化 → 恢复码 → 仪表盘新卡片（24h 成功率/平均延迟/策略/
  最近错误）→ 模型管理新建别名 `smoke-alias` → 通过别名发起 `/v1/responses` 与
  一次 404 → 请求日志页显示 200/404 两条元数据（无提示词）→ 编辑权重 W1→W5 →
  删除目标（confirm 对话框）。未验证的部分：真实 Provider 账号页面与写操作之外
  的浏览器兼容性。

## 41. UI 视觉重设计 — 深色开发者控制台（2026-10-29，完成）

### 41.1 范围

只改 Design Tokens、CSS、组件样式、排版、间距与响应式；**页面结构、路由、业务逻辑、
API 与交互全部保持不变**。没有改动任何 `.rs` 文件；`web/index.html` 未改；`web/app.js`
仅新增两个纯展示类（三列统计网格、紧凑数值、能力字段两列布局）。

| 文件 | 变更 |
| --- | --- |
| `web/app.css` | 全量重写（340 行 → 约 650 行）：令牌、基础组件、表格、表单、模态、Toast、OAuth 页、响应式 |
| `web/app.js` | `statCard` 增加可选紧凑样式；仪表盘第二行统计使用三列网格；能力下拉包进 `.cap-grid` |

### 41.2 设计令牌（深色）

```text
bg #15171b · surface #1c1f24 · surface-hover #252a30 · sidebar #181a1f
border #30353d · border-strong #3b424c
text #e8edf2 / #9ca8b7 / #697586
accent #5bc4b5 · hover #76d8ca · on-accent #0d1917
success #59bd91 · warning #d5a65a · danger #d97575
radius 6 / 8 / 10
```

浅色主题保留并映射为同一语义令牌（背景 #eceff1、卡片 #fff、accent #2b9b8c），
`切换主题` 功能与 `localStorage` 行为不变。

### 41.3 组件

- 按钮：高度 30/26，`border-radius: 999px`，主按钮青绿底深色字；ghost/danger 低饱和。
- 输入/Select：高 30–32，圆角 6，Focus 为青绿 1px + 2px 柔和外圈，无夸张动画。
- Tab（`.segmented`）：2px 内衬、24px 高、无位移；子导航与侧栏使用同一选中语义。
- Badge：19px 高、胶囊、5px 状态点；ok/warn/bad 使用低饱和描边与 10% 底色。
- 表格：表头 10px 大写字距、行高约 34–48、行 Hover 轻微提亮、表头 sticky。
- Switch：新增 `.switch` 设计原语（当前页面未使用，供后续开关复用）。
- 卡片：内边距 13/14，圆角 8，统计数值 20px/600；文本型统计使用 15px 紧凑样式。
- 模态：圆角 10、内边距 16、按钮区顶部 1px 分隔；能力覆盖改为两列，降低高度。
- 空状态：卡片内不再套虚线框；页面级空状态保留低对比虚线。
- 滚动条：8px 细样式；`prefers-reduced-motion` 下关闭过渡。

### 41.4 响应式

- ≤1100：统计卡片 2 列；
- ≤880：侧栏转为顶部横向导航，主区单列；
- ≤620：统计卡片 2 列、策略卡横跨两列、表单/键值单列；
- ≤360：统计卡片回落 1 列。

### 41.5 验证

- `node --check web/app.js` 通过；未修改任何 Rust 源码。
- 浏览器 QA（本地 Chrome，隔离实例 18790，debug 直接从磁盘加载静态资源）：
  - 1440×900 深色：仪表盘、模型管理、请求日志、账号管理、系统设置、新建别名模态。
  - 浅色主题：登录页与模态对比度检查（背景/卡片/边框/输入）。
  - 390×844 移动端：顶部横向导航、2 列统计、表格横向滚动。
  - 交互：主题切换、侧栏选中态、Focus 青绿描边、模态两列能力字段、Toast。
- 视觉方向：dsh-wtq-pool 式紧凑深色控制台；未复制其页面结构，未删除任何功能。

## 42. 移除 CodeBuddy（2026-10-29，完成）

### 42.1 背景

CodeBuddy 从未实现（无 adapter、无路由、无授权流程），仅残留枚举、管理台“待接入”
占位与“计划接入”的文档表述。操作者要求移除，按 ZCode 相同方式处理：代码/UI 删除，
历史研究文档保留但加“已移除、不得据此实现”横幅。

### 42.2 变更

| 文件 | 变更 |
| --- | --- |
| `src/config/mod.rs` | 删除 `ProviderKind::Codebuddy` 及 `as_str`/`from_db_str`/`display_name` 分支 |
| `src/runtime/mod.rs` | 删除“未实现 kind”兜底分支（已无未实现枚举） |
| `web/index.html` | 删除侧栏 `CodeBuddy 待接入` 子导航 |
| `web/app.js` | 设置页“接入状态”删除 CodeBuddy 文案 |
| `src/runtime/tests.rs` | 原“未实现 Provider 拒绝启动”测试改为“配置加载拒绝 `kind = "codebuddy"`” |
| `src/session_service/tests.rs` | 顺带修复依赖随机 UUID 轮询顺序的 flaky 断言 |
| `README.md`、`docs/ARCHITECTURE.md`、`docs/PROVIDERS.md`、`docs/MIGRATION_MATRIX.md` | 能力矩阵/范围改为“已移除”；取消 M15 CodeBuddy 里程碑 |
| `docs/PROVIDER_MIGRATION.md`、`docs/CHECKIN_RESEARCH.md`、`docs/RESEARCH.md` | 顶部横幅：历史研究、禁止实现 |
| `PROGRESS.md` | 当前范围、Web UI、M15 审计表同步 |

未修改：WorkBuddy 的 `codebuddy.cn` / `codebuddy.ai` 域名识别与计费地址——那是
WorkBuddy CN/Intl 的真实协议事实，不是 CodeBuddy Provider。

### 42.3 兼容性

- `data/polygate.db` 实测账号 kind 只有 `codearts`、`qoder`，无 `codebuddy` 行，删除枚举
  不会使现有数据无法解码。
- 旧配置写 `kind = "codebuddy"` 现在在配置加载阶段报 `Config` 错误（unknown variant），
  无兼容 shim——与 ZCode 的处理一致。
- 新增依赖：无。已发布迁移未改。

### 42.4 验证

```text
$ cargo fmt --all -- --check
（exit 0）
$ cargo clippy --workspace --all-targets -- -D warnings
（0 warnings）
$ cargo test --workspace
test result: ok. 629 passed; 0 failed; 0 ignored
$ cargo build --release
（成功）
```

管理台侧栏实测只显示 CodeArts / Qoder / TRAE / WorkBuddy；`GET /v1/models` 不含
CodeBuddy 条目；设置页不再出现“待接入”字样。

## 43. 客户端身份伪装移植 — `client.identity`（2026-10-29，完成）

### 43.1 背景

此前三个 Provider 一律发 `polygate/<version>` 与最小协议头，属“诚实客户端”。上游会核对
官方桌面端的头族：WorkBuddy Intl 缺会话标识直接被业务码 `11128` 拒绝；Qoder 的 cosy 侧
按 `machineid`/`machinetoken` 归类调用方；TRAE 按 `X-App-Id`/`X-Ide-Version` 判定 IDE。

按 AGENTS.md §2 的口径，这些值都是公开客户端常量与按 uid 可复算的派生值，不做设备指纹伪造。
参考来源只有协议事实：`.refs/RobbsLuo_Coding2API`（MIT）、`.refs/chengzhi-c_dsh-wtq-pool/src/upstream.ts`、
本机 `workbuddy2api` 已实测的抓包结论；无第三方代码复制。

默认仍是 honest，旧行为与本字段缺省时完全不变，伪装必须显式打开：

```toml
[providers.options]
"client.identity" = "impersonate"   # honest（默认）| impersonate
```

### 43.2 变更

| 文件 | 变更 |
| --- | --- |
| `src/providers/profile.rs` | **新增**：`Identity` 开关、三 Provider 的版本/UA/头族构造函数、按 uid 的稳定派生（md5→hex 机器码、sha512→base64url token、18 位机器型）、`apply()` 空值跳过；5 个单测 |
| `src/providers/mod.rs` | 声明 `pub mod profile;` |
| `src/providers/workbuddy/http.rs` | `Call` 分类（Chat/Json/Billing/Refresh），按类别与 region 组合 UA 与头族；`x-requested-with`/`x-product` 门头、会话/根/消息 id 成对、`traceparent` 内嵌同一 trace id；`X-Agent-Runtime` 仅 CN 发送 |
| `src/providers/workbuddy/auth.rs` | 刷新请求 UA 改走 `user_agent(region)` |
| `src/providers/workbuddy/provider.rs`、`src/checkin/workbuddy.rs` | `with_identity`；`with_timeouts` 保持开关不变 |
| `src/providers/qoder/cosy.rs` | 伪装时追加 `cosy-clienttype/machinetype/machineid/machinetoken/version/data-policy`；签名四头不变 |
| `src/providers/qoder/{gateway,http}.rs`、`provider.rs`、`src/checkin/qoder.rs` | 同上接入；openapi 侧同样补 `cosy-*` 设备族与 `login-version`，两侧 `cosy-machineid` 由同一 uid 派生 |
| `src/providers/trae/{http,provider}.rs`、`src/checkin/trae.rs` | 伪装时追加 `X-App-Id`/`X-App-Version`/`X-Ide-Version*`/`X-Device-Type`/`X-OS-Version`/`Request-Traffic-Type` 与 `Trae/<ver>` UA；**设备 ID 仍不生成**，账号已导入则沿用 |
| `src/runtime/mod.rs` | `fn identity(provider)` 读取并校验 `client.identity`，对 3 个 Provider 与 3 个签到执行器统一注入 |
| 测试 | `profile.rs` 5、`workbuddy/tests.rs` 4、`qoder/tests/inference.rs` 3、`trae/tests.rs` 3、`runtime/tests.rs` 2 |
| `config.example.toml`、`docs/PROVIDERS.md` §10.3、`docs/WORKBUDDY_MIGRATION.md` §3.2、`docs/TRAE_MIGRATION.md` §3.2/§13、`docs/QODER_PROVIDER_RESEARCH.md` §6.1、三个 `mod.rs` 模块注释 | 开关说明、头族表、Intl 必须伪装的结论 |

### 43.3 边界

- WorkBuddy、TRAE **未对真实上游发起过请求**，仍为 MockVerified；Qoder 已做真实验证（见 §43.5），
  CodeArts 未改动。
- Qoder 签名只覆盖 `cosy-user`/`version`/`data-policy`/`timestamp` 四头，新增头均不参与签名，
  CN 已 Live 验证的签名路径不受影响（有测试断言签名仍可校验）。
- 会话 id 每次请求新生成；只有 `machineid`/`machinetoken`/机器型按 uid 稳定，便于上游归类。
- WorkBuddy 会话头只跟 Chat 走，额度/计费/刷新不带，避免把一次性 id 混进非会话请求。
- TRAE 的 `X-Device-Id`/`X-Machine-Id` 仍取账号已存值，伪装不覆盖、不补造。
- 未知取值在配置加载阶段报 `Config` 错误并提示两个合法值，无兼容 shim。

### 43.4 兼容性

- 缺省或 `honest`：UA 回到 `polygate/<version>`，不发任何新增头——§43 之前的 629 个测试
  全部原样通过即为证据。
- 新增依赖：无（`md-5`、`sha2`、`base64`、`hex` 已在依赖表中）。
- 已发布迁移与数据库结构未改；开关只存在于既有 `provider.options` JSON。

### 43.5 验证

```text
$ cargo fmt --all -- --check
（exit 0）
$ cargo clippy --workspace --all-targets -- -D warnings
（0 warnings）
$ cargo test --workspace
test result: ok. 646 passed; 0 failed; 0 ignored
$ cargo build --release
（成功；首次失败因 8787 上跑着旧 exe 无法覆盖，停进程后通过）
```

**Qoder CN 真实对照**（本机 release/debug 构建 + 已 Live 验证的 CN 账号，
`POST /v1/chat/completions`，`model=auto`，非流式）：

| 模式 | 结果 |
| --- | --- |
| honest（基线） | 200，`content: "pong"` |
| `client.identity = "impersonate"` | 200，`content: "pong"`；新增 `cosy-clienttype/machinetype/machineid/machinetoken/clientip/data-policy` 与 `login-version` 后签名仍被接受 |
| 回滚配置为 honest 后重启 | 200，`content: "pong"` |

即 Qoder 的伪装头**不会**打断已 Live 验证的签名链路，开关可随时回退。

未验证项：

- WorkBuddy、TRAE 伪装模式未真实联调（两家均无真实账号），状态仍为 MockVerified；
  WorkBuddy Intl 是否因伪装而不再返回 `11128` 需真实账号确认。
- Qoder 只测了 CN + `auto` 单模型单轮；`dfmodel`（DeepSeek-V4-Flash 之上游节点）
  在 honest 下即返回 `[FAIL]node:oa_qwen-plus-main`，与本次改动无关。


## 44. 仓库发布准备 — README、LICENSE、工作区清理与 GitHub 仓库（2026-10-29，完成）

### 44.1 变更

| 文件 | 变更 |
| --- | --- |
| `README.md` | 重写：徽章、能力分组、Provider 状态表（MockVerified ≠ LiveVerified）、快速开始、架构图、实测性能、「运行时占用」（release 空闲 14.6 MB / 负载 15.5–16.7 MB，说明选纯 Rust 是为低内存设备）、「参考项目」（Coding2API、dsh-wtq-pool 两个 MIT 仓库地址与各自参考范围）、部署入口、安全默认值、文档索引、许可证 |
| `LICENSE` | **新增** MIT（Copyright 2026 OmaruKoma） |
| `Cargo.toml` | `repository` 由占位 `OWNER/polygate` 改为 `OmaruKoma/polygate` |
| `AGENTS.md` §1 | 许可证段落更新为“已生成 LICENSE + 依赖清点结论” |
| 工作区 | 删除误建的空目录 `NVIDIA Corporation/`、`data/pgtest.exe`、`data/dns_*.txt`、过期 `data/serve_*.log`；`data/`、`target/`、`.refs/` 仍为忽略项 |

### 44.2 依赖许可证清点

`cargo metadata` 全量统计（232 个包）：MIT OR Apache-2.0 类 196、MIT 54、Apache-2.0 OR MIT 36、
Unicode-3.0 18、Unlicense OR MIT 5、BSD-3-Clause 3、Zlib 3、ISC 2、CDLA-Permissive-2.0 2 等，
全部为宽松许可；唯一含 LGPL 的可选项为 `r-efi 6.0.0`（`MIT OR Apache-2.0 OR LGPL-2.1-or-later`，
取 MIT/Apache 分支即可）。无 GPL/AGPL 强制项。

### 44.3 仓库

- 首次提交并推送至 `github.com/OmaruKoma/polygate`，初始为 private，随后按操作者要求转为 **public**
  （公开前的敏感信息复核见 §45）。
- `.refs/`（第三方参考源码）、`data/`（数据库、主密钥、API Key）、`target/` 均不入库；
  提交前做过 `sk-` / `gho_` / `AKIA` 模式扫描，命中项仅在忽略目录 `.refs/` 的第三方测试样本中。

### 44.4 状态

本阶段不改变任何运行时行为；§43 结束时的 `cargo test --workspace`（646 passed）仍为当前基线。
README 中的性能与测试数字均引用 §40 / §43 的实测输出。

## 45. 公开仓库敏感信息审计（2026-10-29，完成）

### 45.1 触发

仓库转为 public 后复核历史文档，发现 `PROGRESS.md`、`docs/RESEARCH.md` 等把本机绝对路径
（Windows 用户目录与工程盘路径）直接写进正文，公开后等同于泄漏用户名与目录结构。

### 45.2 审计范围与方法

对 `git ls-files` 全量文件扫描，模式覆盖：Windows 盘符路径、POSIX 形式挂载路径、用户名片段、
邮箱、私有网段 IP、UUID、`Authorization` 头、`sk-` / `gho_` / `AKIA` 密钥前缀。

| 类别 | 命中 | 处置 |
| --- | --- | --- |
| Windows 绝对路径 | 47 处 / 7 个文件 | 替换为 `~/<project>`、`<repo>`、`<local>/<dir>` 占位形式 |
| POSIX 形式本地路径 | 1 处（用户目录挂载写法） | 同上 |
| 用户名 / 主目录 | 随路径一并替换，全部消除 | — |
| 邮箱 / 私有 IP / Bearer 头 | 0 | — |
| UUID | 6 处，均为上游公开 client_id / app_id（qoder、trae） | 保留 |
| 密钥前缀（`sk-` / `gho_` / `AKIA`） | 0（`.refs/` 第三方测试样本除外，该目录不入库） | — |

### 45.3 变更

- `PROGRESS.md`、`docs/RESEARCH.md`、`docs/CODEARTS_SESSION.md`、
  `docs/CODEARTS_LIVE_VERIFICATION.md`、`docs/MIGRATION_MATRIX.md`、`docs/WEB_ADMIN.md`、
  `docs/WORKBUDDY_MIGRATION.md`：路径脱敏。
- 修正替换产生的排版问题（混用分隔符、重复占位符、指向父目录的孤立 `~`）。
- `docs/MIGRATION_MATRIX.md` 的 PolyGate 行由“尚未生成 LICENSE”更新为 MIT（§44 已落地）。
- **重写 Git 历史**：原两个提交的 blob 中也含这些路径，因此 squash 为单提交后
  `--force-with-lease` 推送；旧提交对象不再被任何 ref 引用（GitHub 侧可能仍按 SHA 暂存一段时间，
  但不可通过浏览或搜索检索）。

### 45.4 验证

```text
$ git grep -nIE "<盘符>:\\\\" -- . ':!Cargo.lock'           （无输出）
$ git grep -nI "<本机用户名片段>"                            （无输出）
$ git grep -nI "<本机工程盘目录名>"                          （无输出）
$ git grep -nIE "Bearer [A-Za-z0-9._-]{20,}"                （无输出）
```

匿名拉取 `raw.githubusercontent.com/OmaruKoma/polygate/main/PROGRESS.md` 后再次 grep，结果同上。

### 45.5 遗留与规范

- `docs/CODEARTS_SESSION.md` 保留本人 `codearts2api` 的公开远端地址与 HEAD：那是公开仓库信息，
  不属于本机信息，保留用于溯源。
- 此后文档规范：涉及本机时一律写 `~/<project>`、`<repo>`、`<local>/<path>`，不出现盘符与用户名。

## 46. aarch64 软路由构建（IPQ6000 / KWRT，2026-10-30，完成）

### 46.1 目标

给 IPQ6000（4×Cortex-A53 @1.2 GHz，OpenWrt / KWRT，musl）出一份静态二进制。
路由器没有 Rust 工具链、libc 版本不可控，因此必须是 `aarch64-unknown-linux-musl` 静态链接。

### 46.2 环境与步骤（构建机 Windows 10 x64）

| 组件 | 版本 | 说明 |
| --- | --- | --- |
| rustc / cargo | 1.98.1 | 由 `rust-toolchain.toml` 固定 |
| 目标 | `aarch64-unknown-linux-musl` | `rustup target add` |
| zig | 0.17.0 | 作为 C 交叉编译器与链接器（ring / 内置 sqlite3 需要 C 工具链） |
| cargo-zigbuild | 0.24.0 | 自动把 zig 注入 `CC`/`AR`/linker |

```bash
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

未改动任何源码：`rustls`（ring）与 `libsqlite3-sys`（bundled）都由 zig 交叉编译通过，
仓库里没有 `cfg(windows)` 之外的平台分支（`cfg(unix)` 仅用于 0600 权限设置，静态 musl 下正常）。

### 46.3 产物

```text
target/aarch64-unknown-linux-musl/release/polygate
  ELF64 LE, e_machine=183 (AArch64), ET_EXEC
  PT_INTERP: none      → 静态链接
  DT_NEEDED: none
  10,963,376 字节（10.46 MiB）；gzip -9 后 4,968,367 字节
  sha256 e288a341a51e48a8de4841b9589485f3b7e4a22734f97d89d70bc403a0d8403d
```

### 46.4 运行验证（qemu 用户态，非真机）

用 `qemu-aarch64-static 7.2.0` 在 WSL Debian 下执行同一份产物：

| 检查 | 结果 |
| --- | --- |
| `polygate --help` | 正常输出 `serve` / `doctor` / `config-example` |
| `polygate config-example` | 正常输出完整配置 |
| `serve`（mock provider，临时 data dir） | 监听成功，启动日志正常 |
| `GET /health` | `{"status":"ok","version":"0.1.0"}` |
| `GET /v1/models` | 返回 `mock-chat` / `mock-reasoner` 目录 |
| `POST /v1/chat/completions`（mock） | 200，`content: "mock: hi"`，usage 正常 |
| `GET /`（管理台） | 200，内嵌前端正常返回 |
| SQLite 迁移 0001–0008 | 在 ext4 路径下自动执行成功 |

WSL 里的 HTTP 探测用 bash `/dev/tcp` 完成（该镜像无 curl），未安装任何额外运行库。

### 46.5 交付物

| 文件 | 用途 |
| --- | --- |
| `deploy/openwrt/polygate.init` | procd 启动脚本（respawn、nofile 4096、读取 `polygate.env`、配置变更触发重启） |
| `deploy/openwrt/polygate.env.example` | `POLYGATE_API_KEY` / `POLYGATE_MASTER_KEY` 模板 |
| `docs/DEPLOYMENT.md` §12 | 传输、自启、SSH 隧道首次初始化、SQLite 落盘位置、资源预期 |

### 46.6 未验证

- **未在真实 IPQ6000 / KWRT 上运行**：本次没有该设备访问，只做了 qemu 用户态验证。
  内核版本、`/overlay` 文件系统行为、procd 生命周期需在真机确认。
- 内存数字仍是 x86_64 release 实测（§41、README「运行时占用」）的推断：A53 上预计 15–20 MB，
  未实测。
- 静态 musl 不读取系统 `openssl`，TLS 完全由 rustls 自带；若路由器有透明代理/证书替换，
  需要自行保证出网路径可用。

## 47. 软路由实机部署与 doctor 误报修复（2026-10-30，完成）

### 47.1 部署结果（10.0.0.1，Kwrt 25.12 / IPQ6000）

- 静态 aarch64 产物装到 `/usr/bin/polygate`，procd 自启（`/etc/init.d/polygate`），
  `listen = 10.0.0.1:8787`，数据目录 `/etc/polygate`（overlay 1.7 G 可用）。
- 内存实测：启动 8.5 MB RSS，400 次请求后 11.3 MB，16 条并发 SSE 峰值 13.6 MB；
  匿名页仅 1.9–6.1 MB，其余为可回收的二进制 text 页。
- 真实 CodeArts CN 账号经面板授权后，`/v1/models` 列出 10 个上游模型；
  `POST /v1/chat/completions`（`deepseek-v4.1-flash`）返回 200 与正常 usage。

### 47.2 两个配置陷阱（文档已补）

1. **provider 只从配置注册**：面板授权的账号不带 provider 实例，缺 `[[providers]]` 时
   签到/检查认证/刷新额度都返回 `provider is not registered`。已给软路由补 CodeArts CN 与
   Qoder CN 两段。
2. **`[admin] redirect_url` 默认是 `127.0.0.1`**：监听局域网地址时浏览器在别的机器上，
   回调打不开。改为 `http://10.0.0.1:8787/oauth/callback` 后，
   `GET /admin/api/bootstrap` 的 `oauth_callback_url` 同步更新（已实测）。

### 47.3 代码修复：doctor 对非 mock provider 的误报

`src/cli/mod.rs` 的 `providers_check` 仍保留早期里程碑的断言：只要启用非 mock provider
就报 “non-mock provider enabled but not implemented”。四个 provider 早已实现，该判断已删除，
现在只汇总已启用项。

### 47.4 验证

```text
$ cargo fmt --all -- --check                     （exit 0）
$ cargo clippy --workspace --all-targets -D warnings   （0 warnings）
$ cargo test --workspace                         646 passed

路由器（service 运行中）：
[ok] providers: mock-cn:mock, codearts-cn:codearts, qoder-cn:qoder
[ok] api_key: 1 key(s) configured   [ok] master_key: usable (environment, 32 bytes)
[ok] database: reachable; migrations applied
[FAIL] listen: Address in use      ← 预期，serve 正在占用端口
```

`/etc/polygate/config.toml.bak-20261030`、`.bak2-20261030` 为两次配置变更的备份。

## 48. CodeArts 远端授权：粘贴回调模式（2026-10-30，完成）

### 48.1 现象

软路由（`listen = 10.0.0.1:8787`）上把 `[admin] redirect_url` 改成局域网地址后，重新授权
失败，回调页显示 `CodeArts authorization was rejected (STS5.1805)`；而此前
`redirect_url = 127.0.0.1` 的流程成功建立了 `hsggga` 账号。

### 48.2 证据

路由器数据库 `admin_auth_flows`：

| 时间（UTC） | redirect_uri | 状态 | 错误 |
| --- | --- | --- | --- |
| 02:38:09 | `http://127.0.0.1:8787/oauth/callback` | succeeded | — |
| 02:46:31 | `http://10.0.0.1:8787/oauth/callback` | failed | `CodeArts authorization was rejected (STS5.1805)` |
| 02:47:10 | `http://10.0.0.1:8787/oauth/callback` | failed | 同上 |
| 02:49:31 | intl 流程 | pending → 过期 | — |

参考实现 `RobbsLuo_Coding2API` 的 CodeArts OAuth 固定使用
`http://127.0.0.1:{port}{redirect_path}`，并专门提供 “paste” 轨道：
「门户把 code 302 回 `127.0.0.1:{port}`（服务端不监听），用户把地址栏链接粘回来」。
即上游把 code 绑定在回环回调上，非回环 `redirect_uri` 在换取 token 时会被拒绝。

### 48.3 变更

| 文件 | 变更 |
| --- | --- |
| `src/admin/auth/flow.rs` | 新增 `complete_codearts_flow(flow_id, raw)` 与 `code_from_callback`（接受完整回调 URL 或裸 code）；把原 `complete_active_flow` 的下半段抽成 `finish_codearts_flow` |
| `src/admin/api/mod.rs` | 新增 `POST /admin/api/auth/codearts/{flow_id}/complete`（需要管理会话 + CSRF） |
| `src/admin/mod.rs` | 注册上述路由 |
| `web/app.js` | CodeArts 描述符加 `pasteCallback: true`，复用 TRAE 已有的粘贴框与 `completePasteFlow` |
| `src/providers/codearts/login.rs` | 令牌交换失败时附带上游 `error_description`（截断 160 字符、去控制字符），新增 2 个单测 |
| `src/admin/auth/flow.rs` | 授权失败写 `warn` 日志（flow id + 错误文本，不含凭据） |
| `docs/WEB_ADMIN.md` §7、`docs/DEPLOYMENT.md` §12、`config.example.toml` | 说明必须保持回环回调，局域网浏览器改用粘贴模式 |
| `src/admin/tests.rs` | 新增 2 个测试：粘贴完整 URL 与裸 code 均成功、且同一身份不重复建号；无 `code` 参数的 URL 返回 400 且流程保持 pending |

### 48.4 验证

```text
$ cargo fmt --all -- --check                              （exit 0）
$ cargo clippy --workspace --all-targets -- -D warnings   （0 warnings）
$ cargo test --workspace                                  650 passed
```

路由器（重新部署 2026-10-30 10:58）：`/admin/static/app.js` 含 `pasteCallback`；
`POST /admin/api/auth/codearts/<id>/complete` 未登录返回 401（路由已挂载）；
`GET /admin/api/bootstrap` 的 `oauth_callback_url` 回到 `http://127.0.0.1:8787/oauth/callback`；
`/v1/models` 仍为 11 个。

### 48.5 未验证

真实账号的粘贴授权未执行（需要操作者完成一次华为登录）；STS 拒绝的具体
`error_description` 已能透出，但本次没有触发新的失败。

## 49. WorkBuddy Intl 授权失败：扁平账户结构与客户端身份（2026-10-30，完成）

### 49.1 现象

面板里 WorkBuddy 国际版登录完成后被弹回登录界面。不是崩溃：路由器进程自 10:58:15 起
未重启，`logread` 无 panic/error。数据库 `admin_auth_flows` 给出真实原因：

| 时间（UTC） | region | 状态 | 错误 |
| --- | --- | --- | --- |
| 02:50:16 | intl | failed | `WorkBuddy authorization did not identify a user` |
| 03:00:30 | intl | failed | 同上 |
| 03:01:04 | intl | failed | 同上 |

### 49.2 原因（两处）

1. `GET /v2/plugin/login/account` 在 Intl 返回**扁平**结构 `data:{uid, enterpriseId, nickname}`，
   而 CN 返回 `data.account.{uid,...}`（`docs/WORKBUDDY_MIGRATION.md` §169-170 已记录两种形态）。
   `build_credential` 只读 `data.account.uid`，回退也只看 token 的 `user_id`，于是 uid 为空，
   流程被判 `AuthInvalid` 并终止 —— 表现为“登录后回到登录界面”。
2. 管理台的授权客户端（`WorkbuddyAuthClient::new`）始终使用 honest 头，未读取
   `client.identity`；按 §43 的结论，Intl 门户要求官方客户端头族。

### 49.3 变更

| 文件 | 变更 |
| --- | --- |
| `src/providers/workbuddy/auth.rs` | `build_credential` 同时接受 `data.account.uid` 与 `data.uid`（`enterpriseId` 同理）；uid 缺失时错误信息附带**字段名**（不含值）便于定位上游结构变化；新增 1 个 Intl 扁平结构测试 |
| `src/admin/mod.rs` | 从 provider `options` 读取 `client.identity`，注入 `WorkbuddyAuthClient`（任一 WorkBuddy provider 配置即生效） |
| 路由器 `config.toml` | 增加 `kind = "workbuddy"`、`region = "intl"`、`"client.identity" = "impersonate"`（备份 `config.toml.bak3-20261030`） |

### 49.4 验证

```text
$ cargo fmt --all -- --check                              （exit 0）
$ cargo clippy --workspace --all-targets -- -D warnings   （0 warnings）
$ cargo test --workspace                                  651 passed
```

路由器新二进制（11:08 启动）：`/health` 200；`/v1/models` 22 个
（CodeArts 10 + Qoder 11 + mock 1，Qoder CN 账号 `edufx` 已在此前授权成功）。

### 49.5 未验证

真实 WorkBuddy Intl 登录未完成（需操作者在官方页面授权）。若仍失败，错误里会带
`fields: ...` 暴露上游返回的字段名，据此再对齐结构。

## 50. Qoder 签到：claim 开关与 TOML 点号键（2026-10-30）

### 50.1 现象

Qoder CN 账号 `edufx` 签到结果为“失败”，错误：

```text
Qoder reward claim is disabled; set the provider option `checkin.claim_enabled = true` to allow claims
```

### 50.2 原因

签到状态查询是只读的，会正常执行；**领取权益**默认关闭（AGENTS.md §2：奖励领取默认不实现，
除非上游明确允许自动化）。`QoderCheckinExecutor` 在存在可领取活动且未开启开关时返回
definitive failure 并点名该设置，所以整次运行被记为失败。

### 50.3 启用方式（已应用到路由器）

```toml
[[providers]]
kind = "qoder"
region = "cn"
enabled = true

  [providers.options]
  "checkin.claim_enabled" = "true"
```

**关键细节**：`options` 是 `HashMap<String, String>`，键里带点时**必须加引号**。
写成 `checkin.claim_enabled = "true"` 会被 TOML 解析成嵌套表 `options.checkin.claim_enabled`，
启动直接报 `invalid type: map, expected a string`（本次实际踩到，已修复并在
`config.example.toml` 中注明）。

### 50.4 验证

```text
$ /etc/init.d/polygate restart
INFO gateway listening address=10.0.0.1:8787
$ curl /health → {"status":"ok"}
$ curl /v1/models → 22
```

路由器配置备份：`config.toml.bak4-20261030`。

### 50.5 未验证

开启开关后的真实 claim 结果未由我触发（需要管理台会话）；操作者点一次「签到」后即可从
`checkin_runs` 与账号详情确认。

## 51. 授权槽位卡死：命名占用者与一键取消（2026-10-30，完成）

### 51.1 现象

管理台连续出现：

```text
another authorization is already in progress; cancel it or wait for it to expire
```

此时再点任何 Provider 的「添加账号」（包括 CodeArts / Qoder CN）都会被拒绝，且界面没有任何
入口告诉操作者该等多久、到底在等谁。

### 51.2 原因

管理台同一时刻只允许一个授权流程（`admin_flows` 表 + `active_flow(now)`）。Qoder Intl 的
设备码流程 `2afdf1bc-9ddc-4d9a-b1cf-ad92bb62215b`（创建 03:07:42Z，`expires_at`
03:17:42Z）在浏览器侧卡在 `selectAccounts` 页，流程一直是 `pending`，因此全局槽位被占满。
旧代码只回一句固定文案，既不含流程类型/区域/创建时间，也没有取消接口——只能干等 10 分钟
`flow_ttl_secs` 到期（而过期清理 `expire_flows` 只在发起/取消授权时运行，不轮询）。

### 51.3 修改

- `AdminState::ensure_no_active_flow(now)`（`src/admin/auth/flow.rs`）：四个 Provider 的
  `start` 路径原有的重复守卫合并到这里，错误信息改为点名占用者：
  `another authorization is already in progress (codearts cn, started 2026-10-30T03:07:42+00:00); ...`
- `AdminState::cancel_active_flow()`：先 `expire_flows`，再取当前槽位并 `cancel_flow`，
  返回 `FlowView`；没有流程时返回 `null`。
- 新接口 `POST /admin/api/auth/active/cancel`，不需要 flow id（操作者界面上本来也看不到未完成
  流程的 id）。
- 前端（`web/app.js`）：发起授权失败且错误含 `already in progress` 时，在状态行下方追加
  「取消进行中的授权」按钮，成功后提示重新点击对应 Provider。
- 测试 `stuck_authorization_can_be_cancelled_without_its_id`：先发起 CodeArts 流程占槽，
  再发起 Qoder 流程断言文案含 `already in progress` 与 `codearts`，调用取消接口断言
  `status = cancelled`，然后再次发起流程成功。

### 51.4 验证

```text
$ cargo fmt --check                              → 通过
$ cargo clippy --all-targets -- -D warnings      → 通过
$ cargo test --workspace                         → 652 passed
$ cargo zigbuild --release --target aarch64-unknown-linux-musl  → 成功
```

aarch64 二进制已替换路由器 `/usr/bin/polygate` 并 `restart`，`/health` 返回 200。

### 51.5 未验证

Qoder Intl 的 `selectAccounts` 循环本身仍未解决：网关侧的设备码轮询（202/404 = 等待）与
超时处理都正常，卡点在浏览器页面。需要操作者在选中账号后观察页面是否回到
`/device/selectAccounts`，以及是否出现授权码/错误提示，才能判断是上游限制还是需要额外参数。

## 52. WorkBuddy CN 未注册：补齐 provider 配置并实测 Chat（2026-10-30，完成）

### 52.1 现象

管理台里 WorkBuddy CN 账号 `744f9313-47dc-4da1-900e-08054ecb91f3`
（外部身份 `workbuddy:cn:744f9313-…`，认证状态有效）面板显示：

```text
额度：未知
检查认证 → provider is not registered
刷新额度 → provider is not registered
执行签到 → provider does not support check-in
```

### 52.2 原因

`ProviderRegistry` 以 `(kind, region)` 为键，且**只包含配置里启用的条目**。路由器
`/etc/polygate/config.toml` 当时只有 `workbuddy`/`intl` 与 `codearts`/`cn`、`qoder`/`cn`、
`mock`/`cn`，没有 `workbuddy`/`cn`，因此：

- `state.registry.get(&ProviderKey::new(Workbuddy, Cn))` 为 `None` → 认证检查与额度刷新
  走 `src/admin/api/mod.rs:789` / `:850` 的 `provider is not registered`。
- 签到执行器的 `RoutingCheckinExecutor::for_request` 在 `(kind, region)` 精确匹配失败后
  回落到同 kind 的任意注册项（`src/checkin/executor.rs:89`），于是拿到 Intl 执行器，
  `checkin()` 因 `region == Intl` 返回 `Unsupported`，最终记为
  `provider does not support check-in`（`src/checkin/model.rs:214`）。

换句话说三处报错是同一个根因：**账号本身有效，但该 Provider 没在配置里启用**。
管理台允许为未启用的 kind/region 发起授权并落库账号，这一点仍是有意保留的行为
（先拿到凭据再改配置），代价是面板会在之后才暴露“未注册”。

### 52.3 修改（仅配置，无代码改动）

在 `/etc/polygate/config.toml` 中 WorkBuddy Intl 之前新增：

```toml
[[providers]]
kind = "workbuddy"
region = "cn"
enabled = true
name = "WorkBuddy CN"
verification = "experimental"

  [providers.options]
  "client.identity" = "impersonate"
  "checkin.claim_enabled" = "true"
```

- `client.identity = "impersonate"`：`src/providers/profile.rs:122` 记录两个网关都会把
  缺少 gate headers 的调用方当作非客户端；Intl 直接答业务码 `11128`。CN 是否也强制未实测，
  保持与 Intl 一致更稳。
- `checkin.claim_enabled = "true"`：与 Qoder CN 一致，签到按钮才会真正领取（默认 false 只查状态）。

`config.example.toml` 同步补了 CN 示例（此前只有 Intl）。

### 52.4 验证（路由器真机）

```text
$ polygate doctor --config /etc/polygate/config.toml --data-dir /etc/polygate
[ok] config: loaded 5 provider entries, listen 10.0.0.1:8787
[ok] providers: mock-cn:mock, codearts-cn:codearts, qoder-cn:qoder,
                workbuddy-cn:workbuddy, workbuddy-intl:workbuddy

$ /etc/init.d/polygate restart      → running，gateway listening address=10.0.0.1:8787
$ GET /v1/models                    → 37 条，其中 owned_by=workbuddy-cn 15 条
$ POST /v1/chat/completions  model=workbuddy-cn::space-bunny  stream=false
  → 200，content="pong"，usage 163/3/166
$ POST /v1/chat/completions  model=workbuddy-cn::space-bunny  stream=true
  → 200，SSE 分块 delta 正常，finish_reason=stop
```

配置备份：`config.toml.bak5-20261030`。

### 52.5 状态与未验证

- WorkBuddy CN：授权、模型发现、非流式 Chat、SSE 均为 **LiveVerified**（2026-10-30）；
  工具调用、额度数值、签到最后结果仍为 MockVerified / 未触发。
- 额度与签到需在管理台点一次「刷新额度」「执行签到」后，从账号详情与 `checkin_runs` 确认。
- WorkBuddy Intl 仍无真实登录，维持 unavailable。
