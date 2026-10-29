# Qoder Provider 研究与接入设计（M13-D1）

> 状态：研究完成，**未写生产代码、未接入真实上游、未执行任何 Qoder 请求**。
> 本文只记录协议事实、PolyGate 可复用点、接入设计与验收计划；M13-D2 才实现。
>
> 研究日期：2026-10-29
> 参考实现：`Coding2API` 提交 `df9936240708f99f5a1cd92e52330b4b2016c840`（MIT）
> 本地路径：`.refs/RobbsLuo_Coding2API/`
> 官方客户端：本机存在 Qoder CN 客户端数据目录 `~/.qoder-cn/`（版本观测 1.1.71），
> 仅确认了文件名（`.auth/machine_id`、`.auth/user`、`.models/`），**未读取凭据内容**。
>
> 本阶段没有任何 Qoder 授权账号，因此 Qoder 的所有能力最高只能到
> `Source Evidenced` 或 `Unverified`，**不得标记 `Live Verified`**。

## 0. 证据等级

| 等级 | 含义 | 本文使用场景 |
| --- | --- | --- |
| **Live Verified** | PolyGate 用真实账号端到端实测通过 | Qoder 无（本阶段无账号） |
| **Fixture Verified** | PolyGate 用确定性 Mock/夹具测试通过 | Qoder 无（尚未实现） |
| **Source Evidenced** | 参考实现的源码/测试可逐行核对；可能含参考作者的实测注释，但不是 PolyGate 实测 | 本文绝大多数结论 |
| **Unverified** | 有线索但无法确认，或依赖上游当前行为 | 会话上限、Intl 签到、刷新轮换等 |
| **Unsupported** | 上游没有该能力，或项目明确不做 | Agent Task API、请求取消 |

原则：

- 参考作者的“实测”只作为线索，不能升级为 PolyGate 的 `Live Verified`。
- 未知额度保持 `null` / `QuotaStatus::Unknown`，不得写成 0 或无限。
- 无法确认的上游行为必须写 `Unverified`，不得用 Mock 结果替代。

## 1. 研究范围、参考来源与结论摘要

### 1.1 参考来源

| 来源 | 路径 | 用途 | 证据 |
| --- | --- | --- | --- |
| Qoder 区域/端点/错误分类/SSE/解析 | `.refs/RobbsLuo_Coding2API/src/provider/qoder/events.py` | 协议事实主来源 | Source Evidenced |
| 设备码 PKCE 登录 | `.refs/RobbsLuo_Coding2API/src/provider/qoder/auth.py` | 认证流程 | Source Evidenced |
| HTTP 客户端、聊天、模型、额度、签到、刷新 | `.refs/RobbsLuo_Coding2API/src/provider/qoder/client.py` | 请求构造与业务端点 | Source Evidenced |
| COSY 签名与设备派生 | `.refs/RobbsLuo_Coding2API/src/provider/qoder/cosy.py` | 私有签名协议 | Source Evidenced |
| 凭据归一化 | `.refs/RobbsLuo_Coding2API/src/provider/qoder/credential.py` | 字段、到期、刷新合并 | Source Evidenced |
| 单测 | `.refs/RobbsLuo_Coding2API/tests/provider/qoder/` | 行为约束（`test_oauth.py`、`test_checkin.py`、`test_client.py`、`test_cosy.py`、`test_credential.py`） | Source Evidenced |
| PolyGate 现有契约 | `src/providers/mod.rs`、`src/account_service/`、`src/admin/`、`src/checkin/` | 复用边界 | 已实现 |
| 本地官方客户端 | `~/.qoder-cn/` | 仅确认目录/文件名 | 本地观察 |

许可证：Coding2API 为 MIT，但本项目**不复制其代码**，只吸收协议事实并独立实现。
参考仓库的测试夹具、抓包与模型缓存不进入 PolyGate；COSY 测试向量由本项目按协议
自行构造（可用参考实现的公开算法做交叉校验，但不复制文件）。

### 1.2 结论摘要

1. **Qoder 是 Model Provider，不是 Agent Provider**：暴露的推理接口是
   OpenAI 形状的流式 chat + tools；`agent_chat_generation` 只是端点命名，
   没有任务创建/查询/取消 API。Source Evidenced。
2. **认证是设备码 + PKCE(S256) 轮询**，不是 OAuth 回调。用户浏览器完成授权，
   网关轮询 `/api/v1/deviceToken/poll`；没有 `code` 回调，也没有 `redirect_uri`。
   Source Evidenced。
3. **CN / Intl 是完全独立的服务面**：不同 openapi/gateway/website/domain/UA/
   nonce 形态；必须按 `Region` 隔离账号、端点与配置，禁止跨区回退。
   Source Evidenced。
4. **推理必须使用 COSY 私有签名**：自定义 Base64 变体、AES-128-CBC 身份密文、
   RSA 包裹会话密钥、`md5` 签名串；模型列表也必须带 COSY 头。
   Source Evidenced。
5. **签到已改为活动制**：`/sash/api/v1/me/campaigns` + `/{id}/claim`，
   必须带 `Cosy-ClientType: 10`；旧 `daily-check-in` 仅作 404/405/410 回退。
   签到会改变账号奖励状态，默认只查状态，claim 必须单独显式授权。
   Source Evidenced；CN 作者实测，PolyGate 未实测。
6. **没有原生会话/任务状态**：无 upstream session id、无 chat 恢复、无取消 API。
   `native_session=false`、`agent_task=false`、`cancellation=false`。
   Source Evidenced / Unsupported。
7. **PolyGate 现有账号、凭据、管理台流程、签到执行器可复用**，不需要新表；
   主要新增是 Qoder adapter、COSY signer、设备轮询流程与 region 配置。

## 2. PolyGate 现有可复用点

