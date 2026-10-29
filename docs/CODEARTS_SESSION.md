# CodeArts 会话墙、本地源码审计与 Provider 接入（M13-A / M13-B）

> 日期：2026-10-29
> 范围：M13-A 源码对照/会话绑定；M13-B Provider 接入与签到执行器。**未启动本地 Go 服务、未调用真实 CodeArts 上游、未使用真实凭据**。
> 相关代码：`src/session_service/`（含 `router.rs`）、`src/providers/codearts/`、`src/checkin/codearts.rs`、`src/storage/sessions.rs`、`migrations/0004_session_binding_revision.sql`。
> 证据等级：**已证实**＝源码/测试夹具直接可见；**强推断**＝多个独立实现相互印证但本次未复现；**推测**＝单一实现注释或作者判断。

## 1. 结论摘要

1. 本地 `~/codearts2api` 的工作区改动（HEAD `f9cb52c` 之上，4 个文件 +298/−3）**没有改变账号选择算法**；改动集中在“上游会话槽”和 `TM.00001041` 错误处理。
2. 会话墙的硬证据是上游错误体：`TM.00001041` + 「并发会话数已达上限(3个)，请关闭部分会话后重试。」；HTTP 状态实测可走 **400**，也可走 429，流内错误事件也可能承载。
3. 限制是**账号级**、**跨模型**的，不是模型级；代码注释与独立实现都指向“每账号 3 个上游会话”，且会话在流结束后仍滞留数十秒，所以它不等于“同时 3 个 HTTP 请求”。
4. Coding2API 用“稳定 `chat_id` + 每账号 3 次启动/60s 的滑动窗口”解决；Johnsheng1 原版完全没有会话管理；本地修改版用“全局 2 个固定会话槽 + 两个会话头”解决。三者不可同时照搬。
5. PolyGate 采用**每下游会话独占绑定**：`(client_session_id, kind, region) → account_id → upstream_session_id`，所有变更走 `binding_revision` CAS；失效只标记、不删除、不静默重建。
6. 本地修改版存在全局槽、跨账号共槽、固定槽 ID 跨进程复用、`conversation.Store` 读改写竞争等问题，不能直接当作正确实现。

## 2. 本地修改版相对原版的差异

### 2.1 diff 总览

审计对象：`~/codearts2api`，远端 `github.com/handsomedogx/codearts2api`，HEAD `f9cb52c`（2026-10-15），未提交工作区改动。

```text
$ git diff --stat
 internal/auth/pool.go       |  11 +++
 internal/codearts/client.go | 208 +++++++++++++++++++++++++++++++++++++++++++-
 internal/config/config.go   |  10 +++
 internal/server/server.go   |  72 +++++++++++++++
 4 files changed, 298 insertions(+), 3 deletions(-)
```

改动按主题可分为三组：

| 主题 | 文件 | 作用 |
| --- | --- | --- |
| 会话槽与 `TM.00001041` | `internal/codearts/client.go`、`internal/auth/pool.go` | 固定上游会话 ID、会话头、cap 错误分类、独立重试预算、不冷却 |
| 配置 | `internal/config/config.go` | `SessionSlots`、`SessionCapRetrySeconds`、`SessionCapMaxRetries`、Benefit 模型列表 |
| 无关的调试代理 | `internal/server/server.go` | `/api/ui/codearts/proxy` + `PostJSON`；与会话无关 |

### 2.2 账号选择逻辑：**未修改**

- HEAD 的 `Pool.Candidates`（`internal/auth/pool.go:120-190`）仍然决定候选：凭据可用、未冷却、未 `needs_relogin`、未达账号并发、模型未冷却；按 `UpdatedAt` 降序；有 `PreferredAccountID` 时把它移到最前；再按 `round_robin` / `random` / `weighted` 排序。
- 工作区 diff 没有触碰 `Candidates`、`usable`、`weightedOrder` 或 `RoutingStrategy`。
- 新增的 `SessionSlots` 只是把“全局同时进入上游的 chat 流”限制为 2 个，**不参与选号**。
- 结论：本地修改版没有新增账号选择逻辑；会话偏好仍由 `conversation.Store.AccountID → PreferredAccountID` 提供（`internal/server/server.go:646`）。这与 PolyGate 目标中的“Scheduler 公平选号”无关，后者仍需单独实现。

