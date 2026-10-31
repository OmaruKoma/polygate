# 部署指南

目标环境：Linux x86_64 / aarch64，单二进制 + SQLite。默认只监听 `127.0.0.1`，
公网访问必须经 TLS 反向代理。

## 1. 构建

```bash
# 本机
cargo build --release --locked
# 产物：target/release/polygate（Linux），Windows 为 polygate.exe

# 交叉编译 aarch64-musl（软路由 / ARM VPS），Windows 或 Linux 均可用 zig 当链接器
rustup target add aarch64-unknown-linux-musl
cargo install cargo-zigbuild --locked      # 需要 zig 0.13+ 在 PATH 里
cargo zigbuild --release --target aarch64-unknown-linux-musl

# 或
cargo install cross
cross build --release --locked --target aarch64-unknown-linux-musl
```

musl 产物是**静态链接**的：没有 `PT_INTERP`、没有 `DT_NEEDED`，可以直接丢到任意
glibc/musl 系统（含 OpenWrt）运行，不依赖路由器的 libc 版本。

参考体积（2026-10-29/30 实测）：Windows x86_64 release、`strip = "symbols"`、
`lto = "thin"`、`panic = "abort"`，二进制 **11,820,032 字节（约 11.3 MiB）**；
aarch64-unknown-linux-musl 为 **10,963,376 字节（约 10.5 MiB）**，gzip 后约 4.7 MiB。

`rust-toolchain.toml` 固定 1.98.1，`Cargo.toml` 的 MSRV 为 1.85。

## 2. 目录布局

```
/opt/polygate/polygate            二进制
/etc/polygate/config.toml         配置（0640，owner root:polygate）
/etc/polygate/polygate.env        环境变量（0600，含密钥）
/var/lib/polygate/polygate.db     SQLite（含 WAL/SHM）
/var/lib/polygate/master.key      凭据加密主密钥（0600，自动生成）
/var/lib/polygate/logs/           可选：由 systemd/journald 收集
```

## 3. systemd

`deploy/polygate.service` 已包含 hardened 单元（`ProtectSystem=strict`、
`ProtectHome`、`NoNewPrivileges`、`MemoryDenyWriteExecute`、`UMask=0077` 等）。

```bash
sudo useradd --system --home /var/lib/polygate --shell /usr/sbin/nologin polygate
sudo install -d -o polygate -g polygate -m 0700 /var/lib/polygate
sudo install -d -o root -g polygate -m 0750 /etc/polygate /opt/polygate
sudo install -m 0755 polygate /opt/polygate/polygate
sudo install -m 0640 -g polygate config.toml /etc/polygate/config.toml
sudo install -m 0644 deploy/polygate.service /etc/systemd/system/polygate.service
sudo systemctl daemon-reload
sudo systemctl enable --now polygate
journalctl -u polygate -f
```

`/etc/polygate/polygate.env`：

```
POLYGATE_API_KEY=pg-<32 hex>
POLYGATE_MASTER_KEY=<32 字节 hex 或 base64>
POLYGATE_LOG_LEVEL=info
```

**主密钥必须备份**：丢失后已保存的账号凭据无法解密，只能重新授权账号。

## 4. Docker（可选）

```bash
docker build -f deploy/Dockerfile -t polygate:local .
docker run -d --name polygate -p 127.0.0.1:8787:8787 \
  -e POLYGATE_API_KEY=pg-... -e POLYGATE_MASTER_KEY=... \
  -v polygate-data:/var/lib/polygate polygate:local
```

`deploy/container-config.toml` 监听 `0.0.0.0:8787`（容器内），端口映射到宿主
`127.0.0.1`，不会直接暴露公网。镜像内以非 root 用户运行。

## 5. 反向代理

Caddy：

```
gateway.example.com {
    encode zstd gzip
    reverse_proxy 127.0.0.1:8787 {
        flush_interval -1
    }
}
```

nginx：

```nginx
location / {
    proxy_pass http://127.0.0.1:8787;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;        # SSE 必须关闭缓冲
    proxy_read_timeout 600s;
    client_max_body_size 16m;
}
```

配置 `server.public_url = "https://gateway.example.com"`，并视情况设置
`server.trust_proxy = true`（仅在本机反代时启用）。管理台在 `trust_proxy` 开启时
要求 `X-Forwarded-Proto=https` 才会使用 Secure Cookie。

