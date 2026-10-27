if (/Mac/.test(navigator.platform)) document.documentElement.classList.add("mac");
// The window only shows what the runtime reports and sends the person's choices back; all logic lives in Rust (wiqo-agent).
// Three stages, in order: sign in (the account) → permissions (can be skipped) → the app.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let snapshot = null;

const CAPABILITIES = {
  "runtime.status": "Durum bilgisi",
  "device.files.list": "Dosyaları listeleme ve arama",
  "device.files.read": "Dosya okuma ve telefona gönderme",
  "device.files.write": "Dosya yazma",
  "device.files.move": "Dosya taşıma ve yeniden adlandırma",
  "device.files.copy": "Dosya kopyalama ve PDF'e çevirme",
  "device.files.delete": "Dosyaları çöpe atma",
  "device.files.utility": "Dosya işleri (zip, PDF, klasör düzenleme)",
  "document.read": "Belge okuma (PDF, Word, Excel)",
  "document.edit": "Belge yazma",
  "artifact.save": "Telefondan gelen dosyayı kaydetme",
  "device.app.list": "Açık uygulamaları görme",
  "device.app.open": "Uygulama açma",
  "device.app.open_by_name": "Ada göre uygulama açma",
  "device.app.control": "Uygulamayı öne getirme",
  "device.app.quit": "Uygulama kapatma",
  "device.url.open": "Web adresi açma",
  "device.system.power": "Kilitleme ve uyku",
  "device.system.utility": "Sistem işleri (koyu mod, pil, pencereler)",
  "device.audio": "Ses seviyesi",
  "device.media": "Medya (oynat, sonraki)",
  "device.display": "Ekran parlaklığı",
  "device.clipboard": "Pano",
  "device.screen.capture": "Ekran görüntüsü",
  "device.code.execute": "Kod ve terminal",
  "device.shortcut.run": "Kestirmeler",
  "device.messaging.send": "Mesaj gönderme",
  "mail.send": "E-posta gönderme",
  "notes.write": "Not yazma",
  "calendar.read": "Takvimi okuma",
  "calendar.write": "Takvime ekleme",
  "reminders.read": "Hatırlatıcıları okuma",
  "reminders.write": "Hatırlatıcı ekleme",
};

const OPERATIONS = window.WIQO_OPERATIONS;

const STATES = {
  online: ["Bağlı", "Telefonun ve Wiqo bu bilgisayarda görev yapabilir."],
  connecting: ["Bağlanıyor…", "Sunucuya bağlanılıyor."],
  retrying: ["Bağlantı koptu", "Yeniden deneniyor."],
  stopped: ["Bağlantı kapalı", "Bağlantıyı sen kestin; görev gelmez."],
  notPaired: ["Giriş yapılmadı", "Hesabınla giriş yap."],
};

function toast(message, tone = "") {
  const box = $("toast");
  box.textContent = message;
  box.className = `toast ${tone}`;
  box.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (box.hidden = true), 3200);
}

function stateKey(connection) {
  return (connection && connection.state) || "stopped";
}

function time(stamp) {
  const date = new Date(stamp);
  return isNaN(date) ? "" : date.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
}

function busy(button, on, label) {
  if (on) {
    button.dataset.label = button.textContent;
    button.textContent = label;
  } else if (button.dataset.label) {
    button.textContent = button.dataset.label;
  }
  button.disabled = on;
}

// ----- stages -----

function setupPending() {
  try {
    if (localStorage.getItem("wiqo.setupDone") === "1") return false;
  } catch { /* storage off: once per launch */ }
  return !setupPending.dismissed && permissionList.some((permission) => !permission.optional && permission.state !== "granted");
}

function finishSetup() {
  setupPending.dismissed = true;
  try { localStorage.setItem("wiqo.setupDone", "1"); } catch { /* per launch only */ }
  render();
}