### 2.3 会话 ID 的创建、复用与销毁

| 实现 | 创建 | 复用 | 销毁 |
| --- | --- | --- | --- |
| Johnsheng1 原版 | 无 `chat_id`、无会话头（`server.py:119-161,536-583`） | 无 | 无 |
| 本地 HEAD（未改） | `chatID = request.ChatID`，空则 `randomHex(16)`；写入 body `chat_id` + `prompt_cache_key`（`git show HEAD:internal/codearts/client.go:290-303`） | 同一 conversation 通过文件存储复用；换账号时 `attemptRequest.ChatID=""`（HEAD `:155-158`） | 无显式销毁；任其自然过期 |
| 本地工作区（改后） | `New` 预生成 `SessionSlots` 个确定性 32-hex ID（`client.go:80-102`） | **每次请求**从全局通道取一个槽 ID，**覆盖** `request.ChatID`（`client.go:343-350`） | 不销毁；流结束后把槽 ID 放回通道（`client.go:529-533`） |
| Coding2API | `derive_chat_id(payload) = sha256(messages)[:32]`；请求体带 `chat_id` + `prompt_cache_key`（`client.py:71-89,253-256`） | 同一消息历史得到同一 `chat_id`，无需服务端会话状态 | 无；靠上游过期 |

关键事实：

- 本地修改版**没有**“每下游会话一个上游会话”的映射。槽 ID 是进程级并发令牌，不是会话身份。
- `x-ot-session-id` / `user-session-id` 两个头在本地修改版中被设置为同一个 `chatID`（`client.go:377-386`）。这是本地作者对“上游按什么计会话数”的判断；**仓库内没有抓包或响应证据**证明只有这两个头生效（见 §3.4）。
- `internal/conversation/store.go` 仍会持久化 `ChatID`，但修改后保存进去的是槽 ID（`server.go:678,750`），因此 README 宣称的“稳定 chat_id 保留 prompt cache 亲和”在槽模式下实际失效。

### 2.4 请求与上游会话的绑定关系

本地 HEAD：

```text
conversation id / history hash
   → conversation.Context { account_id, chat_id }
   → PreferredAccountID + ChatID
   → 若实际账号 != preferred：ChatID 清空，重新随机
```

本地工作区：

```text
任意下游会话
   → 全局 sessionSlots（2 个固定 ID）
   → 覆盖 ChatID / x-ot-session-id / user-session-id
```

因此实际绑定是“**请求 → 槽位 → 账号**”，不是“下游会话 → 上游会话”。不同下游会话会复用同一个槽 ID；同一账号被两个槽同时使用时，两个槽 ID 是同一个账号下的两个上游会话。账号切换仍由候选顺序决定，槽 ID 不随账号变化。

### 2.5 并发请求处理

- 账号级：`Pool.Begin/Finish` 用互斥锁维护进程内 `inFlight`，默认 `AccountMaxConcurrency=4`（`pool.go:83-109`、`config.go:138`）。达到上限的账号从候选中消失，**没有账号级排队**。
- 全局级：`sessionSlots` 容量默认 2、上限 3（`client.go:80-94`），整个 `Client` 共享；等待只受请求 `context` 约束（`client.go:529-535`），没有独立队列超时。
- 会话墙重试：`benefit_cap` 不消耗共享 `attempts/`时间预算，单独 `SessionCapMaxRetries`（默认 8）与 `SessionCapRetrySeconds`（默认 3s）；`wait` 不占用退避序号（`client.go:173-183,218-232,262-276`）。
- 失败处理：`benefit_cap` 在 `MarkFailure` 中直接返回，不写模型冷却（`pool.go:354-368`），下游映射为 429（`server.go:836-847`）。

