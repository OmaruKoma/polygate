# WorkBuddy Provider 迁移研究（M14-A）

> 状态：**研究完成，未实现 Adapter**。本文只记录源码/文档事实与本项目的适配判断。
> 本机**没有**用 WorkBuddy 真实账号发起任何请求；所有“实测”均指参考仓库作者在源码注释中
> 记录的观测，不是 PolyGate 的实测。PolyGate 的真实能力仍以 `docs/PROVIDERS.md` §0 为准。
>
> 关联文档：`docs/PROVIDER_MIGRATION.md`（Coding2API 总表）、`docs/PROVIDERS.md` §4/§13、
> `docs/RESEARCH.md` §11、`docs/CHECKIN_RESEARCH.md`、`docs/ARCHITECTURE.md` §2/§10。

## 0. 证据来源与等级

| 来源 | 位置 | 许可 | 本文用途 |
| --- | --- | --- | --- |
| workbuddy2api | `~/workbuddy2api`（xiaofan6ya/workbuddy2api 1.0.8） | MIT（`LICENSE`，Copyright 2026 HanHan666666 / Chris） | 桌面 `.info` 结构、登录链路、刷新、模型/额度/签到、错误码全表、逆向记录 |
| Coding2API | `.refs/RobbsLuo_Coding2API`（`codebuddy` provider） | MIT | 独立第二实现：请求头、Intl 差异、tool/消息归一、刷新、错误分类 |
| dsh-wtq-pool | `.refs/chengzhi-c_dsh-wtq-pool` | MIT | 2026-10 最新客户端版本/头族、CN/Global 双网关、设备码登录、credits 逐包解析、流中继 |
| PolyGate 现有代码 | `src/` | 本项目 | Provider trait、会话租约、签到执行器、协议类型 |

证据等级沿用 `docs/PROVIDER_MIGRATION.md` §0：`源码+单测` / `源码` / `作者实测` /
`活动不可用` / `不支持` / `已排除`。**没有任何一条进入 `本项目实测`。**

> 需要特别说明：`~/workbuddy2api\logs\converter.log`（28 MB）与
> `%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth` 下可能存在真实登录态/对话内容。
> 本次**没有读取**这些文件，也没有读取任何 `.info` 明文；协议结论全部来自公开源码与作者文档。

## 1. 一句话结论

WorkBuddy（腾讯 CodeBuddy 家族）是一个 **Bearer + 企业/域头 + 强制流式 OpenAI 形状** 的上游：
**聊天无原生会话、无服务端 Agent Task API、无取消接口**；额度单位是 credits，CN 有每日签到。
协议本身不复杂，真正的门槛是 **上游渠道风控要求请求“像官方客户端”**（UA、`X-CodeBuddy-Request`、
`X-Conversation-*`、`X-Stainless-*`、部分场景的 `X-Device-Token`），以及 CN/Intl 两套独立网关。

这与 `AGENTS.md` §2 的红线（不得客户端身份冒充、不得设备指纹伪造、不得风控/额度绕过）直接冲突。
**M14-B 的第一件事不是写 Adapter，而是用最小诚实请求头做一次只读探针**，决定这条路是否可走（§14）。

## 2. 上游协议调用链

```text
PolyGate / 客户端
   │  POST /v1/chat/completions (OpenAI)
   ▼
WorkBuddy adapter（计划）
   │  1. 读账号凭据（SecretBox 解密）→ accessToken / refreshToken / uid / enterpriseId / domain
   │  2. 必要时先刷新（§4）
   │  3. 组装 OpenAI 形状 body：model/messages/tools/tool_choice/stream=true/stream_options.include_usage
   │  4. 注入鉴权头（Authorization + X-User-Id/X-Enterprise-Id/X-Tenant-Id/X-Domain）
   ▼
CN: https://copilot.tencent.com        Global: https://www.workbuddy.ai（或 codebuddy.ai）
   │  POST /v2/chat/completions
   ▼
OpenAI 形状 SSE（data: {chunk} ... data: [DONE]），上游**只支持流式**
   │
   ▼
adapter 解析 SSE → PolyGate StreamEvent → 下游 OpenAI/Anthropic SSE
```

- 聊天 API 是标准 OpenAI `chat/completions`，不是私有帧格式；非流式请求也由网关在本地聚合
  （`workbuddy2api/converter.py:1022`、`:1060-1090`；`Coding2API/src/provider/codebuddy/client.py:231-240`
  “上游只支持流式；非流式由调用方聚合”）。
- 上游域与计费域不同：CN 聊天 `copilot.tencent.com`，计费/登录页 `www.codebuddy.cn`
  （`workbuddy2api/admin/backend.py:119-120`；`dsh/src/upstream.ts:174-179`）。
- 聊天请求没有 `/v2` 之外的版本协商参数；模型目录在 CN 是 `/v2/enterprises/{personal|eid}/models`，
  Global 是 `/v3/config`（§6）。
- 上游会把“业务失败”放在 **HTTP 200 / 400 的 JSON 信封**里（`code != 0`）；只看状态码会误判。
  多个参考实现都独立记录了这一点（`workbuddy2api/docs/protocol-reference.md:1875-1900`；
  `Coding2API/src/provider/codebuddy/events.py:80-120`；`dsh/src/upstream.ts:826-840`）。

## 3. 认证方式与凭据结构

### 3.1 凭据形状（桌面 `.info` 同构 JSON）

参考实现把桌面端登录文件归一成如下结构（**只列字段名，不含任何真实值**）：

```json
{
  "account": {
    "uid": "<stable user id>",
    "uin": "<CN billing identity, may be empty>",
    "nickname": "<display name>",
    "type": "personal | enterprise",
    "enterpriseId": "<tenant id, optional>",
    "enterpriseName": "<optional>",
    "phoneNumber": "<optional, PII>",
    "isCreator": false,
    "isAdmin": false,
    "pluginEnabled": true
  },
  "auth": {
    "accessToken": "<JWT>",
    "refreshToken": "<opaque>",
    "tokenType": "Bearer",
    "expiresIn": 3600,
    "refreshExpiresIn": 86400,
    "expiresAt": 0,
    "refreshExpiresAt": 0,
    "domain": "www.codebuddy.cn | www.workbuddy.ai | www.codebuddy.ai",
    "lastRefreshTime": 0,
    "sessionState": "<opaque>",
    "scope": "<optional>"
  }
}
```

来源：`workbuddy2api/admin/wb_login.py:886-932`（字段逐项对齐本机真实 `.info`，时间戳毫秒、
`expiresIn` 秒）；`dsh/src/accounts.ts:84-101`、`:300-360`（解析、`$wbEncrypted` 信封、字段缺失容忍）；
`Coding2API/src/provider/codebuddy/credential.py:20-60`（归一化凭证类型）。