function render() {
  if (!snapshot) return;
  const signedIn = Boolean(snapshot.account);
  const setup = signedIn && setupPending();
  $("signin").hidden = signedIn;
  $("setup").hidden = !setup;
  $("shell").hidden = !signedIn || setup;
  if (!signedIn || setup) return;

  const key = stateKey(snapshot.connection);
  const [title, detail] = STATES[key] || STATES.stopped;
  $("state-title").textContent = title;
  $("state-detail").textContent = key === "retrying" && snapshot.connection.detail ? `${detail} (${snapshot.connection.detail})` : detail;
  for (const id of ["side-state", "right-state"]) {
    const pill = $(id);
    pill.className = `pill ${key}`;
    pill.querySelector(".label").textContent = title;
  }
  $("right-device").textContent = snapshot.deviceName;
  $("session-full").textContent = snapshot.fullAccess ? "Açık" : "Kapalı";
  $("session-phones").textContent = String(snapshot.phones.length);
  const running = key === "online" || key === "connecting" || key === "retrying";
  $("toggle-connection").textContent = running ? "Bağlantıyı kes" : "Bağlan";
  $("toggle-connection").className = running ? "secondary small" : "primary small";

  renderAccount();
  $("device-name").textContent = snapshot.deviceName;
  $("device-platform").textContent = { macOS: "macOS", windows: "Windows", linux: "Linux" }[snapshot.platform] || snapshot.platform;
  $("phone-count").textContent = String(snapshot.phones.length);
  $("full-access-state").textContent = snapshot.fullAccess ? "Açık" : "Kapalı";
  $("full-access-chip").classList.toggle("on", snapshot.fullAccess);
  $("full-access-chip").setAttribute("aria-pressed", String(snapshot.fullAccess));
  $("approve-on-pc").checked = snapshot.approveOnPc;
  $("reads-without-phone").checked = snapshot.readsWithoutPhone;

  if (!$("phones-pop").hidden && !phonesPopPairing) renderPhonesPop();
  renderCapabilities();
  activityItems(snapshot.activity, $("recent"), 5);
  activityItems(snapshot.activity, $("right-activity"), 6);
  activityItems(snapshot.activity, $("activity"), 100, true);
}

async function refresh() {
  snapshot = await invoke("snapshot");
  render();
}

const SECTION_TITLES = { status: "Ayarlar", agents: "Ayarlar", activity: "Ayarlar", apps: "Uygulamalar", threads: "Ajanlar", permissions: "Ayarlar" };
const SETTINGS_TABS = ["permissions", "status", "agents", "activity"];

function showSection(name) {
  for (const button of document.querySelectorAll(".nav")) button.classList.toggle("active", button.dataset.section === name);
  for (const panel of document.querySelectorAll(".panel")) panel.hidden = panel.dataset.panel !== name;
  document.body.dataset.section = name;
  $("settings-tabs").hidden = !SETTINGS_TABS.includes(name);
  for (const tab of document.querySelectorAll("#settings-tabs button")) tab.classList.toggle("active", tab.dataset.tab === name);
  if (name !== "chat") $("top-title").textContent = SECTION_TITLES[name] || "";
  window.dispatchEvent(new CustomEvent("wiqo-section", { detail: name }));
  if (name === "permissions") loadPermissions();
}
window.showSection = showSection;

// ----- the three columns -----

const columns = { left: true, right: true };
try {
  Object.assign(columns, JSON.parse(localStorage.getItem("wiqo.columns") || "{}"));
} catch { /* defaults */ }

columns.right = false;
document.addEventListener("click", (event) => {
  if (!columns.right || $("right").contains(event.target) || $("session-toggle").contains(event.target)) return;
  columns.right = false;
  layout();
});

function layout() {
  $("shell").classList.toggle("left-closed", !columns.left);
  $("shell").classList.toggle("right-closed", !columns.right);
  $("session-toggle").classList.toggle("on", columns.right);
  $("open-left").hidden = columns.left;
}

function toggleColumn(side) {
  columns[side] = !columns[side];
  // The session bar is a bubble, closed at every launch; only the sidebar remembers its state.
  if (side === "left") try { localStorage.setItem("wiqo.columns", JSON.stringify({ left: columns.left })); } catch { /* per launch */ }
  layout();
}

for (const button of document.querySelectorAll("[data-toggle]")) button.onclick = () => toggleColumn(button.dataset.toggle);
window.addEventListener("resize", layout);
document.addEventListener("keydown", (event) => {
  if (!(event.metaKey || event.ctrlKey)) return;
  if (event.key === "b") { event.preventDefault(); toggleColumn("left"); }
  if (event.key === "i") { event.preventDefault(); toggleColumn("right"); }
  if (event.key === "n") { event.preventDefault(); $("new-chat").click(); }
});
layout();