### 2.6 会话失效后的恢复

- 修改版只处理“会话数打满”，不处理“某个 chat_id 已失效”。没有 `chat_id` 轮换、没有每会话重建、没有失效确认。
- 账号认证失败仍走原有逻辑：401/403 → `auth` / `needs_relogin`；凭证刷新失败 → `credential_refresh`；与本次 diff 无关。
- 槽 ID 永不轮换，意味着若上游真的让某个会话失效，修改版会继续复用同一 ID，无法通过“换会话”恢复。

### 2.7 已修复的问题（相对 HEAD / 相对 Johnsheng1）

1. **HTTP 400 + `TM.00001041` 被误判为不可重试的 `client`**：HEAD 的 `classifyStatus` 对 400 返回 `("client", false)`（`git show HEAD:internal/codearts/client.go:823-835`），并发打满会直接失败；修改版在响应体命中时返回 `benefit_cap`（可重试，429）。
2. **cap 错误消耗共享重试预算**：修改版把 cap 重试从 `attempts` 预算中剥离，避免“短暂打满 → 直接 429”。
3. **cap 错误触发模型冷却**：修改版跳过 `MarkFailure` 的冷却计算，避免一次打满锁模型 10 分钟。
4. **会话数不受控**：修改版固定槽 ID 并发送会话头，试图把每账号会话数压到 2（≤3）。
5. **流内错误**：修改版对 SSE 内部的 `TM.00001041` 也转成 `benefit_cap`（`client.go:461-467`），不再当普通错误结束。

### 2.8 可能存在的竞态与缺陷（不得默认为正确）

| # | 问题 | 位置 | 影响 |
| --- | --- | --- | --- |
| 1 | 槽通道挂在 `Client` 上，**跨账号全局共享** | `client.go:39-43,80-94` | 多账号时总并发被压到 2–3，而不是“每账号 3”；账号多时严重低负载 |
| 2 | 槽 ID 由固定字符串哈希而来，**跨进程相同** | `client.go:99-102` | 两个网关进程各持有 2 个槽，但槽 ID 相同；合计可能 4 个上游会话，超过 3 |
| 3 | 取槽后**无条件覆盖** `request.ChatID` | `client.go:343-350` | 下游会话/账号偏好失效；prompt cache 亲和丢失；不同会话可能共用同一上游会话 |
| 4 | 槽不按账号隔离 | `client.go:529-533` | A 账号的槽被 B 账号请求复用；上游会话 ID 与账号的对应关系只剩“当前这次请求” |
| 5 | `attempts--` 与 `capRetries` 混用 | `client.go:218-232` | 逻辑可读性差；cap 重试期间普通重试预算被冻结，长时间打满时其他错误得不到重试 |
| 6 | `Begin` 失败直接 `break` 到下一账号 | `client.go:183-186` | 账号并发满时没有有界等待；热点账号的请求被推给其他账号，破坏粘性 |
| 7 | `conversation.Store` 读改写无锁 | `internal/conversation/store.go` | 同一会话并发两轮时后写覆盖前写，`Turns`/`HistoryHash` 可能丢失 |
| 8 | 槽释放依赖 `defer`，流生命周期受 5 分钟 HTTP 超时约束 | `client.go:80,343-350` | 长流会长期占用槽；客户端断开时是否能及时释放取决于 context 传播 |
| 9 | 没有会话级串行化 | 全文件 | 同一 `chat_id` 的两个并发请求可能同时写上游会话 |

## 3. CodeArts 会话墙研究

### 3.1 触发条件与作用范围

- **触发**：同一账号的上游会话数达到 3。上游返回错误码 `TM.00001041`，错误文本（测试夹具中的实测原文）：
  `{"error_code":"TM.00001041","error_msg":"并发会话数已达上限(3个)，请关闭部分会话后重试。"}`
