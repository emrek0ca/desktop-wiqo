// Wiqo's own cursor: it goes where the action lands and says what it is doing, then leaves when the work has gone quiet. It only draws: the actions are the helper's.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const IDLE_MS = 4000;
const cursor = document.getElementById("cursor");
const ripple = document.getElementById("ripple");
document.getElementById("stop").textContent = navigator.platform.startsWith("Mac") ? "Durdur ⌃⌥Esc" : "Durdur Ctrl+Alt+Esc";
let leaveTimer = null;

function moveTo(x, y) {
  // Near the screen's edge the words go to the other side of the arrow, so they stay on screen.
  const label = document.getElementById("label");
  label.style.left = x > window.innerWidth - 340 ? "auto" : "22px";
  label.style.right = x > window.innerWidth - 340 ? "22px" : "auto";
  label.style.top = y > window.innerHeight - 60 ? "-30px" : "22px";
  cursor.style.transform = `translate(${x}px, ${y}px)`;
}

listen("wiqo-cursor", ({ payload }) => {
  clearTimeout(leaveTimer);
  cursor.hidden = false;
  cursor.classList.remove("leaving");
  cursor.classList.toggle("dragging", payload.action === "drag");
  document.getElementById("words").textContent = payload.label;
  if (typeof payload.x === "number") moveTo(payload.x, payload.y);
  if (payload.action === "click") {
    ripple.classList.remove("on");
    // Starts again after the cursor has arrived.
    setTimeout(() => { void ripple.offsetWidth; ripple.classList.add("on"); }, 260);
  }
  leaveTimer = setTimeout(() => {
    cursor.classList.add("leaving");
    setTimeout(() => invoke("hide_cursor"), 320);
  }, payload.action === "stop" ? 1200 : IDLE_MS);
});
