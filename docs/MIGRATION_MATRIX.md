# 迁移矩阵与后续里程碑（M10）

> 对照对象：Coding2API（主参考）、dsh-wtq-pool、Johnsheng1/codearts2api、
> liu5269/zcode2api、本地 codearts2api/codearttray，以及 PolyGate 已完成的 M1–M9。
>
> **范围（2026-10-29 调整）**：必须支持 CodeArts、WorkBuddy、TRAE、Qoder；
> CodeBuddy 与 ZCode 均已明确排除（Explicitly Excluded）；本矩阵中相关行
> 只保留历史审计与许可证事实，不再作为迁移目标。
>
> 本文只做规划与判断，不修改数据库 Schema、不实现真实 Provider、不实现签到请求、
> 不开发 Web UI、不提前执行 M11。

## 0. 读表说明

| 判定 | 含义 |
| --- | --- |
| **已实现** | PolyGate M1–M9 已有可编译、可测试的实现，后续只做接线 |
| **可复用** | 现有模块/类型可直接承载新功能，不需要改接口或只改极小处 |
| **需扩展** | 现有接口/表/类型需要新增字段、方法或能力位，但不需要推翻 |
| **需重构** | 现有实现与目标语义冲突，必须改结构或替换 |
| **部分实现** | 已有可编译、可测试的基础层，但尚未达到可用功能 |
| **尚未实现** | 参考实现有、PolyGate 没有 |
| **明确排除** | 本项目不实现，或参考代码不可复用 |

证据口径：参考实现的行为均来自源码路径与函数名；PolyGate 现状来自
`src/` 与 `migrations/0001_init.sql`。没有真实账号实测的能力一律不标“已验证”。

## 1. PolyGate M1–M9 现状盘点

| 模块 | 主要文件 | 已实现内容 | 状态 |
| --- | --- | --- | --- |
| config | `src/config/mod.rs` | TOML + `POLYGATE_*`、`deny_unknown_fields`、Provider/Model/Scheduler/Logging、键校验、`load_with_env` | 完成 |
| error | `src/error/mod.rs` | `ErrorKind` 17 类、OpenAI/Anthropic 错误体、脱敏、`public_view` | 完成 |
| security | `src/security/mod.rs` | AES-256-GCM + AAD、主密钥文件、`SecretBox`、token hash、日志脱敏 | 完成 |
| storage | `src/storage/`、`migrations/0001_init.sql` | 连接池/WAL/busy_timeout/FK、迁移、7 张表、6 个 Repository、账号+凭据事务 | 完成 |
| protocol | `src/protocol/` | 消息/内容块/ChatRequest/Response/Usage/FinishReason/8 类流事件 + 状态机 | 完成 |
| providers | `src/providers/mod.rs`、`registry.rs`、`mock.rs` | `Provider` trait、`Capability`、`ProviderClass`、`VerificationState`、Registry、确定性 Mock | 完成 |
| api | `src/api/` | `/health`、`/v1/models`、`/v1/chat/completions`（JSON+SSE）、API Key 常量时间鉴权、OpenAI DTO/错误体 | 完成 |
| runtime | `src/runtime/` | `Gateway::bind/serve`、启动顺序、优雅关闭、SQLite 关闭、真实 TCP 测试 | 完成 |
| cli | `src/cli/` | `serve`/`doctor`/`config-example`、日志、Ctrl+C/SIGTERM、六项 doctor 检查 | 完成 |
| accounts | `src/accounts/mod.rs` | `AuthStatus`、`Credential`（Debug 脱敏）、`Account` 类型 | 部分（无管理服务） |
| sessions | `src/sessions/mod.rs` | `Session`/`SessionState`/绑定字段 | 部分（无粘性服务） |
| quota | `src/quota/mod.rs` | `QuotaUnit`/`QuotaStatus`/`QuotaSnapshot`/配额组 | 部分（无探测任务） |
| models | `src/models/mod.rs` | 统一模型路由、能力、别名基础 | 部分（无动态目录） |
| scheduler | `src/scheduler/` | 空目录 | 未实现 |
| metrics | `src/metrics/` | 空目录 | 未实现 |

存储表（`migrations/0001_init.sql`）：`accounts`、`credentials`、`sessions`、
`quota_snapshots`、`usage_events`、`settings`、`schema_version`。注意：

- `accounts` 已有 `weight`、`max_concurrency`、`quota_group`、`auth_status`、
  `last_error`、`metadata`；适合 SWRR/Least-Conn/冷却。
- `sessions` 已有 `client_session_id`、`account_id`、`upstream_session_id`、
  `state`、`expires_at`、`request_count`、`UNIQUE(client_session_id, provider_kind, region)`；
  适合显式会话粘性。
- `quota_snapshots` 只有 `used/limit_value/status/raw`，**没有**到期阶梯、
  积分包明细、`probe_failed` 三态字段；迁移签到/额度感知需要扩展。
- **没有** `checkin_runs`、`model_cooldowns`、`auth_flows`、`usage_hourly` 等表。

## 2. 功能迁移矩阵

### 2.1 账号、认证与凭据

