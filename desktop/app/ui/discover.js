// Keşfet: Missions people shared (rova-backend social/, docs/missions.md). A Mission is a recipe someone chose to share; running one puts its words
// in this person's composer with its code, they read and send it, and Wiqo runs it here with their own files and permissions, asking before every
// change. Publishing goes through one dialog: the server checks every field and the person sees what blocked it next to that field.
// One screen, like a social network on a computer: a rail of tabs (Akış, Ara, Bildirimler, Profil, Yeni Mission), the feed in the middle, and a
// Mission or a person opened beside it, never by replacing the page.
(() => {
  const { invoke } = window.__TAURI__.core;
  const R = window.WiqoRender;
  const $ = (id) => document.getElementById(id);
  const icon = (name, size = 14) => window.WiqoIcons.icon(name, size);
  const FEEDS = [["popular", "Senin için"], ["following", "Takip"], ["new", "Yeni"]];
  const VISIBILITY = [["private", "Sadece ben", "Tariflerimde durur, kimse görmez."], ["link", "Bağlantısı olan", "Bağlantıyı verdiğin kişi açar; adın görünmez."],
    ["public", "Keşfet'te herkes", "Adınla Keşfet'te görünür; beğenilir, yorumlanır, remix'lenir."]];
  const COVERS = { ink: ["#121214", "#47474f"], ocean: ["#1a549e", "#40999f"], forest: ["#145c45", "#59a16b"], sunset: ["#db4f33", "#f7a847"],
    rose: ["#c73366", "#f5849e"], violet: ["#5c38b3", "#9e80eb"], sand: ["#b39466", "#e6d1a8"], slate: ["#404d61", "#8090a3"] };
  const coverBackground = (name) => `linear-gradient(135deg, ${(COVERS[name] || COVERS.ink).join(", ")})`;
  const PROFILE_TABS = [["missions", "Mission'lar"], ["remixes", "Remix'ler"], ["liked", "Beğenilenler"], ["saved", "Kaydedilenler"]];
  const state = { tab: "feed", covers: Object.keys(COVERS), feed: "popular", category: "", q: "", next: null, missions: [], results: [], categories: [], needs: [], me: null };
  let searchTimer = null;

  async function call(method, path, body) {
    const answer = await invoke("missions_call", { method, path, body: body ?? null });
    if (answer?.error) {
      const failure = new Error(answer.error.message || "İstek reddedildi.");
      failure.code = answer.error.code;
      failure.findings = answer.error.findings || [];
      throw failure;
    }
    return answer;
  }

  function toast(message) {
    const box = $("toast");
    box.textContent = message;
    box.className = "toast";
    box.hidden = false;
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => { box.hidden = true; }, 3200);
  }

  function showError(message) {
    $("discover-error").textContent = message || "";
    $("discover-error").hidden = !message;
  }

  function label(list, id) {
    return list.find((item) => item.id === id)?.label || "";
  }

  function avatar(author, size = "") {
    const node = R.el("span", `avatar ${size}`.trim());
    if (author?.avatar?.url) {
      const image = R.el("img");
      image.src = author.avatar.url;
      image.alt = "";
      node.append(image);
    } else {
      node.textContent = author?.avatar?.emoji || (author?.displayName || author?.username || "?").slice(0, 1).toUpperCase();
    }
    return node;
  }

  function personLink(author) {
    const link = R.button(`@${author.username}`, "link small", (event) => { event.stopPropagation(); openProfile(author.username); });
    return link;
  }

  function counts(mission) {
    const c = mission.counts;
    return [c.likes && `${c.likes} beğeni`, c.comments && `${c.comments} yorum`, c.copies && `${c.copies} kez kullanıldı`, c.remixes && `${c.remixes} remix`].filter(Boolean).join(" · ");
  }

  // ----- views -----

  /** The middle of the page: one of the rail's tabs. */
  function showTab(tab) {
    state.tab = tab;
    $("discover-title").textContent = { feed: "Akış", search: "Ara", notices: "Bildirimler", mine: "Profil" }[tab];
    for (const name of ["feed", "search", "mine", "notices"]) $(`discover-${name}`).hidden = name !== tab;
    for (const button of document.querySelectorAll(".rail-item")) button.classList.toggle("active", button.dataset.tab === tab);
    showError("");
  }

  /** The side of the page, where a Mission or a person opens next to the feed. */
  function openSide() {
    window.showSection("discover");
    $("discover-side").hidden = false;
    $("discover-side").scrollTop = 0;
    return $("discover-side-body");
  }

  function missionCard(mission) {
    const card = R.el("button", "mission-card");
    card.type = "button";
    if (mission.images?.length) {
      const cover = R.el("img", "mission-cover");
      cover.src = mission.images[0].url;
      cover.alt = "";
      card.append(cover);
    }
    const head = R.el("div", "mission-card-head");
    head.append(icon(mission.icon || "sparkle", 16), R.el("div", "w-title ellipsis", mission.name));
    card.append(head);
    if (mission.description) card.append(R.el("p", "muted small mission-desc", mission.description));
    const meta = R.el("div", "mission-meta muted small");
    if (mission.author) meta.append(avatar(mission.author, "tiny"), personLink(mission.author));
    const category = label(state.categories, mission.category);
    if (category) meta.append(R.el("span", "", category));
    if (mission.origin) meta.append(R.el("span", "", `remix: ${mission.origin.name}`));
    const numbers = counts(mission);
    if (numbers) meta.append(R.el("span", "", numbers));
    card.append(meta);
    card.onclick = () => openMission(mission.code);
    return card;
  }

  /** One post: who shared it, the picture, the words, and the actions on it (like, comment, copy link, run). */
  function post(mission) {
    const node = R.el("article", "post");
    if (mission.author) {
      const head = R.el("div", "post-head");
      const who = R.el("button", "post-author");
      who.type = "button";
      const names = R.el("div");
      names.append(R.el("div", "w-title", mission.author.displayName || `@${mission.author.username}`),
        R.el("div", "muted small", [`@${mission.author.username}`, label(state.categories, mission.category), mission.origin ? `remix: ${mission.origin.name}` : ""].filter(Boolean).join(" · ")));
      who.append(avatar(mission.author), names);
      who.onclick = () => openProfile(mission.author.username);
      head.append(who);
      node.append(head);
    }
    const body = R.el("button", "post-body");
    body.type = "button";
    if (mission.images?.length) {
      const picture = R.el("img", "post-image");
      picture.src = mission.images[0].url;
      picture.alt = "";
      body.append(picture);
    }
    body.append(R.el("div", "post-title", mission.name));
    if (mission.description) body.append(R.el("p", "muted post-text", mission.description));
    body.onclick = () => openMission(mission.code);
    const actions = R.el("div", "post-actions");
    const likeButton = R.button("", `post-action ${mission.liked ? "liked" : ""}`.trim(), async () => {
      try {
        const done = await call(mission.liked ? "DELETE" : "POST", `missions/${mission.code}/like`);
        mission.liked = done.liked;
        mission.counts.likes = done.likes;
        node.replaceWith(post(mission));
      } catch (error) { showError(error.message); }
    });
    likeButton.title = mission.liked ? "Beğenini geri al" : "Beğen";
    likeButton.append(icon("heart", 16), document.createTextNode(String(mission.counts.likes)));
    const commentButton = R.button("", "post-action", () => openMission(mission.code));
    commentButton.title = "Yorumlar";
    commentButton.append(icon("bubble.left", 16), document.createTextNode(String(mission.counts.comments)));
    const linkButton = R.button("", "post-action", async () => {
      try { await navigator.clipboard.writeText(mission.url); toast("Bağlantı kopyalandı."); } catch { /* clipboard off */ }
    });
    linkButton.title = "Bağlantıyı kopyala";
    linkButton.append(icon("square.and.arrow.up", 16));
    const saveButton = R.button("", `post-action ${mission.saved ? "liked" : ""}`.trim(), async () => {
      try {
        mission.saved = (await call(mission.saved ? "DELETE" : "POST", `missions/${mission.code}/save`)).saved;
        node.replaceWith(post(mission));
      } catch (error) { showError(error.message); }
    });
    saveButton.title = mission.saved ? "Kaydedilenlerden çıkar" : "Kaydet";
    saveButton.append(icon(mission.saved ? "bookmark.fill" : "bookmark", 16));
    const runButton = R.button("Dene", "primary small", () => openMission(mission.code));
    actions.append(likeButton, commentButton, saveButton, linkButton, R.el("span", "spacer"), runButton);
    node.append(body, actions);
    const used = [mission.counts.copies && `${mission.counts.copies} kez kullanıldı`, mission.counts.remixes && `${mission.counts.remixes} remix`].filter(Boolean).join(" · ");
    if (used) node.append(R.el("div", "muted small post-foot", used));
    return node;
  }

  function renderFeed() {
    $("discover-feeds").replaceChildren(...FEEDS.map(([id, text]) => R.button(text, `chip ${state.feed === id ? "on" : ""}`, () => { state.feed = id; load(); })));
    $("discover-list").replaceChildren(...state.missions.map(post));
    $("discover-empty").hidden = state.missions.length > 0;
    $("discover-empty").textContent = state.feed === "following"
      ? "Takip ettiğin kimse henüz bir şey paylaşmadı. Ara sekmesinden insanları bulup takip edebilirsin."
      : "Henüz paylaşılan Mission yok. Yeni Mission ile ilkini sen paylaş.";
    $("discover-more").hidden = !state.next;
  }

  async function load(more = false) {
    showTab("feed");
    const query = new URLSearchParams({ tab: state.feed });
    if (more && state.next) query.set("cursor", state.next);
    try {
      const answer = await call("GET", `missions/feed?${query}`);
      state.categories = answer.categories || state.categories;
      state.missions = more ? [...state.missions, ...answer.missions] : answer.missions;
      state.next = answer.next;
    } catch (error) {
      if (!more) state.missions = [];
      showError(error.message);
    }
    renderFeed();
    refreshUnread();
  }

  /** Ara: words and a topic, the results as a grid. */
  async function search() {
    showTab("search");
    const query = new URLSearchParams({ tab: state.q || state.category ? "new" : "popular" });
    if (state.category) query.set("category", state.category);
    if (state.q) query.set("q", state.q);
    try {
      const answer = await call("GET", `missions/feed?${query}`);
      state.categories = answer.categories || state.categories;
      state.results = answer.missions;
    } catch (error) {
      state.results = [];
      showError(error.message);
    }
    $("discover-categories").replaceChildren(...[{ id: "", label: "Tümü" }, ...state.categories].map((item) =>
      R.button(item.label, `chip ${state.category === item.id ? "on" : ""}`, () => { state.category = item.id; search(); })));
    let people = [];
    if (!state.category) { try { people = (await call("GET", `missions/people${state.q ? `?q=${encodeURIComponent(state.q)}` : ""}`)).people; } catch { /* the Missions still show */ } }
    const peopleBox = $("discover-people");
    peopleBox.replaceChildren(...(people.length ? [R.el("h4", "", state.q ? "Kişiler" : "Takip etmeye değer kişiler"), ...people.slice(0, state.q ? 8 : 5).map(personRow)] : []));
    peopleBox.hidden = people.length === 0;
    $("discover-results-title").hidden = state.results.length === 0;
    $("discover-results").replaceChildren(...state.results.map(missionCard));
    $("discover-results-empty").hidden = state.results.length > 0;
  }

  // ----- one Mission -----

  function section(title, items, ordered = false) {
    if (!items.length) return [];
    const list = R.el(ordered ? "ol" : "ul", "mission-list");
    for (const item of items) list.append(R.el("li", "", item));
    return [R.el("h4", "", title), list];
  }

  async function openMission(code) {
    const host = openSide();
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", `missions/${code}`);
    } catch (error) {
      host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    const mission = answer.mission;
    state.needs = answer.needs || state.needs;
    state.categories = answer.categories || state.categories;
    const head = R.el("div", "mission-head");
    head.append(icon(mission.icon || "sparkle", 20), R.el("h2", "", mission.name));
    const byline = R.el("div", "mission-meta muted small");
    if (mission.origin) {
      byline.append(R.el("span", "", "remix:"));
      byline.append(R.button(mission.origin.name, "link small", () => openMission(mission.origin.code)));
    }
    if (mission.visibility === "public" && counts(mission)) byline.append(R.el("span", "", counts(mission)));
    if (mission.hidden) byline.append(R.el("span", "error", "Bildirimler nedeniyle Keşfet'te gizlendi"));
    let following = false;
    if (mission.author && !mission.mine) {
      try { following = (await call("GET", `missions/users/${encodeURIComponent(mission.author.username)}`)).profile.following === true; } catch { /* the button starts as Takip et */ }
    }
    const authorRow = mission.author ? R.el("div", "author-row") : null;
    if (authorRow) {
      const who = R.el("button", "post-author");
      who.type = "button";
      const names = R.el("div");
      names.append(R.el("div", "w-title", mission.author.displayName || `@${mission.author.username}`), R.el("div", "muted small", mission.author.headline || `@${mission.author.username}`));
      who.append(avatar(mission.author), names);
      who.onclick = () => openProfile(mission.author.username);
      authorRow.append(who, R.el("span", "spacer"));
      if (!mission.mine) {
        const follow = R.button(following ? "Takipte" : "Takip et", following ? "secondary small" : "primary small", async () => {
          try { await call(following ? "DELETE" : "POST", `missions/users/${mission.author.username}/follow`); following = !following; follow.textContent = following ? "Takipte" : "Takip et"; follow.className = following ? "secondary small" : "primary small"; } catch (error) { showError(error.message); }
        });
        authorRow.append(follow);
      }
    }

    const actions = R.el("div", "mission-actions");
    actions.append(R.button("Dene · kendi Wiqo'mda çalıştır", "primary", () => run(mission)));
    const chips = R.el("div", "mission-actions");
    if (mission.visibility === "public") {
      const likeButton = R.button(mission.liked ? "Beğendin" : "Beğen", "secondary small", async () => {
        try {
          const done = await call(mission.liked ? "DELETE" : "POST", `missions/${mission.code}/like`);
          mission.liked = done.liked;
          mission.counts.likes = done.likes;
          likeButton.textContent = mission.liked ? "Beğendin" : "Beğen";
        } catch (error) { showError(error.message); }
      });
      chips.append(likeButton);
    }
    const saveButton = R.button(mission.saved ? "Kaydedildi" : "Kaydet", "secondary small", async () => {
      try { mission.saved = (await call(mission.saved ? "DELETE" : "POST", `missions/${mission.code}/save`)).saved; saveButton.textContent = mission.saved ? "Kaydedildi" : "Kaydet"; } catch (error) { showError(error.message); }
    });
    chips.append(saveButton);
    chips.append(R.button("Remix", "secondary small", () => publish({ mode: "remix", mission })));
    chips.append(R.button("Bağlantıyı kopyala", "secondary small", async () => {
      try { await navigator.clipboard.writeText(mission.url); toast("Bağlantı kopyalandı."); } catch { /* clipboard off */ }
    }));
    if (mission.mine) {
      chips.append(R.button("Düzenle", "secondary small", () => publish({ mode: "edit", mission })));
      chips.append(R.button("Sil", "danger small", () => remove(mission)));
    } else if (mission.visibility === "public") {
      chips.append(R.button("Bildir", "link small", () => reportMission(mission)));
    }

    const needs = mission.needs.map((id) => label(state.needs, id)).filter(Boolean);
    const gallery = mission.images.map((image) => {
      const picture = R.el("img", "mission-image");
      picture.src = image.url;
      picture.alt = "";
      return picture;
    });
    host.replaceChildren(...gallery.slice(0, 1), head, ...(authorRow ? [authorRow] : []), byline,
      ...(mission.description ? [R.el("p", "", mission.description)] : []),
      actions, chips,
      R.el("h4", "", "Wiqo'ya verilecek istek"), R.el("p", "mission-prompt", mission.prompt),
      ...section("Adımlar", mission.steps, true), ...section("Senden istenecekler", mission.inputs), ...section("Gerekenler", needs),
      ...(mission.preview ? [R.el("h4", "", "Örnek sonuç"), R.el("p", "mission-prompt muted", mission.preview)] : []),
      ...gallery.slice(1));
    if (mission.visibility === "public") {
      try {
        const remixes = (await call("GET", `missions/${mission.code}/remixes`)).missions;
        if (remixes.length) host.append(R.el("h4", "", "Remix'ler"), (() => { const grid = R.el("div", "mission-grid"); grid.append(...remixes.slice(0, 6).map(missionCard)); return grid; })());
      } catch { /* the page is complete without them */ }
      host.append(await commentsBlock(mission));
    }
  }

  /** Copies the Mission into the person's recipes and puts it in the composer: nothing runs until they read and send it. */
  async function run(mission) {
    const needs = mission.needs.map((id) => label(state.needs, id)).filter(Boolean);
    const offline = mission.needs.includes("pc") && !document.querySelector("#side-state.online");
    const lines = [
      mission.mine ? "" : "Bu istek başka biri tarafından yazıldı. Yazı kutusuna gelecek; göndermeden önce oku ve istersen değiştir. Bu iş boyunca her değişiklik için senden onay istenir.",
      needs.length ? `Gerekenler: ${needs.join("; ")}.` : "",
      mission.inputs.length ? `Senden istenecekler: ${mission.inputs.join("; ")}.` : "",
      offline ? "Bu bilgisayar şu an Wiqo'ya bağlı değil; bağlanmadan bilgisayar adımları çalışmaz." : ""
    ].filter(Boolean);
    const yes = await window.WiqoDialog.confirm({ title: `“${mission.name}” yazı kutusuna gelsin mi?`, message: lines.join("\n\n"), confirm: "Yazı kutusuna koy" });
    if (!yes) return;
    try {
      const answer = await call("POST", `missions/${mission.code}/copy`);
      window.WiqoChat.compose(answer.recipe.prompt, mission.mine ? null : mission.code);
    } catch (error) {
      showError(error.message);
    }
  }

  async function remove(mission) {
    const yes = await window.WiqoDialog.confirm({ title: `“${mission.name}” silinsin mi?`, message: "Bağlantısı çalışmaz olur ve Keşfet'ten kalkar. Başkalarının aldığı kopyalar onlarda kalır.", confirm: "Sil", danger: true });
    if (!yes) return;
    try {
      await call("DELETE", `missions/${mission.id}`);
      toast("Mission silindi.");
      $("discover-side").hidden = true;
      openMine();
    } catch (error) { showError(error.message); }
  }

  async function reportMission(mission) {
    const reason = await ask("Bu Mission'ı neden bildiriyorsun?", "Örn. zararlı bir istek, kişisel bilgi, spam");
    if (reason === null) return;
    try {
      await call("POST", `missions/${mission.code}/report`, { reason });
      toast("Bildirildi. Teşekkürler.");
    } catch (error) { showError(error.message); }
  }

  /** A one-line answer in the window's own dialog style (the webview's prompt() does not show). */
  function ask(title, placeholder, value = "") {
    return new Promise((resolve) => {
      const root = R.el("div", "modal");
      const card = R.el("form", "modal-card");
      const field = R.el("textarea");
      field.rows = 3;
      field.maxLength = 1000;
      field.placeholder = placeholder;
      field.value = value;
      const actions = R.el("div", "modal-actions");
      const done = (answer) => { root.remove(); resolve(answer); };
      actions.append(R.el("span", "spacer"), R.button("Vazgeç", "secondary", () => done(null)));
      const ok = R.el("button", "primary", "Tamam");
      ok.type = "submit";
      actions.append(ok);
      card.append(R.el("h2", "", title), field, actions);
      card.onsubmit = (event) => { event.preventDefault(); done(field.value.trim()); };
      root.append(card);
      document.body.append(root);
      field.focus();
    });
  }

  // ----- comments -----

  async function commentsBlock(mission) {
    const block = R.el("div", "mission-comments");
    block.append(R.el("h4", "", "Yorumlar"));
    let list = [];
    try {
      list = (await call("GET", `missions/${mission.code}/comments`)).comments;
    } catch (error) {
      block.append(R.el("p", "error small", error.message));
      return block;
    }
    const byParent = new Map();
    for (const comment of list) byParent.set(comment.parentId, [...(byParent.get(comment.parentId) || []), comment]);
    const reload = () => openMission(mission.code);
    const commentNode = (comment) => {
      const node = R.el("div", `comment ${comment.parentId ? "reply" : ""}`.trim());
      const head = R.el("div", "mission-meta muted small");
      if (comment.author) head.append(avatar(comment.author, "tiny"), personLink(comment.author));
      head.append(R.el("span", "", new Date(`${comment.createdAt}Z`).toLocaleString("tr-TR", { dateStyle: "short", timeStyle: "short" })));
      if (comment.editedAt) head.append(R.el("span", "", "düzenlendi"));
      node.append(head, R.el("p", comment.deleted ? "muted small" : "", comment.deleted ? "Bu yorum silindi." : comment.body));
      if (!comment.deleted) {
        const tools = R.el("div", "comment-tools");
        tools.append(R.button("Yanıtla", "link small", async () => {
          const body = await ask("Yanıt", "Yanıtını yaz");
          if (body) sendComment(mission, body, comment.id, reload);
        }));
        if (comment.mine) tools.append(R.button("Düzenle", "link small", async () => {
          const body = await ask("Yorumu düzenle", "", comment.body);
          if (!body) return;
          try { await call("PATCH", `missions/comments/${comment.id}`, { body }); reload(); } catch (error) { showError(error.message); }
        }));
        if (comment.canDelete) tools.append(R.button("Sil", "link small", async () => {
          try { await call("DELETE", `missions/comments/${comment.id}`); reload(); } catch (error) { showError(error.message); }
        }));
        node.append(tools);
      }
      return node;
    };
    for (const comment of byParent.get(null) || []) {
      block.append(commentNode(comment));
      for (const reply of byParent.get(comment.id) || []) block.append(commentNode(reply));
    }
    if (list.length === 0) block.append(R.el("p", "muted small", "Henüz yorum yok."));
    const form = R.el("form", "comment-form");
    const field = R.el("textarea");
    field.rows = 2;
    field.maxLength = 1000;
    field.placeholder = "Yorum yaz";
    const send = R.el("button", "primary small", "Gönder");
    send.type = "submit";
    form.append(field, send);
    form.onsubmit = (event) => {
      event.preventDefault();
      if (field.value.trim()) sendComment(mission, field.value.trim(), null, reload);
    };
    block.append(form);
    return block;
  }

  async function sendComment(mission, body, parentId, done) {
    try {
      await call("POST", `missions/${mission.code}/comments`, { body, parentId });
      done();
    } catch (error) {
      if (error.code === "USERNAME_REQUIRED") { toast(error.message); openMine(); return; }
      showError(error.message);
    }
  }

  // ----- people -----

  /** A person as a row: face, name and headline, one follow button. */
  function personRow(person) {
    const row = R.el("div", "person-row");
    const main = R.el("button", "post-author");
    main.type = "button";
    const names = R.el("div");
    names.append(R.el("div", "w-title", person.displayName || `@${person.username}`), R.el("div", "muted small ellipsis", person.headline || `@${person.username}`));
    main.append(avatar(person), names);
    main.onclick = () => openProfile(person.username);
    row.append(main, R.el("span", "spacer"));
    if (!person.mine) {
      const follow = R.button(person.following ? "Takipte" : "Takip et", person.following ? "secondary small" : "primary small", async () => {
        try {
          await call(person.following ? "DELETE" : "POST", `missions/users/${person.username}/follow`);
          person.following = !person.following;
          follow.textContent = person.following ? "Takipte" : "Takip et";
          follow.className = person.following ? "secondary small" : "primary small";
        } catch (error) { showError(error.message); }
      });
      row.append(follow);
    }
    return row;
  }

  /** Followers or following of one person, in the same place as their profile, with a way back. */
  async function showPeople(host, username, kind, back) {
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    try {
      const people = (await call("GET", `missions/users/${encodeURIComponent(username)}/${kind}`)).people;
      host.replaceChildren(R.button("‹ Geri", "link small", back), R.el("h3", "", kind === "followers" ? "Takipçiler" : "Takip edilenler"),
        ...(people.length ? people.map(personRow) : [R.el("p", "muted small", "Kimse yok.")]));
    } catch (error) { host.replaceChildren(R.el("p", "error small", error.message)); }
  }

  /** A profile as a professional network draws it: cover, face, name and headline, place and link, the numbers, then tabs of what they made. */
  async function renderProfile(host, username, tab = "missions") {
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", `missions/users/${encodeURIComponent(username)}?tab=${tab}`);
    } catch (error) {
      host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    const profile = answer.profile;
    const again = (next = tab) => renderProfile(host, username, next);
    const cover = R.el("div", "profile-cover");
    cover.style.background = coverBackground(profile.cover);
    const top = R.el("div", "profile-top");
    const actions = R.el("div", "mission-actions");
    if (profile.mine) {
      actions.append(R.button("Profili düzenle", "secondary small", () => editProfile(profile, () => again())), R.button("Yeni Mission", "primary small", () => publish({ mode: "blank" })));
    } else if (profile.blocked) {
      actions.append(R.button("Engeli kaldır", "secondary small", async () => {
        try { await call("DELETE", `missions/users/${profile.username}/block`); again(); } catch (error) { showError(error.message); }
      }));
    } else {
      actions.append(R.button(profile.following ? "Takipte" : "Takip et", profile.following ? "secondary small" : "primary small", async () => {
        try { await call(profile.following ? "DELETE" : "POST", `missions/users/${profile.username}/follow`); again(); } catch (error) { showError(error.message); }
      }), R.button("Engelle", "link small", async () => {
        if (!(await window.WiqoDialog.confirm({ title: `@${profile.username} engellensin mi?`, message: "Birbirinizin Mission'larını, yorumlarını görmezsiniz; takip biter.", confirm: "Engelle", danger: true }))) return;
        try { await call("POST", `missions/users/${profile.username}/block`); $("discover-side").hidden = true; load(); } catch (error) { showError(error.message); }
      }));
    }
    top.append(avatar(profile, "huge"), R.el("span", "spacer"), actions);
    const info = R.el("div", "profile-info");
    info.append(R.el("h2", "", profile.displayName || `@${profile.username}`), R.el("div", "muted small", `@${profile.username}`));
    if (profile.headline) info.append(R.el("div", "profile-headline", profile.headline));
    if (profile.bio) info.append(R.el("p", "", profile.bio));
    const links = R.el("div", "mission-meta muted small");
    if (profile.place) links.append(R.el("span", "", profile.place));
    if (profile.website) links.append(R.button(profile.website, "link small", () => invoke("open_link", { url: `https://${profile.website}` }).catch(() => {})));
    if (links.childNodes.length) info.append(links);
    const c = profile.counts;
    const stats = R.el("div", "profile-stats");
    const stat = (value, text, onClick) => {
      const cell = R.el(onClick ? "button" : "div", "profile-stat");
      if (onClick) { cell.type = "button"; cell.onclick = onClick; }
      cell.append(R.el("strong", "", String(value)), R.el("span", "muted small", text));
      return cell;
    };
    stats.append(stat(c.followers, "Takipçi", () => showPeople(host, username, "followers", () => again())), stat(c.following, "Takip", () => showPeople(host, username, "following", () => again())),
      stat(c.missions, "Mission"), stat(c.likes, "Beğeni"), stat(c.uses, "Deneme"));
    const tabs = R.el("div", "profile-tabs");
    for (const [id, text] of PROFILE_TABS) {
      if (id === "saved" && !profile.mine) continue;
      tabs.append(R.button(text, `profile-tab ${tab === id ? "on" : ""}`.trim(), () => again(id)));
    }
    const list = R.el("div", "mission-grid");
    const own = profile.mine;
    const card = (mission) => {
      const node = missionCard(mission);
      if (!mission.code) node.onclick = () => publish({ mode: "edit", mission });
      else if (own && tab === "missions") node.querySelector(".mission-meta")?.append(R.el("span", "", VISIBILITY.find(([id]) => id === mission.visibility)?.[1] || ""));
      return node;
    };
    list.append(...answer.missions.map(card));
    const empty = { remixes: own ? "Henüz bir Mission'ı remix'lemedin. Birinin Mission'ında Remix'e bas." : "Henüz remix yok.", liked: "Henüz beğenilen Mission yok.",
      saved: "Kaydettiğin Mission'lar burada durur; yalnızca sen görürsün.", missions: own ? "Henüz paylaştığın bir Mission yok. Yeni Mission'a bas." : "Henüz paylaşılan Mission yok." }[tab];
    host.replaceChildren(cover, top, info, stats, tabs, list, ...(answer.missions.length ? [] : [R.el("p", "muted small", empty)]));
    if (answer.next) {
      const more = R.button("Daha fazla", "secondary small", async () => {
        try {
          const page = await call("GET", `missions/users/${encodeURIComponent(username)}?tab=${tab}&cursor=${encodeURIComponent(more.dataset.next)}`);
          list.append(...page.missions.map(card));
          if (page.next) more.dataset.next = page.next; else more.remove();
        } catch (error) { showError(error.message); }
      });
      more.dataset.next = answer.next;
      host.append(more);
    }
  }

  async function openProfile(username) {
    window.showSection("discover");
    await renderProfile(openSide(), username);
  }

  /** The public profile in one small window: the cover is a row of colours, the rest are short fields with what is left to write beside them. */
  function editProfile(profile, done) {
    const root = R.el("div", "modal");
    const card = R.el("form", "modal-card profile-editor");
    const preview = R.el("div", "profile-preview");
    const previewName = R.el("div", "w-title");
    const previewLine = R.el("div", "small");
    const previewText = R.el("div");
    previewText.append(previewName, previewLine);
    preview.append(avatar(profile, "big"), previewText);
    const values = { cover: profile.cover || "ink" };
    const fields = {};
    const field = (name, text, limit, placeholder, multiline = false) => {
      const wrap = R.el("label", "profile-field");
      const input = R.el(multiline ? "textarea" : "input");
      if (!multiline) input.type = "text";
      else input.rows = 3;
      input.maxLength = limit;
      input.placeholder = placeholder;
      input.value = profile[name] || "";
      const counter = R.el("span", "muted small", `${input.value.length}/${limit}`);
      const head = R.el("div", "profile-field-head");
      head.append(R.el("span", "small", text), counter);
      input.addEventListener("input", () => { counter.textContent = `${input.value.length}/${limit}`; paint(); });
      wrap.append(head, input);
      fields[name] = input;
      return wrap;
    };
    const paint = () => {
      preview.style.background = coverBackground(values.cover);
      previewName.textContent = profile.displayName || `@${fields.username.value || ""}`;
      previewLine.textContent = fields.headline.value;
    };
    const swatches = R.el("div", "swatches");
    const drawSwatches = () => swatches.replaceChildren(...state.covers.map((name) => {
      const swatch = R.button("", `swatch ${values.cover === name ? "on" : ""}`.trim(), () => { values.cover = name; drawSwatches(); paint(); });
      swatch.title = name;
      swatch.style.background = coverBackground(name);
      swatch.type = "button";
      return swatch;
    }));
    const error = R.el("p", "form-error");
    error.hidden = true;
    const actions = R.el("div", "modal-actions");
    const close = () => root.remove();
    const save = R.el("button", "primary", "Kaydet");
    save.type = "submit";
    actions.append(R.el("span", "spacer"), R.button("Vazgeç", "secondary", close), save);
    card.append(R.el("h2", "", "Profili düzenle"), preview, R.el("div", "small muted", "Kapak rengi"), swatches,
      field("username", "Kullanıcı adı", 20, "kullanici_adi"), field("headline", "Başlık", 80, "Ne yaptığını bir cümlede söyle"),
      field("bio", "Hakkında", 280, "Kendini ve neyle ilgilendiğini anlat", true), field("place", "Konum", 40, "Şehir, ülke"), field("website", "Web sitesi", 80, "ornek.com/ben"),
      R.el("p", "muted small", "Ad ve fotoğraf, Ayarlar'daki profilinden gelir. Burada yazdığın her şey herkese açıktır; kişisel bilgi ve iletişim bilgisi yazma."), error, actions);
    fields.username.addEventListener("input", () => { fields.username.value = fields.username.value.toLowerCase(); });
    card.onsubmit = async (event) => {
      event.preventDefault();
      save.disabled = true;
      error.hidden = true;
      try {
        await call("PUT", "missions/me", { username: fields.username.value.trim(), headline: fields.headline.value.trim(), bio: fields.bio.value.trim(),
          place: fields.place.value.trim(), website: fields.website.value.trim(), cover: values.cover });
        close();
        toast("Profil kaydedildi.");
        done();
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      } finally { save.disabled = false; }
    };
    root.append(card);
    document.body.append(root);
    drawSwatches();
    paint();
    fields.headline.focus();
  }

  async function openMine() {
    window.showSection("discover");
    showTab("mine");
    const host = $("discover-mine");
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", "missions/me");
    } catch (error) {
      host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    state.me = answer.profile;
    state.categories = answer.categories;
    state.needs = answer.needs;
    state.covers = answer.covers || state.covers;
    if (answer.profile.username) { await renderProfile(host, answer.profile.username); return; }
    const card = R.el("div", "card profile-form");
    card.append(R.el("h3", "", "Keşfet profilin"), R.el("p", "muted small", "Keşfet'te Mission paylaşmak ve yorum yazmak için bir kullanıcı adı gerekir."),
      R.button("Profili oluştur", "primary small", () => editProfile(answer.profile, openMine)));
    host.replaceChildren(card);
  }

  async function openNotices() {
    window.showSection("discover");
    showTab("notices");
    const host = $("discover-notices");
    try {
      const answer = await call("GET", "missions/notifications");
      const rows = R.el("ul", "list");
      for (const notice of answer.notices) {
        const row = R.el("li", notice.read ? "" : "unread");
        const main = R.button(notice.text, "recipe-main notice", () => {
          if (notice.mission) openMission(notice.mission.code);
          else if (notice.actor.username) openProfile(notice.actor.username);
        });
        row.append(main, R.el("span", "muted small", new Date(`${notice.createdAt}Z`).toLocaleDateString("tr-TR")));
        rows.append(row);
      }
      const card = R.el("div", "card");
      card.append(rows);
      host.replaceChildren(answer.notices.length ? card : R.el("p", "muted small", "Henüz bildirim yok."));
      if (answer.unread) await call("POST", "missions/notifications/read");
      refreshUnread();
    } catch (error) {
      host.replaceChildren(R.el("p", "error small", error.message));
    }
  }

  async function refreshUnread() {
    try {
      const answer = await call("GET", "missions/notifications");
      for (const id of ["discover-unread", "discover-unread-rail"]) {
        $(id).textContent = answer.unread ? String(answer.unread) : "";
        $(id).hidden = !answer.unread;
      }
    } catch { /* the count waits for the next look */ }
  }

  // ----- publishing -----

  /**
   * One dialog for every way a Mission is written: from a finished task (the server drafts it), from a recipe, an edit, or a remix.
   * Nothing leaves the account until the person presses Kaydet, and the default is "Sadece ben".
   */
  async function publish(source) {
    const dialog = $("mission-dialog");
    const field = (name) => dialog.querySelector(`[name="${name}"]`);
    const findingsBox = $("mission-findings");
    findingsBox.replaceChildren();
    findingsBox.hidden = true;
    $("mission-error").hidden = true;
    $("mission-dialog-title").textContent = { blank: "Yeni Mission", task: "Mission olarak paylaş", recipe: "Tarifi paylaş", edit: "Mission'ı düzenle", remix: "Remix yap" }[source.mode];
    if (!state.categories.length || !state.needs.length) {
      try {
        const me = await call("GET", "missions/me");
        state.categories = me.categories;
        state.needs = me.needs;
        state.me = me.profile;
      } catch (error) { showError(error.message); return; }
    }
    field("category").replaceChildren(...state.categories.map((item) => {
      const option = R.el("option", "", item.label);
      option.value = item.id;
      return option;
    }));
    $("mission-needs").replaceChildren(...state.needs.map((item) => {
      const wrap = R.el("label", "pick");
      const box = R.el("input");
      box.type = "checkbox";
      box.value = item.id;
      wrap.append(box, R.el("span", "", item.label));
      return wrap;
    }));
    $("mission-visibility").replaceChildren(...VISIBILITY.map(([id, text, hint]) => {
      const wrap = R.el("label", "pick");
      wrap.title = hint;
      const radio = R.el("input");
      radio.type = "radio";
      radio.name = "visibility";
      radio.value = id;
      radio.onchange = showHint;
      wrap.append(radio, R.el("span", "", text));
      return wrap;
    }));
    const showHint = () => {
      const chosen = dialog.querySelector('[name="visibility"]:checked')?.value || "private";
      $("mission-visibility-hint").textContent = VISIBILITY.find(([id]) => id === chosen)?.[2] || "";
    };
    const fill = (mission) => {
      field("name").value = mission.name || "";
      field("description").value = mission.description || "";
      field("prompt").value = mission.prompt || "";
      field("steps").value = (mission.steps || []).join("\n");
      field("inputs").value = (mission.inputs || []).join("\n");
      field("tags").value = (mission.tags || []).join(", ");
      field("preview").value = mission.preview || "";
      field("category").value = mission.category || "other";
      for (const box of $("mission-needs").querySelectorAll("input")) box.checked = (mission.needs || []).includes(box.value);
      for (const radio of dialog.querySelectorAll('[name="visibility"]')) radio.checked = radio.value === (mission.visibility || "private");
      showHint();
    };
    fill({});
    renderImages(source.mode === "edit" ? source.mission : null);
    if (source.mode === "edit") fill(source.mission);
    if (source.mode === "recipe") fill({ ...source.recipe, visibility: source.recipe.visibility === "private" ? "link" : source.recipe.visibility });
    if (source.mode === "remix") fill({ ...source.mission, name: `${source.mission.name} (remix)`, preview: "", visibility: "private" });
    dialog.hidden = false;

    if (source.mode === "task") {
      const save = $("mission-save");
      save.disabled = true;
      save.textContent = "Taslak yazılıyor…";
      try {
        const answer = await call("POST", "missions/draft", { request: source.request, result: source.result });
        fill({ ...answer.draft, visibility: "private" });
        showFindings(answer.findings);
      } catch (error) {
        fill({ prompt: source.request, visibility: "private" });
        $("mission-error").textContent = `${error.message} İsteğini olduğu gibi koydum; kişisel bilgileri çıkarıp kendin düzenleyebilirsin.`;
        $("mission-error").hidden = false;
      } finally {
        save.disabled = false;
        save.textContent = "Kaydet";
      }
    }

    $("mission-cancel").onclick = $("mission-close").onclick = () => { dialog.hidden = true; };
    dialog.querySelector("details").open = source.mode === "edit";
    $("mission-form").onsubmit = async (event) => {
      event.preventDefault();
      const lines = (name) => field(name).value.split("\n").map((line) => line.trim()).filter(Boolean);
      const visibility = dialog.querySelector('[name="visibility"]:checked')?.value || "private";
      const body = {
        name: field("name").value, description: field("description").value, prompt: field("prompt").value, steps: lines("steps"), inputs: lines("inputs"),
        tags: field("tags").value.split(",").map((tag) => tag.trim()).filter(Boolean), preview: field("preview").value, category: field("category").value,
        needs: [...$("mission-needs").querySelectorAll("input:checked")].map((box) => box.value), visibility
      };
      const save = $("mission-save");
      save.disabled = true;
      save.textContent = visibility === "public" ? "Kontrol ediliyor…" : "Kaydediliyor…";
      $("mission-error").hidden = true;
      try {
        const id = source.mode === "edit" ? source.mission.id : source.mode === "recipe" ? source.recipe.id : null;
        const answer = id ? await call("PATCH", `missions/${id}`, body) : await call("POST", "missions", source.mode === "remix" ? { ...body, origin: source.mission.code } : body);
        dialog.hidden = true;
        const mission = answer.mission;
        if (mission.url) {
          try { await navigator.clipboard.writeText(mission.url); } catch { /* clipboard off */ }
        }
        toast(visibility === "public" ? "Keşfet'te yayında; bağlantısı kopyalandı." : visibility === "link" ? "Bağlantı kopyalandı." : "Tariflerine kaydedildi; kimse görmez.");
        window.dispatchEvent(new CustomEvent("wiqo-recipes-changed"));
        if (mission.code) openMission(mission.code);
      } catch (error) {
        showFindings(error.findings || []);
        $("mission-error").textContent = error.message;
        $("mission-error").hidden = false;
        if (error.code === "USERNAME_REQUIRED") { dialog.hidden = true; toast(error.message); openMine(); }
      } finally {
        save.disabled = false;
        save.textContent = "Kaydet";
      }
    };
  }

  /** A saved Mission's pictures: each one is checked on the server before it is kept, so adding one waits for that check. */
  function renderImages(mission) {
    const host = $("mission-images");
    const error = R.el("p", "error small");
    error.hidden = true;
    if (!mission?.id) {
      host.replaceChildren(R.el("p", "muted small", "Görselleri kaydettikten sonra Düzenle'den ekleyebilirsin (en fazla 3)."));
      return;
    }
    const thumbs = R.el("div", "mission-thumbs");
    for (const image of mission.images) {
      const cell = R.el("div", "mission-thumb");
      const picture = R.el("img");
      picture.src = image.url;
      picture.alt = "";
      const drop = R.button("", "icon-button", async () => {
        try {
          await call("DELETE", `missions/images/${image.id}`);
          mission.images = mission.images.filter((item) => item.id !== image.id);
          renderImages(mission);
        } catch (failure) { error.textContent = failure.message; error.hidden = false; }
      });
      drop.title = "Görseli kaldır";
      drop.append(icon("xmark", 12));
      cell.append(picture, drop);
      thumbs.append(cell);
    }
    host.replaceChildren(thumbs, error);
    if (mission.images.length >= 3) return;
    const pick = R.el("label", "secondary small file-pick", "Görsel ekle");
    const input = R.el("input");
    input.type = "file";
    input.accept = "image/png,image/jpeg,image/webp";
    input.hidden = true;
    pick.append(input);
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      if (file.size > 6_000_000) { error.textContent = "Görsel 6 MB'tan büyük."; error.hidden = false; return; }
      pick.firstChild.textContent = "Kontrol ediliyor…";
      try {
        const dataBase64 = await new Promise((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => resolve(String(reader.result).replace(/^data:[^,]+,/, ""));
          reader.onerror = () => reject(new Error("Görsel okunamadı."));
          reader.readAsDataURL(file);
        });
        mission.images = (await call("POST", `missions/${mission.id}/images`, { dataBase64 })).images;
        renderImages(mission);
      } catch (failure) {
        pick.firstChild.textContent = "Görsel ekle";
        error.textContent = failure.message;
        error.hidden = false;
      }
    };
    host.append(pick, R.el("p", "muted small", "Görseller paylaşılmadan önce kontrol edilir; yüz, iletişim bilgisi ya da özel mesaj görünen görsel eklenmez."));
  }

  const FIELD_NAMES = { name: "Başlık", description: "Açıklama", prompt: "İstek", steps: "Adımlar", inputs: "Senden istenecekler", preview: "Örnek sonuç", tags: "Etiketler" };

  function showFindings(findings) {
    const box = $("mission-findings");
    box.replaceChildren(...findings.map((item) => R.el("li", "", `${FIELD_NAMES[item.field] || item.field}: ${item.label} (${item.sample})`)));
    box.hidden = findings.length === 0;
  }

  // ----- wiring -----

  const TAB_OPENERS = { feed: () => load(), search, notices: openNotices, mine: openMine };
  for (const button of document.querySelectorAll(".rail-item")) button.onclick = () => TAB_OPENERS[button.dataset.tab]();
  $("discover-new").onclick = () => publish({ mode: "blank" });
  $("discover-side-close").onclick = () => { $("discover-side").hidden = true; };
  $("discover-more").onclick = () => load(true);
  $("discover-search-input").addEventListener("input", (event) => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => { state.q = event.target.value.trim(); search(); }, 300);
  });
  window.addEventListener("wiqo-section", (event) => { if (event.detail === "discover" && state.tab === "feed") load(); });
  window.addEventListener("wiqo-signed-in", refreshUnread);
  // A shared link (wiqo://recipe/CODE) opens the Mission it names.
  window.__TAURI__.event.listen("recipe-link", (event) => openMission(String(event.payload || "").toUpperCase()));

  window.WiqoDiscover = {
    open: openMission,
    publishTask: (request, result) => publish({ mode: "task", request, result }),
    publishRecipe: (recipe) => publish({ mode: "recipe", recipe }),
  };
})();