| 能力 | PolyGate 现状 | Qoder 复用方式 | 判定 |
| --- | --- | --- | --- |
| Provider 契约 | `src/providers/mod.rs:494` `Provider` trait；`chat`/`chat_stream`/`list_models`/`quota`/`auth_status` | 新增 `QoderProvider`，实现 chat/stream/models/quota/auth_status | 可复用 |
| 能力声明 | `src/providers/mod.rs:201` `Capabilities` 九个布尔位 | 按 §5 声明；默认 false 的位不打开 | 可复用 |
| 验证状态 | `src/providers/mod.rs:296` `VerificationState` | 初始 `Experimental`；真实账号后才逐能力升级 | 可复用 |
| 区域 | `src/config/mod.rs:189` `Region::{Cn,Intl}`；`ProviderKind::Qoder` 已存在（`:220`） | 同 kind 不同 region 独立注册、独立账号 | 可复用 |
| 账号/凭据 | `AccountService::create/find_by_external`（`src/account_service/service.rs:165`、`:265`）；`CredentialService::save/update/status`（`src/account_service/credential.rs:47`、`:72`、`:119`） | 登录成功后 `external_id = qoder:<region>:<uid>`；凭据 CAS 写入 | 可复用 |
| 加密 | `security::SecretBox` + AAD | 授权流程的 verifier/nonce 加密；凭据由 CredentialService 加密 | 可复用 |
| 管理台鉴权 | `src/admin/mod.rs:149` 起的 admin 路由、会话/CSRF/Origin | Qoder 的 start/status/cancel 走同一中间件 | 可复用 |
| 授权流程表 | `src/storage/admin.rs:29` `AuthFlowRow`；`migrations/0005_admin.sql:37` | `kind="qoder"`，`secrets_encrypted` 存 PKCE/nonce/machine_id | 可复用（无需迁移） |
| 单活动流程约束 | `migrations/0005_admin.sql:56` 唯一部分索引 | Qoder 与 CodeArts 共用“同时只能有一个授权中” | 可复用（已知限制） |
| 签到框架 | `src/checkin/executor.rs:39` `CheckinExecutor`；`CheckinRequest/Outcome`（`src/checkin/model.rs:166`、`:178`） | 新增 `QoderCheckinExecutor` 注册到 `(Qoder, Cn/Intl)` | 可复用 |
| 签到调度 | `CheckinService` / `CheckinScheduler` | 不新增调度器，只注册执行器 | 可复用 |

### 2.1 需要新增但不改现有结构的部分

- `src/providers/qoder/`：`mod.rs`（Provider 实现）、`config.rs`（RealmConfig）、
  `cosy.rs`（签名/编码）、`client.rs`（HTTP）、`events.rs`（SSE/解析）、
  `auth.rs`（设备轮询）、`credential.rs`（字段归一）。
- `src/checkin/qoder.rs`：`CheckinExecutor` 实现。
- `AdminConfig` 增加 Qoder 的 realm 地址覆盖与超时（仅为测试/灰度；默认用常量）。
- `AdminState` 增加 Qoder 授权流程方法；沿用 `AuthFlowRow`，不新增表。

### 2.2 一个明确的限制

`admin_auth_flows` 的唯一部分索引只允许**全局一个** pending/exchanging 流程。
Qoder 接入后仍然成立：不能同时给 CodeArts 和 Qoder 授权。M13-D2 沿用现状，
不在本阶段改 schema；若后续需要并发授权，再单独评估。

## 3. CN / Intl 区域隔离

### 3.1 区域事实表

| 项 | CN | Intl | 证据 |
| --- | --- | --- | --- |
| openapi | `https://openapi.qoder.com.cn` | `https://openapi.qoder.sh` | `events.py:71` |
| 推理 gateway | `https://gateway.qoder.com.cn` | `https://api1.qoder.sh` | `events.py:71` |
| gateway 回退 | 无 | `api2.qoder.sh`、`api3.qoder.sh` | `events.py:71`；`gateway_candidates` `:182` |
| website | `https://qoder.cn` | `https://qoder.com` | `events.py:71` |
| client_id | `e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb` | 同左 | `events.py:71` |
| redirect_uri（不发送） | `qoder-work-cn://` | `qoder://aicoding.aicoding-agent/login-success` | `events.py:71`、`auth.py:100` |
| domain（官方客户端目录） | `qoder.com.cn` | `qoder.com` | `events.py:71` |
| User-Agent | `QoderWork/1.1.64` | `Qoder/1.1.64` | `events.py:71` |
| 授权 nonce | 带横线 UUID | 无横线 hex UUID | `events.py:71`、`auth.py:191` |
| 签到 | 活动制 CN 已观察 | Intl 未确认；旧接口 404/405/410 | Source Evidenced / Unverified |

来源标注：`events.py` 指
`.refs/RobbsLuo_Coding2API/src/provider/qoder/events.py`。

### 3.2 隔离规则

1. **区域必须显式选择**：管理台添加 Qoder 账号时由用户选择 CN/Intl，
   不根据邮箱、IP 或浏览器语言猜测。
2. **账号只属于一个区域**：`(ProviderKind::Qoder, Region)` 是注册键；
   `external_id` 包含 region；CN 账号不得调用 Intl 端点，反之亦然。
3. **不做跨区回退**：CN 登录失败不得自动尝试 Intl；Intl 签到 404 不得回落 CN。
4. **不共享 token**：两个区域的上游不同，token/refresh token 不可互换。
5. **配置独立**：`qoder.cn.*` 与 `qoder.intl.*` 分开；默认值来自 §3.1，
   允许配置文件覆盖（测试/灰度），但不得把 Intl 地址填进 CN。
6. **同一 client_id 不代表同一服务**：CN/Intl 使用相同 client_id 是
   Source Evidenced 的事实，但授权页、token、额度、签到都按区域独立处理。

### 3.3 区域识别

参考实现有 `detect_realm_from_domain`（`events.py:172`）：域名含 `qoder.sh`
或含 `qoder.com` 但不含 `qoder.com.cn` 判为 Intl，否则 CN。该函数只用于
**导入已有凭据**时的提示，不作为登录流程的推断依据。PolyGate 登录流程必须
显式传 region；导入路径若缺 region，默认拒绝并要求操作者选择（避免把 Intl
账号写进 CN 池）。

## 4. 认证：设备码 + PKCE 轮询

### 4.1 流程

```text
管理员(浏览器)                PolyGate                        Qoder 上游
     |  选择 Qoder + 区域 + 名称   |                                |
     |-------------------------->|                                |
     |                           | 生成 verifier/challenge(S256)  |
     |                           | 生成 nonce + machine_id        |
     |                           | 拼授权 URL（本地，不触网）      |
     |<-- authorize_url ---------|                                |
     |  打开授权页并登录          |                                |
     |------------------------------------------------------------>|
     |                           | GET deviceToken/poll?nonce=... |
     |                           |------------------------------->|
     |                           |  202/404 = 等待；200 + token   |
     |                           |<-------------------------------|
     |                           | GET /api/v1/userinfo (Bearer)  |
     |                           |------------------------------->|
     |                           |<-- uid/nickname/org -----------|
     |                           | 加密保存凭据，流程 completed    |
     |<-- 状态: completed -------|                                |
```

### 4.2 本地构造

- PKCE：`verifier` 64 字节随机、字符集 `A-Za-z0-9-._~`；`challenge` 为
  BASE64URL(SHA256(verifier)) 去 padding，`challenge_method=S256`
  （`auth.py:91`）。
- nonce：CN 为 `str(uuid4())`（带横线），Intl 为 `uuid4().hex`
  （`auth.py:191`）。
- machine_id：参考实现优先读官方客户端 `~/{domain}/.auth/machine_id`，
  读不到再用本机派生的带横线 UUID（`auth.py:53`、`:74`）。
  **PolyGate 不读取官方客户端文件**；每次授权流程生成一个带横线 UUID，
  只用于授权页参数，不写入账号长期身份。设备长期身份来自 COSY 的 uid 派生
  （§6.1），不需要这个 machine_id。
