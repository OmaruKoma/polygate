# Web 管理台（M13-C1）

> 状态：**代码完成，Mock 流程已验证；真实 CodeArts 登录未验证**（无授权账号，未发出真实上游请求）。
> 本文描述已实现内容、安全边界与未验证项；进度以 `PROGRESS.md` §25 为准。

## 1. 目标与边界

M13-C 阻塞的根因不是 Provider 本身，而是“没有一条正常、安全的加号路径”。本阶段把账号接入的
正式入口定为 **Web 管理台**：浏览器内完成 CodeArts 授权、凭据加密保存、账号状态查看与日常管理。

实现范围：

- 嵌入式静态前端（`web/`，HTML/CSS/原生 JS，`rust-embed` 打包进二进制），无 Node 运行时、无 CDN。
- 管理 API（`/admin/api/*`）与首次设置、登录、恢复码、会话、CSRF、Origin/Host 校验。
- CodeArts 浏览器授权（PKCE + DPoP）适配器，以及授权流程状态机。
- 账号列表/详情/改名/启停/删除/本地状态/真实探测/额度刷新/签到记录。
- Dashboard、模型、签到、用量只读视图，全部来自现有服务。

明确不做：CLI 账号 CRUD、旧 codearts2api/DPAPI 导入、跨机加密迁移包、完整多账号负载均衡、
其他真实 Provider、Anthropic/Responses 新协议、M13-C2（真实联调）。

## 2. 模块布局

```text
src/admin/
├── mod.rs            # 嵌入式资源、AdminState/AdminSettings、路由、安全响应头、错误 JSON
├── auth/
│   ├── mod.rs        # 首次设置、登录、恢复码、会话、Cookie、CSRF、Origin/Host、限速
│   └── flow.rs       # 授权流程状态机：start/status/cancel/complete
├── account/mod.rs    # 登录结果落库（身份去重、凭据 CAS、视图脱敏）
├── api/mod.rs        # Dashboard/账号/模型/签到/用量/授权端点；OAuth 回调页
└── tests.rs          # Mock STS + 真实 Axum Router 集成测试
web/
├── index.html        # 设置/登录/应用外壳（侧栏、主题、模态框）
├── app.css           # 深/浅色变量、响应式布局
├── app.js            # API 封装、路由、账号流程、CSRF
└── oauth-callback.js # 回调页向 opener 发送结果并关闭窗口
```

依赖方向：

```text
admin ──> account_service, api::ModelCatalog, checkin, providers::codearts::login,
          security::SecretBox, storage::AdminRepo, error
admin ──✗──> sched/直接 SQLite/具体 Provider 请求（除 CodeArts 登录适配器）
```

处理器不直接写 SQL；账号与凭据写入统一走 `AccountService` / `CredentialService`。CodeArts 协议
（URL 参数、PKCE、DPoP、token 交换、身份查询）集中在 `providers::codearts::login`，管理台只编排流程。

## 3. CodeArts 授权方式（源码依据）

参考本地 `~/codearts2api`（操作者本人作品，仅吸收协议事实，独立实现）：

| 事实 | 来源 |
| --- | --- |
| 门户 `/portal/authorize` 参数：`client_id`、`uri_scheme=codearts-snap`、`port`、`code_challenge`、`code_challenge_method=SHA-256`、`ticket_id`、`auth_callback_url`、`plugin-name=codearts-snap_vscode`、`plugin-version=26.7.2` | `internal/auth/manager.go:160-190` |
| Intl 额外加 `quickcompfalg=1` | `internal/auth/manager.go:186`、`internal/config/config.go` |
| PKCE verifier 为 64 随机字节的十六进制（128 字符），challenge = base64url(SHA-256(verifier)) | `internal/auth/dpop.go:36` |
| 每次流程生成新的 P-256 密钥对，ES256 DPoP JWT | `internal/auth/dpop.go:46-80` |
| 交换：`POST {sts}/v1/oauth2/tokens`，form `client_id`、`code`、`code_verifier`、`grant_type=authorization_code`、`redirect_uri`，头 `DPoP` | `internal/auth/manager.go:196`、`internal/config/config.go:29` |
| 身份：`GET {sts}/v5/caller-identity`，SDK-HMAC-SHA256 签名 | `internal/config/config.go:30` |
| Token 响应：`credentials.access_key_id/secret_access_key/security_token/expiration` + `refresh_token` | 同上 |

