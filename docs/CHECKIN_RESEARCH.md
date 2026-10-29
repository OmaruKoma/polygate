# 签到机制专项审计（M10）

> **2026-10-29：CodeBuddy 已从本项目范围移除。** 本文中 CodeBuddy/`codebuddy_intl`
> 相关内容仅作历史协议研究，不构成实现计划；代码与 UI 中不得再注册该 Provider。


> 审计对象：Coding2API（本地 `.refs/RobbsLuo_Coding2API`）为主，dsh-wtq-pool、
> Johnsheng1/codearts2api、本地 `codearts2api`、zcode2api 为辅。
>
> 本文只做源码研究，不实现签到请求、不修改 Schema、不开发 Web UI。
> 所有“实测”均为参考仓库源码注释中的作者观测，本机未用真实账号复核。

## 0. 结论摘要

1. Coding2API 有 **5 个可签到渠道**：CodeArts、Qoder、CodeBuddy CN、TRAE、OfficeAce。
2. CodeBuddy Intl **显式不支持**：`src/provider/codebuddy/intl.py:83` 把
   `checkin/checkin_scope/checkin_status` 全部设为 `None`；上游
   `/billing/meter/checkin-activity-status` 回 `active:false` 且活动字段全空。
3. Kilo、Zen **没有签到**；ZCode 参考仓库也没有签到，且其验证码求解明确排除。
4. 签到由两层组成：**任务层** `CheckinTask`（作用域去重、当日封账、时点抖动）
   + **Provider 层** 各渠道自己的状态机/协议。
5. 一键签到 = `POST /api/credentials/batch` 的 `action=checkin`；批量上限 200，
   在途并发 4，逐条隔离失败。自动签到 = `TaskRunner` 的 `checkin` 循环。
6. 签到结果**不落库**：`_done_scopes`、`_fired_slots` 都在进程内存；
   `schema.sql:9` 明确不建 `checkins` 表，靠上游幂等 + 进程内封账。
7. 自动签到成功后**不会立即刷新额度**；手动签到/批量签到会
   `schedule_probe(credential_id)` 异步探测。自动路径的 `CheckinTask` 在
   `tasks/runner.py:638` 构造时没有传 `on_success`。
8. 时点窗口是 `[时点-抖动, 时点]`，**不补签**：服务在窗口内没醒就错过当天。
   dsh 的实现相反，`dueCheckinSlot` 会补做当天已过但未消费的小时。
9. 合规风险集中在：CodeArts 的 claim/confirm、OfficeAce 的 bonus claim 属于
   “权益领取”；TRAE 每次尝试生成新设备号；Qoder 活动 claim 可能涉及
   “同一自然人已领”判定。迁移时默认关闭，逐渠道确认上游是否允许自动化。

## 1. 审计方法与证据

- 逐个读取 `src/tasks/checkin.py`、`src/tasks/runner.py`、`src/api/admin_credentials.py`、
  `src/db/schema.sql`、各 `src/provider/*/client.py` 与 `checkin.py`。
- 单测证据：`tests/tasks/test_checkin.py`（221 行）、
  `tests/tasks/test_checkin_schedule.py`（247 行）、
  `tests/provider/codearts/test_checkin.py`（264 行）、
  `tests/provider/qoder/test_checkin.py`（384 行）、
  `tests/provider/trae/test_checkin.py`（141 行）、
  `tests/provider/codebuddy/test_checkin.py`（196 行）、
  `tests/provider/officeace/test_provider.py`（251 行，含签到）。
- 单测均使用 `httpx.MockTransport` 或假仓储，证明的是**实现分支**，不是上游当前行为。
- 前端证据：`web/src/pages/CredentialsPage.tsx`、`web/src/api/client.ts`。

### 1.1 审计项对照

| 要求项 | 本文位置 | 关键证据 |
| --- | --- | --- |
| 1 单账号签到方法 | §3、§4 | `tasks/checkin.py:154`；各 `provider/*/client.py` |
| 2 批量一键签到入口 | §5 | `api/admin_credentials.py:501`；`CredentialsPage.tsx:177` |
| 3 自动定时签到 | §6 | `tasks/runner.py:199/327/638`；`tasks/checkin.py:82` |
| 4 签到状态查询 | §7 | `api/admin_credentials.py:589`；各 `checkin_status` |
| 5 签到后额度刷新 | §8 | `admin_credentials.py:210`；`tasks/runner.py:638` 无 `on_success` |
| 6 失败处理 | §9.1/§9.2 | `tasks/checkin.py:200`；`admin_credentials.py:188` |
| 7 过期凭据处理 | §9.3 | `RefreshTask` 独占刷新；签到只记失败 |
| 8 各 Provider 差异 | §4、§13 | 各渠道状态机 |
| 9 定时任务实现 | §6、§10.1 | `asyncio` 循环；`due()`/抖动 |
| 10 签到结果存储 | §10.2 | 内存 `_done_scopes`；`schema.sql:9` 不建表 |

## 2. 总体架构与调用链

### 2.1 任务层