- 授权 URL：
  `{website}/device/selectAccounts?challenge=...&challenge_method=S256&nonce=...&client_id=...&machine_id=...`
  （`auth.py:100`、`:116`）。当前 CN/Intl 都**不发送** `redirect_uri`
  （`send_redirect_uri=False`）；带上会被授权页拒绝（参考源码注释）。
- 授权状态 TTL：600 秒（`auth.py:46`）；状态单所有者，成功后消费，
  重复 poll 不得重复建账号（`AuthStateStore`，`auth.py:128` 起）。

### 4.3 轮询语义

`GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256`

| 上游响应 | 含义 | PolyGate 处理 |
| --- | --- | --- |
| `202` 或 `404` | 用户尚未完成授权 | 继续等待，不报错（`auth.py:48`） |
| `200` + `token`/`device_token` | 授权完成、token 就绪 | 拉 userinfo，保存凭据 |
| `200` 但无 token | 授权完成、token 未就绪 | 继续等待（`auth.py:227`） |
| 其他 4xx/5xx | 失败 | 流程失败，记录受控错误，不泄漏正文 |
| 超过 600s | 过期 | 流程 `expired`，需重新发起 |

`poll` 请求头只需 `Accept: application/json` 与 UA，不带 Authorization
（`auth.py:227`）。轮询间隔参考 5 秒（`auth.py:191` 的 `interval=5`）。

### 4.4 身份与凭据字段

登录成功后应从上游取得：

| 字段 | 来源 | 用途 | 证据 |
| --- | --- | --- | --- |
| `access_token` | deviceToken `token` / `device_token` / `access_token` | COSY 身份密文、openapi Bearer | `credential.py:147` |
| `refresh_token` | deviceToken `refresh_token` / `refreshToken` | 刷新（唯一保活手段） | `credential.py:182` |
| `uid` | userinfo `id`，其次 deviceToken `user_id`/`userId`/`uid` | COSY 设备派生、账号去重 | `credential.py:182` |
| `nickname` | userinfo `name`/`nickname` | 展示 | 同上 |
| `user_type` | userinfo `user_type`/`userType`，默认 `personal_professional_trial` | COSY 身份字段 | 同上 |
| `organization_id/name` | userinfo | COSY 身份字段 | 同上 |
| `expires_at` | `expires_in`（毫秒）/ `expires_at`(RFC3339) / JWT `exp` | 预刷新 | `credential.py:156` |
| `realm` | 流程显式选择 | 端点选择 | `credential.py:58` |
| `domain` | realm 配置 | 官方客户端目录/诊断 | `credential.py:182` |

**uid 缺失的处理**：参考实现用 access token 前 16 位兜底（`auth.py:204`），
因为随机值会让同一账号每次登录变成不同设备。PolyGate 设计上更严格：
userinfo 失败且 deviceToken 无 uid 时，**拒绝完成登录**并提示“无法确认账号
身份，请重试”，避免多账号被上游关联成同一设备。该决定需要 M13-D2 确认；
若上游确实不返回 uid，再退化为“token 前缀 + 明确告警”。

### 4.5 刷新与到期

- 端点：`POST {openapi}/api/v1/deviceToken/refresh`
  body `{"refresh_token": "..."}`，**不带 Authorization**（`client.py:577`）。
- 响应只含 token 族字段；`merge_refreshed` 保留 uid/昵称/组织/realm
  （`credential.py:214`），否则刷新后 COSY 身份退化为 anonymous。
- `expires_in` 是**毫秒**；`expires_at` 是 RFC3339；都没有时回落 JWT `exp`；
  仍不可得则为 0（未知），**不编造 30 天**（`credential.py:156`）。
- 刷新成功必须让旧 COSY 会话失效（token 变了，`client.py:577` 末尾）。
- 刷新是否一次性、旧 refresh token 是否立即失效：**Unverified**。
  PolyGate 必须单写者 + 凭据 revision CAS，禁止并发刷新。
- 登录、刷新都只走官方公开的设备授权/刷新接口，不破解验证码、不模拟 MFA、
  不读取浏览器 LocalStorage、不要求用户粘贴 token。

### 4.6 认证证据等级

| 结论 | 等级 |
| --- | --- |
| 设备码 PKCE 轮询、202/404 pending、600s TTL、无 redirect_uri | Source Evidenced |
| CN/Intl 的 website/client_id/nonce 形态 | Source Evidenced（参考提交时点） |
| 官方客户端版本漂移（参考 1.1.64 / 本地 1.1.71）后参数是否仍有效 | Unverified |
| PolyGate 真实账号登录 | 未执行，Unverified |

## 5. Provider 分类与能力矩阵

### 5.1 分类

**Model Provider（OpenAI-compatible chat surface）**。

理由：

- 唯一推理入口是 `agent_chat_generation`，请求/响应是 OpenAI chat 形状，
  支持流式、tools、`reasoning_content`（`client.py:201`、`events.py:404`）。
- 端点名里的 `agent` 与 `agent_id=agent_common` 是上游固定字段，
  不代表存在任务生命周期 API。
- 没有“创建任务/查询状态/取消任务/读取上下文”的独立接口；参考实现也没有
  Agent Task 客户端（Source Evidenced）。
- 因此不把 Agent 协议强行塞进 OpenAI Chat；`agent_task` 保持 Unsupported。

### 5.2 能力矩阵

| 能力 | 值 | 依据 | 等级 |
| --- | --- | --- | --- |
| `authentication` | true | 设备码 PKCE | Source Evidenced + Live Verified（CN） |
| `model_discovery` | true | `/algo/api/v2/model/list` | Live Verified（CN，11 个模型） |
| `quota` | true | `/api/v2/quota/usage` | Live Verified（CN） |
| `chat` | true | `agent_chat_generation` | Live Verified（CN，非流式聚合） |
| `streaming` | true | 上游只支持流式；SSE 信封 | Live Verified（CN） |
| `tools` | true | tools/tool_choice 透传；响应兼容两种 tool 形状 | Live Verified（CN，含多轮工具结果） |
| `native_session` | false | 无 upstream session id / 恢复接口 | Source Evidenced |
| `agent_task` | false | 无任务 API | Unsupported |
| `cancellation` | false | 无取消接口；只能断开连接 | Unsupported |

Provider 状态仍为 `VerificationState::Experimental`（整体不升级）；逐能力实测结果
记录在 `PROGRESS.md` §31 与 `docs/PROVIDERS.md`。

### 5.3 非流式 Chat 的实现方式

参考实现只发 `stream: true`（`client.py:201` 固定写入），没有非流式上游调用。
PolyGate 的 `Provider::chat` 应在网关内消费 SSE 并聚合为 `ChatResponse`：

- 聚合 content / reasoning / tool_calls / usage / finish_reason；
- 不额外发明一个上游非流式端点；
- 聚合器与 CodeArts 的 SSE 聚合逻辑可共享，但不要假设两边帧格式相同。

这是 PolyGate 设计决定，不是上游事实。