// Shortcut hints are written for the Mac; elsewhere the same keys go with Ctrl.
if (!/Mac/i.test(navigator.platform)) {
  for (const element of document.querySelectorAll("[title*='⌘']")) element.title = element.title.replaceAll("⌘", "Ctrl+");
}

$("me-menu").onclick = (event) => {
  event.stopPropagation();
  $("me-popover").hidden = !$("me-popover").hidden;
};
document.addEventListener("click", () => ($("me-popover").hidden = true));

function renderWork(items) {
  const box = $("right-work");
  box.replaceChildren();
  // "İşi durdur" only while something runs.
  $("right-stop").hidden = !items.length;
  if (!items.length) {
    box.append(Object.assign(document.createElement("div"), { className: "muted small", textContent: "Boşta" }));
    return;
  }
  for (const item of items) {
    const row = document.createElement("div");
    row.className = "side-item running";
    const title = document.createElement("div");
    title.textContent = OPERATIONS[item.kind] || item.kind;
    const detail = document.createElement("div");
    detail.className = "muted small ellipsis";
    detail.textContent = item.detail;
    row.append(title, detail);
    box.append(row);
  }
}
for (const button of document.querySelectorAll(".nav")) button.onclick = () => showSection(button.dataset.section);
for (const tab of document.querySelectorAll("#settings-tabs button")) tab.onclick = () => showSection(tab.dataset.tab);
for (const button of document.querySelectorAll("[data-goto]")) button.onclick = () => showSection(button.dataset.goto);

// ----- stage 1: sign in, create an account, reset the password -----

let registering = false;

function showAuth(card) {
  for (const id of ["auth-main", "auth-google", "auth-forgot"]) $(id).hidden = id !== card;
}

function formError(id, message) {
  $(id).textContent = message || "";
  $(id).hidden = !message;
}

// Server messages come back as plain sentences; a network failure gets one the person can act on.
function readable(error) {
  const text = String(error || "");
  return /error sending request|connect|dns|timed out/i.test(text) ? "Sunucuya ulaşılamadı; internet bağlantını kontrol et." : text;
}

function setMode(register) {
  registering = register;
  $("invite").hidden = !register;
  $("forgot-open").hidden = register;
  $("auth-title").textContent = register ? "Hesap oluştur" : "Giriş yap";
  $("signin-submit").textContent = register ? "Hesap oluştur" : "Giriş yap";
  $("mode-question").textContent = register ? "Hesabın var mı?" : "Hesabın yok mu?";
  $("mode-switch").textContent = register ? "Giriş yap" : "Hesap oluştur";
  $("password").placeholder = register ? "Şifre (en az 8 karakter)" : "Şifre";
  $("password").autocomplete = register ? "new-password" : "current-password";
  formError("signin-error", "");
}

$("mode-switch").onclick = () => setMode(!registering);
$("password-toggle").onclick = () => {
  const shown = $("password").type === "text";
  $("password").type = shown ? "password" : "text";
  $("password-toggle").title = shown ? "Şifreyi göster" : "Şifreyi gizle";
  $("password-toggle").replaceChildren(WiqoIcons.icon(shown ? "eye" : "eye.slash", 14));
};

async function signedIn(next) {
  snapshot = next || (await invoke("snapshot"));
  $("password").value = "";
  showAuth("auth-main");
  await loadPermissions();
  render();
  loadProfile();
  window.dispatchEvent(new Event("wiqo-signed-in"));
}

$("signin-form").onsubmit = async (event) => {
  event.preventDefault();
  formError("signin-error", "");
  const email = $("email").value.trim();
  const password = $("password").value;
  if (!/^\S+@\S+\.\S+$/.test(email)) return formError("signin-error", "Geçerli bir e-posta adresi yaz.");
  if (password.length < (registering ? 8 : 1)) return formError("signin-error", registering ? "Şifre en az 8 karakter olmalı." : "Şifreni yaz.");
  const button = $("signin-submit");
  busy(button, true, registering ? "Hesap oluşturuluyor…" : "Giriş yapılıyor…");
  try {
    const next = registering
      ? await invoke("register", { email, password, invite: $("invite").value })
      : await invoke("sign_in", { email, password });
    await signedIn(next);
  } catch (error) {
    formError("signin-error", readable(error));
  }
  busy(button, false);
};