- **HTTP 形态**：实测可走 **HTTP 400**（Coding2API 注释明确写“并发超限走 400 + `TM.00001041`”），也可走 429；本地修改版对两者都从响应体识别。
- **流内形态**：HTTP 200 的 SSE 内也可能出现 `error_code=TM.00001041` 的错误事件（Coding2API `tests/regression/test_stream_slot_release.py:75-82`；本地 `client.go:461-467`）。
- **作用域**：**账号级、跨模型**。同一账号下任何模型共用这 3 个会话；错误不点名模型，不应写成模型级冷却。
- **与并发数的关系**：不是严格的“同时 3 个 HTTP 请求”。Coding2API 的实现注释记录：会话在 HTTP 流结束后仍滞留数十秒，3 并发打满后单请求约 68s 才恢复；因此它用“每账号每约 60s 最多 3 次启动”的滑动窗口，而不是纯在途上限（`src/tasks/pacer.py:16-40`、`src/config.py:257-271`）。
- **与账号的关系**：是。不同账号各自有 3 个会话额度；一个账号打满不影响其他账号。
- **与上游会话的关系**：是。计数对象是上游会话（`chat_id` / 会话头），不是 token、模型或 IP。

### 3.2 源码位置与证据

| 证据 | 位置 | 内容 |
| --- | --- | --- |
| 错误分类 | `.refs/RobbsLuo_Coding2API/src/provider/codearts/events.py:89,384-399` | `TM.00001041`/“并发” → `CONCURRENCY`；400/429 响应体识别 |
| 响应用例（实测原文） | `.refs/RobbsLuo_Coding2API/tests/provider/codearts/test_events.py:244-260` | 400 + `并发会话数已达上限(3个)` → `CONCURRENCY` |
| 流内错误 | `.refs/RobbsLuo_Coding2API/tests/regression/test_stream_slot_release.py:75-82` | 已输出正文后收到 `TM.00001041` 错误事件 |
| 节流窗口 | `.refs/RobbsLuo_Coding2API/src/tasks/pacer.py:16-40`；`src/config.py:257-271` | 每账号 3 并发、60s 窗口；会话滞留约 60s |
| 本地识别 | `~/codearts2api\internal\codearts\client.go:417-430,461-467,546-548` | 响应体/流内命中 `TM.00001041` 或“并发会话数已达上限” → `benefit_cap` |
| 本地不冷却 | `internal/auth/pool.go:354-368` | `benefit_cap` 不写模型冷却 |
| 本地槽 | `internal/codearts/client.go:80-102,343-350,529-533` | 全局 2 槽、覆盖 ChatID、发送会话头 |

**请求证据缺口**：仓库里没有保存触发请求的原始头/体，也没有成功绕过会话墙的对照抓包。因此“两个会话头才是计数键”只有本地作者注释，未达到“已证实”。

### 3.3 是否存在明确的官方限制

- 未找到华为云公开文档明确写“CodeArts Agent 每账号 3 个会话”。能拿到的最强证据是上游自己的错误文本 `并发会话数已达上限(3个)`，它直接给出了数值 3。
- 该限制的**计时窗口/会话存活时长**没有官方数值；Coding2API 的 60s 是其实测与节流器设计，不是官方声明。
- 因此实现上只能“保守限制 + 遇到错误可解释地重试”，不能宣称完整复刻官方语义。

### 3.4 已证实 vs 推测

