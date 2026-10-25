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

  function renderList() {
    const box = $("conversation-list");
    box.replaceChildren();
    const query = $("search").value.trim().toLocaleLowerCase("tr");
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

  async function open(id) {
    if (turn) return toast("Çalışan cevap bitmeden sohbet değişmez; önce Durdur.", "bad");
    conversation = id;
    messages = [];
    window.showSection("chat");
    render();
    renderList();
    try {
      const value = await invoke("chat_messages", { conversation: id });
      messages = (value.messages || []).map((item) => ({
        role: item.role === "user" ? "user" : "assistant",
        text: item.content || "",
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

  function messageNode(message, index) {
    const latest = index === messages.length - 1;
    if (message.role === "user") {
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
      return row;
    }
    const row = R.el("div", `msg assistant${message.error ? " error" : ""}`);
    const live = latest && turn !== null;
    if (message.trace) {
      const node = traceNode(message.trace, live);
      if (node) row.append(node);
    }
    if (message.text) row.append(R.markdown(message.text, live));
    const ctx = context(message, latest);
    const replaced = new Set((message.widgets || []).map((widget) => widget.replaces).filter(Boolean));
    for (const widget of message.widgets || []) {
      if (widget.widgetId && replaced.has(widget.widgetId)) continue;
      const node = R.widget(widget, ctx);
      if (node) row.append(node);
    }
    for (const artifact of message.artifacts || []) row.append(R.artifactCard(artifact, ctx));
    for (const confirmation of message.confirmations || []) {
      row.append(R.widget({ type: "approval", title: confirmation.title || confirmation.summary, detail: confirmation.message || confirmation.detail, confirmationId: confirmation.id, details: confirmation.details, expiresAt: confirmation.expiresAt }, ctx));
    }
    for (const taskCard of message.tasks || []) {
      const node = R.widget(taskCard, ctx);
      if (node) row.append(node);
    }
    if (message.location) row.append(locationCard(message));
    if (message.note) row.append(R.el("div", "msg-note", message.note));
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
      row.append(tools);
    }
    return row;
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
    $("chat-messages").replaceChildren(...messages.map(messageNode));
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

  function renderSide() {
    const steps = messages.flatMap((message) => message.steps || []).slice(-12);
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

  listen("wiqo", (event) => {
    const payload = event.payload;
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
    if (task) await invoke("chat_cancel", { task, conversation });
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
})();
