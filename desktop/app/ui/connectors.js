// "Uygulamalar": the accounts Wiqo may use for the person (Google, Notion, ...): connect in the browser, see which account, read-only or read and write, disconnect.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const $ = (id) => document.getElementById(id);
  let busy = null;

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };

  function problem(message) {
    $("apps-error").hidden = !message;
    $("apps-error").textContent = message || "";
  }

  async function run(provider, action) {
    busy = provider;
    problem("");
    render(last);
    try {
      await action();
    } catch (error) {
      problem(String(error));
    }
    await load();
  }

  let last = null;

  function row(provider, data) {
    const connection = data.connections.find((item) => item.provider === provider.id);
    const missing = data.capabilities.filter((item) => item.owner === provider.id && item.status === "needs_scope");
    const needsReconnect = connection?.status === "needs_reconnect" || missing.length > 0;
    const item = el("div", "app-row");
    const text = el("div", "app-text");
    text.append(el("div", "w-title", provider.name));
    text.append(el("div", "muted small", connection
      ? needsReconnect ? (missing.length ? "Bazı erişimler eksik" : "Bağlantının süresi doldu") : connection.accountLabel || "Bağlı"
      : provider.summary || "Bağlı değil"));
    item.append(text);
    const actions = el("div", "app-actions");
    if (busy === provider.id) actions.append(el("div", "spinner small-spinner"));
    else if (connection) {
      const level = el("select", "app-level");
      for (const [value, label] of [["write", "Oku ve yaz"], ["read", "Sadece oku"]]) {
        const option = el("option", "", label);
        option.value = value;
        option.selected = (connection.accessLevel || "write") === value;
        level.append(option);
      }
      level.onchange = () => run(provider.id, () => invoke("connector_access", { provider: provider.id, level: level.value }));
      actions.append(level);
      if (needsReconnect) {
        const again = el("button", "primary small", "Yeniden bağla");
        again.onclick = () => run(provider.id, () => invoke("connector_connect", { provider: provider.id }));
        actions.append(again);
      }
      const cut = el("button", "secondary small", "Kes");
      cut.onclick = () => run(provider.id, () => invoke("connector_disconnect", { provider: provider.id }));
      actions.append(cut);
    } else {
      const connect = el("button", "primary small", "Bağla");
      connect.onclick = () => run(provider.id, () => invoke("connector_connect", { provider: provider.id }));
      actions.append(connect);
    }
    item.append(actions);
    return item;
  }

  function render(data) {
    if (!data) return;
    const list = $("apps-list");
    list.replaceChildren();
    const usable = data.providers.filter((provider) => provider.available !== false || data.connections.some((item) => item.provider === provider.id))
      .sort((a, b) => (data.connections.some((item) => item.provider === b.id) ? 1 : 0) - (data.connections.some((item) => item.provider === a.id) ? 1 : 0));
    for (const provider of usable) list.append(row(provider, data));
    const soon = data.providers.filter((provider) => provider.available === false && !data.connections.some((item) => item.provider === provider.id)).map((provider) => provider.name);
    $("apps-soon").hidden = soon.length === 0;
    $("apps-soon").textContent = soon.length ? `Yakında: ${soon.join(", ")}` : "";
  }

  async function load() {
    try {
      last = await invoke("connectors");
      busy = null;
      render(last);
    } catch (error) {
      busy = null;
      problem(String(error));
    }
  }

  window.addEventListener("wiqo-section", (event) => { if (event.detail === "apps") load(); });
  window.addEventListener("focus", () => { if (document.body.dataset.section === "apps") load(); });
  listen("connector-result", (message) => {
    const { status, message: reason } = message.payload;
    problem(status === "error" ? reason || "Bağlantı tamamlanamadı." : "");
    load();
  });
})();