要点：

- **accessToken 是 JWT**，`uid` 必须与 JWT `sub` 一致（参考实现做交叉校验，防拼接）：
  `workbuddy2api/admin/wb_login.py:196-260`。
- `refreshToken` 非空才算可续期；没有刷新令牌的凭证在过期后只能重新登录
  （`workbuddy2api/admin/wb_login.py:200-230`；`dsh/src/accounts.ts:636-642`）。
- `domain` 决定网关与区域，不是装饰字段：空/`codebuddy.cn` 走 CN；
  `workbuddy.ai` / `workbuddy.cc` / `codebuddy.ai` 走 Global（`dsh/src/upstream.ts:478-505`）。
- 新版桌面端会把 `auth.accessToken`/`refreshToken`/`nickname` 等做成 `$wbEncrypted` 信封
  （5.6.0+）。参考实现通过本机 `at-rest` 打开；**PolyGate 不实现 DPAPI/信封解密**，
  只接受用户显式导入的明文凭证 JSON 或面板设备码授权结果（§4.3）。

### 3.2 请求头（事实 vs 红线）

参考实现实际发送的头族（`dsh/src/upstream.ts:520-700`；`Coding2API/src/provider/codebuddy/headers.py:60-125`；
`workbuddy2api/converter.py:390-405`）：

| 头 | 值/来源 | 是否协议必需 | PolyGate 判断 |
| --- | --- | --- | --- |
| `Authorization` | `Bearer <accessToken>` | 是 | 实现 |
| `X-User-Id` | `account.uid` | 是（无 uid 时官方用 `X-No-User-Id`） | 实现 |
| `X-Enterprise-Id` / `X-Tenant-Id` | `account.enterpriseId` | 企业账号需要 | 有则实现 |
| `X-Domain` | 凭据 `auth.domain` | 是（决定租户路由） | 实现 |
| `Content-Type` / `Accept` | JSON | 是 | 实现 |
| `X-CodeBuddy-Request: 1` | 官方“闸门头” | 参考实现称每个 API 请求必带 | `impersonate` 下对所有请求发送 |
| `X-Conversation-ID` / `X-Conversation-Request-ID` / `X-Root-Request-ID` / `X-Conversation-Message-ID` / `X-Request-ID` | 每请求随机 UUID/hex | Intl 缺失会 11128；CN 不要求但接受 | `impersonate` 下仅 chat 发送；不做“派生稳定 ID”或轮换规避 |
| `X-Agent-Intent: craft`、`X-Agent-Purpose: conversation`、`X-Agent-Type: main` | 官方客户端常量 | 参考实现称必带 | `impersonate` 下仅 chat 发送 |
| `X-IDE-Version` / `X-Product-Version` / `X-Product: SaaS` | 官方版本 | 版本闸门/用量归属 | `impersonate` 下发送（版本取公开客户端常量，不探测本机客户端） |
| `User-Agent` | `WorkBuddy/<ver> WorkBuddy/<ver> CLI/<ver>` | 参考实现称“必须伪装官方” | 诚实模式用 PolyGate UA；`impersonate` 发官方 UA |
| `x-stainless-*` | OpenAI JS SDK 指纹 | 参考实现称腾讯校验 | `impersonate` 下发送 |
| `X-Machine-ID` / `X-Session-ID` / `X-Device-Token` | 设备/风控指纹（Turing SDK） | 官方可降级；`X-Device-Token` 由 DLL 生成 | 仍不实现（AGENTS.md §2） |
| `traceparent` / `b3` / `X-B3-*` / `X-Trace-ID` | 请求 trace | 官方客户端注入 | `impersonate` 下每请求随机生成 |
| `Origin` / `Referer` | 计费域 | 桌面端请求带 | `impersonate` 下发送 |

关键结论：

1. **`X-Device-Token` 不是协议必需**。workbuddy2api 逆向确认：官方客户端拿不到 token 时会
   自动降级为不带该头，签到/短信仍可工作；纯 Python 无法自行生成（DLL + `tdid.m.qq.com`）
   （`workbuddy2api/docs/protocol-reference.md:10.9.1`，约 2340-2370 行）。
   dsh 的聊天/额度/签到请求也**完全不带** `X-Device-Token`（`src/upstream.ts:520-700`）。
2. 但其他“像官方客户端”的头族被多个参考实现描述为降低 11128 风控的关键。**是否真的必需，
   没有公开的、无伪装头的成功证据**。M15 的处理是把它做成 `client.identity` 开关
   （默认 `honest`，`impersonate` 时按请求类别分别发送：chat 带会话族与 chat UA，
   额度/签到带桌面 UA 与闸门头，refresh 带 `X-Auth-Refresh-Source`）。Intl 没有会话族
   会被 11128 拒绝，因此 Intl 必须开 `impersonate`。
3. `X-Auth-Refresh-Source` 的取值不一致：`plugin`（workbuddy2api `converter.py:345`；
   Coding2API `refresh.py:70-74`）与 `workbuddy`（dsh `upstream.ts:668`）。说明它是按
   客户端渠道/品牌变化的字段，**不能当作稳定协议常量**。

## 4. 认证生命周期

### 4.1 设备码登录（官方正常授权路径）

dsh 的 CLI 登录流程（`src/login.ts:130-250`）：

```text
1. POST {base}/v2/plugin/auth/state?platform=CLI        body {}
   → { code:0, data:{ state, authUrl } }
2. 用户在浏览器打开 authUrl，在**官方页面**完成登录（短信/扫码等由腾讯处理）
3. GET  {base}/v2/plugin/auth/token?state=<state>
   → 未完成：code != 0；完成：data:{ accessToken, refreshToken, expiresIn, domain, ... }
4. GET  {base}/v2/plugin/login/account?state=<state>   Authorization: Bearer <accessToken>
   → data:{ uid, uin, enterpriseId, nickname }
```

- 没有 PKCE；`state` 是唯一关联。
- workbuddy2api 的网页短信流程用 `platform=workbuddy`，并复刻了 Keycloak/OneID 多跳
  （`admin/wb_login.py:20-52`）。**PolyGate 不实现短信/验证码自动化**；只用官方页面 +
  轮询（dsh 的 `platform=CLI` 路径），或直接导入明文 `.info`。
- 登录 UA 也被上游按渠道区分（dsh `LOGIN_UA = CLI/2.63.2 CodeBuddy/2.63.2`）；
  诚实 UA 是否被接受同样需要探针。

### 4.2 Token 有效期与刷新

- access token 是 JWT，真实寿命由 `expiresIn`（秒）与 JWT `exp` 决定；参考实现遇到
  `expiresIn` 缺失时**假定 1 小时**，而不是把 0 当“永不过期”
  （`dsh/src/upstream.ts:220-245`；`workbuddy2api/converter.py:320-335`）。
