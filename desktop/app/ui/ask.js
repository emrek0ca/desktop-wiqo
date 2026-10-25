// The one place Wiqo asks for permission on this computer: a card docked above the composer, in the main window. A step of the server's agent that
// needs a "yes" from the person (a move, a delete, a send) waits here; the corner mascot only brings this window forward. Enter allows, Esc refuses.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const NAMES = window.WIQO_OPERATIONS || {};
  const questions = [];
  let dock = null;
  let timer = null;

  function build() {
    dock = document.createElement("div");
    dock.id = "ask-dock";
    dock.hidden = true;
    dock.innerHTML = `
      <div class="ask-title"><b>Wiqo bunu yapmak için izin istiyor</b><span class="ask-timer muted small"></span></div>
      <div class="ask-what"></div>
      <div class="ask-detail muted small"></div>
      <div class="ask-actions">
        <button class="ask-deny secondary" type="button">Reddet <kbd>Esc</kbd></button>
        <button class="ask-allow primary" type="button">İzin ver <kbd>↵</kbd></button>
      </div>`;
    document.body.append(dock);
    dock.querySelector(".ask-deny").onclick = () => answer(false);
    dock.querySelector(".ask-allow").onclick = () => answer(true);
  }

  function render() {
    if (!dock) build();
    const question = questions[0];
    dock.hidden = !question;
    clearInterval(timer);
    if (!question) return;
    dock.querySelector(".ask-what").textContent = question.title || NAMES[question.kind] || question.kind;
    dock.querySelector(".ask-detail").textContent = question.detail || "";
    const tick = () => {
      const left = Math.max(0, Math.round((question.deadline - Date.now()) / 1000));
      dock.querySelector(".ask-timer").textContent = `${left} sn`;
    };
    tick();
    timer = setInterval(tick, 1000);
  }

  function answer(allow) {
    const question = questions.shift();
    if (question?.confirmation) settle(question, allow);
    else if (question) invoke("answer_approval", { id: question.id, allow });
    render();
  }

  // A question the server asked in the chat (a step it holds for a "yes"): same card, answered through the chat's own confirm call, and the chat
  // message that carries it is told the outcome (render.js listens for "wiqo-confirmed").
  async function settle(question, approved) {
    const detail = { id: question.confirmation.id, approved };
    try {
      detail.result = await invoke("chat_confirm", { confirmation: question.confirmation, approved });
    } catch (error) {
      detail.error = String(error);
    }
    window.dispatchEvent(new CustomEvent("wiqo-confirmed", { detail }));
  }

  function confirm({ id, title, detail, expiresAt, details }) {
    const left = expiresAt ? Math.round((Date.parse(expiresAt) - Date.now()) / 1000) : 300;
    if (left <= 0) return;
    add({ id: `confirmation:${id}`, title: title || "Onay gerekiyor", detail: detail || "", seconds: left, confirmation: { id, details: details || {} } });
  }
  window.WiqoAsk = { confirm };

  function add(question) {
    if (questions.some((open) => open.id === question.id)) return;
    questions.push({ ...question, deadline: Date.now() + question.seconds * 1000 });
    render();
  }

  function load() {
    // (the server's chat confirmations are added by chat.js as they arrive)
    invoke("pending_approvals").then((open) => { for (const question of open) add(question); });
  }

  listen("wiqo", (message) => {
    const event = message.payload;
    if (event.type === "approval") add(event);
    else if (event.type === "approvalClosed") {
      const index = questions.findIndex((open) => open.id === event.id);
      if (index >= 0) questions.splice(index, 1);
      render();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (!questions.length) return;
    const typing = event.target instanceof HTMLTextAreaElement && event.target.value.trim() !== "";
    if (event.key === "Escape") { event.preventDefault(); answer(false); }
    else if (event.key === "Enter" && !typing && !event.shiftKey) { event.preventDefault(); answer(true); }
  }, true);
  window.addEventListener("focus", load);
  load();
})();
