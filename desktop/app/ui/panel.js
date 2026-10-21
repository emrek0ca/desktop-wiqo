// The corner card: what Wiqo is doing right now and the "PC'de onayla" questions. It hides itself a few seconds after the work ends.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);
const NAMES = window.WIQO_OPERATIONS;

let items = [];
const approvals = [];
let hideTimer = null;
let countdown = null;

function name(kind) {
  return NAMES[kind] || kind;
}

function render() {
  const approval = approvals[0];
  $("approval").hidden = !approval;
  $("work").hidden = Boolean(approval) || items.length === 0;
  if (approval) {
    $("approval-kind").textContent = name(approval.kind);
    $("approval-detail").textContent = approval.detail || "Ayrıntı yok.";
  }
  $("work-title").textContent = items.length > 1 ? `Wiqo ${items.length} iş yapıyor` : "Wiqo çalışıyor";
  const list = $("work-list");
  list.replaceChildren();
  for (const item of items.slice(0, 4)) {
    const row = document.createElement("li");
    const title = document.createElement("b");
    title.textContent = name(item.kind);
    const detail = document.createElement("span");
    detail.textContent = item.detail;
    row.append(title, detail);
    list.append(row);
  }
  clearTimeout(hideTimer);
  if (!approval && items.length === 0) hideTimer = setTimeout(() => invoke("hide_panel"), 3000);
}

function tick() {
  clearInterval(countdown);
  const approval = approvals[0];
  if (!approval) return;
  const update = () => {
    const left = Math.max(0, Math.round((approval.deadline - Date.now()) / 1000));
    $("approval-timer").textContent = `${left} sn`;
  };
  update();
  countdown = setInterval(update, 1000);
}

function answer(allow) {
  const approval = approvals.shift();
  if (approval) invoke("answer_approval", { id: approval.id, allow });
  tick();
  render();
}

$("approval-allow").onclick = () => answer(true);
$("approval-deny").onclick = () => answer(false);
$("work-stop").onclick = () => invoke("stop_work");
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") approvals.length ? answer(false) : invoke("stop_work");
  if (event.key === "Enter" && approvals.length) answer(true);
});

listen("wiqo", (message) => {
  const event = message.payload;
  if (event.type === "working") {
    items = event.items;
    render();
  } else if (event.type === "approval") {
    approvals.push({ ...event, deadline: Date.now() + event.seconds * 1000 });
    tick();
    render();
  } else if (event.type === "approvalClosed") {
    const index = approvals.findIndex((approval) => approval.id === event.id);
    if (index >= 0) approvals.splice(index, 1);
    tick();
    render();
  }
});

invoke("working").then((current) => {
  items = current;
  render();
});