$("google").onclick = async () => {
  formError("signin-error", "");
  showAuth("auth-google");
  try {
    const next = await invoke("google_sign_in");
    // null: an older server answers through the wiqo:// link (the `signed-in` event below).
    if (next) await signedIn(next);
  } catch (error) {
    showAuth("auth-main");
    if (!/iptal/i.test(String(error))) formError("signin-error", readable(error));
  }
};
$("google-cancel").onclick = () => {
  invoke("cancel_google_sign_in");
  showAuth("auth-main");
};
listen("signed-in", () => signedIn());
listen("sign-in-failed", (message) => {
  showAuth("auth-main");
  formError("signin-error", String(message.payload));
});

// Forgot password: the code goes to the mail, then code + new password sign straight in.
let resetStage = false;
$("forgot-open").onclick = () => {
  resetStage = false;
  $("forgot-email").value = $("email").value;
  $("reset-fields").hidden = true;
  $("forgot-submit").textContent = "Kod gönder";
  $("forgot-lead").textContent = "E-postana 6 haneli kod göndereceğiz.";
  formError("forgot-error", "");
  showAuth("auth-forgot");
};
$("forgot-back").onclick = () => showAuth("auth-main");
$("forgot-form").onsubmit = async (event) => {
  event.preventDefault();
  formError("forgot-error", "");
  const email = $("forgot-email").value.trim();
  if (!/^\S+@\S+\.\S+$/.test(email)) return formError("forgot-error", "Geçerli bir e-posta adresi yaz.");
  const button = $("forgot-submit");
  if (!resetStage) {
    busy(button, true, "Gönderiliyor…");
    try {
      await invoke("forgot_password", { email });
      resetStage = true;
      $("reset-fields").hidden = false;
      $("forgot-lead").textContent = `Kod ${email} adresine gönderildi.`;
      busy(button, false);
      button.textContent = "Şifreyi değiştir";
      $("reset-code").focus();
    } catch (error) {
      busy(button, false);
      formError("forgot-error", readable(error));
    }
    return;
  }
  const code = $("reset-code").value.trim();
  const password = $("reset-password").value;
  if (!/^\d{6}$/.test(code)) return formError("forgot-error", "Kod 6 haneli olmalı.");
  if (password.length < 8) return formError("forgot-error", "Yeni şifre en az 8 karakter olmalı.");
  busy(button, true, "Değiştiriliyor…");
  try {
    await signedIn(await invoke("reset_password", { email, code, password }));
  } catch (error) {
    formError("forgot-error", readable(error));
  }
  busy(button, false);
};

// ----- the account -----

let profile = null;

function avatarInto(element, text) {
  element.replaceChildren();
  if (profile?.avatarUrl && /^https:\/\//.test(profile.avatarUrl)) {
    const image = document.createElement("img");
    image.src = profile.avatarUrl;
    image.alt = "";
    image.referrerPolicy = "no-referrer";
    image.onerror = () => (element.textContent = text);
    element.append(image);
  } else {
    element.textContent = text;
  }
}

function renderAccount() {
  if (!snapshot?.account) return;
  const name = profile?.displayName || snapshot.account.split("@")[0];
  const initial = (name || "W").slice(0, 1).toLocaleUpperCase("tr");
  avatarInto($("me-avatar"), initial);
  avatarInto($("account-avatar"), initial);
  $("account-email").textContent = name;
  $("account-name").textContent = name;
  $("account-mail").textContent = snapshot.account;
  const provider = profile?.authProvider;
  $("account-provider").textContent = provider === "google" ? "Google" : provider === "apple" ? "Apple" : "E-posta";
}

async function loadProfile() {
  try {
    profile = await invoke("profile");
  } catch {
    profile = null;
  }
  renderAccount();
}

async function signOut() {
  $("me-popover").hidden = true;
  const yes = await WiqoDialog.confirm({
    title: "Çıkış yapılsın mı?",
    message: "Bu bilgisayar, yeniden giriş yapana kadar görev almaz. Bağlı telefonların listede kalır.",
    confirm: "Çıkış yap",
    danger: true,
  });
  if (!yes) return;
  try {
    snapshot = await invoke("sign_out");
    profile = null;
    setMode(false);
    showAuth("auth-main");
    window.dispatchEvent(new Event("wiqo-signed-out"));
    render();
  } catch (error) {
    toast(`Çıkış yapılamadı: ${error}`, "bad");
  }
}
$("sign-out").onclick = signOut;
$("account-signout").onclick = signOut;