## 6. 配置与环境变量

| 环境变量 | 说明 |
| --- | --- |
| `POLYGATE_CONFIG` | 配置文件路径 |
| `POLYGATE_API_KEY` | 追加一个网关 API Key（与文件内的合并） |
| `POLYGATE_MASTER_KEY` | 32 字节 hex/base64 主密钥，优先于 `master.key` |
| `POLYGATE_LISTEN` | 覆盖 `server.listen` |
| `POLYGATE_PUBLIC_URL` | 覆盖 `server.public_url` |
| `POLYGATE_DATABASE_URL` | 覆盖 `database.url` |
| `POLYGATE_LOG_LEVEL` | tracing filter，如 `info,polygate=debug` |
| `POLYGATE_TRUST_PROXY` | `true/false` |
| `POLYGATE_SCHEDULER_STRATEGY` | `round_robin` / `smooth_weighted_round_robin` / `least_connections` |
| `POLYGATE_MOCK_ENABLED` | 是否注册本地 Mock Provider（生产应为 false） |

关键配置项：`server.max_body_bytes`（默认 16 MiB）、`server.request_timeout_secs`
（600）、`database.max_connections`（默认 4，上限 8）、`[scheduler]` 见
`docs/ROUTING.md`。

## 7. 数据库迁移

迁移内嵌在二进制内，启动时自动执行；`schema_version` 记录版本，`_sqlx_migrations`
记录校验和。**不要修改已发布的迁移**；升级时：

1. 停止服务。
2. 备份 `polygate.db`（以及 `-wal`/`-shm`，或使用 `sqlite3 .backup`）。
3. 启动新版本，迁移自动执行。
4. 检查 `/health` 与管理台。

当前 schema 版本：8（`0001`–`0008`）。

## 8. 备份与恢复

必须备份：

- `polygate.db`（账号、凭据密文、别名、日志）
- `master.key` 或 `POLYGATE_MASTER_KEY`（缺失则凭据不可读）

```bash
sqlite3 /var/lib/polygate/polygate.db ".backup '/backup/polygate-$(date +%F).db'"
install -m 0600 /var/lib/polygate/master.key /backup/master.key
```

恢复：停止服务 → 放回数据库与主密钥 → 启动。

## 9. 日志与观测

- 结构化日志输出到 stdout，交给 journald/Docker 收集。
- 敏感值（token、cookie、Authorization、AK/SK）在日志与错误体中自动脱敏。
- `GET /health` 返回 `{"status":"ok","version":"..."}`，可作为存活探针。
- 管理台仪表盘展示 24h 成功率、平均延迟与最近错误；详细元数据在“请求日志”。
- 请求日志保留：在 `[metrics]` 设置 `log_retention_days = 30`，网关在启动时和每 6 小时
  删除超过该天数的记录；`request_logs = false` 时不写日志，保留任务也不启动。留空表示
  不自动清理，只接受管理台「请求日志」页的手动清理。
- 需要立即清理时仍可调用管理接口（需要管理员会话与 CSRF）：

```bash
curl -X POST https://gateway.example.com/admin/api/logs/purge \
  -H 'Content-Type: application/json' -b cookies.txt -H 'X-CSRF-Token: ...' \
  -d '{"older_than_days":30}'
```

## 10. 性能基线（2026-10-29 实测）

环境：Windows 10（MECHREVO 9955HX），release 构建，Mock Provider（无真实上游），
每个请求写入一条 `request_logs` + 一条 `usage_events`，16 个 keep-alive 连接各发
100 次 `POST /v1/responses`（约 1 KB 请求体）：

| 指标 | 数值 |
| --- | --- |
| 请求数 | 1600 |
| 总耗时 | 0.852 s |
| 吞吐 | ≈ 1878 req/s |
| 平均延迟 | 7.27 ms |
| p50 | 4.13 ms |
| p95 | 21.06 ms |
| 最大 | 284.98 ms |
| 常驻内存（工作集） | 16.7 MB（压测后） |
| 私有内存 | 7.2 MB |