- 刷新端点：`POST {chat_base}/v2/plugin/auth/token/refresh`，body `{}`，头：
  `Authorization: Bearer <access>` + `X-Refresh-Token: <refresh>` + `X-Auth-Refresh-Source`
  （`plugin` 或 `workbuddy`）+ 常用域/身份头（`workbuddy2api/converter.py:335-380`；
  `Coding2API/src/provider/codebuddy/refresh.py:60-125`；`dsh/src/upstream.ts:1500-1520`）。
- 响应 `data` 至少含 `accessToken`，可能含轮换后的 `refreshToken`、`expiresIn`、`domain`。
  **refreshToken 可能轮换，也可能原样保留**；新响应缺少 refreshToken 时必须沿用旧值
  （`converter.py:355-375`；`refresh.py:110-125`）。
- `401/403` 或 `invalid_grant` / `Offline user session not found`：refresh token 已被上游
  撤销/会话被删，**重试无意义**，只能重新登录（`refresh.py:76-86`；
  `workbuddy2api/docs/protocol-reference.md:10.8.4-10.8.5`）。
- **刷新节奏**：官方客户端按固定 24h 节奏刷新，而不是只看本地过期时间；长期不刷新会让
  服务端 offline session 被删除（`workbuddy2api/docs/protocol-reference.md:10.8.4`，
  约 2225-2260 行）。dsh 的实现在“距过期 5 分钟”或“签发超过 6 小时”时刷新
  （`src/accounts.ts:734-756`、`:2465-2480`）。PolyGate 应采用“24h 上限 + 临期 5 分钟 +
  单写者 CAS”，并记录 `lastRefreshTime`。
- 刷新失败分两类：网络/5xx/结构异常**可重试**；401/403/无效 grant **终态**，标记
  `AuthStatus::Expired` 并要求重新授权。不要在聊天请求里无限重试刷新。

### 4.3 PolyGate 的存储建议

- 只把 `accessToken` / `refreshToken` / JWT 等秘密放进 `security::SecretBox` 加密的
  credential JSON；`uid`、`enterpriseId`、`domain`、`nickname` 作为普通账号字段/元数据。
- **不保存 `phoneNumber`**（PII，且与调用无关）；日志/错误/管理 API 不输出 token、JWT、
  refresh token、cookie。
- 不读取 DPAPI/`$wbEncrypted`；只支持“导入明文 `.info` 的 `auth`+`account` 子集”或
  “面板设备码授权结果”。
- 刷新所有权：沿用 CodeArts/Qoder 的单写者锁 + `CredentialStore::update_credential`
  修订号 CAS；聊天请求绝不并发刷新同一账号。

## 5. CN / Intl 支持矩阵

| 能力 | CN | Intl（Global） | 证据 |
| --- | --- | --- | --- |
| 聊天基址 | `https://copilot.tencent.com` | `https://www.workbuddy.ai`；`codebuddy.ai` 同栈但 token 绑定自己的域 | `dsh/src/upstream.ts:174-215`、`:478-505`；`workbuddy2api/converter.py:130`；`Coding2API/src/provider/codebuddy/headers.py:16-17` |
| 登录/计费域 | 计费 `https://www.codebuddy.cn`；登录页 `www.workbuddy.cn` / `www.codebuddy.cn` | 与网关同源 `www.workbuddy.ai`（`codebuddy.ai` 独立） | `dsh/src/login.ts:24-50`；`workbuddy2api/admin/backend.py:119-120` |
| 模型目录 | `GET /v2/enterprises/{personal|eid}/models` | `GET /v3/config`（要求桌面 UA） | `dsh/src/upstream.ts:210-215`、`:1531-1607`；`workbuddy2api/converter.py:511-552` |
| 聊天 | `POST /v2/chat/completions` | 同路径 | 同上 |
| 刷新 | `/v2/plugin/auth/token/refresh` | 同路径 | §4.2 |
| 额度 | `/v2/billing/meter/get-user-resource` | 同路径（计费基址换成 global） | `dsh/src/upstream.ts:1609-1700`；`Coding2API/src/provider/codebuddy/client.py:300-345` |
| 签到 | 支持（活动制） | **默认不支持/未验证**：Coding2API 对 Intl 显式 `checkin=None` | `Coding2API/src/provider/codebuddy/intl.py:50-70`；`workbuddy2api/admin/backend.py:342-367` |
| 首条 system | 不要求 | **要求** `messages[0]` 为 system，否则 400/11128 | `Coding2API/src/provider/codebuddy/intl.py:25-35`、`client.py:85-100`；`dsh/src/upstream.ts:1098-1127` |
| 会话头 | 不强制 | 缺 `X-Conversation-ID` 会 11128 | `dsh/src/upstream.ts:570-600` |
| 账号体系 | 独立 | 独立；`workbuddy.ai` 与 `codebuddy.ai` token 互不通用 | `dsh/src/upstream.ts:478-505`；`docs/PROVIDERS.md` §4 |

结论：**CN 先做**；Intl 只做显式 `region = intl` 注册，首版能力状态 `unavailable`，
签到关闭，模型目录与首条 system 规则单独适配。

## 6. 模型列表

### 6.1 端点与解析

CN（`dsh/src/upstream.ts:1531-1607`；`workbuddy2api/converter.py:511-552`）：

```text
GET {chat_base}/v2/enterprises/{personal | <enterpriseId>}/models
Authorization: Bearer <access>
X-User-Id / X-Enterprise-Id / X-Domain
```

响应 `data` 同时含 `models[]` 与 `agents[]`：

- `models[]` 条目：`id`、`name`、`maxInputTokens`、`maxOutputTokens`、`credits`（如 `"x0.05"`）、
  `reasoning { supportedEfforts[], defaultEffort, canDisableThinking }`、`supportsImages`、
  `disabled`（`true` 时不可用）；dsh 还处理 `disabledMultimodal` 负向拼写
  （`src/upstream.ts:1075-1096`）。
- `agents[]` 里 `name == "cli"` 的 `models` 是官方 CLI 实际可选的模型集合；参考实现只暴露
  该子集（`dsh/src/upstream.ts:1580-1607`）。若 `agents` 缺失，回退全部 `models`。
- 模型必须有正数 `maxInputTokens` / `maxOutputTokens`，否则丢弃（dsh 同处）。
- workbuddy2api 在两种形状间兼容：`data.models` 为对象数组，或 `data.agents[].models`
  为字符串数组（`converter.py:511-540`）。

Global（`dsh/src/upstream.ts:1531-1607`；`Coding2API/src/provider/codebuddy/intl.py:25-45`）：

```text
GET https://www.workbuddy.ai/v3/config      (desktop UA)
```