// ----- stage 2: permissions (also in Ayarlar) -----

const PERMISSION_STATES = { granted: ["Verildi", "ok"], denied: ["Kapalı", "bad"], notDetermined: ["Sorulmadı", "wait"] };
let permissionList = [];
let asking = null;
let highlighted = null;

function permissionRows(target) {
  target.replaceChildren();
  for (const permission of permissionList) {
    const row = document.createElement("li");
    if (permission.id === highlighted) row.className = "highlight";
    const text = document.createElement("div");
    const title = document.createElement("div");
    title.className = "perm-title";
    title.textContent = permission.title;
    if (permission.optional) {
      const tag = document.createElement("span");
      tag.className = "tag";
      tag.textContent = "isteğe bağlı";
      title.append(tag);
    }
    const why = document.createElement("div");
    why.className = "muted small";
    why.textContent = permission.detail ? `${permission.why} (${permission.detail})` : permission.why;
    text.append(title, why);
    const side = document.createElement("div");
    side.className = "perm-side";
    const [label, tone] = PERMISSION_STATES[permission.state] || PERMISSION_STATES.notDetermined;
    const badge = document.createElement("span");
    badge.className = `badge ${permission.needsRestart ? "wait" : tone}`;
    badge.textContent = permission.needsRestart ? "Yeniden başlat" : label;
    side.append(badge);
    if (permission.state !== "granted") {
      const button = document.createElement("button");
      button.className = `small ${permission.optional || permission.state === "denied" ? "secondary" : "primary"}`;
      button.disabled = asking !== null;
      button.textContent = asking === permission.id ? "Bekleniyor…" : permission.state === "denied" || permission.id === "files" ? "Ayarları aç" : "İzin ver";
      button.onclick = () => askPermission(permission.id);
      side.append(button);
    }
    row.append(text, side);
    target.append(row);
  }
}

function renderPermissions() {
  const restart = permissionList.some((permission) => permission.needsRestart);
  $("system-permissions-card").hidden = permissionList.length === 0;
  $("permissions-restart").hidden = !restart;
  $("setup-restart").hidden = !restart;
  const allGranted = permissionList.every((permission) => permission.optional || permission.state === "granted");
  $("setup-done").textContent = allGranted ? "Devam" : "Bunlarla devam et";
  permissionRows($("system-permissions"));
  permissionRows($("setup-permissions"));
}

async function loadPermissions() {
  try {
    permissionList = await invoke("permissions");
  } catch {
    permissionList = [];
  }
  renderPermissions();
}

async function askPermission(id) {
  asking = id;
  renderPermissions();
  try {
    permissionList = await invoke("request_permission", { id });
  } catch (error) {
    toast(`İzin istenemedi: ${error}`, "bad");
  }
  asking = null;
  renderPermissions();
}

$("setup-done").onclick = finishSetup;
$("setup-skip").onclick = finishSetup;
for (const id of ["setup-restart", "permissions-restart"]) $(id).onclick = () => invoke("restart_app");

// The person grants in System Settings: the list follows while it is on screen.
setInterval(() => {
  const visible = !$("setup").hidden || !document.querySelector('[data-panel="permissions"]').hidden;
  if (visible && asking === null && document.visibilityState === "visible") loadPermissions();
}, 2000);
window.addEventListener("focus", loadPermissions);

// ----- phones -----

// The phone bubble opens under the phone icon: no phone linked -> the QR code at once; linked -> the phones and "Telefon ekle".
let phonesPopAnchor = null;
let phonesPopPairing = false;

function closePhonesPop() {
  if ($("phones-pop").hidden) return;
  $("phones-pop").hidden = true;
  if (phonesPopPairing) invoke("cancel_pairing");
  phonesPopPairing = false;
}

function phonesPopLine(parent, tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  node.textContent = text;
  parent.append(node);
  return node;
}