| 结论 | 等级 |
| --- | --- |
| 每账号最多 3 个上游会话；错误码 `TM.00001041`；错误文本含“并发会话数已达上限(3个)” | **已证实**（上游错误体 + 两个独立实现） |
| 可出现在 HTTP 400、429、以及 HTTP 200 的流内错误事件 | **已证实**（Coding2API 测试夹具与本地代码分支） |
| 账号级、跨模型，不是模型级 | **强推断**（错误文本不含模型；两个实现都按账号/并发处理） |
| 会话在流结束后仍滞留数十秒、有效口径约“每账号每 60s 3 次启动” | **强推断**（Coding2API 源码注释称实测；本次未复现） |
| `x-ot-session-id` / `user-session-id` 才是上游计会话的键，body `chat_id` 不是 | **推测**（仅本地修改版注释；无抓包） |
| 固定 32-hex 槽 ID 可被上游接受并复用 | **推测**（本地实现选择；未见响应验证） |
| 同一 `chat_id` 的并发请求会破坏上游会话 | **推测**（无证据；PolyGate 仍提供可选串行化） |

## 4. PolyGate 会话映射设计

### 4.1 数据模型

复用 `sessions` 表与 `SessionRepo`，新增迁移 `0004_session_binding_revision.sql`（旧迁移未改）：

```text
sessions
  UNIQUE (client_session_id, provider_kind, region)
  account_id            → accounts(id) ON DELETE CASCADE
  upstream_session_id   → nullable；只属于该 account_id
  state                 → active | expired | invalid
  binding_revision      → CAS 令牌（迁移 0004）
  invalid_reason        → 失效/重置原因（迁移 0004）
  expires_at / last_used_at / request_count
```

对应关系：

```text
Client Session
  → Provider Kind   ┐
  → Region          ├─ 组成唯一键；CN/Intl 永不共享
  → Account ID      ┘
  → Upstream Session ID（可空；写入时校验 account + revision）
```

### 4.2 生命周期与不变式

| 操作 | 行为 |
| --- | --- |
| `bind_if_absent(key, account_id, upstream?)` | 仅当键不存在时插入；已存在则原样返回（`created=false`）。**不会**因为调用方建议另一个账号而改绑 |
| `attach_upstream(key, account, revision, id)` | 仅当 `account_id` 与 `binding_revision` 都匹配时写入；否则返回 `session_binding_invalid`。防止 A 账号的 `chat_id` 写到 B 账号 |
| `acquire(key)` | 校验存在、未过期、`state=active`、账号存在/启用/认证可调度；然后取会话锁 + 账号槽。缺失/过期/失效**不创建、不迁移、不删除** |
| `finish()` | 原子 `request_count+1` 并延长 TTL；释放会话锁和账号槽 |
| `invalidate(key, reason)` | `state=invalid`，保留 account 与 upstream id，记录原因；幂等 |
| `reset_upstream(key, reason)` | 保持账号，清空 upstream id，`state=active`，延长 TTL，记录原因；适配器必须重发完整历史 |
| `rebind(key, account_id)` | 显式换账号；upstream id 清空；CAS 防止并发改绑 |
| `recover(key, strategy)` | 显式选择 `ResetUpstream` 或 `Rebind`；没有“自动静默恢复” |

### 4.3 并发与等待

`src/session_service/limits.rs`：

- **账号级在途上限**：`min(max_in_flight, accounts.max_concurrency)`；CodeArts 的 `max_in_flight` 取 3。
- **滑动窗口**：`max_starts=3` / `window=60s`；按启动时刻计数，流结束不退还窗口配额（对齐会话滞留）。
- **会话级串行化**：`serialize_session=true` 时，同一 `(kind, region, client_session_id)` 的请求排队；不同会话互不阻塞。
- **有界等待**：整个获取过程共用一个 `queue_timeout`（CodeArts 75s，略长于观测到的约 60s 会话滞留窗口，第 4 个突发请求会等待而不是立刻失败）；超时返回 `NoCapacity`（503），不会无限等待。
- **取消安全**：future 被取消时信号量 permit 与互斥锁 guard 随 drop 释放。
- 进程内状态：账号槽与窗口是**进程内**的；数据库只持久化绑定，不持久化并发租约。多实例部署时需要在后续 Scheduler 阶段改为数据库租约（见 §6）。