同一 `{ models, agents }` 形状；CodeBuddy Intl 的 `/v3/config` 返回全部模型，但套餐决定
能否调用（`11158 model [...] is not allowed for user type [...]`），Coding2API 对 Intl
逐个探活过滤（`client.py:54-70`、`:372-395`）。

### 6.2 模型繁忙程度（可选）

`POST /v2/chat/busy`，body `{ "models": ["..."] }`，每次最多 50 个，5 秒超时；响应
`data[] = { model, busy_level }`，`busy_level ∈ idle|normal|busy|congested|unavailable`
（`workbuddy2api/admin/backend.py:264-270`、`admin/model_busy.py:6-25`）。
这是**观测数据**，不是调度必需；PolyGate 首版可不接。

### 6.3 静态兜底

参考实现有静态模型列表（`workbuddy2api/converter.py:600-615`；Coding2API
`DEFAULT_MODELS = ("glm-5.2", "deepseek-v4-pro")`）。PolyGate 不得把兜底列表当实时目录：
发现失败时该 Provider 不贡献模型（与现有 `ModelCatalog::discover` 语义一致），
只可在管理台显示“缓存/兜底”并标明来源。

## 7. 普通聊天 vs Agent 任务

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 是否有独立 Agent Task API | **没有**。参考实现只有 `/v2/chat/completions`；Codex/Claude Code 的 agent 循环在客户端 | `Coding2API/src/provider/codebuddy/client.py:231-240`；`PROVIDER_MIGRATION.md` §1 结尾 |
| 桌面端 “Agent / Ask 模式” 是否是请求字段 | 未发现独立端点或必填字段；只在错误码/UI 语义中出现（2003/2004/1003） | `workbuddy2api/docs/protocol-reference.md:10.4.0` 错误表 |
| 桌面的 `craft` 意图 | 通过 `X-Agent-Intent: craft` 等头表达，不是 body 字段 | `Coding2API/src/provider/codebuddy/headers.py:100-110`；`dsh/src/upstream.ts:590-620` |
| Agent 工具执行 | 客户端本地执行；上游只返回 `tool_calls`。`ardot.tencent.com/mcp` 是独立设计工具服务，不属于聊天协议 | `workbuddy2api/admin/backend.py:49-52`、`:1266`；`docs/PROVIDERS.md` §13 |
| PolyGate 应声明 | `ProviderClass::Model`；`agent_task = false` | 本判断 |

因此：**不能**把 WorkBuddy 标成 `Hybrid`/`Agent`；如果未来要提供“服务端 Agent”，那是
PolyGate 自己实现的工具循环，不是上游能力。

## 8. 流式响应格式

- 标准 OpenAI SSE：`data: {"id":...,"choices":[{"delta":{...},"finish_reason":...}],"usage":...}`，
  结束 `data: [DONE]`（`Coding2API/src/provider/codebuddy/events.py:1-35`；
  `workbuddy2api/converter.py:1184-1265`）。
- `delta` 可含：`content`、`reasoning_content`、`tool_calls[]`；`usage` 通常出现在最后
  （Coding2API 强制 `stream_options.include_usage=true`，`client.py:285-295`）。
- 参考实现观察到每个 delta 带空占位键：`tool_calls: []`、`function_call: null`、
  `refusal: ""`、`extra_fields: null`；部分客户端会误判为“工具调用开始”。转发前应忽略/
  删除空占位（`workbuddy2api/converter.py:1200-1210`、`admin/routers/proxy.py`；
  `upstream_compat` 第 9 节，`docs/protocol-reference.md` 相关段）。
- `finish_reason` 可能是 `content-filter`（连字符），表示内容审核拦截；这不是传输错误，
  应映射为 `FinishReason::ContentFilter`（`converter.py:1080-1100`、`:1265`；
  PolyGate `src/protocol/response.rs:30-41` 需补 `content-filter` 拼写）。
- 上游可能把错误信封塞进 HTTP 200 的 JSON（`content-type: application/json` 而非 SSE）；
  必须先读小响应体判断 `code`，再决定是否当流处理（`dsh/src/upstream.ts:1400-1448`）。
- 空 finish_reason：参考实现曾有“空 finish_reason → null”的本地补丁
  （`docs/RESEARCH.md:24`），说明上游会发空串。PolyGate 的
  `FinishReason::from_upstream("")` 已映射为 `Unknown`，不要伪造成 `stop`。

## 9. Tool Calling

- 请求：标准 `tools`（`type:function`）、`tool_choice`（`auto`/`none`/`required`/指定函数）；
  历史消息含 `assistant.tool_calls` 与 `role:tool` + `tool_call_id`。
- 上游返回：标准 `delta.tool_calls[]`（`index`/`id`/`function.name`/`function.arguments`
  增量），客户端负责拼接参数并执行。
- 参考实现必须做的归一（**协议事实，不是私有业务**）：
  - `role: developer` → `system`；腾讯后端不认 `developer`，实测会判 11128
    （`Coding2API/src/provider/codebuddy/client.py:242-250`；`dsh/src/upstream.ts:1310-1315`）。
  - 清理历史里的“脏 tool_call”（无 `function`/空 name）及其悬空 `tool` 结果；
    空 assistant 占位整条丢弃（`Coding2API/client.py:110-150`）。
  - `tool_choice: "none"` 时删除 `tools`/`functions`；字符串形式与对象形式归一
    （`dsh/src/upstream.ts:1320-1350`）。
  - 缺 `reasoning_effort` 时，官方客户端会带默认值；否则上游把思考写进可见
    `content`、`reasoning_content` 恒空（`Coding2API/client.py:265-280`）。
    这是“保持思考通道”的兼容处理，不是绕过。
  - 工具/消息里出现其他厂商客户端指纹（如 Claude Code 的 system 文案）会触发 11128；
    参考实现做“中和/折叠”。**PolyGate 不实现这类改写**（§14）。
- 错误码：`11129` 工具定义不合法、`10012` 找不到工具、`11148/11149` 工具记录异常、
  `11115` 上下文超长（`workbuddy2api/docs/protocol-reference.md:10.4.0`）。

## 10. 会话管理设计

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 上游是否有会话 ID | 没有可恢复的原生会话；`conversationId` 只出现在请求头/上报事件中 | `dsh/src/upstream.ts:586-591` 明确“同一 conversation id 再次发送会被当作新回合，上游不保留历史” |
| 会话创建/恢复/销毁接口 | 未发现 | 全部参考实现只调 `/v2/chat/completions` 与 `/v2/report` |
| 多轮如何工作 | 客户端每次回传完整 `messages` | 同上；`Coding2API/client.py:231-240` |
| 会话头是否可用于亲和 | 上游不校验一致性；参考实现每请求随机生成 | `Coding2API/src/provider/codebuddy/activity.py:97-100`；`dsh/src/upstream.ts:586-600` |
| 并发会话限制 | 存在业务码 `10105 ConversationLimitExceeded`（“并发会话数超限”），但阈值/作用域未验证 | `workbuddy2api/docs/protocol-reference.md:1946` |
| 多端占用 | 本地错误表有 `4013`（疑似“会话被其他端占用”），但错误文案表编码损坏，**未确认** | `workbuddy2api/admin/data/upstream_error_messages.json`（条目存在，文本不可读） |