async function showPairing(pop) {
  phonesPopPairing = true;
  pop.replaceChildren();
  phonesPopLine(pop, "h3", "", "Telefonunu bağla");
  phonesPopLine(pop, "p", "muted small", "Telefondaki Wiqo'da PC'ye bağlan'ı aç ve bu kodu okut. Kod 10 dakika geçerli.");
  const qr = phonesPopLine(pop, "div", "qr", "");
  qr.innerHTML = `<div class="spinner"></div>`;
  const error = phonesPopLine(pop, "p", "error small", "");
  error.hidden = true;
  try {
    const offer = await invoke("start_pairing");
    qr.innerHTML = offer.qrSvg;
  } catch (failure) {
    qr.innerHTML = "";
    error.textContent = `Kod oluşturulamadı: ${failure}`;
    error.hidden = false;
  }
  if (snapshot.phones.length > 0) {
    const back = phonesPopLine(pop, "button", "link small", "Vazgeç");
    back.onclick = () => { invoke("cancel_pairing"); phonesPopPairing = false; renderPhonesPop(); };
  }
}

function renderPhonesPop() {
  const pop = $("phones-pop");
  if (pop.hidden) return;
  if (!snapshot || snapshot.phones.length === 0) { showPairing(pop); return; }
  phonesPopPairing = false;
  pop.replaceChildren();
  phonesPopLine(pop, "h3", "", snapshot.phones.length === 1 ? "Bağlı telefon" : `${snapshot.phones.length} bağlı telefon`);
  for (const phone of snapshot.phones) {
    const row = document.createElement("div");
    row.className = "phone-row";
    const text = document.createElement("div");
    phonesPopLine(text, "div", "w-title", phone.name);
    phonesPopLine(text, "div", "muted small", `${phone.platform === "ios" ? "iPhone" : phone.platform === "android" ? "Android" : "Telefon"} · bu bilgisayara görev verebilir`);
    const remove = phonesPopLine(row, "button", "danger", "Kaldır");
    row.prepend(text);
    remove.onclick = async () => {
      closePhonesPop();
      if (!(await WiqoDialog.confirm({ title: `${phone.name} kaldırılsın mı?`, message: "Bu telefon artık bu bilgisayara görev veremez.", confirm: "Kaldır", danger: true }))) return;
      snapshot = await invoke("remove_phone", { pairingId: phone.pairingId });
      render();
      toast("Telefon kaldırıldı.");
    };
    pop.append(row);
  }
}

function togglePhonesPop(anchor) {
  const pop = $("phones-pop");
  if (!pop.hidden && phonesPopAnchor === anchor) { closePhonesPop(); return; }
  phonesPopAnchor = anchor;
  const box = anchor.getBoundingClientRect();
  // Under the icon, right edges aligned, and never past the window's edge.
  const width = 300;
  pop.style.top = `${Math.round(box.bottom + 8)}px`;
  pop.style.left = `${Math.max(12, Math.min(Math.round(box.right - width), window.innerWidth - width - 12))}px`;
  pop.hidden = false;
  renderPhonesPop();
}

for (const button of document.querySelectorAll(".phones-link")) button.onclick = (event) => { event.stopPropagation(); togglePhonesPop(button); };
document.addEventListener("click", (event) => { if (!$("phones-pop").hidden && !$("phones-pop").contains(event.target)) closePhonesPop(); });
document.addEventListener("keydown", (event) => { if (event.key === "Escape") closePhonesPop(); });

listen("pairing-failed", (message) => {
  toast(`Bağlantı kurulamadı: ${message.payload}`, "bad");
});

// ----- settings -----

function renderCapabilities() {
  const capabilities = $("capabilities");
  capabilities.replaceChildren();
  for (const capability of snapshot.capabilities) {
    const row = document.createElement("li");
    const label = document.createElement("label");
    label.className = "toggle-row";
    label.style.width = "100%";
    const text = document.createElement("div");
    text.textContent = CAPABILITIES[capability.id] || capability.id;
    if (capability.asks) {
      const tag = document.createElement("span");
      tag.className = "tag";
      tag.textContent = "onay ister";
      text.append(tag);
    }
    const input = document.createElement("input");
    input.type = "checkbox";
    input.className = "switch";
    input.checked = capability.enabled;
    input.onchange = async () => {
      snapshot = await invoke("set_capability", { id: capability.id, enabled: input.checked });
      render();
    };
    label.append(text, input);
    row.append(label);
    capabilities.append(row);
  }
}

