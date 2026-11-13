# Doubao（豆包 / DoubaoWork）迁移记录

参考实现：<https://github.com/2951461586/Doubao2API>（Apache-2.0，Go 单文件网关）。
本文只记录**协议事实**与移植结论；没有复制对方代码。

日期：2026-11-13。验证环境：本机 Windows 豆包桌面端登录态 + OpenWrt 路由器
（`10.0.0.1`，aarch64 musl）上的 PolyGate 0.2.18 临时实例。

## 1. 应用形态

- 桌面端是 **Chromium 原生应用（Lark 内嵌内核）**，不是 Electron；
  业务包在 `app/local_webcontents/biz/biz.pak`。
- 本机安装路径：`%LOCALAPPDATA%\Doubao`（不是参考项目默认的 `DoubaoWork`）。
  该路径差异使参考项目的自动导入默认失败，必须以 `-doubao-dir` 指定
  `...\Doubao\User Data`。
- 登录态落盘：`User Data\Default\Network\Cookies`（SQLite）＋ `Local State`
  （DPAPI 保护的 AES-256-GCM 主密钥，Chrome 130+ 另带 32 字节域名绑定前缀）。

## 2. 鉴权

| 项 | 结论 |
| --- | --- |
| 会话 | Cookie：`sessionid`（或 `sessionid_ss`）、`sid_guard`、`ttwid`、`odin_tt` |
| CSRF | 头 `x-tt-passport-csrf-token` ← Cookie `passport_csrf_token`（或 `_default`） |
| 设备身份 | `device_id`、`web_id`、`fp`（§3 实测为必需） |
| 刷新 | 无 refresh token；过期后重新导入 |
| 伪装 | `Origin` / `Referer` / 浏览器 UA **均不需要**（实测可省） |

## 3. 设备身份是硬约束（本次实测的核心结论）

同一个登录态、同一份请求体，只改公参：

| 公参 | 结果 |
| --- | --- |
| 完整（`device_id` + `web_id` + `fp` + `msToken`） | `SSE_ACK`，正常回答 |
| 仅 `aid` / `device_platform` / `version_code` / 随机 `web_tab_id` | `STREAM_ERROR` `710022004 rate limited`，`extra.decision` 为 `shark_admin` 的 `verify`/`slide` |

结论：**设备身份不是可选装饰，而是风控输入**。因此 PolyGate 只保存运维从自己客户端
导入的值，绝不生成、不轮换；缺失时本地返回 `AuthInvalid` 且不发出请求
（`provider.rs` 的 `missing_identity` 与 `http.rs::open_stream`）。

同样地，`Origin`/`Referer`/浏览器 UA 三家省略仍能成功，所以实现里不带这三个头，
避免"伪装成桌面客户端"。

补充（§11）：三件套**缺一个都不行**，但入口并不比对具体值——全随机组合同样能进。
本机真实值的取法见 §11。

## 4. 请求

`POST https://www.doubao.com/chat/completion?<公参>`

公参（`http.rs::query`）：`aid=582478`、`real_aid`、`device_id`、`tea_uuid`、
`web_id`、`device_platform=web`、`language=zh`、`region=CN`、`sys_region=CN`、
`pkg_type=release_version`、`version_code=20800`、`pc_version=2.1.7`、
`chromium_version`、`client_platform=pc_client`、`runtime=web`、`runtime_version=3.5.4`、
`samantha_web=1`、`use-olympus-account=1`、`fp`、`web_tab_id`（每次随机）。

请求体（`payload.rs::build_body`）：

- `client_meta`：`local_conversation_id`、`conversation_id="0"`、
  `bot_id=7338286299411103781`。
- `messages[0].content_block`：一个 `block_type=10000` 文本块。图片块（10052）需要
  上游资源中心 `uri`，本适配器不实现。
- `option.need_deep_think` 与 `ext.use_deep_think`：0 / 1 / 3。
- `option.answer_with_suggest` **必须为 true**：置 false 上游不再返回思维链。
- `option.sse_recv_event_options.support_chunk_delta = true`。