| 能力 | 参考实现事实 | PolyGate 现状 | 判定 | 迁移动作 |
| --- | --- | --- | --- | --- |
| 账号 CRUD / 启停 / 权重 | Coding2API `credentials` 表（`src/db/schema.sql:42-67`）：`enabled`、`disabled`、`pinned`、`weight` 无（其调度不用权重）、`tags`；管理 API 在 `src/api/admin_credentials.py` | `accounts` 表 + Repository；无管理 API | **需扩展** | 新增 AccountService/Admin API；复用表字段；补 `pinned`/`disabled_reason`/`tags` 视需要 |
| 凭据加密 | Coding2API Fernet `data_enc`；本地 Go 工程自管 | `SecretBox` AES-GCM + AAD 绑定 account_id；`credentials` 表 | **可复用** | 直接使用；禁止明文落库/日志 |
| 授权流程（OAuth/设备码/回调） | Coding2API `Provider` Protocol 有 `start_auth/poll_auth/complete_callback/import_credential`（`src/provider/base.py` 末尾） | `Provider` trait 无 auth 流程；只有 `auth_status` | **需扩展** | M11 增加 `AuthFlow`/`AuthSession`/`AuthResult` 或独立 `AuthProvider` trait；先做手工导入 |
| 凭据刷新 | Coding2API 每渠道 `refresh`；CodeArts 一次性 token 只由 `RefreshTask` 写 | trait 无 refresh；`RefreshTask` 不存在 | **尚未实现** | 增加 refresh 钩子 + 单写者任务；CodeArts 必须串行 |
| 账号状态机 | `health`（NULL/-1/0-100）、`cooling_until`、`err_count`、`disabled_reason` | `AuthStatus` 只有 6 态；无健康度/冷却字段 | **需扩展** | 增加健康度/冷却/错误计数；不要把 `AuthStatus` 与调度健康度混为一谈 |
| 账号去重/身份 scope | `checkin_scope` / `activity_scope` 按上游账号 | 无 | **需扩展** | Provider 提供 `account_identity()`；没有则回落 account_id |

### 2.2 调度与账号池

| 能力 | 参考实现事实 | PolyGate 现状 | 判定 | 迁移动作 |
| --- | --- | --- | --- | --- |
| 选择算法 | Coding2API `src/engine/scheduler.py:258-309`：免费优先 → pin → 在途最少 → 36h 到期积分 → 7d 到期积分 → health → 剩余积分 → credential_id | `SchedulerConfig` 有 SWRR/RR/Least-Conn 枚举，但 `src/scheduler/` 为空 | **尚未实现** | 实现 Scheduler v1；默认 SWRR；两账号可验证 50:50 |
| 权重 | Coding2API 无权重；dsh 有 priority/round-robin/expiry/sticky | `accounts.weight` 已有 | **可复用** | SWRR 用 weight；权重 2:1 需测试 |
| 并发上限 | Coding2API `Pacer` 按桶在途/窗口；CodeArts 每账号 3 会话 | `max_concurrency` 字段 + `queue_timeout_secs` | **需扩展** | 增加租约/信号量；超限返回 `NoCapacity` 或排队 |
| 冷却 | Coding2API 13 类 `ErrKind`、账号级/模型级分离、Retry-After 双端钳制、`credential_model_cooldowns` 表 | 只有 `cooldown_base/max` 配置；无错误分类映射 | **需扩展** | 增加冷却状态与分类映射；至少区分账号级/模型级 |
| 会话粘性 | Coding2API `engine/affinity.py`：body 显式键 > header > 消息前缀指纹；用户名掺入；TTL/容量 | `sessions` 表有绑定字段；无服务 | **可复用 + 需扩展** | 实现显式 ID/header/前缀三级；持久化绑定；TTL/失效语义 |
| 健康/耗尽 | Coding2API `health` 三态；`EXHAUSTED=-1`；`is_selectable` 不排除 -1，只排最后 | `QuotaStatus` 有 known/unknown/error/partial | **需扩展** | 明确“耗尽是否可最后兜底”；未知绝不写成 0 |
| 错误隔离 | Coding2API 流内错误换号、`max_rotate=3`、INVALID 跳过整个 provider | 无 | **尚未实现** | 在 executor 层实现；不要把上游原文回显 |
| 两账号公平 | 见 §3 | 无 | **尚未实现** | 用 SWRR + least-inflight；测试 100 会话 50:50、并发 8 时均摊 |

### 2.3 会话与 API

| 能力 | 参考实现事实 | PolyGate 现状 | 判定 | 迁移动作 |
| --- | --- | --- | --- | --- |
| OpenAI Chat | Coding2API `src/api/chat.py`、`src/compat/openai/*` | 已实现 JSON+SSE、工具调用、usage、错误体 | **已实现** | 直接复用；补真实 Provider 接线 |
| 请求执行路径 | Coding2API `engine/executor.py`：选号 → 换号 → 粘性 → 统计 | `src/api/mod.rs` 的 chat handler 直接调用单个 provider，并使用占位 `Credential`（M9 已知问题 §18.9-8） | **需重构** | 抽出 executor：模型解析 → 选号 → 解密凭据 → 调用 → 分类错误 → 换号；API 层不直接碰账号/调度 |
| 模型列表 | Coding2API 动态目录 + 别名 + 可用性过滤 | `src/api/catalog.rs` 从 Registry 发现 | **可复用** | 增加真实 Provider 的 `list_models`；别名/可用性后续 |
| Anthropic Messages | Coding2API 有兼容层 | 未实现 | **尚未实现** | 第 4 级；不在 M11–M14 |
| OpenAI Responses | Coding2API 有兼容层 | 未实现 | **尚未实现** | 同上 |
| API Key / 管理鉴权 | Coding2API RBAC（admin/operator/viewer）、Key 配额 | Gateway API Key 常量时间校验；无管理 API/RBAC | **需扩展** | M11 增加 admin token/RBAC；不要复用用户 Key 做管理 |
| 用量与成本 | Coding2API `usage_events` + `usage_hourly` + 价格表 | `usage_events` 基础字段；无聚合/成本/延迟 | **需扩展** | 后续里程碑；不要伪造成本 |
| 请求日志/审计 | Coding2API `audit_events`、`logs.py` | 无 | **尚未实现** | 管理后台阶段 |

### 2.4 Provider 契约与真实接入