$("autostart").onchange = async (event) => {
  try {
    event.target.checked = await invoke("set_autostart", { on: event.target.checked });
  } catch (error) {
    event.target.checked = !event.target.checked;
    toast(`Ayarlanamadı: ${error}`, "bad");
  }
};
invoke("autostart").then((on) => ($("autostart").checked = on)).catch(() => {});

// Full access, one tap next to "+": turning it on shows what it means first; turning it off needs no question.
$("full-access-chip").onclick = async () => {
  const on = !snapshot.fullAccess;
  if (on && !(await WiqoDialog.confirm({
    title: "Tam yetki açılsın mı?",
    message: "Wiqo, bu bilgisayarda dosyaları taşıyıp kopyalayabilir, uygulamaları açıp kapatabilir ve silme dahil adımları sana sormadan yapar. İstediğin an bu düğmeyle kapatabilirsin.",
    confirm: "Tam yetkiyi aç",
    cancel: "Vazgeç",
    danger: true
  }))) return;
  try {
    snapshot = await invoke("set_full_access", { on });
    render();
    toast(on ? "Tam yetki açık." : "Tam yetki kapandı.");
  } catch (error) {
    toast(`Ayarlanamadı: ${error}`, "bad");
  }
};

$("reads-without-phone").onchange = async (event) => {
  snapshot = await invoke("set_reads_without_phone", { on: event.target.checked });
  render();
};

$("approve-on-pc").onchange = async (event) => {
  snapshot = await invoke("set_approve_on_pc", { on: event.target.checked });
  render();
};

// ----- status and activity -----

$("toggle-connection").onclick = async () => {
  const key = stateKey(snapshot.connection);
  const running = key === "online" || key === "connecting" || key === "retrying";
  snapshot = await invoke("set_connected", { on: !running });
  render();
};

// "İşi durdur" (or Esc): the running jobs stop and the task grant ends; the connection stays.
async function stopWork() {
  await invoke("stop_work");
  toast("Çalışan iş durduruldu.");
}
$("stop-work").onclick = stopWork;
$("right-stop").onclick = stopWork;
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (!$("report").hidden) return closeReport();
  if (!$("shell").hidden) stopWork();
});

async function undo(args) {
  try {
    toast(await invoke("undo", args), "ok");
  } catch (error) {
    toast(String(error), "bad");
  }
  await refresh();
}

function activityRow(item) {
  const row = document.createElement("li");
  const mark = document.createElement("span");
  mark.className = `mark ${item.ok ? "ok" : "bad"}`;
  mark.append(window.WiqoIcons.icon(item.ok ? "checkmark.circle.fill" : "exclamationmark.circle.fill", 13));
  const body = document.createElement("div");
  const title = document.createElement("div");
  title.textContent = OPERATIONS[item.kind] || item.kind;
  const detail = document.createElement("div");
  detail.className = "muted small";
  detail.textContent = item.undone ? `${item.message} · geri alındı` : item.message;
  body.append(title, detail);
  const side = document.createElement("span");
  side.className = "muted small row-side";
  if (item.undo.length > 0 && !item.undone) {
    const button = document.createElement("button");
    button.className = "link small";
    button.textContent = "Geri al";
    button.onclick = () => undo({ key: item.key });
    side.append(button);
  }
  side.append(document.createTextNode(time(item.at)));
  row.append(mark, body, side);
  return row;
}

// Commands of one task (the server's run, or one terminal session) sit together, with one "Görevi geri al" for all their changes.
function activityItems(list, target, limit, grouped = false) {
  target.replaceChildren();
  const items = list.slice(0, limit);
  if (items.length === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = "Henüz bir işlem yok.";
    target.append(empty);
    return;
  }
  for (let index = 0; index < items.length; ) {
    const item = items[index];
    let end = index + 1;
    if (grouped && item.task) while (end < items.length && items[end].task === item.task) end++;
    const group = items.slice(index, end);
    if (group.length > 1) {
      const head = document.createElement("li");
      head.className = "task-head";
      const label = document.createElement("span");
      label.textContent = `Görev · ${group.length} adım`;
      head.append(label);
      if (group.some((step) => step.undo.length > 0 && !step.undone)) {
        const button = document.createElement("button");
        button.className = "link small";
        button.textContent = "Görevi geri al";
        button.onclick = async () => {
          if (await WiqoDialog.confirm({ title: "Görev geri alınsın mı?", message: "Bu görevin yaptığı dosya değişiklikleri geri alınır. Sonradan değişmiş dosyalara dokunulmaz.", confirm: "Geri al" })) undo({ task: item.task });
        };
        head.append(button);
      }
      target.append(head);
    }
    for (const step of group) target.append(activityRow(step));
    index = end;
  }
}

