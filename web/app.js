(() => {
  "use strict";

  const state = {
    csrf: null,
    user: null,
    view: "dashboard",
    provider: "codearts",
    accountRegion: "cn",
    bootstrap: null,
    flow: null,
    flowKind: null,
    flowTimer: null,
    flowPopup: null,
  };

  const PROVIDERS = {
    codearts: {
      label: "CodeArts",
      authPath: "codearts",
      siteNote: "将打开华为 CodeArts 官方授权页面。PolyGate 只保存授权返回的凭据，不接触账号密码。",
      startLabel: "打开授权页面",
      successToast: "CodeArts 账号授权成功",
      supportsCheckin: true,
    },
    qoder: {
      label: "Qoder",
      authPath: "qoder",
      siteNote:
        "将打开 Qoder 官方授权页面。在官方页面登录并确认后回到本页面，PolyGate 只保存授权返回的凭据，不接触账号密码。",
      startLabel: "打开授权页面",
      successToast: "Qoder 账号授权成功",
      supportsCheckin: true,
      pollInterval: 2000,
      autoCheckNote: "等待在官方页面完成授权，本页面会自动检查结果…",
    },
    workbuddy: {
      label: "WorkBuddy",
      authPath: "workbuddy",
      siteNote:
        "将打开 WorkBuddy 官方授权页面。在官方页面登录并确认后回到本页面，PolyGate 只保存授权返回的凭据，不接触账号密码；不会伪造设备指纹或代替你在页面上操作。",
      startLabel: "打开授权页面",
      successToast: "WorkBuddy 账号授权成功",
      supportsCheckin: true,
      pollInterval: 5000,
      autoCheckNote: "等待在官方页面完成授权，本页面会自动检查结果…",
    },
    trae: {
      label: "TRAE",
      authPath: "trae",
      siteNote:
        "将打开 TRAE 官方授权页面。需要填入与登录浏览器一致的 machineId / deviceId（32 位十六进制）；PolyGate 不会生成或伪造设备身份。授权完成后浏览器会停在一个提示页，把地址栏链接粘贴回本窗口即可完成绑定。",
      startLabel: "打开授权页面",
      successToast: "TRAE 账号授权成功",
      supportsCheckin: true,
      needsDeviceIds: true,
      pasteCallback: true,
      cnLoginOnly: true,
      autoCheckNote: "等待在官方页面完成授权；完成后把地址栏链接粘贴到下方。",
    },
  };

  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => Array.from(root.querySelectorAll(selector));

  function esc(value) {
    return String(value ?? "")
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function fmtDate(value) {
    if (!value) return "—";
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return esc(value);
    return date.toLocaleString("zh-CN", { hour12: false });
  }

  function toast(message, kind = "") {
    const node = $("#toast");
    node.textContent = message;
    node.className = `toast ${kind}`.trim();
    node.classList.remove("hidden");
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => node.classList.add("hidden"), 3600);
  }

  async function api(path, options = {}) {
    const headers = { "Content-Type": "application/json" };
    if (state.csrf) headers["X-CSRF-Token"] = state.csrf;
    const response = await fetch(path, {
      credentials: "same-origin",
      ...options,
      headers: { ...headers, ...(options.headers || {}) },
    });
    let data = null;
    try {
      data = await response.json();
    } catch (_) {
      data = null;
    }
    if (response.status === 401 && options.auth !== false) {
      state.csrf = null;
      showAuth("login");
    }
    if (!response.ok) {
      const error = new Error(data?.error?.message || `请求失败 (HTTP ${response.status})`);
      error.status = response.status;
      error.code = data?.error?.code;
      throw error;
    }
    return data;
  }

  // ---------------------------------------------------------------- auth flow

  function showAuth(mode) {
    $("#boot").classList.add("hidden");
    $("#app").classList.add("hidden");
    $("#auth-screen").classList.remove("hidden");
    $("#setup-form").classList.toggle("hidden", mode !== "setup");
    $("#login-form").classList.toggle("hidden", mode !== "login");
    $("#recover-form").classList.toggle("hidden", mode !== "recover");
    $("#auth-error").textContent = "";
    if (mode === "setup") $("#setup-password").focus();
    if (mode === "login") $("#login-password").focus();
    if (mode === "recover") $("#recover-code").focus();
  }

  function enterApp(session) {
    state.csrf = session.csrf_token;
    state.user = session.username || "admin";
    $("#admin-user").textContent = state.user;
    $("#auth-screen").classList.add("hidden");
    $("#boot").classList.add("hidden");
    $("#app").classList.remove("hidden");
    renderRoute();
  }

  async function loadSession() {
    try {
      const session = await api("/admin/api/session", { auth: false });
      enterApp(session);
      return true;
    } catch (error) {
      if (error.status === 401) return false;
      throw error;
    }
  }

  async function boot() {
    applyTheme(localStorage.getItem("polygate.theme") || "dark");
    try {
      state.bootstrap = await api("/admin/api/bootstrap", { auth: false });
      $("#version-label").textContent = `v${state.bootstrap.version || ""}`;
      if (!state.bootstrap.initialized) {
        showAuth("setup");
        return;
      }
      if (await loadSession()) return;
      showAuth("login");
    } catch (error) {
      showAuth("login");
      $("#auth-error").textContent = error.message;
    }
  }

  async function submitSetup(event) {
    event.preventDefault();
    const password = $("#setup-password").value;
    const confirm = $("#setup-confirm").value;
    if (password !== confirm) {
      $("#auth-error").textContent = "两次输入的密码不一致";
      return;
    }
    try {
      const data = await api("/admin/api/setup", {
        method: "POST",
        auth: false,
        body: JSON.stringify({ password }),
      });
      showRecovery(data.recovery_code, () => enterApp(data));
    } catch (error) {
      $("#auth-error").textContent = error.message;
    }
  }

  async function submitLogin(event) {
    event.preventDefault();
    try {
      const data = await api("/admin/api/login", {
        method: "POST",
        auth: false,
        body: JSON.stringify({
          username: $("#login-username").value.trim() || "admin",
          password: $("#login-password").value,
        }),
      });
      $("#login-password").value = "";
      enterApp(data);
    } catch (error) {
      $("#auth-error").textContent = error.message;
    }
  }

  async function submitRecover(event) {
    event.preventDefault();
    try {
      const data = await api("/admin/api/recover", {
        method: "POST",
        auth: false,
        body: JSON.stringify({
          recovery_code: $("#recover-code").value.trim(),
          new_password: $("#recover-password").value,
        }),
      });
      $("#recover-code").value = "";
      $("#recover-password").value = "";
      showRecovery(data.recovery_code, () => enterApp(data));
    } catch (error) {
      $("#auth-error").textContent = error.message;
    }
  }

  function showRecovery(code, after) {
    openModal({
      title: "保存恢复码",
      body: `
        <p class="muted">恢复码只显示这一次。请保存在离线位置；忘记管理员密码时用它恢复访问。</p>
        <pre class="json">${esc(code)}</pre>`,
      actions: [
        {
          label: "我已保存",
          primary: true,
          onClick: () => {
            closeModal();
            after();
          },
        },
      ],
      dismissible: false,
    });
  }

  async function logout() {
    try {
      await api("/admin/api/logout", { method: "POST" });
    } catch (_) {
      // Session may already be gone.
    }
    state.csrf = null;
    showAuth("login");
  }

  // ---------------------------------------------------------------- navigation

  const routeMeta = {
    dashboard: ["仪表盘", "账号、额度与运行状态"],
    accounts: ["账号管理", "Provider 账号授权与状态"],
    models: ["模型管理", "原生模型目录、别名与多目标路由"],
    checkins: ["签到管理", "M12 签到记录（只读）"],
    usage: ["用量统计", "已记录的下游请求"],
    logs: ["请求日志", "请求元数据与清理策略（不含提示词与凭据）"],
    settings: ["系统设置", "实例与安全信息"],
  };

  function navigate(view) {
    state.view = view;
    location.hash = `#/${view}`;
    renderRoute();
  }

  function renderRoute() {
    const raw = location.hash.replace(/^#\/?/, "");
    const view = routeMeta[raw] ? raw : "dashboard";
    state.view = view;
    const [title, subtitle] = routeMeta[view];
    $("#page-title").textContent = title;
    $("#page-subtitle").textContent = subtitle;
    $$(".nav-item").forEach((item) => item.classList.toggle("active", item.dataset.view === view));
    $$(".subnav-item[data-provider]").forEach((item) =>
      item.classList.toggle(
        "active",
        view === "accounts" && item.dataset.provider === state.provider,
      ),
    );
    const container = $("#view");
    container.innerHTML = `<div class="empty">正在加载…</div>`;
    const render = {
      dashboard: renderDashboard,
      accounts: renderAccounts,
      models: renderModels,
      checkins: renderCheckins,
      usage: renderUsage,
      logs: renderLogs,
      settings: renderSettings,
    }[view];
    render(container).catch((error) => {
      container.innerHTML = `<div class="empty"><strong>加载失败</strong>${esc(error.message)}</div>`;
    });
  }

  // ---------------------------------------------------------------- dashboard

  async function renderDashboard(container) {
    const data = await api("/admin/api/dashboard");
    const checkin = data.checkin?.latest;
    const requestRecent = data.requests?.recent || [];
    const routing = data.routing || {};
    const successRate =
      routing.success_rate_24h === null || routing.success_rate_24h === undefined
        ? "—"
        : `${(routing.success_rate_24h * 100).toFixed(1)}%`;
    const latency = routing.requests_24h
      ? `${Math.round(routing.avg_latency_ms_24h || 0)} ms`
      : "—";
    container.innerHTML = `
      <div class="grid cards">
        ${statCard("Providers", `${data.providers.total}`, `${data.providers.enabled} 个已启用`)}
        ${statCard("账号", `${data.accounts.total}`, `${data.accounts.available} 个可用 · ${data.accounts.abnormal} 个异常`)}
        ${statCard("额度", `${data.quota.known}`, `${data.quota.unknown} 个未知`)}
        ${statCard("请求总数", `${data.requests.total}`, `${formatTokens(data.requests.input_tokens)} 输入 / ${formatTokens(data.requests.output_tokens)} 输出`)}
      </div>
      <div class="grid cards three">
        ${statCard("24h 成功率", successRate, `${routing.requests_24h || 0} 请求 · ${routing.failures_24h || 0} 失败`)}
        ${statCard("24h 平均延迟", latency, "来自请求日志")}
        ${statCard("调度策略", strategyLabel(routing.strategy), "账号轮询与故障冷却", true)}
      </div>
      <div class="grid two">
        <div class="card">
          <h2>最近签到</h2>
          ${checkin
            ? `<div class="value">${esc(checkinStatus(checkin.status).label)}</div>
               <div class="sub">${esc(checkin.account_name || "")} · ${esc(checkin.business_date || "")}</div>
               <div class="sub">${esc(checkin.summary || "")}</div>`
            : `<div class="empty"><strong>暂无签到记录</strong>签到数据会在账号执行签到后出现。</div>`}
        </div>
        <div class="card">
          <h2>最近请求</h2>
          ${requestRecent.length
            ? `<table class="compact"><tbody>${requestRecent
                .map(
                  (item) => `<tr><td><span class="strong">${esc(item.model || "—")}</span><div class="tiny">${esc(item.status)} · ${fmtDate(item.created_at)}</div></td>
                  <td class="tiny">${formatTokens(item.input_tokens)} / ${formatTokens(item.output_tokens)}</td></tr>`,
                )
                .join("")}</tbody></table>`
            : `<div class="empty"><strong>暂无请求</strong>通过 /v1 调用后这里会显示统计。</div>`}
        </div>
      </div>
      <div class="card">
        <div class="section-head"><h2>最近错误</h2><span class="muted">不含请求内容</span></div>
        ${(routing.recent_errors || []).length
          ? `<div class="table-wrap"><table><thead><tr><th>时间</th><th>Provider</th><th>模型</th><th>API</th><th>状态</th><th>分类</th><th>延迟</th></tr></thead><tbody>
            ${(routing.recent_errors || [])
              .map(
                (item) => `<tr>
                  <td class="tiny">${fmtDate(item.created_at)}</td>
                  <td>${esc(item.provider_kind)}</td>
                  <td>${esc(item.model || "—")}</td>
                  <td class="tiny">${esc(item.api)}</td>
                  <td><span class="badge bad">${item.http_status}</span></td>
                  <td class="tiny">${esc(item.error_category || "—")}</td>
                  <td class="tiny">${item.latency_ms} ms</td>
                </tr>`,
              )
              .join("")}
          </tbody></table></div>`
          : `<div class="empty"><strong>最近没有失败请求</strong>失败会在 /v1 调用出错时记录在这里。</div>`}
      </div>
      <div class="card">
        <div class="section-head"><h2>最近账号</h2><button class="btn small" id="dash-add">添加账号</button></div>
        ${data.recent_accounts.length
          ? `<div class="table-wrap"><table><thead><tr><th>账号</th><th>Region</th><th>认证</th><th>可用</th><th>更新时间</th></tr></thead><tbody>
            ${data.recent_accounts
              .map(
                (account) => `<tr>
                  <td><span class="strong">${esc(account.name)}</span><div class="tiny">${esc((PROVIDERS[account.kind] || {}).label || account.kind)} · ${esc(account.id.slice(0, 8))}</div></td>
                  <td>${regionLabel(account.region)}</td>
                  <td>${authBadge(account.auth_status)}</td>
                  <td>${account.enabled ? '<span class="badge ok">已启用</span>' : '<span class="badge muted">已禁用</span>'}</td>
                  <td class="tiny">${fmtDate(account.updated_at)}</td>
                </tr>`,
              )
              .join("")}
          </tbody></table></div>`
          : `<div class="empty"><strong>还没有账号</strong>添加一个 CodeArts 或 Qoder 账号以开始使用真实 Provider。</div>`}
      </div>`;
    $("#dash-add")?.addEventListener("click", () => {
      state.provider = "codearts";
      navigate("accounts");
    });
  }

  function statCard(label, value, sub, compact = false) {
    return `<div class="card"><h3>${esc(label)}</h3><div class="value${compact ? " compact" : ""}">${esc(value)}</div><div class="sub">${esc(sub)}</div></div>`;
  }

  function formatTokens(value) {
    const number = Number(value || 0);
    if (number >= 1_000_000) return `${(number / 1_000_000).toFixed(1)}M`;
    if (number >= 1_000) return `${(number / 1_000).toFixed(1)}k`;
    return String(number);
  }

  // ---------------------------------------------------------------- accounts

  async function renderAccounts(container) {
    const region = state.accountRegion;
    const provider = PROVIDERS[state.provider] || PROVIDERS.codearts;
    const data = await api(
      `/admin/api/accounts?kind=${encodeURIComponent(state.provider)}&region=${encodeURIComponent(region)}`,
    );
    container.innerHTML = `
      <div class="section-head">
        <div class="filters">
          <div class="segmented" id="region-filter">
            <button data-region="cn" class="${region === "cn" ? "active" : ""}">CN 中国站</button>
            <button data-region="intl" class="${region === "intl" ? "active" : ""}">Intl 国际站</button>
          </div>
          <span class="muted">${provider.label} · ${data.count} 个账号</span>
        </div>
        <button class="btn primary" id="add-account">添加账号</button>
      </div>
      ${data.items.length ? accountsTable(data.items, provider) : `
        <div class="empty">
          <strong>还没有 ${provider.label} ${region === "cn" ? "CN" : "Intl"} 账号</strong>
          点击“添加账号”，在官方 ${provider.label} 页面完成浏览器授权。
        </div>`}`;

    $$("#region-filter button").forEach((button) =>
      button.addEventListener("click", () => {
        state.accountRegion = button.dataset.region;
        renderRoute();
      }),
    );
    $("#add-account")?.addEventListener("click", openAddAccount);
    $$("[data-action]", container).forEach((button) =>
      button.addEventListener("click", () => handleAccountAction(button.dataset.action, button.dataset.id)),
    );
  }

  function supportsCheckin(provider, region) {
    if (!provider.supportsCheckin) return false;
    // WorkBuddy and TRAE only expose a CN check-in endpoint; the server also
    // reports `Unsupported` for Intl, this keeps the button honest.
    if (["workbuddy", "trae"].includes(provider.authPath) && region === "intl") {
      return false;
    }
    return true;
  }

  function accountsTable(items, provider) {
    return `<div class="table-wrap"><table>
      <thead><tr><th>账号</th><th>认证</th><th>额度</th><th>最近签到</th><th>启用</th><th>更新时间</th><th></th></tr></thead>
      <tbody>${items
        .map(
          (account) => `<tr>
            <td><span class="strong">${esc(account.name)}</span>
              <div class="tiny">${regionLabel(account.region)} · ${esc(account.credential?.present ? "凭据已加密保存" : "未授权")}</div></td>
            <td>${authBadge(account.auth_status)}</td>
            <td>${quotaText(account.quota)}</td>
            <td>${account.last_checkin
              ? checkinStatus(account.last_checkin.status).badge
              : supportsCheckin(provider, account.region)
                ? '<span class="badge muted" title="只记录 PolyGate 自己的签到运行；在官方网页签到不会写入本地记录">本地无记录</span>'
                : '<span class="badge muted">不支持</span>'}</td>
            <td>${account.enabled ? '<span class="badge ok">已启用</span>' : '<span class="badge muted">已禁用</span>'}</td>
            <td class="tiny">${fmtDate(account.updated_at)}</td>
            <td><div class="row-actions">
              <button class="btn small" data-action="detail" data-id="${esc(account.id)}">详情</button>
              <button class="btn small" data-action="${account.enabled ? "disable" : "enable"}" data-id="${esc(account.id)}">${account.enabled ? "禁用" : "启用"}</button>
              <button class="btn small danger" data-action="delete" data-id="${esc(account.id)}">删除</button>
            </div></td>
          </tr>`,
        )
        .join("")}</tbody></table></div>`;
  }

  async function handleAccountAction(action, id) {
    if (action === "detail") return openAccountDetail(id);
    if (action === "delete") return openDeleteAccount(id);
    if (action === "enable" || action === "disable") {
      try {
        await api(`/admin/api/accounts/${encodeURIComponent(id)}`, {
          method: "PATCH",
          body: JSON.stringify({ enabled: action === "enable" }),
        });
        toast(action === "enable" ? "账号已启用" : "账号已禁用", "ok");
        renderRoute();
      } catch (error) {
        toast(error.message, "error");
      }
    }
  }

  function openAddAccount() {
    const region = state.accountRegion;
    const provider = PROVIDERS[state.provider] || PROVIDERS.codearts;
    const modal = openModal({
      title: `添加 ${provider.label} 账号`,
      body: `
        <p class="muted">${esc(provider.siteNote)}</p>
        <label>站点</label>
        <div class="segmented" id="flow-region">
          <button data-region="cn" class="${region === "cn" ? "active" : ""}">CN 中国站</button>
          <button data-region="intl" class="${region === "intl" ? "active" : ""}" ${provider.cnLoginOnly ? "disabled title=\"TRAE 国际站授权尚未验证\"" : ""}>Intl 国际站</button>
        </div>
        ${state.provider === "qoder"
          ? `<p class="hint">Qoder Intl 授权流程尚未用真实账号实测；失败时请改用 CN 站点。</p>`
          : ""}
        ${provider.cnLoginOnly
          ? `<p class="hint">TRAE 仅 CN 站点提供已验证的授权入口；国际站凭据可在导入后刷新。</p>`
          : ""}
        ${provider.needsDeviceIds
          ? `<label>machineId（32 位十六进制）<input id="flow-machine" maxlength="32" spellcheck="false" placeholder="与登录浏览器一致的 machineId"></label>
             <label>deviceId（32 位十六进制）<input id="flow-device" maxlength="32" spellcheck="false" placeholder="与登录浏览器一致的 deviceId"></label>
             <p class="hint">PolyGate 不会生成设备标识，也不会修改浏览器指纹；这两个值只随该账号凭据保存。</p>`
          : ""}
        <label>账号名称（可选）<input id="flow-name" maxlength="64" placeholder="例如：主账号"></label>
        <div id="flow-status" class="muted"></div>`,
      actions: [
        { label: "取消", onClick: cancelFlowAndClose },
        { label: provider.startLabel, primary: true, onClick: () => startAuthFlow(modal) },
      ],
    });
    let selected = region;
    $$("#flow-region button", modal).forEach((button) =>
      button.addEventListener("click", () => {
        selected = button.dataset.region;
        $$("#flow-region button", modal).forEach((item) => item.classList.toggle("active", item === button));
      }),
    );
    modal.provider = provider;
    modal.flowRegion = () => selected;
  }

  async function startAuthFlow(modal) {
    const provider = modal.provider || PROVIDERS[state.provider] || PROVIDERS.codearts;
    const status = $("#flow-status", modal);
    const region = provider.cnLoginOnly ? "cn" : modal.flowRegion();
    const name = $("#flow-name", modal).value.trim();
    const body = { region, name: name || null };
    if (provider.needsDeviceIds) {
      const machine = $("#flow-machine", modal).value.trim();
      const device = $("#flow-device", modal).value.trim();
      const hex32 = /^[0-9a-fA-F]{32}$/;
      if (!hex32.test(machine) || !hex32.test(device)) {
        status.textContent = "machineId 与 deviceId 都必须是 32 位十六进制字符串。";
        return;
      }
      body.machine_id = machine;
      body.device_id = device;
    }
    status.textContent = "正在创建授权流程…";
    const popup = window.open("about:blank", `polygate-${provider.authPath}-auth`, "width=560,height=760");
    try {
      const flow = await api(`/admin/api/auth/${provider.authPath}/start`, {
        method: "POST",
        body: JSON.stringify(body),
      });
      state.flow = flow;
      state.flowKind = provider.authPath;
      state.flowPopup = popup;
      if (popup) {
        popup.location.href = flow.authorize_url;
      }
      const link = `<a href="${esc(flow.authorize_url)}" target="_blank" rel="noreferrer">打开授权页面</a>`;
      if (provider.pasteCallback) {
        status.innerHTML = `${provider.autoCheckNote || ""} ${link}
          <label>回调链接<input id="flow-callback-url" spellcheck="false" autocomplete="off" placeholder="授权完成后，从浏览器地址栏复制完整链接"></label>
          <button class="btn primary" type="button" id="flow-complete">完成授权</button>
          <div id="flow-paste-hint" class="muted" role="status"></div>`;
        $("#flow-complete").addEventListener("click", () =>
          completePasteFlow(flow.flow_id, provider),
        );
      } else {
        status.innerHTML = `${provider.autoCheckNote || "等待在官方页面完成授权…"} ${link}`;
      }
      pollFlow(flow.flow_id, provider);
    } catch (error) {
      if (popup) popup.close();
      status.textContent = error.message;
      toast(error.message, "error");
    }
  }

  function completePasteFlow(flowId, provider) {
    const input = $("#flow-callback-url");
    const hint = $("#flow-paste-hint");
    const url = input?.value.trim() || "";
    if (!url) {
      if (hint) hint.textContent = "请先粘贴授权完成后的回调链接。";
      return;
    }
    if (hint) hint.textContent = "正在完成授权…";
    api(`/admin/api/auth/${provider.authPath}/${encodeURIComponent(flowId)}/complete`, {
      method: "POST",
      body: JSON.stringify({ url }),
    })
      .then(() => {
        // The flow poll observes `succeeded` and closes the modal.
      })
      .catch((error) => {
        if (hint) hint.textContent = error.message;
        toast(error.message, "error");
      });
  }

  function pollFlow(flowId, provider) {
    clearInterval(state.flowTimer);
    const interval = provider.pollInterval || 1600;
    state.flowTimer = setInterval(async () => {
      try {
        const flow = await api(`/admin/api/auth/${provider.authPath}/${encodeURIComponent(flowId)}`);
        if (flow.status === "succeeded") {
          clearInterval(state.flowTimer);
          state.flowTimer = null;
          if (state.flowPopup && !state.flowPopup.closed) state.flowPopup.close();
          closeModal();
          toast(provider.successToast, "ok");
          state.accountRegion = flow.region || state.accountRegion;
          renderRoute();
        } else if (["failed", "cancelled", "expired"].includes(flow.status)) {
          clearInterval(state.flowTimer);
          state.flowTimer = null;
          const status = $("#flow-status");
          if (status) status.textContent = flow.error || `授权${flowStatusLabel(flow.status)}`;
          toast(flow.error || `授权${flowStatusLabel(flow.status)}`, "error");
        }
      } catch (error) {
        // Transient upstream failures are expected; keep polling until the flow
        // reports a terminal state. Only auth loss stops the loop.
        if (error.status === 401) {
          clearInterval(state.flowTimer);
          state.flowTimer = null;
        }
      }
    }, interval);
  }

  async function cancelFlowAndClose() {
    clearInterval(state.flowTimer);
    state.flowTimer = null;
    if (state.flowPopup && !state.flowPopup.closed) state.flowPopup.close();
    const flow = state.flow;
    const kind = state.flowKind;
    state.flow = null;
    state.flowKind = null;
    closeModal();
    if (flow && kind && ["pending", "exchanging"].includes(flow.status)) {
      try {
        await api(`/admin/api/auth/${kind}/${encodeURIComponent(flow.flow_id)}/cancel`, { method: "POST" });
      } catch (_) {
        // Flow may have completed or expired.
      }
    }
  }

  async function openAccountDetail(id) {
    let account;
    try {
      account = await api(`/admin/api/accounts/${encodeURIComponent(id)}`);
    } catch (error) {
      toast(error.message, "error");
      return;
    }
    const provider = PROVIDERS[account.kind] || PROVIDERS.codearts;
    const actions = [{ label: "关闭", onClick: closeModal }];
    if (supportsCheckin(provider, account.region)) {
      actions.push({ label: "执行签到", onClick: () => runCheckin(id) });
    }
    actions.push({ label: "检查认证", onClick: () => probeAccount(id) });
    actions.push({ label: "刷新额度", primary: true, onClick: () => refreshQuota(id) });
    const metadata = account.metadata || {};
    const details = [
      ["账号 ID", esc(account.id)],
      ["Provider", `${esc(provider.label)} · ${regionLabel(account.region)}`],
      ["认证状态", `${authBadge(account.auth_status)} ${account.auth_checked_at ? `<span class="tiny">${fmtDate(account.auth_checked_at)}</span>` : ""}`],
      ["启用状态", account.enabled ? "已启用" : "已禁用"],
      ["凭据", account.credential?.present ? `已加密保存 · revision ${esc(account.credential.revision ?? "—")}` : "未授权"],
      ["额度", quotaText(account.quota)],
      ...(supportsCheckin(provider, account.region)
        ? [["最近签到", account.last_checkin ? esc(checkinStatus(account.last_checkin.status).label) : "暂无"]]
        : []),
      ...(metadata.nickname ? [["昵称", esc(metadata.nickname)]] : []),
      ...(metadata.user_type ? [["账号类型", esc(metadata.user_type)]] : []),
      ...(metadata.organization_name ? [["组织", esc(metadata.organization_name)]] : []),
      ...(metadata.auth_source ? [["授权来源", esc(metadata.auth_source)]] : []),
      ["外部身份", esc(account.external_id || "—")],
      ["更新时间", fmtDate(account.updated_at)],
    ];
    openModal({
      title: account.name,
      body: `
        <dl class="kv">
          ${details.map(([key, value]) => `<dt>${esc(key)}</dt><dd>${value}</dd>`).join("")}
        </dl>
        <h3>非敏感元数据</h3>
        <pre class="json">${esc(JSON.stringify(account.metadata || {}, null, 2))}</pre>`,
      actions,
    });
  }

  async function runCheckin(id) {
    toast("正在执行今日签到…");
    try {
      const result = await api(`/admin/api/accounts/${encodeURIComponent(id)}/checkin`, {
        method: "POST",
      });
      toast(result.summary || checkinStatus(result.status).label, result.status === "failed" ? "error" : "ok");
      closeModal();
      renderRoute();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function probeAccount(id) {
    toast("正在请求上游检查认证状态…");
    try {
      const result = await api(`/admin/api/accounts/${encodeURIComponent(id)}/auth-status`, {
        method: "POST",
        body: JSON.stringify({ probe: true }),
      });
      toast(`认证状态：${authLabel(result.auth_status)}`, "ok");
      closeModal();
      renderRoute();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function refreshQuota(id) {
    toast("正在刷新额度…");
    try {
      const result = await api(`/admin/api/accounts/${encodeURIComponent(id)}/quota`, { method: "POST" });
      toast(result.unknown ? "上游未提供额度，保持未知" : "额度已刷新", "ok");
      closeModal();
      renderRoute();
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function openDeleteAccount(id) {
    openModal({
      title: "删除账号",
      body: `
        <p>删除后该账号的凭据、会话绑定、额度和签到记录会一并删除，无法撤销。</p>
        <p class="muted">请输入账号 ID 确认：<code>${esc(id)}</code></p>
        <label>账号 ID<input id="delete-confirm" autocomplete="off"></label>`,
      actions: [
        { label: "取消", onClick: closeModal },
        {
          label: "永久删除",
          danger: true,
          onClick: async () => {
            const value = $("#delete-confirm").value.trim();
            try {
              await api(`/admin/api/accounts/${encodeURIComponent(id)}?confirm=${encodeURIComponent(value)}`, {
                method: "DELETE",
              });
              closeModal();
              toast("账号已删除", "ok");
              renderRoute();
            } catch (error) {
              toast(error.message, "error");
            }
          },
        },
      ],
    });
  }

  // ---------------------------------------------------------------- read-only views

  async function renderModels(container) {
    const data = await api("/admin/api/model-routes");
    const aliases = data.aliases || [];
    const native = data.native || [];
    const providerIds = [...new Set(native.map((model) => model.provider))];

    container.innerHTML = `
      <div class="section-head">
        <h2>模型别名</h2>
        <div class="topbar-actions">
          <span class="muted">${data.discovered_at ? `目录发现于 ${fmtDate(data.discovered_at)}` : "尚未发现模型"}</span>
          <button class="btn primary small" id="model-add" ${providerIds.length ? "" : "disabled"}>新建别名</button>
        </div>
      </div>
      <p class="muted">别名对客户端可见；同一别名可挂多个 Provider/上游模型目标。原生模型 ID 始终可用，别名不能与原生 ID 重名。同名上游模型来自多个 Provider 时需使用 <code>provider::model</code> 或建立别名。</p>
      ${aliases.length
        ? `<div class="table-wrap"><table><thead><tr><th>别名</th><th>目标</th><th>优先级 / 权重</th><th>能力</th><th>状态</th><th></th></tr></thead><tbody>
          ${aliases
            .map(
              (alias) => `<tr>
                <td><span class="strong">${esc(alias.alias)}</span><div class="tiny">${esc(alias.display_name || "")}</div></td>
                <td>
                  ${alias.targets
                    .map(
                      (target) => `<div class="tiny">
                        <span class="strong">${esc(target.provider)}</span> · ${esc(target.upstream_model)}
                        ${target.provider_enabled ? "" : '<span class="badge muted">Provider 禁用</span>'}
                        ${target.registered ? "" : '<span class="badge warn">未注册</span>'}
                      </div>`,
                    )
                    .join("")}
                </td>
                <td>${alias.targets
                  .map((target) => `<div class="tiny">P${target.priority} · W${target.weight}</div>`)
                  .join("")}</td>
                <td>${alias.targets
                  .map((target) => `<div class="tiny">${capabilityBadges(target.capabilities)}</div>`)
                  .join("")}</td>
                <td>${alias.enabled ? '<span class="badge ok">启用</span>' : '<span class="badge muted">全部禁用</span>'}</td>
                <td class="tiny">
                  ${alias.targets
                    .map(
                      (target) => `<div class="row-actions">
                        <button class="btn ghost small" data-edit="${esc(target.id)}">编辑</button>
                        <button class="btn ghost small" data-toggle="${esc(target.id)}" data-enabled="${target.enabled}">${target.enabled ? "禁用" : "启用"}</button>
                        <button class="btn ghost small danger" data-delete="${esc(target.id)}">删除</button>
                      </div>`,
                    )
                    .join("")}
                </td>
              </tr>`,
            )
            .join("")}
        </tbody></table></div>`
        : `<div class="empty"><strong>尚无别名</strong>建立别名后可以按优先级/权重在多个 Provider 之间路由。</div>`}
      <div class="section-head"><h2>原生模型</h2><span class="muted">启动时从已启用 Provider 发现</span></div>
      ${native.length
        ? `<div class="table-wrap"><table><thead><tr><th>模型 ID</th><th>Provider</th><th>Region</th><th>上下文</th><th>可用</th></tr></thead><tbody>
          ${native
            .map(
              (model) => `<tr>
                <td><span class="strong">${esc(model.id)}</span><div class="tiny">${esc(model.display_name || "")}</div></td>
                <td>${esc(model.provider)}<div class="tiny">${esc(model.kind)}</div></td>
                <td>${regionLabel(model.region)}</td>
                <td>${model.context_window ? formatTokens(model.context_window) : "—"}</td>
                <td>${model.available ? '<span class="badge ok">可用</span>' : '<span class="badge muted">不可用</span>'}</td>
              </tr>`,
            )
            .join("")}
        </tbody></table></div>`
        : `<div class="empty"><strong>尚未发现模型</strong>模型目录在启动时从已启用且有可用账号的 Provider 读取。</div>`}`;

    const targets = aliases.flatMap((alias) =>
      alias.targets.map((target) => ({ ...target, alias: alias.alias })),
    );

    $("#model-add")?.addEventListener("click", () => {
      openModelCreate(native, providerIds);
    });
    $$("[data-toggle]").forEach((button) =>
      button.addEventListener("click", async () => {
        const enabled = button.dataset.enabled !== "true";
        try {
          await api(`/admin/api/model-routes/${encodeURIComponent(button.dataset.toggle)}`, {
            method: "PATCH",
            body: JSON.stringify({ enabled }),
          });
          toast(enabled ? "目标已启用" : "目标已禁用", "ok");
          renderRoute();
        } catch (error) {
          toast(error.message, "error");
        }
      }),
    );
    $$("[data-delete]").forEach((button) =>
      button.addEventListener("click", async () => {
        if (!window.confirm("删除该路由目标？使用该别名的客户端会立即受影响。")) return;
        try {
          await api(`/admin/api/model-routes/${encodeURIComponent(button.dataset.delete)}`, {
            method: "DELETE",
          });
          toast("目标已删除", "ok");
          renderRoute();
        } catch (error) {
          toast(error.message, "error");
        }
      }),
    );
    $$("[data-edit]").forEach((button) =>
      button.addEventListener("click", () => {
        const target = targets.find((item) => item.id === button.dataset.edit);
        if (target) openModelEdit(target);
      }),
    );
  }

  function capabilityBadges(caps) {
    const source = caps && typeof caps === "object" ? caps : {};
    const labels = { streaming: "流式", tools: "工具", vision: "视觉", reasoning: "推理" };
    const parts = Object.entries(labels).map(([key, label]) => {
      const value = source[key];
      if (value === true) return `<span class="badge ok">${label}</span>`;
      if (value === false) return `<span class="badge muted">无${label}</span>`;
      return `<span class="badge muted">${label} 继承</span>`;
    });
    return parts.join(" ");
  }

  function capabilitySelects(prefix, caps = {}) {
    const labels = { streaming: "流式", tools: "工具调用", vision: "视觉输入", reasoning: "推理" };
    return `<div class="cap-grid">${Object.entries(labels)
      .map(
        ([key, label]) => `<label>${label}
          <select data-cap="${key}" id="${prefix}-${key}">
            <option value="" ${caps[key] === undefined || caps[key] === null ? "selected" : ""}>继承 Provider</option>
            <option value="true" ${caps[key] === true ? "selected" : ""}>强制支持</option>
            <option value="false" ${caps[key] === false ? "selected" : ""}>不支持</option>
          </select>
        </label>`,
      )
      .join("")}</div>`;
  }

  function readCapabilities(root) {
    const caps = {};
    $$("[data-cap]", root).forEach((select) => {
      if (select.value === "true") caps[select.dataset.cap] = true;
      if (select.value === "false") caps[select.dataset.cap] = false;
    });
    return caps;
  }

  function openModelCreate(native, providerIds) {
    const dedup = [...new Set(native.map((model) => model.id))];
    const modal = openModal({
      title: "新建模型别名",
      body: `
        <label>别名<input id="model-alias" placeholder="例如 coding-fast" maxlength="64"></label>
        <label>目标 Provider
          <select id="model-provider">
            ${providerIds.map((id) => `<option value="${esc(id)}">${esc(id)}</option>`).join("")}
          </select>
        </label>
        <label>上游模型<input id="model-upstream" list="native-models" placeholder="上游模型 ID"></label>
        <datalist id="native-models">${dedup.map((id) => `<option value="${esc(id)}"></option>`).join("")}</datalist>
        <label>显示名（可选）<input id="model-display" maxlength="120"></label>
        <div class="grid two">
          <label>优先级（越大越先）<input id="model-priority" type="number" value="0" step="1"></label>
          <label>权重（≥1）<input id="model-weight" type="number" value="1" min="1" step="1"></label>
        </div>
        ${capabilitySelects("model-cap")}
        <p class="hint">不选择能力覆盖时沿用 Provider 声明；网关不会把工具/视觉请求路由到未声明的目标。</p>`,
      actions: [
        { label: "取消", onClick: closeModal },
        {
          label: "创建",
          primary: true,
          onClick: async () => {
            try {
              await api("/admin/api/model-routes", {
                method: "POST",
                body: JSON.stringify({
                  alias: $("#model-alias", modal).value.trim(),
                  provider: $("#model-provider", modal).value,
                  upstream_model: $("#model-upstream", modal).value.trim(),
                  display_name: $("#model-display", modal).value.trim() || null,
                  priority: Number($("#model-priority", modal).value || 0),
                  weight: Number($("#model-weight", modal).value || 1),
                  capabilities: readCapabilities(modal),
                }),
              });
              closeModal();
              toast("别名目标已创建", "ok");
              renderRoute();
            } catch (error) {
              toast(error.message, "error");
            }
          },
        },
      ],
    });
  }

  function openModelEdit(target) {
    const modal = openModal({
      title: `编辑目标 · ${target.alias}`,
      body: `
        <p class="muted">${esc(target.provider)} · ${esc(target.upstream_model)}</p>
        <label>显示名<input id="edit-display" maxlength="120" value="${esc(target.display_name || "")}"></label>
        <div class="grid two">
          <label>优先级<input id="edit-priority" type="number" step="1" value="${Number(target.priority || 0)}"></label>
          <label>权重（≥1）<input id="edit-weight" type="number" min="1" step="1" value="${Number(target.weight || 1)}"></label>
        </div>
        ${capabilitySelects("edit-cap", target.capabilities || {})}
        <label>状态
          <select id="edit-enabled">
            <option value="true" ${target.enabled ? "selected" : ""}>启用</option>
            <option value="false" ${target.enabled ? "" : "selected"}>禁用</option>
          </select>
        </label>`,
      actions: [
        { label: "取消", onClick: closeModal },
        {
          label: "保存",
          primary: true,
          onClick: async () => {
            try {
              await api(`/admin/api/model-routes/${encodeURIComponent(target.id)}`, {
                method: "PATCH",
                body: JSON.stringify({
                  display_name: $("#edit-display", modal).value.trim(),
                  priority: Number($("#edit-priority", modal).value || 0),
                  weight: Number($("#edit-weight", modal).value || 1),
                  enabled: $("#edit-enabled", modal).value === "true",
                  capabilities: readCapabilities(modal),
                }),
              });
              closeModal();
              toast("目标已更新", "ok");
              renderRoute();
            } catch (error) {
              toast(error.message, "error");
            }
          },
        },
      ],
    });
  }

  async function renderLogs(container) {
    const data = await api("/admin/api/logs?limit=200");
    const items = data.items || [];
    container.innerHTML = `
      <div class="section-head">
        <h2>请求日志</h2>
        <span class="muted">仅记录路由元数据；不保存提示词、消息内容、工具参数或凭据</span>
      </div>
      <div class="card">
        <h2>清理策略</h2>
        <div class="inline-form">
          <label>保留天数<input id="log-days" type="number" min="1" value="30"></label>
          <button class="btn small" id="log-purge">清理更早记录</button>
          <button class="btn small danger" id="log-clear">清空全部</button>
        </div>
        <p class="hint">建议保留 7–30 天；开启自动清理需要外部定时任务调用清理接口。</p>
      </div>
      ${items.length
        ? `<div class="table-wrap"><table><thead><tr><th>时间</th><th>Provider / 账号</th><th>模型</th><th>API</th><th>状态</th><th>错误</th><th>延迟</th><th>Tokens</th></tr></thead><tbody>
          ${items
            .map(
              (item) => `<tr>
                <td class="tiny">${fmtDate(item.created_at)}</td>
                <td>${esc(item.provider || item.provider_kind)}<div class="tiny">${esc(item.account_id ? item.account_id.slice(0, 8) : "—")} · ${regionLabel(item.region)}</div></td>
                <td>${esc(item.model || "—")}<div class="tiny">${esc(item.upstream_model || "")}</div></td>
                <td class="tiny">${esc(item.api)}${item.stream ? " · 流式" : ""}</td>
                <td>${item.http_status < 400 ? `<span class="badge ok">${item.http_status}</span>` : `<span class="badge bad">${item.http_status}</span>`}</td>
                <td class="tiny">${esc(item.error_category || "—")}</td>
                <td class="tiny">${item.latency_ms} ms</td>
                <td class="tiny">${formatTokens(item.input_tokens)} / ${formatTokens(item.output_tokens)}</td>
              </tr>`,
            )
            .join("")}
        </tbody></table></div>`
        : `<div class="empty"><strong>暂无请求日志</strong>通过 /v1 调用后会在这里出现（不含请求内容）。</div>`}`;

    $("#log-purge")?.addEventListener("click", async () => {
      const days = Number($("#log-days").value || 0);
      if (days <= 0) return toast("请输入大于 0 的天数", "error");
      try {
        const result = await api("/admin/api/logs/purge", {
          method: "POST",
          body: JSON.stringify({ older_than_days: days }),
        });
        toast(`已清理 ${result.deleted} 条记录`, "ok");
        renderRoute();
      } catch (error) {
        toast(error.message, "error");
      }
    });
    $("#log-clear")?.addEventListener("click", async () => {
      if (!window.confirm("清空全部请求日志？此操作不可恢复。")) return;
      try {
        const result = await api("/admin/api/logs/purge", {
          method: "POST",
          body: JSON.stringify({ all: true }),
        });
        toast(`已删除 ${result.deleted} 条记录`, "ok");
        renderRoute();
      } catch (error) {
        toast(error.message, "error");
      }
    });
  }


  async function renderCheckins(container) {
    const data = await api("/admin/api/checkins");
    container.innerHTML = `
      <p class="muted">本页面只读展示签到记录；单个账号的签到现在“账号管理 → 详情 → 执行签到”中触发，每个账号每天最多执行一次。</p>
      ${data.items.length
        ? `<div class="table-wrap"><table><thead><tr><th>账号</th><th>Provider</th><th>支持签到</th><th>最近签到</th><th>结果</th><th>时间</th></tr></thead><tbody>
          ${data.items
            .map(
              (item) => `<tr>
                <td><span class="strong">${esc(item.account_name)}</span><div class="tiny">${esc(item.account_id.slice(0, 8))}</div></td>
                <td>${esc(item.kind)} · ${regionLabel(item.region)}</td>
                <td>${item.supported ? "是" : "否"}</td>
                <td>${item.last ? checkinStatus(item.last.status).badge : '<span class="badge muted">暂无</span>'}</td>
                <td class="tiny">${esc(item.last?.summary || "—")}</td>
                <td class="tiny">${item.last ? fmtDate(item.last.completed_at) : "—"}</td>
              </tr>`,
            )
            .join("")}
        </tbody></table></div>`
        : `<div class="empty"><strong>暂无账号</strong>添加账号后才能展示签到记录。</div>`}`;
  }

  async function renderUsage(container) {
    const data = await api("/admin/api/usage");
    const summary = data.summary;
    container.innerHTML = `
      <div class="grid cards">
        ${statCard("请求数", String(summary.requests), "全部已记录请求")}
        ${statCard("输入 Tokens", formatTokens(summary.input_tokens), "来自上游 usage")}
        ${statCard("输出 Tokens", formatTokens(summary.output_tokens), "来自上游 usage")}
        ${statCard("记录条数", String(data.items.length), "最近 20 条")}
      </div>
      ${data.items.length
        ? `<div class="table-wrap"><table><thead><tr><th>时间</th><th>Provider</th><th>模型</th><th>状态</th><th>输入</th><th>输出</th></tr></thead><tbody>
          ${data.items
            .map(
              (item) => `<tr><td class="tiny">${fmtDate(item.created_at)}</td><td>${esc(item.provider_kind)}</td>
                <td>${esc(item.model || "—")}</td><td>${esc(item.status)}</td>
                <td>${formatTokens(item.input_tokens)}</td><td>${formatTokens(item.output_tokens)}</td></tr>`,
            )
            .join("")}
        </tbody></table></div>`
        : `<div class="empty"><strong>暂无用量</strong>通过 /v1/chat/completions 调用后会记录在这里。</div>`}`;
  }

  async function renderSettings(container) {
    const bootstrap = state.bootstrap || {};
    container.innerHTML = `
      <div class="grid two">
        <div class="card">
          <h2>实例</h2>
          <dl class="kv">
            <dt>版本</dt><dd>${esc(bootstrap.version || "—")}</dd>
            <dt>管理员</dt><dd>${esc(state.user || "admin")}</dd>
            <dt>CodeArts 回调</dt><dd>${esc(bootstrap.oauth_callback_url || "—")}</dd>
            <dt>初始化限制</dt><dd>${bootstrap.setup_local_only ? "仅本机 loopback" : "允许远程（需自行保证 HTTPS）"}</dd>
          </dl>
        </div>
        <div class="card">
          <h2>安全</h2>
          <p class="muted">管理员会话使用 HttpOnly + SameSite=Strict Cookie；修改请求校验 CSRF 与 Origin。管理 API 与 /v1 API Key 完全分离。</p>
          <p class="muted">凭据只经 SecretBox（AES-256-GCM）加密保存；浏览器不会收到已保存的完整凭据。</p>
        </div>
      </div>
      <div class="card">
        <h2>接入状态</h2>
        <p class="muted">CodeArts 具备真实 Adapter，可授权并参与模型发现、对话与签到。</p>
        <p class="muted">Qoder 已注册为 Provider：设备码授权、模型发现、额度、对话/流式/工具调用与签到均已实现；CN 账号已验证授权、模型、额度、签到与对话。</p>
        <p class="muted">TRAE 与 WorkBuddy 已接入授权、额度、签到与对话/流式/工具调用，均只做 Mock 验证，尚无真实账号实测；TRAE 授权需要你自行提供 machineId / deviceId。</p>
      </div>`;
  }

  // ---------------------------------------------------------------- helpers

  const AUTH_LABELS = {
    valid: ["有效", "ok"],
    unknown: ["未探测", "muted"],
    expired: ["已过期", "warn"],
    refresh_required: ["需刷新", "warn"],
    invalid: ["无效", "bad"],
    restricted: ["受限", "bad"],
  };

  function authLabel(status) {
    return AUTH_LABELS[status]?.[0] || status || "未知";
  }

  function authBadge(status) {
    const [label, tone] = AUTH_LABELS[status] || [status || "未知", "muted"];
    return `<span class="badge ${tone}">${esc(label)}</span>`;
  }

  function regionLabel(region) {
    return region === "intl" ? "Intl" : region === "cn" ? "CN" : esc(region || "—");
  }

  function strategyLabel(strategy) {
    return (
      {
        round_robin: "轮询",
        smooth_weighted_round_robin: "加权轮询",
        least_connections: "最少连接",
      }[strategy] || esc(strategy || "—")
    );
  }

  function quotaText(quota) {
    if (!quota) return '<span class="badge muted">未知</span>';
    if (quota.status === "unknown" || quota.remaining === null || quota.remaining === undefined) {
      return '<span class="badge muted">未知</span>';
    }
    const unit = { credits: "积分", tokens: "tokens", requests: "次" }[quota.unit] || "未知单位";
    const total = quota.total !== null && quota.total !== undefined ? ` / ${quota.total}` : "";
    return `<span class="strong">${esc(quota.remaining)}${total}</span> <span class="tiny">${esc(unit)}</span>`;
  }

  const CHECKIN_LABELS = {
    succeeded: ["已签到", "ok"],
    already_checked_in: ["今日已签", "ok"],
    failed: ["失败", "bad"],
    unknown: ["结果未知", "warn"],
    pending: ["等待中", "muted"],
    running: ["执行中", "warn"],
    unsupported: ["不支持", "muted"],
  };

  function checkinStatus(status) {
    const [label, tone] = CHECKIN_LABELS[status] || [status || "未知", "muted"];
    return { label, badge: `<span class="badge ${tone}">${esc(label)}</span>` };
  }

  function flowStatusLabel(status) {
    return { failed: "失败", cancelled: "已取消", expired: "已过期" }[status] || status;
  }

  // ---------------------------------------------------------------- modal

  function openModal({ title, body, actions = [], dismissible = true }) {
    closeModal();
    const root = $("#modal-root");
    const wrapper = document.createElement("div");
    wrapper.className = "modal-backdrop";
    wrapper.innerHTML = `
      <div class="modal" role="dialog" aria-modal="true">
        <h2>${esc(title)}</h2>
        <div class="modal-body">${body}</div>
        <div class="modal-actions"></div>
      </div>`;
    const actionsRoot = $(".modal-actions", wrapper);
    actions.forEach((action) => {
      const button = document.createElement("button");
      button.className = `btn ${action.primary ? "primary" : ""} ${action.danger ? "danger" : ""}`.trim();
      button.textContent = action.label;
      button.addEventListener("click", action.onClick);
      actionsRoot.appendChild(button);
    });
    if (dismissible) {
      wrapper.addEventListener("click", (event) => {
        if (event.target === wrapper) closeModal();
      });
    }
    root.appendChild(wrapper);
    const first = $("input, select, button.primary", wrapper);
    if (first) first.focus();
    return wrapper;
  }

  function closeModal() {
    $("#modal-root").innerHTML = "";
  }

  // ---------------------------------------------------------------- theme

  function applyTheme(theme) {
    const value = theme === "light" ? "light" : "dark";
    document.documentElement.dataset.theme = value;
    localStorage.setItem("polygate.theme", value);
  }

  function toggleTheme() {
    applyTheme(document.documentElement.dataset.theme === "light" ? "dark" : "light");
  }

  // ---------------------------------------------------------------- wiring

  document.addEventListener("DOMContentLoaded", () => {
    $("#setup-form").addEventListener("submit", submitSetup);
    $("#login-form").addEventListener("submit", submitLogin);
    $("#recover-form").addEventListener("submit", submitRecover);
    $("#show-recover").addEventListener("click", () => showAuth("recover"));
    $("#back-login").addEventListener("click", () => showAuth("login"));
    $("#logout").addEventListener("click", logout);
    $("#theme-toggle").addEventListener("click", toggleTheme);
    $$(".nav-item[data-view]").forEach((item) =>
      item.addEventListener("click", () => navigate(item.dataset.view)),
    );
    $$(".subnav-item[data-provider]").forEach((item) =>
      item.addEventListener("click", () => {
        state.provider = item.dataset.provider;
        navigate("accounts");
      }),
    );
    window.addEventListener("hashchange", () => {
      if (state.csrf) renderRoute();
    });
    window.addEventListener("message", (event) => {
      if (event.origin !== window.location.origin) return;
      const payload = event.data;
      if (!payload || payload.type !== "polygate:oauth" || !state.flow) return;
      if (payload.flowId && payload.flowId !== state.flow.flow_id) return;
      if (payload.success) {
        const provider = PROVIDERS[state.flowKind] || PROVIDERS.codearts;
        clearInterval(state.flowTimer);
        state.flowTimer = null;
        closeModal();
        toast(provider.successToast, "ok");
        renderRoute();
      }
    });
    boot();
  });
})();
