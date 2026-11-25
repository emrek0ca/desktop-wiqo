// Projeler: the phones' projects on the computer (iOS ActHub / Android ActScreen). Ready templates from the server (wire.ts ProjectTemplate, with the
// picture the server drew of each), a project written or changed by the writer (WriterJob, shown as it goes), its page, code and history, published at
// its own address (PublishedSite), and kept the same on every device: end-to-end encrypted with the account's project key, the same key and format the
// phones use (act-vault.ts: AES-GCM, HKDF "wiqo.act.v1"), so a phone's recovery code opens the same projects here.
(() => {
  const { invoke } = window.__TAURI__.core;
  const R = () => window.WiqoRender;
  const icon = (name, size) => window.WiqoIcons.icon(name, size);
  const $ = (id) => document.getElementById(id);
  const el = (tag, className, text) => { const node = document.createElement(tag); if (className) node.className = className; if (text !== undefined && text !== null) node.textContent = String(text); return node; };
  const button = (label, className, onclick) => { const b = el("button", className, label); b.type = "button"; b.onclick = onclick; return b; };
  const api = (method, path, body) => invoke("code_sync", { method, path, body: body ?? null });
  const toast = (text, tone) => window.toast?.(text, tone);

  const CATEGORIES = ["İşletme sayfası", "Kurumsal site", "Landing page", "Davetiye & ilan", "İşletme araçları"];
  const ACCEPTED = new Set(["html", "htm", "css", "js", "json", "txt", "md", "py", "svg", "csv"]);
  const KINDS = new Set((window.WiqoSchema?.schema?.$defs?.ProjectKind?.enum) || ["web", "python"]);
  const kindOf = (kind) => (KINDS.has(kind) ? kind : "web");

  // ----- the project key (act-vault.ts, the same bytes on every device) -----

  const B32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  const enc = new TextEncoder();
  const b64 = (bytes) => { let text = ""; for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000)); return btoa(text); };
  const unb64 = (text) => Uint8Array.from(atob(text.replace(/[^A-Za-z0-9+/=]/g, "")), (c) => c.charCodeAt(0));
  const hex = (buffer) => [...new Uint8Array(buffer)].map((b) => b.toString(16).padStart(2, "0")).join("");
  function encode32(bytes) {
    let out = "", buffer = 0, bits = 0;
    for (const byte of bytes) { buffer = (buffer << 8) | byte; bits += 8; while (bits >= 5) { out += B32[(buffer >>> (bits - 5)) & 31]; bits -= 5; } }
    if (bits) out += B32[(buffer << (5 - bits)) & 31];
    return out.match(/.{1,5}/g).join("-");
  }
  function decode32(code) {
    const out = []; let buffer = 0, bits = 0;
    for (const c of code.toUpperCase().replace(/[\s-]/g, "")) { const v = B32.indexOf(c); if (v < 0) throw new Error("Geçersiz kurtarma kodu."); buffer = (buffer << 5) | v; bits += 5; if (bits >= 8) { out.push((buffer >>> (bits - 8)) & 255); bits -= 8; } }
    return new Uint8Array(out);
  }
  const sha = async (bytes) => new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));

  let keys = null;
  async function loadKeys(create = true) {
    if (keys) return keys;
    let stored = await invoke("act_key");
    if (!stored) {
      if (!create) return null;
      stored = b64(crypto.getRandomValues(new Uint8Array(32)));
      await invoke("act_key_set", { key: stored });
    }
    const master = unb64(stored);
    const base = await crypto.subtle.importKey("raw", master, "HKDF", false, ["deriveKey", "deriveBits"]);
    const derive = (info) => crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: enc.encode("wiqo.act.v1"), info: enc.encode(info) }, base, 256);
    const content = await crypto.subtle.importKey("raw", await derive("content"), "AES-GCM", false, ["encrypt", "decrypt"]);
    const pathKey = await crypto.subtle.importKey("raw", await derive("path"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const hashKey = await crypto.subtle.importKey("raw", await derive("hash"), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
    const checksum = (await sha(master)).slice(0, 2);
    keys = {
      recoveryCode: encode32(new Uint8Array([...master, ...checksum])),
      async seal(value) {
        const nonce = crypto.getRandomValues(new Uint8Array(12));
        const cipher = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, content, enc.encode(JSON.stringify(value))));
        return b64(new Uint8Array([...nonce, ...cipher]));
      },
      async open(blob) {
        try { const data = unb64(blob); if (data.length < 28) return null; const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: data.slice(0, 12) }, content, data.slice(12)); return JSON.parse(new TextDecoder().decode(plain)); } catch { return null; }
      },
      pathToken: async (path) => hex(await crypto.subtle.sign("HMAC", pathKey, enc.encode(path))),
      contentHash: async (path, text) => hex(await crypto.subtle.sign("HMAC", hashKey, enc.encode(`${path}\0${text}`))),
    };
    return keys;
  }

  async function importRecoveryCode(code) {
    const packed = decode32(code);
    if (packed.length !== 34) throw new Error("Geçersiz kurtarma kodu.");
    const master = packed.slice(0, 32), check = (await sha(master)).slice(0, 2);
    if (check[0] !== packed[32] || check[1] !== packed[33]) throw new Error("Kurtarma kodu doğrulanamadı; harfleri kontrol et.");
    await invoke("act_key_set", { key: b64(master) });
    keys = null;
    // Projects synced under the old key cannot be read any more; the ones on the server come back under the new key.
    projects = projects.filter((item) => !item.hubId);
    await persist();
  }

  // ----- the projects on this computer -----

  let projects = [];
  let loadedFor = null;
  const uuid = () => crypto.randomUUID();
  const safePath = (path) => { const parts = path.split("/"); return !!path && !path.includes("\\") && !path.includes("\0") && parts.length <= 4 && parts.every((part) => part && !part.startsWith(".")); };
  const syncable = (path, content) => safePath(path) && ACCEPTED.has(path.split(".").pop().toLowerCase()) && enc.encode(content).length <= 100 * 1024;
  function history(summary, source, before, after, verified = true) {
    const paths = [...new Set([...Object.keys(before), ...Object.keys(after)])].sort();
    const changes = paths.flatMap((path) => (before[path] === after[path] ? [] : [{ path, ...(before[path] !== undefined ? { before: before[path] } : {}), ...(after[path] !== undefined ? { after: after[path] } : {}) }]));
    return { id: uuid(), summary, source, verified, date: Date.now(), changes };
  }

  async function load() {
    const account = $("account-email")?.textContent || "";
    if (loadedFor === account) return;
    try { const text = await invoke("projects_read"); projects = text ? JSON.parse(text).projects || [] : []; } catch { projects = []; }
    loadedFor = account;
  }
  let saving = Promise.resolve();
  function persist() {
    const text = JSON.stringify({ version: 1, projects });
    saving = saving.then(() => invoke("projects_write", { data: text })).catch((error) => toast(String(error), "bad"));
    return saving;
  }
  async function save(item) {
    item.updatedAt = Date.now();
    const index = projects.findIndex((row) => row.id === item.id);
    if (index >= 0) projects[index] = item; else projects.unshift(item);
    await persist();
    draw();
    syncOne(item).catch(() => {});
  }

  // ----- sync (the same merge as the phones: changed on both sides keeps this computer's copy next to the server's) -----

  async function openRows(rows, k) {
    const files = {};
    for (const row of rows) {
      if (!row.b) continue;
      const envelope = await k.open(row.b);
      if (!envelope || !syncable(envelope.path, envelope.content ?? "") || row.p !== (await k.pathToken(envelope.path)) || files[envelope.path] !== undefined) throw new Error("Şifreli proje bu bilgisayarda açılamadı. Kurtarma kodunu kontrol et.");
      // A deleted file's envelope has no content: `null` here and on Android, the key left out on iOS (Swift's encoder); both read the same.
      if (envelope.content != null) {
        if (row.h !== (await k.contentHash(envelope.path, envelope.content))) throw new Error("Proje dosyasının doğrulaması başarısız.");
        files[envelope.path] = envelope.content;
      }
    }
    return files;
  }
  const sealRow = async (path, content, k) => (content === null
    ? { p: await k.pathToken(path), b: await k.seal({ path, content: null }), h: null, d: true }
    : { p: await k.pathToken(path), b: await k.seal({ path, content }), h: await k.contentHash(path, content) });
  async function hashes(files, k) { const out = {}; for (const [path, content] of Object.entries(files)) out[path] = await k.contentHash(path, content); return out; }

  async function syncProject(source, k) {
    const project = { ...source, base: { ...(source.base || {}) }, version: source.version || 0 };
    const own = Object.fromEntries(Object.entries(project.files).filter(([path, content]) => syncable(path, content)));
    if (!project.hubId) {
      const created = await api("POST", "code-sync/projects", { meta: await k.seal({ name: project.name, kind: project.kind }) });
      project.hubId = created.id;
      const index = projects.findIndex((row) => row.id === project.id);
      if (index >= 0) { projects[index].hubId = created.id; await persist(); }
    }
    const snapshot = await api("GET", `code-sync/projects/${project.hubId}/snapshot`);
    let remote = await openRows(snapshot.files, k);
    const base = project.base;
    const merged = { ...remote };
    for (const path of new Set([...Object.keys(own), ...Object.keys(base)])) {
      const local = own[path];
      const localHash = local === undefined ? undefined : await k.contentHash(path, local);
      if (localHash === base[path]) continue;
      const remoteHash = remote[path] === undefined ? undefined : await k.contentHash(path, remote[path]);
      const remoteChanged = remoteHash !== base[path];
      if (remoteChanged && local !== undefined && local !== remote[path]) {
        let copy = path.replace(/(\.[^./]+)?$/, " - Bilgisayar kopyası$1"), n = 2;
        while (merged[copy] !== undefined || own[copy] !== undefined) copy = path.replace(/(\.[^./]+)?$/, ` ${n++}$1`);
        merged[copy] = local;
      } else if (!remoteChanged && local === undefined) delete merged[path];
      else if (!remoteChanged) merged[path] = local;
    }
    const diff = async (against) => { const rows = []; for (const path of new Set([...Object.keys(merged), ...Object.keys(against)])) if (merged[path] !== against[path]) rows.push(await sealRow(path, merged[path] ?? null, k)); return rows; };
    let changes = await diff(remote);
    if (changes.length) {
      let pushed = await api("POST", `code-sync/projects/${project.hubId}/push`, { baseVersion: snapshot.version, summary: "değişiklik", device: "Bilgisayar", changes });
      if (pushed.conflict) {
        const latest = await api("GET", `code-sync/projects/${project.hubId}/snapshot`);
        remote = await openRows(latest.files, k);
        changes = await diff(remote);
        if (changes.length) pushed = await api("POST", `code-sync/projects/${project.hubId}/push`, { baseVersion: latest.version, summary: "değişiklik", device: "Bilgisayar", changes });
        if (!pushed.ok) throw new Error("Eşitlemede çakışma oldu; proje bu bilgisayarda korundu.");
      } else if (!pushed.ok) throw new Error(pushed.error || "Proje eşitlenemedi.");
      const latest = await api("GET", `code-sync/projects/${project.hubId}/snapshot`);
      remote = await openRows(latest.files, k);
      project.version = latest.version;
    } else project.version = snapshot.version;
    // Files the server does not carry (a picture, a big file) stay on this computer.
    const kept = Object.fromEntries(Object.entries(project.files).filter(([path, content]) => !syncable(path, content)));
    project.files = { ...kept, ...remote };
    project.base = await hashes(remote, k);
    return project;
  }

  let syncing = false;
  async function syncOne(item) {
    if (item.mode === "deviceOnly" || syncing) return;
    const k = await loadKeys();
    const synced = await syncProject(item, k);
    const index = projects.findIndex((row) => row.id === item.id);
    if (index < 0) return;
    // Changed here while it synced: only the server's id is kept; the next sync merges the newer change from the old base.
    if (projects[index].updatedAt !== item.updatedAt) projects[index] = { ...projects[index], hubId: synced.hubId };
    else projects[index] = { ...projects[index], ...synced, history: projects[index].history };
    await persist();
  }

  async function syncAll() {
    if (syncing) return;
    syncing = true; note("Eşitleniyor…");
    try {
      const k = await loadKeys();
      const { projects: remoteList = [] } = await api("GET", "code-sync/projects");
      const known = new Set(projects.map((item) => item.hubId).filter(Boolean));
      for (const item of [...projects]) {
        if (item.mode === "deviceOnly") continue;
        try { const synced = await syncProject(item, k); Object.assign(projects.find((row) => row.id === item.id) || {}, synced); if (synced.hubId) known.add(synced.hubId); }
        catch (error) { note(String(error.message || error)); }
      }
      for (const remote of remoteList) {
        if (known.has(remote.id)) continue;
        const meta = await k.open(remote.meta);
        if (!meta?.name) continue;
        try {
          const snapshot = await api("GET", `code-sync/projects/${remote.id}/snapshot`);
          const files = await openRows(snapshot.files, k);
          if (!Object.keys(files).length) continue;
          projects.push({ id: uuid(), name: meta.name, kind: kindOf(meta.kind), files, mode: "devices", hubId: remote.id, version: snapshot.version, base: await hashes(files, k), updatedAt: Date.parse(remote.updatedAt) || Date.now(), history: [history("Başka cihazdan alındı", "Başka cihaz", {}, files)] });
        } catch (error) { note(String(error.message || error)); }
      }
      await persist();
      if (!notice.startsWith("Şifreli") && !notice.startsWith("Proje dosyasının")) note("");
    } catch (error) { note(String(error.message || error)); }
    finally { syncing = false; draw(); }
  }

  // ----- the page -----

  let view = { tab: "mine", category: null, project: null, pane: "preview", file: null };
  let notice = "";
  let job = null;
  let templates = null;
  const pictures = new Map();
  const note = (text) => { notice = text; const box = $("projects-notice"); if (box) { box.textContent = text; box.hidden = !text; } };

  function bundleWeb(files) {
    const entry = files["index.html"] ?? Object.entries(files).find(([path]) => path.toLowerCase().endsWith(".html"))?.[1];
    if (!entry) return "";
    const escape = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    let html = entry;
    for (const [path, content] of Object.entries(files)) {
      if (path.endsWith(".css")) html = html.replace(new RegExp(`<link\\b[^>]*href=["']${escape(path)}["'][^>]*>`, "i"), () => `<style>${content.replace(/<\/style/gi, "<\\/style")}</style>`);
      if (path.endsWith(".js")) html = html.replace(new RegExp(`<script\\b[^>]*src=["']${escape(path)}["'][^>]*>\\s*<\\/script>`, "i"), () => `<script>${content.replace(/<\/script/gi, "<\\/script")}</script>`);
    }
    return html;
  }
  function networkOrigins(files) {
    try { return [...new Set((JSON.parse(files["act.network.json"] || "{}").origins || []).filter((origin) => /^https:\/\/[a-z0-9.-]+(:\d+)?$/i.test(origin)))].slice(0, 8); } catch { return []; }
  }
  const relative = (time) => {
    const minutes = Math.max(0, Math.floor((Date.now() - time) / 60000));
    if (minutes < 1) return "şimdi"; if (minutes < 60) return `${minutes} dk. önce`;
    const hours = Math.floor(minutes / 60); if (hours < 24) return `${hours} sa. önce`;
    const days = Math.floor(hours / 24); return days === 1 ? "Dün" : days < 7 ? `${days} gün önce` : new Date(time).toLocaleDateString("tr-TR");
  };

  function draw() {
    const host = $("projects-body");
    if (!host) return;
    host.replaceChildren(view.project ? studio(projects.find((item) => item.id === view.project)) : home());
    note(notice);
  }

  function home() {
    const page = el("div", "proj-home");
    const ask = el("form", "proj-ask");
    const input = el("textarea"); input.placeholder = "Ne yapalım? “Kafem için menü sayfası”, “Python ile birim çevirici”…"; input.rows = 2;
    const go = el("button", "ui-button primary", job ? "Yazılıyor…" : "Oluştur"); go.type = "submit"; go.disabled = Boolean(job);
    ask.onsubmit = (event) => { event.preventDefault(); if (input.value.trim()) write(input.value.trim(), null); };
    input.onkeydown = (event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) ask.requestSubmit(); };
    ask.append(input, go);
    page.append(ask);
    if (job && !job.projectId) page.append(timeline(job));

    const bar = el("div", "proj-bar");
    const seg = el("div", "ui-segmented");
    for (const [tab, label] of [["mine", "Projelerim"], ["templates", "Şablonlar"]]) { const b = button(label, view.tab === tab ? "on" : "", () => { view.tab = tab; draw(); }); seg.append(b); }
    bar.append(seg);
    const tools = el("div", "proj-tools");
    const sync = button("", "icon-button", () => syncAll()); sync.title = "Eşitle"; sync.append(icon("arrow.clockwise", 14));
    const key = button("", "icon-button", () => keyDialog()); key.title = "Cihazlar arası (kurtarma kodu)"; key.append(icon("key", 14));
    tools.append(sync, key); bar.append(tools);
    page.append(bar);
    const box = el("p", "error small"); box.id = "projects-notice"; box.hidden = true; page.append(box);

    if (view.tab === "templates") {
      const chips = el("div", "ui-chips");
      for (const name of [null, ...CATEGORIES]) { const chip = button(name || "Tümü", `ui-option${view.category === name ? " on" : ""}`, () => { view.category = name; draw(); }); chips.append(chip); }
      page.append(chips, gallery());
      return page;
    }
    if (!projects.length) { page.append(el("div", "card muted", "Henüz proje yok. Yukarıya ne istediğini yaz ya da bir şablonla başla.")); return page; }
    const list = el("div", "proj-list");
    for (const item of [...projects].sort((a, b) => b.updatedAt - a.updatedAt)) {
      const row = button("", "proj-row", () => { view = { ...view, project: item.id, pane: "preview", file: null }; draw(); });
      const mark = el("span", "proj-mark"); mark.append(icon(item.kind === "python" ? "terminal" : "safari", 16));
      const text = el("div", "ui-list-text"); text.append(el("div", "ui-strong", item.name), el("div", "ui-muted small", `${item.kind === "python" ? "Python" : "Web"} · ${relative(item.updatedAt)}${item.mode === "deviceOnly" ? " · sadece bu bilgisayar" : ""}`));
      row.append(mark, text, el("span", "ui-muted", "›"));
      list.append(row);
    }
    page.append(list);
    return page;
  }

  function gallery() {
    const grid = el("div", "proj-grid");
    if (!templates) {
      grid.append(el("div", "muted small", "Şablonlar yükleniyor…"));
      api("GET", "code-sync/templates").then((list) => { templates = list.templates || []; draw(); }).catch((error) => { note(String(error)); });
      return grid;
    }
    for (const item of templates.filter((row) => !view.category || row.category === view.category)) {
      const card = button("", "proj-card", () => fromTemplate(item));
      card.disabled = Boolean(job);
      const cover = el("div", "proj-cover");
      if (item.preview) {
        const img = el("img"); img.alt = "";
        const cached = pictures.get(item.preview);
        if (cached) img.src = cached; else invoke("code_sync_picture", { path: item.preview }).then((url) => { pictures.set(item.preview, url); img.src = url; }).catch(() => img.remove());
        cover.append(img);
      } else cover.append(icon("square.grid.2x2", 22));
      if (item.pages > 1) cover.append(el("span", "proj-pages", `${item.pages} sayfa`));
      const text = el("div", "proj-card-text"); text.append(el("div", "ui-strong", item.title), el("div", "ui-muted small", item.description));
      card.append(cover, text);
      grid.append(card);
    }
    return grid;
  }

  async function fromTemplate(template) {
    note("Şablon indiriliyor…");
    try {
      const bundle = await api("GET", `code-sync/templates/${encodeURIComponent(template.id)}`);
      if (!bundle.files?.["index.html"]) throw new Error("Şablon eksik indi; yeniden dene.");
      const item = { id: uuid(), name: bundle.title.slice(0, 60), kind: kindOf(bundle.kind), files: bundle.files, mode: "devices", updatedAt: Date.now(), history: [history(`“${bundle.title}” şablonundan oluşturuldu`, "Şablon", {}, bundle.files)] };
      note("");
      view = { ...view, tab: "mine", project: item.id, pane: "preview", file: null };
      await save(item);
    } catch (error) { note(String(error.message || error)); }
  }

  // ----- the writer (WriterJob): a new project or a change to one, shown as it goes -----

  const STAGES = ["Yazılıyor", "Deneniyor", "Kaydediliyor"];
  const stageOf = (current) => (["checking", "awaiting_device_check", "device_verified", "device_failed"].includes(current.phase) ? 1 : current.phase === "done" || current.status === "done" ? 2 : 0);
  function timeline(current) {
    const box = el("div", "proj-timeline");
    const head = el("div", "ui-row"); head.append(el("span", "spinner tiny"), el("span", "ui-strong", current.status === "queued" ? "Sırada" : STAGES[stageOf(current)]));
    const stop = button("Durdur", "ui-button plain", () => api("POST", `code-sync/writer/jobs/${encodeURIComponent(current.id)}/cancel`, {}).catch(() => {}));
    if (current.id) head.append(stop);
    const steps = el("div", "proj-stages");
    STAGES.forEach((title, index) => { const stage = stageOf(current); const cell = el("div", `proj-stage ${index < stage ? "done" : index === stage ? "on" : ""}`); cell.append(el("i"), el("span", "small", title)); steps.append(cell); });
    box.append(head, steps);
    if (current.contract) { const details = el("details", "proj-contract"); details.append(el("summary", "small", "Proje sözleşmesi hazır · oku"), el("pre", "ui-code", current.contract)); box.append(details); }
    if (current.progress) box.append(el("div", "ui-muted small", current.progress));
    return box;
  }

  const KEY_FILES = new Set(["README.md", "PROJE.md", "package.json", "index.html"]);
  function filesFor(files, request) {
    const rows = Object.entries(files).filter(([path]) => path !== "act.images.json").map(([path, content]) => ({ path, content }));
    const total = rows.reduce((sum, row) => sum + row.content.length, 0);
    if (rows.length <= 40 && total <= 1500000 && rows.every((row) => row.content.length <= 120000)) return rows;
    const terms = request.toLocaleLowerCase("tr-TR").split(/[^\p{L}\p{N}_]+/u).filter((term) => term.length >= 3);
    const score = (row) => (KEY_FILES.has(row.path) ? 25 : 0) + terms.filter((term) => row.path.toLowerCase().includes(term) || row.content.toLowerCase().includes(term)).length * 6;
    let used = 0;
    return rows.filter((row) => row.content.length <= 120000).sort((a, b) => score(b) - score(a)).filter((row) => (used + row.content.length <= 1500000 ? (used += row.content.length, true) : false)).slice(0, 40);
  }

  async function write(request, target, operation = target ? "edit" : "create") {
    if (job) return;
    job = { id: null, status: "queued", progress: "Sunucuya bağlanıyor…", projectId: target?.id };
    draw();
    try {
      const { jobId } = await api("POST", "code-sync/writer/jobs", { operation, request, files: target ? filesFor(target.files, request) : [], kind: target?.kind ?? "web", ...(target ? { name: target.name } : {}), longRun: true });
      job = { ...job, id: jobId };
      for (let attempt = 0; attempt < 400; attempt++) {
        const current = await api("GET", `code-sync/writer/jobs/${encodeURIComponent(jobId)}`);
        job = { ...current, projectId: target?.id };
        draw();
        if (["done", "failed", "cancelled"].includes(current.status)) {
          await api("DELETE", `code-sync/writer/jobs/${encodeURIComponent(jobId)}`).catch(() => {});
          if (current.status !== "done" || !current.result) throw new Error(current.error || (current.errorCode === "VALIDATION_FAILED" ? "Üretilen kod doğrulamadan geçemedi; isteği değiştirip yeniden dene." : current.status === "cancelled" ? "Durduruldu." : "Değişiklik üretilemedi."));
          const result = current.result;
          if (operation === "explain" || operation === "review") { job = null; showText(result.text || result.summary); return; }
          const before = target?.files ?? {};
          const after = { ...before };
          if (!target && current.contract) after["PROJE.md"] = current.contract;
          for (const change of result.changes) { if (change.path === "act.images.json") continue; if (change.after === null) delete after[change.path]; else after[change.path] = change.after; }
          const entry = history(result.summary || request, "Wiqo", before, after, !result.needsDeviceCheck);
          const item = target
            ? { ...target, name: result.name || target.name, kind: kindOf(result.kind), files: after, history: [entry, ...target.history].slice(0, 30) }
            : { id: uuid(), name: result.name || request.split(/[.!?\n]/)[0].slice(0, 60) || "Yeni proje", kind: kindOf(result.kind), files: after, mode: "devices", history: [entry] };
          job = null;
          view = { ...view, tab: "mine", project: item.id, pane: "preview", file: null };
          await save(item);
          return;
        }
        await new Promise((resolve) => setTimeout(resolve, 1500));
      }
      throw new Error("İşlem beklenenden uzun sürdü. Proje değişmedi; biraz sonra yeniden dene.");
    } catch (error) {
      job = null;
      note(String(error.message || error));
      draw();
    }
  }

  function showText(text) {
    const dialog = el("dialog", "proj-dialog");
    const body = R()?.markdown ? R().markdown(text || "", false) : el("div", "", text);
    dialog.append(body, button("Kapat", "ui-button", () => dialog.close()));
    dialog.onclose = () => dialog.remove();
    document.body.append(dialog); dialog.showModal();
  }

  // ----- the studio -----

  function studio(item) {
    if (!item) { view.project = null; return home(); }
    const page = el("div", "proj-studio");
    const head = el("div", "proj-head");
    const back = button("", "icon-button", () => { view.project = null; draw(); }); back.title = "Projeler"; back.append(icon("chevron.left", 15));
    const name = el("input", "proj-name"); name.value = item.name; name.onchange = () => { const value = name.value.trim(); if (value) save({ ...item, name: value.slice(0, 60) }); };
    const seg = el("div", "ui-segmented");
    for (const [pane, label] of [["preview", item.kind === "python" ? "Çalıştır" : "Önizleme"], ["code", "Kod"], ["history", "Geçmiş"]]) seg.append(button(label, view.pane === pane ? "on" : "", () => { view.pane = pane; draw(); }));
    const actions = el("div", "proj-tools");
    if (item.kind !== "python") { const pub = button("Yayınla", "ui-button primary", () => publishDialog(item)); actions.append(pub); }
    const more = button("", "icon-button", () => menu(item, more)); more.title = "Diğer"; more.append(icon("ellipsis", 15));
    actions.append(more);
    head.append(back, name, seg, actions);
    page.append(head);
    const box = el("p", "error small"); box.id = "projects-notice"; box.hidden = true; page.append(box);
    const body = el("div", "proj-pane");
    if (view.pane === "preview") body.append(item.kind === "python" ? pythonPane(item) : previewPane(item));
    else if (view.pane === "code") body.append(codePane(item));
    else body.append(historyPane(item));
    page.append(body);
    if (job && job.projectId === item.id) page.append(timeline(job));
    const ask = el("form", "proj-ask compact");
    const input = el("textarea"); input.rows = 1; input.placeholder = "Ne değişsin? “Başlığı büyüt, renkleri koyu yap”…"; input.disabled = Boolean(job);
    const go = el("button", "ui-button primary", "Değiştir"); go.type = "submit"; go.disabled = Boolean(job);
    ask.onsubmit = (event) => { event.preventDefault(); if (input.value.trim()) write(input.value.trim(), item); };
    input.onkeydown = (event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); ask.requestSubmit(); } };
    ask.append(input, go);
    page.append(ask);
    return page;
  }

  function previewPane(item) {
    const html = bundleWeb(item.files);
    if (!html) return el("div", "card muted", "Bu projede sayfa (index.html) yok.");
    const frame = el("iframe", "proj-frame");
    frame.setAttribute("sandbox", "allow-scripts allow-forms allow-modals allow-popups");
    frame.srcdoc = html;
    return frame;
  }

  function pythonPane(item) {
    const box = el("div", "proj-run");
    const out = el("pre", "ui-code", "Çalıştır'a bas; çıktı burada görünür.");
    const run = button("Çalıştır", "ui-button primary", async () => {
      run.disabled = true; out.textContent = "Çalışıyor…";
      try {
        const files = Object.entries(item.files).filter(([path]) => path.endsWith(".py") || path.endsWith(".txt") || path.endsWith(".csv") || path.endsWith(".json")).map(([path, content]) => ({ path, content }));
        const result = await api("POST", "code-sync/run", { files });
        out.textContent = [result.stdout, result.stderr && `\n${result.stderr}`, result.timedOut ? "\n(Süre doldu.)" : "", `\n— çıkış kodu ${result.exitCode} · ${Math.round(result.durationMs)} ms`].filter(Boolean).join("");
      } catch (error) { out.textContent = String(error.message || error); }
      finally { run.disabled = false; }
    });
    box.append(run, out);
    return box;
  }

  function codePane(item) {
    const box = el("div", "proj-code");
    const list = el("div", "proj-files");
    const paths = Object.keys(item.files).sort((a, b) => (a === "index.html" ? -1 : b === "index.html" ? 1 : a.localeCompare(b)));
    const current = view.file && item.files[view.file] !== undefined ? view.file : paths[0];
    for (const path of paths) list.append(button(path, `proj-file${path === current ? " on" : ""}`, () => { view.file = path; draw(); }));
    const add = button("+ Dosya", "proj-file add", () => {
      const path = prompt("Dosya adı (ör. sayfa.html, veri.json):");
      if (!path) return;
      if (!syncable(path, "") || item.files[path] !== undefined) return toast("Bu ad kullanılamaz.", "bad");
      view.file = path;
      save({ ...item, files: { ...item.files, [path]: "" }, history: [history(`${path} eklendi`, "Bilgisayar", item.files, { ...item.files, [path]: "" }), ...item.history].slice(0, 30) });
    });
    list.append(add);
    const editor = el("textarea", "proj-editor"); editor.spellcheck = false;
    editor.value = current ? item.files[current] : "";
    editor.onkeydown = (event) => { if (event.key === "Tab") { event.preventDefault(); editor.setRangeText("  ", editor.selectionStart, editor.selectionEnd, "end"); } if (event.key === "s" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); commit(); } };
    const commit = () => {
      if (!current || editor.value === item.files[current]) return;
      const after = { ...item.files, [current]: editor.value };
      save({ ...item, files: after, history: [history(`${current} düzenlendi`, "Bilgisayar", item.files, after), ...item.history].slice(0, 30) });
    };
    editor.onblur = commit;
    box.append(list, editor);
    return box;
  }

  function historyPane(item) {
    const box = el("div", "proj-history");
    item.history.forEach((entry, index) => {
      const row = el("div", "proj-history-row");
      const text = el("div", "ui-list-text");
      text.append(el("div", "ui-strong", entry.summary), el("div", "ui-muted small", `${entry.source} · ${relative(entry.date)} · ${entry.changes.length} dosya${entry.verified === false ? " · doğrulanmadı" : ""}`));
      row.append(text);
      if (index === 0 && entry.changes.length && item.history.length > 1) row.append(button("Geri al", "ui-button", () => {
        const after = { ...item.files };
        for (const change of entry.changes) { if (change.before === undefined) delete after[change.path]; else after[change.path] = change.before; }
        save({ ...item, files: after, history: [history(`Geri alındı: ${entry.summary}`, "Geri alma", item.files, after), ...item.history].slice(0, 30) });
      }));
      box.append(row);
    });
    return box;
  }

  function menu(item, anchor) {
    const pop = el("div", "popover proj-menu");
    const close = () => { pop.remove(); document.removeEventListener("mousedown", outside, true); };
    const outside = (event) => { if (!pop.contains(event.target)) close(); };
    const entry = (label, run, danger) => { const b = button(label, `pop-item${danger ? " danger-text" : ""}`, () => { close(); run(); }); pop.append(b); };
    entry("Kodu açıkla", () => write("Bu projenin ne yaptığını ve nasıl çalıştığını kısaca açıkla.", item, "explain"));
    entry("Gözden geçir", () => write("Bu projeyi gözden geçir: hataları ve iyileştirmeleri söyle.", item, "review"));
    entry(item.mode === "deviceOnly" ? "Cihazlar arasında eşitle" : "Sadece bu bilgisayarda tut", () => save({ ...item, mode: item.mode === "deviceOnly" ? "devices" : "deviceOnly" }));
    entry("Sil", async () => {
      if (!confirm(`“${item.name}” silinsin mi? ${item.hubId ? "Diğer cihazlardan da silinir." : ""}`)) return;
      if (item.hubId) await api("DELETE", `code-sync/projects/${item.hubId}`).catch((error) => toast(String(error), "bad"));
      projects = projects.filter((row) => row.id !== item.id);
      view.project = null; await persist(); draw();
    }, true);
    const rect = anchor.getBoundingClientRect();
    Object.assign(pop.style, { position: "fixed", top: `${rect.bottom + 6}px`, right: `${window.innerWidth - rect.right}px`, zIndex: 50 });
    document.body.append(pop);
    setTimeout(() => document.addEventListener("mousedown", outside, true));
  }

  const slugFrom = (text) => text.toLocaleLowerCase("tr").replace(/ç/g, "c").replace(/ğ/g, "g").replace(/ı/g, "i").replace(/ö/g, "o").replace(/ş/g, "s").replace(/ü/g, "u").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);

  function publishDialog(item) {
    const dialog = el("dialog", "proj-dialog");
    dialog.append(el("div", "ui-h3", "Yayınla"), el("div", "ui-muted small", "Sayfa herkesin açabileceği bir adreste yayınlanır; değiştirince yeniden yayınla."));
    const field = el("label", "ui-field"); field.append(el("span", "ui-muted", "Adres"));
    const input = el("input"); input.value = item.site?.slug || slugFrom(item.name) || "sitem"; field.append(input);
    const status = el("div", "small");
    const link = el("div");
    const showLink = () => {
      link.replaceChildren();
      if (!item.site?.url) return;
      const row = el("div", "ui-row");
      const a = button(item.site.url, "link", () => R().openLink(item.site.url));
      const copy = button("Kopyala", "ui-button", () => navigator.clipboard.writeText(item.site.url).then(() => toast("Kopyalandı.")));
      const down = button("Kaldır", "ui-button destructive", async () => {
        try { await api("DELETE", `code-sync/sites/${encodeURIComponent(item.site.slug)}`); item.site = null; await save({ ...item }); showLink(); status.textContent = "Yayından kaldırıldı."; }
        catch (error) { status.textContent = String(error.message || error); }
      });
      row.append(a, copy, down); link.append(row);
    };
    const go = button(item.site?.url ? "Güncelle" : "Yayınla", "ui-button primary", async () => {
      go.disabled = true; status.textContent = "Yayınlanıyor…";
      try {
        const site = await api("PUT", "code-sync/sites", { slug: slugFrom(input.value), projectId: item.id, html: bundleWeb(item.files), origins: networkOrigins(item.files) });
        item.site = { slug: site.slug, url: site.url };
        await save({ ...item });
        status.textContent = "Yayında."; go.textContent = "Güncelle"; showLink();
      } catch (error) { status.textContent = String(error.message || error); }
      finally { go.disabled = false; }
    });
    const row = el("div", "ui-row"); row.append(go, button("Kapat", "ui-button", () => dialog.close()));
    dialog.append(field, link, status, row);
    dialog.onclose = () => dialog.remove();
    document.body.append(dialog); dialog.showModal(); showLink();
  }

  async function keyDialog() {
    const dialog = el("dialog", "proj-dialog");
    dialog.append(el("div", "ui-h3", "Cihazlar arası projeler"), el("div", "ui-muted small", "Projeler uçtan uca şifreli eşitlenir; anahtar yalnızca cihazlarında. Telefondaki projeleri burada açmak için telefonun Projeler › Kurtarma kodu ekranındaki kodu gir."));
    const mine = el("div", "proj-code-box");
    try { const k = await loadKeys(); mine.append(el("div", "ui-muted small", "Bu bilgisayarın kodu"), el("code", "", k.recoveryCode)); } catch (error) { mine.append(el("div", "error small", String(error.message || error))); }
    const field = el("label", "ui-field"); field.append(el("span", "ui-muted", "Telefonun kurtarma kodu"));
    const input = el("input"); input.placeholder = "ABCDE-FGHIJ-…"; field.append(input);
    const status = el("div", "small");
    const use = button("Bu kodu kullan", "ui-button primary", async () => {
      try { await importRecoveryCode(input.value); status.textContent = "Kod kaydedildi; projeler alınıyor…"; await syncAll(); dialog.close(); }
      catch (error) { status.textContent = String(error.message || error); }
    });
    const row = el("div", "ui-row"); row.append(use, button("Kapat", "ui-button", () => dialog.close()));
    dialog.append(mine, field, status, row);
    dialog.onclose = () => dialog.remove();
    document.body.append(dialog); dialog.showModal();
  }

  let synced = false;
  window.addEventListener("wiqo-section", async (event) => {
    if (event.detail !== "projects") return;
    await load();
    draw();
    if (!synced) { synced = true; syncAll(); }
  });

  const handled = new Set();
  window.WiqoProjects = {
    open: async (id) => { await load(); view = { ...view, tab: "mine", project: id }; window.showSection("projects"); },
    /** The chat's project handoff (actTask {taskId, operation, request, project?}): written here once, in the named project when this computer has it. */
    run: async (task) => {
      await load();
      const target = projects.find((item) => item.id === task.project?.id || item.id === task.project?.projectId || item.name === task.project?.name) || null;
      window.showSection("projects");
      if (handled.has(task.taskId) || job) { view = { ...view, tab: "mine", project: target?.id || null }; draw(); return; }
      handled.add(task.taskId);
      const operation = ["create", "edit", "explain", "review"].includes(task.operation) ? task.operation : target ? "edit" : "create";
      view = { ...view, tab: "mine", project: target?.id || null };
      write(task.request, operation === "create" ? null : target, operation === "edit" && !target ? "create" : operation);
    },
  };
})();