## 6. Chat、SSE、多轮与错误

### 6.1 COSY 请求签名

> **2026-10-29 实测修正**：本节原稿基于 Coding2API 1.1.64 的签名形状。真实 CN 网关
> 对该形状返回 `403 {"code":"101","message":"Signature invalid"}`。逐项比对本机官方
> 客户端（Qoder CN 1.1.71，`resources/app.asar` 的 `lXt`/`iqe`）后，下列方案已用真实
> 账号端到端验证（模型列表 + Chat/SSE/工具）。参考实现的“排序 JSON、富身份、
> `Cosy-ClientType` 头族”均不是必需，且被拒绝。详见 `PROGRESS.md` §31.2。

推理与模型列表使用 COSY，而不是普通 Bearer：

1. 生成临时密钥 `temp_key`：**16 个十六进制字符**（uuid 去横线截前 16 位）。
   任意 16 字节的二进制密钥会被上游拒绝（同样表现为 `Signature invalid`）。
2. `info` = base64(AES-128-CBC(身份 JSON, key=temp_key, iv=temp_key))。身份 JSON
   键序固定：`uid`、`aid`（空串）、`name`、`email`（未知时为空串）、
   `security_oauth_token`（access token）。不包含 refresh token 或组织字段。
3. `cosy_key` = base64(RSA_PKCS1v15(temp_key))，使用与官方客户端相同的服务端公钥
   （`cosy.rs` 中的常量）。
4. payload = base64(固定顺序 JSON：
   `{"version":"v1","requestId":"<无横线 uuid>","info":"…","cosyVersion":"1.1.71","ideVersion":""}`)。
5. `Authorization: Bearer COSY.<payload_b64>.<digest>`；
   `digest = md5(payload_b64 
 cosy_key 
 date 
 body 
 path)`；
   `path` 是 URL path 去掉 `/algo` 前缀，query 不参与。
6. 请求头只需 `cosy-user`、`cosy-key`、`cosy-date`、`authorization`，加固定
   `user-agent: Go-http-client/2.0`、`accept-encoding: identity`、`accept`、
   `content-type`；推理再加 `cache-control: no-cache`、`x-model-key`、
   `x-model-source: system`。默认**不发送** `cosy-clienttype` / `cosy-machineid` /
   `cosy-machinetoken` / `cosy-clientip` / `login-version` 等客户端身份字段；
   `client.identity = "impersonate"` 时全部补齐（`cosy-machineid` 等按 uid 稳定派生，
   见 `src/providers/profile.rs`）。签到活动接口在两种模式下都需要
   `Cosy-ClientType: 10`（§10）。
7. `requestId` 每请求新生成；COSY 会话按凭据缓存，token 轮换后必须重建。
8. 模型列表的签名 body 是 `qoder_encode("")`（即空串）；方法必须是 GET。

合规说明：默认方案只保留协议必需的加密身份字段，没有设备指纹、没有 `clientip`、
没有版本/登录版本头，也不读取官方客户端的 machine id；`info` 中只放 uid、昵称和
access token（由 `SecretBox` 解密后仅在内存中使用）。M15 把上述字段做成
`client.identity` 开关，取值仍是派生的公开值，不落盘、不读取本机客户端文件。

### 6.2 推理请求

- `POST {gateway}/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`
  （`events.py:134`）。
- body（`client.py:201`）：
  - `messages`：原样透传；
  - `model`：下游模型 id；
  - `stream: true`；
  - `agent_id: "agent_common"`；
  - 透传 `tools`、`tool_choice`、`temperature`、`top_p`、`stop`、
    `presence_penalty`、`frequency_penalty`、`response_format`；
  - `parameters.max_tokens`（或 `max_completion_tokens`）；
  - `parameters.reasoning_effort`（或 `reasoning.effort`）。
- 请求体需要 `qoder_encode` 自定义编码（`cosy.py:55`），并参与签名。
- gateway 回退：Intl 有 api1→api2→api3（`client.py:325`）。只在**尚未收到
  任何内容帧**时切换；已产生 token 计费的响应不得重放，否则可能重复计费。
  这是 PolyGate 的策略决定，不是参考实现承诺。

### 6.3 多轮与 system prompt

- `messages` 原样透传，包括 `role: "system"`；参考实现**不注入**任何
  baseprompt/chat_context 模板（`client.py:201` 的 docstring 明确拒绝凭空补
  未验证字段）。Source Evidenced。
- 多轮工具调用的两个必要条件（`client.py:131`、`:156`）：
  1. assistant 的 `content:null` 归一为空串——上游会把 null 当空消息跳过，
     其后的 tool 结果变成“孤儿”，报 tool 配对 400；
  2. 丢弃没有对应 assistant tool_call 的孤立 `role:"tool"` 消息。
- assistant `tool_calls` 必须保持 OpenAI 形状（`id` + `type:"function"` +
  `function.name/arguments`）。这一点与 PolyGate M13-C2 修过的 CodeArts
  问题相同，Qoder adapter 必须复用正确的上游形状，不能直接序列化内部
  `ToolCall`。Source Evidenced（Qoder 侧）+ 本项目实测教训（CodeArts 侧）。
- 上游是否接受空 system、多个 system、developer role：**Unverified**；
  D2 用真实账号逐项验证，不在实现中猜测。

### 6.4 SSE 信封与结束

- 外层帧：`data: {"statusCodeValue":200,"body":"<内层 JSON 字符串>",...}`
  （`events.py:295`）。
- 内层 `body`：OpenAI chunk（`choices[].delta`），可能同时含
  `content`、`reasoning_content`、`tool_calls`、`usage`、`finish_reason`
  （`events.py:404`）。
- 保活帧：外层 200、内层 `"null"`（或空对象/空数组），必须跳过而不是报错
  （`events.py:331`）。
- 结束：外层/顶层 `[DONE]`；或连接正常关闭。无 `[DONE]` 的关闭视为正常结束
  还是截断，需要按是否收到 `finish_reason` 判断；两者都没有则
  `UpstreamProtocolViolation`。
- 内层 `statusCodeValue` 非 200：按错误处理，不能当成内容（`events.py:295`）。
- 控制帧（官方客户端同样特殊处理）：`data: [NOTIFICATIONS]#…` 是遥测噪声，
  直接跳过；`data: [NOT_EXCEED_QUOTA]` 是“尚未超额”的保活；`data: [EXCEED_QUOTA]`
  表示本次请求额度耗尽。PolyGate 把后者归一成 `402` 信封 → `QuotaExhausted`，
  避免半截回答后只剩“缺少 finish_reason”的截断错误。该映射依据官方客户端的
  标记名与忽略逻辑，**尚无真实超额样本**。
- 中断：无字节超过 120 秒视为空闲中断（`Timeout`），不再无限占用会话/账号租约；
  连接中断沿用 `Transport`/`Timeout` 分类。流内错误帧（如 402）作为终止错误
  返回，下游收到 error 事件 + `[DONE]`，不会把半截内容当成功。
