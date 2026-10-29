# CodeArts 真实联调验证记录（M13-C）

> 状态：**Partial Live Verified** — 1 个 CN 账号已完成授权、基础调用、多轮、工具调用与会话粘性验证
> 日期：2026-10-29（UTC 04:26 首次真实成功；05:00 工具/多轮/会话验证）
> 结论：经 Web 管理台完成真实 CodeArts 授权后，token 交换、`caller-identity`、模型发现、
> 额度、认证探测、非流式 Chat、流式 SSE、Tool Calling（非流式与流式）、多轮工具结果回传、
> 会话粘性均成功。Session wall 恢复、Token 自然刷新、签到、Intl 仍未验证；Provider 仍标记 `experimental`。

## 1. 环境

| 项 | 值 |
| --- | --- |
| 工作目录 | `<repo>`（`main`，尚无提交） |
| 操作系统 | Windows 10.0.26300 x64 |
| 工具链 | `rustc 1.98.1`（`rust-toolchain.toml` 固定），`CARGO_HTTP_CHECK_REVOKE=false` |
| PolyGate 版本 | M1–M13-C1 + 真实登录修复，292 个测试通过（见 `PROGRESS.md` §26） |
| Provider 状态 | CodeArts `verification = experimental` |
| 真实上游请求数 | **已完成授权、模型、额度、Chat、SSE 等请求**（见 §4） |
| 签到写操作数 | **0** |
| 读取/解密的外部密钥 | **0** |

## 2. 历史阻塞记录（2026-10-29 04:10 之前）

M13-C 首次执行时没有授权账号，以下为当时的只读盘点；M13-C1 提供 Web 授权入口后，
操作者通过浏览器完成授权，阻塞已解除。

| 检查项 | 结果 | 说明 |
| --- | --- | --- |
| `<repo>/data/` | 不存在 | 没有 `config.toml`、没有 `polygate.db`、没有 `master.key` |
| 项目内配置 | 仅 `config.example.toml` | 示例中含有凭据 JSON 形状，但没有真实值 |
| 环境变量 | 未发现 `POLYGATE_*` / `CODEARTS_*` / `SNAP_*` / `STS_*` | 不以值形式输出 |
| 默认数据目录 | `data/`（相对工作目录） | `Config::load` 未找到该目录，首次启动也没有执行过 |
| CLI 命令 | `serve`、`doctor`、`config-example` | 没有账号导入或凭据设置命令（按 M13-C1 方向，账号入口为 Web 管理台） |
| 管理 HTTP API | 已实现（M13-C1） | `/admin/api/*`，但本记录形成时尚未用真实账号授权 |
| `Gateway::accounts()` | 仅代码级服务 | 需要先有 Gateway/Storage，无法从外部安全注入账号 |
| 本地 `codearts2api` 账号 | 2 条 CN 记录 | 元数据可读；密钥字段为 `protected` / `protection`，非明文 |
| 本地保护方式 | `dpapi-current-user` | 该应用不提供明文导出；本次**未解密**、未绕过 |

当时同时存在两个阻塞项：

1. **没有授权凭据进入 PolyGate**：账号必须经 `AccountService`/`CredentialService`
   写入 SQLite，并以 `SecretBox` 加密；当时没有 CLI、HTTP 或配置文件路径可完成这件事。
2. **外部账号不可直接复用**：`codearts2api` 的账号文件不含顶层
   `access_key_id` / `secret_access_key` / `security_token` / `refresh_token`，
   密钥由 DPAPI 保护。即使从元数据看账号存在，也不能作为 PolyGate 的授权凭据。

解除方式：M13-C1 的 Web 管理台（`docs/WEB_ADMIN.md`）提供浏览器授权；操作者本人在
Brave 中完成华为云登录与 CodeArts 授权，PolyGate 收到 `code` 后完成 token 交换并加密入库。

补充观察（脱敏）：两条账号记录均为 `cn` 环境，`expiration` 约为
`2026-10-29T04:00:05Z`（检查时距到期约 65 分钟）。STS 凭据本身是短时的；解除阻塞后
需要重新导入仍然有效的凭据，而不是复用旧文件。