历史处理：上游自己维护会话，但 OpenAI 客户端每轮都带全量历史；适配器把历史压平成
一段提示（`system` 在前，`Assistant:` 前缀标注助手轮次），不依赖服务端会话 id。

## 5. 响应

| 事件 | 处理 |
| --- | --- |
| `SSE_ACK` | 记 `ack_client_meta.conversation_id` 作为响应 id |
| `STREAM_CHUNK` / `STREAM_MSG_NOTIFY` | 取 `patch_op[].patch_value.content_block[]`（旧形状也支持 `content.content_block`） |
| `block_type=10040` | 第 1 个容器 → 思维链；第 2 个起 → 正文 |
| `block_type=10000` | `content.text_block.text` |
| `{"text": "..."}` | 紧凑增量，按当前阶段归入思维链或正文 |
| `block_type=10024/10025/10101` | 工具 / 搜索 / 加载提示，不映射为可见内容 |
| `STREAM_ERROR` 或任意帧的 `error_code` | 流内错误；`710022004` → `RateLimited` |

`finish_reason`：上游不给 OpenAI 式原因，有正文时记 `stop`，否则 `unknown`（不臆造）。

## 6. 模型

上游没有模型名路由，只有思考档位。对外暴露：

| id | 档位 | 上下文（文档值，上游不报） |
| --- | --- | --- |
| `doubao` | 0 | 128K |
| `doubao-think` | 1 | 128K |
| `doubao-expert` | 3 | 256K |

未知模型名回落到 `doubao`，与参考实现一致（保证任何客户端可用）。

## 7. 用量与额度

上游不返回 token 数，也没有额度接口。因此：`usage = null`、额度 `unknown`
（`capabilities.quota = false`）。**不做字符数估算**——估算值不是上游事实，
写进统计会污染用量页。

## 8. 未实现

| 能力 | 原因 |
| --- | --- |
| 图片 / 文件 | 上游只接受自家资源中心 `uri`，需要 `prepare_upload` + imageX SigV4 直传 + `CommitImageUpload` 三步；未实现，故明确返回 `Unsupported` |
| 工具调用 | 上游聊天没有 tool surface |
| 会话续接 / 取消 | 无对应接口 |
| 签到 | 未找到可自动化的签到/权益接口，且属政策敏感项，不实现 |
| 扫码登录 | 服务端在路由器上无法展示二维码轮询交互，且会把账号密码类流程引入网关 |

## 9. 实测记录（2026-11-13）

在路由器上跑 PolyGate 0.2.18 临时实例（`127.0.0.1:18799`），导入本机桌面端登录态后：

| 用例 | 结果 |
| --- | --- |
| `POST /admin/api/auth/doubao/session` | 200，账号 `doubao-e2e`，`external_id=doubao:cn:…` |
| `GET /v1/models` | 三个档位模型，`kind=doubao`，`verification=experimental` |
| `doubao` 非流式 | `{"content":"OK"}`，`finish_reason=stop`，`usage=null` |
| `doubao-think` 流式 | 分块正文正常 |
| `doubao-expert` 非流式 | 正文 + `reasoning_content` 均正确解析 |

## 10. 边界

- 协议随时可能变化；状态保持 `experimental`。
- 登录态会过期（上游返回 `STREAM_ERROR`/401），需要重新导入。
- Cookie 与设备参数等同账号凭据，只经 `SecretBox` 加密保存，日志与 API 不回传明文。
## 11. 本地一键提取设备信息（`tools/doubao-extract.js`）

豆包桌面端是 Chromium 应用，登录态与设备身份都在本机，可以脚本化取出：

```bash
node tools/doubao-extract.js          # 或双击 tools/doubao-extract.cmd
```

