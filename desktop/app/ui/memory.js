// Ekran hafızası (wiqo-ops screen_memory.rs): turned on only here, on this computer. Its state, a pause, a full forget, and the applications never
// read. What it holds never leaves the computer; a search from a chat returns only the passages that match.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);

  function render(state) {
    const on = Boolean(state?.enabled);
    $("memory-enabled").checked = on;
    $("memory-state-row").hidden = !on;
    $("memory-exclude-form").hidden = !on;
    if (!on) return;
    const paused = state.pausedUntil ? new Date(state.pausedUntil) : null;
    $("memory-state").textContent = paused ? "Duraklatıldı" : "Açık";
    const size = state.bytes > 1024 * 1024 ? `${(state.bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.round(state.bytes / 1024)} KB`;
    $("memory-detail").textContent = paused
      ? `${paused.getFullYear() - new Date().getFullYear() > 0 ? "Sürdürene kadar" : `${paused.toLocaleString("tr-TR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })} tarihine kadar`} ekran okunmuyor.`
      : `${state.days} günde ${state.entries} okuma, ${size}. Mac'te Ekran Kaydı izni gerekir.`;
    $("memory-pause").textContent = paused ? "Sürdür" : "Duraklat";
    $("memory-pause").dataset.action = paused ? "resume" : "pause";
    if (document.activeElement !== $("memory-excluded")) $("memory-excluded").value = (state.excluded || []).join(", ");
  }

  async function load() {
    render(await invoke("screen_memory").catch(() => null));
  }

  $("memory-enabled").onchange = async (event) => {
    const on = event.target.checked;
    if (!on) {
      const yes = await window.WiqoDialog.confirm({ title: "Ekran hafızası kapatılsın mı?", message: "Kapatınca şimdiye kadar okunanların hepsi bu bilgisayardan silinir.", confirm: "Kapat ve sil", danger: true });
      if (!yes) { event.target.checked = true; return; }
    }
    render(await invoke("set_screen_memory", { on }).catch(() => null));
  };
  $("memory-pause").onclick = async () => {
    render(await invoke("screen_memory_action", { action: $("memory-pause").dataset.action || "pause", hours: null, excluded: null }).catch(() => null));
  };
  $("memory-forget").onclick = async () => {
    const yes = await window.WiqoDialog.confirm({ title: "Ekran hafızası silinsin mi?", message: "Okunanların hepsi bu bilgisayardan silinir; hafıza açık kalır.", confirm: "Hepsini sil", danger: true });
    if (yes) render(await invoke("screen_memory_action", { action: "forget", hours: null, excluded: null }).catch(() => null));
  };
  $("memory-exclude-form").onsubmit = async (event) => {
    event.preventDefault();
    const excluded = $("memory-excluded").value.split(",").map((name) => name.trim()).filter(Boolean);
    render(await invoke("screen_memory_action", { action: "exclude", hours: null, excluded }).catch(() => null));
  };

  window.addEventListener("wiqo-section", (event) => { if (event.detail === "settings") load(); });
  load();
})();