### 4.4 会话失效与恢复

- 上游明确拒绝会话（适配器映射为 `ErrorKind::SessionBindingInvalid`）→ `classify_session_failure` 返回 `UpstreamSessionInvalid`。
- 恢复只能由调用方显式选择：
  - `ResetUpstream`：同账号、新上游会话；完整历史重发；旧 ID 不保留。
  - `Rebind`：换账号；新账号必须 kind/region 匹配；upstream id 清空。
- `acquire` 对 `invalid`/过期绑定返回 `SessionBindingInvalid`，并在消息中带 `invalid_reason`；行不会被删除，也不会被 `bind_if_absent` 静默替换。
- 账号不可用：禁用 → `AccountDisabled`；认证过期 → `AuthExpired`；无效 → `AuthInvalid`；受限 → `ChannelRestricted`。绑定保持原账号，由 Scheduler 决定等待还是显式 rebind。

### 4.5 与未来 Scheduler 的边界

- 本阶段**不实现**账号轮询、额度感知、公平负载均衡；`SessionBindingService` 不选号。
- Scheduler 的职责：
  1. 为新会话选一个候选账号，调用 `bind_if_absent`；
  2. 后续请求先 `get`/`acquire`，把 `session.account_id` 作为硬约束；
  3. 账号不可用时决定“等待原账号”还是显式 `rebind`；不得静默改绑；
  4. 用 `SessionRepo::list_by_account` / `count_active_by_account` 观察绑定分布。
- `Gateway::sessions()` 已暴露该服务；M13-B 新增 `SessionRouter` 并接入 CodeArts Chat（见 §7），Mock 仍使用占位凭据。

## 5. 已验证行为

新增 14 个服务层测试（`src/session_service/tests.rs`，全部使用临时 SQLite；并发测试使用短真实等待，不访问网络）：

1. 绑定是 create-only 且账号粘性优先于调用方建议。
2. 两个并发 `bind_if_absent` 恰好一个成功，账号一致。
3. 相同 client id 的 CN/Intl 绑定互不共享，upstream id 不同。
4. `attach_upstream` 拒绝错误账号与过期 revision，不覆盖既有 id。
5. `rebind` 换账号并清空 upstream id；仓库级 stale CAS 失败。
6. `invalidate` 保留账号与 upstream id、记录原因、阻断 `acquire`、幂等。
7. `reset_upstream` 保留账号、清空 upstream id、恢复可获取。
8. 过期绑定不会被 `bind_if_absent` 续期或替换，必须显式恢复。
9. 禁用账号 → `AccountDisabled`；认证失效 → `AuthInvalid`。
10. 账号槽上限生效，释放后等待者被唤醒。
11. 队列超时返回 `NoCapacity`。
12. 同一会话串行化，其他会话不受阻塞。
13. 绑定在关闭并重开数据库后仍可读、可获取。
14. 会话失败分类表稳定。

命令与结果：

```text
$ cargo test session_service
test result: ok. 14 passed; 0 failed

$ cargo test
test result: ok. 210 passed; 0 failed
```

M13-B 追加 6 个 `SessionRouter` 测试（粘性、跨账号隔离、失效重置、认证失败持久化、无账号
503、lease drop 释放 permit），以及 CodeArts Provider/签到/运行时测试；全量 268 个测试通过。

**未验证**：任何真实 CodeArts 请求、会话头语义、3 会话窗口的真实时长、跨进程并发。

## 6. 未解决问题

