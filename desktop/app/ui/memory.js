// Ekran hafızası (wiqo-ops screen_memory.rs): one switch, turned on only here, on this computer. What it holds never leaves the computer; turning it off
// forgets everything it read.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);

  function render(state) {
    const on = Boolean(state?.enabled);
    $("memory-enabled").checked = on;
    if (!on) return;
    const size = state.bytes > 1024 * 1024 ? `${(state.bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.round(state.bytes / 1024)} KB`;
    $("memory-detail").textContent = `${state.days} günde ${state.entries} okuma, ${size}`;
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
    $("memory-detail").textContent = "";
    render(await invoke("set_screen_memory", { on }).catch(() => null));
  };

  window.addEventListener("wiqo-section", (event) => { if (event.detail === "settings") load(); });
  load();
})();