## 3. 授权凭据的获取边界

本次遵守以下约束：

- 未要求用户在聊天、日志或报告中粘贴 Token、Cookie、AK/SK、Refresh Token 或私钥。
- 未读取、导出或解密 `codearts2api` 的 DPAPI 保护内容。
- 未为了测试而撤销、损坏或强制过期任何有效凭据。
- 未调用 `/v1/ops/*` 的 claim/confirm；真实签到属于**另行显式授权**的测试。
- 未用 Mock/fixture 结果冒充真实响应。

## 4. 能力验证矩阵

证据等级：**Live Verified**（真实成功请求）/ **Fixture Verified**（本地 mock 与脱敏
fixture）/ **Source Evidenced**（仅源码依据）/ **Unverified** / **Failed**。
只有真实请求可标 `Live Verified`；一次端到端成功也不会自动把 Provider 升级为
`verified`。

| 能力 | 计划验证内容 | 结果 | 现有最高证据 |
| --- | --- | --- | --- |
| 认证与签名 | AK/SK + `X-Security-Token` 是否被华为 APIG 接受 | **Live Verified** | Live Verified |
| 认证错误分类 | 401/403、`APIG.0301`、STS 过期 | **部分 Live**：无效 code 返回 `STS5.1805` | Live + Fixture |
| 模型发现 | `/v1/model/builtin` 字段与 `Agent-Type` | **Live Verified**（发现 GLM/Qwen/DeepSeek/Kimi 等模型） | Live Verified |
| 额度 | 单位、剩余、套餐到期日、缺字段 | **Live Verified**：`credits`，总额 15700，剩余 15136.91 | Live Verified |
| 非流式 Chat | 单轮、`finish_reason`、`usage` | **Live Verified**：`deepseek-v4.1-flash` 返回 200 | Live Verified |
| 多轮 Chat | system/developer/user/assistant/tool 历史 | **Live Verified**：assistant tool_calls + tool 结果回传，最终答案正确 | Live Verified |
| SSE | 增量文本、`reasoning_content`、`[DONE]`、EOF | **Live Verified**：收到 reasoning 与 content 增量 | Live Verified |
| SSE 取消 | 客户端断开后租约/并发槽释放 | **Unverified**（流被 curl 提前关闭，未验证上游停止） | Source Evidenced + 单元测试 |
| Tool Calling | 无副作用工具调用与参数增量 | **Live Verified**：非流式 `finish_reason=tool_calls`，流式收到参数增量；多轮 tool 结果回传成功 | Live Verified |
| 会话粘性 | 同账号同 `chat_id`、跨账号隔离、重启后持久 | **Live Verified**：同一 `x-session-id` 复用同一 upstream `chat_id`，request_count 递增 | Live Verified |
| 会话墙 | `TM.00001041` 触发与恢复（不主动压测） | **部分 Live**：测试中意外触发 3 会话上限并正确映射 429；未验证恢复 | Live + Fixture |
| 签到（只读状态） | `ELIGIBLE`/`CLAIMED`/`CONFIRMED`/`CONSUMED` | **Unverified** | Fixture Verified |
| 签到（claim/confirm） | 需独立显式授权 | **未授权，未执行** | Fixture Verified |
| Token 刷新 | refresh_token + DPoP + PKCE + CAS | **Unverified**（未等到自然过期） | Fixture Verified |
| 刷新错误恢复 | `invalid_grant` 等分类与单次消费 | **Unverified** | Fixture Verified |

`Provider.verification` 保持 `experimental`：已完成基础真实验证，但刷新、签到、Intl 尚未验证，不整体升级。

### 4.1 按能力验证状态

| 能力 | 状态 |
| --- | --- |
| authentication | **Live Verified** |
| model_discovery | **Live Verified** |
| quota | **Live Verified** |
| chat | **Live Verified** |
| streaming | **Live Verified** |
| tools | **Live Verified**（含流式参数增量与多轮结果回传） |
| multi_turn | **Live Verified** |
| session_affinity | **Live Verified**（单账号；同一客户端会话复用同一 upstream chat_id） |
| session_wall_recovery | Unverified（仅意外观察到 3 会话上限 429） |
| token_refresh | Unverified（等待自然过期） |
| checkin | Unverified |
| intl | Unverified（无账号） |