- 上游可能不发 `usage`；此时 PolyGate 的 usage 必须保持未知/0 的既有语义，
  不得估算 token。

### 6.5 错误分类

参考实现把 HTTP 状态与流内信封状态统一走 `classify_error_code`
（`events.py:207`、`:255`）。映射到 PolyGate `ErrorKind`：

| 上游信号 | 参考分类 | PolyGate `ErrorKind` | 处理 |
| --- | --- | --- | --- |
| 402 | CREDIT | `QuotaExhausted` | 冷却到额度恢复；不封号 |
| 正文含 `Billing daily count exceeded`（403/110） | CREDIT | `QuotaExhausted` | 日边界恢复，不重登、不硬禁用 |
| 排队标记（`isQueued` 等） | QUEUE | `RateLimited` | 短退避/换模型 |
| 401 | DEAD | `AuthInvalid` / `AuthExpired` | 单写者刷新；仍失败则标记需重新登录 |
| 403 + `TOKEN_EXPIRE`/`12153`/`Offline user session not found` | DEAD | `AuthInvalid` | 刷新；失败则重新授权 |
| 403 其他 | SOFT | `RateLimited` 或 `UpstreamServer` | 短冷却，不硬禁用 |
| 429 | MODEL | `RateLimited` | 模型级冷却，可换模型 |
| 418/500/502/503/504 | SOFT | `UpstreamServer` | 短冷却换号 |
| 400/404/422 | INVALID | `BadRequest` / `ModelUnavailable` | 请求或模型无效，换号无用 |
| 400 + `[FAIL]node:` / `Execution failed` | MODEL（节点瞬时） | `UpstreamServer` | 只冷却该模型，可重试其他模型 |
| 超时/DNS/TLS/连接重置 | Transport/Timeout | `Timeout` / `Transport` | 未收到内容帧才允许换 host |
| 其他 | OTHER | `Internal` / `UpstreamServer` | 记录脱敏分类 |

证据：Source Evidenced。真实账号下的状态码分布与是否仍有新变体：Unverified。

实现说明（2026-10-29，`PROGRESS.md` §32）：HTTP 状态与流内信封共用同一判据；
`400 + [FAIL]node:` 归 `UpstreamServer`（节点瞬时，不罚账号），`404/400 + model
markers` 归 `ModelUnavailable`（换号无用），429 归 `RateLimited`，402 与
`[EXCEED_QUOTA]` 归 `QuotaExhausted`。PolyGate 目前只有账号级失败策略
（`classify_session_failure`），**没有**按模型冷却的独立表；模型级错误不会误伤账号。

## 7. Tool Calling

| 项 | 事实 | 证据 |
| --- | --- | --- |
| 请求 | `tools`、`tool_choice` 原样透传 | `client.py:201` |
| 非流式响应 | PolyGate 聚合 SSE 后得到 `tool_calls` | 设计决定 |
| 流式响应 | 增量 `tool_calls`（id/name/arguments 分片） | `events.py:391` |
| 旧式兼容 | 单数 `function_call` 归一为 `tool_calls` | `events.py:391` |
| 空噪声 | 无 name 且 arguments 空的调用片丢弃 | `events.py:377` |
| 多轮回传 | assistant tool_calls + `role:"tool"`；null content 归一、孤立 tool 丢弃 | `client.py:131`、`:156` |
| `finish_reason` | 由内层 choice 透传，通常为 `tool_calls` | `events.py:404` |

Qoder 的 tool 形状与 CodeArts 不完全相同：CodeArts 需要 `type:"function"`
包装，Qoder 参考实现同时接受新式 `tool_calls` 与旧式 `function_call`。
PolyGate 统一输出 OpenAI 新式形状，不要按渠道分叉内部协议。

真实工具调用、并行 tool_calls、参数增量拼接、工具结果多轮：**Unverified**，
M13-D2 验收的第一优先级（与 CodeArts 相同，因为这是 Coding Agent 的核心）。

## 8. Agent 工作流

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 是否有独立 Agent Task API | 否 | Source Evidenced（参考实现只有 chat） |
| 是否有任务创建/状态/事件/取消 | 否 | Unsupported |
| 是否有上下文/工作区管理 | 否（网关侧自行维护 messages） | Unsupported |
| `agent_chat_generation` 是否等价 Agent | 否，只是推理端点命名 | Source Evidenced |
| 桌面端是否有未公开的 agent 协议 | 可能有，但无证据 | Unverified |

设计约束：

- `Capabilities::agent_task = false`，`Provider::agent_task` 保持默认
  `Unsupported`。
- 不把 Agent 的“任务/步骤/工具编排”概念塞进 OpenAI Chat；PolyGate 只做
  chat + tools 的透明转发。
- 若未来要支持 Qoder Agent，必须先拿到独立的协议证据，再新增能力位与
  数据模型；不在 M13-D2 预留半成品。

## 9. 模型发现与额度

### 9.1 模型发现

- 端点：`GET {gateway}/algo/api/v2/model/list?Encode=1`（`events.py:136`）。
- 必须带整套 COSY 头；签名 body 是 `qoder_encode("")`，方法必须 GET
  （`events.py:138` 注释：带头 POST/PUT 会 400，不带头裸 GET 会 403）。
- 响应 `payload["chat"]`，每项字段（`events.py:467`）：

| 上游字段 | PolyGate 目标 | 说明 |
| --- | --- | --- |
| `key` | `UpstreamModel.id` | 必需；空则跳过 |
| `display_name` | `UpstreamModel.display_name` | 缺失为 `None` |
| `max_input_tokens` | `UpstreamModel.context_window` | 缺失为 `None`，不猜 |
| `price_factor` | 展示用 credit 倍率（可放 `raw`/metadata） | 0 表示免费；不是余额 |
| `is_vl` | `ModelCapabilities.vision` | 缺失为 false/unknown |
| `supportsToolCall` | `ModelCapabilities.tools` | 缺失不谎报 true |
| `is_reasoning` | `ModelCapabilities.reasoning` | 缺失为 false/unknown |

- 无可用模型时参考实现直接报协议错误（`events.py:467` 末尾），
  PolyGate 应把该账号标为模型发现失败，而不是返回空列表冒充“无模型”。
- 参考实现按 realm 缓存 10 分钟（`client.py:372` 注释）。PolyGate 的
  `ModelCatalog` 已有自己的刷新策略，D2 只接 `list_models`，不重复建缓存层。
- 模型 id 是否区分 CN/Intl：**Unverified**；按区域分别发现，不合并列表。

### 9.2 额度

- 端点：`GET {openapi}/api/v2/quota/usage`，纯 Bearer（`events.py:109`、
  `client.py:428`）。