这些数字衡量的是**网关自身开销**，不含任何真实上游延迟，也不是 VPS 基准。真实
Provider 的端到端延迟取决于上游。请在目标机器上用相同方法复测，不要引用本表作为
线上容量承诺。

## 11. 排障

| 现象 | 排查 |
| --- | --- |
| 启动报 “gateway API key is too short” | Key 至少 16 字符，且不能用示例占位符 |
| 管理台无法首次初始化 | `admin.setup_local_only = true` 时只接受本机 loopback 访问 |
| SSE 被缓冲/不实时 | 反代关闭 `proxy_buffering`，Caddy 设置 `flush_interval -1` |
| 升级后 500 且日志提示 migration | 数据库被旧版本锁定或迁移文件被改动；用备份恢复后重新升级 |
| 账号显示已授权但不可调度 | 凭据缺失/过期、账号禁用、或额度快照 `remaining <= 0`；在账号详情刷新额度 |
| 模型 404 | 原生模型未发现（Provider 未启用/无可用账号）或别名全部目标禁用 |

## 12. OpenWrt / KWRT（aarch64 软路由）

适用：IPQ6000 / IPQ807x / MT7986 等 Cortex-A53 平台，OpenWrt 21.02+ 或基于它的 KWRT。
用 `aarch64-unknown-linux-musl` 静态产物，不需要 Entware，也不需要路由器自带 libc 匹配。

```bash
# 1. 传二进制（约 10.5 MiB，gzip 后 4.7 MiB）
scp polygate-0.2.1-aarch64-linux-musl root@ROUTER:/usr/bin/polygate
ssh root@ROUTER 'chmod +x /usr/bin/polygate && /usr/bin/polygate --version'

# 2. 配置目录
ssh root@ROUTER 'mkdir -p /etc/polygate'
scp config.example.toml root@ROUTER:/etc/polygate/config.toml
scp deploy/openwrt/polygate.env.example root@ROUTER:/etc/polygate/polygate.env
ssh root@ROUTER 'chmod 600 /etc/polygate/polygate.env'
# 编辑 polygate.env：填入 POLYGATE_API_KEY 与 POLYGATE_MASTER_KEY

# 3. 开机自启（procd）
scp deploy/openwrt/polygate.init root@ROUTER:/etc/init.d/polygate
ssh root@ROUTER 'chmod +x /etc/init.d/polygate && /etc/init.d/polygate enable && /etc/init.d/polygate start'
ssh root@ROUTER 'logread -e polygate | tail'
```

**首次初始化管理台**：`admin.setup_local_only = true` 只接受 loopback 访问，用 SSH 隧道：

```bash
ssh -L 8787:127.0.0.1:8787 root@ROUTER
# 然后本机浏览器打开 http://127.0.0.1:8787/
```

局域网直连则在 `config.toml` 把 `listen` 改成 `0.0.0.0:8787`，并保留网关 API Key 鉴权；
不要把这个端口暴露到公网。

**存储注意**：`data/` 里的 SQLite 会写 WAL。放在 `/overlay`（ext4/f2fs）没问题；
放在 UBIFS/JFFS2 的裸 flash 上建议把 `--data-dir` 指到 USB 或 `/tmp` 之外的 ext4 分区，
并降低日志保留量：

```bash
# config.toml
[database]
url = "sqlite:///mnt/usb/polygate/polygate.db?mode=rwc"
max_connections = 2
usage_flush_secs = 15
```

**资源**：IPQ6000（4×A53 @1.2 GHz）上单实例常驻约 15–20 MB，空闲 CPU 接近于 0；
同时保持 20 条 SSE 流式连接时内存增加约 1–2 MB。

**已知限制**：静态 musl 构建无法使用系统 CA 以外的自定义证书库；如路由器做了 HTTPS
透明代理/证书替换，请让网关走正常出网路径，不要在网关侧注入代理证书。

**CodeArts 授权在路由器上**：CodeArts 把授权 code 绑定在回环回调上，所以
`[admin] redirect_url` 保持默认 `http://127.0.0.1:8787/oauth/callback`，**不要**改成
`10.0.0.1`（会得到 `STS5.1805`）。浏览器回调页打不开时，复制地址栏链接，粘贴到面板的
「回调链接」框里点「完成授权」，换取在路由器上完成。详见 `docs/WEB_ADMIN.md` §7。
