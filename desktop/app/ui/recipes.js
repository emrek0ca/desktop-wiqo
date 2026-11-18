// Tariflerim: the person's recipes (rova-backend agent/skills.ts). A recipe goes into the composer to be read and sent by the person, never sent for
// them. "Bir kez göster, öğreneyim" records a task done once on this computer (wiqo-ops recorder.rs); its steps are shown here before anything
// leaves the computer, and the server's model turns them into a recipe. Sharing one, or opening a shared link (wiqo://recipe/CODE), goes through
// Keşfet (discover.js): the same dialog and the same checks as any Mission.
(() => {
  const { invoke } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);
  const icon = (name, size = 15) => window.WiqoIcons.icon(name, size);
  const SOURCE = { chat: "Sohbetten", recording: "Kayıttan", shared: "Paylaşılan" };
  const SHARED = { link: "Bağlantıyla paylaşıldı", public: "Keşfet'te" };
  let recipes = [];
  let recordedSteps = [];
  let recordTimer = null;

  function toast(message, tone = "") {
    const box = $("toast");
    box.textContent = message;
    box.className = `toast ${tone}`.trim();
    box.hidden = false;
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => { box.hidden = true; }, 3200);
  }

  function showError(message) {
    const box = $("recipes-error");
    box.textContent = message || "";
    box.hidden = !message;
  }

  function use(recipe) {
    window.WiqoChat.compose(recipe.prompt);
    invoke("skill_action", { id: recipe.id, action: "used" }).catch(() => {});
  }

  async function remove(recipe) {
    const yes = await window.WiqoDialog.confirm({ title: `"${recipe.name}" silinsin mi?`, message: recipe.shareCode ? "Paylaştığın bağlantı da çalışmaz olur." : "", confirm: "Sil", danger: true });
    if (!yes) return;
    try {
      await invoke("skill_action", { id: recipe.id, action: "delete" });
      load();
    } catch (error) {
      showError(String(error));
    }
  }

  function row(recipe) {
    const item = document.createElement("li");
    item.className = "recipe-row";
    const main = document.createElement("button");
    main.className = "recipe-main";
    main.title = "Sohbete koy";
    main.append(icon(recipe.icon || "sparkle", 16));
    const text = document.createElement("div");
    text.className = "recipe-text";
    const name = document.createElement("div");
    name.className = "w-title";
    name.textContent = recipe.name;
    const meta = document.createElement("div");
    meta.className = "muted small";
    meta.textContent = [SOURCE[recipe.source] || "", SHARED[recipe.visibility] || "", recipe.steps?.length ? `${recipe.steps.length} adım` : "", recipe.uses ? `${recipe.uses} kez kullanıldı` : ""].filter(Boolean).join(" · ");
    text.append(name, meta);
    main.append(text);
    main.onclick = () => use(recipe);
    const actions = document.createElement("div");
    actions.className = "recipe-actions";
    const shareButton = document.createElement("button");
    shareButton.className = "icon-button";
    shareButton.title = "Paylaş";
    shareButton.append(icon("square.and.arrow.up", 14));
    shareButton.onclick = () => window.WiqoDiscover.publishRecipe(recipe);
    const deleteButton = document.createElement("button");
    deleteButton.className = "icon-button";
    deleteButton.title = "Sil";
    deleteButton.append(icon("trash", 14));
    deleteButton.onclick = () => remove(recipe);
    actions.append(shareButton, deleteButton);
    item.append(main, actions);
    return item;
  }

  function renderIdeas() {
    const host = $("recipe-ideas");
    host.replaceChildren();
    for (const recipe of recipes.slice(0, 3)) {
      const chip = document.createElement("button");
      chip.className = "idea recipe-idea";
      chip.append(icon(recipe.icon || "sparkle", 14), document.createTextNode(recipe.name));
      chip.onclick = () => use(recipe);
      host.append(chip);
    }
    host.hidden = recipes.length === 0;
  }

  async function load() {
    try {
      const answer = await invoke("skills_list");
      recipes = answer.skills || [];
      showError("");
    } catch (error) {
      recipes = [];
      if (!/oturum|giriş/i.test(String(error))) showError(String(error));
    }
    const list = $("recipes-list");
    list.replaceChildren(...recipes.map(row));
    list.parentElement.hidden = recipes.length === 0;
    $("recipes-empty").hidden = recipes.length > 0;
    renderIdeas();
  }

  // ----- "Bir kez göster, öğreneyim" -----

  function elapsed(since) {
    const seconds = Math.max(0, Math.floor((Date.now() - new Date(since).getTime()) / 1000));
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  }

  function showRecording(since) {
    $("record-bar").hidden = !since;
    clearInterval(recordTimer);
    if (!since) return;
    $("record-time").textContent = elapsed(since);
    recordTimer = setInterval(() => { $("record-time").textContent = elapsed(since); }, 1000);
  }

  function describe(step) {
    const time = new Date(step.at).toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    const what = {
      app: () => `${step.app} açıldı${step.title ? ` · ${step.title}` : ""}`,
      window: () => `${step.app}: ${step.title}`,
      url: () => `${step.app}: ${step.url}`,
      download: () => `İndirildi: ${step.path}`,
      file: () => `${step.path} ${step.change}`,
      clipboard: () => step.change,
    }[step.kind];
    return `${time}  ${what ? what() : step.kind}`;
  }

  function renderSteps() {
    const list = $("record-steps");
    list.replaceChildren(...recordedSteps.map((step, index) => {
      const item = document.createElement("li");
      item.className = "record-step";
      const text = document.createElement("span");
      text.textContent = describe(step);
      const drop = document.createElement("button");
      drop.className = "icon-button";
      drop.type = "button";
      drop.title = "Bu satırı çıkar";
      drop.append(icon("xmark", 12));
      drop.onclick = () => { recordedSteps.splice(index, 1); renderSteps(); };
      item.append(text, drop);
      return item;
    }));
    $("record-save").disabled = recordedSteps.length < 2;
  }

  $("record-open").onclick = async () => {
    const since = await invoke("record_start");
    showRecording(since);
    toast("Kayıt başladı. İşi bir kez yap, bitince üstteki Bitir'e bas.");
  };
  $("record-cancel").onclick = async () => {
    await invoke("record_cancel");
    showRecording(null);
  };
  $("record-stop").onclick = async () => {
    recordedSteps = await invoke("record_stop");
    showRecording(null);
    $("record-error").hidden = true;
    $("record-note").value = "";
    renderSteps();
    $("record-review").hidden = false;
    window.showSection("settings");
  };
  $("record-discard").onclick = () => { $("record-review").hidden = true; recordedSteps = []; };
  $("record-review-form").onsubmit = async (event) => {
    event.preventDefault();
    const button = $("record-save");
    button.disabled = true;
    button.textContent = "Tarif yazılıyor…";
    try {
      const answer = await invoke("record_to_skill", { events: recordedSteps, note: $("record-note").value.trim() || null });
      $("record-review").hidden = true;
      recordedSteps = [];
      toast(`"${answer.skill.name}" tarifi hazır. Düzenlemek için sohbete koyup değiştirebilirsin.`);
      load();
    } catch (error) {
      const box = $("record-error");
      box.textContent = String(error);
      box.hidden = false;
    } finally {
      button.disabled = recordedSteps.length < 2;
      button.textContent = "Tarife çevir";
    }
  };

  invoke("recording_since").then((since) => showRecording(since)).catch(() => {});
  window.addEventListener("wiqo-signed-in", load);
  window.addEventListener("wiqo-recipes-changed", load);
  window.addEventListener("wiqo-section", (event) => { if (event.detail === "settings" || event.detail === "chat") load(); });
  load();
})();
