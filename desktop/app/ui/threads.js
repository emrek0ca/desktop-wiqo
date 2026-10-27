// Ajanlar: the agent chat. A strip of the people you can talk to and one list of conversations on the left, the thread in the middle, and a side page (an agent's
// profile, a group's members, a job's steps) on the right instead of pop-up windows. Each message shows who wrote it; while an agent answers, its avatar types and
// the text arrives piece by piece (backend docs/agent-chat.md). The server finishes a turn even if the window closes, so the thread is read again when a turn
// ends and every few seconds (faster while a group's job runs).
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const $ = (id) => document.getElementById(id);

  const PERSONAS = [["general", "Genel"], ["research", "Araştırma"], ["developer", "Geliştirici"], ["execution", "Yürütme"]];
  const STYLES = [
    ["Neşeli", "Neşeli ve samimi konuşur; kısa yazar, ara sıra espri yapar."],
    ["Resmî", "Resmî ve saygılı konuşur; net, düzenli cümleler kurar."],
    ["Kısa ve net", "Çok kısa ve net konuşur; gereksiz söz etmez, doğrudan sonuca gider."],
    ["Mentor", "Sabırlı bir mentor gibi konuşur; nedenini anlatır, soru sorarak yönlendirir."]
  ];
  const POLL_MS = 8000;
  const POLL_WORK_MS = 3000;

  let lists = { bots: [], groups: [] };
  let open = null; // { thread, messages, work }
  let turn = null; // { id, typing: Set, live: Map }
  let query = "";
  let pollTimer = null;

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const fold = (text) => String(text).toLocaleLowerCase("tr-TR");
  const avatarSrc = (persona) => `img/agent-${PERSONAS.some(([id]) => id === persona) ? persona : "general"}.png`;
  const avatar = (persona, size = 28) => {
    const image = el("img", "t-avatar");
    image.src = avatarSrc(persona);
    image.alt = "";
    image.style.width = image.style.height = `${size}px`;
    return image;
  };
  const stack = (members, size = 24) => {
    const box = el("div", "t-stack");
    for (const member of members.slice(0, 3)) box.append(avatar(member.persona, size));
    return box;
  };
  const memberOf = (id) => open?.thread.members.find((member) => member.id === id);
  const activeWork = () => open?.work?.find((job) => ["running", "waiting_device", "waiting_input", "paused"].includes(job.state));

  function when(value) {
    if (!value) return "";
    const date = new Date(`${String(value).replace(" ", "T")}Z`);
    if (Number.isNaN(date.getTime())) return "";
    return date.toDateString() === new Date().toDateString()
      ? date.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" })
      : date.toLocaleDateString("tr-TR", { day: "numeric", month: "short" });
  }

  // ----- the list: people on top, then every conversation -----

  function row(thread) {
    const button = el("button", `t-row${open?.thread.id === thread.id ? " active" : ""}`);
    button.append(thread.kind === "group" ? stack(thread.members) : avatar(thread.members[0]?.persona, 34));
    const copy = el("div", "t-row-copy");
    const head = el("div", "t-row-head");
    head.append(el("span", "t-row-title ellipsis", thread.title), el("span", "muted small", when(thread.updatedAt)));
    copy.append(head, el("div", "muted small ellipsis", thread.preview || thread.members.map((member) => member.name).join(", ")));
    button.append(copy);
    button.onclick = () => go(thread);
    return button;
  }

  function renderList() {
    const needle = fold(query.trim());
    const people = $("t-people");
    people.replaceChildren();
    for (const bot of lists.bots.filter((item) => !needle || fold(item.title).includes(needle))) {
      const person = el("button", `t-person${open?.thread.id === bot.id ? " active" : ""}`, "");
      person.title = bot.title;
      person.append(avatar(bot.members[0]?.persona, 40), el("span", "small ellipsis", bot.title));
      person.onclick = () => go(bot);
      people.append(person);
    }
    const add = el("button", "t-person");
    add.title = "Yeni ajan";
    const plus = el("span", "t-plus", "+");
    add.append(plus, el("span", "small", "Yeni"));
    add.onclick = () => showSide({ kind: "agent", id: null });
    people.append(add);

    const box = $("threads-list-body");
    box.replaceChildren();
    const conversations = [...lists.groups, ...lists.bots]
      .filter((item) => item.kind === "group" || item.turnCount > 0)
      .filter((item) => !needle || fold(item.title).includes(needle) || fold(item.preview).includes(needle))
      .sort((a, b) => String(b.updatedAt ?? "").localeCompare(String(a.updatedAt ?? "")));
    box.append(el("div", "overline t-section", "Konuşmalar"));
    if (conversations.length === 0) box.append(el("div", "muted small t-empty", needle ? "Sonuç bulunamadı." : "Bir ajana dokunup konuşmaya başla ya da yeni bir grup kur."));
    for (const item of conversations) box.append(row(item));
  }

  async function loadList() {
    try {
      lists = await invoke("threads_list");
      $("threads-error").hidden = true;
    } catch (error) {
      $("threads-error").textContent = String(error);
      $("threads-error").hidden = false;
    }
    renderList();
  }

  // ----- the thread -----

  const bubble = (content) => { const box = el("div", "t-bubble md"); box.append(window.WiqoRender.markdown(content, false)); return box; };
  const typingBubble = () => { const box = el("div", "t-bubble t-typing"); box.append(el("span"), el("span"), el("span")); return box; };

  function agentLine(agentId, body, group) {
    const member = memberOf(agentId);
    const line = el("div", "t-line agent");
    line.append(avatar(member?.persona, 28));
    const column = el("div", "t-col");
    if (group && member) column.append(el("div", "t-name muted small", member.name));
    column.append(body);
    line.append(column);
    return line;
  }

  function renderMessages(stick = true) {
    const box = $("thread-messages");
    const scroll = $("thread-scroll");
    const nearEnd = scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 80;
    box.replaceChildren();
    if (!open) return;
    const group = open.thread.kind === "group";
    if (open.messages.length === 0 && !turn) {
      const hello = el("div", "t-hello");
      hello.append(group ? stack(open.thread.members, 40) : avatar(open.thread.members[0]?.persona, 56));
      hello.append(el("h3", "", open.thread.title), el("p", "muted small", group ? "Bir şey yaz; ekipten uygun olan cevap verir. @isim ile birini çağır, “birbirinizle konuşun” de ya da ortak bir iş ver." : "Merhaba de ya da bir iş ver."));
      box.append(hello);
    }
    for (const message of open.messages) {
      if (message.role === "note") box.append(el("div", "t-note muted small", message.content));
      else if (message.role === "user") { const line = el("div", "t-line user"); line.append(el("div", "t-bubble user", message.content)); box.append(line); }
      else box.append(agentLine(message.agentId, bubble(message.content), group));
    }
    if (turn) {
      for (const [agentId, text] of turn.live) box.append(agentLine(agentId, bubble(text), group));
      for (const agentId of turn.typing) if (!turn.live.has(agentId)) box.append(agentLine(agentId, typingBubble(), group));
    }
    if (stick || nearEnd) scroll.scrollTop = scroll.scrollHeight;
  }

  function renderHead() {
    const head = $("thread-head");
    head.replaceChildren();
    if (!open) return;
    const { thread } = open;
    const who = el("button", "t-who");
    who.append(thread.kind === "group" ? stack(thread.members) : avatar(thread.members[0]?.persona, 34));
    const copy = el("div", "t-head-copy");
    copy.append(el("div", "t-head-title", thread.title), el("div", "muted small ellipsis", thread.kind === "group" ? thread.members.map((member) => member.name).join(", ") : "Profil ve ayarlar"));
    who.append(copy);
    who.title = thread.kind === "group" ? "Grup bilgisi" : "Ajan profili";
    who.onclick = () => showSide(thread.kind === "group" ? { kind: "group", thread } : { kind: "agent", id: thread.members[0]?.id });
    head.append(who, el("span", "spacer"));
    const clear = el("button", "secondary small", thread.kind === "group" ? "Grubu sil" : "Geçmişi temizle");
    clear.onclick = async () => {
      const group = thread.kind === "group";
      const ok = await window.WiqoDialog.confirm({ title: group ? "Grup silinsin mi?" : "Konuşma geçmişi temizlensin mi?", message: group ? `“${thread.title}” ve mesajları silinir.` : `${thread.title} ile olan mesajların silinir; ajan silinmez.`, confirm: group ? "Sil" : "Temizle", danger: true });
      if (!ok) return;
      try { await invoke("thread_delete", { thread: thread.id }); } catch (error) { return note(String(error)); }
      if (group) { open = null; showPane(); hideSide(); } else { open.messages = []; open.work = []; renderMessages(); renderWork(); }
      await loadList();
    };
    head.append(clear);
  }

  function renderWork() {
    const box = $("thread-work");
    box.replaceChildren();
    const job = open && (activeWork() ?? open.work?.[0]);
    box.hidden = !job;
    if (!job) return;
    const active = activeWork() === job;
    const head = el("button", "t-work-head");
    const involved = open.thread.members.filter((member) => job.steps.some((step) => step.agentId === member.id));
    head.append(stack(involved.length ? involved : open.thread.members, 22));
    const copy = el("div", "t-work-copy");
    const status = job.state === "done" ? "Bitti" : job.state === "failed" ? "Tamamlanamadı" : job.state === "cancelled" ? "Durduruldu" : job.state === "paused" ? "Duraklatıldı" : `${job.done}/${job.steps.length}${job.progress ? ` · ${job.progress}` : " adım"}`;
    copy.append(el("div", "t-head-title", job.title), el("div", "muted small ellipsis", status));
    head.append(copy);
    if (active) { const track = el("div", "agent-track"); const fill = el("div"); fill.style.width = `${Math.round((job.done / Math.max(job.steps.length, 1)) * 100)}%`; track.append(fill); head.append(track); }
    head.onclick = () => showSide({ kind: "work", job });
    box.append(head);
    if (active) {
      const stop = el("button", "secondary small", "Durdur");
      stop.onclick = async () => { try { await invoke("work_cancel", { task: job.taskId }); } catch (error) { note(String(error)); } await openThread(open.thread.id, true); };
      box.append(stop);
    }
  }

  function showPane() {
    $("thread-empty").hidden = Boolean(open);
    $("thread-body").hidden = !open;
    renderHead();
    renderWork();
    renderMessages();
    renderMentions();
    syncSend();
    renderList();
  }

  function syncSend() {
    $("t-send").hidden = Boolean(turn);
    $("t-stop").hidden = !turn;
    $("t-send").disabled = !open;
  }

  function note(text) {
    if (!open) return;
    open.messages.push({ role: "note", content: text });
    renderMessages();
  }

  // The "@" word being typed, and the members it could be.
  function renderMentions() {
    const box = $("thread-mentions");
    box.replaceChildren();
    const input = $("t-input");
    const at = input.value.lastIndexOf("@");
    const word = at >= 0 ? input.value.slice(at + 1) : null;
    if (!open || open.thread.kind !== "group" || word === null || /\s/u.test(word)) { box.hidden = true; return; }
    const options = open.thread.members.filter((member) => !word || fold(member.name).startsWith(fold(word)));
    box.hidden = options.length === 0;
    for (const member of options) {
      const chip = el("button", "t-chip");
      chip.type = "button";
      chip.append(avatar(member.persona, 18), document.createTextNode(` ${member.name}`));
      chip.onclick = () => { input.value = `${input.value.slice(0, at)}@${member.name} `; renderMentions(); input.focus(); };
      box.append(chip);
    }
  }

  /** Direct chats leave this page for the main chat; groups keep their pane until they run on the main engine too. */
  function go(thread) {
    window.WiqoChat.open(thread.id);
  }

  async function openThread(id, silent = false) {
    try {
      const result = await invoke("thread_open", { thread: id });
      if (silent && open && open.thread.id === id && !turn) {
        const same = open.messages.filter((message) => message.role !== "note").length === result.messages.length;
        open.thread = result.thread;
        open.work = result.work || [];
        renderWork();
        renderHead();
        if (!same) { open.messages = result.messages; renderMessages(); }
        schedulePoll();
        return;
      }
      open = { thread: result.thread, messages: result.messages, work: result.work || [] };
    } catch (error) {
      if (!silent) { $("threads-error").textContent = String(error); $("threads-error").hidden = false; }
      return;
    }
    if (!silent) { turn = null; hideSide(); }
    showPane();
    schedulePoll();
  }

  function schedulePoll() {
    clearTimeout(pollTimer);
    pollTimer = setTimeout(() => {
      if (document.body.dataset.section === "threads" && open && !turn && !document.hidden) { openThread(open.thread.id, true); loadList(); } else schedulePoll();
    }, activeWork() ? POLL_WORK_MS : POLL_MS);
  }

  // ----- sending -----

  async function send() {
    const input = $("t-input");
    const text = input.value.trim();
    if (!open || !text || turn) return;
    input.value = "";
    open.messages.push({ role: "user", content: text });
    turn = { id: crypto.randomUUID(), typing: new Set(), live: new Map() };
    syncSend();
    renderMentions();
    renderMessages();
    try {
      await invoke("thread_send", { turn: turn.id, thread: open.thread.id, message: text });
    } catch (error) {
      turn = null;
      note(String(error));
      syncSend();
    }
  }

  function onEvent(thread, event) {
    if (!open || open.thread.id !== thread || !turn) return;
    switch (event.type) {
      case "agent_typing": turn.typing.add(event.agentId); break;
      case "agent_delta": turn.live.set(event.agentId, (turn.live.get(event.agentId) || "") + event.text); break;
      case "agent_idle": turn.typing.delete(event.agentId); turn.live.delete(event.agentId); break;
      case "agent_message":
        turn.typing.delete(event.agentId);
        turn.live.delete(event.agentId);
        open.messages.push({ role: "assistant", agentId: event.agentId, content: event.content });
        break;
      case "system_note": open.messages.push({ role: "note", content: event.text }); break;
      case "error": open.messages.push({ role: "note", content: event.message }); break;
      case "work_started": openThread(open.thread.id, true); break;
      case "turnEnd": {
        const failed = event.error;
        turn = null;
        if (failed) open.messages.push({ role: "note", content: failed });
        syncSend();
        renderMessages();
        loadList();
        openThread(open.thread.id, true);
        return;
      }
      default: return;
    }
    renderMessages(false);
  }

  listen("wiqo", (event) => {
    const payload = event.payload;
    if (payload.type !== "thread" || !turn || payload.turn !== turn.id) return;
    onEvent(payload.thread, payload.event);
  });

  // ----- the side page: an agent, a group, a job (no pop-ups) -----

  function hideSide() { $("thread-side").hidden = true; $("thread-side").replaceChildren(); }

  const field = (label, control) => { const box = el("label", "t-field"); box.append(el("span", "overline", label), control); return box; };

  function sideFrame(title, onSave, saveLabel) {
    const side = $("thread-side");
    side.replaceChildren();
    side.hidden = false;
    const head = el("div", "t-side-head");
    const close = el("button", "icon-button", "×");
    close.title = "Kapat";
    close.onclick = hideSide;
    head.append(el("h3", "", title), el("span", "spacer"), close);
    const body = el("div", "t-side-body");
    const error = el("p", "error small");
    error.hidden = true;
    const foot = el("div", "t-side-foot");
    if (onSave) {
      const save = el("button", "primary", saveLabel);
      save.onclick = async () => { save.disabled = true; error.hidden = true; try { await onSave(); } catch (failure) { error.textContent = String(failure); error.hidden = false; } save.disabled = false; };
      foot.append(save);
    }
    side.append(head, body, error, foot);
    return { body, foot, error };
  }

  async function showSide(target) {
    if (target.kind === "group") return groupSide(target.thread);
    if (target.kind === "work") return workSide(target.job);
    return agentSide(target.id);
  }

  async function agentSide(id) {
    let current = null;
    if (id) {
      try { current = (await invoke("agent_list")).agents.find((agent) => agent.id === id) ?? null; } catch (error) { return note(String(error)); }
    }
    const builtin = Boolean(current?.isBuiltin);
    const name = el("input"); name.type = "text"; name.maxLength = 60; name.value = current?.name ?? ""; name.placeholder = "Örn. Selin";
    const role = el("textarea"); role.rows = 3; role.maxLength = 1500; role.value = current?.role ?? ""; role.disabled = builtin; role.placeholder = "Ne yapar? (en az 10 karakter)";
    const personality = el("textarea"); personality.rows = 3; personality.maxLength = 600; personality.value = current?.personality ?? ""; personality.placeholder = "Nasıl konuşur? Örn. neşeli, kısa yazar";
    const persona = el("select");
    for (const [value, label] of PERSONAS) { const option = el("option", "", label); option.value = value; persona.append(option); }
    persona.value = current?.persona ?? "general"; persona.disabled = builtin;
    const frame = sideFrame(current ? current.name : "Yeni ajan", async () => {
      const body = builtin ? { name: name.value, personality: personality.value } : { name: name.value, role: role.value, personality: personality.value, persona: persona.value };
      const saved = await invoke("agent_save", { id: current?.id ?? null, body });
      await loadList();
      await window.WiqoChat.open(`agent-dm-${saved.agent.id}`);
    }, current ? "Kaydet" : "Oluştur");
    const preview = avatar(persona.value, 72);
    persona.onchange = () => { preview.src = avatarSrc(persona.value); };
    const hero = el("div", "t-hero");
    hero.append(preview);
    const templates = el("div", "t-templates");
    for (const [label, text] of STYLES) { const chip = el("button", "t-chip", label); chip.type = "button"; chip.onclick = () => { personality.value = text; }; templates.append(chip); }
    frame.body.append(hero, field("Ad", name), field("Görev", role), field("Konuşma tarzı", personality), templates, ...(builtin ? [] : [field("Maskot", persona)]));
    if (current?.memory) {
      const memory = el("div", "t-memory muted small", current.memory);
      const forget = el("button", "link small danger-text", "Hafızayı sıfırla");
      forget.onclick = async () => {
        if (!(await window.WiqoDialog.confirm({ title: "Hafıza sıfırlansın mı?", message: "Ajanın senin hakkında hatırladıkları silinir.", confirm: "Sıfırla", danger: true }))) return;
        try { await invoke("agent_forget", { id: current.id }); memory.remove(); forget.remove(); await loadList(); } catch (error) { note(String(error)); }
      };
      frame.body.append(field("Hatırladıkları", memory), forget);
    }
    if (current && !builtin) {
      const remove = el("button", "link small danger-text", "Ajanı sil");
      remove.onclick = async () => {
        if (!(await window.WiqoDialog.confirm({ title: `${current.name} silinsin mi?`, message: "Ajan ve onunla olan konuşma kaldırılır; gruplardan çıkar.", confirm: "Sil", danger: true }))) return;
        try { await invoke("agent_remove", { id: current.id }); } catch (error) { return note(String(error)); }
        hideSide(); open = null; showPane(); await loadList();
      };
      frame.foot.append(remove);
    }
  }

  function groupSide(existing) {
    const candidates = lists.bots.map((bot) => bot.members[0]).filter(Boolean);
    const title = el("input"); title.type = "text"; title.placeholder = "Grubun adı (isteğe bağlı)"; title.value = existing?.title ?? "";
    const picks = new Set((existing?.members ?? []).map((member) => member.id));
    const list = el("div", "t-picks");
    for (const member of candidates) {
      const item = el("label", "t-pick");
      const box = el("input"); box.type = "checkbox"; box.checked = picks.has(member.id);
      box.onchange = () => { if (box.checked && picks.size >= 4) { box.checked = false; return; } if (box.checked) picks.add(member.id); else picks.delete(member.id); };
      item.append(box, avatar(member.persona, 24), el("span", "", member.name));
      list.append(item);
    }
    const frame = sideFrame(existing ? "Grup bilgisi" : "Yeni grup", async () => {
      const agentIds = candidates.map((member) => member.id).filter((id) => picks.has(id));
      if (agentIds.length < 2) throw new Error("Grup için 2-4 ajan seç.");
      const body = { title: title.value.trim() || undefined, agentIds };
      const result = existing ? await invoke("thread_update", { thread: existing.id, body }) : await invoke("thread_create", { body });
      await loadList();
      await openThread(result.thread.id);
    }, existing ? "Kaydet" : "Oluştur");
    frame.body.append(field("Ad", title), field("Üyeler (2-4 ajan)", list), el("p", "muted small", "Bir mesaja uygun olan ajan cevap verir. @isim ile birini çağırabilir, “birbirinizle konuşun” diyerek kendi aralarında konuşturabilir ya da ortak bir iş verebilirsin."));
  }

  function workSide(job) {
    const frame = sideFrame(job.title, null, "");
    for (const step of job.steps) {
      const item = el("div", "t-step");
      item.append(el("span", `t-step-dot ${step.status}`, step.status === "done" ? "✓" : step.status === "failed" ? "✕" : step.status === "running" ? "…" : "·"), el("span", "t-step-title", step.title), el("span", "muted small", step.agentName || ""));
      frame.body.append(item);
    }
    if (job.summary) frame.body.append(field("Sonuç", el("div", "md small", job.summary)));
  }

  // ----- wiring -----

  $("t-new-group").onclick = () => groupSide(null);
  $("t-send").onclick = send;
  $("t-stop").onclick = () => { if (open) invoke("thread_stop", { thread: open.thread.id }).catch(() => undefined); };
  $("t-input").addEventListener("input", renderMentions);
  $("t-input").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) { event.preventDefault(); send(); }
  });
  $("t-search").addEventListener("input", (event) => { query = event.target.value; renderList(); });

  window.addEventListener("wiqo-section", async (event) => {
    if (event.detail !== "threads") return;
    $("top-title").textContent = "Ajanlar";
    await loadList();
    if (open) await openThread(open.thread.id, true);
  });
})();