PolyGate 设计：

- `Capability::NativeSession = false`；`ProviderContext.upstream_session_id` 对 WorkBuddy
  **不传给上游**（没有对应字段）。
- 复用 `SessionRouter` 的**账号亲和**：同一客户端会话 → 同一账号，提高前缀缓存命中、
  降低多账号轮换特征；这正是 dsh/Coding2API 的 sticky 设计
  （`workbuddy2api/docs/protocol-reference.md:10.3`；`dsh/src/accounts.ts` 会话粘性）。
- 不做“每次请求换 conversation id 来规避并发限制”（本任务明确禁止）；`10105` 按
  `RateLimited` 返回，不自动换号硬撞。
- 客户端断开只释放本地租约；上游没有取消接口，不能声称已停止计费（同 §12）。

## 11. 额度与签到

### 11.1 额度查询（credits）

```text
POST {billing_base}/v2/billing/meter/get-user-resource
body: {} 或 { PageNumber, PageSize, Status:[0,3], PackageEndTimeRangeBegin/End, ProductCode? }
```

响应嵌套：`data.Response.Data.Accounts[]`，每个套餐：

- `CapacityType == 4`：月度套餐 → `CycleCapacitySize` / `CycleCapacityRemain` / `CycleEndTime`；
- 其他：一次性套餐 → `CapacitySize` / `CapacityRemain` / `ExpiredTime`；
- `CapacityUnit == "credits"`；`Status == 0` 为有效；
- 参考实现按包求和，跳过已用完/已过期包；中文实现只取 `CycleCapacityRemain`
  （`dsh/src/upstream.ts:1609-1695`；`workbuddy2api/converter.py:578-601`；
  `Coding2API/client.py:300-360`）。

PolyGate 映射：`QuotaUnit::Credits`，`remaining = Σ 可用包剩余`，`total = Σ 有效包容量`；
**形状不符/无包时保持 `unknown`（null），不得写 0**。`raw` 保留原始响应。
`ProductCode` 不是必需：workbuddy2api 不带（部分账号会拒绝错误产品码），dsh 带 `p_tcaca`；
首版**不带**，失败再按配置覆盖。

### 11.2 签到（CN）

```text
POST {billing_base}/v2/billing/meter/checkin-activity-status   body {}
POST {billing_base}/v2/billing/meter/daily-checkin             body {}
```

状态字段：`active`、`today_checked_in`、`streak_days`、`daily_credit`、`today_credit`、
`next_streak_day`、`streak_bonus_credit`、`end_time`（活动结束时间）
（`dsh/src/upstream.ts:1700-1740`；`workbuddy2api/admin/backend.py:342-367`；
`Coding2API/src/provider/codebuddy/checkin.py:37-100`）。

领取成功：`code == 0` 且 `data.credit` 是有限数值（`checkin.py:63-80`）。
业务失败码在两套参考里不一致：

| 来源 | 已领 | 无资格 | 活动结束 |
| --- | --- | --- | --- |
| workbuddy2api（`backend.py:1720-1728`） | `1001` | `1002` | `1003` |
| Coding2API（`checkin.py:70-80`） | HTTP 400 + `code=10001` + “已签到” | 未列 | 未列 |

这是**同一字段在不同端点/版本的不同语义**，不能与聊天错误码表混用。M14-B 必须先查状态、
以状态接口为准；claim 结果只做保守解释（明确成功才记成功，未知记 Unknown）。
签到是副作用操作：沿用 `checkin.claim_enabled` 显式开关、按业务日去重、活动结束后停止。

## 12. 并发限制、取消与错误码

### 12.1 并发

- 上游限流是**多维**的：账号级、账号×模型级、秒/分/时/日窗口、排队、并发会话；
  阈值没有任何公开数字（参考实现全部用“观测 + 退避”）。
- 明确证据：`10105 ConversationLimitExceeded`（并发会话超限）、`6001/6002/6003/6005/6006/6007`
  （TPS/TPM/TPH/RPS/RPM/RPH）、`6004/6008`（TPD/RPD，日额度）、`6020-6022`（排队，
  带 `retry_after` 秒或毫秒）（`workbuddy2api/docs/protocol-reference.md:1911-1930`；
  `admin/model_limits.py:6-50`）。
- 参考网关自身的限制（不是上游）：`max_in_flight` 按账号、`MIN_PICK_GAP_S = 0.1` 防撞号、
  `MAX_ROTATE` 默认 6、sticky TTL 1800s（`workbuddy2api/admin/pool.py:57`、`:719-740`；
  `admin/config.py:141-157`）。这些只能作为保守初始值，不能写成上游上限。
- 取消：**没有上游取消接口**。客户端断开只能停止读取并释放连接/租约；
  不得声称已取消上游计费（与 Qoder/CodeArts 同）。`Capabilities::cancellation = false`。

### 12.2 错误分类（建议映射）

分类必须**先看业务码，再看 HTTP 状态**；上游会把限流/错误放在 200 或 400 里。

| 上游信号 | 含义 | PolyGate `ErrorKind` | 账号动作 |
| --- | --- | --- | --- |
| `401` / `12153` / “Offline user session not found” | 登录态失效/会话被删 | `AuthExpired`（可先刷新一次）或 `AuthInvalid` | 标记 expired/invalid；不自动换号 |
| `11140` + “account has been restricted” | 账号级风控限制 | `ChannelRestricted` | 标记 restricted；**不换号重试** |
| `402` / `412` / `14018` / `14001` / `14012` / `14013` / `14014` | credits 用尽 | `QuotaExhausted` | 不清除账号，等签到/充值 |
| `1005` | 参考实现解释冲突：Coding2API 记为“权益/套餐耗尽”；workbuddy2api 本地错误表同码文本不可读 | `ChannelRestricted`（暂定） | 不重试；M14-B 实测确认 |
| `6004` / `6008` | 账号×模型日额度 | `RateLimited`（不罚账号） | 可换模型；本版无模型冷却表 |
| `6020` / `6021` / `6022` | 模型排队 | `RateLimited` + `retry_after` | 有界退避 |
| `6000`–`6003` / `6005`–`6007` / `429` | 秒/分/时级限流 | `RateLimited` | 有界退避/换号 |
| `14003` | 模型繁忙 | `RateLimited`（模型级语义） | 可换模型 |
| `11102` / `11158` | 无此模型 / 套餐不含 | `ModelUnavailable` | 换模型，不罚账号 |
| `11115` / `10105` | 上下文超长 / 并发会话超限 | `BadRequest` / `RateLimited` | 不重试 / 有界 |
| `11128` | 渠道/提示词风控、缺会话头 | `UpstreamServer`（先按请求级处理） | 不罚账号；不自动改写提示词 |
| `403` 无 JSON（HTML/WAF） | IP/网关级拦截 | `UpstreamServer` | 停止重试（IP 级） |
| `408` / `425` / 5xx | 瞬时/上游故障 | `Timeout` / `UpstreamServer` | 有界重试 |
| `finish_reason: "content-filter"` | 内容审核拦截 | 正常结束 + `FinishReason::ContentFilter` | 不重试 |
| HTTP 200 + `code != 0` | 业务错误 | 按上表 | 按上表 |
| `11128` 的两种解释 | ① 渠道窗口型瞬时拦截 ② system/assistant 文案命中提示词审查 ③ Intl 缺会话头 | 都归请求级 | 不换号硬撞；不实现提示词折叠 |