## 5. 实际响应结构（脱敏）

**授权**：浏览器回调携带 `code`；后端 token 交换与 `/v5/caller-identity` 成功，账号以
`SecretBox` 密文入库。凭据、AK/SK、STS token、refresh token、DPoP 私钥不记录。

**认证探测**：`auth_status` 返回 `valid`，`persisted=true`。

**模型发现**：返回真实模型列表，包含
`GLM-5.2`、`Qwen3-VL-235B`、`deepseek-v4.1-flash`、`deepseek-v4.1-flash-1017`、
`glm-5.2-sft-harmony`、`glm-5.3`、`kimi-k2.6-vl`、`kimi-k3` 等；统一模型条目带
`provider=codearts-cn`、`verification=experimental`。

**额度**：

```json
{
  "unit": "credits",
  "total": 15700.0,
  "remaining": 15136.91,
  "status": "known",
  "raw": {
    "end_date": "2026-11-17",
    "packages": [
      {"name": "usageBasicPackageCredit", "total": 500.0, "used": 0.0},
      {"name": "usageOnDemandPackageCredit", "total": 0.0, "used": 0.0},
      {"name": "usageBonusPackageCredit", "total": 15200.0, "used": 563.09, "expiring": 60.49}
    ]
  }
}
```

**非流式 Chat**（`deepseek-v4.1-flash`，HTTP 200，约 0.77s）：

```json
{
  "object": "chat.completion",
  "model": "deepseek-v4.1-flash",
  "choices": [{
    "message": {"role": "assistant", "content": null, "reasoning_content": "..."},
    "finish_reason": "length"
  }],
  "usage": {"prompt_tokens": 35, "completion_tokens": 16, "total_tokens": 51}
}
```

`content` 为空是因为 `max_tokens=16` 被 reasoning 消耗完，`finish_reason=length` 正确反映。

**SSE**：同一模型流式返回 `chat.completion.chunk`，先输出 `delta.reasoning_content`，随后
输出 `delta.content:"OK"`；客户端提前关闭连接，未观察到完整 `[DONE]`。

**Tool Calling（非流式）**：`deepseek-v4.1-flash` 返回：

```json
{
  "finish_reason": "tool_calls",
  "message": {
    "role": "assistant",
    "content": null,
    "tool_calls": [{
      "id": "call_...",
      "type": "function",
      "function": {"name": "get_weather", "arguments": "{\"city\": \"Beijing\"}"}
    }]
  }
}
```

**Tool Calling（流式）**：SSE 先返回 `tool_calls[0]` 的 id/name，随后增量返回
`function.arguments`，最后 `finish_reason=tool_calls`；参数拼接为合法 JSON。

**多轮工具结果回传**：将 assistant `tool_calls` 与 `role=tool` 结果回传后，模型返回
`finish_reason=stop` 和最终自然语言答案，说明上游接受该消息历史。

**会话粘性**：使用显式 `x-session-id` 的请求复用同一条 `sessions` 行：`account_id` 与
`upstream_session_id` 不变，`request_count` 递增；不同会话 ID 对应不同 upstream chat_id。

**会话墙（意外观察）**：短时间创建第 4 个上游会话时返回
`TM.00001041`（并发会话数已达上限 3），PolyGate 映射为 HTTP 429 `rate_limit_exceeded`；
未验证释放后的恢复行为，也未主动压测。

**无效 code**：token 端点返回 HTTP 400 业务码 `STS5.1805`，被映射为
`CodeArts authorization was rejected (STS5.1805)`。

## 6. 与源码依据的差异

发现并修复 1 个真实差异：多轮请求中，PolyGate 曾把内部 `ToolCall` 结构直接放入
assistant `tool_calls`，缺少 OpenAI 的 `type:"function"` 与 `function` 包装，上游返回
`inferhub.modelarts.81001.400 invalid argument`。改为 OpenAI 形状后多轮成功。

