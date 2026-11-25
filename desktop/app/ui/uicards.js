// Cards (backend schema/ui.ts): the same cards iOS (WiqoUiCards.swift) and Android (UiCards.tsx) draw, from the same schema (schema.generated.js).
// A card is a flat list of nodes from `root`; every node type the schema lists is drawn here, an unknown one shows its fallback text. Everything the
// model wrote goes in as text; the only HTML a card may carry (an `html` node) runs in a sandboxed frame with no network.
(() => {
  const R = () => window.WiqoRender;
  const icon = (name, size) => window.WiqoIcons.icon(name, size);
  const SCHEMA = window.WiqoSchema || { uiNodeTypes: [], uiActionTypes: [] };
  const PLOT = window.WiqoPlotExpr;

  /** What this window tells the server it can draw and run (the schema's own lists, as the phones send theirs). */
  const ACTIONS = ["send", "event", "open_url", "copy", "share", "dial", "open_map", "stop_task", "retry"].filter((type) => SCHEMA.uiActionTypes.includes(type));
  const capabilities = { version: 1, nodes: SCHEMA.uiNodeTypes.filter((type) => type !== "approval"), actions: ACTIONS, html: true, presentations: ["inline"] };

  const GAP = { none: 0, xs: 4, sm: 8, md: 12, lg: 18, xl: 26 };
  const ICON = {
    file: "doc.text", document: "doc.text", spreadsheet: "tablecells", presentation: "play.rectangle", pdf: "doc.text", image: "photo", video: "play.rectangle", code: "chevron.left.forwardslash.chevron.right",
    folder: "folder", link: "link", share: "link", copy: "doc.on.doc", check: "checkmark", close: "xmark", warning: "exclamationmark.triangle", error: "exclamationmark.circle.fill", info: "info.circle",
    question: "questionmark.circle", lock: "lock.shield", search: "magnifyingglass", add: "plus", refresh: "arrow.clockwise", clock: "timer", calendar: "calendar", alarm: "timer", mail: "envelope",
    person: "person.crop.circle", location: "location", map: "map", computer: "laptopcomputer", mobile: "iphone", sun: "sun.max.fill", cloud: "cloud.fill", rain: "cloud.rain.fill", snow: "cloud.snow.fill",
    bolt: "bolt", sparkles: "sparkle", money: "dollarsign.circle", chart: "chart.bar", list: "list.bullet", terminal: "terminal", globe: "safari", settings: "gearshape", arrow_up: "arrow.up", arrow_right: "chevron.right",
  };
  const SERIES = ["#3b6fd8", "#d97706", "#0e9f6e", "#9b5de5", "#e0457b", "#0891b2"];
  const TONE = { success: "var(--success-text)", warning: "var(--warning-text)", danger: "var(--danger-text)", info: "var(--link)", accent: "var(--link)", muted: "var(--text2)" };
  const toneColor = (tone) => TONE[tone] || "var(--ink)";

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }
  const svgNS = "http://www.w3.org/2000/svg";
  function svg(tag, attrs, text) {
    const node = document.createElementNS(svgNS, tag);
    for (const [key, value] of Object.entries(attrs || {})) node.setAttribute(key, String(value));
    if (text !== undefined) node.textContent = text;
    return node;
  }
  const https = (url) => (typeof url === "string" && /^https:\/\//i.test(url) ? url : null);
  const pictureUrl = (url) => (typeof url === "string" && /^(https:|data:image\/)/i.test(url) ? url : null);
  const hostOf = (url) => { try { return new URL(url).hostname.replace(/^www\./, ""); } catch { return url; } };
  const number = (value, unit) => {
    const abs = Math.abs(value);
    const text = abs >= 1e6 ? `${(value / 1e6).toLocaleString("tr-TR", { maximumFractionDigits: 1 })} Mn` : abs >= 1e4 ? `${(value / 1e3).toLocaleString("tr-TR", { maximumFractionDigits: 1 })} B` : value.toLocaleString("tr-TR", { maximumFractionDigits: abs < 10 ? 2 : 1 });
    if (!unit) return text;
    return ["₺", "$", "€"].includes(unit) ? `${unit}${text}` : `${text}${/^[%°]/.test(unit) ? "" : " "}${unit}`;
  };

  /** One card. `act(action, nodeId, values)` runs what a tap asks for (the chat decides how). */
  function card(doc, act) {
    const nodes = new Map((doc.nodes || []).map((node) => [node.id, node]));
    const values = {};
    const ctx = { doc, nodes, values, act: (action, nodeId, extra) => act(action, doc.id, nodeId, extra), depth: 0, inForm: false, rerender: () => {} };
    const box = el("div", "ui-card");
    if (doc.status === "stale") box.classList.add("stale");
    const draw = () => { box.replaceChildren(); const root = nodes.get(doc.root); box.append(root ? node(root.id, ctx) || el("span") : el("div", "ui-muted", doc.fallbackText)); };
    ctx.rerender = draw;
    draw();
    return box;
  }

  function holds(when, values) {
    if (!when) return true;
    const current = values[when.name] || "";
    switch (when.is) {
      case "equals": return current === (when.value || "");
      case "not_equals": return current !== (when.value || "");
      case "empty": return current.trim() === "";
      case "filled": return current.trim() !== "";
      case "checked": return current === "true";
      default: return true;
    }
  }

  function node(id, ctx) {
    const n = ctx.nodes.get(id);
    if (!n || ctx.depth > 14 || !holds(n.when, ctx.values)) return null;
    const child = (childId, extra = {}) => node(childId, { ...ctx, depth: ctx.depth + 1, ...extra });
    const drawn = draw(n, ctx, child);
    if (drawn && n.a11y) drawn.setAttribute("aria-label", n.a11y);
    return drawn;
  }

  function draw(n, ctx, child) {
    switch (n.type) {
      case "stack": {
        const box = el("div", `ui-stack ${n.direction === "horizontal" ? "row" : "col"} surface-${n.surface || "none"}${n.wrap ? " wrap" : ""}${n.scroll ? " scroll" : ""}`);
        box.style.gap = `${GAP[n.gap || "sm"]}px`;
        if (n.padding) box.style.padding = `${GAP[n.padding]}px`;
        if (n.align) box.style.alignItems = { start: "flex-start", center: "center", end: "flex-end", stretch: "stretch" }[n.align];
        if (n.surface === "tinted") box.dataset.tone = n.tone || "default";
        for (const id of n.children || []) { const c = child(id); if (c) { const grow = ctx.nodes.get(id)?.grow; if (grow && n.direction === "horizontal") c.style.flex = String(grow); box.append(c); } }
        return box;
      }
      case "grid": {
        const box = el("div", "ui-grid");
        box.style.gridTemplateColumns = `repeat(${Math.min(n.columns || 2, 4)}, minmax(0, 1fr))`;
        box.style.gap = `${GAP[n.gap || "sm"]}px`;
        for (const id of n.children || []) { const c = child(id); if (c) box.append(c); }
        return box;
      }
      case "section": {
        const box = el("div", "ui-section");
        const head = el("div", "ui-section-head");
        if (n.title) head.append(el("div", "ui-h3", n.title));
        if (n.subtitle) head.append(el("div", "ui-muted", n.subtitle));
        const body = el("div", "ui-stack col");
        for (const id of n.children || []) { const c = child(id); if (c) body.append(c); }
        if (n.collapsible) { let open = !n.collapsed; body.hidden = !open; head.classList.add("tappable"); head.onclick = () => { open = !open; body.hidden = !open; }; }
        box.append(head, body);
        return box;
      }
      case "tabs": {
        const box = el("div", "ui-stack col");
        const bar = el("div", "ui-segmented");
        const body = el("div");
        const show = (index) => {
          [...bar.children].forEach((b, at) => b.classList.toggle("on", at === index));
          body.replaceChildren(); const c = child(n.items[index].child); if (c) body.append(c);
        };
        n.items.forEach((item, index) => { const b = el("button", "", item.label); b.type = "button"; b.onclick = () => show(index); bar.append(b); });
        box.append(bar, body); show(0);
        return box;
      }
      case "divider": return n.label ? (() => { const d = el("div", "ui-divider labeled"); d.append(el("span", "", n.label)); return d; })() : el("hr", "ui-divider");
      case "text": {
        const style = n.style || "body";
        const t = n.format === "markdown" && R() ? R().markdown(n.text, false) : el("div", "", n.text);
        t.classList.add("ui-text", `ui-${style}`);
        if (n.tone) t.style.color = toneColor(n.tone);
        if (n.align) t.style.textAlign = { start: "left", center: "center", end: "right" }[n.align];
        if (n.maxLines) { t.style.webkitLineClamp = n.maxLines; t.classList.add("clamp"); }
        return t;
      }
      case "badge": { const b = el("span", `ui-badge tone-${n.tone || "default"}`); if (n.icon) b.append(icon(ICON[n.icon] || "sparkle", 11)); b.append(el("span", "", n.text)); return b; }
      case "icon": { const i = el("span", "ui-icon"); i.append(icon(ICON[n.name] || "sparkle", { sm: 14, md: 18, lg: 26, xl: 36 }[n.size || "md"])); i.style.color = toneColor(n.tone); return i; }
      case "callout": {
        const box = el("div", `ui-callout tone-${n.tone}`);
        box.append(icon(ICON[n.icon] || (n.tone === "danger" ? "exclamationmark.circle.fill" : n.tone === "warning" ? "exclamationmark.triangle" : "info.circle"), 16));
        const text = el("div", "");
        if (n.title) text.append(el("div", "ui-strong", n.title));
        text.append(el("div", "", n.text));
        box.append(text);
        return box;
      }
      case "image": {
        const url = pictureUrl(n.url) || (typeof n.url === "string" && n.url.startsWith("/media/") ? n.url : null);
        if (!url) return null;
        const fig = el("figure", "ui-image");
        const img = el("img"); img.src = url; img.alt = n.alt || ""; img.loading = "lazy";
        if (n.width && n.height) img.style.aspectRatio = `${n.width} / ${n.height}`;
        if (n.fit === "contain") img.style.objectFit = "contain";
        img.onclick = () => n.action ? ctx.act(n.action, n.id) : R()?.openLink?.(url);
        fig.append(img);
        if (n.caption) fig.append(el("figcaption", "ui-muted", n.caption));
        return fig;
      }
      case "media": return linkRow(n.title || (n.kind === "video" ? "Videoyu aç" : "Sesi aç"), hostOf(n.url), "play.rectangle", () => open(n.url));
      case "link": return linkRow(n.label, n.detail || hostOf(n.url), "link", () => https(n.url) && open(n.url));
      case "code": { const pre = el("pre", "ui-code"); pre.append(el("code", "", n.code)); const copy = el("button", "ui-copy", "Kopyala"); copy.type = "button"; copy.onclick = () => navigator.clipboard.writeText(n.code).catch(() => {}); const box = el("div", "ui-code-box"); if (n.filename) box.append(el("div", "ui-muted", n.filename)); box.append(pre, copy); return box; }
      case "math": { const m = el("div", "ui-math", n.latex); return m; }
      case "stat": {
        const box = el("div", "ui-stat");
        box.append(el("div", "ui-muted", n.label));
        const v = el("div", "ui-stat-value", n.value); if (n.tone) v.style.color = toneColor(n.tone); box.append(v);
        if (n.delta) { const d = el("div", `ui-delta ${n.trend || "flat"}`, `${n.trend === "up" ? "↑ " : n.trend === "down" ? "↓ " : ""}${n.delta}`); box.append(d); }
        if (n.hint) box.append(el("div", "ui-muted small", n.hint));
        return box;
      }
      case "keyValue": { const box = el("div", "ui-kv"); for (const row of n.rows || []) { const r = el("div", "ui-kv-row"); r.append(el("span", "ui-muted", row.label)); const v = el("span", "ui-kv-value", row.value); if (row.tone) v.style.color = toneColor(row.tone); r.append(v); box.append(r); } return box; }
      case "list": {
        const box = el("div", "ui-list");
        (n.items || []).forEach((item, index) => {
          const row = el(item.action ? "button" : "div", "ui-list-row"); if (item.action) { row.type = "button"; row.onclick = () => ctx.act(item.action, n.id); }
          const lead = el("span", "ui-list-lead", n.style === "numbers" ? `${index + 1}.` : n.style === "checklist" ? (item.checked ? "✓" : "○") : item.leading ? "" : "•");
          if (item.leading && n.style !== "numbers" && n.style !== "checklist") lead.append(icon(ICON[item.leading] || "sparkle", 14));
          const text = el("div", "ui-list-text"); text.append(el("div", item.subtitle ? "ui-strong" : "", item.title)); if (item.subtitle) text.append(el("div", "ui-muted", item.subtitle));
          row.append(lead, text);
          if (item.badge) row.append(el("span", `ui-badge tone-${item.badgeTone || "default"}`, item.badge));
          if (item.trailing) { const t = el("span", "ui-strong", item.trailing); if (item.tone) t.style.color = toneColor(item.tone); row.append(t); }
          box.append(row);
        });
        return box;
      }
      case "table": return table(n, ctx);
      case "chart": return chart(n);
      case "plot": return plot(n);
      case "bars": {
        const top = Math.max(n.max || 0, ...n.items.map((item) => item.value), 1e-6);
        const box = el("div", "ui-bars");
        if (n.title) box.append(el("div", "ui-muted", n.title));
        for (const item of n.items) {
          const row = el("div", "ui-bar");
          const head = el("div", "ui-bar-head"); head.append(el("span", "ui-strong", item.label), el("span", "", item.display || number(item.value, n.unit)));
          const track = el("div", "ui-track"); const fill = el("div", "ui-fill"); fill.style.width = `${Math.max(3, Math.min(100, (item.value / top) * 100))}%`; if (item.tone) fill.style.background = toneColor(item.tone);
          track.append(fill); row.append(head, track); box.append(row);
        }
        return box;
      }
      case "progress": { const box = el("div", "ui-stack col"); if (n.label) box.append(el("div", "ui-muted", n.label)); const track = el("div", "ui-track"); const fill = el("div", "ui-fill"); fill.style.width = `${Math.round((n.value ?? 0.35) * 100)}%`; track.append(fill); box.append(track); return box; }
      case "steps": {
        const box = el("div", "ui-steps");
        for (const item of n.items || []) {
          const row = el("div", `ui-step ${item.status}`);
          row.append(el("span", "ui-step-dot", item.status === "done" ? "✓" : item.status === "failed" ? "✕" : item.status === "active" ? "•" : ""));
          const text = el("div", "ui-list-text"); text.append(el("div", "", item.title)); if (item.detail) text.append(el("div", "ui-muted", item.detail));
          row.append(text); if (item.meta) row.append(el("span", "ui-muted small", item.meta));
          box.append(row);
        }
        return box;
      }
      case "map": { const box = el("div", "ui-stack col"); for (const pin of n.pins || []) box.append(linkRow(pin.title, pin.subtitle || `${pin.lat.toFixed(4)}, ${pin.lng.toFixed(4)}`, "location", () => open(`https://www.google.com/maps/search/?api=1&query=${pin.lat},${pin.lng}`))); return box; }
      case "file": return linkRow(n.title, n.filename, "doc.text", () => n.actions?.[0] && ctx.act(n.actions[0], n.id));
      case "button": {
        const b = el("button", `ui-button ${n.style || "secondary"}${n.fullWidth ? " full" : ""}`);
        b.type = "button"; b.disabled = Boolean(n.disabled) || ctx.doc.status === "completed";
        if (n.icon) b.append(icon(ICON[n.icon] || "sparkle", 13));
        b.append(el("span", "", n.label));
        b.onclick = () => ctx.act(n.action, n.id, ctx.inForm ? valuesOf(ctx.values) : undefined);
        return b;
      }
      case "choice": return choice(n, ctx);
      case "textField": {
        const box = el("label", "ui-field"); box.append(el("span", "ui-muted", `${n.label}${n.required ? " *" : ""}`));
        const input = el(n.kind === "multiline" ? "textarea" : "input");
        if (input.tagName === "INPUT") input.type = { number: "number", email: "email", url: "url", phone: "tel", date: "date", time: "time", datetime: "datetime-local" }[n.kind] || "text";
        input.value = ctx.values[n.name] ?? n.value ?? ""; input.placeholder = n.placeholder || ""; if (n.maxLength) input.maxLength = n.maxLength;
        input.oninput = () => { ctx.values[n.name] = input.value; };
        box.append(input); return box;
      }
      case "toggle": {
        const box = el("label", "ui-toggle"); const input = el("input"); input.type = "checkbox"; input.checked = (ctx.values[n.name] ?? String(Boolean(n.value))) === "true";
        input.onchange = () => { ctx.values[n.name] = String(input.checked); };
        const text = el("div"); text.append(el("div", "", n.label)); if (n.detail) text.append(el("div", "ui-muted", n.detail));
        box.append(text, input); return box;
      }
      case "slider": {
        const box = el("label", "ui-field"); const head = el("div", "ui-bar-head"); const value = el("span", "ui-strong");
        const input = el("input"); input.type = "range"; input.min = n.min; input.max = n.max; input.step = n.step || (n.max - n.min) / 100; input.value = ctx.values[n.name] ?? n.value ?? n.min;
        const show = () => { value.textContent = `${input.value}${n.unit ? ` ${n.unit}` : ""}`; ctx.values[n.name] = input.value; };
        input.oninput = show; show(); head.append(el("span", "", n.label), value); box.append(head, input); return box;
      }
      case "form": {
        const box = el("form", "ui-form"); box.onsubmit = (event) => event.preventDefault();
        for (const id of n.children || []) { const c = child(id, { inForm: true }); if (c) box.append(c); }
        const done = ctx.doc.status === "completed";
        const send = el("button", "ui-button primary full", done ? "Gönderildi" : n.submitLabel || "Gönder"); send.type = "button"; send.disabled = done;
        send.onclick = () => ctx.act(n.submit, n.id, valuesOf(ctx.values));
        box.append(send); return box;
      }
      case "approval": { const box = el("div", "ui-callout tone-warning"); const text = el("div"); text.append(el("div", "ui-strong", n.title)); if (n.detail) text.append(el("div", "", n.detail)); text.append(el("div", "ui-muted small", "Onay, sohbetteki onay kartından verilir.")); box.append(text); return box; }
      case "html": return htmlCard(n);
      case "diff": {
        const box = el("div", "ui-code-box"); box.append(el("div", "ui-strong", `${n.filename}  +${n.added || 0} −${n.removed || 0}`));
        if (!n.collapsed) { const pre = el("pre", "ui-code"); for (const line of (n.lines || []).slice(0, 200)) { const l = el("div", `diff-${line.kind}`, `${line.kind === "add" ? "+ " : line.kind === "remove" ? "- " : "  "}${line.text}`); pre.append(l); } box.append(pre); }
        return box;
      }
      case "quote": { const q = el("blockquote", "ui-quote"); q.append(el("div", "", n.text)); if (n.source) { const s = el("div", "ui-muted", `— ${n.source}`); if (https(n.url)) { s.classList.add("tappable"); s.onclick = () => open(n.url); } q.append(s); } return q; }
      case "countdown": return countdown(n);
      case "gallery": { const box = el("div", "ui-gallery"); for (const image of n.images || []) { const url = pictureUrl(image.url); if (!url) continue; const fig = el("figure"); const img = el("img"); img.src = url; img.alt = image.alt || ""; img.loading = "lazy"; fig.append(img); if (image.caption) fig.append(el("figcaption", "ui-muted", image.caption)); box.append(fig); } return box; }
      case "skeleton": { const box = el("div", "ui-stack col"); const count = n.shape === "image" ? 1 : Math.max(1, Math.min(n.lines || 3, 8)); for (let i = 0; i < count; i++) { const bar = el("div", "ui-skeleton"); bar.style.height = n.shape === "image" ? "140px" : "14px"; if (i) bar.style.width = "70%"; box.append(bar); } return box; }
      case "spacer": { const s = el("div"); s.style.height = `${GAP[n.size] || 8}px`; return s; }
      case "avatar": { const a = el("span", `ui-avatar ${n.size || "md"}`); if (https(n.imageUrl)) { const img = el("img"); img.src = n.imageUrl; a.append(img); } else a.textContent = (n.name || "?").slice(0, 1).toUpperCase(); return a; }
      case "sources": return sources(n);
      case "product": return product(n);
      case "flow": return flow(n);
      case "news": return news(n);
      case "suggestions": {
        const box = el("div", "ui-suggestions"); box.append(el("div", "ui-h3", n.title || "Daha fazla araştır"));
        for (const item of n.items || []) { const b = el("button", "ui-suggestion"); b.type = "button"; b.append(el("span", "", item.label), el("span", "", "→")); b.onclick = () => ctx.act({ type: "send", text: item.prompt || item.label }, n.id); box.append(b); }
        return box;
      }
      default: return n.fallback ? el("div", "ui-muted", n.fallback) : null;
    }
  }

  const valuesOf = (values) => Object.entries(values).map(([name, value]) => ({ name, value: String(value) }));
  const open = (url) => window.WiqoRender?.openLink ? window.WiqoRender.openLink(url) : window.__TAURI__.core.invoke("open_link", { url }).catch(() => {});

  function linkRow(label, detail, iconName, onclick) {
    const row = el("button", "ui-link-row"); row.type = "button"; row.onclick = onclick;
    row.append(icon(iconName, 15));
    const text = el("div", "ui-list-text"); text.append(el("div", "ui-strong", label)); if (detail) text.append(el("div", "ui-muted small", detail));
    row.append(text, el("span", "ui-muted", "↗"));
    return row;
  }

  function favicon(url, size = 20) {
    const box = el("span", "ui-favicon"); box.style.width = box.style.height = `${size}px`;
    const host = hostOf(url);
    box.textContent = host.slice(0, 1).toUpperCase();
    const img = el("img"); img.src = `https://${host}/favicon.ico`; img.alt = ""; img.onload = () => { box.textContent = ""; box.append(img); };
    return box;
  }

  function sources(n) {
    const box = el("div", "ui-sources");
    const head = el("button", "ui-sources-head"); head.type = "button";
    const faces = el("span", "ui-faces"); for (const item of (n.items || []).slice(0, 5)) faces.append(favicon(item.url, 22));
    head.append(faces, el("span", "ui-muted", `${n.label || "Kaynaklar"} · ${(n.items || []).length}`));
    const list = el("div", "ui-stack col"); list.hidden = true;
    for (const item of n.items || []) list.append(linkRow(item.title || hostOf(item.url), [hostOf(item.url), item.published].filter(Boolean).join(" · "), "link", () => open(item.url)));
    head.onclick = () => { list.hidden = !list.hidden; };
    box.append(head, list);
    return box;
  }

  function table(n, ctx) {
    const box = el("div", "ui-table-box");
    const tableEl = el("table", `ui-table${n.striped ? " striped" : ""}`);
    let rows = [...(n.rows || [])];
    let sort = null;
    const head = el("thead"); const hr = el("tr");
    const body = el("tbody");
    const fill = () => {
      body.replaceChildren();
      for (const row of rows) {
        const tr = el("tr"); if (row.tone) tr.dataset.tone = row.tone;
        if (row.action) { tr.classList.add("tappable"); tr.onclick = () => ctx.act(row.action, n.id); }
        row.cells.forEach((cell, index) => { const td = el("td", "", cell); const align = n.columns[index]?.align; if (align === "end") td.style.textAlign = "right"; tr.append(td); });
        body.append(tr);
      }
    };
    n.columns.forEach((column, index) => {
      const th = el("th", "", column.label);
      if (column.align === "end") th.style.textAlign = "right";
      if (n.sortable) {
        th.classList.add("tappable");
        th.onclick = () => {
          sort = sort?.index === index ? { index, up: !sort.up } : { index, up: true };
          const num = (text) => Number(String(text).replace(/[^\d,.-]/g, "").replace(/\./g, "").replace(",", "."));
          rows = [...n.rows].sort((a, b) => { const x = a.cells[index] || "", y = b.cells[index] || ""; const nx = num(x), ny = num(y); const order = Number.isFinite(nx) && Number.isFinite(ny) && x && y ? nx - ny : x.localeCompare(y, "tr"); return sort.up ? order : -order; });
          fill();
        };
      }
      hr.append(th);
    });
    head.append(hr); tableEl.append(head, body); fill();
    if (n.footer) { const tf = el("tfoot"); const tr = el("tr"); for (const cell of n.footer.cells) tr.append(el("td", "", cell)); tf.append(tr); tableEl.append(tf); }
    box.append(tableEl);
    if (n.caption) box.append(el("div", "ui-muted small", n.caption));
    return box;
  }

  function chart(n) {
    const box = el("div", "ui-chart");
    if (n.title) box.append(el("div", "ui-strong", n.title));
    const W = 560, H = n.heightHint === "sm" ? 160 : n.heightHint === "lg" ? 280 : 220;
    const s = svg("svg", { viewBox: `0 0 ${W} ${H}`, width: "100%", preserveAspectRatio: "none", class: "ui-svg" });
    if (n.kind === "pie" || n.kind === "donut") {
      const points = n.series[0]?.points || []; const total = points.reduce((sum, p) => sum + Math.max(0, p.y), 0) || 1;
      const r = Math.min(H / 2 - 8, 90), cx = r + 10, cy = H / 2; let from = -Math.PI / 2;
      points.forEach((point, index) => {
        const to = from + (Math.max(0, point.y) / total) * Math.PI * 2;
        s.append(svg("path", { d: `M${cx},${cy} L${cx + r * Math.cos(from)},${cy + r * Math.sin(from)} A${r},${r} 0 ${to - from > Math.PI ? 1 : 0} 1 ${cx + r * Math.cos(to)},${cy + r * Math.sin(to)} Z`, fill: SERIES[index % SERIES.length] }));
        s.append(svg("text", { x: cx * 2 + 20, y: 24 + index * 20, "font-size": 12, fill: "currentColor" }, `${point.x} · %${Math.round((point.y / total) * 100)}`));
        s.append(svg("rect", { x: cx * 2 + 6, y: 14 + index * 20, width: 9, height: 9, rx: 2, fill: SERIES[index % SERIES.length] }));
        from = to;
      });
      if (n.kind === "donut") s.append(svg("circle", { cx, cy, r: r * 0.58, fill: "var(--canvas)" }));
      box.append(s);
      return box;
    }
    const pad = { l: 48, r: 10, t: 10, b: 24 }; const pw = W - pad.l - pad.r, ph = H - pad.t - pad.b;
    const labels = n.series[0]?.points.map((p) => p.x) || [];
    const all = n.series.flatMap((series) => series.points.map((p) => p.y)).concat(n.goal ? [n.goal.value] : []);
    let lo = n.yMin ?? (n.kind === "bar" ? Math.min(0, ...all) : Math.min(...all)), hi = n.yMax ?? Math.max(...all);
    if (hi === lo) { hi += 1; lo -= 1; }
    const y = (v) => pad.t + ph - ((v - lo) / (hi - lo)) * ph;
    const x = (i) => pad.l + (labels.length <= 1 ? pw / 2 : (i / (labels.length - 1)) * pw);
    for (let i = 0; i <= 3; i++) { const v = lo + ((hi - lo) * i) / 3; s.append(svg("line", { x1: pad.l, x2: W - pad.r, y1: y(v), y2: y(v), stroke: "var(--line)" })); s.append(svg("text", { x: pad.l - 6, y: y(v) + 4, "font-size": 10, fill: "var(--text2)", "text-anchor": "end" }, number(v, n.unit))); }
    if (n.goal) s.append(svg("line", { x1: pad.l, x2: W - pad.r, y1: y(n.goal.value), y2: y(n.goal.value), stroke: "var(--text3)", "stroke-dasharray": "4 4" }));
    const barW = Math.max(4, Math.min(28, pw / Math.max(1, labels.length) / Math.max(1, n.series.length) - 4));
    n.series.forEach((series, si) => {
      const color = SERIES[si % SERIES.length];
      if (n.kind === "bar") {
        series.points.forEach((p, i) => { const c = pad.l + ((i + 0.5) / labels.length) * pw; const left = c - (barW * n.series.length) / 2 + si * barW; const top = y(Math.max(p.y, 0)), bottom = y(Math.max(lo, 0)); s.append(svg("rect", { x: left, y: Math.min(top, bottom), width: barW - 2, height: Math.max(1, Math.abs(bottom - top)), rx: 3, fill: color })); });
        return;
      }
      const pts = series.points.map((p, i) => `${x(i)},${y(p.y)}`).join(" ");
      if (n.kind === "area") s.append(svg("polygon", { points: `${x(0)},${y(lo)} ${pts} ${x(series.points.length - 1)},${y(lo)}`, fill: color, opacity: 0.15 }));
      s.append(svg("polyline", { points: pts, fill: "none", stroke: color, "stroke-width": 2.2, "stroke-linejoin": "round", "vector-effect": "non-scaling-stroke" }));
    });
    const every = Math.ceil(labels.length / 6);
    labels.forEach((label, i) => { if (i % every === 0) s.append(svg("text", { x: n.kind === "bar" ? pad.l + ((i + 0.5) / labels.length) * pw : x(i), y: H - 6, "font-size": 10, fill: "var(--text2)", "text-anchor": "middle" }, String(label).slice(0, 10))); });
    box.append(s);
    if (n.series.length > 1) { const legend = el("div", "ui-legend"); n.series.forEach((series, si) => { const item = el("span"); const dot = el("i"); dot.style.background = SERIES[si % SERIES.length]; item.append(dot, el("span", "", series.name)); legend.append(item); }); box.append(legend); }
    return box;
  }

  /** A function graph from its expression (the server's own grammar, schema.generated.js), with the zeros and the y-intercept marked. */
  function plot(n) {
    const box = el("div", "ui-chart");
    if (n.title) box.append(el("div", "ui-strong", n.title));
    const W = 560, H = n.heightHint === "sm" ? 200 : n.heightHint === "lg" ? 340 : 280;
    const parsed = (n.functions || []).map((fn) => ({ fn, parsed: PLOT ? PLOT.parsePlotExpr(fn.expr) : { ok: false } }));
    const xMin = n.xMin ?? -6, xMax = n.xMax ?? 6;
    const samples = [];
    for (const item of parsed) if (item.parsed.ok) for (let i = 0; i <= 200; i++) { const v = PLOT.evalPlot(item.parsed.node, xMin + ((xMax - xMin) * i) / 200); if (Number.isFinite(v)) samples.push(v); }
    samples.sort((a, b) => a - b);
    const q = (p) => samples[Math.floor((samples.length - 1) * p)] ?? 0;
    let yMin = n.yMin ?? Math.min(q(0.03), 0), yMax = n.yMax ?? Math.max(q(0.97), 0);
    if (yMax - yMin < 1e-6) { yMin -= 1; yMax += 1; }
    if (n.yMin === undefined) yMin -= (yMax - yMin) * 0.08; if (n.yMax === undefined) yMax += (yMax - yMin) * 0.08;
    const sx = (v) => ((v - xMin) / (xMax - xMin)) * W, sy = (v) => H - ((v - yMin) / (yMax - yMin)) * H;
    const s = svg("svg", { viewBox: `0 0 ${W} ${H}`, width: "100%", class: "ui-svg ui-plot" });
    if (yMin < 0 && yMax > 0) s.append(svg("line", { x1: 0, x2: W, y1: sy(0), y2: sy(0), stroke: "var(--text3)" }));
    if (xMin < 0 && xMax > 0) s.append(svg("line", { x1: sx(0), x2: sx(0), y1: 0, y2: H, stroke: "var(--text3)" }));
    const marks = [];
    parsed.forEach(({ fn, parsed: result }, index) => {
      if (!result.ok) return;
      let d = "", pen = false, prev = null;
      for (let i = 0; i <= 400; i++) {
        const xv = xMin + ((xMax - xMin) * i) / 400; const v = PLOT.evalPlot(result.node, xv);
        if (!Number.isFinite(v) || v < yMin - (yMax - yMin) * 3 || v > yMax + (yMax - yMin) * 3) { pen = false; prev = null; continue; }
        d += `${pen ? "L" : "M"}${sx(xv).toFixed(1)},${sy(v).toFixed(1)}`; pen = true;
        if (index === 0 && prev !== null && prev * v <= 0 && prev !== 0 && marks.length < 8) {
          const root = v === 0 ? xv : xv - ((xMax - xMin) / 400) * (v / (v - prev));
          if (!marks.some((mark) => Math.abs(mark.x - root) < (xMax - xMin) / 80)) marks.push({ x: root, y: 0, label: `x≈${Number(root.toFixed(2))}` });
        }
        prev = v;
      }
      if (index === 0 && xMin < 0 && xMax > 0) { const y0 = PLOT.evalPlot(result.node, 0); if (Number.isFinite(y0)) marks.push({ x: 0, y: y0, label: `(0, ${Number(y0.toFixed(2))})` }); }
      s.append(svg("path", { d, fill: "none", stroke: toneColor(fn.tone) === "var(--ink)" ? SERIES[index % SERIES.length] : toneColor(fn.tone), "stroke-width": 2.4 }));
    });
    for (const mark of [...marks, ...(n.points || []).map((p) => ({ x: p.x, y: p.y, label: p.label || "" }))]) {
      s.append(svg("circle", { cx: sx(mark.x), cy: sy(mark.y), r: 4, fill: "var(--ink)" }));
      if (mark.label) s.append(svg("text", { x: sx(mark.x) + 6, y: sy(mark.y) - 6, "font-size": 11, fill: "var(--text2)" }, mark.label));
    }
    box.append(s);
    const legend = el("div", "ui-legend");
    (n.functions || []).forEach((fn, index) => { const item = el("span"); const dot = el("i"); dot.style.background = SERIES[index % SERIES.length]; item.append(dot, el("span", "", fn.label || `y = ${fn.expr}`)); legend.append(item); });
    box.append(legend);
    const failed = parsed.filter((item) => !item.parsed.ok);
    if (failed.length) box.append(el("div", "ui-error", `Çizilemedi: ${failed.map((item) => item.fn.expr).join(", ")}`));
    return box;
  }

  function product(n) {
    const box = el("div", "ui-list");
    for (const item of n.items || []) {
      const row = el(https(item.url) ? "button" : "div", "ui-product"); if (https(item.url)) { row.type = "button"; row.onclick = () => open(item.url); }
      const pic = el("span", "ui-product-pic"); if (https(item.imageUrl)) { const img = el("img"); img.src = item.imageUrl; img.loading = "lazy"; pic.append(img); } else pic.append(icon("sparkle", 18));
      const text = el("div", "ui-list-text"); text.append(el("div", "ui-strong", item.title)); if (item.price) text.append(el("div", "ui-stat-value small", item.price));
      text.append(el("div", "ui-muted small", [item.store, item.rating ? `★ ${item.rating.toFixed(1)}` : null, item.note].filter(Boolean).join(" · ")));
      if (https(item.url)) text.append(el("div", "ui-link", "Satın al ↗"));
      row.append(pic, text); box.append(row);
    }
    if (n.caption) box.append(el("div", "ui-muted small", n.caption));
    return box;
  }

  function flow(n) {
    const box = el("div", "ui-flow");
    (n.steps || []).forEach((step, index) => {
      const last = index === n.steps.length - 1 && n.highlightLast;
      const cell = (title, detail) => { const c = el("div", `ui-flow-box${last ? " last" : ""}`); c.append(el("div", "ui-strong", title)); if (detail) c.append(el("div", "ui-muted small", detail)); return c; };
      if (step.group) { const g = el("div", "ui-flow-group"); g.append(el("div", "ui-muted small", step.title)); const row = el("div", "ui-flow-row"); for (const item of step.group) row.append(cell(item.title)); g.append(row); box.append(g); }
      else box.append(cell(step.title, step.detail));
      if (index < n.steps.length - 1) box.append(el("div", "ui-flow-arrow", "↓"));
    });
    return box;
  }

  function news(n) {
    const box = el("div", "ui-news");
    if (n.title) box.append(el("div", "ui-h2", n.title));
    if (n.checkedAt) { const row = el("div", "ui-row"); row.append(el("span", "ui-badge tone-success", "Web üzerinden doğrulandı"), el("span", "ui-muted small", n.checkedAt)); box.append(row); }
    const meta = (item) => [item.category, item.published, item.category ? null : item.source].filter(Boolean).join(" · ");
    const chip = (item) => { const c = el("span", "ui-source-chip"); c.append(favicon(item.url, 16), el("span", "", item.source)); if (item.more?.length) c.append(el("span", "ui-muted", `+${item.more.length}`)); return c; };
    const story = (item, lead) => {
      const row = el("button", lead ? "ui-story lead" : "ui-story"); row.type = "button"; row.onclick = () => open(item.url);
      const pic = el("span", "ui-story-pic"); if (https(item.image)) { const img = el("img"); img.src = item.image; img.loading = "lazy"; img.onerror = () => img.remove(); pic.append(img); }
      const text = el("div", "ui-list-text");
      if (meta(item)) text.append(el("div", "ui-muted small", meta(item)));
      text.append(el("div", lead ? "ui-h3" : "ui-strong", item.title));
      if (item.summary) text.append(el("div", "", item.summary));
      const foot = el("div", "ui-row"); foot.append(chip(item), el("span", "ui-link", `${item.linkLabel || "Haberi oku"} ↗`)); text.append(foot);
      if (lead && !https(item.image)) row.append(text); else row.append(pic, text);
      return row;
    };
    const items = n.items || [];
    if ((n.layout || "lead") === "list") items.forEach((item) => box.append(story(item, false)));
    else { box.append(story(items[0], true)); items.slice(1).forEach((item) => box.append(story(item, false))); }
    return box;
  }

  function choice(n, ctx) {
    const box = el("div", "ui-choice"); if (n.label) box.append(el("div", "ui-muted", n.label));
    const selected = () => new Set(String(ctx.values[n.name] ?? (n.value || []).join(",")).split(",").filter(Boolean));
    const pick = (id) => {
      if (n.mode === "multi") { const set = selected(); if (set.has(id)) set.delete(id); else set.add(id); ctx.values[n.name] = [...set].join(","); refresh(); return; }
      ctx.values[n.name] = id; refresh();
      if (ctx.inForm) return;
      const option = n.options.find((item) => item.id === id);
      if (n.onSelect) ctx.act(n.onSelect, n.id, [{ name: n.name, value: id }]);
      else if (option) ctx.act({ type: "send", text: option.label, once: true }, n.id);
    };
    const list = el("div", (n.display || "list") === "chips" ? "ui-chips" : "ui-options");
    const refresh = () => { const set = selected(); [...list.children].forEach((b) => b.classList.toggle("on", set.has(b.dataset.id))); };
    for (const option of n.options || []) { const b = el("button", "ui-option"); b.type = "button"; b.dataset.id = option.id; b.append(el("span", "", option.label)); if (option.detail) b.append(el("span", "ui-muted small", option.detail)); b.onclick = () => pick(option.id); list.append(b); }
    box.append(list); refresh(); return box;
  }

  function countdown(n) {
    const box = el("div", "ui-countdown"); if (n.label) box.append(el("div", "ui-muted", n.label));
    const value = el("div", "ui-stat-value"); box.append(value);
    const tick = () => {
      if (!box.isConnected && box.dataset.started) return;
      box.dataset.started = "1";
      const left = Date.parse(n.until) - Date.now();
      value.textContent = left > 0 ? `${Math.floor(left / 864e5)}g ${Math.floor(left / 36e5) % 24}s ${Math.floor(left / 6e4) % 60}d ${Math.floor(left / 1e3) % 60}sn` : n.doneText || "Zamanı geldi";
      if (left > 0) setTimeout(tick, 1000);
    };
    tick(); return box;
  }

  /** A small page the card brings (a tool, a simulation): sandboxed, no network, Wiqo's colours as CSS variables, as tall as its content. */
  function htmlCard(n) {
    const box = el("div", "ui-stack col");
    if (n.title) box.append(el("div", "ui-strong", n.title));
    const frame = el("iframe", "ui-html");
    frame.setAttribute("sandbox", "allow-scripts");
    frame.style.height = `${{ sm: 200, lg: 460, tall: 620 }[n.heightHint] || 320}px`;
    const css = getComputedStyle(document.documentElement);
    const v = (name, fallback) => css.getPropertyValue(name).trim() || fallback;
    frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; font-src data:">
<style>:root{--wq-bg:${v("--canvas", "#fff")};--wq-surface:${v("--fill", "#f5f5f5")};--wq-fg:${v("--ink", "#111")};--wq-muted:${v("--text2", "#6b6b6b")};--wq-accent:${v("--ink", "#111")};--wq-border:${v("--line", "#e5e5e5")};--wq-radius:14px}
html,body{margin:0;background:transparent;color:var(--wq-fg);font:14px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif}*{box-sizing:border-box}.wq-card{background:var(--wq-surface);border-radius:var(--wq-radius);padding:12px}.wq-row{display:flex;gap:8px;align-items:center}.wq-label{color:var(--wq-muted);font-size:13px}.wq-value{font-weight:600}.wq-btn,button{min-height:36px;padding:0 14px;border-radius:999px;border:1px solid var(--wq-border);background:var(--wq-surface);color:var(--wq-fg);font:inherit;font-weight:600}input[type=range]{width:100%;accent-color:var(--wq-fg)}canvas,svg{max-width:100%}</style></head>
<body>${n.html}<script>function h(){parent.postMessage({wiqoHeight:Math.ceil(document.documentElement.scrollHeight)},"*")}addEventListener("load",function(){h();setTimeout(h,400);setTimeout(h,1200)});new ResizeObserver(h).observe(document.body)</script></body></html>`;
    const onMessage = (event) => { if (event.source === frame.contentWindow && event.data && typeof event.data.wiqoHeight === "number") frame.style.height = `${Math.min(900, Math.max(60, event.data.wiqoHeight))}px`; };
    window.addEventListener("message", onMessage);
    box.append(frame);
    return box;
  }

  /** A card event from the stream applied to a message's cards: a whole card replaces the one with its id; a patch adds, replaces or removes nodes. */
  function applyEvent(cards, event) {
    if (event.type === "ui") {
      const doc = event.doc;
      if (!doc?.id || !Array.isArray(doc.nodes)) return cards;
      return cards.some((c) => c.id === doc.id) ? cards.map((c) => (c.id === doc.id ? doc : c)) : [...cards, doc];
    }
    return cards.map((c) => {
      if (c.id !== event.docId) return c;
      const remove = new Set(event.remove || []);
      const upsert = new Map((event.upsert || []).map((n) => [n.id, n]));
      let nodes = c.nodes.filter((n) => !remove.has(n.id)).map((n) => upsert.get(n.id) || n);
      nodes = [...nodes, ...(event.upsert || []).filter((n) => !c.nodes.some((old) => old.id === n.id))];
      for (const piece of event.appendText || []) nodes = nodes.map((n) => n.id !== piece.id ? n : n.type === "text" ? { ...n, text: n.text + piece.text } : n.type === "code" ? { ...n, code: n.code + piece.text } : n);
      return { ...c, nodes, ...(typeof event.root === "string" ? { root: event.root } : {}) };
    });
  }

  window.WiqoCards = { capabilities, card, applyEvent };
})();