依据：`workbuddy2api/docs/protocol-reference.md:1840-1950`（错误码全表/处置矩阵）、
`:1911-1930`（6000 族）；`Coding2API/src/provider/codebuddy/events.py:55-150`；
`dsh/src/upstream.ts:440-475`、`:826-840`。

PolyGate 的 `classify_session_failure` 已能把 `ChannelRestricted/AuthExpired` 映射为
“账号不可用但不换号”、`RateLimited/NoCapacity` 映射为 “busy”（`src/session_service/service.rs:112-122`），
无需为 WorkBuddy 改策略；只需在适配器里把业务码映射正确。

## 13. 与 CodeArts 的复用边界

| 能力 | 可复用 | 说明 |
| --- | --- | --- |
| HTTP client | 部分 | `CodeArtsHttp` 绑定 AK/SK/STS 签名，不能直接用；沿用 `reqwest::Client` + 超时/大小上限模式 |
| SSE parser | 部分 | CodeArts 的 SSE 是“累计全文”自定义格式（`codearts/events.rs`），**不能**用于 WorkBuddy；WorkBuddy 是标准 OpenAI 行式 SSE，可参考 Qoder `SseDecoder` 的行缓冲思路，或抽公共行解码器 |
| Error mapping | 只复用 `ErrorKind` 与 `GatewayError`，**不复用分类表** | WorkBuddy 业务码完全不同；新建 `providers/workbuddy/error.rs` |
| CredentialService | 是 | `SecretBox` + revision CAS + 单写者刷新（`src/account_service/credential.rs`） |
| Session Repository | 是 | 账号亲和/租约；`upstream_session_id` 留空或仅内部关联 |
| QuotaRefreshHook | 是 | 签到成功后写真实额度快照（`src/checkin/quota.rs`） |
| CheckinExecutor | 是 | 新增 `WorkbuddyCheckinExecutor`，注册到 `RoutingCheckinExecutor`（`src/checkin/executor.rs:39-70`、`src/runtime/mod.rs:294+`） |
| Provider trait | 是 | `ChatRequest/ChatResponse/StreamEvent` 足够覆盖文本/推理/工具/usage |
| CodeArts 专属 | **不可复制** | STS AK/SK、DPoP、`chat_id` prompt-cache、`/v1/ops/*` 签到、CodeArts SSE 差分 |

## 14. 认证安全与红线（本阶段最重要）

本任务允许研究“正常授权账号”的认证流程，禁止验证码绕过、付费额度绕过、未授权访问、
反复创建会话来规避限制。`AGENTS.md` §2 还禁止设备指纹伪造与客户端身份冒充。

按此标准，**以下参考实现中的能力明确不迁移**：

1. **Turing `X-Device-Token` / 机器指纹**：由原生 SDK + `tdid.m.qq.com` 生成，属设备指纹。
2. **官方 UA / `x-stainless-*` / `X-IDE-Version` / `X-Machine-ID` / `X-Session-ID` 伪造**：
   属客户端身份冒充。注意：dsh 把 UA 写成 `WorkBuddy/5.9.3 ... CLI/2.165.0` 并称“必须”；
   这只是提高通过率，不是协议语义。M15 已按 `client.identity = "impersonate"` 可选启用
   （见 §3.2 与 `PROGRESS.md` §43）；`X-Machine-ID`/`X-Session-ID`/`X-Device-Token` 仍未发。
3. **提示词“中和/折叠”**（`sanitize_channel_markers`、`foldSystemIntoUser`、
   `renameInContent`）与 `desensitize.py`：针对上游内容审核的规避，不实现。
4. **反复更换 conversation id / 账号以规避 10105/限流**：本任务明确禁止，也不符合
   `AGENTS.md` “不得额度/风控绕过”。
5. **网页短信登录复刻、验证码转发**：不做；只用官方页面 + 轮询，或导入已授权凭证。
6. **封禁/11140 后自动换号续跑**：不做；标记 restricted 并停止。

这意味着 M14-B 存在一个**明确的 Go/No-Go 条件**：

> 用“诚实头”集合（Bearer、`X-User-Id`、`X-Enterprise-Id`、`X-Tenant-Id`、`X-Domain`、
> `Content-Type`、`Accept`、PolyGate 自己的 UA；不伪造版本/设备/会话指纹）对 CN 账号做
> **只读探针**：额度查询 → 模型列表 → 一次最小非流式聊天。
>
> - 若三者都成功：按诚实头实现 Provider（可能仍需 `X-CodeBuddy-Request` 等单个“闸门头”，
>   需逐项确认其是否属于协议必需而非身份伪造；确认后记录在案）。
> - 若稳定返回 11128/403：**停止实现聊天**，WorkBuddy 标记 `unavailable`，并向项目所有者
>   报告“必须修改 AGENTS.md 才能继续”，不得偷偷加入伪装头。
> - 只读的额度/状态查询若可用，可先作为受限能力提供；签到 claim 仍需显式授权。

该探针属于 M14-B，且需要真实 WorkBuddy 账号与公开的授权；本机当前没有 WorkBuddy 桌面端
安装（`<local>/workbuddy`、`<local>/WorkBuddy` 均不存在），但存在 `CodeBuddyExtension` 数据目录；
**本阶段未读取其中任何凭据**。

## 15. Provider 架构适配

### 15.1 现有抽象是否够用

