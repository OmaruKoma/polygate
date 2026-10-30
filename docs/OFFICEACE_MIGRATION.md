# OfficeAce（华为 AgentArts 办公智能体）接入说明

参考实现：`RobbsLuo/Coding2API`（MIT）的 `src/provider/officeace/*` 与
`TECHNICAL.md` §5.6。协议事实来自该项目的实测记录（2026-10-21 ~ 26），
**只吸收协议结论，未复制代码**。PolyGate 侧的实现是独立 Rust。

## 1. 一句话结构

OfficeAce 的聊天是**标准 OpenAI 协议**，比 CodeArts/Qoder 简单；复杂点全在别处：

| 面 | 端点 | 认证/签名 | 凭据寿命 |
| --- | --- | --- | --- |
| 聊天 + 模型目录 | `{gateway}/v2/chat/completions`、`/v2/models` | `Authorization: Basic <app_key:app_secret>` | **不过期** |
| 积分 / 签到 | `{gateway}/v1/subscription*` | `V11-HMAC-SHA256`（临时 AK/SK） | 约 2 小时 |
| 登录辅助 | `{cloud}/v1/claw/*` | `SDK-HMAC-SHA256`（临时 AK/SK） | 同上 |

模型目录 `/v2/models` 只回 `id`/`name`，不带上限；PolyGate 用
`officeace2api`（MIT）`KNOWN_META` 表做 fallback（1M/256K/198K/128K → 数字），
上游有值时不覆盖。该参考项目的 `modelProbe` 只用 `max_tokens:1` 探活，不探测 context。

`{gateway}` 每个账号不同（`client-permission-validate` 下发），
`{cloud}` 固定 `https://agentarts.cn-southwest-2.myhuaweicloud.com`。

把两套凭据混用会得到 `APIG.0303 app not found`：临时 AK/SK 不是 Basic 凭据。

## 2. 两套签名

V11 与 V4 只在三处不同（其余逐字节相同，canonical 原语复用
`src/providers/codearts/signer.rs`）：

1. `StringToSign` 多一行 `YYYYMMDD/{region}/apic`；
2. 签名密钥要派生：`HMAC(HMAC(AK, SK), scope + 0x01)[:32]` 后**转 hex 文本**；
3. **不发送** `X-Sdk-Content-Sha256`（多一个头就会改变 SignedHeaders）。

`CanonicalURI` 每段 percent-encode 且末尾补 `/`；query 必须参与签名；
`X-Project-ID` 有则计入 SignedHeaders。

`signer.rs` 用**参考实现产出的固定向量**做逐字节比对测试（同一输入下
`Authorization` 完全一致），不是自己算自己。

## 3. 积分与签到（与 CodeArts 的差别）

- **积分就是主账**：单位 `officeace_points`，没有并行的 token 池。额度直接
  取 `GET /v1/subscription` 的 `skus[].quotas[]`（`sku_attr_code ==
  officeace_points`）加 `bonus_skus[]`，取「总额 − 已用」，过期条目不计入。
  任何数字缺失都保持 `null`，不写 0。
- **签到是写操作**：`POST /v1/subscription/bonus/claim`，body 必须是空 JSON
  对象 `{}`（裸 POST 回 500 `OfficeAce.11020001`），该字节串同时参与 V11 签名。
  上游对当日重复领取幂等。
- **收益按剩余量增量算**：`points` 是活动累计发放量（桌面端签到、活动页手动领
  都计入），拿它当本次收获会虚高；签到前后各取一次 `bonus_skus` 剩余量做差。
- 参考实现实测：新用户一次性 4000 分，每日签到 1000 分，各自独立到期。
  实际数值以账号与上游为准，PolyGate 不写死。
- 只有聊天凭据（无临时 AK/SK）的账号：签到返回 `Unsupported` 并说明要重新授权；
  额度显示「未知」而不是 0。

**开关**：领取会改变账号状态，默认关闭，必须在 provider 里显式打开：

```toml
[providers.options]
"checkin.claim_enabled" = "true"
```

未打开时手动/自动签到都**不会**发写请求，结果是「已禁用，请设置 …」。

## 4. 登录：PKCE + DPoP + 云端轮询

```
① POST {cloud}/v1/claw/auth/state            → state（必须云端签发）
② 本地生成 PKCE + P-256 DPoP 密钥
③ 浏览器登录华为云并授权（回调由云端承接，不监听本地端口）
④ GET  {cloud}/v1/claw/auth/code?state=…     → 202 等待 / 200 拿到 code
⑤ POST {sts}/v1/oauth2/tokens                → 临时 AK/SK + project_id
⑥ GET  {cloud}/v1/claw/client-permission-validate
                                              → app key/secret + 每账号网关地址
```

- `client_id = pdp5_for_agentarts`，与 CodeArts 的 `codearts-agent` **不通用**
  （refresh_token 绑定 client_id，混用回 `STS5.1806`）。
