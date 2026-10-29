# AGENTS.md — PolyGate 开发规范

纯 Rust 多 Provider Coding Agent Gateway + 账号池。服务端目标 Linux，默认只监听 `127.0.0.1`。
进度与阶段边界以 `PROGRESS.md` 为准，架构以 `docs/ARCHITECTURE.md` 为准。

## 1. 许可证与来源

- 项目采用 **MIT**（`LICENSE`，2026-10-29 生成）。依赖许可证已清点：全部为 MIT / Apache-2.0 / BSD / Zlib / ISC / Unicode-3.0 等宽松许可，含 LGPL 的 `r-efi` 为三选一许可，取 MIT/Apache-2.0 即可。新增依赖仍须逐个核对。
- 禁止复制 AGPL-3.0、GPL-3.0 或第三方无 LICENSE 项目的受保护代码；只吸收协议事实与设计结论，独立实现。
- 本地 `codearts2api` / `codearttray` 为**操作者本人作品**（2026-10-29 确认），复用其代码不受上述第三方限制；复制进本项目时保留原版权声明，并在 `LICENSE` / `NOTICE` 中如实标注来源。其依赖的第三方库仍需单独核对许可证。
- 新增依赖前确认其许可证与本项目兼容，并在 `Cargo.lock` 中锁定。

## 2. 安全与合规红线

- 不实现：验证码破解、批量注册、设备指纹伪造、客户端身份冒充、封禁后自动换号续跑、
  额度/风控绕过、未授权的凭据共享。
- 签到/权益领取（`/v1/ops/claim`、`/v1/ops/confirm`）默认不实现，除非上游明确允许自动化。
- 凭据只经 `security::SecretBox` 加解密；日志、错误、管理 API 不得输出 token/cookie/Authorization。
- 未知额度必须保持 `unknown`（数值为 `null`），不得写成 0 或无限。
- 只有真实账号端到端验证过的 Provider 能力才能标 `verified`；不得用 Mock 结果替代。

## 3. 阶段纪律

- 迁移顺序固定：Rust Core → CodeArts 账号池 → 会话粘性 → API 兼容 → CodeArts Provider → Web UI → 其他 Provider。
- 每次只执行当前里程碑（M1–M9 见 `PROGRESS.md` §9），不提前实现后续里程碑。
- 不接入真实上游、不声明实测结果，除非任务明确要求并具备凭据。
- 复用现有脚手架，不删除已有功能，不修改与当前任务无关的模块。

## 4. 代码结构

- 单 crate，库入口 `src/lib.rs`，二进制入口 `src/main.rs`。
- 依赖方向单向：`providers` 不依赖 `scheduler`/`storage`；`api` 不直接访问具体 Provider；
  `storage` 不依赖 `api`/`providers`；`security` 为纯工具模块。
- 模块只在有实际内容时声明；空目录保留为后续里程碑占位。
- 禁止 `unsafe`（`#![forbid(unsafe_code)]`）。
- 错误统一走 `error::GatewayError` / `ErrorKind`，不向调用方泄漏上游原始敏感信息。

## 5. 构建与验证

```bash
cargo build          # 必须成功
cargo test           # 必须全绿
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- 工具链由 `rust-toolchain.toml` 固定（1.98.1）；`Cargo.toml` 的 MSRV 为 1.85。
- 测试不得依赖真实网络或真实凭据；Mock 必须是确定性的。
- 未运行的测试、未验证的能力必须在 `PROGRESS.md` 中如实标注。

## 6. 文档更新

- 每个里程碑完成后更新 `PROGRESS.md`：实际修改、实际执行的命令与输出、剩余问题。
- 协议/能力结论变化时同步 `docs/PROVIDERS.md`；架构决策变化时同步 `docs/ARCHITECTURE.md`。
- 文档用中文；代码标识符与注释用英文。