| 能力 | 参考实现事实 | PolyGate 现状 | 判定 | 迁移动作 |
| --- | --- | --- | --- | --- |
| Provider trait | Coding2API `base.Provider`：auth/refresh/quota/stream/models/checkin/growth/activity | PolyGate trait 只有 chat/quota/models/auth_status | **需扩展** | M12 增加 checkin/refresh；M11 增加 auth |
| 能力声明 | Coding2API 用 `getattr(..., None)` 探测 | PolyGate `Capabilities` 默认 false，显式声明 | **可复用** | 新能力位默认 false；不引入 Python 式探测 |
| 错误分类 | Coding2API `ErrKind` + 每渠道 `classify_status` | `ErrorKind` 17 类；无上游业务码映射 | **需扩展** | 每渠道 adapter 内做业务码 → `ErrorKind` 映射 |
| Mock | Coding2API 测试用 `MockTransport` | `src/providers/mock.rs` 确定性 Mock + 故障注入 | **可复用** | 增加 checkin 场景；不要用 Mock 冒充真实能力 |
| CodeArts | 见 `docs/PROVIDER_MIGRATION.md` §2.1 | 无 adapter | **尚未实现** | 第一个真实 Provider |
| Qoder | 见 §2.2；M13-D1 研究、M13-D2-A 协议基础、M13-D2-B 设备码授权、M13-D2-C 额度/签到/模型/推理；CN 授权、模型发现、额度、签到（含一次领取）、Chat、SSE、工具调用与多轮工具结果均已真实验证 | 已注册 `qoder-cn` Provider（认证/模型/额度/Chat/SSE/工具 + 签到执行器） | **已实现（CN 已验证；刷新与 Intl 未验证）** | 刷新与 Intl 单独验证；并发/限流只观察不压测 |
| TRAE | 见 §2.3 | 无 | **尚未实现** | 设备身份风险高，靠后 |
| CodeBuddy CN/Intl | 见 §2.4/2.5 | 无枚举、无 adapter（2026-10-29 移除） | **已移除** | 不得据此实现 |
| OfficeAce | 见 §2.6 | 无枚举/无 adapter | **尚未实现** | 需先决定是否纳入 ProviderKind |
| Kilo | 见 §2.7 | 无 | **待定** | 无凭证免费渠道；不纳入也不影响主线 |
| Zen | 见 §2.8 | 无 | **明确排除** | 不实现任何门禁伪装 |
| Agent Task | Coding2API 没有独立 Agent Task API；只有 chat | trait 有 `AgentTask` 占位 | **明确排除（当前）** | 所有 Provider 保持 `AgentTask` Unsupported |

> 现有 `docs/PROVIDERS.md` 已将 OfficeAce、Kilo 排除在主线 Provider 范围之外；
> 本表列出它们是为了完整记录 Coding2API 的 8 个上游，**不改变**该决定。
> 若要纳入，先新增 `ProviderKind` 枚举并单独走 Provider 验收流程。

### 2.5 任务、签到与运维

| 能力 | 参考实现事实 | PolyGate 现状 | 判定 | 迁移动作 |
| --- | --- | --- | --- | --- |
| 后台任务框架 | Coding2API `tasks/runner.py`：asyncio 循环、热更周期、手动触发单飞、no-op 不入账 | 无 | **尚未实现** | M12 实现任务 runner/状态；先只接签到 |
| 全局节流 | Coding2API `tasks/pacer.py`：严格串行 + 并发桶/窗口 | 无 | **尚未实现** | M12 实现 Pacer；签到/探测共用 |
| 签到 | 见 `docs/CHECKIN_RESEARCH.md` | 无 | **尚未实现** | M12 通用框架 + Mock；M14 起接真实 |
| 签到结果持久化 | Coding2API 不落库（内存封账） | 无 | **需扩展** | 建议新增 `checkin_runs`；不要照搬内存方案 |
| 额度探测 | Coding2API `tasks/quota_probe.py`；`credit_events` diff | 无任务；有 `quota_snapshots` | **需扩展** | M11/M12 增加探测任务；表加三态/到期字段 |
| 成长中心/活跃上报 | Coding2API 仅 CodeBuddy；含不可逆动作 | 无 | **明确暂缓** | 不是签到；后续单独评估，默认关闭 |
| 告警/备份/清理 | Coding2API `tasks/alerting.py`、`backup.py`、`retention.py` | 无 | **尚未实现** | 运维阶段；不阻塞主线 |
| Doctor/CLI | PolyGate 已有六项检查 | 已实现 | **可复用** | 增加 provider/checkin 自检项 |
| Web UI | Coding2API 完整管理台；dsh 面板 | 无 | **尚未实现** | 最后阶段；数据必须来自真实 API |

## 3. 账号池与两账号公平调度

### 3.1 Coding2API 的选择链

`src/engine/scheduler.py:258-309` 的排序键（升序）：

1. `_free_first_rank`：候选里存在 `credit_rate == 0` 的免费渠道时，免费渠道整体优先；
2. `pinned`：手动 pin 的凭证优先（`pinned` 非空时只在 pinned 里选）；
3. `inflight`：在途请求最少优先（最少连接）；
4. `-expiring_credits(36h)`：36 小时内到期积分多者优先；
5. `-expiring_credits(7d)`：次窗口；
6. `_rank(health)`：known(0) → unknown(1) → exhausted(2)；
7. `-_health_value`：同档健康度高者优先；
8. `-_remaining_value`：剩余积分多者优先；
9. `credential_id`：最终稳定排序。

**两个账号的含义**：

- 若两个账号静态指标完全相同（同 health、同到期、同余额），排序会退化到
  `credential_id`，**顺序请求会一直选同一个号**；只有并发请求因 `inflight`
  不同才会分摊。这是 Coding2API 自己记录的实测问题（`scheduler.py:268-275`
  注释：codebuddy_intl 六号 2 小时一个号吃 180+ 次）。
- 因此“两个账号公平”不能照搬这条链；必须显式加入轮转（SWRR/RR）或
  以 inflight 为主键。PolyGate 的 `SchedulerStrategy` 已有
  `SmoothWeightedRoundRobin`/`RoundRobin`/`LeastConnections`，应默认 SWRR，
  并为两账号写 50:50 的确定性测试。

### 3.2 dsh 的分布模式

`src/accounts.ts:1093-1160` 定义 `AccountDistribution`：

- `priority`：一个号用到被限流，再换下一个；冷却结束回到队首；
- `round-robin`：连续请求轮转；
- `expiry`：一次性积分包最早到期者优先；月包忽略；
- `sticky`：同一对话固定一个号，新对话轮到下一个号；上游 prompt cache 按账号，
  中途换号会丢缓存。