```
TaskRunner.start()
  └─ _loop("checkin", _sync_checkin, _checkin_interval)
       └─ 每 60s（配了时点）或 600s（未配）醒来
            └─ _sync_checkin()
                 ├─ manual=False：CheckinTask.due() 不满足 → 返回 None（no-op，不记运行态）
                 └─ CheckinTask.run_once()
                      └─ 逐条 candidate：
                           provider.checkin(data)
```

- `src/tasks/runner.py:217-225`：启动时只立即跑额度探测与 token 预刷新；
  签到**不在启动时立即跑**，要等第一个 sleep 周期。
- `src/tasks/runner.py:199-207`：`_checkin_interval` = `60.0`（配置了时点）或
  `600.0`（留空，旧行为）。
- `src/tasks/runner.py:327-335`：`_sync_checkin` 先 `due()`，再 `run_once()`；
  `manual=True` 绕过 `due()`，但不绕开关。
- `src/tasks/runner.py:638`：`CheckinTask(credentials, providers, pacer=pacer,
  times=..., jitter_minutes=...)`，**没有 `on_success`**。
- `src/tasks/status.py:49`：任务清单中的 `checkin` 描述；`TaskStatusStore`
  只保留进程内“最近一轮真实执行”，no-op 不覆盖上次结果。

### 2.2 手工/一键入口

```
POST /api/credentials/{id}/checkin
  └─ admin_credentials.checkin_credential
       └─ checkin_credential_once
            ├─ provider.checkin(data)
            └─ 成功且非 already → schedule_probe(id)

POST /api/credentials/batch  {action:"checkin", ids:[...]}
  └─ batch_credentials
       ├─ 去重、上限 200、Semaphore(4)
       └─ checkin_credential_once 逐条隔离
```

- 单条：`src/api/admin_credentials.py:492-499`。
- 批量：`:501-587`，`BATCH_ACTIONS` 在 `:52` 含 `checkin`，`BATCH_MAX_IDS=200`
  在 `:57`，`BATCH_CONCURRENCY=4` 在 `:66`。
- 共用实现：`checkin_credential_once` 在 `:188-217`；异常被收成
  `{"ok": false, "message": "签到失败：<受控原因>"}`，不返回 500。
- 批量只对 `toggle`/`revive` 写一条汇总审计（`:72-75`、`:573-587`）；
  签到是只读语义，不写审计。
- 前端：`web/src/pages/CredentialsPage.tsx:177` 批量按钮表含
  `{ key: "checkin", action: "checkin", verb: "每日签到" }`；
  单行按钮在 `:1884-1887`；`supportsCheckin()` 在 `:120-122` 只排除
  `codebuddy_intl`。

### 2.3 Provider 层

| Provider | 入口方法 | 协议 |
| --- | --- | --- |
| CodeArts | `codearts/client.py:380` `checkin` | `/v1/ops/delivery` → `/v1/ops/claim` → `/v1/ops/confirm` |
| Qoder | `qoder/client.py:435` `checkin` | `/sash/api/v1/me/campaigns` + `/{id}/claim`；回退 `daily-check-in` |
| CodeBuddy CN | `codebuddy/client.py:814` → `codebuddy/checkin.py:164` `claim` | `/billing/meter/daily-checkin` + `checkin-activity-status` |
| TRAE | `trae/client.py:747` `checkin` | `/trae/api/v2/ug/checkin_credits/status` + `/claim` |
| OfficeAce | `officeace/client.py:290` `claim_bonus` | `POST /v1/subscription/bonus/claim` |
| CodeBuddy Intl | `intl.py:83` `checkin = None` | 不支持 |
| Kilo / Zen | 无方法 | 不支持 |

## 3. 单账号签到：`CheckinTask.run_once` 的完整语义

源码：`src/tasks/checkin.py:154-217`。

1. **遍历候选**：`self._credentials.candidates()`。
2. **跳过软暂停/硬禁用**：`if candidate.disabled or not candidate.enabled: skipped += 1`。
   这一步必须早于 `seen.add`，否则暂停凭证会占用同账号其他凭证的名额
   （`:161-166` 注释）。
3. **解密**：`credential_data(candidate.credential_id)`；`CredentialDecryptError`
   只跳过该条，整轮不失败（`:167-174`）。
4. **取 provider 与 scope**：`provider.checkin_scope(data)`；缺失 provider/scope
   则 skipped（`:175-182`）。
5. **scope 回落**：`scope = checkin_scope(data) or f"credential|{id}"`（`:183`）。
   身份未知时绝不共享 scope，最坏多签一次，上游幂等。
6. **封账检查**：`_sealed(done_key, current)`（`:149-152`、`:187`）。
   `done_key = f"{day}:{scope}"`。
7. **同轮去重**：`scope in seen` → skipped；否则 `seen.add(scope)`（`:188-192`）。
8. **节流**：`Pacer.wait_turn()`，与 quota_probe/growth 共用全局串行节流（`:193-196`）。
9. **调用**：`with credential_scope(id): provider.checkin(data)`（`:197-199`）。
10. **异常隔离**：`except Exception` → `failed += 1`，只记 warning，继续下一条
    （`:200-204`）。
11. **成功封账**：`self._done_scopes[done_key] = result.seal_until or self._day_end(current)`
    （`:208-212`）；`on_success` 若装配则回调。
12. **失败不封账**：`failed += 1`，下一轮继续重试（`:213-216`）。