// ----- "Sorun bildir" -----

const CATEGORIES = [
  ["connection", "Bağlantı"], ["task", "Görev çalışmadı"], ["permission", "İzin sorunu"],
  ["crash", "Çöktü / dondu"], ["slow", "Yavaş"], ["idea", "Öneri"], ["other", "Diğer"],
];
let category = "task";

function openReport(preset) {
  category = preset || "task";
  const chips = $("report-categories");
  chips.replaceChildren();
  for (const [id, label] of CATEGORIES) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = `chip${id === category ? " on" : ""}`;
    chip.textContent = label;
    chip.onclick = () => {
      category = id;
      for (const other of chips.children) other.classList.toggle("on", other === chip);
    };
    chips.append(chip);
  }
  $("report-note").value = "";
  $("report").hidden = false;
  $("report-note").focus();
}

function closeReport() {
  $("report").hidden = true;
}

async function submitReport(save) {
  const button = save ? $("report-save") : $("report-send");
  busy(button, true, save ? "Kaydediliyor…" : "Gönderiliyor…");
  try {
    const outcome = await invoke("report_problem", {
      report: { category, note: $("report-note").value, includeLogs: $("report-logs").checked, contactOk: $("report-contact").checked, save },
    });
    closeReport();
    if (outcome.sent) toast("Teşekkürler, bildirimin Wiqo ekibine ulaştı.", "ok");
    else if (save) toast("Rapor masaüstüne kaydedildi.", "ok");
    else toast("Şu an gönderilemedi; rapor masaüstüne kaydedildi, sonra iletebilirsin.", "bad");
  } catch (error) {
    toast(`Rapor oluşturulamadı: ${error}`, "bad");
  }
  busy(button, false);
}

$("report-problem").onclick = () => {
  $("me-popover").hidden = true;
  openReport();
};
$("report-cancel").onclick = closeReport;
$("report-save").onclick = () => submitReport(true);
$("report-form").onsubmit = (event) => {
  event.preventDefault();
  submitReport(false);
};
$("report").onclick = (event) => {
  if (event.target === $("report")) closeReport();
};
invoke("unseen_crashes").then((count) => {
  if (count > 0) {
    openReport("crash");
    $("report-note").placeholder = "Wiqo geçen sefer beklenmedik şekilde kapandı. Ne yapıyordun? (isteğe bağlı)";
  }
}).catch(() => {});

// ----- live updates -----

listen("wiqo", async (message) => {
  const event = message.payload;
  switch (event.type) {
    case "permissionNeeded": {
      await loadPermissions();
      highlighted = event.id;
      const permission = permissionList.find((item) => item.id === event.id);
      if ($("setup").hidden) showSection("permissions");
      renderPermissions();
      toast(`Bu görev için ${permission ? permission.title : "bir"} izni gerekiyor; tek tıkla ver.`, "bad");
      return;
    }
    case "paired":
      toast(`${event.phone} bağlandı.`, "ok");
      phonesPopPairing = false;
      refresh().then(renderPhonesPop);
      break;
    case "activity":
      if (!snapshot) return;
      snapshot.activity.unshift(event.item);
      snapshot.activity = snapshot.activity.slice(0, 100);
      render();
      return;
    case "connection":
      if (!snapshot) return;
      snapshot.connection = event.state;
      render();
      return;
    case "working":
      renderWork(event.items);
      return;
    case "approval": case "approvalClosed": case "chat":
      return;
  }
  await refresh();
});

listen("update-ready", (message) => {
  $("update-text").textContent = `Wiqo ${message.payload} hazır; yeniden başlatınca geçer.`;
  $("update-banner").hidden = false;
});
$("update-restart").onclick = () => invoke("restart_app");

loadPermissions().then(refresh).then(() => {
  showSection("chat");
  if (snapshot?.account) loadProfile();
});