`src/source-pool.ts:20-160` 的 `SourcePool`：

- `orderSourceCredentials`：用户指定的号优先 → 未过期优先 → 刚失败被 hold 的靠后；
- `noteSourceFailure`：优先采用上游 `Retry-After`，否则用用户配置的冷却；
- `packUrgency`：一次性包到期越早越优先，无到期信息排最后；
- `sourceRotation`：每个 region 独立游标，只有聊天推进，模型列表/健康检查不推进。

### 3.3 对 PolyGate 的建议

| 需求 | 建议 |
| --- | --- |
| 默认策略 | SWRR（`weight` 默认 1）；两账号 1:1 严格交替 |
| 并发分摊 | 叠加 least-inflight 作为第二键；顺序请求仍由 SWRR 轮转 |
| 额度感知 | 仅在已知额度时参与；未知不排除、不写 0；快过期优先 |
| 会话粘性 | 显式 session id > header > 前缀指纹；粘性优先于 SWRR，但低于手动 pin |
| 健康状态 | known/unknown/exhausted 三态；exhausted 默认排除，除非配置允许最后兜底 |
| 冷却 | 账号级与 (account, model) 级分开；Retry-After 双端钳制；未知不惩罚 |
| 错误隔离 | 单账号失败不影响其他账号；换号上限；不因一次失败禁用 |
| 两账号验收 | 100 个新会话 50:50；权重 2:1；并发 8 时在途均摊；一个号冷却时全部走另一个；恢复后按权重回归 |

## 4. 许可证审计

| 仓库/项目 | LICENSE | 关键文件 | 代码复用 | 设计借鉴 | 约束 |
| --- | --- | --- | --- | --- | --- |
| Coding2API | **MIT** | `.refs/RobbsLuo_Coding2API/LICENSE`；`NOTICE` | 允许，需保留版权与许可声明 | 允许 | `NOTICE` 还列出 codebuddy2api、trae2api-web、workbuddy-auto-signin、workbuddy2api、workbuddy-manager、qoder2api-hub、codearts2api 等 MIT 参考；复制其代码时需一并遵守上游声明 |
| dsh-wtq-pool | **MIT** | `.refs/chengzhi-c_dsh-wtq-pool/LICENSE` | 允许，需保留声明 | 允许 | 插件/渲染器代码不要整段照搬；其 UI 只做独立重绘 |
| Johnsheng1/codearts2api | **MIT** | `.refs/Johnsheng1_codearts2api/LICENSE` | 允许，需保留声明 | 允许 | 单文件实现，签到端点与 Coding2API 不同；只作协议事实 |
| liu5269/zcode2api | **AGPL-3.0** | `.refs/liu5269_zcode2api/LICENSE` | **禁止复制**（且 Provider 已排除） | 仅可读历史协议事实 | 网络使用也会触发 AGPL；**ZCode 已从 PolyGate 支持范围移除**，验证码求解、设备指纹参数明确不迁移 |
| yetone/magpie | **MIT** | `.refs/yetone_magpie/LICENSE`、`THIRD_PARTY_NOTICES` | 允许，需保留声明 | 可参考插件架构 | 本项目当前不依赖它；若引入需核对其 THIRD_PARTY_NOTICES |
| 本地 `codearts2api`（Go） | **无 LICENSE 文件；操作者本人作品** | `~/codearts2api` | **允许（作者授权）** | 允许 | 无第三方许可证障碍；复制时保留原版权声明，并在 PolyGate `LICENSE`/`NOTICE` 标注来源；其第三方依赖仍需单独核对 |
| 本地 `codearttray` | **无 LICENSE 文件；操作者本人作品** | `~/codearttray` | **允许（作者授权）** | 允许 | 同上；托盘/自启/桌面 UI 仍可不迁移，但这属于设计取舍，不是许可证限制 |
| PolyGate 自身 | **MIT** | `LICENSE` | — | — | 2026-10-29 已生成 LICENSE 并完成依赖许可证清点（全部宽松许可） |

补充：

- 参考仓库的 MIT 只覆盖其自身代码，不自动覆盖其 `NOTICE` 中列出的第三方项目；
  如果复制了某个文件，必须同时保留该文件原有版权/许可声明。
- Coding2API 的 `NOTICE` 明确“代码为独立实现，不复制上述项目源代码”；这不等于
  我们可以任意复制 Coding2API 的代码，只是说明其自身来源。
- 测试夹具、抓包、模型缓存、截图、`assets/` 等可能不含在 MIT 授权范围内；
  不要直接复制到 PolyGate。
- 依赖许可证清点（`cargo deny`/`cargo about` 或人工清单）尚未执行；发布门禁
  必须包含这一步。

## 5. M11+ 里程碑重排

排序遵循用户给定的优先级：账号认证与管理 → 签到基础设施 → 真实 Provider →
自动/一键签到 → 负载均衡与会话粘性 → Web UI；同时保证“真实签到依赖已验证
Provider”。每个里程碑仍然单独执行、单独验收、完成即停。

