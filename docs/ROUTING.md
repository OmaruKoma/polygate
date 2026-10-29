# 模型路由与多账号调度

## 1. 名称解析顺序

请求里的 `model` 依次尝试：

1. **别名**（`model_routes` 表）：客户端只看到别名，实际目标由一个或多个
   Provider/上游模型组成。
2. **原生模型 ID**：启动时从已启用 Provider 发现。
3. **`provider::model`**：同名模型被多个 Provider 提供时的显式消歧，例如
   `qoder-cn::gmodel`。

规则：

- 别名语法：1–64 个字符，仅 `[A-Za-z0-9._-/]`，不能包含 `::`。
- 别名**不能**与任何原生模型 ID 同名；创建时直接拒绝。
- 别名不会递归解析：目标必须指向原生上游模型 ID。
- 同名原生模型且未用 `provider::` 消歧时返回 `400`，并列出候选 Provider。
- 未知模型返回 `404`，不会发起任何上游调用。

## 2. 能力校验

解析阶段会根据请求内容推导需求：

| 需求 | 触发条件 |
| --- | --- |
| `streaming` | `stream: true` |
| `tools` | 请求带非空 `tools` 或 `tool_choice` |
| `vision` | 消息里出现图片块（`image_url`/`image`/`input_image`/`image/*`） |

目标的生效能力 = 目标上的显式覆盖，未覆盖时沿用 Provider 声明。缺少所需能力时
直接返回 `400`（例如“does not support tool calling”），**不会把工具或视觉请求
发给没有声明该能力的模型**。

## 3. 多目标与优先级

同一别名可以挂多个目标（管理台“模型管理 → 新建别名”，或
`POST /admin/api/model-routes`）：

- `priority`：越大越先尝试。
- `weight`：同优先级内的账号/目标权重（1–1000）。
- `enabled`：单独启停某个目标；全部禁用时该别名从 `/v1/models` 消失，调用返回
  `404`。
- 同一别名的 `(provider, upstream_model)` 组合唯一。

## 4. 账号调度

候选账号先经过 `AccountService` 过滤（启用、认证可分发、凭据存在），再由
`src/scheduler/` 选择：

| 策略 | 行为 |
| --- | --- |
| `round_robin` | 按 `(kind, region)` 严格轮询 |
| `smooth_weighted_round_robin`（默认） | nginx 式平滑加权轮询，使用账号 `weight` |
| `least_connections` | 选择当前在途请求最少的账号 |

过滤规则：

- **额度**：最近一次快照 `remaining <= 0` 的账号被排除；`quota_min_ratio > 0` 时低于
  该比例的账号也被排除。**未知额度永不排除**，也不会被写成 0。
- **冷却**：连续传输错误/超时/5xx 达到 `failure_threshold` 后按指数退避冷却
  （`cooldown_base_secs` → `cooldown_max_secs`）。客户端错误、429、402、认证失败
  不触发冷却。
- 不同 Provider/Region 的账号互相隔离；CN/Intl 凭据不会串用。

所有调度状态在进程内、按账号数量有界，无后台任务、无全局异步大锁。多进程共享同一
SQLite 时，凭据刷新通过 `credential_refresh_leases` 跨进程租约 + 修订号 CAS 协调。

## 5. 会话粘性

- `x-session-id` 存在时，同一 session 始终落在同一账号，并复用上游 session id
  （Provider 支持时）。
- 没有该头时，网关用 `model + 首条用户文本` 派生稳定的 `auto-*` id。
- 同一 session 的请求串行执行（per-session gate），不同 session 可并发。
- 账号被显式排除（如 402 后）时允许改绑，但**不会沿用旧上游 session id**。

## 6. 故障切换与重试

```
目标 1 → 账号 A（失败） → 账号 B → … → 目标 2 → …
```

- 只有**尚未产生任何可见输出**且失败可安全重放时才切换：认证失败、账号禁用、渠道
  受限、429、402、本地无容量。
- 传输错误、超时、5xx 属于“结果不确定”：直接返回错误，**绝不自动重发**，避免上游
  已执行却被重复计费/重复执行。
- 模型不可用（404/模型标记）视为目标级失败，直接尝试下一个目标。
- 切换账号时会排除已失败账号；当没有其他账号时返回最初的业务错误（例如 402），
  而不是笼统的 503。
- 流式请求：连接建立前可切换；开始传输后任何错误只以流内错误事件结束，不会拼接
  第二段回答。
- 每个请求最多 `3` 次调度尝试（`src/api/mod.rs` 的 `MAX_DISPATCH_ATTEMPTS`）。

## 7. 管理接口

| 接口 | 用途 |
| --- | --- |
| `GET /admin/api/model-routes` | 别名 + 原生模型目录 |
| `POST /admin/api/model-routes` | 新建别名目标 |
| `PATCH /admin/api/model-routes/{id}` | 修改显示名/优先级/权重/能力/启停 |
| `DELETE /admin/api/model-routes/{id}` | 删除目标 |
| `GET /admin/api/scheduler` | 当前策略与每个账号的在途/连续失败/冷却 |
| `GET /admin/api/logs?limit=200` | 最近请求日志 |
| `POST /admin/api/logs/purge` | `{"older_than_days":30}` 或 `{"all":true}` |

所有管理写操作要求登录会话 + CSRF 头。
