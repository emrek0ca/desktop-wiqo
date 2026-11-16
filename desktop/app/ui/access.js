// Erişim (wiqo-ops terminal/policy.rs): chosen next to the message box, the way a coding agent asks "what may I do". "Sorarak" is the default: a step that changes,
// deletes or sends asks first. "Tam yetki" is the person saying yes to everything on this computer; it stays until they change it. Only here can it be turned on.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);
  const button = $("access-button");
  const menu = $("access-menu");

  function show(full) {
    $("access-label").textContent = full ? "Tam yetki" : "Sorarak";
    button.classList.toggle("full", full);
    for (const item of menu.querySelectorAll("[data-access]")) item.classList.toggle("on", (item.dataset.access === "full") === full);
  }

  function close() {
    menu.hidden = true;
    button.setAttribute("aria-expanded", "false");
  }

  async function choose(mode) {
    close();
    const full = mode === "full";
    if (full) {
      const yes = await window.WiqoDialog.confirm({
        title: "Tam yetki verilsin mi?",
        message: "Wiqo bu bilgisayarda paket kurabilir, komut çalıştırabilir ve dosyaları değiştirebilir; her adım için sormaz. Yalnız kendi bilgisayarında ve güvendiğin bir hesapla ver.",
        confirm: "Tam yetki ver"
      });
      if (!yes) return;
    }
    show(await invoke("set_full_access", { on: full }).catch(() => !full));
  }

  button.onclick = (event) => {
    event.stopPropagation();
    const open = menu.hidden;
    menu.hidden = !open;
    button.setAttribute("aria-expanded", String(open));
  };
  for (const item of menu.querySelectorAll("[data-access]")) item.onclick = () => choose(item.dataset.access);
  document.addEventListener("click", (event) => { if (!menu.hidden && !menu.contains(event.target)) close(); });
  document.addEventListener("keydown", (event) => { if (event.key === "Escape" && !menu.hidden) { event.stopImmediatePropagation(); close(); } }, true);

  invoke("full_access").then(show).catch(() => show(false));
})();