- 解析（`events.py:511`）：
  - `userQuota.remaining/total/used`：基础额度；
  - `addOnQuota.remaining/total/used`：赠送/签到等附加额度；
  - `remaining = userQuota.remaining + addOnQuota.remaining`；
  - `total = userQuota.total + addOnQuota.total`；
  - `expiresAt`：到期 epoch（毫秒归一）；
  - 明细包（基础/赠送）可放 `QuotaSnapshot.raw`，不新增表字段。
- 单位：`QuotaUnit::Credits`；`price_factor` 只是消耗倍率，不是余额。
- 免费/订阅/赠送的区分：基础 vs 附加额度可区分；具体套餐类型需要
  `/api/v2/user/plan`（`events.py:110`），参考实现只定义常量、未解析。
  该端点**Unverified**，D2 不实现，除非真实账号验证有明确字段。
- **未知语义**：字段缺失时不能按 0 处理。参考实现 `_num` 缺省 0
  （`events.py:504`），PolyGate 必须更严格：只要关键字段缺失/非数值，就返回
  `QuotaStatus::Unknown` 或 `Partial`，`remaining/total` 为 `None`。
  这是本项目 `AGENTS.md` 的硬性要求（未知不得写 0/无限）。
- 额度是否跨 CN/Intl、是否按模型分组：Unverified；按账号/区域独立存储。

### 9.3 消耗与恢复

- 402 / `Billing daily count exceeded` 表示额度或日计费次数用尽：
  前者等待额度恢复，后者等待上游日边界；都不应硬禁用账号。
- 日边界时区：参考注释称“次日 04:00”（`events.py` 的 `DAILY_QUOTA_MARKERS`
  注释），但具体时区/是否随账号变化 **Unverified**；PolyGate 只做短冷却 +
  重新探测，不写死“04:00 恢复”。
- 签到奖励到账后应触发一次额度刷新（复用 `QuotaRefreshHook`），但不得把
  “签到成功”当成“额度已到账”的证据。

## 10. 签到

### 10.1 端点与协议

| 步骤 | 请求 | 关键要求 | 证据 |
| --- | --- | --- | --- |
| 查活动 | `GET {openapi}/sash/api/v1/me/campaigns` | 必须 `Cosy-ClientType: 10` | `events.py:117`、`client.py:435` |
| 领奖励 | `POST {openapi}/sash/api/v1/me/campaigns/{campaignId}/claim` | 同上；body `{}` | `events.py:118`、`client.py:517` |
| 旧查状态（回退） | `GET /sash/api/v1/me/daily-check-in/status` | 仅当活动制 404/405/410 | `events.py:111`、`client.py:486` |
| 旧领取（回退） | `POST /sash/api/v1/me/daily-check-in/claim` | 仅回退路径 | `events.py:112`、`client.py:545` |

- `Cosy-ClientType` 是**必需**的：缺失时上游静默返回
  `{"campaigns":[]}`（HTTP 200），看起来像“活动未开放”，实际是被降级。
  Source Evidenced（参考作者实测注释）。
- 活动筛选：`actionType == "CLAIM_BENEFIT"` 才是可领奖励；
  `VIEW_DETAILS` 等展示位即使 `claimStatus=CLAIMABLE` 也必须过滤
  （`events.py:127`、`client.py:435`）。
- 状态：`CLAIMABLE` 可领、`CLAIMED` 已领、`BLOCKED` +
  `SAME_PERSON_ALREADY_CLAIMED` 表示同一自然人已领（归一为“今日已签”，
  不是失败）（`events.py:127`、`client.py:517`）。
- claim 响应：`replayed:true` 视为幂等重放/今日已签；其他 `BLOCKED` 才是
  真实失败（`events.py:665`、`client.py:517`）。

### 10.2 时间窗口

- 每日签到窗口按 UTC+8 10:00 轮换：当前窗口 10:00 → 次日 09:59；
  `endAt` 的最大值就是下一轮开放时刻（`events.py:587`）。
- 封账应封到窗口结束，而不是本地自然日 00:00；否则凌晨那轮会把整天封掉，
  表现为“10:00 新窗口不再签”（参考实现踩过的坑，`events.py:587` 注释）。
- 没有连续签到天数：新协议 `streak_days = None`，不得编造 0
  （`events.py:607`）。
- 时区固定 UTC+8 是 CN 观察；Intl 是否同样：**Unverified**。

### 10.3 不可用与失败分类

| 情形 | 参考行为 | PolyGate 应映射 |
| --- | --- | --- |
| campaign 端点 404/405/410 且旧接口也不可用 | `ok=false` + 明确 message | `CheckinOutcome::Unsupported`（不是 `Failed`） |
| 活动列表为空且 `active=false` | 不重试 | `Unsupported`，summary 为“本区域无签到活动” |
| 今日已 CLAIMED | 已签 | `AlreadyCheckedIn` |
| 可领但用户未授权 claim | 只查询 | 只返回状态，不调用 claim |
| claim 返回 `BLOCKED`（非同一自然人） | 失败 | `Failed { category: Upstream }` |
| 同一自然人已领 | 已签 | `AlreadyCheckedIn` |
| `replayed:true` | 已签 | `AlreadyCheckedIn` |
| 401/会话失效 | 认证失败 | `Failed { category: Auth }`，并标记凭据需刷新/重登 |
| 429 | 限流 | `Failed { category: RateLimit }`（唯一允许自动重试的类别） |
| 超时/连接重置 | 结果未知 | `Unknown`（绝不自动重试） |
| 5xx | 上游故障 | `Unknown` 或 `Failed { Upstream }`；若请求已发出，优先 `Unknown` |

**关键设计修正**：`CheckinOutcome` 没有 `Unavailable` 变体，
`CheckinStatus::Failed` 又允许自动重试（`src/checkin/model.rs` 的
`allows_auto_retry`）。因此“本区域没有签到接口”必须映射为
`CheckinOutcome::Unsupported`（settled、不重试），不能映射为 `Failed`，
否则调度器会每轮重试一个永远不存在的接口。

### 10.4 与 M12 框架的接线

- 新增 `QoderCheckinExecutor`，实现 `CheckinExecutor`
  （`src/checkin/executor.rs:39`）：
  - `supports(ProviderKind::Qoder)`：该 executor 已按 region 注册，因此返回 true；
    “本区域没有签到接口”由 `check_status`/`checkin` 返回 `Unsupported` 表达，
    这样 D2 仍能做只读探测；
  - `check_status`：只读，查 campaign/旧状态；返回
    `CheckinProbe::Confirmed`、`Unknown`，或 `Err(Unsupported)`；
  - `checkin`：执行 claim；但**默认不自动调用**，由显式授权开关控制。
- 注册方式：`RoutingCheckinExecutor::with(ProviderKind::Qoder, Region::Cn/Intl, ...)`
  （`src/checkin/executor.rs:67`），不新建调度器。
- 默认策略：
  - 自动签到默认关闭；
  - 管理台/CLI 只能先“查询状态”；
  - claim 必须由操作者显式确认，且每个账号每日最多一次；
  - 不得反复执行可能产生副作用的请求。
