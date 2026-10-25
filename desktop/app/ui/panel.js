// The corner mascot, nothing else. It shows while Wiqo works or waits for a "yes" and hides a few seconds after the work ends.
// A click (or a question to answer) brings the main window forward, where the permission card lives (ask.js).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let working = 0;
let waiting = 0;
let hideTimer = null;

function render() {
  document.body.classList.toggle("waiting", waiting > 0);
  clearTimeout(hideTimer);
  if (!working && !waiting) hideTimer = setTimeout(() => invoke("hide_panel"), 3000);
}

document.getElementById("mascot").onclick = () => invoke("show_main_window");

listen("wiqo", (message) => {
  const event = message.payload;
  if (event.type === "working") working = event.items.length;
  else if (event.type === "approval") waiting += 1;
  else if (event.type === "approvalClosed") waiting = Math.max(0, waiting - 1);
  render();
});

invoke("working").then((current) => { working = current.length; render(); });
invoke("pending_approvals").then((open) => { waiting = open.length; render(); });
