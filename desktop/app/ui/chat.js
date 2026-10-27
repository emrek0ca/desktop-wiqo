// The chat: conversations on the left, the reply streaming in the middle (text first, then its cards), this computer's steps and files on the
// right. PC steps run here; the corner panel asks before anything that changes, deletes or sends.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const R = window.WiqoRender;
  const $ = (id) => document.getElementById(id);
  const uuid = () => crypto.randomUUID();

  let conversation = uuid();
  let conversations = [];
  let messages = [];
  // The members of an open group chat (agent-group-<id>) by id: their names head their replies.
  let members = new Map();
  let turn = null;
  let task = null;
  let pending = [];
  let renderQueued = false;

  const toast = (message, tone) => {
    const box = $("toast");
    box.textContent = message;
    box.className = `toast ${tone || ""}`;
    box.hidden = false;
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => (box.hidden = true), 3200);
  };

  // ----- conversation list -----

  function group(date) {
    const day = new Date(date);
    if (isNaN(day)) return "Daha eski";
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const diff = (today - new Date(day.getFullYear(), day.getMonth(), day.getDate())) / 86_400_000;
    if (diff <= 0) return "Bugün";
    if (diff <= 1) return "Dün";
    if (diff <= 7) return "Bu hafta";
    if (diff <= 30) return "Bu ay";
    return "Daha eski";
  }

  let listSignature = "";

  function renderList() {
    const box = $("conversation-list");
    const query = $("search").value.trim().toLocaleLowerCase("tr");
    // The list is only redrawn when it changed (it used to flash after every answer).
    const signature = JSON.stringify([conversations.map((item) => [item.id, item.title, item.updatedAt, item.preview]), conversation, query]);
    if (signature === listSignature) return;
    listSignature = signature;
    box.replaceChildren();
    const items = conversations.filter((item) => !query || `${item.title || ""} ${item.preview || ""}`.toLocaleLowerCase("tr").includes(query));
    let last = null;
    for (const item of items) {
      const label = group(item.updatedAt);
      if (label !== last) {
        box.append(R.el("div", "conv-group", label));
        last = label;
      }
      const row = R.el("button", `conv${item.id === conversation && document.body.dataset.section === "chat" ? " on" : ""}`);
      row.type = "button";
      row.title = item.preview || "";
      row.append(R.el("span", "conv-title", item.title || item.preview || "Sohbet"));
      row.onclick = () => open(item.id);
      box.append(row);
    }
    if (!items.length) box.append(R.el("div", "conv-empty", query ? "Eşleşen sohbet yok." : "Sohbetlerin burada görünür."));
  }

  async function loadConversations() {
    try {
      const value = await invoke("chat_conversations");
      conversations = value.conversations || [];
    } catch {
      conversations = [];
    }
    renderList();
    setTitle();
  }

  function setTitle() {
    if (document.body.dataset.section !== "chat") return;
    const current = conversations.find((item) => item.id === conversation);
    $("top-title").textContent = current?.title || (messages.length ? messages[0].text.slice(0, 60) : "Yeni sohbet");
  }

  const isGroup = () => conversation.startsWith("agent-group-");

  async function open(id) {
    if (turn) return toast("Çalışan cevap bitmeden sohbet değişmez; önce Durdur.", "bad");
    conversation = id;
    messages = [];
    window.showSection("chat");
    render();
    renderList();
    members = new Map();
    if (isGroup()) {
      try {
        const opened = await invoke("thread_open", { thread: id });
        members = new Map((opened.thread?.members || []).map((member) => [member.id, member.name]));
      } catch { /* the names are missing; the replies still show */ }
    }
    try {
      const value = await invoke("chat_messages", { conversation: id });
      messages = (value.messages || []).map((item) => ({
        role: item.role === "user" ? "user" : "assistant",
        text: item.role !== "user" && item.agentId && members.has(item.agentId) ? `**${members.get(item.agentId)}**\n\n${item.content || ""}` : item.content || "",
        widgets: item.widgets || [],
        artifacts: item.artifacts || [],
        steps: [],
        confirmations: [],
      }));
    } catch (error) {
      toast(String(error), "bad");
    }
    render(true);
    setTitle();
  }

  function fresh() {
    if (turn) return toast("Çalışan cevap bitmeden yeni sohbet açılmaz; önce Durdur.", "bad");
    conversation = uuid();
    messages = [];
    window.showSection("chat");
    render(true);
    renderList();
    setTitle();
    $("chat-input").focus();
  }

  // ----- messages -----

  // ----- the thinking timeline (iOS ThinkingTrace) -----

  const WORK_PHASES = new Set(["plan", "tool", "device", "recover", "resume"]);
  const newTrace = () => ({ steps: [], startedAt: Date.now(), endedAt: null, persona: null, didThink: false, failed: false, local: 0 });
  const settled = (label) => label.trim().replace(/[….]+$/, "");

  function closeRunning(trace, state) {
    for (const step of trace.steps) if (step.state === "running") Object.assign(step, { state, endedAt: Date.now() });
  }

  function note(trace, label) {
    label = String(label || "").trim();
    if (!label || trace.endedAt || trace.steps.at(-1)?.label === label) return;
    closeRunning(trace, "done");
    trace.steps.push({ id: `local:${++trace.local}`, phase: "other", label, state: "running", startedAt: Date.now() });
  }

  // A backend `activity` (or a PC step): opens, adopts or closes a line by id.
  function apply(trace, { id, phase, label, state, persona }) {
    if (persona && !trace.endedAt) trace.persona = persona;
    label = String(label || "").trim();
    if (!id || !label) return;
    const existing = trace.steps.find((step) => step.id === id);
    if (existing) {
      if (existing.state === "running" && state !== "running") Object.assign(existing, { state, endedAt: Date.now() });
      return;
    }
    if (trace.endedAt) return;
    const last = trace.steps.at(-1);
    if (last && last.id.startsWith("local:") && last.label === label) {
      Object.assign(last, { id, phase });
      if (state !== "running" && last.state === "running") Object.assign(last, { state, endedAt: Date.now() });
      return;
    }
    if (state === "running") closeRunning(trace, "done");
    trace.steps.push({ id, phase: phase || "other", label, state, startedAt: Date.now(), endedAt: state === "running" ? null : Date.now() });
  }

  function finishTrace(trace, failed) {
    if (!trace || trace.endedAt) return;
    closeRunning(trace, failed ? "failed" : "done");
    trace.failed = failed;
    trace.endedAt = Date.now();
  }

  const duration = (ms) => {
    const seconds = Math.max(1, Math.round(ms / 1000));
    if (seconds < 60) return `${seconds} sn`;
    const minutes = Math.floor(seconds / 60);
    return seconds % 60 ? `${minutes} dk ${seconds % 60} sn` : `${minutes} dk`;
  };

  const showsTimeline = (trace) => trace.steps.some((step) => WORK_PHASES.has(step.phase)) || trace.steps.length >= 3;

  function summaryOf(trace) {
    const time = duration((trace.endedAt || Date.now()) - trace.startedAt);
    if (!showsTimeline(trace) && trace.didThink) return `${time} düşündü`;
    if (trace.failed || trace.steps.some((step) => step.state === "failed")) return `${trace.steps.length} adım · ${time} · bir adım tamamlanamadı`;
    return `${trace.steps.length} adımda tamamlandı · ${time}`;
  }

  const PERSONAS = new Set(["general", "research", "developer", "execution"]);
  function mascot(trace, working) {
    const image = R.el("img", `mascot${working ? " working" : ""}`);
    image.src = `img/agent-${PERSONAS.has(trace.persona) ? trace.persona : "general"}.png`;
    image.alt = "";
    return image;
  }

  function traceNode(trace, live) {
    if (live && !trace.endedAt) {
      const row = R.el("div", "trace live");
      const current = [...trace.steps].reverse().find((step) => step.state === "running");
      row.append(mascot(trace, true), R.el("span", "shimmer", current ? current.label : trace.didThink ? "Düşünüyor…" : "Hazırlanıyor…"));
      return row;
    }
    if (!trace.steps.length && !trace.didThink) return null;
    if (!showsTimeline(trace) && !trace.didThink) return null;
    const details = R.el("details", "trace");
    const summary = R.el("summary");
    summary.append(window.WiqoIcons.icon("chevron.right", 11, 0.3, "trace-chevron"), mascot(trace, false), R.el("span", "", summaryOf(trace)));
    details.append(summary);
    const list = R.el("div", "trace-steps");
    for (const step of trace.steps) {
      const row = R.el("div", `trace-step ${step.state}`);
      const mark = step.state === "running" ? R.el("span", "spinner tiny") : window.WiqoIcons.icon(step.state === "failed" ? "exclamationmark.circle.fill" : "checkmark.circle.fill", 13);
      row.append(mark, R.el("span", "", settled(step.label)));
      list.append(row);
    }
    details.append(list);
    return details;
  }

  function context(message, latest) {
    return {
      latest,
      toast,
      send: (text) => send(text),
      answer: (option, taskId, questionId) => send(option, { taskId, questionId }),
      retry: () => {
        const lastUser = [...messages].reverse().find((item) => item.role === "user");
        if (lastUser) send(lastUser.text);
      },
    };
  }

  const userRows = new WeakMap();

  function messageNode(message, index) {
    const latest = index === messages.length - 1;
    if (message.role === "user") {
      const cached = userRows.get(message);
      if (cached) return cached;
      const row = R.el("div", "msg user");
      const bubble = R.el("div", "bubble", message.text);
      if (message.images?.length) {
        const thumbs = R.el("div", "thumbs");
        for (const image of message.images) {
          const img = R.el("img");
          img.src = image;
          thumbs.append(img);
        }
        row.append(thumbs);
      }
      row.append(bubble);
      userRows.set(message, row);
      return row;
    }
    return assistantRow(message, latest);
  }

  // An assistant message is four parts kept apart (the thinking trace, the text, the cards, the tools), each redrawn only when its own content changed:
  // a streamed word used to rebuild the whole conversation, so pictures reloaded, cards flashed and the page seemed to refresh on its own.
  function assistantPartNodes(message, latest, live) {
    const parts = { trace: [], text: [], cards: [], tools: [] };
    if (message.trace) {
      const node = traceNode(message.trace, live);
      if (node) parts.trace.push(node);
    }
    if (message.text) parts.text.push(R.markdown(message.text, live));
    const ctx = context(message, latest);
    const replaced = new Set((message.widgets || []).map((widget) => widget.replaces).filter(Boolean));
    // A file is drawn once: the server sends it as a file of the reply and also as a card of the same file.
    const filed = new Set((message.artifacts || []).map((artifact) => artifact.artifactId || artifact.id).filter(Boolean));
    for (const widget of message.widgets || []) {
      if (widget.widgetId && replaced.has(widget.widgetId)) continue;
      if (widget.type === "artifact" && filed.has(widget.artifactId || widget.id)) continue;
      const node = R.widget(widget, ctx);
      if (node) parts.cards.push(node);
    }
    for (const artifact of message.artifacts || []) parts.cards.push(R.artifactCard(artifact, ctx));
    for (const confirmation of message.confirmations || []) {
      parts.cards.push(R.widget({ type: "approval", title: confirmation.title || confirmation.summary, detail: confirmation.message || confirmation.detail, confirmationId: confirmation.id, details: confirmation.details, expiresAt: confirmation.expiresAt }, ctx));
    }
    for (const taskCard of message.tasks || []) {
      const node = R.widget(taskCard, ctx);
      if (node) parts.cards.push(node);
    }
    if (message.location) parts.cards.push(locationCard(message));
    if (message.note) parts.cards.push(R.el("div", "msg-note", message.note));
    if (!live && message.text) {
      const tools = R.el("div", "msg-tools");
      const copy = R.button("", "icon-button", async () => {
        try {
          await navigator.clipboard.writeText(message.text);
          toast("Kopyalandı.");
        } catch { /* clipboard off */ }
      });
      copy.title = "Kopyala";
      copy.append(window.WiqoIcons.icon("doc.on.doc", 14));
      tools.append(copy);
      if (latest) {
        const again = R.button("", "icon-button", () => context(message, true).retry());
        again.title = "Yeniden oluştur";
        again.append(window.WiqoIcons.icon("arrow.clockwise", 14));
        tools.append(again);
      }
      parts.tools.push(tools);
    }
    return parts;
  }

  /** What decides how each part looks: when it is the same as last time, the part is left alone. */
  function partSignatures(message, latest, live) {
    return {
      trace: JSON.stringify([message.trace?.steps?.map((step) => [step.id, step.state, step.label]), message.trace?.endedAt, message.trace?.persona, message.trace?.failed, live]),
      text: `${live ? 1 : 0}|${message.error ? 1 : 0}|${message.text || ""}`,
      cards: JSON.stringify([message.widgets, (message.artifacts || []).map((artifact) => artifact.artifactId || artifact.id), message.confirmations, message.tasks, message.location, message.note, latest]),
      tools: `${!live && message.text ? 1 : 0}|${latest ? 1 : 0}|${message.text || ""}`
    };
  }

  const rowCache = new WeakMap();

  function assistantRow(message, latest) {
    const live = latest && turn !== null;
    let entry = rowCache.get(message);
    if (!entry) {
      const row = R.el("div", "msg assistant");
      const parts = {};
      for (const name of ["trace", "text", "cards", "tools"]) {
        parts[name] = R.el("div", "part");
        row.append(parts[name]);
      }
      entry = { row, parts, sig: {} };
      rowCache.set(message, entry);
    }
    entry.row.classList.toggle("error", Boolean(message.error));
    const signatures = partSignatures(message, latest, live);
    const stale = Object.keys(signatures).filter((name) => entry.sig[name] !== signatures[name]);
    if (stale.length > 0) {
      const nodes = assistantPartNodes(message, latest, live);
      for (const name of stale) {
        entry.parts[name].replaceChildren(...nodes[name]);
        entry.sig[name] = signatures[name];
      }
    }
    return entry.row;
  }

  // The server's held steps are asked in the one permission card (ask.js), not in the message.
  function askFor(confirmations) {
    for (const confirmation of confirmations) {
      window.WiqoAsk?.confirm({ id: confirmation.id, title: confirmation.title || confirmation.summary, detail: confirmation.message || confirmation.detail, expiresAt: confirmation.expiresAt, details: confirmation.details });
    }
  }

  // ----- "where are you" (the weather) -----
  // First this computer's own location (macOS asks for the permission the first time), turned into a city here; only the city is sent.
  // When there is none (refused, off, Windows) the card asks for it.

  async function sendCity(reply, city) {
    const place = reply.location;
    place.state = "sending";
    render();
    try {
      await invoke("continue_location", { requestId: place.requestId, city: city || null });
      place.state = "sent";
      place.city = city || "";
    } catch (error) {
      place.state = "ask";
      place.error = String(error);
    }
    render();
  }

  async function locate(reply, requestId, prompt) {
    if (!requestId || reply.location) return;
    reply.location = { requestId, prompt: prompt || "Hangi şehir için bakayım?", state: "locating" };
    render();
    let city = null;
    try {
      city = await invoke("current_city");
    } catch { /* no location */ }
    if (city) return sendCity(reply, city);
    reply.location.state = "ask";
    render();
  }

  function locationCard(message) {
    const place = message.location;
    const box = R.el("div", "place-card");
    const head = R.el("div", "place-head");
    head.append(window.WiqoIcons.icon("location", 14));
    if (place.state === "locating") {
      head.append(R.el("span", "", "Konumun bulunuyor…"));
      box.append(head);
      return box;
    }
    if (place.state === "sent" || place.state === "sending") {
      head.append(R.el("span", "", place.state === "sending" ? "Gönderiliyor…" : place.city ? `Şehir: ${place.city}` : "Şehirsiz devam edildi"));
      box.append(head);
      return box;
    }
    head.append(R.el("span", "", place.prompt));
    box.append(head);
    const form = R.el("form", "place-form");
    const field = R.el("input");
    field.type = "text";
    field.placeholder = "Şehir, ör. İstanbul";
    field.maxLength = 120;
    field.value = place.draft || "";
    field.oninput = () => (place.draft = field.value);
    const go = R.el("button", "primary small", "Gönder");
    go.type = "submit";
    const skip = R.button("Şehirsiz devam", "link small", () => sendCity(message, null));
    form.append(field, go, skip);
    form.onsubmit = (event) => {
      event.preventDefault();
      if (field.value.trim()) sendCity(message, field.value.trim());
    };
    box.append(form);
    if (place.error) box.append(R.el("div", "place-error", place.error));
    requestAnimationFrame(() => field.focus());
    return box;
  }

  function render(scrollToEnd) {
    const scroller = $("chat-scroll");
    const atBottom = scrollToEnd || scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
    // The rows of the conversation stay in place; only the ones whose content changed redraw their changed parts (see assistantRow).
    const box = $("chat-messages");
    const rows = messages.map(messageNode);
    rows.forEach((row, position) => { if (box.children[position] !== row) box.insertBefore(row, box.children[position] ?? null); });
    while (box.children.length > rows.length) box.lastElementChild.remove();
    $("chat-empty").hidden = messages.length > 0;
    $("chat-send").hidden = turn !== null;
    $("chat-stop").hidden = turn === null;
    if (atBottom) scroller.scrollTop = scroller.scrollHeight;
    renderSide();
  }

  // Streaming text re-renders at most once a frame.
  function queueRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => {
      renderQueued = false;
      render();
    });
  }

  let sideSignature = "";

  function renderSide() {
    const steps = messages.flatMap((message) => message.steps || []).slice(-12);
    const signature = JSON.stringify([steps.map((step) => [step.summary, step.status]), messages.flatMap((message) => [...(message.artifacts || []), ...(message.widgets || []).filter((widget) => widget.type === "artifact")]).map((file) => file.artifactId || file.id)]);
    if (signature === sideSignature) return;
    sideSignature = signature;
    const box = $("right-steps");
    box.replaceChildren();
    if (!steps.length) box.append(R.el("div", "muted small", "Henüz yok"));
    for (const step of steps.reverse()) {
      const row = R.el("div", `side-item ${R.toneOf(step.status)}`);
      row.append(R.el("div", "ellipsis", step.summary || "PC adımı"), R.el("div", "muted small", R.statusLabel(step.status)));
      box.append(row);
    }
    const files = messages.flatMap((message) => [...(message.artifacts || []), ...(message.widgets || []).filter((widget) => widget.type === "artifact")]);
    const list = $("right-files");
    list.replaceChildren();
    if (!files.length) list.append(R.el("div", "muted small", "Henüz yok"));
    for (const file of files.slice(-8).reverse()) {
      const id = file.artifactId || file.id;
      const row = R.button("", "side-item file", async () => {
        try {
          await invoke("artifact_save", { id, filename: file.filename || file.title || "Wiqo dosyası", open: true });
        } catch (error) {
          toast(String(error), "bad");
        }
      });
      row.append(R.el("div", "ellipsis", file.title || file.filename || "Dosya"));
      list.append(row);
    }
  }

  // ----- sending -----

  async function send(text, answer) {
    const message = String(text || "").trim();
    if ((!message && !pending.length) || turn) return;
    window.showSection("chat");
    if (isGroup()) return sendGroup(message);
    const history = messages.slice(-16).map((item) => ({ role: item.role, content: String(item.text).slice(0, 4000) }));
    const attachments = pending.map(({ name, mimeType, dataBase64 }) => ({ name, mimeType, dataBase64 }));
    messages.push({ role: "user", text: message || "Bu görselleri incele.", images: pending.map((item) => `data:${item.mimeType};base64,${item.dataBase64}`) });
    messages.push({ role: "assistant", text: "", steps: [], widgets: [], artifacts: [], confirmations: [], tasks: [], trace: newTrace() });
    pending = [];
    renderAttachments();
    turn = uuid();
    render(true);
    await invoke("chat_send", {
      turn,
      conversation,
      history,
      message: message || "Bu görselleri incele.",
      taskId: answer?.taskId || null,
      questionId: answer?.questionId || null,
      attachments,
    });
  }

  // A group turn: the people it is for answer one after the other, each as a message headed with their name.
  async function sendGroup(text) {
    if (!text) return;
    window.showSection("chat");
    messages.push({ role: "user", text });
    turn = uuid();
    groupLive = new Map();
    render(true);
    try {
      await invoke("thread_send", { turn, thread: conversation, message: text });
    } catch (error) {
      turn = null;
      messages.push({ role: "assistant", text: `_${String(error)}_`, steps: [], widgets: [], artifacts: [], confirmations: [], tasks: [] });
      render(true);
    }
  }

  let groupLive = new Map();
  const groupMessage = (agentId, text) => ({ role: "assistant", agentId, text: `**${members.get(agentId) || "Ajan"}**\n\n${text}`, steps: [], widgets: [], artifacts: [], confirmations: [], tasks: [] });

  listen("wiqo", (event) => {
    const payload = event.payload;
    if (payload.type === "thread" && turn && payload.turn === turn && payload.thread === conversation) {
      const data = payload.event;
      switch (data.type) {
        case "agent_typing":
          if (!groupLive.has(data.agentId)) { const live = groupMessage(data.agentId, "…"); groupLive.set(data.agentId, { live, text: "" }); messages.push(live); }
          break;
        case "agent_delta": {
          const entry = groupLive.get(data.agentId);
          if (entry) { entry.text += data.text; Object.assign(entry.live, groupMessage(data.agentId, entry.text)); }
          break;
        }
        case "agent_idle": {
          const entry = groupLive.get(data.agentId);
          if (entry) messages = messages.filter((item) => item !== entry.live);
          groupLive.delete(data.agentId);
          break;
        }
        case "agent_message": {
          const entry = groupLive.get(data.agentId);
          if (entry) Object.assign(entry.live, groupMessage(data.agentId, data.content));
          else messages.push(groupMessage(data.agentId, data.content));
          groupLive.delete(data.agentId);
          break;
        }
        case "system_note": messages.push({ role: "assistant", text: `_${data.text}_`, steps: [], widgets: [], artifacts: [], confirmations: [], tasks: [] }); break;
        case "error": messages.push({ role: "assistant", text: `_${data.message}_`, steps: [], widgets: [], artifacts: [], confirmations: [], tasks: [] }); break;
        case "turnEnd": turn = null; groupLive = new Map(); break;
        default: break;
      }
      queueRender();
      return;
    }
    if (payload.type !== "chat" || payload.turn !== turn) return;
    const reply = messages[messages.length - 1];
    const data = payload.event;
    switch (data.type) {
      case "task":
        task = data.taskId;
        break;
      case "activity":
        if (data.activity) apply(reply.trace, data.activity);
        break;
      case "status":
        note(reply.trace, data.text);
        break;
      case "thinking":
        reply.trace.didThink = true;
        break;
      case "location_required":
        note(reply.trace, "Konum gerekiyor");
        locate(reply, data.requestId, data.prompt);
        break;
      case "delta":
        reply.text += data.text || "";
        return queueRender();
      case "reset":
        reply.text = "";
        break;
      case "step_update": {
        const index = reply.steps.findIndex((step) => step.id === data.step.id);
        if (index >= 0) reply.steps[index] = { ...reply.steps[index], ...data.step };
        else reply.steps.push(data.step);
        const state = data.step.status === "running" ? "running" : data.step.status === "completed" ? "done" : "failed";
        apply(reply.trace, { id: `pc:${data.step.id}`, phase: "device", label: data.step.summary || "PC adımı", state });
        break;
      }
      case "widget_update":
        if (data.widget) {
          const index = reply.widgets.findIndex((widget) => widget.widgetId && widget.widgetId === data.widget.widgetId);
          if (index >= 0) reply.widgets[index] = data.widget;
          else reply.widgets.push(data.widget);
        }
        break;
      case "confirmation_needed":
        reply.confirmations = data.backendConfirmations || [];
        askFor(reply.confirmations);
        break;
      case "done":
        if (data.reply) reply.text = data.reply;
        if (data.widgets) reply.widgets = data.widgets;
        if (data.artifacts) reply.artifacts = data.artifacts;
        if (data.backendConfirmations) { reply.confirmations = data.backendConfirmations; askFor(reply.confirmations); }
        reply.tasks = ["agentTask", "projectTask", "deferredTask", "actTask"]
          .filter((key) => data[key] && typeof data[key] === "object")
          .map((key) => ({ ...data[key], type: { agentTask: "agent_task", projectTask: "project_task", deferredTask: "deferred_task", actTask: "act_task" }[key] }));
        if (!reply.text && !reply.widgets.length && !reply.artifacts.length && !(data.steps || []).length) reply.text = "Tamam.";
        break;
      case "error":
        reply.error = true;
        reply.text = data.message || "İstek tamamlanamadı.";
        break;
      case "turnEnd":
        if (data.error) {
          reply.error = true;
          reply.note = data.error;
        }
        finishTrace(reply.trace, Boolean(reply.error));
        turn = null;
        task = null;
        loadConversations();
        break;
    }
    render();
  });

  // ----- composer -----

  const input = $("chat-input");
  function autosize() {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 220)}px`;
  }
  input.addEventListener("input", autosize);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      $("chat-form").requestSubmit();
    }
  });
  $("chat-form").onsubmit = (event) => {
    event.preventDefault();
    const text = input.value;
    input.value = "";
    autosize();
    send(text);
  };
  $("chat-stop").onclick = async () => {
    const reply = messages[messages.length - 1];
    if (reply?.trace) note(reply.trace, "Durduruluyor");
    render();
    if (isGroup()) await invoke("thread_stop", { thread: conversation });
    else if (task) await invoke("chat_cancel", { task, conversation });
  };
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && turn && task) $("chat-stop").click();
  });

  function renderAttachments() {
    const box = $("chat-attachments");
    box.replaceChildren();
    box.hidden = pending.length === 0;
    pending.forEach((item, index) => {
      const chip = R.el("div", "attachment");
      const img = R.el("img");
      img.src = `data:${item.mimeType};base64,${item.dataBase64}`;
      chip.append(img, R.button("×", "remove", () => {
        pending.splice(index, 1);
        renderAttachments();
      }));
      box.append(chip);
    });
  }

  $("chat-file").onchange = async (event) => {
    for (const file of [...event.target.files].slice(0, 4 - pending.length)) {
      if (file.size > 6 * 1024 * 1024) {
        toast(`${file.name} çok büyük (en çok 6 MB).`, "bad");
        continue;
      }
      const dataBase64 = await new Promise((resolve) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result).split(",")[1] || "");
        reader.readAsDataURL(file);
      });
      pending.push({ name: file.name, mimeType: file.type || "image/png", dataBase64 });
    }
    event.target.value = "";
    renderAttachments();
  };

  for (const idea of document.querySelectorAll(".idea")) idea.onclick = () => send(idea.textContent);
  $("new-chat").onclick = fresh;
  $("search").addEventListener("input", renderList);
  window.addEventListener("wiqo-section", () => {
    renderList();
    setTitle();
  });
  window.addEventListener("wiqo-signed-in", loadConversations);
  invoke("chat_account").then((email) => {
    if (email) loadConversations();
  });
  render(true);
  // The Ajanlar page opens a teammate's direct chat here: it is Wiqo's own chat, the server resolves the agent from the conversation id.
  window.WiqoChat = { open };
})();