- CN 已观察有活动制签到；Intl 的可用性 **Unverified**。Intl 返回 404/405/410
  时必须显示“本区域未开放”，不能显示“签到成功”。

### 10.5 签到证据等级

| 结论 | 等级 |
| --- | --- |
| 活动制端点、`Cosy-ClientType: 10`、CLAIM_BENEFIT 过滤、窗口 10:00 | Source Evidenced（含参考作者 CN 实测） |
| Intl 签到是否可用 | Unverified |
| 真实账号 claim 结果、奖励到账时间 | 未执行，Unverified |
| PolyGate 幂等/重启/重复 claim | 未实现，Unverified |

## 11. 会话、并发与限流

### 11.1 会话

| 问题 | 结论 | 证据 |
| --- | --- | --- |
| 是否有 upstream session id | 无（COSY 请求无会话 id） | Source Evidenced |
| 是否可恢复中断的 chat | 否 | Unsupported |
| 是否有 prompt cache / chat_id 亲和 | 未发现；`requestId` 每请求新生成 | Source Evidenced / Unverified |
| 多轮是否依赖服务端状态 | 否，依赖客户端回传 messages | Source Evidenced |
| COSY session cache 是否等于会话 | 否，只是签名材料缓存 | Source Evidenced |

因此：

- `native_session = false`。
- PolyGate 的会话粘性只能做**下游到账号**的粘性（同一客户端会话固定到同一
  Qoder 账号），不能伪造上游 session id。
- 不复制 CodeArts 的“3 会话墙”假设；Qoder 没有证据表明存在同类限制
  （Unverified，不是“无限制”）。

### 11.2 并发与限流

- 已知信号：429（模型级）、403 排队标记、402/日计费次数用尽、403 瞬时抖动
  （`events.py:207`）。
- 未知：每账号并发上限、每分钟请求数、是否按模型限流、是否按设备/uid 限流。
  全部 **Unverified**。
- 设计：
  - 复用现有账号并发租约/冷却，不新增 Qoder 专用限流器；
  - 429/排队 → 短冷却 + 可换模型/账号；
  - 402/日额度 → 长冷却到重新探测成功，不硬禁用；
  - 401/会话失效 → 单写者刷新；失败才标记需重新授权；
  - 不对同一请求做“已产生内容后”的重放。
- 多账号：每个账号独立 uid、独立 COSY 派生标识；不得共享 token 或设备标识。

### 11.3 取消

- 上游没有取消 API；`Capabilities::cancellation = false`。
- 下游断开 SSE 时，网关只能停止读取并释放连接；不得声称已取消上游计费。
- 管理台/日志不得把“连接关闭”写成“任务已取消”。

**D2-E 实测（2026-10-29，本地 Mock 网关 + 真实 Router/Provider）：** 下游丢弃 SSE body 会立即
释放会话锁与账号槽（不调用 `finish()`，不计 usage、不延长 TTL，与既有租约文档一致）；
120 s 空闲超时会终止流、走 `report_failure` + `finish()` 并释放租约；同一会话的并发请求由
per-session 锁串行化，不同会话互不阻塞；流内 `[EXCEED_QUOTA]` 正常终止并释放租约。
上游会话失效的恢复路径只在策略层验证：同一账号重置 upstream id，**不会**静默换号；
当前没有任何适配器会把上游错误映射为 `SessionBindingInvalid`（Qoder 无 upstream session）。
详见 `PROGRESS.md` §33。

## 12. Web 管理台接入设计与 M13-D2 拆分

### 12.1 用户流程

目标流程（不实现 UI，只设计）：

```text
账号管理 → 添加账号
  → Provider: Qoder
  → 区域: 国内版 / 国际版（必选，不允许默认猜测）
  → 名称（可选）
  → [开始授权]
      → 显示授权链接 + “等待浏览器授权…”
      → 用户在新标签打开链接，用官方账号登录并确认
      → 页面每 2s 轮询流程状态
      → 成功：显示账号已添加 / 昵称 / 区域 / 凭据状态
      → 失败/过期：显示受控错误 + 重新开始
```

安全要求：

- 浏览器只拿到 `authorize_url` 与流程状态，**永远不拿 token/refresh token**。
- 不要求用户粘贴 token、Cookie、AK/SK 或私钥。
- 不读取浏览器 LocalStorage；不使用旧 `codearts2api` 的凭据。
- 区域在流程创建时锁定；完成后账号的 region 不可被静默改写。
- 取消只取消本地流程，不调用上游吊销（没有该接口）。

### 12.2 管理 API 设计

| 方法 | 路径 | 作用 | 备注 |
| --- | --- | --- | --- |
| `POST` | `/admin/api/auth/qoder/start` | 创建流程，返回 `flow_id` + `authorize_url` | body: `{region, name?}`；CSRF 必需 |
| `GET` | `/admin/api/auth/qoder/{flow_id}` | 轮询状态；pending 时触发一次上游 poll | 间隔 ≥5s；不返回 secrets |
| `POST` | `/admin/api/auth/qoder/{flow_id}/cancel` | 取消本地流程 | 不吊销上游 |

复用：

- `src/admin/mod.rs:149` 起的鉴权/CSRF/Origin 中间件；
- `AuthFlowRow`（`src/storage/admin.rs:29`）：`kind="qoder"`，
  `authorize_url` 存设备授权 URL，`ticket_id` 存 nonce，
  `redirect_uri` 存空串（设备流程无回调；列为 NOT NULL，空串可接受），
  `secrets_encrypted` 存 `{verifier, nonce, machine_id, region}`；
- `AdminState::start_* / flow_status / cancel_*` 的模式
  （`src/admin/auth/flow.rs:34`、`:99`、`:108`）。

新增内部接口（不暴露给浏览器）：

- `QoderAuth::start(region, name) -> {flow_id, authorize_url}`；
- `QoderAuth::poll(flow_id) -> Pending | Completed {credential, identity} | Failed`；
- 完成后调用 `persist_qoder_login`（与
  `src/admin/account/mod.rs:18` 的 `persist_codearts_login` 同构）：
  - `external_id = qoder:<region>:<uid>`；
  - 已存在则 `CredentialService::update(revision)` CAS，否则 `save`；
  - metadata 合并 `{uid, nickname, realm, domain, user_type, organization_id,
    organization_name}`；
  - `record_auth_status(Valid)`。

### 12.3 为什么不新增表

`admin_auth_flows` 已能表达设备流程：

- `status`: `pending` / `exchanging` / `completed` / `failed` / `cancelled` /
  `expired`（沿用现有字符串）；
- `ticket_id` 可放 nonce（唯一性由流程 id 保证）；
- `redirect_uri` 空串；
- `secrets_encrypted` 放 PKCE 等短生命周期秘密。

唯一需要确认的是 `AuthFlowRow.redirect_uri` 是否允许空串（列 NOT NULL 但
无 CHECK）；若现有测试/代码假定非空，D2 可在流程层写管理台地址作为占位，
不改变 schema。**这是实现细节，不影响设计。**