| 目标值 | 来源 | 说明 |
| --- | --- | --- |
| Cookie（26 项） | `Default/Network/Cookies` | Chromium v10：DPAPI 解包 `Local State` 的 `os_crypt.encrypted_key` 得 AES-256-GCM 密钥，明文前缀 `SHA256(host_key)` 去掉 |
| `device_id` | `Local State` → `aha.device.device_id` | 桌面端原生设备号；与 localStorage `tea_cache_tokens_582478` 里的 `user_unique_id` 一致 |
| `web_id` | `Default/Local Storage/leveldb` → `tea_cache_tokens_582478` | leveldb 有前缀压缩，键名可能被吃掉；脚本按「582478 → 配对 `user_unique_id` → 最新时间戳」三级回退 |
| `fp` | 同目录 localStorage 的 `s_v_web_id` | 页面自己发的 `s_v_web_id`（`verify_…`），与 Cookie 里同名的那个**不是**同一个值 |

输出 `tools/doubao-account.json`（已加入 `.gitignore`，含明文凭据，不要提交）。

实测踩到的两个坑：

- **活动档案的 Cookie 库是独占打开的**（`dwShareMode=0`，连只读都 EBUSY），所以客户端
  开着的时候读不到。脚本会明确报错让你先退出豆包，或加 `--kill` 让它自己关掉
  （`taskkill /F /IM Doubao.exe`，聊天记录在服务端，不会丢）。
- **一个账号一个档案**：换号后新账号落在 `Profile 4`，旧号留在 `Default`。脚本按
  `Network/Cookies` 的 mtime 倒序排，第一个标「最近使用」。读不到最新档案时**直接失败**，
  绝不回退到旧账号——那会把错的登录态塞给你。

校验过的部分：

- 提取出的 Cookie 与人工从客户端复制导入的那 26 项**逐字节一致**（旧账号）。
- 新账号（`Profile 4`，28 项）用脚本输出直接请求 `/chat/completion`：拿到 `SSE_ACK`，
  `inner_did` 回显 `4228…7545`（即 `aha.device.device_id`），并完整走完
  `SSE_REPLY_END`，客户端里能看到回复。

已知边界：

- 只支持 Windows DPAPI + Chromium `v10`。遇到 app-bound 加密（`v20`）直接报错退出，不猜测。
- 找不到的字段只报「缺失」，**不生成随机占位值**（参考项目会伪造，本脚本不伪造设备身份）。
- 读 Cookie 库 + DPAPI 解包是杀软眼里的高危行为，卡巴斯基会拦截甚至删除脚本，需要放行该目录。
- 桌面上只有「原生 `device_id`」；网页端那套 15 位 `device_id` 不落盘。

关于「三件套到底比对什么」：在风控放行窗口里，全真实 / 全随机 / 单值真实 六种组合
返回同一个结果，说明入口只校验**存在与形状**，不比对具体值；但随后账号被软限流
（`710022002`），无法再用成功响应二次确认。脚本因此只取本机真实值，不做随机化。

## 12. 工作模式（DeepSeek / Kimi / GLM）——2026-11-13 实测

桌面客户端的「工作」模型不是 `need_deep_think` 的 0/1/3 三档，而是同一条
`POST /chat/completion` 上的另一条泳道。抓包（Chromium 调试端口 + 页面内 hook fetch）
比对出真实字段：

| 位置 | 普通对话 | 工作模型 |
| --- | --- | --- |
| `option.need_deep_think` | `0` / `1` / `3` | `10001`（菜单 `menu_type`） |
| `option.agent_mode` | 缺省 | `1` |
| `option.conversation_init_ext` | 缺省 | `{"model_item_key": "<key>"}` |
| `option.model_config` | 缺省 | `{model_item_key, model_extra_params:{memory_profile:"300k_230", total_window_size:"300000"}, reasoning_effort:5}` |
| query | 基础公参 | 客户端还带 `pc_version` / `channel` / `samantha_web` / `use-olympus-account` / `runtime` 等，实测可省 |

本机抓到并实测可用的 `model_item_key`：