| 抽象 | 现状 | WorkBuddy 适配 |
| --- | --- | --- |
| `ProviderClass` | `Model/Agent/Hybrid`（`src/providers/mod.rs:266-285`） | **Model**；无服务端 Agent Task |
| `Capabilities` | auth/model/quota/chat/streaming/tools/native_session/agent_task/cancellation | auth、model_discovery、quota、chat、streaming、tools = true；native_session/agent_task/cancellation = false |
| `ChatRequest` | model/messages/tools/tool_choice/max/temperature/top_p/stop/stream/metadata（`src/protocol/request.rs:25-51`） | **缺** `reasoning_effort`（及可选 `thinking`/`enable_thinking`）；**缺** assistant 历史里的 `reasoning_content` 入参 |
| `ChatResponse` | text/reasoning/tool_calls/finish/usage（`src/protocol/response.rs:97-109`） | 够用 |
| `StreamEvent` | start/text/reasoning/tool start/args/usage/finish/error（`src/protocol/stream.rs:21-60`） | 够用；`content-filter` 需映射为 `FinishReason::ContentFilter` |
| `Session` | `SessionRouter` 账号亲和；`upstream_session_id` 可选 | 只做账号亲和；不伪造上游会话 |
| `Cancel` | `Capabilities::cancellation` + `Provider::cancel` | false；断开即停止本地读取 |

### 15.2 最小协议扩展（不直接大改通用协议）

1. `ChatRequest` 增加可选 `reasoning_effort: Option<String>` 与
   `thinking: Option<Value>`（或 `enable_thinking: Option<bool>`）。默认 `None` 表示
   “客户端没要求”，适配器按上游官方默认处理；不替客户端强制档位。
2. OpenAI 入站 `WireMessage` 目前 `deny_unknown_fields`（`src/api/openai.rs:61-73`），
   `reasoning_content` 会导致 400。需要允许 assistant 消息携带 `reasoning_content`
   （映射为 `ContentBlock::Reasoning`，是否回传上游由适配器决定）。
3. `FinishReason::from_upstream` 增加 `"content-filter"` 拼写（当前只认 `content_filter`）。
4. 图片：`ContentBlock::Unsupported` 已能承载（`src/api/openai.rs:360-390`）；
   WorkBuddy 首版**拒绝**图片块并返回明确错误，后续再按 `supportsImages` 实现。
5. SSE：新增 `providers/workbuddy/events.rs`，标准 OpenAI 行式解析；可复用 Qoder 的行缓冲
   思路，但不要把 Qoder 的信封逻辑搬过来。

## 16. 已验证与未验证

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 桌面 `.info` 字段与 `auth/account` 结构 | 源码+作者实测 | workbuddy2api/dsh 均对齐真实文件 |
| 设备码登录端点与轮询 | 源码+作者实测 | dsh `login.ts`；未由 PolyGate 复核 |
| 刷新端点/头/轮换行为 | 源码+作者实测 | 两套参考一致；`X-Auth-Refresh-Source` 取值不一致 |
| `X-Device-Token` 非必需 | 作者实测（官方客户端降级路径） | 但会影响风控通过率，未知 |
| CN 聊天/模型/额度/签到端点 | 源码+作者实测 | 无 PolyGate 真实请求 |
| Intl 网关与 token 域绑定 | 源码+作者实测（dsh 2026-10） | WorkBuddy Intl 未实测 |
| 11128 的真实触发条件 | **未验证** | 三种解释并存，且与提示词内容相关 |
| 诚实头能否通过 11128 | **未验证（关键）** | M14-B Go/No-Go |
| 上游并发阈值、模型级日额度阈值 | 未验证 | 参考实现只有退避策略 |
| `10105` / `4013` 的准确语义 | 未验证 | `4013` 文案表编码损坏；`3004` 仅见于同一张损坏表，未采用 |
| 签到业务码 1001/1002/1003 vs 10001 | 冲突 | 端点/版本差异，需实测 |
| PolyGate Provider 实现 | **核心 + 生命周期已实现（M14-B/M14-C，MockVerified）** | 凭据读取、模型发现、非流式/SSE、工具、错误归一化、刷新/轮换/CAS/跨进程租约、设备码授权、额度、CN 签到；真实联调仍缺失，见 `PROGRESS.md` §36/§37 |

## 17. M14-B 最小实现计划

### 17.1 目标

在**不修改通用协议大结构**的前提下，交付一个可被真实 CN 账号验证的 WorkBuddy Provider：
认证/刷新、模型发现、非流式 + 流式聊天、工具调用、额度查询；签到作为独立 opt-in。

> 执行状态（2026-10-29）：M14-B 交付 B1/B2 离线部分；M14-C 交付 B3 刷新/设备码、
> B4 额度与 CN 签到（均为 MockVerified，见 `PROGRESS.md` §36/§37 与本节 §17.4）。
> **B0 真实只读探针与真实聊天仍未执行**，需要凭据与显式授权；B5 的真实部分同样未完成。

### 17.2 阶段与验收

**B0 — 合规探针（先决条件，0.5 天）**

- 用独立 Rust bin 或 `#[ignore]` 测试，读取一条**操作者显式提供**的 CN 凭据（不落盘、不打印），
  发：额度 → 模型列表 → 一次最小聊天（`max_tokens` 小、无工具）。
- 头集合只含诚实头（§14）；记录每个请求的 HTTP 状态与业务码。
- 通过条件：三项均 `code == 0` 且无 11128；否则停止并报告。
- 该 bin 在完成后删除，不进入发布物。

**B1 — 离线协议层（1-2 天，无网络）**

- `src/providers/workbuddy/credential.rs`：解析/校验 `auth+account`，只保留必要字段；
  不保存手机号；`Debug` 脱敏。
- `error.rs`：业务码表 + HTTP 分类（§12.2），固定向量单测。
- `models.rs`：`{models, agents}` 解析，`agents[name=cli]` 过滤，零/缺字段处理。
- `quota.rs`：月度/一次性包解析；unknown 安全。
- `events.rs`：标准 OpenAI SSE 行解析 + 空占位忽略 + `content-filter` + 截断/空闲错误。
- `convert.rs`：ChatRequest → 上游 body；developer→system；脏 tool_call 清理；
  tool_choice 归一；不实现任何提示词中和/脱敏。
- 单测全部使用 `test_support::spawn_server`，覆盖：正常流、工具分片、usage、空 finish、
  `[DONE]` 缺失、`content-filter`、200 错误信封、429/6004/11140/11128。

**B2 — Provider 与 runtime 接入（1-2 天）**

- `ProviderKind::Workbuddy` 注册；CN 默认启用，Intl 需显式配置。
- `Provider` 实现：`info/capabilities/list_models/chat/chat_stream/quota`；
  非流式聚合本地流。
- `Capabilities`：auth/model_discovery/quota/chat/streaming/tools；native_session/agent_task/
  cancellation = false。
- 会话：复用 `SessionRouter`；`ProviderContext` 不携带上游会话。
- 模型目录：`Gateway::bind` 时用账号凭据发现；失败不贡献模型。
- 额度：`QuotaSnapshot { unit: Credits, total/remaining }`；未知保持 null。
- 错误：6004 等映射；`ChannelRestricted` 不换号。

