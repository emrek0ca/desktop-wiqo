// Tam yetki (wiqo-ops terminal/policy.rs): the person's own switch for this computer. Only here can it be turned on.
(() => {
  const { invoke } = window.__TAURI__.core;
  const toggle = document.getElementById("access-enabled");

  async function load() {
    toggle.checked = Boolean(await invoke("full_access").catch(() => false));
  }

  toggle.onchange = async (event) => {
    const on = event.target.checked;
    if (on) {
      const yes = await window.WiqoDialog.confirm({
        title: "Tam yetki açılsın mı?",
        message: "Wiqo bu bilgisayarda paket kurabilir, komut çalıştırabilir ve dosyaları değiştirebilir; her adım için sormaz. Yalnız kendi bilgisayarında ve güvendiğin bir hesapla aç.",
        confirm: "Aç"
      });
      if (!yes) { event.target.checked = false; return; }
    }
    toggle.checked = Boolean(await invoke("set_full_access", { on }).catch(() => !on));
  };

  load();
  window.addEventListener("wiqo-section", (event) => { if (event.detail === "settings") load(); });
})();