| key | 名称 | 倍率 |
| --- | --- | --- |
| `person-deepseek-v4-1-flash-261017` | DeepSeek V4.1 Flash | — |
| `person-glm-5-3-261111` | GLM 5.3 | 0.80x |
| `person-glm-5-3-flash-261111` | GLM 5.3 Flash | — |
| `person-kimi-k3-1-pro-261111` | Kimi K3 Pro | — |
| `person-kimi-k3-1-pro-highspeed-261111` | Kimi K3 Pro Highspeed | — |

（另一个档案的缓存里还有 `person-kimi-k3-261111`、`deepseek-v4-pro-ga-260813`、
`seed-lite-7b`——菜单是按账号下发，key 会随灰度变。）

实测结论：

- 只带 cookies + 三件套、按上表拼 `option`，服务端返回 `SSE_ACK` → 思维链
  （`block_type 10040`）→ `SSE_REPLY_END`，与客户端一致；不需要 `general_task_param`
  那套本地沙箱参数。
- 把 `model_item_key` 放在 `option` 顶层无效（乱写的值也照样 200 走默认），必须落在
  `conversation_init_ext` / `model_config` 里。
- 非法 key 不报错，回落默认模型——所以网关侧的模型名单必须按实测 key 维护，不能靠校验。
- 工作泳道的模型自我认知统一是「豆包」（服务端 persona），不能靠问「你是谁」区分后端。

PolyGate 侧：`region::WORK_MODELS` 维护上表，`payload::build_body` 在命中工作模型时
自动补 `agent_mode` / `conversation_init_ext` / `model_config`，`/v1/models` 同时列出
三档对话模型和这五个工作模型。

## 13. 额度探测与「是不是真回落」——2026-11-13 二次实测

### 13.1 订阅额度接口

`POST https://www.doubao.com/alice/commerce/sale/subscription/quota/summary/`，
body `{"product_line":"doubao"}`，只带 cookies + 公参即可，**不需要** `a_bogus`
（同域其它 `/alice/*` 接口都要签名）。

免费账号的实况：

```json
{"data":{"current_subscription":{},"enterprise_package_entitlement_groups":[],
 "enterprise_subscription":{"sku_key":""},
 "enterprise_window_limit_section":{"entitlement_count":0,"usage_exhausted":false,"window_limit_groups":[]},
 "member_info":{"hasActiveSubscription":false,"hasEnterpriseSubscription":false,"usr_type":0},
 "switch_data":{"show_consumption":false},"window_limit_section":{}},"code":0}
```

有额度时数值落在 `window_limit_section.window_limit_groups[0].{total,remaining}`。
`provider::quota_with_context` → `/v1/models` 之外的 `POST /admin/api/accounts/{id}/quota`
已接上：没有数值就报 `unknown`（`remaining = null`）并在 `error` 里写明原因，绝不写 0。

### 13.2 为什么不能靠响应判断后端模型

- 响应 `ext` 里的 `model_id` / `model_type` / `llm_model_type` 恒为 `"38"`，它是
  agent 级编号，和菜单里选的第三方模型无关。
- `mode_downgrade_reason` 在选中 DeepSeek 时是 `"success"`，但用**伪造的 key**
  请求同样是 `"success"`（且照样出思维链），所以它也不指示回落。
- 普通对话泳道与工作泳道的可分差异只有：`agent_name`（`豆包` vs `Agent-GeneralTask`）、
  有无 `mode_downgrade_reason`、有无 `block_type 10040` 思维链。
- 工作泳道（DeepSeek / GLM / Kimi）的自我认知一律是「豆包」，**官方客户端选中
  DeepSeek V4.1 Flash 后问「你是谁」同样回答「我是豆包，你在豆包办公模式里的 AI 助手…」**
  （2026-11-13 20:49 CDP 抓包原文）。所以「回答说是豆包」不是网关回落，是上游 persona。

结论：能否用上第三方模型只能以账号菜单是否下发这些 key 为准；网关不做二次校验，
名单按 `region::WORK_MODELS` 手工维护。