### 12.4 M13-D2 拆分

| 子阶段 | 内容 | 退出条件 | 真实上游 |
| --- | --- | --- | --- |
| **D2-0 政策确认** | COSY 头族/设备标识的合规解释；是否接受 Qoder 接入；获取授权账号 | 项目所有者明确答复 | 否 |
| **D2-A COSY signer** | 自定义 Base64、AES-CBC、RSA 包裹、md5 签名、uid 派生；区域/错误/Chat 转换/SSE 基础 | 固定向量单测全绿；与参考算法交叉校验；无网络 **（2026-10-29 完成，见 `PROGRESS.md` §28）** | 否 |
| **D2-B 认证 + 管理台流程** | 设备轮询、AuthFlowRow、加密秘密、账号持久化、单写者刷新 | Mock poll server 覆盖 202/404/200/超时/重复 poll/区域隔离；真实登录 **（2026-10-29 完成，见 `PROGRESS.md` §29；CN 设备码授权/userinfo/落库已 Live Verified，刷新与 Intl 未验证）** | 是（CN） |
| **D2-C 模型 + 额度** | COSY GET、字段映射、未知语义、额度 raw | 额度与模型发现均已完成并通过真实账号验证（11 个模型）**（2026-10-29，见 `PROGRESS.md` §30、§31）** | 是 |
| **D2-D Chat + SSE + tools** | 流式、非流式聚合、多轮、工具、错误映射、gateway 回退 | Mock 信封流全部通过；真实账号完成非流式、流式、工具调用与多轮工具结果 **（2026-10-29，见 `PROGRESS.md` §31.4）** | 是 |
| **D2-E 会话/并发观察** | 下游粘性只绑账号；不伪造 upstream session；观察 429/403 | 租约生命周期 6 项场景已用本地 Mock 网关端到端验证（断连/空闲超时/同会话并发/额度/失效/重启），真实账号仅冒烟；**双账号负载均衡未做** **（2026-10-29，见 `PROGRESS.md` §33）** | 部分（Mock） |
| **D2-F 签到执行器** | 状态查询 + 活动筛选 + 窗口 + 幂等映射；claim 开关默认关闭 | 已实现并通过 Mock；真实账号 campaign 查询与一次 claim 已验证（+100 credits）**（2026-10-29，见 `PROGRESS.md` §30）** | 是（CN） |
| **D2-G 真实验收** | 端到端逐能力验收 + 文档 | `cargo fmt/clippy/test/build` 全绿；能力状态表更新 | 是 |

依赖顺序：D2-0 → D2-A → D2-B → D2-C → D2-D → D2-E；D2-F 可在 D2-B 后并行；
D2-G 最后。

### 12.5 风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| COSY 是逆向协议，随时可能变 | 聊天/模型全挂 | signer 隔离、固定向量、清晰错误分类；不把签名散落在业务代码 |
| COSY 头族涉及客户端身份 | 违反 `AGENTS.md` §2 的风险 | D2-0 明确政策；不做随机指纹/绕过；必要时放弃接入 |
| 硬编码 RSA 公钥轮换 | 签名失败 | 公钥可配置覆盖；失败归类为 `UpstreamServer`/协议错误，不误判账号 |
| 官方客户端版本漂移（1.1.64→1.1.71） | 授权参数/client_id 可能失效 | D2-B 用真实账号验证；失败不改猜参数 |
| 刷新 token 轮换语义未知 | 并发刷新烧票 | 单写者 + CAS；绝不并发刷新 |
| uid 缺失导致设备关联 | 多账号被上游关联 | 拒绝完成登录或明确告警；不随机造 uid |
| 签到副作用 | 改变账号奖励状态 | 默认只查；claim 显式授权；每日至多一次；超时记 Unknown 不重试 |
| Intl 签到不存在 | 误报成功/无限重试 | 404/405/410 → `Unsupported`；UI 显示“本区域未开放” |
| 额度字段缺失被写成 0 | 违反未知语义 | 严格 `Unknown/Partial`；`remaining/total = None` |
| 非流式聚合与上游只有流式 | 实现复杂/漏帧 | 复用聚合器；以 `finish_reason`/`[DONE]` 判定结束 |
| 单活动授权流程 | 不能同时授权 CodeArts/Qoder | 明确 UI 提示；后续再评估放宽索引 |

### 12.6 未决问题

1. **COSY 客户端身份字段**是否被接受为“协议必需”，还是视为冒充客户端？
   （D2-0 阻塞项）
2. 参考 client_id/website 在官方 1.1.71 上是否仍有效？（真实登录验证）
3. userinfo 是否稳定返回 uid？缺失时是拒绝登录还是 token 前缀兜底？
4. refresh token 是否一次性？旧 token 刷新后是否立即失效？
5. Intl 是否有活动制签到？旧接口是否可用？
6. `/api/v2/user/plan` 是否提供套餐/免费/订阅信息？字段是什么？
7. `x-model-key` 与 body `model` 不一致时上游以哪个为准？（实现上保持一致）
8. 每日计费次数的重置时区是否固定 UTC+8？
9. 每账号并发/速率限制的真实数值？
10. 非流式请求是否值得在上游支持（当前只有流式）？

### 12.7 验收与证据记录

M13-D2 完成后，`docs/CODEARTS_LIVE_VERIFICATION.md` 的模式应复制到 Qoder：

| 能力 | 验收方法 | 记录 |
| --- | --- | --- |
| authentication | 真实设备授权 + 重启后凭据可用 | 日期/区域/结果 |
| model_discovery | 真实模型列表非空、字段合理 | 数量/示例 id |
| quota | 真实额度；缺字段时保持 unknown | 数值/单位 |
| chat | 非流式聚合 + 流式 | 延迟/usage |
| tools | 非流式 + 流式 + 多轮工具结果 | finish_reason/参数 |
| session_affinity | 同一下游会话固定同一账号 | 不伪造 upstream id |
| token_refresh | 自然接近过期时刷新；不破坏有效凭据 | 单写者/CAS |
| checkin_status | 只读查询 | 活动名/窗口/状态 |
| checkin_claim | 单独显式授权后执行一次 | 结果/是否 already |
| intl | 有账号才做 | Unverified 直到完成 |

未执行的项目必须写 `Unverified`，不得用 Mock 冒充。

### 12.8 本阶段结论

- Qoder 可以按 **Model Provider** 接入，协议路径清晰：设备 PKCE 登录 →
  COSY 签名推理/模型 → Bearer 额度/签到 → 单写者刷新。
- 最大风险不是业务逻辑，而是 **COSY 私有签名 + 客户端身份合规**；
  D2 必须先做 signer 与政策确认，再碰真实账号。
- PolyGate 现有 Provider/账号/管理台/签到框架足够承载，不需要新表、
  不需要新调度器、不需要改 M12/M13-C 结构。
- 真实能力状态保持 `Experimental`；没有任何 Qoder `Live Verified`。