| 里程碑 | 目标 | 依赖 | 退出条件 |
| --- | --- | --- | --- |
| **M11 账户与凭据管理** | AccountService + 管理 API；手工导入/启停/权重/标签；凭据加解密；`AuthStatus` 生命周期；admin token/RBAC 最小集 | M1–M9 | 账号 CRUD 走真实 SQLite；凭据不出现在日志/响应；导入坏凭据有可读错误；无真实 Provider **（2026-10-29 完成服务层；HTTP 管理端点未实现，见 `PROGRESS.md` §20）** |
| **M12 Provider 契约扩展 + 通用签到框架（Mock）** | 独立 `CheckinExecutor`（未改 `Provider` trait）、`CheckinService`、`CheckinScheduler`、QuotaRefreshHook、`checkin_runs`/`checkin_schedules` 迁移；Mock 全状态覆盖 | M11 | 仅 Mock：单账号/两账号/批量/自动/重启/补签/并发/崩溃恢复/失败隔离测试全绿；**不接真实上游** **（2026-10-29 完成签到引擎；Provider 契约未扩展，见 `PROGRESS.md` §21）** |
| **M13 CodeArts Provider 实测** | 真实 auth/refresh（单写者）/模型/额度/聊天/SSE/工具/错误分类；可参考或复用操作者本人 Go 工程（作者授权） | M11 | 真实账号端到端通过后才 `Verified`；无凭据保持 `Experimental`；refresh 不并发 **（2026-10-29：M13-A 会话墙/diff 审计 + 会话绑定见 §22；M13-B Provider/会话路由/签到执行器已实现并通过 mock 测试，真实账号未验证，见 `PROGRESS.md` §23）** |
| **M14 CodeArts 签到** | 复用 M12 框架接 `/v1/ops/*`；单账号 → 批量 → 自动；默认关闭，配置显式开启 **（M13-B 已实现执行器与真实额度 Hook，未做真实账号验证）** | M12、M13 | 真实账号验证活动/已签/CLAIMED 补 confirm/活动结束；失败不封账；成功后触发额度探测 |
| ~~M15 CodeBuddy CN Provider + 签到~~ | **已取消**：CodeBuddy 于 2026-10-29 从范围移除 | — | 不实现 |
| **M16（可选）OfficeAce Provider + 签到** | 标准 OpenAI + Basic；伪 200 错误帧；bonus claim（默认关闭） | M12、M15；需先确认是否纳入范围 | 端到端；无控制面凭据时明确失败 |
| **M17 Qoder Provider + 签到** | COSY 签名测试向量；模型/额度/聊天；活动制 + 旧接口回退（M13-D1 研究已完成，实现入口为 M13-D2） | M12、M15；M13-D2-0 政策确认 | 签名向量通过；区域无签到接口返回 `Unsupported`（settled，不自动重试）而非 `Failed`；真实能力逐项 `Verified` 前保持 `Experimental` |
| **M18 TRAE Provider + 签到** | 设备头族、模型、聊天、签到回查确认；Intl 不联系 | M12、M17 | 端到端；9074 软失败；设备号合规确认后才开启签到 |
| **M19 调度器与会话粘性** | SWRR/RR/Least-Conn、权重、并发租约、冷却、健康、额度感知；显式/header/前缀粘性；两账号验收 | M11 | 100 会话 50:50；权重 2:1；超并发 `NoCapacity`；重启绑定恢复；不静默迁移 |
| **M20 Web UI** | Accounts/Providers/Models/Sessions/Usage/Checkin/Logs/Settings；数据全部来自真实 API | M11–M19 | 无 Mock 冒充健康；暗色主题；危险操作二次确认 |
| **M21 部署与发布** | Docker/systemd、LICENSE、NOTICE、依赖许可证清点、发布门禁 | M20 | `cargo build/test/clippy/fmt` 全绿；许可证清单完整；无 AGPL/第三方无 LICENSE 代码 |

说明：

- 本地 Go 工程 `codearts2api` 与托盘 `codearttray` 均为操作者本人作品（2026-10-29
  确认），M13/M19 可直接复用其签名、账号池、冷却与会话绑定实现；复制时保留版权
  声明并在 PolyGate `LICENSE`/`NOTICE` 标注来源。托盘/自启/桌面 UI 仍不作为目标
  （设计取舍，不是许可证限制）。
- M19 与 M12–M18 无强依赖；如果更早需要多账号聊天，可把 M19 提前到 M14 之后。
  本表按用户给定优先级排列，因此放在签到功能之后。
- “自动/一键签到”在 M12 已具备框架，在 M14 起才接真实渠道；后续每个 Provider
  的签到都复用同一框架，不重复实现。
- 每个 Provider 的 `Verified` 必须单独验收；不能因为同渠道另一个区域通过就
  推断本区域通过。

## 6. 明确排除与非目标

以下内容不在 M11+ 主线，且部分永久排除：

- OpenCode Zen 作为上游：**永久排除**；不迁移 UA/session/工具注入门禁。
- 验证码破解、批量注册、设备指纹伪造、客户端身份冒充、封禁后自动换号续跑、
  额度/风控绕过、未授权凭据共享：**永久排除**。
- CodeArts `/v1/ops/claim`+`/confirm`、OfficeAce `/v1/subscription/bonus/claim`
  等权益领取：默认不实现；只有上游明确允许自动化且运维显式开启才做。
- Anthropic Messages、OpenAI Responses 适配器：不在 M11–M18；属于 API 兼容阶段。
- Agent Task API：Coding2API 没有独立 Agent Task；所有 Provider 保持 Unsupported。
- 成长中心/活跃上报/抽奖/兑换：不是签到；含不可逆动作，暂缓。
- **ZCode 整体排除**：不实现 Provider、不迁移 `zcode2api` 任何功能、不注册、不进入模型列表；
  验证码求解与 `X-Aliyun-Captcha-Verify-Param` 继续排除。不得因参考实现支持而重新加入。
- 本地 `codearts2api`/`codearttray`：操作者本人作品，代码可复用；托盘/自启/桌面 UI
  仍可不迁移（设计取舍），不是许可证限制。发布前补 `LICENSE`/`NOTICE` 并标注来源。

## 7. 证据缺口与验证计划

| 缺口 | 影响 | 计划 |
| --- | --- | --- |
| 没有任何真实账号端到端验证 | 所有 Provider 只能 `Experimental` | M13 起逐个真实账号验收；记录日期/环境/结果 |
| CodeArts 两套签到端点未对齐 | 可能接错套餐 | 用真实账号分别探测 `/v1/ops/*` 与 `/api/v1/benefit/claim` |
| Qoder 国际站签到可用性未知 | 可能把“无接口”当失败 | M13-D1 已确认 CN 为活动制；Intl 仍 Unverified。D2 只读探测；404/405/410 映射 `Unsupported`，不自动重试 |
| TRAE 国际站签到未知 | 可能误打 CN 接口 | 按区域关闭；真实 Intl 账号确认 |
| 签到幂等性未逐渠道确认 | 重启后可能重复领取 | M12 持久化 `checkin_runs`；每个渠道验证重复 claim |
| 依赖许可证未清点 | 发布阻塞 | M21 执行 `cargo deny`/人工清单，补 LICENSE/NOTICE |
| 两账号公平未实测 | 调度可能偏斜 | M19 用确定性测试 + 真实两账号长跑 |
| 额度单位不统一 | 排序/展示可能混淆 | 保持 `unit` 显式；未知不折算 |