关键设计点：

- `seal_until` 是渠道可选的“封账截止时刻”。Qoder 封到下一轮活动开放（10:00），
  OfficeAce 封到北京时间次日 0 点，其余封到本地自然日结束。
- `_done_scopes` 的键含 `day`，跨天由 `_prune` 清理（`:140-147`），避免长驻进程
  每天每账号攒一条。
- `_day_end` 用 `time.mktime` 本地时区次日 0 点（`:124-127`）。
- `TaskReport` 只有 `attempted/succeeded/failed/skipped` 四个计数（`tasks/__init__.py`）。

## 4. 各渠道签到语义

### 4.1 CodeArts

源码：`src/provider/codearts/client.py:322-424`；门面 `codearts/__init__.py:109-135`。

- 状态查询：`GET {snap}/v1/ops/delivery?channel=IDE`，**不带 `snap-manager` 前缀**
  （带了回 APIG.0101，`:327` 注释）。
- 四种状态（`codearts/events.py:407-409`）：
  - `ELIGIBLE` + `claimable` → `claim` + `confirm`
  - `CLAIMED`（已领未确认）→ **只补 confirm**，不能当已签跳过
  - `CONFIRMED` / `CONSUMED` → 已签
  - `ELIGIBLE` 但不可领 / 未知状态 → `ok=false`，下轮重试
- `claim_daily_credit`（`:335`）：`POST /v1/ops/claim`，body 含
  `campaignId=1`、`idempotentKey`、`channel=IDE`；业务码非 0 是业务失败
  （HTTP 200），不能只看状态码。
- `_confirm_daily_credit`（`:362`）：`POST /v1/ops/confirm`；积分在 claim 时已到账，
  confirm 只把状态从 `CLAIMED` 推到 `CONFIRMED`；confirm 失败返回 `ok=false`，
  让任务下轮补确认。
- 活动不存在 / 已结束：`ok=True`（不是错误），避免后台每 10 分钟刷失败日志。
- `checkin_status`（`__init__.py:113`）：只读展示 `active/today_checked_in/
  today_credit/activity_name`。
- `checkin_scope`（`:128`）：`codearts|uid`；uid 为空返回空串，调用方回落凭证 ID。

另一套协议：`Johnsheng1/codearts2api/benefit.py:41` 使用
`GET/POST /api/v1/benefit/claim`（opengw），`create_time` 是最近签到时间；
它是独立脚本，配合 Windows 计划任务（`benefit.py:19-20`）。本地 Go 版
`codearts2api` **未发现签到代码**。两套端点不能混用。

### 4.2 Qoder

源码：`src/provider/qoder/client.py:435-580`；门面 `qoder/__init__.py:118-135`。

- 2026-10 起改为活动制：优先 `GET /sash/api/v1/me/campaigns`，必须带
  `Cosy-ClientType: 10`；否则上游静默返回空列表。
- 活动制不可用（404/405/410）时回退旧 `daily-check-in/{status,claim}`。
- 活动制 claim：`POST /sash/api/v1/me/campaigns/{id}/claim`。
  - `replayed:true` → 幂等重放 → `already_checked_in`
  - `BLOCKED` / `SAME_PERSON_ALREADY_CLAIMED` → 归一为“今日已签”
  - 明确 `BLOCKED`（非同一自然人）→ `ok=false`，交人工/重试
- `seal_until` 来自 `qoder_events.checkin_window_end(data)`：封到当前签到窗口结束
  （= 下一轮 10:00 开放时刻），每天只打一次上游。
- 区域不可用：`CheckinResult(ok=False, message="本区域未开放签到接口（HTTP 4xx）")`。
  注意 `CheckinResult` 没有 `skipped` 位，所以这里的 `ok=false` 会让任务每轮重试；
  这是“接口存在但活动不可用”与“失败”混在一起的设计缺口（见 §13）。

### 4.3 CodeBuddy CN

源码：`src/provider/codebuddy/checkin.py`；门面 `codebuddy/client.py:814-888`。

- `parse_checkin_response`（`checkin.py:46-68`）：
  - `code == 0` 且 `data.credit` 是有限非布尔数值 → 成功
  - `code` 非 0 但 message 含“已签到” → `ok=True, already_checked_in=True`
  - 其他 → `ok=False`
- `parse_checkin_status`（`:70-95`）：`code==0` 且 `data` 是对象才有效；
  可选字段缺失不报错。`active=false` 是正常状态，不抛异常。
- `fetch_status`（`:134`）：`POST /billing/meter/checkin-activity-status`；
  401/403 视为凭证被拒，≥500 视为上游失败。
- `claim`（`:164`）：`POST /billing/meter/daily-checkin`；成功后回查状态，
  回查失败不影响“已领取”的结论（`_status_or_none`）。
- `checkin_scope_key`（`:112`）：`endpoint|user_id`；身份为空返回空串。

### 4.4 CodeBuddy Intl

- `intl.py:83-86`：`checkin = None`、`checkin_scope = None`、`checkin_status = None`。
- 文件顶部说明：上游 `/billing/meter/checkin-activity-status` 回 `active:false`
  且活动字段全空；`/v2/activity/*` 整段挂在认证插件后，401 不能证明子路由存在。