- DPoP 私钥与 refresh_token 一起加密入库：STS 把票绑定在签发时的公钥上。
- 未开通的账号：第 ⑥ 步 200 但 `subscription: null`，管理台直接提示
  「华为云账号 X 还没有开通 OfficeAce」并给出下一步，而不是笼统的登录失败。
- **添加第二个账号**：勾选「先退出当前华为云会话」。不勾时浏览器里的会话会被
  直接复用，用户看不到登录页，容易把新账号记成旧账号。

管理台入口：账号管理 → OfficeAce → 添加账号。流程走「单槽」授权（与其他
provider 共用），同一时间只允许一条。

## 5. 已实现能力

| 能力 | 状态 | 说明 |
| --- | --- | --- |
| 授权探测 | MockVerified | `GET /v2/models`；401/APIG.1009 → Invalid |
| 模型发现 | MockVerified | `/v2/models` 全部条目（**不主动探活**，见 §6） |
| Chat（非流式） | MockVerified | 上游只走 SSE，网关本地聚合 |
| Chat（SSE） | MockVerified | `reasoning_content` → `ReasoningDelta`；伪 200 错误帧 → `Error` 事件 |
| 工具调用 | MockVerified | `tools`/`tool_choice` 透传；`tool_calls` 分片按 index 合并 |
| 额度 | MockVerified | 积分主账；无控制面凭据时 `unknown` 且数值为 null |
| 签到 | MockVerified | 默认关闭，需 `checkin.claim_enabled` |
| 凭据续期 | MockVerified | 一次性 refresh_token + DPoP，CAS 回写，单账号串行 |
| 原生会话 / 取消 / Agent Task | Unsupported | 上游无对应接口 |

**没有任何真实 OfficeAce 账号参与验证**：全部状态止于 MockVerified，
provider 的 `verification` 写 `experimental`。

## 6. `max_tokens` 改写

上游把 `max_tokens` 当「推理 + 正文」总预算，可用模型里多数是推理模型；
给小了正文直接为空。因此：

- `< 16` 原样透传（精确探针请求不许放大）；
- `>= 16` 追加 1024，再夹到模型族上限（`glm-5*` 131072、`qwen3*` 65536、
  其余 393216）；
- `max_completion_tokens` 归一并只发 `max_tokens`。

## 7. 错误分类要点

业务码先于 HTTP 状态判断：

| 上游码 | 归属 | PolyGate `ErrorKind` |
| --- | --- | --- |
| `ModelArts.81004` / `81009` | 这个号没这个模型 | `ModelUnavailable` |
| `81111` / `81112` / `81114` | 该模型 TPM 限流 | `RateLimited` |
| `81113` / `81001` | 请求体过大 / 参数非法 | `BadRequest` |
| `APIG.1009` / `APIG.0303` | app key 坏了 | `AuthInvalid` |
| `14018` / `insufficient balance` | 积分耗尽 | `QuotaExhausted` |

81004/81009 走 403/404，但按状态码判会把整条凭据打死——它们只说明
「这个号没有这个模型」。限流还可能以 **HTTP 200 + error 帧**出现，甚至首帧
正常、第二帧才报错，所以流解析不把 2xx 当成功。

## 8. 配置

```toml
[[providers]]
kind = "officeace"
region = "cn"                    # 唯一部署；intl 会在启动时报配置错误
enabled = true
name = "OfficeAce CN"
verification = "experimental"

  [providers.options]
  "checkin.claim_enabled" = "true"   # 领取会改变账号状态，默认 false
  # "cloud_url" = "https://agentarts.cn-southwest-2.myhuaweicloud.com"
  # "sts_url"   = "https://sts.cn-north-4.myhuaweicloud.com"
```

模型目录只在**启动时**发现：授权成功或换号后需要重启网关，才会进入
`/v1/models`。

## 9. 已知限制

- **不主动探活模型**：目录里挂着该账号用不了的模型（实测 34 个里 11 个可用），
  探活等于真发推理、消耗积分。按要求失败的请求会标记该模型不可用。
- **不展示费率**：参考实现会拉 `/v1/model-token-rates` 的 `cost_coefficient`；
  PolyGate 的模型结构目前没有倍率字段，先不展示。
- **不做积分→token 估算**：`cost_coefficient` 与 `tokens_per_point` 的叠乘关系
  官方未说明，不猜。
- **未做单账号并发上限**：参考实现按「每号约 60s 3 个会话」节流；PolyGate 的
  调度器按账号健康度轮换，未加 per-account pacer。真实压测后再补。
- **签到封账**：PolyGate 的签到服务按业务日期（本地时区）记账；上游的
  daily 活动以北京时间 0 点换新，两者在当前时区配置（Asia/Shanghai）下一致。