**B3 — 刷新与凭证生命周期（1 天）**

- 单写者刷新（`CredentialStore` CAS），24h 上限 + 临期 5 分钟；refresh token 轮换保存；
  401/12153 先刷新一次（仅在任何内容输出前），失败标 `Expired`。
- 管理台：WorkBuddy 账号导入（粘贴 `.info` 子集）或设备码流程（官方页面 + 轮询）；
  不实现短信/验证码自动化。

**B4 — 签到（可选，0.5 天，需 explicit claim 开关）**

- 状态查询只读；claim 受 `checkin.claim_enabled` 控制；业务日去重；活动结束停止；
  1001/1002/1003 与 10001 兼容处理；成功写 `QuotaRefreshHook`。
- Intl 不注册签到。

**B5 — 验证与文档（0.5 天）**

- Mock 单测全绿；真实 CN 只读/聊天按 B0 结果验证；更新 `PROGRESS.md`、`PROVIDERS.md`；
  Intl 标 `unavailable`，WorkBuddy CN 在真实验证前保持 `experimental`。

### 17.3 明确不做（本阶段与后续，除非政策变更）

- Turing 设备指纹、官方 UA/版本/Stainless/MachineID/SessionID 伪造；
- 提示词中和、内容审核规避、脱敏绕过；
- conversation ID 轮换/多账号轮换规避限流；
- 封禁/11140 后自动换号；
- 短信验证码/登录页复刻；
- 独立 Agent Task API（上游不存在）。

### 17.4 M14-C 执行记录（2026-10-29，完成，MockVerified）

| 计划项（§17.2 B3/B4） | 结果 | 证据 |
| --- | --- | --- |
| 单写者刷新 + `CredentialStore` CAS | 已实现：进程内按账号串行；跨进程 SQLite 租约（`credential_refresh_leases`，60 s TTL）；写入 revision CAS | `src/providers/workbuddy/provider.rs`、`migrations/0006_refresh_lease.sql` |
| refresh token 轮换 | 已实现：响应有则覆盖，缺失沿用旧值；写库失败不报告刷新成功 | `credential.rs`、`provider.rs` |
| 临期 5 分钟预刷新 | 已实现（300 s；过期时间未知则不刷新） | `REFRESH_SKEW_SECONDS` |
| 401/12153 单次刷新重试 | 已实现：仅在尚未输出内容时强制刷新一次 | `chat_with_context` / `chat_stream_with_context` |
| 管理台设备码流程 | 已实现：start / poll / cancel；真实 `authUrl`；`state` 加密存储；TTL min(admin, 600 s) | `src/admin/auth/workbuddy.rs`、`src/providers/workbuddy/auth.rs` |
| 额度（credits、月度/一次性、未知 null） | 已实现：`/v2/billing/meter/get-user-resource`，`*Precise` 优先，`TotalDosage` 不用，明细写 `raw` | `src/providers/workbuddy/quota.rs` |
| CN 签到 + claim 开关 + 业务日去重 + QuotaRefreshHook | 已实现：状态只读；claim 受 `checkin.claim_enabled`；复用 M12 去重与租约；成功后刷新额度 | `src/checkin/workbuddy.rs`、`src/runtime/mod.rs` |
| Intl 不注册签到 | 已实现为显式 `Unsupported`，零请求 | `WorkbuddyCheckinExecutor` |

**本轮与计划的差异（如实记录）：**

1. **24 h 固定刷新节奏未实现。** 上游/参考实现提到官方客户端约每 24 h 刷新一次以免
   offline session 被删除，但凭据里没有可信的签发时间；在没有真实观测前不伪造
   `lastRefreshTime`。当前仅按 access token 过期时间 + 5 分钟窗口刷新。列为验证项。
2. **401 重试范围**：只覆盖聊天与流式建流阶段；额度/状态探测收到 401 不自动重试，
   由管理台再次点击刷新（避免后台无限重试）。
3. **签到端点前缀**：使用 `/v2/billing/meter/*`（workbuddy2api 实测），Coding2API 为
   `/billing/meter/*`；不自动回退猜测。
4. **签到码**：`1001/1002/1003` 与 `10001` 同时识别；“已签到”优先于失败。
5. **`code = 0` 无 `credit`**：按成功处理（workbuddy2api 口径），摘要注明“上游未报告
   奖励数值”，不臆造 0。
6. **额度字段**：`Cycle*` 与 `Capacity*` 均解析，按 `CapacityType == 4` 区分并互相兜底。
7. **无独立“用户码”**：上游只给 `authUrl` + `state`，界面只展示真实 URL。
8. **CompatibilityGap 缩小一项**：客户端身份头与 UA 伪装改为 `client.identity`
   开关（M15），设备证明与提示词中和仍不实现；真实上游是否接受诚实头仍未验证
   （见 §14 与 `PROGRESS.md` §36.5、§43）。

**状态口径：** 以上全部为 **MockVerified**；真实上游能力数为 0，WorkBuddy CN 保持
`experimental`，Intl 保持 `unavailable`（聊天首条 system/会话头与签到均未实测）。

## 18. 参考位置索引

| 主题 | 位置 |
| --- | --- |
| 桌面 `.info` 结构/登录/刷新 | `~/workbuddy2api\admin\wb_login.py:1-60`、`:186-260`、`:860-940`；`admin\converter.py:245-430` |
| 上游基址/模型/额度/签到/聊天 | `~/workbuddy2api\admin\backend.py:119-120`、`:264-270`、`:342-367`；`converter.py:511-601`、`:1022` |
| 错误码全表与处置 | `~/workbuddy2api\docs\protocol-reference.md:1840-2010` |
| 头族与逆向结论 | `~/workbuddy2api\docs\protocol-reference.md:264-420`、`:2270-2370` |
| 第二实现（头/Intl/工具归一/刷新） | `.refs/RobbsLuo_Coding2API/src/provider/codebuddy/{headers,intl,client,refresh,events,checkin}.py` |
| 最新 CN/Global 版本与头族、设备码登录 | `.refs/chengzhi-c_dsh-wtq-pool/src/{upstream,login,accounts}.ts` |
| PolyGate Provider/协议/租约/签到 | `src/providers/mod.rs`、`src/protocol/*`、`src/session_service/*`、`src/checkin/*`、`src/runtime/mod.rs` |

## 19. 许可证备注

workbuddy2api 与 Coding2API 均为 MIT；dsh-wtq-pool 为 MIT。本文只吸收协议事实并独立实现，
未复制其受保护代码。若 M14-B 复用任何代码片段，必须在 `NOTICE`/`LICENSE` 中保留原版权声明。
`AGENTS.md` §1 仍要求发布前完成全部依赖与来源清点。
