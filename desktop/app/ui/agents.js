// Ajanlar: everything Wiqo does for the person in one list (reminders, routines, watches, jobs, to-dos, goals, helpers), the same list the phones draw
// (backend agent/overview.ts). Each row says what it is, when it happens and where it stands; the buttons on a row are the actions the server allows.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);

  const ICONS = { reminder: "clock.badge.checkmark", routine: "arrow.clockwise", watch: "eye", job: "timer", todo: "checklist", goal: "sparkle", helper: "sparkle" };
  const LABELS = { pause: "Duraklat", resume: "Sürdür", run: "Şimdi çalıştır", stop: "Durdur", complete: "Tamamlandı", delete: "Sil" };
  const GROUPS = [["now", "Şu an"], ["scheduled", "Zamanlı"], ["todos", "Yapılacaklar"], ["helpers", "Ajanlarım"], ["recent", "Son sonuçlar"]];
  const TONE = { attention: "warning", failed: "danger", running: "success", waiting: "warning", paused: "neutral", done: "neutral" };
  const STATE = { attention: "Bakman gerek", failed: "Başarısız", running: "Çalışıyor", waiting: "Bekliyor", paused: "Duraklatıldı", done: "Bitti" };

  let busy = false;

  const subtitle = (item) => {
    if (item.kind === "goal" && item.progress) return item.progress.total ? `${item.progress.done}/${item.progress.total} tamamlandı` : "Alt görev yok";
    // A long job (campaign): how many of its checkable items passed, then what it does or waits for.
    if (item.kind === "job" && item.progress && item.progress.total) return [`${item.progress.done}/${item.progress.total} madde`, item.detail].filter(Boolean).join(" · ");
    return item.when || item.detail || "";
  };

  function row(item) {
    const li = document.createElement("li");
    const icon = document.createElement("span");
    icon.className = "agent-icon";
    icon.append(window.WiqoIcons ? window.WiqoIcons.icon(ICONS[item.kind] || "sparkle", 16) : "");
    const copy = document.createElement("div");
    copy.className = "agent-copy";
    const title = document.createElement("div");
    title.className = "agent-title";
    title.textContent = item.title;
    const sub = document.createElement("div");
    sub.className = "muted small";
    sub.textContent = subtitle(item);
    copy.append(title, sub);
    if ((item.kind === "goal" || item.kind === "job") && item.progress && item.progress.total) {
      const track = document.createElement("div");
      track.className = "agent-track";
      const fill = document.createElement("div");
      fill.style.width = `${Math.round((item.progress.done / item.progress.total) * 100)}%`;
      track.append(fill);
      copy.append(track);
    }
    li.append(icon, copy);
    if (STATE[item.state] && item.state !== "done") {
      const pill = document.createElement("span");
      pill.className = `pill-status ${TONE[item.state] || "neutral"}`;
      pill.textContent = STATE[item.state];
      li.append(pill);
    }
    const actions = document.createElement("div");
    actions.className = "agent-actions";
    for (const action of item.actions) {
      if (!LABELS[action] || (action === "run" && item.state === "paused")) continue;
      const button = document.createElement("button");
      button.className = action === "delete" ? "danger" : "secondary small";
      button.textContent = LABELS[action];
      button.onclick = () => perform(item, action);
      actions.append(button);
    }
    // An automation's last runs (as the phones' history page): when, how it went, and what started it.
    const automation = /^automation:(\d+)$/.exec(item.ref || "");
    if (automation) {
      const history = document.createElement("button");
      history.className = "secondary small";
      history.textContent = "Geçmiş";
      history.onclick = () => runs(li, Number(automation[1]), history);
      actions.prepend(history);
    }
    li.append(actions);
    return li;
  }

  const RUN_STATE = { completed: "Tamamlandı", failed: "Başarısız", running: "Çalışıyor", queued: "Sırada", cancelled: "İptal edildi", waiting: "Bekliyor", awaiting_device: "PC bekleniyor", unknown: "Belirsiz" };
  const RUN_TONE = { completed: "success", failed: "danger", running: "warning", cancelled: "neutral" };
  async function runs(li, id, toggle) {
    const open = li.nextElementSibling?.classList.contains("agent-runs") ? li.nextElementSibling : null;
    if (open) { open.remove(); toggle.textContent = "Geçmiş"; return; }
    const box = document.createElement("li");
    box.className = "agent-runs";
    box.textContent = "Yükleniyor…";
    li.after(box);
    toggle.textContent = "Gizle";
    try {
      const value = await invoke("automation_runs", { id });
      box.replaceChildren();
      const list = (value.runs || []).slice(0, 10);
      if (!list.length) box.textContent = "Henüz çalışmadı.";
      for (const run of list) {
        const row = document.createElement("div");
        row.className = "agent-run";
        const head = document.createElement("div");
        head.className = "agent-run-head";
        const when = document.createElement("span");
        when.className = "muted small";
        when.textContent = new Date(run.startedAt).toLocaleString("tr-TR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
        const pill = document.createElement("span");
        pill.className = `pill-status ${RUN_TONE[run.status] || "neutral"}`;
        pill.textContent = RUN_STATE[run.status] || run.status;
        head.append(when, pill);
        row.append(head);
        if (run.eventNote) { const note = document.createElement("div"); note.className = "small"; note.textContent = `Tetikleyen: ${run.eventNote}`; row.append(note); }
        const said = run.error || run.reply;
        if (said) { const text = document.createElement("div"); text.className = `small ${run.error ? "error" : "muted"}`; text.textContent = said.length > 280 ? `${said.slice(0, 280)}…` : said; row.append(text); }
        box.append(row);
      }
    } catch (error) {
      box.textContent = String(error);
    }
  }

  async function perform(item, action) {
    if (busy) return;
    if (action === "delete" && !(await WiqoDialog.confirm({ title: `“${item.title}” silinsin mi?`, message: "Bu geri alınamaz.", confirm: "Sil" }))) return;
    busy = true;
    try {
      await invoke("agent_action", { reference: item.ref, action });
      await load();
    } catch (error) {
      showError(String(error));
    } finally {
      busy = false;
    }
  }

  function showError(message) {
    const box = $("agents-error");
    box.textContent = message || "";
    box.hidden = !message;
  }

  async function load() {
    try {
      const overview = await invoke("agents_overview");
      showError("");
      const host = $("agents-groups");
      host.replaceChildren();
      let total = 0;
      for (const [key, name] of GROUPS) {
        const items = overview[key] || [];
        if (!items.length) continue;
        total += items.length;
        const card = document.createElement("div");
        card.className = "card";
        const heading = document.createElement("div");
        heading.className = "overline";
        heading.textContent = name;
        const list = document.createElement("ul");
        list.className = "list agents";
        for (const item of items) list.append(row(item));
        card.append(heading, list);
        host.append(card);
      }
      $("agents-empty").hidden = total > 0;
    } catch (error) {
      showError(String(error));
    }
  }

  window.addEventListener("wiqo-section", (event) => {
    if (event.detail === "settings") load();
  });
})();