实现差异（有意为之，不复制缺陷）：

- **不复用** codearts2api 的全局 session-slot 设计；本实现一个实例同时只允许一个授权流程，因为
  回调只带 `code`、不带 `state`，PKCE 是唯一绑定。并发发起返回 `409`，不排队、不互相覆盖。
- 流程秘密（PKCE verifier、DPoP 私钥 JWK）用 `SecretBox` 加密后入库，AAD 为
  `polygate:admin-flow:v1:<flow_id>`；解密缓冲区用后零化。
- 账号去重使用上游身份 `(kind, region, external_id)`，`external_id = codearts:<region>:<principal_id>`；
  重复授权更新同一账号的凭据（CAS），不会产生重复账号。

## 4. 账号添加流程

```text
浏览器「添加账号」
  └─ POST /admin/api/auth/codearts/start {region, name?}
       ├─ 失效过期流程；若已有 pending/exchanging → 409
       ├─ 生成 PKCE + P-256 DPoP；加密 secrets；写 admin_auth_flows
       └─ 返回 {flow_id, authorize_url, expires_at}
            │
            ├─ 前端打开授权窗口（noopener 关闭，保留 opener 通信）
            ▼
      CodeArts 门户/登录/同意
            │ 302 → /oauth/callback?code=...
            ▼
      GET /oauth/callback
       ├─ 渲染结果页；oauth-callback.js postMessage 给管理台并关窗
       └─ 同时由前端轮询 GET /admin/api/auth/codearts/{flow_id}
            │
            ▼
      complete_active_flow(code)
       ├─ claim pending → exchanging（条件 UPDATE，单执行者）
       ├─ 解密 PKCE/DPoP；POST token 端点
       ├─ GET caller-identity（HMAC 签名）
       ├─ 凭据 JSON 交 AccountService：新账号原子创建，旧账号 CAS 更新
       └─ 流程 succeeded/failed；失败原因脱敏
```

结果语义：

- 只有 token 交换与身份查询都成功，才写入凭据并把 `auth_status` 置为 `Valid`；任何一步失败都不会
  产生“已授权”账号。
- 账号与凭据创建复用 `Storage::create_account` 的同一事务；唯一约束冲突时整体回滚（有测试）。
- 浏览器只拿到流程状态与公开账号字段；AK/SK、STS token、refresh token、DPoP 私钥、PKCE verifier
  永远不会出现在任何管理 JSON、日志或前端存储中。

## 5. 管理 API

