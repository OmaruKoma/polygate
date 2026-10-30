# Loomy（讯飞桌面助手）接入说明

参考实现：`Patrick130306/loomy2api`（MIT），协议事实来自其 `docs/PROTOCOL.md`，
源头是官方 Electron 客户端 `0.9.38` 的明文主进程代码与 `.env.prod`。**只吸收协议事实，
未复制代码。**

## 1. 接入方式：导入会话，不做密码登录

Loomy 的模型网关用**登录会话本身**当 Bearer（`useSessionAuth: true`），没有 API Key。
会话可从桌面客户端的 `userData/auth-session.json` 取出，字段：

```json
{"session": "<32 字符>", "userid": "<18 位>", "phone": "...", "updatedAt": 1793337000}
```

PolyGate 只接收 `session` 和 `userid`：

- 不保存手机号、密码；
- 不实现手机号+密码登录（参考项目实现了，这里有意不做：它需要长期保存密码，
  收益只是 14 天续期一次的人工操作）；
- 不生成、不轮换设备身份（`devid` / `campus_device_id`），不领取激活/邀请奖励。

**会话 14 天有效且没有 refresh token**（`expire: 1209600`）。过期后上游返回 401，
账号详情显示“已过期”，重新导入即可；导入时会探测 `/models`，失败的会话不会入库。

## 2. 上游

| 用途 | 地址 |
| --- | --- |
| 模型网关 | `https://loomyad.xunfei.cn/api/v1` |
| 账号服务 | `https://account.xfinfr.com`（本适配器**不访问**） |

每个模型请求必须带：

```
Authorization: Bearer <session>
token: <session>                    # 两个头都要
traceparent: 00-<32hex>-<16hex>-01  # 缺失会挂到超时
loomy-version: 0.9.38               # 只检查存在
```

User-Agent 保持 `polygate/<version>`：上游只校验 `loomy-version`，不需要伪装客户端 UA。

## 3. 已实现能力

| 能力 | 状态 | 说明 |
| --- | --- | --- |
| 认证探测 | LiveVerified | `GET /models` 只读探测；401 → `Expired`，403 → `Invalid` |
| 模型发现 | LiveVerified | 13 条目录，按 `type == "chat"` 过滤后 10 个聊天模型 |
| Chat（非流式） | LiveVerified | |
| Chat（SSE） | LiveVerified | `reasoning_content` 映射为 `ReasoningDelta` |
| 工具调用 | 已实现（未实测） | `tools` / `tool_choice` 原样透传，响应映射为 `ToolCall` |
| 额度 | LiveVerified | `points/records` → `credits`；`remaining = availableBalance`，`total = balance + dailyBalance` |
| 会话续期 | Unsupported | 无 refresh token，只能重新导入 |
| 签到 / 领奖 | Unsupported | 不实现（每日额度由上游自动刷新） |
| 原生会话 / 取消 / Agent Task | Unsupported | 上游无对应接口 |
| 图片/视频模型 | 未接入 | 目录里的 `type == "image"` 条目不进入 `/v1/models` |

2026-10-30 实测（CN 账号）：

```
GET  /models                     → 200，13 条（chat 10 + image 3）
POST /chat/completions           → 200，spark-x2.6 返回 reasoning_content + points_consumed:0
GET  /points/records             → balance 7000 + daily 4953 = available 11953
PolyGate /v1/models              → 10 条 owned_by=loomy-cn
PolyGate /v1/chat/completions    → 200（非流式）与 SSE 均正常
```

## 4. 配置

```toml
[[providers]]
kind = "loomy"
region = "cn"          # 唯一部署；intl 会在启动时报配置错误
enabled = true
name = "Loomy CN"
verification = "experimental"
```

会话通过管理台导入：**Loomy → 添加账号 → 粘贴 session（可填 userid）**，
对应 `POST /admin/api/auth/loomy/session`。

## 5. 已知限制

- **模型目录只在启动时发现**：导入账号后需要重启网关，模型才会进入 `/v1/models`
  （导入接口会返回 `restart_required: true`，界面提示重启）。
- 不导入 `userid` 时，账号身份退化为“会话哈希”，换会话会新建账号而不是轮换凭据。
- 上游 `usage` 的 `points_consumed` 保留在额度原始数据里，不映射成 token。
  参考项目的用法是拿它扣额度，这里只用于统计，不做本地扣减。
- 图片模型（`Hy-Image-3.5-preview`、`doubao-seedream-5-lite`、`qwen-image-3.0-pro`）
  目前不暴露；接入需要 `/images/generations` 与多模态消息，另开里程碑。