- 结论：**接口存在但活动不可用**；不是账号级未开启，不能通过重试或换号解决。

### 4.5 TRAE

源码：`src/provider/trae/client.py:743-840`。

- `checkin_scope`：`trae|uid`。
- 判定纪律（作者实测教训）：
  1. 先 `status` 查 `checked_in/credits/enable`；已签 → `already_checked_in`。
  2. `enable=false` → `ok=false`，“当前账号不可签到”。
  3. claim 返回 `code:0` **不能**当成功；必须回查 `status.credits`，只有
     `checked_in=true` 且 `credits` 有增量才返回成功。
  4. `9074`（当前参与用户太多）按限流软失败；一轮内最多 `CHECKIN_ATTEMPTS` 次，
     每次换一个全新设备号；仍失败返回软失败，不伪装成功。
- 设备号：`trae/credential.py:122` `new_checkin_device_id()` 生成 16 位数字；
  同一轮 status 与 claim 必须用同一个设备号。
- 国际站：Coding2API 没有单独区分；dsh 明确“国际账号没有签到，绝不联系”。
  迁移时必须按区域关闭，不能对 Intl 账号调用 CN 签到接口。

### 4.6 OfficeAce

源码：`src/provider/officeace/client.py:264-345`。

- 状态：`GET /v1/subscription`（带 `x-subscription-type: v2`），读 `daily.claimed/
  credit/activity_name`；没有控制面凭据（只有聊天 app_key）→ `active:false`。
- 领取：`POST /v1/subscription/bonus/claim`，body 必须是空 JSON 对象且带
  `Content-Type: application/json`；裸 POST 会 500 `OfficeAce.11020001`。
- 成功判定：签到前后 `_bonus_snapshot` 的剩余量增量 > 0；`points` 是活动累计
  发放量，不能当本次收获。
- 幂等：上游对当日重复领取返回 200 + 同样活动列表，`current_value` 不变；
  增量 ≤ 0 且已有余额 → `already_checked_in`；既无活动也无增量 → `ok=false`
  让任务下轮再试（宁可多刷日志也不漏签）。
- `seal_until`：`_next_beijing_midnight(moment)`（`officeace/client.py:474`）。
- 合规：这是“领当日奖励积分”，属于权益领取，默认应关闭。

## 5. 一键/批量签到

### 5.1 后端

- 端点：`POST /api/credentials/batch`，body `{action:"checkin", ids:[...]}`。
- 支持动作：`probe`、`toggle`、`checkin`、`revive`（`admin_credentials.py:52`）。
- 上限：200 条（`:57`）；去重保序（`:521-530`）。
- 在途并发：`asyncio.Semaphore(4)`（`:66`、`:534`）；只约束 `probe/checkin`
  这两类真正出网的动作，`toggle/revive` 不受限（`:546-548`）。
- 逐条隔离：`InvalidRequest` → 该条 `{ok:false, error:...}`；其他异常 →
  `{ok:false, error:"action_failed"}`，不拖垮整批（`:559-565`）。
- 返回：`{action,total,succeeded,failed,results:[...]}`（`:586-587`）。
- 审计：`checkin` 不写审计；只有 `toggle/revive` 写一条汇总（`:72-75`、`:573-587`）。

### 5.2 前端

- `BATCH_BUTTONS` 中含 `checkin`（`CredentialsPage.tsx:173-181`）。
- 批量结果浮层逐条列失败原因，最多 5 条（`:183-200` 附近）。
- 单行“签到”按钮常驻（`:1864-1887`）。
- `supportsCheckin()` 只排除 `codebuddy_intl`（`:120-122`）；Kilo/Zen 无凭证行，
  实际入口不出现。注意 `:1867` 的注释“签到现全渠道支持”与 Kilo/Zen 事实不符，
  属于前端文案过度概括，不应作为迁移依据。

### 5.3 语义边界

- 批量不是事务：已成功的不会因后面失败回滚。
- 批量成功后清空选中集合（`:531-534`），避免重复点“签到”。
- 批量结果不落库；页面刷新后只剩凭证行的实时状态。

## 6. 自动定时签到

### 6.1 任务装配

- `TaskRunner._loop_specs()` 永远包含 `checkin`（`runner.py:240-251`）。
- 周期：`_checkin_interval` = 60s（配置了时点）或 600s（留空）。
- 启动：`start()` 只立即跑额度探测与 refresh，不立即跑签到（`:217-225`）。
  因此首次签到最早在启动后 60s/600s。
- 手动 `run_task("checkin")` 绕过 `due()`，但任务正在跑时立即返回
  `busy=True`，不排队（`:277-296`）。
- `TaskRunner` 的 `_loop` 每次 sleep 前重新求值间隔，支持热更（`:383-406`）。
- 异常被 `_guarded` 吞掉并记运行态；不会拖垮服务（`:432-454`）。

### 6.2 时点与抖动

源码：`src/tasks/checkin.py:20-139`。

- `checkin_times` 是 `HH:MM,HH:MM`；解析宽容，坏段只 warning 不致命
  （`:20-47`）。