所有 `/admin/api/*`（除 bootstrap/setup/login/recover）都要求会话 Cookie + 修改类请求的 CSRF 头。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/admin/api/bootstrap` | 是否需要首次设置、是否已登录 |
| POST | `/admin/api/setup` | 首次设置管理员密码（仅 loopback） |
| POST | `/admin/api/login` | 登录，失败限速 |
| POST | `/admin/api/recover` | 恢复码重置密码并吊销全部会话 |
| GET | `/admin/api/session` | 当前会话信息 |
| POST | `/admin/api/logout` | 注销 |
| GET | `/admin/api/dashboard` | 计数、最近请求、最近签到、额度概览 |
| GET | `/admin/api/providers` | Provider 能力/账号/凭据状态 |
| GET | `/admin/api/models` | 现有 `ModelCatalog` 快照 |
| GET | `/admin/api/checkins` | 各账号最近签到 |
| GET | `/admin/api/usage` | 用量汇总与最近记录 |
| GET/POST | `/admin/api/accounts` | 列表 / 新建（当前仅 CodeArts；其他 Provider 返回 501） |
| GET/PATCH/DELETE | `/admin/api/accounts/{id}` | 详情 / 改名与启停 / 删除（需 `confirm=<id>`） |
| GET/POST | `/admin/api/accounts/{id}/auth-status` | 本地状态 / 真实探测 |
| GET/POST | `/admin/api/accounts/{id}/quota` | 本地额度 / 刷新额度 |
| GET | `/admin/api/accounts/{id}/checkins` | 单账号签到记录 |
| POST | `/admin/api/auth/codearts/start` | 发起授权 |
| GET | `/admin/api/auth/codearts/{flow_id}` | 轮询状态（惰性过期） |
| POST | `/admin/api/auth/codearts/{flow_id}/cancel` | 取消 pending 流程 |
| GET | `/oauth/callback` | 授权回调结果页（不需要管理会话） |

额度语义：刷新失败写入 `error` 快照；未知额度保持 `unknown`/`null`，UI 显示“未知”，不显示 0 或 ∞。

## 6. 安全设计

**管理员认证**

- 首次设置无默认密码；密码至少 12 字符，拒绝 `password` 等弱子串。
- Argon2id 哈希（`spawn_blocking`，避免阻塞运行时）。
- 恢复码 `pgrec-...` 只显示一次，DB 存 SHA-256；使用后轮换并吊销全部会话。
- 会话 token `pgadm-...` 随机生成，DB 只存 SHA-256；Cookie 名 `polygate_admin`，
  `HttpOnly; SameSite=Strict; Path=/admin`；HTTPS（配置或 `X-Forwarded-Proto`）下加 `Secure`。
- 会话过期/吊销检查在每次请求执行；注销即吊销。
- 登录失败限速（默认 5 次 / 300 秒 / 对端地址），超限 `429 + Retry-After`。

**请求防护**

- 修改类请求必须同时通过 Origin 校验与 `X-PolyGate-CSRF` 校验。
- Origin 必须精确匹配 `server.public_url`、`[admin].allowed_origins` 或 `Host` + 请求 scheme；
  不使用通配符，不跟随上游跳转。
- 首次设置要求 **TCP 对端是 loopback 且 Host 是 loopback**（`localhost`/`127.0.0.1`/`::1`）。
  仅检查对端会让同机反向代理暴露设置页；测试覆盖该场景。
- 请求体上限 32 KiB；错误响应统一脱敏，不回显请求内容或凭据。
- `/v1` 的 Gateway API Key 不授予任何管理权限；测试覆盖“带 API Key 访问管理 API 仍 401”。
- 安全响应头：CSP `default-src 'self'`（无内联脚本/样式）、`frame-ancestors 'none'`、
  `X-Frame-Options: DENY`、`nosniff`、`Referrer-Policy: no-referrer`、管理页 `Cache-Control: no-store`。

**凭据边界**

- 库内只保存 `SecretBox` 密文；管理 API 永不返回凭据字段。
- 授权流程秘密单独加密，AAD 与流程 id 绑定。
- 真实探测/额度刷新失败只记录脱敏错误，不把上游原文透传给浏览器。

## 7. 配置

`config.example.toml` 的 `[admin]` 段：

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` | `true` | 是否挂载管理台（`/v1` 不受影响） |
| `redirect_url` | `http://127.0.0.1:<port>/oauth/callback` | 授权回调；必须与 CodeArts 门户允许值一致 |
| `setup_local_only` | `true` | 首次设置仅限 loopback；不建议关闭 |
| `secure_cookie` | 跟随请求 | 可强制 `Secure` |
| `allowed_origins` | `[]` | 额外的精确 Origin |
| `flow_ttl_secs` | `600` | 授权流程有效期 |
| `login_max_failures` / `login_window_secs` | `5` / `300` | 登录限速 |
| `codearts_*` | 与源码一致的默认值 | 门户/插件/STS 参数，通常无需修改 |

HTTPS/反向代理：默认只监听 `127.0.0.1`。公网部署必须置于 TLS 反向代理之后，设置
`server.public_url`（或 `trust_proxy = true` 并正确传递 `X-Forwarded-Proto`），并把
`redirect_url` 配成外部可访问的 HTTPS 地址。

**局域网访问**（例如路由器上的 `listen = "10.0.0.1:8787"`）：CodeArts 把授权 code 绑定在
**回环回调**上，把 `redirect_url` 改成 `http://10.0.0.1:8787/oauth/callback` 会让 STS 在换取
token 时返回 `STS5.1805`。正确做法是保持默认的 `127.0.0.1` 回调，浏览器回调页打不开时用
面板里的**粘贴框**完成授权：

