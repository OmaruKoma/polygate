# PolyGate

![Rust](https://img.shields.io/badge/rust-1.85%2B-000?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-5bc4b5)
![Version](https://img.shields.io/badge/version-0.1.0-5bc4b5)
![Tests](https://img.shields.io/badge/tests-646%20passing-59bd91)

多 Provider 账号池 + Coding Agent API 网关。用一套本地凭据池，向 Claude Code、Codex、
Cursor、Cline、Continue 等客户端提供 **OpenAI Chat Completions**、**OpenAI Responses**
与 **Anthropic Messages** 三种兼容接口。

纯 Rust 单二进制（无 Node、无 Python 运行时），默认只监听 `127.0.0.1`，内置 Web 管理台、
SQLite 持久化、账号调度、会话粘性、模型别名与请求日志。凭据只以 AES-256-GCM 加密落盘。

选纯 Rust 的目的就是**能在低内存设备上长期跑**：软路由、路由器、NAS、1 GB 内存的小 VPS。
没有 JIT、没有 V8 堆、没有进程外运行时，常驻只有十几 MB（实测见下文[运行时占用](#运行时占用)）。

---

## 特性

**协议层**

- `POST /v1/chat/completions` — JSON 与 SSE，支持 tools / tool_choice、`reasoning_content`、`stream_options.include_usage`
- `POST /v1/responses` — 文本、多轮、function call，完整 SSE 事件生命周期（`response.*`）
- `POST /v1/messages` — Anthropic Messages：system / messages / tool_use / tool_result / SSE
- `GET /v1/models`、`GET /health`；三种接口统一 OpenAI / Anthropic 风格的错误体

**账号池与调度**

- 多账号、按 `(Provider, Region)` 隔离（CN / Intl 不串号）
- 三种策略：轮询、平滑加权轮询、最少连接
- 额度感知（只排除**已知**耗尽账号，未知额度保持 `unknown`）、失败冷却、并发许可
- 会话粘性（`x-session-id` / 首条用户消息哈希），租约结束自动归还槽位
- 仅在可见输出之前做故障切换；流式中途失败以 in-stream error 帧结束，绝不静默重放

**模型注册表**

- 别名 → 多目标（provider + upstream model），支持优先级、权重、能力覆盖
- 能力校验（tools / vision / context），`provider::model` 消除歧义，不遮蔽原生模型 ID

**运维**

- Web 管理台：账号授权、模型/别名、请求日志、额度与签到、设置
- 请求日志只记录元数据（Provider、账号内部 ID、模型、状态、延迟、token 数）
- SQLite 迁移内嵌于二进制，启动自动执行；systemd 加固单元与多阶段 Dockerfile
- 深色控制台设计系统（细边框、小圆角、青绿强调色），适配光色主题与移动端

---

## 支持的 Provider

| Provider | 认证方式 | 状态 |
| --- | --- | --- |
| CodeArts | 浏览器 OAuth + 会话续期 | **LiveVerified**（CN 单账号） |
| Qoder | OAuth + COSY 网关 | **LiveVerified**（CN 单账号） |
| WorkBuddy | 设备码 / 登录态 | MockVerified（无真实账号） |
| TRAE | 登录态 | MockVerified（无真实账号） |
| Loomy（讯飞） | 导入桌面端会话（14 天） | **LiveVerified**（CN 单账号，2026-10-30） |
| Mock | — | 仅用于测试与本地演示 |

> **MockVerified ≠ LiveVerified**：WorkBuddy / TRAE 的协议、续期、额度与签到只经过本地
> Mock 服务器验证，没有向真实上游发过请求。Loomy 的模型发现、Chat/SSE 与额度已用真实
> 账号实测，但会话续期、工具调用与图片模型未验证。逐项状态见 [`PROGRESS.md`](PROGRESS.md)。
>
> 明确不支持、也不会加入：CodeBuddy、ZCode、OpenCode Zen。

---

## 快速开始

```bash
# 1. 构建（工具链固定 Rust 1.98.1，MSRV 1.85）
cargo build --release

# 2. 生成配置
./target/release/polygate config-example > data/config.toml

# 3. 设置密钥：网关 API Key ≥ 16 位；主密钥丢失后已存凭据不可恢复
export POLYGATE_API_KEY="pg-$(openssl rand -hex 16)"
export POLYGATE_MASTER_KEY="$(openssl rand -hex 32)"

# 4. 自检并启动
./target/release/polygate --config data/config.toml --data-dir data doctor
./target/release/polygate --config data/config.toml --data-dir data serve
```

管理台：<http://127.0.0.1:8787/> — 首次访问设置管理员密码，然后在「账号管理」完成授权，
「模型管理」确认模型目录与别名。

第一次调用：

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $POLYGATE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "messages": [{"role": "user", "content": "用一句话解释 Rc<RefCell<T>>"}]
  }'
```

流式与工具调用：

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Authorization: Bearer $POLYGATE_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "coding",
    "stream": true,
    "messages": [{"role": "user", "content": "列一下当前目录"}],
    "tools": [{"type": "function", "function": {"name": "ls", "parameters": {"type": "object"}}}]
  }'
```

客户端接入（OpenAI 兼容）：把 `base_url` 指向 `http://127.0.0.1:8787/v1`，API Key 填
`POLYGATE_API_KEY` 的值，模型名填别名。Anthropic 客户端指向 `http://127.0.0.1:8787`，
模型名同样支持别名。

---

## 架构

```
                   ┌──────────────────────────────────────────┐
  OpenAI / Anthropic│  api/  chat.completions · responses · messages
  兼容客户端 ───────▶│        │  WireRequest → 统一内部 ChatRequest
                   │        ▼
                   │  model_service  别名解析 / 能力校验 / 目标排序
                   │        ▼
                   │  session_service 会话粘性 + 租约；scheduler 选号/冷却/额度
                   │        ▼
                   │  providers/  codearts · qoder · workbuddy · trae · mock
                   └────────┬─────────────────────────────────┘
                            ▼                    storage/  SQLite(WAL) + 迁移
                     上游 Provider 账号池        security/ SecretBox(AES-256-GCM)
```

依赖方向单向：`providers` 不依赖 `scheduler` / `storage`；`api` 不直接接触具体 Provider；
`storage` 不依赖 `api` / `providers`。详见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)。

---

## 实测性能

`cargo build --release`（LTO thin、strip、panic=abort），16 条 keep-alive 连接、
1600 次 Mock 上游请求（同机、Windows）：

| 指标 | 数值 |
| --- | --- |
| 总耗时 | 0.852 s |
| 吞吐 | ≈ 1878 req/s |
| 平均 / p50 / p95 | 7.27 ms / 4.13 ms / 21.06 ms |
| 最长 | 284.98 ms（冷启动首请求） |
| 常驻内存 | 16.7 MB working set / 7.2 MB private |
| 二进制 | 11.8 MB |

数字来自本机 Mock 上游，不含真实上游网络延迟。

## 运行时占用

单实例、release 构建、连接内嵌 SQLite 的实测（Windows 10，同机 Mock 上游）：

| 场景 | Working Set | Private Commit |
| --- | --- | --- |
| 刚启动、空闲 | 14.6 MB | 4.6 MB |
| 800 次 `/health`（8 条长连接） | 14.8 MB | 5.1 MB |
| 再 160 次 chat（含 40 条 SSE 流） | 15.5 MB | 5.1 MB |
| 1600 次请求基准（16 连接） | 16.7 MB | 7.2 MB |

内存不随请求量单调增长：请求路径上的分配都是短命的，真正会涨的是磁盘上的 SQLite 文件；
调度器与会话租约是有界内存结构。并发流式连接数是唯一的上限来源，每条约几十 KB。
所以 1 GB 内存的路由器/NAS 上跑一个完整网关 + 管理台没有压力，
Linux 下 RSS 通常还更低（10–20 MB）。

`cargo build` 的 debug 构建会到 60 MB 上下（未优化 + 大粒度栈提交），部署请一律用 `--release`。

---

## 配置与部署

- 配置：`data/config.toml`（或 `POLYGATE_CONFIG`），完整示例见 [`config.example.toml`](config.example.toml)
- 环境变量：`POLYGATE_API_KEY`、`POLYGATE_MASTER_KEY`、`POLYGATE_LISTEN`、
  `POLYGATE_DATABASE_URL`、`POLYGATE_LOG_LEVEL`、`POLYGATE_TRUST_PROXY`、
  `POLYGATE_SCHEDULER_STRATEGY`
- 密钥不入配置文件；主密钥只来自 `POLYGATE_MASTER_KEY` 或 `data/master.key`（0600，自动生成）

Linux 部署（systemd / Docker / 反代 / 备份 / 升级）见 [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)；
现成的单元文件与镜像定义在 [`deploy/`](deploy/)。

**软路由 / ARM 盒子**：提供 `aarch64-unknown-linux-musl` 静态产物（10.5 MiB，gzip 4.7 MiB），
不依赖路由器 libc，IPQ6000 / MT7986 / IPQ807x 等 Cortex-A53 平台可直接运行。procd 启动脚本与
环境变量模板在 [`deploy/openwrt/`](deploy/openwrt/)，步骤见
[`docs/DEPLOYMENT.md` §12](docs/DEPLOYMENT.md#12-openwrt--kwrtaarch64-软路由)。

## 安全默认值

- 只监听 `127.0.0.1`；公网部署必须置于 TLS 反向代理之后
- 管理面与数据面凭据分离；管理台用 HttpOnly Cookie + CSRF + Origin 校验
- 凭据只经 `SecretBox` 加解密；日志、错误、管理 API 不回显 token / cookie / Authorization
- 请求日志不含提示词、消息内容、工具参数
- 未知额度保持 `unknown`（`null`），不当成 0 或无限
- 默认以 `polygate/<version>` 标识自身；不生成设备指纹，不实现验证码破解、批量注册、封禁换号续跑

## 文档

| 文档 | 内容 |
| --- | --- |
| [`docs/API.md`](docs/API.md) | 三种 API 的请求 / 响应 / SSE / 错误格式与示例 |
| [`docs/ROUTING.md`](docs/ROUTING.md) | 模型别名、能力校验、账号调度与故障切换规则 |
| [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) | Linux 构建、systemd、Docker、反代、备份、升级 |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 模块职责、依赖方向与技术决策 |
| [`docs/PROVIDERS.md`](docs/PROVIDERS.md) | Provider 能力矩阵与协议差异 |
| [`docs/LOOMY_MIGRATION.md`](docs/LOOMY_MIGRATION.md) | Loomy 接入：会话导入、协议头、能力与限制 |
| [`docs/WEB_ADMIN.md`](docs/WEB_ADMIN.md) | 管理台使用说明 |
| [`PROGRESS.md`](PROGRESS.md) | 里程碑、实测结果、未完成项与已知问题 |
| [`AGENTS.md`](AGENTS.md) | 开发规范与安全红线 |

## 开发

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

测试不依赖真实网络与真实凭据，Mock 上游全部确定性。

## 参考项目

本项目在协议事实与设计结论上参考了这两个 MIT 项目，**没有复制它们的代码**：

| 项目 | 参考的部分 |
| --- | --- |
| [RobbsLuo/Coding2API](https://github.com/RobbsLuo/Coding2API) | 各 Provider 的协议事实：CodeArts 会话槽位与 `TM.00001041` 并发错误、签到端点与额度字段、WorkBuddy / TRAE 的请求头族与事件格式 |
| [chengzhi-c/dsh-wtq-pool](https://github.com/chengzhi-c/dsh-wtq-pool) | 账号池设计：槽位持久化、延迟开放重试、额度分组；Web UI 的视觉语言（深灰分层、青绿强调、细边框、胶囊按钮） |

审计过程与逐条结论见 [`docs/RESEARCH.md`](docs/RESEARCH.md)、[`docs/CHECKIN_RESEARCH.md`](docs/CHECKIN_RESEARCH.md)、
[`docs/CODEARTS_SESSION.md`](docs/CODEARTS_SESSION.md)；与本机自研的 `codearts2api` 的关系见 [`PROGRESS.md`](PROGRESS.md)。
路由、调度、存储、三种 wire API 与前端均为独立实现。

## 许可证

[MIT](LICENSE)。依赖均为 MIT / Apache-2.0 / BSD / Zlib / ISC 等宽松许可（含多许可选项）。
上述参考项目均为 MIT（Copyright 2026 Hongsheng Luo、chengzhi-c）；本项目只吸收协议事实与
设计结论，未包含其代码，因此不附带其版权声明。