- 留空 → `due()` 恒 True，保持旧的全天 10 分钟轮询。
- 有配置 → 每个“日期+时点”抽一次抖动偏移（0..`jitter_minutes`），
  窗口是 `[slot - offset, slot]`；同一天同一时点只放行一次。
- 抖动最多提前到当日 0 点（`span = min(jitter, slot)`），避免窗口跨到前一天。
- `_fired_slots`、`_slot_offsets` 都是内存；跨天由 `_prune_slots` 清理。
- **不补签**：若服务在 `[slot-offset, slot]` 内没有醒着，`due()` 在 `slot` 之后
  永远返回 False，当天不会再签。这与 dsh 的 catch-up 语义相反。

### 6.3 配置

- `src/config.py:169-176`：`checkin_times` 默认空；`checkin_jitter_minutes` 默认 0。
- `src/runtime_settings.py:253-260`：两项都是可热更配置，归属任务 `checkin`。
- 周期不是独立配置项，由 `has_times()` 决定 60s/600s（`runner.py:199-207`）。

## 7. 签到状态查询

- `GET /api/credentials/{id}/checkin`（`admin_credentials.py:589-597`）：
  只读，不写任何状态；provider 必须实现 `checkin_status`，否则 400
  “credential does not support checkin status”。
- 各渠道返回形状：
  - CodeArts：`{active,today_checked_in,today_credit,activity_name}`
  - Qoder：`CheckinStatus` 的 dict（`active/today_checked_in/streak_days/
    today_credit/total_credits/activity_name/is_streak_day`）
  - CodeBuddy CN：同上，来自 `checkin-activity-status`
  - OfficeAce：`{active,today_checked_in,today_credit,total_credits,activity_name}`
  - TRAE：**没有 `checkin_status` 方法**；只有 `checkin` 内部调用 status。
  - CodeBuddy Intl：`None`，前端隐藏入口。
- 状态是实时上游查询，不缓存、不落库；上游失败时返回 400/异常，不伪造“未签到”。

## 8. 签到后额度刷新

- 手动单条：`checkin_credential_once` 中 `if result.ok and not
  result.already_checked_in: schedule_probe(credential_id)`（`admin_credentials.py:210-211`）。
- 批量：同一 helper，因此每条成功都会 `schedule_probe`。
- 自动：`tasks/runner.py:638` 构造 `CheckinTask` 时**没有 `on_success`**，
  所以自动签到成功后不会立即探测；只能等 `quota_probe` 的下一轮（默认 60 分钟，
  `quota_probe_minutes`）或管理台手动探测。
- `schedule_probe`（`src/main.py:769-800`）：异步 `create_task`，失败只
  `mark_probe_failed`，不阻塞响应；完成的 task 从 `pending_probes` 摘除。
- `credit_events` 表记录两次探测之间的净变化，`source` 只有 `observed`/`sync`，
  **不写“签到 +N”**（`schema.sql:171-197`、`db/repo.py:520`）。这是正确的：
  上游不打日志，diff 不能归因。
- 结论：签到后的额度可见性在自动路径上是“最多滞后一轮探测”；迁移时应显式
  在签到成功后触发一次额度探测（成功且非 already 时），而不是依赖小时轮询。

## 9. 失败与过期凭据处理

### 9.1 任务层

- 任何异常 → 该条 `failed += 1`，只记 warning，继续下一条；不会中断整轮。
- 不修改凭证的 `enabled/disabled/auth_status`；签到失败不会自动禁用账号。
- 失败不封账，下一轮继续重试；没有指数退避，重试节奏就是 60s/600s。
- `Pacer` 全局串行化上游请求，避免一轮内并发打爆。
- 解密失败（`APP_SECRET` 更换/密文损坏）只跳过该条，不让整轮失败。

### 9.2 手工入口

- 异常被翻译成受控原因（`describe_probe_failure`）：
  `credential_rejected` / `rate_limited` / `upstream_unavailable` /
  `upstream_rejected` / `upstream_response_invalid` / `network_unreachable` /
  `upstream_timeout` / `unknown_error`。
- 上游原文只进服务端日志（截断 200 字），不回显给客户端。

### 9.3 过期凭据

- 签到本身**不刷新 token**；CodeArts/Qoder/OfficeAce 的 refresh 由
  `RefreshTask` 独占（一次性 refresh_token 必须单写者）。
- 过期凭据在签到时会得到 401/403/业务码；任务层只记失败，不会自动重新登录。
- CodeBuddy Intl 的“不支持”与“凭证过期”是两回事：前者应在能力层过滤，
  后者应触发重新授权，不能都表现成每轮失败。
- 重登录/人工恢复不在签到任务内；管理台有 revive/重新授权入口。

### 9.4 各渠道特殊失败

| 渠道 | 失败语义 | 处理 |
| --- | --- | --- |
| CodeArts | claim 业务码非 0（如 40001 未到刷新时间） | `ok=false`，下轮重试 |
| CodeArts | confirm 失败 | `ok=false`，下轮补确认；积分已到账 |
| Qoder | 活动接口 404/405/410 | 回退旧接口；仍不可用 → `ok=false` + 区域文案 |
| Qoder | `BLOCKED` | 明确失败；`SAME_PERSON_ALREADY_CLAIMED` 归一为已签 |
| CodeBuddy | `code=10001` + “已签到” | 成功（already） |
| CodeBuddy | 401/403 | 协议违规（凭证被拒），任务记失败 |
| TRAE | `9074` | 软失败，换设备号重试；全失败返回失败 |
| TRAE | claim `code:0` 但回查无增量 | 失败，不伪装成功 |
| OfficeAce | 无控制面凭据 | `ok=false` + 明确文案 |
| OfficeAce | 无活动/无增量 | `ok=false`，下轮再试 |