1. 面板「账号管理 → CodeArts → 授权」，弹窗打开华为授权页并完成登录；
2. 浏览器跳到 `http://127.0.0.1:8787/oauth/callback?code=...`（可能显示无法访问）；
3. 复制地址栏完整链接，粘贴回面板的「回调链接」输入框，点「完成授权」。

网关会用该流程保存的 PKCE verifier 与 DPoP 私钥在服务端完成换取，浏览器不需要能访问回调地址。
同样的粘贴模式也用于 TRAE。当前生效值可以在未登录的 `GET /admin/api/bootstrap` 里看到
（`oauth_callback_url`）。

## 8. 前端

- 单页、无构建步骤；首次加载 `bootstrap` 决定显示“设置 / 登录 / 应用”。
- 侧栏：概览、账号（CodeArts/Qoder/WorkBuddy/TRAE；CodeBuddy 明确标注“待接入”）、
  模型、签到、用量、**请求日志**、设置。
- 概览：Provider/账号/可用/异常计数、最近签到、最近请求、额度概览、**24h 成功率 /
  平均延迟 / 调度策略 / 最近错误**；无数据时显示空状态。
- 模型页：原生模型目录 + 别名管理（新建、编辑优先级/权重/能力、启停、删除目标）。
- 请求日志页：只读元数据（时间、Provider/账号内部 ID、别名与上游模型、API、流式、
  HTTP 状态、错误类别、延迟、token），支持按天清理与清空；不展示请求内容或凭据。
- 账号页：名称、Provider、区域、认证状态（区分“本地记录”与“实时探测”）、启用状态、额度、
  最近签到、更新时间；操作：添加、详情、改名、启停、删除（输入 id 二次确认）、检测认证、刷新额度。
- 主题：深/浅色切换，`localStorage` 只保存主题偏好，不保存任何凭据。
- 所有服务端字符串在插入 DOM 前转义；无内联脚本，符合 CSP。
- 授权窗口：打开门户后轮询流程状态；取消会调用 cancel；成功/失败通过 `postMessage` 与轮询双通道。

## 9. 测试

`src/admin/tests.rs` 使用确定性 Mock STS（token 端点、`caller-identity`）与真实 Axum Router：

- 首次设置：bootstrap、弱密码拒绝、loopback 限制、同机反向代理拒绝、无默认密码。
- 登录：Cookie 属性、错误密码、限速、恢复码轮换与吊销。
- 授权：CN/Intl URL 参数隔离、成功落库、失败不建号、取消、超时、重复授权更新同一账号、
  并发流程 409、库内密文不含明文。
- 账号：列表/详情不泄露凭据、改名/启停、删除确认、未实现 Provider 501、额度未知保持未知。
- 安全：未认证 401、API Key 不授予管理权限、CSRF、Origin 不匹配、CSP/嵌入资源。
- 事务：账号唯一约束冲突时整体回滚。

## 10. 未验证项与限制

- **真实 CodeArts 登录已验证**（2026-10-29，1 个 CN 账号）：浏览器授权、token 交换、
  `caller-identity`、模型发现、额度、非流式/流式 Chat、Tool Calling、多轮与会话粘性均成功；
  Token 刷新、签到、会话墙恢复、Intl 仍未验证，Provider 仍为 `experimental`。
  详见 `docs/CODEARTS_LIVE_VERIFICATION.md` §4.1。
- 授权回调只带 `code`；若门户未来引入 `state`，应升级为多流程并发。
- 单实例内存限速器；多实例部署需外部限速（当前不在范围内）。
- 管理台不提供独立审计日志页面、实时日志流、凭据导出（后续阶段）；请求元数据日志
  已提供。
- 首次设置后若忘记密码，只能使用恢复码；恢复码也丢失时需要离线重置数据库中的管理员行。
- `AccountService` 凭据更新使用修订号 CAS；并发刷新/重新授权冲突时返回可重试错误，未做自动重试。

## 11. 下一阶段入口

- M13-C2：使用真实授权账号完成 CodeArts 登录、Chat、额度、状态探测的端到端验证，并把
  `docs/CODEARTS_LIVE_VERIFICATION.md` 的结论从 Blocked 更新为真实结果。
- 在此之前不进入 M14（真实签到）与其他 Provider。
