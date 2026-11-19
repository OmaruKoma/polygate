# TeleAgent（中国电信 星辰超级智能体）接入说明

分析对象：`C:\Program Files\TeleAgent`（v3.1.0 桌面版），2026-11-18 ~ 11-19。
协议事实来自桌面端明文源码（`app.asar`）、Go 引擎（`super-agent-code`）的 IDA
反编译、客户端内置 `llm_tap` 抓包，以及 2026-11-19 对真实上游的端到端实测。
**只吸收协议事实，未复制任何第三方代码。**

## 1. 接入方式：导入登录令牌，不做密码登录

网关不是 new-api，是电信自研 `superCowork` 网关（`Server: openresty` + `upms` OAuth），
只在协议层兼容 OpenAI。鉴权是三层叠加：

| 层 | 内容 | 说明 |
| --- | --- | --- |
| 产品常量 | `Authorization: Bearer sk-qvp4m6h9g20uM6PgYP7E` | 客户端内置，所有用户相同 |
| 账号身份 | `X-Token: <tenantId>:<JWT>` | 登录态，JWT 约 7 天有效 |
| 请求签名 | `X-SA-*` 五个头 | 只对 `POST /chat/completions` 必需 |

PolyGate 只接收 `X-Token` 和 `device-meta.json` 里的 `deviceId` / `installId`：

- 不保存手机号、密码，不实现登录；
- 不生成、不伪造设备身份；实测上游**不校验** `installId`/`deviceId`（任意 UUID 都能过），
  但仍原样转发，因为客户端始终发送；
- 不调用本地引擎 `127.0.0.1:4397`（其签名密钥只存在于 Electron 进程内存）。

令牌提取：`tools/teleagent-extract.cmd`（Windows，DPAPI + AES-256-GCM 解 `v10` 密文，
输出 `token` / `device_id` / `install_id` / 到期时间）。

## 2. 上游

| 用途 | 地址 |
| --- | --- |
| 模型调用 | `POST https://agent.teleai.com.cn/superCowork/sapi/api/v1/chat/completions` |
| 模型目录 | `GET  /superCowork/sapi/api/v1/model-options?language=zh-CN` |
| 积分余额 | `GET  /superCowork/sapi/user/portal/companyStore/userCenter` |

`model-options` 与 `userCenter` 只带 `X-Token` + `X-Nonce` + `X-Timestamp` 即可（实测）；
`chat/completions` 必须带签名头：

```
X-SuperAgent-Timestamp     Unix 秒
X-SuperAgent-Nonce         随机 UUID
X-SuperAgent-Sign-Version  "v1"
X-SuperAgent-Signature     见 §3
X-SuperAgent-Install-Id    device-meta.json 的 installId
X-SuperAgent-Device-Id     device-meta.json 的 deviceId
X-App-Version              客户端版本（默认 "3.1.0"）
X-Route-Target             "ops-gateway"
```

User-Agent 保持 `polygate/<version>`：上游不检查 UA，网关不冒充桌面客户端。

## 3. 签名算法（两段 HMAC-SHA256）

来源：Go 引擎 `(*GatewaySigner).Sign` @ `0x140d9a2e0`、`(*SuperAgentTransport).RoundTrip`
@ `0x140d9b740`。

```
jwt3 = X-Token 最后一个 "." 之后的部分

k1  = hex( HMAC-SHA256( key = jwt3,
        "superagent-auth-v1" / xToken / installId / timestamp / nonce ) )

sig = hex( HMAC-SHA256( key = k1,
        "superagent-auth-v1" \n METHOD \n uri \n timestamp \n nonce \n appVersion \n sha256hex(body) ) )
```

- 两段的 **key 都是十六进制字符串本身**（不是原始摘要）；
- `uri` 是 path + query，不含 host；
- **body hash 必须对实际发送的原始字节计算**。`llm_tap` 落盘的是 Go 重新序列化后的
  JSON（会转义 `<>&`、规范化空白），拿它重放签名必然 MISMATCH；
- 网关为此先把请求体序列化成 `Vec<u8>`，签名和发送用同一份字节。

黄金向量（Node 独立实现）写在 `providers::teleagent::signer` 的测试里，防止算法漂移。

## 4. 已实现能力

| 能力 | 状态 | 证据 |
| --- | --- | --- |
| 授权探测 | 已实现 | `GET /model-options`；`40108` → 已过期 |
| 模型目录 | 已实现 | `data.models[].model`；`chat-pro` 补文档值 400k/64k |
| 对话（非流式） | 已实现 | 实测 200，`reasoning_content` + `content` |
| 流式（SSE） | 已实现 | 实测 200，OpenAI chunk + `[DONE]` |
| 额度 | 已实现 | `current_points_balance`（字符串）→ credits；无总额，`total = null` |
| 工具调用 | 已实现（透传） | 上游是 OpenAI 协议；未用真实账号验证 |
| 视觉 | 未实现 | 目录里没有视觉模型 |
| 会话续接 / 取消 / 签到 | 不实现 | 上游无对应接口 |

额度实测（2026-11-19）：`{"current_points_balance":"9096.17","plan_display_name":"免费版"}`。

## 5. 配置

```toml
[[providers]]
kind = "teleagent"
region = "cn"
enabled = true
name = "TeleAgent CN"
verification = "experimental"
```

管理台「账号管理 → TeleAgent → 导入令牌」，粘贴 `tools/teleagent-extract.cmd` 输出的
`token` / `device_id` / `install_id`。

## 6. 已知限制

- **令牌 7 天过期，没有 refresh token**：上游返回 `40108` 时重新导入；
- `deviceId` / `installId` 上游不校验，但仍按客户端行为原样转发；
- `model-options` 的 `context_limit` 恒为 `null`，只有 `chat-pro` 用文档值（400k/64k）；
- 模型自述会被上游 persona 包装（实测 `chat-pro` 回答自己是 GLM），不能靠自述判断后端；
- 错误码：`4001` 缺/无效 `X-Token`、`40101` 缺签名头、`40107` 缺用户身份、
  `40108` 令牌无效（见 `docs/TELEAGENT_MIGRATION.md` §12 的完整表）。
