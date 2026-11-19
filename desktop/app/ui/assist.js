// Agents without a page of their own (as the phone's WiqoComposerAssist): they are called from the message box. `@` lists the person's agents and
// puts the name in the message (the server answers as that agent); `/` lists what the box can start (/ajan, /grup) and opens each as a small card
// above the box. Two or more `@names` offer a group of them. The old Ajanlar page is only the place an agent is edited, reached from /ajan.
(() => {
  const { invoke } = window.__TAURI__.core;
  const R = window.WiqoRender;
  const $ = (id) => document.getElementById(id);
  const input = $("chat-input");
  const host = $("assist");
  const COMMANDS = [
    ["/ajan", "person.crop.circle", "Ajanların; yeni ajan oluştur. Örnek: haftalık rapor yazan bir ajan"],
    ["/grup", "person.2", "2-4 ajanı tek sohbette topla. Örnek: yazar + denetçi"],
  ];
  let team = null;
  let card = null;
  const fold = (text) => text.toLocaleLowerCase("tr").normalize("NFC");
  const agents = () => (team?.bots || []).flatMap((bot) => bot.members.slice(0, 1));

  async function loadTeam() {
    try { team = await invoke("threads_list"); } catch { /* the lists wait for the next try */ }
    refresh();
  }

  /** `/word` when the whole message is that word; `@word` when the message ends in one. */
  function trigger(text) {
    if (text.startsWith("/") && !/\s/.test(text)) return { kind: "command", query: text.slice(1) };
    const last = text.split(" ").at(-1) || "";
    return last.startsWith("@") && !text.endsWith(" ") ? { kind: "mention", query: last.slice(1) } : null;
  }

  function mentioned(text) {
    const members = [...agents()].sort((a, b) => b.name.length - a.name.length);
    const found = [];
    for (const match of text.matchAll(/@([\p{L}\p{N}_-]+)/gu)) {
      const word = fold(match[1]);
      const member = members.find((item) => word.startsWith(fold(item.name).replace(/\s+/g, "")));
      if (member && !found.some((item) => item.id === member.id)) found.push(member);
    }
    return found;
  }

  function row(leading, title, detail, onclick) {
    const button = R.el("button", "assist-row");
    button.type = "button";
    const text = R.el("div", "assist-text");
    text.append(R.el("div", "w-title", title), R.el("div", "muted small", detail));
    button.append(leading, text);
    button.onclick = onclick;
    return button;
  }

  const face = (persona) => { const node = R.el("span", "avatar"); node.textContent = ""; node.append(window.WiqoIcons.icon("person.crop.circle", 18)); node.dataset.persona = persona || ""; return node; };

  function pickMention(member) {
    const at = input.value.lastIndexOf("@");
    input.value = `${input.value.slice(0, at)}@${member.name.replace(/\s+/g, "")} `;
    done();
    input.dispatchEvent(new Event("input"));
    input.focus();
  }

  function runCommand(name) {
    input.value = "";
    done();
    input.dispatchEvent(new Event("input"));
    card = name === "/ajan" ? { kind: "agents" } : { kind: "group", preselected: [] };
    loadTeam();
    refresh();
  }

  function shell(title) {
    const box = R.el("div", "assist-card");
    const head = R.el("div", "assist-head");
    const close = R.button("", "icon-button", () => { card = null; refresh(); });
    close.title = "Kapat";
    close.append(window.WiqoIcons.icon("xmark", 13));
    head.append(R.el("h4", "", title), close);
    box.append(head);
    return box;
  }

  function agentsCard() {
    const box = shell("Ajanların");
    const strip = R.el("div", "assist-strip");
    for (const member of agents()) {
      const button = R.el("button", "assist-person");
      button.type = "button";
      button.append(face(member.persona), R.el("span", "small", member.name));
      // An agent's own chat is Wiqo's chat screen.
      button.onclick = () => { card = null; refresh(); window.WiqoChat.open(`agent-dm-${member.id}`); };
      strip.append(button);
    }
    const manage = R.button("Ajanları düzenle ya da yeni ajan", "secondary small", () => { card = null; refresh(); window.showSection("threads"); });
    box.append(strip, manage);
    return box;
  }

  function groupCard(preselected) {
    const box = shell("Grup");
    const picks = new Set(preselected);
    if (team?.groups?.length) {
      const strip = R.el("div", "assist-strip");
      for (const group of team.groups) {
        const button = R.button(group.title, "chip", () => { card = null; refresh(); window.WiqoChat.open(group.id); });
        strip.append(button);
      }
      box.append(strip);
    }
    box.append(R.el("p", "muted small", "Yeni grup için 2-4 ajan seç. Mesajına uygun olan cevap verir; @isim ile birini çağırabilirsin."));
    const chips = R.el("div", "chips");
    const make = R.button("Kur", "primary small", async () => {
      const carried = input.value;
      try {
        const result = await invoke("thread_create", { body: { title: title.value.trim() || undefined, agentIds: [...picks] } });
        card = null;
        refresh();
        await window.WiqoChat.open(result.thread.id);
        if (carried.trim()) window.WiqoChat.compose(carried);
      } catch (error) { problem.textContent = String(error); problem.hidden = false; }
    });
    const sync = () => { make.disabled = picks.size < 2; for (const [id, chip] of chipById) chip.classList.toggle("on", picks.has(id)); };
    const chipById = new Map();
    for (const member of agents()) {
      const chip = R.button(member.name, "chip", () => {
        if (picks.has(member.id)) picks.delete(member.id); else if (picks.size < 4) picks.add(member.id);
        sync();
      });
      chipById.set(member.id, chip);
      chips.append(chip);
    }
    const title = R.el("input");
    title.type = "text";
    title.placeholder = "Grup adı (isteğe bağlı)";
    title.maxLength = 80;
    const problem = R.el("p", "error small");
    problem.hidden = true;
    const actions = R.el("div", "assist-actions");
    actions.append(title, make);
    box.append(chips, actions, problem);
    sync();
    return box;
  }

  function refresh() {
    host.replaceChildren();
    host.hidden = true;
    if (window.WiqoChat.inGroup()) return;
    if (card) {
      host.append(card.kind === "agents" ? agentsCard() : groupCard(card.preselected));
      host.hidden = false;
      return;
    }
    const found = trigger(input.value);
    if (found) {
      const list = R.el("div", "assist-card assist-list");
      if (found.kind === "mention") {
        const query = fold(found.query);
        const members = agents().filter((member) => !query || fold(member.name).includes(query));
        if (!members.length) list.append(R.el("p", "muted small assist-empty", team ? "Eşleşen ajan yok. /ajan ile yenisini oluştur." : "Yükleniyor…"));
        for (const member of members) list.append(row(face(member.persona), `@${member.name}`, "Bu mesajı o cevaplar", () => pickMention(member)));
      } else {
        const query = fold(found.query);
        for (const [name, symbol, detail] of COMMANDS.filter(([name]) => !query || name.slice(1).includes(query))) {
          const icon = R.el("span", "avatar");
          icon.append(window.WiqoIcons.icon(symbol, 16));
          list.append(row(icon, name, detail, () => runCommand(name)));
        }
      }
      host.append(list);
      host.hidden = false;
      return;
    }
    const named = mentioned(input.value);
    if (named.length >= 2) {
      host.append(R.button(`Bu ${named.length} ajanla grup kur`, "secondary small", () => { card = { kind: "group", preselected: named.map((member) => member.id) }; refresh(); }));
      host.hidden = false;
    } else if (!localStorage.getItem("wiqo.tip.agents.done") && !input.value && agents().length && window.WiqoChat.userTurns() >= 2) {
      const tip = R.el("div", "assist-tip muted small", "İpucu: @ ile ajanlarını çağır, / ile ajan ya da grup kur.");
      tip.append(R.button("", "icon-button", () => { done(); refresh(); }));
      tip.lastChild.append(window.WiqoIcons.icon("xmark", 12));
      host.append(tip);
      host.hidden = false;
    }
  }

  /** `@` or `/` was used: the one-time hint has done its job. */
  function done() {
    try { localStorage.setItem("wiqo.tip.agents.done", "1"); } catch { /* the hint may show again */ }
  }

  input.addEventListener("input", refresh);
  $("assist-chip-at").onclick = () => { input.value = "@"; input.dispatchEvent(new Event("input")); input.focus(); };
  $("assist-chip-slash").onclick = () => { input.value = "/"; input.dispatchEvent(new Event("input")); input.focus(); };
  window.addEventListener("wiqo-signed-in", loadTeam);
  loadTeam();
})();