## 10. 定时任务实现与结果存储

### 10.1 定时实现

- Python `asyncio`：每个任务一个 `asyncio.Task`，`_loop` 里 `sleep(interval)`
  后跑一轮；`stop()` 取消并等待全部任务（`runner.py:383-406`、`:524-530`）。
- 周期可热更：间隔是零参 lambda，每次 sleep 前现读。
- 手动与周期互斥：`_running` 集合；手动触发撞上运行中立即返回 busy，
  不排队（`:419-430`）。
- 签到没有独立的“时点调度器”，靠短周期醒来问 `due()`。

### 10.2 结果存储

- **没有 `checkins` 表**：`schema.sql:9-11` 明确说明签到去重由
  `CheckinTask` 的当日作用域集合实现，以上游 status 为准。
- `_done_scopes: dict[str, int]`（`checkin.py:72`）：scope → 封账截止 epoch；
  进程重启即丢。
- `_fired_slots` / `_slot_offsets`：进程内时点放行状态；重启即丢。
- `TaskStatusStore`：进程内最近一轮 `TaskRun`（`status.py:111-160`），
  重启后显示“本次启动以来未运行”。
- 唯一落库的签到相关数据是 `growth_events`（成长中心，仅 CodeBuddy）与
  `credit_events`（额度净变化，不归因）。
- 设计取舍（源码注释）：不落库避免迁移/保留期/清理；代价是重启后无法回答
  “今天到底签没签”，只能再问上游。上游签到幂等是这一取舍的前提。
- 风险：如果某个渠道的签到**不幂等**（每次 claim 都发奖/计数），重启后重签
  可能重复领取或触发风控。迁移时必须逐渠道确认幂等性，不能假设所有上游都幂等。

## 11. dsh-wtq-pool 的对照实现

dsh 是 TypeScript 插件，覆盖 WorkBuddy/CodeBuddy、Qoder、TRAE；它的签到设计
比 Coding2API 更细，值得借鉴的是**槽位持久化**与**延迟开放重试**。

### 11.1 时点计算

源码：`src/checkin-time.ts`。

- 固定时区 `Asia/Shanghai`（`:19`），默认 `09:00`（`:23`）；
- 每 60s 轮询（`:26`）；
- `zonedDayHour` 用 `Intl.DateTimeFormat` 取北京时间的日期与小时（`:30-43`）；
- `normaliseCheckinHours` 只接受 0–23 的整数/数字字符串，空列表回落默认 9 点
  （`:67-77`）；
- `dueCheckinSlot`：返回**最早一个已过且未消费**的小时槽（`:92-104`）。
  这是 catch-up：机器睡过 09:00，醒来仍会补签，而不是等明天。

### 11.2 槽位持久化

- `CHECKIN_SLOTS_FILE = "checkin-slots.json"`（`src/host.ts:860`），
  保留 3 天（`:862`）；
- `loadCheckinSlots` / `saveCheckinSlots`（`:873-930`）按天分组，
  写入前裁剪旧日期；
- `runCheckinPass` 在启动时就把槽位记为已消费（`host.ts:5178-5230` 附近），
  防止长跑一轮被下一次 tick 重复启动。

### 11.3 一次签到轮次

`runCheckinPass`（`host.ts:5178`）：

- 单飞：`checkinPassRunning` + `checkinPassToken`；手动/定时/成长互斥；
- 按来源并发 fan-out：WorkBuddy、Qoder、TRAE 各自 `isolatedCheckinWorker`，
  一个来源抛错不影响其他来源的计数（`:5240-5245`）；
- 每来源内部串行，并按账号间隔：
  Qoder `QODER_ACCOUNT_DELAY_MS`、TRAE 5s ± 2s、WorkBuddy 700ms
  （`:5622`、`:5834`、`:5913`）；
- Qoder 账号先读 campaigns；没有可领时返回 `opensAt`（10:00 北京时间），
  `runCheckinPass` 把 `qoderRetryAt` 写成 `min(opening, backoff)`，到点只重跑
  Qoder，不重跑整个 pass（`:5268-5290` 附近）；
- 失败分类：`qoderFailureRetryable` 只对 408/429/5xx 重试；4xx 判定不重试
  （`host.ts:5880` 附近）；
- TRAE 国际账号不联系（`traeCheckinAll` 只处理 CN，`host.ts:5894` 注释）；
- 过期 token 跳过（桌面端会刷新）。
- 每一步支持 `AbortSignal`，服务停止时中止，不留下半轮计数。

### 11.4 与 Coding2API 的差异