## 8. M14-B 执行记录 — WorkBuddy 核心 Provider（2026-10-29）

> 状态定义：**Implemented** = 代码存在；**MockVerified** = Mock HTTP Server 测试通过；
> **LiveVerified** = 真实上游验证；**Unsupported** = 参考实现或上游不支持；
> **CompatibilityGap** = 参考实现有、本项目因边界不迁移。MockVerified 不等于 LiveVerified。

### 8.1 已实现（MockVerified）

| 能力 | 实现 | 测试 |
| --- | --- | --- |
| 凭据读取与校验 | 扁平 JSON 与桌面 `.info` 两种形状；毫秒/秒过期归一；`uid` 与 JWT `sub` 一致 | `credential.rs` 6 个单测 |
| 区域隔离 | CN / Intl 独立基址、默认域、模型路径；跨区域凭据拒绝 | `region.rs` 4 个单测 + 集成测试 |
| 模型发现 | CN `/v2/enterprises/{id|personal}/models`；Intl `/v3/config`；`agents[name=cli]` 过滤；禁用跳过 | `models.rs` 4 个单测 + 2 个集成测试 |
| 非流式聊天 | 上游仅 SSE，网关本地聚合 | `chat_aggregates_the_stream_into_one_response` |
| SSE 流式聊天 | 纯 OpenAI 行格式；空占位忽略；截断检测 | 3 个集成/单元测试 |
| Tool Calling | 请求 `tools`/`tool_choice`、历史 `tool_calls`/`tool`；流式分片拼装 | 2 个集成测试 + 1 个转换单测 |
| 错误归一化 | 业务码优先；401/402/403/429/5xx/超时/200 错误信封 | `error.rs` 6 个单测 + 4 个集成测试 |
| 请求取消 | 无上游取消接口；丢弃流即停止读取，`cancellation=false` | `dropping_the_stream_releases_the_request` |
| 凭据脱敏 | `Debug` 隐藏 token；错误消息走 `redact_secrets` | 2 个单测 |

### 8.2 未实现 / 未验证

| 项 | 状态 | 说明 |
| --- | --- | --- |
| Access Token 刷新、refresh 轮换、CAS 持久化 | 未实现 | M14-C |
| 设备码 / 官方页面授权、凭据导入 UI | 未实现 | M14-C；不做验证码自动化 |
| 额度查询 | 未实现 | M14-C；`quota` 当前返回 `Unsupported` |
| 签到（状态/领取） | 未实现 | M14-C；claim 需显式开关 |
| 真实上游任何能力 | **未验证** | 无账号、未发真实请求 |
| Intl 首条 system、会话头要求 | CompatibilityGap | 不注入占位 system、不伪造会话头；Intl 状态 `unavailable` |

### 8.3 CompatibilityGap（记录原始行为，不迁移）

| 参考实现行为 | 依赖 | 本项目处理 |
| --- | --- | --- |
| 伪造 UA `WorkBuddy/… CLI/…` | 规避“使用端”归因 | 发送 `polygate/<version>` |
| `X-IDE-*`、`X-Product`、`X-Agent-*`、`X-CodeBuddy-Request`、`X-Conversation-*`、`x-stainless-*` | 客户端身份校验 | 不发送，按请求级错误处理上游拒绝 |
| `X-Device-Token`（Turing SDK）、`X-Machine-ID`、`X-Session-ID` | 设备证明 | 不实现、不派生 |
| system 折叠 / channel marker 中和 / 零宽脱敏 | 绕过 11128 | 不实现；不做任何提示词或历史改写 |
| `reasoning_effort` / `enable_thinking` 默认值 | 官方 CLI 指纹 | 不补默认值 |
| 静态模型兜底列表 | 目录发现失败时仍显示模型 | 不贡献模型，发现失败即失败 |

以上缺口意味着：**诚实头能否被上游接受未经验证**。M14-C 的真实探针必须按
“额度 → 模型列表 → 最小聊天”的顺序执行，结果写入 `PROGRESS.md`；失败时标记
`unavailable`，不回填伪装头。

### 8.4 M14-C 待办

1. 刷新链路：单写者 + `CredentialStore` CAS、refresh 轮换、临期预刷新、401 单次重试（仅在任何内容输出前）。
2. 授权入口：官方页面/设备码 + 明文凭据导入；不实现短信验证码自动化。
3. 额度：月度包与一次性包解析，未知保持 `null`，credits 不折算 token。
4. 签到：只读状态 → 受开关控制的 claim → `QuotaRefreshHook`。
5. 真实只读探针与结果记录；据此决定 WorkBuddy 的 `experimental` / `unavailable` 状态。

## 9. M14-C 执行记录 — WorkBuddy 认证生命周期、额度与签到（2026-10-29）

> 状态口径不变：**Implemented / MockVerified** 只表示离线测试通过；**LiveVerified**
> 需要真实账号端到端验证；**Unsupported** 是明确的否定能力；**CompatibilityGap**
> 是参考实现有、本项目按边界不迁移的行为。

### 9.1 已实现（MockVerified）

