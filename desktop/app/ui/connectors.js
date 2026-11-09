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

  // The logos of the apps (the same files as the phone's Apps screen); an app without one shows its first letter.
  const LOGOS = { google: "google.svg", notion: "notion.svg", slack: "slack.png", github: "github.svg", gitlab: "gitlab.svg", linear: "linear.svg", jira: "jira.svg", asana: "asana.svg",
    dropbox: "dropbox.svg", microsoft: "microsoft.svg", x: "x.svg", reddit: "reddit.png", linkedin: "linkedin.svg" };

  function logo(provider, size = "") {
    const box = el("span", `app-logo ${size}`);
    if (LOGOS[provider.id]) {
      const image = document.createElement("img");
      image.src = `img/apps/${LOGOS[provider.id]}`;
      image.alt = "";
      image.onerror = () => { image.remove(); box.textContent = provider.name.slice(0, 1); };
      box.append(image);
    } else box.textContent = provider.name.slice(0, 1);
    return box;
  }

  // Tapping a connected app: its account, read-only or read and write, reconnect, disconnect (as the phone's long-press menu).
  function sheet(provider, connection, needsReconnect, missing) {
    const modal = el("div", "modal");
    const card = el("div", "modal-card app-sheet");
    const close = () => modal.remove();
    modal.onclick = (event) => { if (event.target === modal) close(); };
    const head = el("div", "app-sheet-head");
    head.append(logo(provider, "big"));
    const titles = el("div");
    titles.append(el("div", "w-title", provider.name));
    titles.append(el("div", "muted small", needsReconnect ? (missing.length ? "Bazı erişimler eksik" : "Bağlantının süresi doldu") : connection.accountLabel || "Bağlı"));
    head.append(titles);
    card.append(head);
    const level = el("select", "app-level");
    for (const [value, label] of [["write", "Oku ve yaz"], ["read", "Sadece oku"]]) {
      const option = el("option", "", label);
      option.value = value;
      option.selected = (connection.accessLevel || "write") === value;
      level.append(option);
    }
    level.onchange = () => { close(); run(provider.id, () => invoke("connector_access", { provider: provider.id, level: level.value })); };
    const field = el("label", "app-sheet-field");
    field.append(el("span", "muted small", "Erişim"), level);
    card.append(field);
    const actions = el("div", "modal-actions");
    const cut = el("button", "secondary danger", "Bağlantıyı kes");
    cut.onclick = () => { close(); run(provider.id, () => invoke("connector_disconnect", { provider: provider.id })); };
    actions.append(cut, el("span", "spacer"));
    if (needsReconnect) {
      const again = el("button", "primary", "Yeniden bağla");
      again.onclick = () => { close(); run(provider.id, () => invoke("connector_connect", { provider: provider.id })); };
      actions.append(again);
    }
    const done = el("button", needsReconnect ? "secondary" : "primary", "Tamam");
    done.onclick = close;
    actions.append(done);
    card.append(actions);
    modal.append(card);
    document.body.append(modal);
  }

  function row(provider, data) {
    const connection = data.connections.find((item) => item.provider === provider.id);
    const missing = data.capabilities.filter((item) => item.owner === provider.id && item.status === "needs_scope");
    const needsReconnect = connection?.status === "needs_reconnect" || missing.length > 0;
    const tile = el("button", `app-tile${connection ? (needsReconnect ? " warn" : " on") : ""}`);
    tile.type = "button";
    tile.append(logo(provider));
    tile.append(el("span", "app-name", provider.name));
    tile.append(el("span", "app-state", busy === provider.id ? "…" : connection ? (needsReconnect ? "Yeniden bağla" : connection.accountLabel || "Bağlı") : "Bağla"));
    if (connection) tile.append(el("span", "app-dot"));
    if (busy === provider.id) tile.classList.add("busy");
    tile.onclick = () => {
      if (busy) return;
      if (connection) sheet(provider, connection, needsReconnect, missing);
      else run(provider.id, () => invoke("connector_connect", { provider: provider.id }));
    };
    return tile;
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

  // The chat's app tiles (render.js, app_suggestion) wear the same logos.
  window.WiqoApps = { logo };

  window.addEventListener("wiqo-section", (event) => { if (event.detail === "apps") load(); });
  window.addEventListener("focus", () => { if (document.body.dataset.section === "apps") load(); });
  listen("connector-result", (message) => {
    const { status, message: reason } = message.payload;
    problem(status === "error" ? reason || "Bağlantı tamamlanamadı." : "");
    load();
  });
})();