| 维度 | Coding2API | dsh |
| --- | --- | --- |
| 时区 | 本地时区（`time.localtime`） | 固定 `Asia/Shanghai` |
| 错过时点 | 不补签 | catch-up，最早未消费槽 |
| 槽位状态 | 内存 | `checkin-slots.json` 持久化 3 天 |
| 多来源 | 一个任务串行所有凭证 | 按来源并发、来源内串行 |
| 账号间隔 | Pacer 全局串行 | 每来源独立延迟（700ms/5s/…） |
| 开放时间重试 | 无（Qoder 靠 seal_until 下一轮） | `opensAt` 精确定时 + 退避 |
| 失败分类 | 统一 failed | 可重试 408/429/5xx vs 终止 4xx |
| 中止 | 任务取消 | AbortSignal + 所有权 token |
| 结果 | 内存 TaskRun | 槽位文件 + 面板 last pass |

## 12. 其他参考仓库

### 12.1 Johnsheng1/codearts2api

- `benefit.py` 是独立脚本：`GET/POST /api/v1/benefit/claim`，
  `create_time` 是最近签到时间；`main` 支持 `status`/`claim`/`models`/默认
  “签到 + 查询余额”。
- 没有任务调度、没有批量、没有状态持久化；README 建议用 Windows 计划任务
  每天 09:05 执行（`benefit.py:19-20`）。
- 与 Coding2API 的 `/v1/ops/*` 是两套端点，迁移前必须确认账号套餐对应哪套。

### 12.2 本地 codearts2api（Go）

- 未发现 `ops/delivery|claim|confirm` 或 `benefit/claim`；签到不在其范围。
- 操作者本人作品（2026-10-29 确认），代码可按作者授权复用；复制时保留声明并标注来源。
- 账号池/调度证据见 `docs/RESEARCH.md` §10。

### 12.3 zcode2api

- 没有签到；有验证码求解（`app/captcha.py`、`captcha_node/solver.js`）与
  `X-Aliyun-Captcha-Verify-Param`。
- AGPL-3.0；本项目不复制代码、不实现验证码求解。

## 13. 分类结论

> 本审计**没有“已实现有验证”项**：本机没有用真实账号复核。参考仓库的作者实测
> 归入“已实现未验证（作者实测）”，不能当作本项目验证。

| 渠道 | 分类 | 依据 |
| --- | --- | --- |
| CodeArts | **已实现未验证（参考仓库有单测 + 作者实测注释）** | `codearts/client.py:380`；`tests/provider/codearts/test_checkin.py`；活动不存在/结束返回 ok |
| Qoder | **已实现未验证（参考仓库有单测 + 作者实测注释）** | `qoder/client.py:435`；`tests/provider/qoder/test_checkin.py`；区域不可用返回 ok=false |
| CodeBuddy CN | **已实现未验证（参考仓库有单测）** | `codebuddy/checkin.py`；`tests/provider/codebuddy/test_checkin.py` |
| CodeBuddy Intl | **接口存在但活动不可用（渠道显式关闭）** | `codebuddy/intl.py:83`；UI 注释 `CredentialsPage.tsx:108-118` |
| TRAE | **已实现未验证（参考仓库有单测；CN 可用，Intl 未区分）** | `trae/client.py:747`；`tests/provider/trae/test_checkin.py`；dsh 明确 Intl 无签到 |
| OfficeAce | **已实现未验证（参考仓库有单测；权益领取）** | `officeace/client.py:290`；`tests/provider/officeace/test_provider.py` |
| Kilo | **不支持** | `kilo/client.py` 无 checkin 方法 |
| Zen | **不支持 + 本项目排除** | `zen/client.py` 无 checkin；门禁伪装不迁移 |
| ZCode | **不支持** | zcode2api 无签到；验证码求解排除；**ZCode 已于 2026-10-29 整体排除，不实现 Provider** |

### 13.1 M14-E 落地状态（2026-10-29）

- TRAE：CN 状态查询与可选 claim 已实现（`src/checkin/trae.rs`），默认
  `checkin.claim_enabled = false`；Intl 返回 `Unsupported` 且不发送请求。
- 设备号：**不生成**。只接受运维显式配置的 16 位数字 `checkin.device_id`；
  缺失/非法时不发请求。未决问题 4 以“不生成”结案。
- 成功判定：`code:0` 不算成功，必须回查 `checked_in` 且 credits 增加；
  `9095` 记为已领取、`9074` 记为设备/参与上限且不重试。
- 状态：MockVerified；无真实账号，未发真实请求。

## 14. PolyGate 迁移设计建议（只设计，不实现）

### 14.1 类型模型

建议新增（M12）：

```rust
pub enum CheckinState {
    Done { credit: Option<f64> },
    Already,
    Unavailable { reason: String },  // 活动未开放 / 区域不支持
    Failed { retryable: bool, reason: String },
    Unsupported,
}

pub struct CheckinOutcome {
    pub state: CheckinState,
    pub message: String,
    pub seal_until: Option<DateTime<Utc>>,
    pub activity: Option<CheckinActivity>, // 连续天数/今日积分等展示字段
}
```

关键：`Unavailable` 必须与 `Failed` 分开，否则“区域不支持”会被任务无限重试。
`retryable` 也要显式，避免把 4xx 判定当瞬时失败。

### 14.2 Provider 契约