| 能力 | 实现 | 测试 |
| --- | --- | --- |
| 临期预刷新 | 300 s 窗口；未知过期时间不刷新 | `a_valid_credential_is_used_without_refreshing` |
| refresh 轮换 | 新 refresh token 覆盖旧值；缺失则保留 | `a_rotated_refresh_token_is_persisted_before_the_chat` |
| 进程内单飞 | 按账号 `tokio::Mutex` | `concurrent_calls_refresh_once` |
| 跨进程租约 | SQLite `credential_refresh_leases`（60 s TTL）；拿不到租约等待对方写入 | `another_process_holding_the_lease_is_waited_for_not_duplicated`、`refresh_leases_coordinate_across_connections_and_expire` |
| revision CAS | `CredentialStore::update_credential`；冲突重读 | `a_cas_conflict_reloads_the_winners_credential` |
| 持久化失败 | 返回 `Storage`，不继续聊天、不报告刷新成功 | `a_failed_persist_never_reports_a_successful_refresh` |
| 失败分类 | 401/403 → `AuthInvalid`；5xx/网络 → 上游/传输错误 | `a_rejected_refresh_is_auth_invalid_and_persists_nothing`、`a_transport_failure_is_not_an_auth_failure` |
| 401 单次重试 | 未输出内容前强制刷新一次；不再循环 | `a_401_on_chat_triggers_one_forced_refresh_and_one_retry`、`a_persistent_401_is_retried_only_once` |
| 设备码授权 | start / poll / account；11217、12151；TTL 600 s | `auth.rs` 6 个单测 + 2 个管理台流程测试 |
| 额度 | 月度包 + 一次性包；`*Precise`；credits；空包 Unknown | `quota.rs` 5 个单测 + 3 个集成测试 |
| CN 签到 | 只读状态；受 `checkin.claim_enabled` 控制的 claim；1001/10001/已签到归一 | `checkin/workbuddy.rs` 7 个测试 |
| 签到后额度 | 复用 `QuotaRefreshHook`；`quota_with_context` 先刷新 | `a_workbuddy_checkin_refreshes_quota_after_success` |
| 禁用账号 | 后台循环跳过 | `a_disabled_account_is_skipped_by_the_background_loop` |

### 9.2 Unsupported / 未验证

| 项 | 状态 | 说明 |
| --- | --- | --- |
| Intl 签到 | Unsupported | 参考实现关闭；执行器零请求返回 |
| 真实上游任何能力 | 未验证 | 无账号、未发真实请求 |
| 桌面 `.info` 加密信封 | 不实现 | 只接受明文凭据导入 |
| 设备证明 / 客户端身份头 | CompatibilityGap | 不伪造，见 §8.3 |

### 9.3 与参考实现的差异（已记录）

1. 签到业务码两套来源冲突（`1001/1002/1003` vs `10001`），同时识别；真实环境再确认。
2. 签到/状态端点使用 `/v2/billing/meter/*`（workbuddy2api），Coding2API 为
   `/billing/meter/*`；不自动重试猜测。
3. `code == 0` 但缺少 `data.credit` 时按成功处理（workbuddy2api 口径），
   Coding2API 更严格；摘要明确“上游未报告奖励数值”。
4. 额度字段 `Cycle*` 与 `Capacity*` 均支持，按包类型优先并互相兜底。
5. 新增 `credential_refresh_leases` 迁移（0006），仅用于刷新租约，不存秘密。

### 9.4 下一步

- 真实账号只读探针（额度 → 模型 → 最小聊天），需要凭据与显式授权。
- M14-D（TRAE）尚未开始；按阶段纪律停在本轮。

## 10. M14-D 执行记录 — TRAE 核心 Provider（2026-10-29）

> 状态口径同 §9：**Implemented / MockVerified** 只表示离线测试通过；
> **LiveVerified** 需要真实账号；**CompatibilityGap** 是参考实现有、
> 本项目按边界不迁移的行为。

### 10.1 已实现（MockVerified）

| 能力 | 实现 | 测试 |
| --- | --- | --- |
| 凭据解析 | 扁平 + `{auth,account}` 嵌套；`expires_at` 秒/毫秒；JWT `exp` 只读回落 | `flat_credential_is_loaded`、`nested_ide_shape_is_loaded_with_millisecond_expiry`、`jwt_exp_is_the_last_expiry_fallback` |
| 区域隔离 | CN/Intl realm；`domain` 必须属于区域；`apiHost` 白名单 | `a_token_from_another_realm_is_rejected`、`an_out_of_realm_api_host_is_rejected`、`regions_never_share_credentials_or_hosts` |
| 设备号处理 | 只透传导入值，绝不生成 | `missing_device_identifiers_stay_empty`、`imported_device_identifiers_are_forwarded_not_invented` |
| 模型发现 | `POST get_detail_param`；`context_window_tokens.max > dev` 取 max | `models_use_the_directory_envelope`、`catalogue_uses_the_max_window` |
| 聊天信封 | `function=solo_work_lite`、`config_name`、`model`、`stream=true` | `envelope_and_content_are_rewritten` |
| 消息改写 | `developer`→`system`；content 数组化；`function`→`function_call` | `tool_history_is_converted_to_solo_shape` |
| tools | `parameters` 字符串化；`tool_choice: none` 删除；强制函数 → 名称 | `tool_parameters_are_stringified`、`tool_choice_none_drops_the_tools`、`a_forced_function_becomes_its_name` |
| SSE | 命名事件 `output/token_usage/done/error`；`[DONE]` 兜底 | `output_carries_content_reasoning_and_tools`、`done_data_frame_terminates_the_stream` |
| 工具分片 | 按 index 合并；无 name 的参数续片保留 | `a_tool_continuation_without_a_name_is_kept`、`tool_calls_survive_streaming_fragments` |
| Usage | `cache_read_input_tokens` / `cached_tokens` 链；真实 0 保留 | `usage_keeps_cache_read_and_zero` |
| 非流式 | 聚合同一 SSE 流；错误事件不伪装成功 | `non_streaming_chat_aggregates_named_events` |
| 错误分类 | 状态 + 信封业务码 + 流内 error；脱敏截断 | `upstream_failures_are_classified`、`stream_codes_match_the_http_mapping` |
| 超时 / 断流 | 短请求 30 s；流空闲 120 s；EOF 无结束标记 = 截断 | `idle_stream_is_cut_off`、`a_slow_model_request_times_out`、`a_truncated_stream_is_reported_as_an_error_event` |
| 运行时 | `ProviderKind::Trae` 注册 | `bind_accepts_the_trae_provider` |