以下既有推断仍未覆盖：

- `TM.00001041` 的实际 HTTP 状态与触发计数键（未压测会话墙）。
- `chat_id` / `prompt_cache_key` 是否形成 prompt cache 亲和（单账号、未验证）。
- refresh token 单次消费与 DPoP 低 S 的真实约束（未等到自然过期）。
- 会话失效是否存在独立业务码（未触发）。
- 工具调用参数增量（未调用工具）。

## 7. 修复

真实授权最初失败，错误为
`error sending request ... <- dns error <- 不知道这样的主机 (os error 11001)`。
根因是 `AdminConfig::default()` 中的默认 STS 主机写成了
`sts.cn-north-4.huaweicloud.com`，缺少 `my`，正确值为
`sts.cn-north-4.myhuaweicloud.com`（`config.example.toml` 与参考工程均为后者）。

修复：

- `src/config/mod.rs` 改正默认 STS URL；
- 新增回归测试 `admin_defaults_use_real_huawei_endpoints`；
- 授权失败日志保留阶段与错误链（不含凭据），便于后续诊断；
- `src/providers/codearts/convert.rs` 将 assistant `tool_calls` 转为 OpenAI 形状
  （`type:"function"` + `function.name/arguments`），并加强 `tool_history_roundtrips` 断言。

修复后同一授权流程成功，工具调用与多轮通过；全量测试 292 个通过。

## 8. 解除阻塞的安全步骤

M13-C1 已提供正常入口：启动 Gateway 后打开 `http://127.0.0.1:8787/admin/`，完成首次设置，
在“账号 → 添加账号”中选择 CN/Intl 并完成 CodeArts 浏览器授权。凭据由后端直接加密入库，
浏览器不会拿到 AK/SK、STS token、refresh token 或私钥。之后按以下顺序验证：

1. **授权**：操作者本人在浏览器完成授权；不要把 Token/Cookie/AK/SK 粘贴到聊天、日志或报告。
2. **确认落库**：管理台显示账号、区域与本地认证状态；数据库中凭据应为 SecretBox 密文。
3. **启动自检**：`doctor` 校验 master key、数据库与账号存在性；确认 CodeArts provider
   已注册且模型发现不再为空。
4. **按风险顺序验证**：
   `auth_status` → `list_models` → `quota` → 单轮非流式 Chat → 多轮 → SSE（含取消）→
   Tool Calling → 会话粘性/持久化 → 刷新（优先自然到期，不破坏有效凭据）。
5. **会话墙**：只做 3 个并发会话的最小观察；不主动压测、不反复触发。
6. **签到**：单独发起授权请求，先只读查询状态；只有用户明确允许领取奖励时才调用
   claim/confirm，且不重复执行有副作用的请求。
7. 每次真实请求后写回本文档：环境、时间、脱敏请求/响应、证据等级、差异、修复与回归
   测试。真实响应导致适配器修改时，先保存最小脱敏复现，再加回归测试并跑全量套件。

## 9. 未验证清单（保持 Unverified）

已转为 Live Verified：签名/认证、模型发现、额度、非流式单轮 Chat、SSE 文本增量、
Tool Calling（非流式/流式）、多轮工具结果回传、单账号会话粘性。

仍未验证：

- 会话墙 `TM.00001041` 释放后的恢复行为（仅意外触发一次 429，未主动压测）。
- 客户端取消后上游是否停止计费/释放会话。
- Token 自然过期后的 refresh + DPoP + PKCE + CAS 流程。
- refresh token 单次消费、`invalid_grant` 等失败分类。
- 签到状态机、业务码与 claim/confirm（需单独显式授权）。
- CodeArts Intl 区域。
- 多账号隔离与负载均衡（当前仅 1 个 CN 账号）。

## 10. 下一入口

M13-C2 已完成授权、模型、额度、Chat、SSE、工具调用、多轮和单账号会话粘性的真实验证。
剩余：Token 自然刷新（等待过期）、签到（独立显式授权）、会话墙恢复观察、CodeArts Intl。
Provider 继续保持 `experimental`，按能力状态见 §4.1。