- `Capability::Checkin` 默认 false；`Provider::checkin` / `checkin_status`
  默认 `Err(Unsupported)`。
- 需要 `checkin_scope(&Credential) -> Option<String>`：返回 `None` 时调用方
  回落到 `account_id`；绝不能返回一个所有未知身份都相同的字符串。
- 需要 `CheckinActivity` 展示类型，避免各 provider 返回私有 dict。
- 不引入 Python 式 `getattr` 能力探测；用 trait 默认实现 + `Capabilities`。

### 14.3 通用任务

- 新增 `CheckinService`（或 `tasks::checkin`）：
  - 候选：`accounts` 中 enabled 且 `Capabilities::Checkin` 的账号；
  - scope 去重：同上游账号只签一次；
  - 封账：`(account_id/scope, local_day)`；
  - 时点：配置 `times`、`jitter`、`timezone`（默认 `Asia/Shanghai`）；
  - 补签：采用 dsh 的 catch-up 语义（最早未消费且已过的槽）；
  - 重试：per-account backoff；`Unavailable` 不重试到当天结束；
  - 并发：自动路径串行/小并发，批量路径显式上限；
  - 成功且 `Done` → 触发一次额度探测（不要依赖小时轮询）。
- 持久化建议：新增 `checkin_runs` 表（M12 迁移）：
  `id, account_id, scope, slot (local date+hour), state, credit, message,
  started_at, finished_at, error`；唯一键 `(account_id, slot)` 或
  `(scope, slot)`。这样重启后能回答“今天签没签”，也能给 UI 历史。
  即使上游幂等，也不应把“进程没重启”当作状态存储。
- 不要把 `seal_until` 只放内存；它必须能跨重启恢复。

### 14.4 API / UI

- `POST /admin/accounts/{id}/checkin`（单条）
- `POST /admin/accounts/checkin`（批量，body `{ids:[...]}`，上限 200，逐条结果）
- `GET /admin/accounts/{id}/checkin`（实时状态）
- `GET /admin/checkin/runs?account_id=&from=&to=`（历史，来自 `checkin_runs`）
- 自动任务状态复用现有任务运行态/`doctor` 风格；UI 显示“最近一次真实执行”，
  no-op 不覆盖。
- 所有返回只含受控原因，不含 token/上游原文。

### 14.5 配置

```toml
[checkin]
enabled = false                 # 默认关闭；权益领取需显式开启
times = ["09:00"]               # 空 = 不启用定时（不是全天轮询）
jitter_minutes = 0
timezone = "Asia/Shanghai"
batch_max = 200
batch_concurrency = 4
quota_refresh_after_success = true
```

与 Coding2API 的差异：留空不应意味着“全天每 10 分钟检查”，而应明确
“未配置定时 → 只允许手动”。这是更安全、更可解释的默认值。

### 14.6 合规边界

- CodeArts `/v1/ops/claim` + `/confirm`、OfficeAce `/v1/subscription/bonus/claim`
  属于权益领取：默认关闭，配置显式开启，文档写明风险。
- Qoder campaign claim、TRAE checkin claim 同理；不实现验证码、不伪造设备指纹、
  不绕过平台限制。
- TRAE 的“新设备号”必须先确认是正常客户端参数还是反滥用标识；无法确认则不迁移。
- 不实现“封禁后自动换号续跑”；签到失败不自动禁用/换号。
- 所有签到请求必须经过 `SecretBox` 解密后的内存凭据，日志只输出账号 id 与
  受控原因。

### 14.7 测试计划（M12/M15）

- Mock Provider 覆盖：`Done`/`Already`/`Unavailable`/`Failed(retryable)`/
  `Failed(non-retryable)`/`Unsupported`。
- 两账号：同 scope 去重、不同 scope 各签一次、一账号失败不影响另一账号。
- 时点：注入时钟，验证 catch-up、jitter 只抽一次、同槽只跑一次、跨天清理。
- 重启：`checkin_runs` 恢复封账；不会重复领取。
- 批量：200 上限、并发 4、逐条隔离、成功/失败计数。
- 自动：成功后触发额度探测；`Unavailable` 不再重试。
- 安全：日志/响应不含凭据；错误原因受控。

## 15. 未决问题

1. CodeArts 两套签到端点（`/v1/ops/*` vs `/api/v1/benefit/claim`）分别对应哪些
   套餐/区域？必须用真实账号探测，不能按 README 猜。
2. Qoder 国际站是否完全无签到，还是活动制接口在特定账号可见？
3. ~~TRAE 国际站是否真的没有签到~~ → M14-E 按 dsh 证据实现为 `Unsupported`，
   不发请求；若上游以后确认可用再评估。
4. ~~TRAE 的 16 位设备号是否属于“设备指纹伪造”~~ → 是，因此不生成；
   只接受运维显式提供的 16 位数字（见 §13.1）。
5. OfficeAce 的 bonus claim 是否允许自动化？默认关闭，需上游明确许可。
6. CodeBuddy CN 的签到活动是否对所有账号开放？`active:false` 时是渠道级还是
   账号级？
7. 各上游签到是否幂等？不幂等的渠道必须有持久化封账，不能只靠内存。
8. 签到成功后的额度刷新是否应强制？若探测本身会触发风控，应做成可配置。