### 10.2 Unsupported / 未验证

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 额度查询 | M14-D 未实现 → §11 已实现 | M14-D 时 `quota = false` |
| Token 刷新 | M14-D 未实现 → §11 已实现 | 过期凭据本地拦截，零请求 |
| 签到 | M14-D 未实现 → §11 已实现 | 设备号由运维提供；不生成 |
| 管理台 TRAE 授权 | M14-D 未启用 → §11 已实现 | 登录 URL 需运维提供设备 ID |
| 真实上游任何能力 | 未验证 | 无账号、未发真实请求 |

### 10.3 与参考实现的差异（已记录）

1. 孤儿 tool 结果 / 无 name 的 tool call → `BadRequest`；参考实现静默丢弃。
2. 动态模型失败不回退静态表；参考实现的 `STATIC_MODELS` 含未验证/内部模型。
3. `401` 映射 `AuthExpired`（参考 DEAD），为 M14-E 刷新重试保留正确语义。
4. 不发送 IDE 头族、设备号、Referer/timezone、`Trae/…` UA。
5. 不转发未观测的采样字段，不估算积分，不做 pacer/提示词改写。
6. Intl OAuth 主机没有源码证据，`region.rs` 留空且不猜。

### 10.4 下一步

- ~~M14-E：TRAE `ExchangeToken` 刷新、额度、签到、管理台授权~~ → 见 §11。
- 真实账号只读探针（模型 → 最小聊天）仍需凭据与显式授权；本轮未执行。

## 11. M14-E 执行记录 — TRAE 认证生命周期、额度与签到（2026-10-29）

> 状态口径同 §10。

### 11.1 已实现（MockVerified）

| 能力 | 实现 | 测试 |
| --- | --- | --- |
| 登录 URL | `www.trae.cn/authorization`；`client_id`、`auth_callback_url`、`login_trace_id`（machine+device 后 16 位） | `login_url_carries_the_supplied_identity`、`login_trace_is_the_last_sixteen_characters` |
| 回调解析 | `refreshToken`；缺失时回退 `userJwt.RefreshToken` → `Token` | `callback_parsing_accepts_encoded_json`、`callback_parsing_falls_back_to_user_jwt` |
| 设备身份 | 运维提供 hex32；不生成；Intl 登录 `Unsupported` | `trae_login_requires_operator_supplied_device_identity`、`international_login_is_unsupported` |
| 授权流 | 单活动流、加密设备 ID、trace 绑定、粘贴回调完成、取消 | `trae_paste_callback_completes_without_echoing_tokens`、`trae_callback_from_another_login_keeps_the_flow_pending` |
| token 换取 | `ExchangeToken` + 缺失 uid 时 `GetUserInfo` | `completion_exchanges_and_fills_missing_identity` |
| 刷新 | 提前 300 s；refresh token 轮换；CAS；租约；冲突用赢家 | `an_expiring_credential_is_refreshed_once_and_persisted_with_cas`、`a_cas_conflict_uses_the_winner_instead_of_refreshing_twice`、`another_process_holding_the_lease_is_waited_for_not_duplicated`、`a_storage_failure_surfaces_instead_of_a_silent_retry` |
| 401 重试 | 一次强制刷新后重试；第二次终止 | `a_401_on_chat_triggers_one_forced_refresh_and_retry` |
| 认证状态 | `auth_status_with_context`：Valid / Expired / Invalid / Restricted | `a_rejected_refresh_marks_the_account_invalid`、`a_fresh_credential_is_not_refreshed` |
| 额度 CN | `ide_user_ent_usage`；逐包 + 到期阶梯；Unknown 保持 null | `cn_quota_sums_credit_packs_without_converting_units`、`quota_without_a_pack_stays_unknown_never_zero` |
| 额度 Intl | `ide_user_pay_status`；仅订阅计划，数字 null | `intl_quota_reports_the_plan_and_leaves_numbers_unknown` |
| 额度 + 刷新 | 过期凭据先刷新再查询 | `quota_with_context_refreshes_before_spending_the_call` |
| 签到 CN | 状态查询；claim 受 `checkin.claim_enabled`；16 位数字设备号 | `src/checkin/trae.rs` 11 个离线测试 |
| 管理台 | 4 条路由 / 授权流 / 不回显 token | `src/admin/auth/trae.rs` + admin 3 个集成测试 |

### 11.2 Unsupported / 未验证

| 项 | 状态 | 说明 |
| --- | --- | --- |
| Intl 登录 | Unsupported | 无可信 OAuth 主机；不按 CN 猜测 |
| Intl 签到 | Unsupported | 无证据；零请求 |
| 自动生成设备号 | 不实现 | 设备指纹伪造红线 |
| 真实上游任何能力 | 未验证 | 无 TRAE 账号，未发真实请求 |
| Web UI 端到端 | 未点击验证 | JS 语法检查通过；后端路由由集成测试覆盖 |

### 11.3 与参考实现的差异（已记录）

1. 设备身份由运维提供，登录 hex32 与签到 16 位数字分离且不互相推导。
2. 回调只接受面板内粘贴，公开 GET 不完成授权，避免无认证写入凭据。
3. CAS 冲突时使用赢家凭据，绝不用已轮换的 refresh token 再刷一次。
4. 额度端点以 Coding2API 主路径为准；`usage_summary` 仅在响应自带时采用。
5. Intl 额度只报计划，所有数值保持 null（从不写 0 或无限）。
6. 签到成功必须回查 `checked_in` 且 credits 增加；`code:0` 本身不算成功。

### 11.4 下一步

- M14-F 尚未开始；TRAE 真实账号只读探针需凭据与显式授权。
- 若真实上游拒绝“诚实非 IDE 头 + 运维设备号”，记录 CompatibilityGap，不伪造头。