1. **会话头语义未证实**：`x-ot-session-id` / `user-session-id` 是否真是计数键、是否必须参与签名、是否与 body `chat_id` 冲突，需要真实账号抓包/对照实验。
2. **固定槽与每会话绑定的冲突**：上游上限是每账号 3 个会话；PolyGate 的设计是每下游会话一个 upstream id。若客户端会话数 >3，必然触发会话墙。M13 的 Provider 需要决定：把绑定会话映射到有限槽（牺牲亲和）还是拒绝/排队（保亲和）。本地修改版选择了前者，Coding2API 选择了窗口 + 稳定 chat_id。
3. **多实例**：绑定在 SQLite，但账号槽/窗口/会话锁是进程内的；两个网关进程会各自放行 3 个并发，合计超过上游上限。需要数据库租约或单实例部署约束。
4. **会话存活时长**：60s 来自参考实现注释，未独立测量；窗口参数可能需要按真实账号调整。
5. **会话失效没有独立错误码**：没有找到“chat session expired”这类明确字符串；适配器只能把 4xx/业务码映射为 `SessionBindingInvalid`，可能误判。需要 M13/M14 真实请求验证。
6. **没有上游会话删除**：本地失效/重置只改本地记录，上游会话仍占用额度直到自然过期；不能通过“删除会话”立即释放 3 个名额。
7. **没有 HTTP 管理面**：绑定查询/失效/重置只有服务层接口，未暴露管理端点或 UI。
8. ~~未接入 Chat 路由/Provider~~：M13-B 已解决（`SessionRouter` + `CodeArtsProvider`，见 §7）；真实上游仍未验证。

## 7. M13-B 实现状态（2026-10-29）

### 7.1 接线方式

```text
POST /v1/chat/completions
  └─ ModelCatalog → ProviderKey(kind, region)
  └─ SessionRouter::open(SessionKey{client_session_id, kind, region})
       ├─ 绑定存在 → acquire（账号槽 + 会话锁）
       ├─ 不存在   → 选第一个可用账号 → bind_if_absent（写入新 32-hex 上游 ID）→ acquire
       ├─ 无上游 ID → attach_upstream（account + binding_revision CAS）
       └─ AccountService::authorization_context_for → Credential
  └─ CodeArtsProvider::chat_with_context / chat_stream_with_context
       └─ chat_id = context.upstream_session_id（prompt_cache_key 同值）
  └─ 成功后 finish（touch TTL）；失败按 SessionFailure 恢复
```

- SSE：`leased_stream` 把 `RouteLease` 与 provider stream 绑定；流结束/断开才释放租约。
- `client_session_id`：优先 `x-session-id` / `x-polygate-session-id` / `session_id`；
  没有时用 `model + 首条 user 消息` 的 SHA-256 前缀（`auto-…`），保证同一会话多轮稳定。
- `TM.00001041` / `并发` → `RateLimited`（`AccountBusy`），**不**改绑、**不**无限重试。
- `SessionBindingInvalid` → `reset_upstream`（保留账号，丢弃上游 ID，重发完整历史）。
- `AuthInvalid` / `AuthExpired` / `AccountDisabled` / `ChannelRestricted` →
  持久化账号 AuthStatus；绑定与账号均保留。

### 7.2 与本地修改版的关键差异

- 不复制全局固定 32-hex 槽；每个 `(client session, kind, region)` 生成独立上游 ID。
- 不覆盖客户端/会话存储中的 `chat_id`；`chat_id` 只来自绑定，账号间绝不共享。
- 会话槽不跨账号共享；`acquire` 前已校验账号 kind/region/启用/认证状态。
- 有限并发由账号槽 + 启动窗口 + 会话锁实现，有统一 deadline，不靠新建会话绕过。

### 7.3 仍未验证（真实账号）

1. 签名/STS/AK-SK 是否被 APIG 接受；refresh + DPoP 是否能换票。
2. `chat_id` 是否真是会话计数键，以及 3 会话/60s 滞留的真实边界。
3. `builtinModels` / `statistics/plugin` / `/v1/ops/*` 的真实字段与状态。
4. 本设计在 >3 个活跃下游会话时的表现：目前用窗口/排队缓解，但未决定“槽位复用 vs 拒绝”。
5. 跨进程/多实例下的账号槽与上游会话计数。

以上验证完成前，Provider 保持 `experimental`，不得写入 `verified`。
