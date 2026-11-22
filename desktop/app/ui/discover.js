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
  const FEEDS = [["popular", "Senin için"], ["following", "Takip"], ["new", "Yeni"], ["problems", "Problemler"]];
  const PROBLEM_TABS = [["open", "Çözüm bekleyen"], ["foryou", "Sana uygun"], ["new", "Yeni"], ["solved", "Çözülen"], ["needed", "Yaşadıklarım"]];
  const REPLY_KINDS = { comment: "Yorum", experience: "Deneyim", alternative: "Kullandığım araç", feedback: "Geri bildirim", suggestion: "Öneri" };
  const VISIBILITY = [["private", "Sadece ben", "Kayıtlı işlerimde durur, kimse görmez."], ["link", "Bağlantısı olan", "Bağlantıyı verdiğin kişi açar; adın görünmez."],
    ["public", "Keşfet'te herkes", "Adınla Keşfet'te görünür; başkaları kendine alır, yorumlar, kendi versiyonunu yapar."]];
  const COVERS = { ink: ["#121214", "#47474f"], ocean: ["#1a549e", "#40999f"], forest: ["#145c45", "#59a16b"], sunset: ["#db4f33", "#f7a847"],
    rose: ["#c73366", "#f5849e"], violet: ["#5c38b3", "#9e80eb"], sand: ["#b39466", "#e6d1a8"], slate: ["#404d61", "#8090a3"] };
  // One colour per person (backend PROFILE_COLORS): the circle behind the initial, and the cover when no picture is chosen.
  const PROFILE_COLORS = { indigo: "#555bd8", blue: "#2978f0", teal: "#0da0a3", green: "#33a854", orange: "#f28721", pink: "#e84987", purple: "#8c54e6", graphite: "#5c6470" };
  const colorOf = (key) => PROFILE_COLORS[key] || PROFILE_COLORS.indigo;
  const coverBackground = (name) => PROFILE_COLORS[name] ? `linear-gradient(135deg, ${colorOf(name)}d9, ${colorOf(name)})` : `linear-gradient(135deg, ${(COVERS[name] || COVERS.ink).join(", ")})`;
  const mediaRequests = new Map();
  const coverSources = new WeakMap();

  function nativeImage(source) {
    if (!/^https?:\/\/[^/]+\/missions\/(avatars|images|covers)\//.test(source)) return Promise.resolve(null);
    if (!mediaRequests.has(source)) {
      const request = invoke("discover_image", { url: source }).catch(() => null).finally(() => mediaRequests.delete(source));
      mediaRequests.set(source, request);
    }
    return mediaRequests.get(source);
  }

  function displayImage(image, source, onLoad) {
    image.alt = "";
    image.hidden = true;
    image.referrerPolicy = "no-referrer";
    image.onload = () => { image.hidden = false; onLoad?.(image); };
    image.onerror = async () => {
      image.onerror = null;
      const fallback = await nativeImage(source);
      if (fallback) image.src = fallback;
    };
    image.src = source;
  }

  /** A cover: the colour, and a chosen picture over it. */
  const paintCover = (node, color, url) => {
    const previous = coverSources.get(node);
    if (previous && previous.url === url && previous.color === color) return;
    const cover = { url, color };
    coverSources.set(node, cover);
    const background = coverBackground(color);
    node.style.background = background;
    if (url) displayImage(new Image(), url, (image) => {
      if (coverSources.get(node) === cover) node.style.background = `center / cover no-repeat url(${JSON.stringify(image.src)}), ${background}`;
    });
  };
  const PROFILE_TABS = [["missions", "Mission'lar"], ["remixes", "Remix'ler"], ["liked", "Beğenilenler"], ["saved", "Kaydedilenler"]];
  const state = { tab: "feed", covers: Object.keys(COVERS), feed: "popular", problemTab: "open", posts: [], category: "", q: "", next: null, missions: [], results: [], categories: [], needs: [], me: null };
  let searchTimer = null;
  let viewRequest = 0;
  const panelRequests = new WeakMap();

  async function call(method, path, body) {
    const answer = await invoke("missions_call", { method, path, body: body ?? null }).catch((error) => {
      throw error instanceof Error ? error : new Error(typeof error === "string" ? error : "Keşfet isteği tamamlanamadı.");
    });
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

  async function copyShareLink(url, prefix = "") {
    try {
      await navigator.clipboard.writeText(url);
      toast(`${prefix}Bağlantı kopyalandı.`);
    } catch {
      toast(`${prefix}Bağlantı kopyalanamadı.`);
    }
  }

  async function openWebLink(url) {
    try { await invoke("open_link", { url }); }
    catch { toast("Bağlantı açılamadı."); }
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
    node.textContent = author?.avatar?.emoji || (author?.displayName || author?.username || "?").slice(0, 1).toLocaleUpperCase("tr");
    node.style.background = colorOf(author?.avatar?.color);
    node.style.color = "#fff";
    if (author?.avatar?.url) displayImage(R.el("img"), author.avatar.url, (image) => node.replaceChildren(image));
    return node;
  }

  function personLink(author) {
    const link = R.button(`@${author.username}`, "link small", (event) => { event.stopPropagation(); openProfile(author.username); });
    return link;
  }

  /**
   * "Zamanla" (as the phone's): one way to make anything run on its own. The choice becomes the first words of the composer's request (the server reads
   * Turkish schedules), to be read and sent by the person.
   */
  function schedule(prompt, code = null) {
    const root = R.el("div", "modal");
    const card = R.el("div", "modal-card");
    card.append(R.el("h2", "", "Ne zaman çalışsın?"), R.el("p", "muted small", "Saati ve sıklığı yazı kutusunda değiştirebilirsin; gönderince Wiqo kendisi çalıştırır."));
    for (const [text, lead] of [["Her gün sabah", "Her gün saat 09:00'da şunu yap:"], ["Hafta içi her gün", "Hafta içi her gün saat 09:00'da şunu yap:"],
      ["Her hafta", "Her pazartesi saat 09:00'da şunu yap:"], ["Bir şey değişince haber ver", "Şunu izle; değişince ya da yeni bir şey çıkınca bana haber ver:"],
      ["Kendim yazayım", "Bu işi düzenli olarak çalıştır (ne sıklıkla ve saat kaçta olacağını yaz):"]]) {
      card.append(R.button(text, "secondary", () => { root.remove(); window.WiqoChat.compose(`${lead}\n\n${prompt}`, code, false); }));
    }
    const actions = R.el("div", "modal-actions");
    actions.append(R.el("span", "spacer"), R.button("Vazgeç", "secondary", () => root.remove()));
    card.append(actions);
    root.append(card);
    document.body.append(root);
  }

  /** The one number a card shows: how many people it worked for (their own verdict), or else how many took it. Never likes. */
  function trustText(mission) {
    if (mission.verdicts?.worked) return `${mission.verdicts.worked} kişide işe yaradı`;
    if (mission.counts.succeeded) return `${mission.counts.succeeded} kişi sonucunu aldı`;
    return mission.counts.copies ? `${mission.counts.copies} kişi aldı` : "";
  }

  function counts(mission) {
    const c = mission.counts;
    return [c.succeeded && `${c.succeeded} kişi kendi versiyonunu üretti`, c.likes && `${c.likes} beğeni`, c.comments && `${c.comments} yorum`, c.copies && `${c.copies} kez kullanıldı`, c.remixes && `${c.remixes} remix`].filter(Boolean).join(" · ");
  }

  // ----- views -----

  /** The middle of the page: one of the rail's tabs. */
  function showTab(tab) {
    clearTimeout(searchTimer);
    state.tab = tab;
    for (const name of ["feed", "search", "mine", "notices"]) $(`discover-${name}`).hidden = name !== tab;
    for (const button of document.querySelectorAll(".rail-item")) button.classList.toggle("active", button.dataset.tab === tab);
    showError("");
    return ++viewRequest;
  }

  function showDiscover() {
    if (document.body.dataset.section !== "discover") window.showSection("discover");
  }

  /** The side of the page, where a Mission or a person opens next to the feed. */
  function openSide() {
    showDiscover();
    $("discover-side").hidden = false;
    $("discover-side").scrollTop = 0;
    return $("discover-side-body");
  }

  function beginPanel(host) {
    const request = {};
    panelRequests.set(host, request);
    return () => panelRequests.get(host) === request;
  }

  function closeSide() {
    panelRequests.delete($("discover-side-body"));
    $("discover-side").hidden = true;
  }

  function missionCard(mission) {
    const card = R.el("button", "mission-card");
    card.type = "button";
    if (mission.images?.length) {
      const cover = R.el("img", "mission-cover");
      displayImage(cover, mission.images[0].url);
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
    const numbers = trustText(mission);
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
        R.el("div", "muted small", [`@${mission.author.username}`, label(state.categories, mission.category), mission.origin ? `kaynak: ${mission.origin.name}` : ""].filter(Boolean).join(" · ")));
      who.append(avatar(mission.author), names);
      who.onclick = () => openProfile(mission.author.username);
      head.append(who);
      node.append(head);
    }
    const body = R.el("button", "post-body");
    body.type = "button";
    if (mission.images?.length) {
      const picture = R.el("img", "post-image");
      displayImage(picture, mission.images[0].url);
      body.append(picture);
    }
    body.append(R.el("div", "post-title", mission.name));
    if (mission.description) body.append(R.el("p", "muted post-text", mission.description));
    body.onclick = () => openMission(mission.code);
    const actions = R.el("div", "post-actions");
    const commentButton = R.button("", "post-action", () => openMission(mission.code));
    commentButton.title = "Yorumlar";
    commentButton.append(icon("bubble.left", 16), document.createTextNode(String(mission.counts.comments)));
    const linkButton = R.button("", "post-action", () => copyShareLink(mission.url));
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
    const runButton = R.button("Kendine al", "primary small", () => openMission(mission.code));
    // One number people can trust instead of likes: how many it worked for, or else how many took it.
    const trust = trustText(mission);
    actions.append(commentButton, saveButton, ...(mission.url ? [linkButton] : []), ...(trust ? [R.el("span", "muted small", trust)] : []), R.el("span", "spacer"), runButton);
    node.append(body, actions);
    return node;
  }

  /** Search sits on top of the feed (the rail has no tab of its own for it). */
  function searchPill() {
    const pill = R.button("", "search discover-search discover-search-pill", () => { search(); setTimeout(() => $("discover-search-input")?.focus(), 0); });
    pill.append(icon("magnifyingglass", 13), R.el("span", "muted", "İş, problem ya da kişi ara"));
    return pill;
  }

  function renderFeed() {
    $("discover-feeds").replaceChildren(searchPill(), ...FEEDS.map(([id, text]) => R.button(text, `chip ${state.feed === id ? "on" : ""}`, () => { state.feed = id; load(); })));
    $("discover-list").replaceChildren(...state.missions.map(post));
    $("discover-empty").hidden = state.missions.length > 0;
    $("discover-empty").textContent = state.feed === "following"
      ? "Takip ettiğin kimse henüz bir şey paylaşmadı. Ara sekmesinden insanları bulup takip edebilirsin."
      : "Henüz paylaşılan iş yok. Bitirdiğin bir işi paylaş, ilk sen ol.";
    $("discover-more").hidden = !state.next;
  }

  async function load(more = false) {
    if (state.feed === "problems") return loadProblems(more);
    if (more && (!state.next || $("discover-more").hidden)) return;
    const request = showTab("feed");
    $("discover-more").hidden = true;
    if (!more) state.next = null;
    const query = new URLSearchParams({ tab: state.feed });
    if (more && state.next) query.set("cursor", state.next);
    try {
      const answer = await call("GET", `missions/feed?${query}`);
      if (request !== viewRequest) return;
      state.categories = answer.categories;
      state.missions = more ? [...state.missions, ...answer.missions] : answer.missions;
      state.next = answer.next;
    } catch (error) {
      if (request !== viewRequest) return;
      if (!more) state.missions = [];
      showError(error.message);
    }
    renderFeed();
    refreshUnread();
  }

  /** Ara: words and a topic, the results as a grid. */
  async function search() {
    const request = showTab("search");
    const { q, category } = state;
    const query = new URLSearchParams({ tab: q || category ? "new" : "popular" });
    if (category) query.set("category", category);
    if (q) query.set("q", q);
    const [missions, peopleResult, postsResult] = await Promise.allSettled([
      call("GET", `missions/feed?${query}`),
      category ? null : call("GET", `missions/people${q ? `?q=${encodeURIComponent(q)}` : ""}`),
      q.length < 2 ? null : call("GET", `missions/problems/feed?${new URLSearchParams({ tab: "new", q })}`),
    ]);
    if (request !== viewRequest || q !== state.q) return;
    const answer = missions.status === "fulfilled" ? missions.value : null;
    if (answer) state.categories = answer.categories;
    else showError(missions.reason.message);
    state.results = answer?.missions ?? [];
    $("discover-categories").replaceChildren(...[{ id: "", label: "Tümü" }, ...state.categories].map((item) =>
      R.button(item.label, `chip ${category === item.id ? "on" : ""}`, () => { state.category = item.id; search(); })));
    const people = peopleResult.value?.people ?? [];
    const peopleBox = $("discover-people");
    peopleBox.replaceChildren(...(people.length ? [R.el("h4", "", q ? "Kişiler" : "Takip etmeye değer kişiler"), ...people.slice(0, q ? 8 : 5).map(personRow)] : []));
    peopleBox.hidden = people.length === 0;
    // Problems that match the words: one search finds work, needs and people.
    let problemsBox = $("discover-problems");
    if (!problemsBox) {
      problemsBox = R.el("div");
      problemsBox.id = "discover-problems";
      $("discover-results-title").before(problemsBox);
    }
    const posts = postsResult.value?.posts.slice(0, 4) ?? [];
    problemsBox.replaceChildren(...(posts.length ? [R.el("h4", "", "Problemler"), ...posts.map(problemPost)] : []));
    $("discover-results-title").hidden = state.results.length === 0;
    $("discover-results").replaceChildren(...state.results.map(missionCard));
    $("discover-results-empty").hidden = !answer || state.results.length > 0 || posts.length > 0 || people.length > 0;
  }

  // ----- problems and ideas (rova-backend social/problems.ts) -----

  /** What people did around a problem, in one line: people who have it, what they wrote and what was built; never likes. */
  function postCounts(item) {
    const c = item.counts;
    return [c.needs && (item.kind === "problem" ? `${c.needs} kişi yaşıyor` : `${c.needs} kişi kullanırdı`), c.echoes && `${c.echoes} benzer paylaşım`, c.experiences && `${c.experiences} deneyim`,
      item.kind === "problem" && c.ideas && `${c.ideas} öneri`, c.solutions && `${c.solutions} çözüm`, c.worked && `${c.worked} kişide işe yaradı`].filter(Boolean).join(" · ");
  }

  async function toggleNeed(item) {
    const done = await call(item.needed ? "DELETE" : "POST", `missions/problems/${item.code}/need`);
    item.needed = done.needed;
    item.counts.needs = done.needs;
  }

  function needLabel(item) {
    return item.needed ? (item.kind === "problem" ? "Ben de yaşıyorum ✓" : "Kullanırdım ✓") : item.kind === "problem" ? "Ben de yaşıyorum" : "Kullanırdım";
  }

  function problemPost(item) {
    const node = R.el("article", "post");
    const head = R.el("div", "post-head");
    if (item.author) {
      const who = R.el("button", "post-author");
      who.type = "button";
      who.append(avatar(item.author, "tiny"), R.el("span", "muted small", `@${item.author.username}`));
      who.onclick = () => openProfile(item.author.username);
      head.append(who);
    }
    head.append(R.el("span", "spacer"), R.el("span", "muted small", item.solved ? "Çözüldü" : item.kind === "problem" ? "Problem" : "Fikir"));
    const body = R.el("button", "post-body");
    body.type = "button";
    if (item.problem) body.append(R.el("div", "muted small", `“${item.problem.title}” için`));
    body.append(R.el("div", "post-title", item.title));
    if (item.body) body.append(R.el("p", "muted post-text", item.body));
    body.onclick = () => openProblem(item.code);
    const actions = R.el("div", "post-actions");
    if (!item.mine) actions.append(R.button(needLabel(item), item.needed ? "primary small" : "secondary small", async () => {
      try { await toggleNeed(item); node.replaceWith(problemPost(item)); } catch (error) { showError(error.message); }
    }));
    const talk = R.button("", "post-action", () => openProblem(item.code));
    talk.title = "Tartışma";
    talk.append(icon("bubble.left", 16), document.createTextNode(String(item.counts.replies)));
    actions.append(talk, R.el("span", "spacer"), R.button("Çöz", "primary small", () => openProblem(item.code)));
    node.append(head, body);
    const numbers = postCounts(item);
    if (numbers) node.append(R.el("div", "muted small post-foot", numbers));
    node.append(actions);
    return node;
  }

  /** Akış's "Problemler": what people need solved, the most needed first, with the "+" that asks for a new one. */
  async function loadProblems(more = false) {
    if (more && (!state.next || $("discover-more").hidden)) return;
    const request = showTab("feed");
    $("discover-more").hidden = true;
    if (!more) state.next = null;
    const query = new URLSearchParams({ tab: state.problemTab });
    if (more && state.next) query.set("cursor", state.next);
    try {
      const answer = await call("GET", `missions/problems/feed?${query}`);
      if (request !== viewRequest) return;
      state.categories = answer.topics;
      state.posts = more ? [...state.posts, ...answer.posts] : answer.posts;
      state.next = answer.next;
    } catch (error) {
      if (request !== viewRequest) return;
      if (!more) state.posts = [];
      showError(error.message);
    }
    $("discover-feeds").replaceChildren(searchPill(), ...FEEDS.map(([id, text]) => R.button(text, `chip ${state.feed === id ? "on" : ""}`, () => { state.feed = id; load(); })),
      R.el("span", "spacer"), R.button("Problem paylaş", "primary small", () => writePost()));
    const tabs = R.el("div", "mission-actions");
    tabs.append(...PROBLEM_TABS.map(([id, text]) => R.button(text, `chip ${state.problemTab === id ? "on" : ""}`, () => { state.problemTab = id; loadProblems(); })));
    $("discover-list").replaceChildren(tabs, ...state.posts.map(problemPost));
    $("discover-empty").hidden = state.posts.length > 0;
    $("discover-empty").textContent = state.problemTab === "needed" ? "“Ben de yaşıyorum” dediğin bir problem yok."
      : "Burada henüz bir şey yok. Yaşadığın bir problemi paylaş: başkaları da yaşıyorsa birlikte çözersiniz.";
    $("discover-more").hidden = !state.next;
    refreshUnread();
  }

  /**
   * Writing a problem (or, with `problemCode`, an idea for one). Before it is posted the problems that already say the same are offered, so the person
   * can say "Ben de yaşıyorum" there instead of writing it twice.
   */
  function writePost(problemCode = null, posted = null) {
    const root = R.el("div", "modal");
    const card = R.el("form", "modal-card");
    card.append(R.el("h2", "", problemCode ? "Fikrin ne?" : "Hangi problemi yaşıyorsun?"), R.el("p", "muted small", "Herkes görür. Kişisel bilgi yazma."));
    const title = R.el("input");
    title.maxLength = 120;
    title.placeholder = problemCode ? "Örn. Ödeme gününde otomatik hatırlatma" : "Örn. Kiracılarımın ödemelerini takip edemiyorum";
    const text = R.el("textarea");
    text.rows = 4;
    text.maxLength = 2000;
    text.placeholder = problemCode ? "Nasıl çalışır, kime yarar?" : "Biraz anlat: ne zaman, neden zor, bugün nasıl idare ediyorsun?";
    const similarBox = R.el("div");
    const error = R.el("p", "form-error");
    error.hidden = true;
    let checked = Boolean(problemCode);
    const fail = (message) => { error.textContent = message; error.hidden = false; };
    const done = (item) => { root.remove(); if (posted) posted(item); else { state.feed = "problems"; load(); openProblem(item.code); } };
    const create = async (force) => {
      try {
        const answer = await call("POST", problemCode ? `missions/problems/${problemCode}/ideas` : "missions/problems", { title: title.value.trim(), body: text.value.trim(), force });
        done(answer.post);
      } catch (failure) { fail(failure.message); }
    };
    const actions = R.el("div", "modal-actions");
    const cancel = R.button("Vazgeç", "secondary", () => root.remove());
    cancel.type = "button";
    const submit = R.el("button", "primary", "Paylaş");
    submit.type = "submit";
    actions.append(cancel, R.el("span", "spacer"), submit);
    card.append(title, text, similarBox, error, actions);
    card.onsubmit = async (event) => {
      event.preventDefault();
      if (title.value.trim().length < 8) return fail("Başlık birkaç kelime olmalı.");
      if (!checked) {
        checked = true;
        let similar = [];
        try { similar = (await call("POST", "missions/problems/similar", { title: title.value.trim(), body: text.value.trim() })).similar; } catch { /* posting still works */ }
        if (similar.length) {
          similarBox.replaceChildren(R.el("h4", "", "Aynı problemi yaşayan başkaları da var"),
            R.el("p", "muted small", "Katılırsan ihtiyaç tek yerde toplanır ve daha görünür olur."),
            ...similar.slice(0, 3).map((item) => {
              const row = R.el("div", "author-row");
              row.append(R.el("span", "", item.title), R.el("span", "spacer"), R.button("Katıl", "secondary small", async () => {
                try { if (!item.mine) await call("POST", `missions/problems/${item.code}/need`); root.remove(); openProblem(item.code); } catch (failure) { fail(failure.message); }
              }));
              return row;
            }));
          submit.textContent = "Yine de ayrı paylaş";
          return;
        }
      }
      create(submit.textContent !== "Paylaş");
    };
    root.append(card);
    document.body.append(root);
    title.focus();
  }

  /** "İşe yaradı mı?": what ranks a solution and marks a problem solved. Only someone who tried it answers; the author sees the counts. */
  function verdictRow(mission) {
    const v = mission.verdicts || { worked: 0, partly: 0, failed: 0, mine: null };
    const box = R.el("div", "mission-actions");
    if (mission.mine) {
      if (v.worked + v.partly + v.failed > 0) box.append(R.el("span", "muted small", `Deneyenler: ${v.worked} işe yaradı · ${v.partly} kısmen · ${v.failed} işe yaramadı`));
      return box;
    }
    box.append(R.el("span", "muted small", mission.solves ? "Problemini çözdü mü?" : "Denedin mi, işe yaradı mı?"));
    for (const [id, text, count] of [["worked", "İşe yaradı", v.worked], ["partly", "Kısmen", v.partly], ["failed", "Yaramadı", v.failed]]) {
      box.append(R.button(count ? `${text} ${count}` : text, v.mine === id ? "primary small" : "secondary small", async () => {
        try {
          mission.verdicts = (await call("POST", `missions/${mission.code}/verdict`, { verdict: v.mine === id ? null : id })).verdicts;
          box.replaceWith(verdictRow(mission));
        } catch (error) { showError(error.message); }
      }));
    }
    return box;
  }

  /**
   * One problem (or idea) beside the feed: who has it, its talk, its ideas and solutions, what Wiqo found on the web, and "Bu probleme çözüm üret", which
   * reads it all into ways to solve it; the chosen way's words go to the composer with the post's code (read as someone else's words: every change on
   * this computer is asked), and what is shared from the result solves this post.
   */
  async function openProblem(code) {
    const host = openSide();
    const current = beginPanel(host);
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let detail;
    let replies = [];
    try {
      [detail, replies] = await Promise.all([call("GET", `missions/problems/${code}`), call("GET", `missions/problems/${code}/replies`).then((answer) => answer.replies).catch(() => [])]);
    } catch (error) {
      if (current()) host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    if (!current()) return;
    const item = detail.post;
    const isProblem = item.kind === "problem";
    const again = () => { if (current()) openProblem(code); };
    const note = R.el("p", "muted small");
    note.hidden = true;
    const say = (message) => { note.textContent = message; note.hidden = false; };

    const head = R.el("div", "mission-meta muted small");
    head.append(R.el("span", "", isProblem ? "Problem" : "Fikir"));
    if (item.solved) head.append(R.el("span", "", "Çözüldü"));
    if (item.problem) head.append(R.button(`“${item.problem.title}” için`, "link small", () => openProblem(item.problem.code)));
    const byline = R.el("div", "mission-meta muted small");
    if (item.author) byline.append(avatar(item.author, "tiny"), personLink(item.author));
    byline.append(R.el("span", "", new Date(`${item.createdAt}Z`).toLocaleDateString("tr-TR")));
    const people = [isProblem ? `${item.counts.needs} kişi bu problemi yaşıyor` : `${item.counts.needs} kişi bunu kullanırdı`,
      item.counts.validated > 0 && item.counts.validated < item.counts.needs ? `${item.counts.validated} doğrulanmış hesap` : "",
      item.counts.echoes ? `${item.counts.echoes} kişi benzerini yazdı` : ""].filter(Boolean).join(" · ");

    const briefBox = R.el("div");
    const solve = R.button(isProblem ? "Bu probleme çözüm üret" : "Bu fikri üret", "primary", async () => {
      solve.disabled = true;
      solve.textContent = "Problem okunuyor…";
      try {
        const { brief } = await call("POST", `missions/problems/${item.code}/brief`);
        briefBox.replaceChildren(R.el("h4", "", "Nasıl çözülsün? · Wiqo'nun önerisi"),
          ...(brief.summary ? [R.el("p", "", brief.summary)] : []),
          ...(brief.needs.length ? [R.el("p", "muted small", `Gereksinimler: ${brief.needs.join("; ")}`)] : []),
          ...brief.options.map((option) => {
            const row = R.el("div", "try-result");
            row.append(R.el("div", "w-title", `${option.title} · ${option.effort}`), R.el("p", "muted small", option.approach),
              R.button("Bununla başla", "primary small", () => window.WiqoChat.compose(option.prompt, item.code, false)));
            return row;
          }),
          ...(brief.acceptance.length ? [R.el("p", "muted small", `Kabul ölçütleri: ${brief.acceptance.join("; ")}`)] : []),
          R.el("p", "muted small", "Seçtiğin yolun isteği yazı kutusuna gelir; göndermeden önce oku."));
      } catch (error) { say(error.message); }
      solve.disabled = false;
      solve.textContent = isProblem ? "Bu probleme çözüm üret" : "Bu fikri üret";
    });
    const chips = R.el("div", "mission-actions");
    if (!item.mine) chips.append(R.button(needLabel(item), item.needed ? "primary small" : "secondary small", async () => {
      try { await toggleNeed(item); again(); } catch (error) { say(error.message); }
    }));
    chips.append(R.button("Bağlantıyı kopyala", "secondary small", () => copyShareLink(item.url)));
    if (item.mine) {
      chips.append(R.button("Sil", "danger small", async () => {
        if (!(await window.WiqoDialog.confirm({ title: "Paylaşım silinsin mi?", message: "Fikirleri ve tartışması da silinir. Çözümler sahiplerinde kalır.", confirm: "Sil", danger: true }))) return;
        try { await call("DELETE", `missions/problems/${item.code}`); if (current()) closeSide(); if (state.tab === "feed") load(); } catch (error) { say(error.message); }
      }));
    } else {
      chips.append(R.button("Bildir", "link small", async () => {
        const reason = await ask("Bu paylaşımı neden bildiriyorsun?", "Örn. spam, hakaret, kişisel bilgi");
        if (reason === null) return;
        try { await call("POST", `missions/problems/${item.code}/report`, { reason }); say("Bildirildi. Teşekkürler."); } catch (error) { say(error.message); }
      }));
    }

    const list = (title, items, open, sub) => items.length ? [R.el("h4", "", title), ...items.map((entry) => {
      const row = R.el("button", "mission-card");
      row.type = "button";
      row.append(R.el("div", "w-title", entry.title || entry.name));
      const line = sub(entry);
      if (line) row.append(R.el("div", "muted small", line));
      row.onclick = () => open(entry.code);
      return row;
    })] : [];
    const solutions = list(`Çözümler (${detail.solutions.length})`, detail.solutions, openMission, (mission) => [mission.author ? `@${mission.author.username}` : "",
      mission.verdicts?.worked ? `${mission.verdicts.worked} kişide işe yaradı` : "", mission.verdicts?.partly ? `${mission.verdicts.partly} kısmen` : "", mission.verdicts?.failed ? `${mission.verdicts.failed} işe yaramadı` : ""].filter(Boolean).join(" · "));
    const ideas = list(`Eski fikirler (${detail.ideas.length})`, detail.ideas, openProblem, postCounts);
    const similar = list("Benzer problemler", detail.similar, openProblem, postCounts);

    const insightBox = R.el("div");
    const showInsight = (insight) => {
      if (!insight) {
        const button = R.button("Bugün ne kullanılıyor, araştır", "secondary small", async () => {
          button.disabled = true;
          button.textContent = "Web'de bakılıyor…";
          try { showInsight((await call("POST", `missions/problems/${item.code}/research`)).insight); } catch (error) { say(error.message); button.disabled = false; button.textContent = "Bugün ne kullanılıyor, araştır"; }
        });
        insightBox.replaceChildren(R.el("h4", "", "Mevcut alternatifler"), button);
        return;
      }
      insightBox.replaceChildren(R.el("h4", "", "Mevcut alternatifler"),
        ...(insight.summary ? [R.el("p", "", insight.summary)] : []),
        ...insight.alternatives.map((alternative) => {
          const row = R.el("div", "try-result");
          row.append(R.button(alternative.name, "link", () => openWebLink(alternative.url)));
          if (alternative.gap) row.append(R.el("p", "muted small", `Eksiği: ${alternative.gap}`));
          return row;
        }),
        ...(insight.gaps.length ? [R.el("p", "muted small", `Karşılanmayanlar: ${insight.gaps.join("; ")}`)] : []),
        ...(insight.audience ? [R.el("p", "muted small", `Kimler: ${insight.audience}`)] : []),
        ...(insight.benefit ? [R.el("p", "muted small", `Kazanç: ${insight.benefit}`)] : []),
        R.el("p", "muted small", "Wiqo'nun web araştırması; tahmindir, topluluğun sayılarına katılmaz."));
    };
    showInsight(detail.insight);

    const talk = R.el("div");
    const kinds = isProblem ? ["comment", "experience", "alternative", "suggestion"] : ["comment", "feedback", "alternative"];
    let kind = "comment";
    const kindChips = R.el("div", "mission-actions");
    const drawKinds = () => kindChips.replaceChildren(...kinds.map((id) => R.button(REPLY_KINDS[id], `chip ${kind === id ? "on" : ""}`, () => { kind = id; drawKinds(); })));
    drawKinds();
    const input = R.el("textarea");
    input.rows = 2;
    input.maxLength = 1000;
    input.placeholder = isProblem ? "Sen de yaşıyor musun? Deneyimini, kullandığın aracı ya da nasıl çözülebileceğini yaz." : "Bu fikir işine yarar mı? Neyi değiştirirdin?";
    const send = R.button("Gönder", "primary small", async () => {
      if (!input.value.trim()) return;
      try { await call("POST", `missions/problems/${item.code}/replies`, { kind, body: input.value.trim() }); again(); } catch (error) { say(error.message); }
    });
    talk.append(R.el("h4", "", item.counts.replies ? `Tartışma (${item.counts.replies})` : "Tartışma"),
      ...replies.map((reply) => {
        const row = R.el("div", "comment");
        if (reply.parentId) row.style.marginLeft = "24px";
        const meta = R.el("div", "mission-meta muted small");
        if (reply.author && !reply.deleted) meta.append(avatar(reply.author, "tiny"), personLink(reply.author));
        if (reply.kind !== "comment") meta.append(R.el("strong", "", REPLY_KINDS[reply.kind] || ""));
        if (reply.affected && reply.kind === "experience") meta.append(R.el("span", "", "yaşıyor"));
        if (reply.canDelete) meta.append(R.button("Sil", "link small", async () => {
          try { await call("DELETE", `missions/problems/replies/${reply.id}`); again(); } catch (error) { say(error.message); }
        }));
        row.append(meta, R.el("p", reply.deleted ? "muted small" : "", reply.deleted ? "Bu yazı silindi." : reply.body));
        return row;
      }),
      kindChips, input, send);

    host.replaceChildren(head, R.el("h2", "", item.title), byline,
      ...(item.body ? [R.el("p", "", item.body)] : []),
      ...(item.hidden ? [R.el("p", "error small", "Bildirimler nedeniyle gizlendi; inceleniyor.")] : []),
      ...(item.audience ? [R.el("h4", "", "Kimler yaşıyor"), R.el("p", "", item.audience)] : []),
      ...(item.workaround ? [R.el("h4", "", "Bugün nasıl idare ediliyor"), R.el("p", "", item.workaround)] : []),
      R.el("p", "w-title", people), solve, briefBox, chips, note, ...solutions, ...ideas, insightBox, ...similar, talk);
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
    const current = beginPanel(host);
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", `missions/${code}`);
    } catch (error) {
      if (current()) host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    if (!current()) return;
    const mission = answer.mission;
    state.needs = answer.needs || state.needs;
    state.categories = answer.categories || state.categories;
    const head = R.el("div", "mission-head");
    head.append(icon(mission.icon || "sparkle", 20), R.el("h2", "", mission.name));
    const byline = R.el("div", "mission-meta muted small");
    if (mission.origin) {
      byline.append(R.el("span", "", "kaynak:"));
      byline.append(R.button(mission.origin.name, "link small", () => openMission(mission.origin.code)));
      if (mission.origin.updatedSince) byline.append(R.el("span", "", "kaynak güncellendi"));
    }
    if (mission.solves) byline.append(R.el("span", "", "Çözdüğü:"), R.button(mission.solves.title, "link small", () => openProblem(mission.solves.code)));
    if (mission.visibility === "public" && trustText(mission)) byline.append(R.el("span", "", trustText(mission)));
    if (mission.hidden) byline.append(R.el("span", "error", "Bildirimler nedeniyle Keşfet'te gizlendi"));
    let following = false;
    if (mission.author && !mission.mine) {
      try { following = (await call("GET", `missions/users/${encodeURIComponent(mission.author.username)}`)).profile.following === true; } catch { /* the button starts as Takip et */ }
    }
    if (!current()) return;
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
    actions.append(R.button("Kendine al", "primary", () => run(mission)));
    const chips = R.el("div", "mission-actions");
    const saveButton = R.button(mission.saved ? "Kaydedildi" : "Kaydet", "secondary small", async () => {
      try { mission.saved = (await call(mission.saved ? "DELETE" : "POST", `missions/${mission.code}/save`)).saved; saveButton.textContent = mission.saved ? "Kaydedildi" : "Kaydet"; } catch (error) { showError(error.message); }
    });
    chips.append(saveButton);
    chips.append(R.button("Zamanla", "secondary small", async () => {
      try { schedule((await call("POST", `missions/${mission.code}/copy`)).recipe.prompt, mission.mine ? null : mission.code); } catch (error) { showError(error.message); }
    }));
    chips.append(R.button("Kendi versiyonunu paylaş", "secondary small", () => publish({ mode: "remix", mission })));
    if (mission.visibility === "public") {
      const likeButton = R.button(mission.liked ? "Beğendin" : "Beğen", "link small", async () => {
        try {
          const done = await call(mission.liked ? "DELETE" : "POST", `missions/${mission.code}/like`);
          mission.liked = done.liked;
          likeButton.textContent = mission.liked ? "Beğendin" : "Beğen";
        } catch (error) { showError(error.message); }
      });
      chips.append(likeButton);
    }
    if (mission.url) chips.append(R.button("Bağlantıyı kopyala", "secondary small", () => copyShareLink(mission.url)));
    if (mission.mine) {
      chips.append(R.button("Düzenle", "secondary small", () => publish({ mode: "edit", mission })));
      if (mission.solves && mission.id) chips.append(R.button("Problemden ayır", "link small", async () => {
        try { await call("PATCH", `missions/${mission.id}`, { solves: null }); if (current()) openMission(mission.code); } catch (error) { showError(error.message); }
      }));
      chips.append(R.button("Sil", "danger small", () => remove(mission, current)));
    } else if (mission.visibility === "public") {
      chips.append(R.button("Bildir", "link small", () => reportMission(mission)));
    }

    const needs = mission.needs.map((id) => label(state.needs, id)).filter(Boolean);
    const gallery = mission.images.map((image) => {
      const picture = R.el("img", "mission-image");
      displayImage(picture, image.url);
      return picture;
    });
    host.replaceChildren(...gallery.slice(0, 1), head, ...(authorRow ? [authorRow] : []), byline,
      ...(mission.description ? [R.el("p", "", mission.description)] : []),
      actions, chips, ...(mission.visibility === "public" ? [verdictRow(mission)] : []),
      R.el("h4", "", "Wiqo'ya verilecek istek"), R.el("p", "mission-prompt", mission.prompt),
      ...section("Adımlar", mission.steps, true), ...section("Senden istenecekler", mission.inputs), ...section("Gerekenler", needs),
      ...(mission.preview ? [R.el("h4", "", "Örnek sonuç"), R.el("p", "mission-prompt muted", mission.preview)] : []),
      ...gallery.slice(1));
    if (mission.visibility === "public") {
      try {
        const remixes = (await call("GET", `missions/${mission.code}/remixes`)).missions;
        if (!current()) return;
        if (remixes.length) host.append(R.el("h4", "", "Ondan yapılan versiyonlar"), (() => { const grid = R.el("div", "mission-grid"); grid.append(...remixes.slice(0, 6).map(missionCard)); return grid; })());
      } catch { /* the page is complete without them */ }
      if (!current()) return;
      const comments = await commentsBlock(mission, current);
      if (current()) host.append(comments);
    }
  }

  /** Copies the Mission into the person's recipes and puts it in the composer: nothing runs until they read and send it. */
  /**
   * "Dene": what the Mission made for its author, the person's own inputs, and two ways to go: run it now on this computer with their own data
   * (someone else's Mission still asks before every change), or put its words in the composer to read first.
   */
  async function run(mission) {
    const needs = mission.needs.map((id) => label(state.needs, id)).filter(Boolean);
    const offline = mission.needs.includes("pc") && !document.querySelector("#side-state.online");
    const root = R.el("div", "modal");
    const card = R.el("form", "modal-card try-card");
    card.append(R.el("h2", "", mission.name));
    if (!mission.mine) card.append(R.el("p", "muted small", "Başka biri yazdı; bu bilgisayarda, kendi verilerinle çalışır. Bilgisayarındaki her değişiklik için senden onay istenir."));
    if (needs.length) card.append(R.el("p", "muted small", `Gerekenler: ${needs.join("; ")}`));
    card.append(R.el("p", "muted small", "Hesabına yalnızca istek ve adımlar kopyalanır; paylaşan kişinin dosyaları, hesapları ve bilgisayarı kopyalanmaz."));
    if (offline) card.append(R.el("p", "muted small", "Bu bilgisayar şu an Wiqo'ya bağlı değil; bağlanmadan bilgisayar adımları çalışmaz."));
    if (mission.preview) {
      const result = R.el("div", "try-result");
      result.append(R.el("div", "small muted", mission.mine || !mission.author ? "Örnek sonuç" : `@${mission.author.username} için üretilen sonuç`), R.el("p", "", mission.preview));
      card.append(result);
    }
    const fields = mission.inputs.map((text) => {
      const field = R.el("textarea");
      field.rows = 2;
      field.placeholder = text;
      return [text, field];
    });
    if (fields.length) card.append(R.el("div", "small muted", "Senin bilgilerin"), ...fields.map(([, field]) => field));
    const error = R.el("p", "form-error");
    error.hidden = true;
    const start = async (sendNow) => {
      try {
        const answer = await call("POST", `missions/${mission.code}/copy`);
        const filled = fields.map(([text, field]) => [text, field.value.trim()]).filter(([, value]) => value);
        const prompt = filled.length ? `${answer.recipe.prompt}\n\nBenim bilgilerim:\n${filled.map(([text, value]) => `- ${text}: ${value}`).join("\n")}` : answer.recipe.prompt;
        root.remove();
        window.WiqoChat.compose(prompt, mission.mine ? null : mission.code, sendNow);
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      }
    };
    const actions = R.el("div", "modal-actions");
    const later = R.button("Önce yazı kutusunda gör", "secondary", () => start(false));
    later.type = "button";
    const now = R.el("button", "primary", mission.needs.includes("pc") || mission.needs.includes("files") ? "Bu bilgisayarda çalıştır" : "Çalıştır");
    now.type = "submit";
    const cancel = R.button("Vazgeç", "secondary", () => root.remove());
    cancel.type = "button";
    actions.append(cancel, R.el("span", "spacer"), later, now);
    card.append(error, actions);
    card.onsubmit = (event) => { event.preventDefault(); start(true); };
    root.append(card);
    document.body.append(root);
    (fields[0]?.[1] || now).focus();
  }

  async function remove(mission, current) {
    const yes = await window.WiqoDialog.confirm({ title: `“${mission.name}” silinsin mi?`, message: "Bağlantısı çalışmaz olur ve Keşfet'ten kalkar. Başkalarının aldığı kopyalar onlarda kalır.", confirm: "Sil", danger: true });
    if (!yes) return;
    try {
      await call("DELETE", `missions/${mission.id}`);
      toast("Paylaşım silindi.");
      if (current()) { closeSide(); openMine(); }
    } catch (error) { showError(error.message); }
  }

  async function reportMission(mission) {
    const reason = await ask("Bu paylaşımı neden bildiriyorsun?", "Örn. zararlı bir istek, kişisel bilgi, spam");
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

  async function commentsBlock(mission, current) {
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
    const reload = () => { if (current()) openMission(mission.code); };
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
    const current = beginPanel(host);
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    try {
      const people = (await call("GET", `missions/users/${encodeURIComponent(username)}/${kind}`)).people;
      if (!current()) return;
      host.replaceChildren(R.button("‹ Geri", "link small", back), R.el("h3", "", kind === "followers" ? "Takipçiler" : "Takip edilenler"),
        ...(people.length ? people.map(personRow) : [R.el("p", "muted small", "Kimse yok.")]));
    } catch (error) { if (current()) host.replaceChildren(R.el("p", "error small", error.message)); }
  }

  /** A profile as a professional network draws it: cover, face, name and headline, place and link, the numbers, then tabs of what they made. */
  async function renderProfile(host, username, tab = "missions") {
    const current = beginPanel(host);
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", `missions/users/${encodeURIComponent(username)}?tab=${tab}`);
    } catch (error) {
      if (current()) host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    if (!current()) return;
    const profile = answer.profile;
    const again = (next = tab) => renderProfile(host, username, next);
    const cover = R.el("div", "profile-cover");
    paintCover(cover, profile.cover, profile.coverUrl);
    const top = R.el("div", "profile-top");
    const actions = R.el("div", "mission-actions");
    if (profile.mine) {
      actions.append(R.button("Profili düzenle", "secondary small", editInSettings), R.button("Paylaş", "primary small", shareFromWork));
    } else if (profile.blocked) {
      actions.append(R.button("Engeli kaldır", "secondary small", async () => {
        try { await call("DELETE", `missions/users/${profile.username}/block`); if (current()) again(); } catch (error) { showError(error.message); }
      }));
    } else {
      actions.append(R.button(profile.following ? "Takipte" : "Takip et", profile.following ? "secondary small" : "primary small", async () => {
        try { await call(profile.following ? "DELETE" : "POST", `missions/users/${profile.username}/follow`); if (current()) again(); } catch (error) { showError(error.message); }
      }), R.button("Engelle", "link small", async () => {
        if (!(await window.WiqoDialog.confirm({ title: `@${profile.username} engellensin mi?`, message: "Birbirinizin paylaşımlarını, yorumlarını görmezsiniz; takip biter.", confirm: "Engelle", danger: true }))) return;
        try { await call("POST", `missions/users/${profile.username}/block`); if (current()) { closeSide(); load(); } } catch (error) { showError(error.message); }
      }));
    }
    top.append(avatar(profile, "huge"), R.el("span", "spacer"), actions);
    const info = R.el("div", "profile-info");
    info.append(R.el("h2", "", profile.displayName || `@${profile.username}`), R.el("div", "muted small", `@${profile.username}`));
    if (profile.headline) info.append(R.el("div", "profile-headline", profile.headline));
    if (profile.bio) info.append(R.el("p", "", profile.bio));
    const links = R.el("div", "mission-meta muted small");
    if (profile.place) links.append(R.el("span", "", profile.place));
    if (profile.website) links.append(R.button(profile.website, "link small", () => openWebLink(`https://${profile.website}`)));
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
      stat(c.missions, "İş"), stat(c.likes, "Beğeni"), stat(c.uses, "Alan"));
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
    const empty = { remixes: own ? "Henüz başkasının işinden bir versiyon yapmadın." : "Henüz versiyon yok.", liked: "Henüz beğenilen iş yok.",
      saved: "Kaydettiğin işler burada durur; yalnızca sen görürsün.", missions: own ? "Henüz paylaştığın bir iş yok." : "Henüz paylaşılan iş yok." }[tab];
    // The people this person makes things with (public remixes only): who built on their work, and whose work they built on.
    const makers = R.el("div", "profile-circle");
    call("GET", `missions/users/${encodeURIComponent(username)}/circle`).then((found) => {
      const people = [...found.builtOnThem.map((person) => [person, `${person.count} remix'ledi`]), ...found.builtOn.map((person) => [person, `${person.count} işinden üretti`])];
      if (!people.length) return;
      const row = R.el("div", "profile-circle-row");
      for (const [person, caption] of people) {
        const cell = R.button("", "profile-circle-person", () => openProfile(person.username));
        cell.type = "button";
        cell.append(avatar(person, "big"), R.el("div", "small ellipsis", person.displayName || `@${person.username}`), R.el("div", "muted small", caption));
        row.append(cell);
      }
      makers.append(R.el("div", "small muted", "Birlikte üretenler"), row);
    }).catch(() => {});
    host.replaceChildren(cover, top, info, stats, makers, tabs, list, ...(answer.missions.length ? [] : [R.el("p", "muted small", empty)]));
    if (answer.next) {
      const more = R.button("Daha fazla", "secondary small", async () => {
        more.disabled = true;
        try {
          const page = await call("GET", `missions/users/${encodeURIComponent(username)}?tab=${tab}&cursor=${encodeURIComponent(more.dataset.next)}`);
          if (!current()) return;
          list.append(...page.missions.map(card));
          if (page.next) more.dataset.next = page.next; else more.remove();
        } catch (error) { if (current()) showError(error.message); }
        finally { more.disabled = false; }
      });
      more.dataset.next = answer.next;
      host.append(more);
    }
  }

  async function openProfile(username) {
    await renderProfile(openSide(), username);
  }

  /**
   * The one profile, in one window opened from Settings (Keşfet's "Profili düzenle" leads here too): the name, then what others see on Keşfet —
   * handle, headline, bio, location, link and cover. Saved through `/profile`, the same row the phone's Settings edits. The photo is chosen on the phone.
   */
  async function editProfile(done = () => {}) {
    let profile;
    try { profile = await call("GET", "profile"); } catch (failure) { toast(failure.message); return; }
    const root = R.el("div", "modal");
    const card = R.el("form", "modal-card profile-editor");
    const preview = R.el("div", "profile-preview");
    const previewName = R.el("div", "w-title");
    const previewLine = R.el("div", "small");
    const previewText = R.el("div");
    previewText.append(previewName, previewLine);
    const face = () => avatar({ avatar: { kind: profile.avatarKind, emoji: profile.avatarKind === "emoji" ? profile.avatarEmoji : null, color: values.color,
      url: profile.avatarKind === "photo" && profile.avatarImage ? `data:image/jpeg;base64,${profile.avatarImage}` : profile.avatarKind === "initial" ? profile.avatarUrl : null }, displayName: profile.displayName }, "big");
    // `cover`: undefined (unchanged), null (back to the colour) or a cropped picture as base64.
    const values = { color: profile.avatarColor || "indigo", cover: undefined };
    const coverShown = () => (values.cover === undefined ? profile.coverUrl : values.cover ? `data:image/jpeg;base64,${values.cover}` : null);
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
      paintCover(preview, values.color, coverShown());
      preview.replaceChildren(face(), previewText);
      previewName.textContent = fields.displayName.value || `@${fields.username.value || ""}`;
      previewLine.textContent = fields.headline.value;
      remove.hidden = !coverShown();
    };
    const swatches = R.el("div", "swatches");
    const drawSwatches = () => swatches.replaceChildren(...Object.keys(PROFILE_COLORS).map((name) => {
      const swatch = R.button("", `swatch ${values.color === name ? "on" : ""}`.trim(), () => { values.color = name; drawSwatches(); paint(); });
      swatch.title = name;
      swatch.style.background = colorOf(name);
      swatch.type = "button";
      return swatch;
    }));
    const file = R.el("input");
    file.type = "file";
    file.accept = "image/*";
    file.hidden = true;
    file.onchange = () => {
      const chosen = file.files?.[0];
      file.value = "";
      if (!chosen) return;
      const reader = new FileReader();
      reader.onload = () => cropCover(reader.result, (base64) => { values.cover = base64; paint(); });
      reader.onerror = () => toast("Görsel açılamadı.");
      reader.readAsDataURL(chosen);
    };
    const remove = R.button("Renk", "secondary small", () => { values.cover = null; paint(); });
    remove.type = "button";
    const coverActions = R.el("div", "mission-actions");
    const fromLibrary = R.button("Kütüphane", "secondary small", () => coverLibrary((url) => cropCover(url, (base64) => { values.cover = base64; paint(); })));
    const fromDisk = R.button("Bilgisayardan", "secondary small", () => file.click());
    fromLibrary.type = "button";
    fromDisk.type = "button";
    coverActions.append(fromLibrary, fromDisk, remove, file);
    const error = R.el("p", "form-error");
    error.hidden = true;
    const actions = R.el("div", "modal-actions");
    const close = () => root.remove();
    const save = R.el("button", "primary", "Kaydet");
    save.type = "submit";
    actions.append(R.el("span", "spacer"), R.button("Vazgeç", "secondary", close), save);
    card.append(R.el("h2", "", "Profili düzenle"), preview, coverActions, R.el("div", "small muted", "Renk · profil ve kapak"), swatches,
      field("displayName", "Ad", 40, "Adın"), field("username", "Kullanıcı adı", 20, "kullanici_adi"), field("headline", "Başlık", 80, "Ne yaptığını bir cümlede söyle"),
      field("bio", "Hakkında", 280, "Kendini ve neyle ilgilendiğini anlat", true), field("location", "Konum", 40, "Şehir, ülke"), field("website", "Web sitesi", 80, "ornek.com/ben"),
      field("preferredAddress", "Hitap", 40, "Örn. Emre, Hocam"), error, actions);
    fields.username.addEventListener("input", () => { fields.username.value = fields.username.value.toLowerCase().replace(/^@/, ""); });
    card.onsubmit = async (event) => {
      event.preventDefault();
      save.disabled = true;
      error.hidden = true;
      const handle = fields.username.value.trim();
      try {
        await call("PUT", "profile", { displayName: fields.displayName.value.trim() || null, preferredAddress: fields.preferredAddress.value.trim() || null, headline: fields.headline.value.trim(),
          bio: fields.bio.value.trim(), location: fields.location.value.trim() || null, website: fields.website.value.trim(), avatarColor: values.color,
          ...(values.cover !== undefined ? { coverImage: values.cover } : {}), ...(handle ? { username: handle } : {}) });
        close();
        toast("Profil kaydedildi.");
        done();
        if (state.tab === "mine") openMine();
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      } finally { save.disabled = false; }
    };
    root.append(card);
    document.body.append(root);
    drawSwatches();
    paint();
    fields.displayName.focus();
  }

  /** The free cover library (backend profile/cover-library, Lorem Picsum · Unsplash): wide pictures, more on scroll; a click hands the large one on. */
  function coverLibrary(choose) {
    const root = R.el("div", "modal");
    const card = R.el("div", "modal-card cover-library");
    const grid = R.el("div", "cover-grid");
    const status = R.el("p", "muted small", "Yükleniyor…");
    const close = R.button("Kapat", "secondary", () => root.remove());
    const actions = R.el("div", "modal-actions");
    actions.append(R.el("span", "muted small", "Fotoğraflar: Lorem Picsum · Unsplash"), R.el("span", "spacer"), close);
    card.append(R.el("h2", "", "Kapak kütüphanesi"), grid, status, actions);
    root.append(card);
    document.body.append(root);
    let page = 0;
    let loading = false;
    const more = async () => {
      if (loading || page >= 30) return;
      loading = true;
      try {
        const answer = await call("GET", `profile/cover-library?page=${page + 1}`);
        page = answer.page;
        for (const pick of answer.picks) {
          const tile = R.button("", "cover-tile", () => { root.remove(); choose(pick.image); });
          tile.type = "button";
          tile.title = pick.author;
          tile.style.backgroundImage = `url("${pick.thumb}")`;
          grid.append(tile);
        }
        status.textContent = "";
      } catch (failure) {
        status.textContent = failure.message;
      } finally { loading = false; }
    };
    grid.addEventListener("scroll", () => { if (grid.scrollTop + grid.clientHeight > grid.scrollHeight - 300) more(); });
    more();
  }

  /** Cropping like X: the picture under a fixed 3:1 frame, dragged to place and zoomed with the wheel or the slider. Output 1500×500 JPEG (base64). */
  function cropCover(source, done) {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onerror = () => toast("Görsel açılamadı.");
    image.onload = () => {
      const root = R.el("div", "modal");
      const card = R.el("div", "modal-card cover-cropper");
      const canvas = R.el("canvas", "cover-crop");
      canvas.width = 900;
      canvas.height = 300;
      const zoomInput = R.el("input");
      zoomInput.type = "range";
      zoomInput.min = "1";
      zoomInput.max = "5";
      zoomInput.step = "0.01";
      zoomInput.value = "1";
      const view = { zoom: 1, x: 0, y: 0 };
      const base = Math.max(canvas.width / image.width, canvas.height / image.height);
      const size = () => ({ w: image.width * base * view.zoom, h: image.height * base * view.zoom });
      const clamp = () => {
        const { w, h } = size();
        const mx = Math.max((w - canvas.width) / 2, 0);
        const my = Math.max((h - canvas.height) / 2, 0);
        view.x = Math.min(Math.max(view.x, -mx), mx);
        view.y = Math.min(Math.max(view.y, -my), my);
      };
      const draw = (target, scale) => {
        const ctx = target.getContext("2d");
        const { w, h } = size();
        ctx.fillStyle = "#000";
        ctx.fillRect(0, 0, target.width, target.height);
        ctx.drawImage(image, (canvas.width / 2 + view.x - w / 2) * scale, (canvas.height / 2 + view.y - h / 2) * scale, w * scale, h * scale);
      };
      const redraw = () => { clamp(); draw(canvas, 1); };
      let dragging = null;
      canvas.addEventListener("pointerdown", (event) => { dragging = { x: event.clientX, y: event.clientY, from: { x: view.x, y: view.y } }; canvas.setPointerCapture(event.pointerId); });
      canvas.addEventListener("pointermove", (event) => {
        if (!dragging) return;
        const ratio = canvas.width / canvas.getBoundingClientRect().width;
        view.x = dragging.from.x + (event.clientX - dragging.x) * ratio;
        view.y = dragging.from.y + (event.clientY - dragging.y) * ratio;
        redraw();
      });
      canvas.addEventListener("pointerup", () => { dragging = null; });
      canvas.addEventListener("wheel", (event) => { event.preventDefault(); view.zoom = Math.min(Math.max(view.zoom * (event.deltaY < 0 ? 1.06 : 0.94), 1), 5); zoomInput.value = String(view.zoom); redraw(); }, { passive: false });
      zoomInput.addEventListener("input", () => { view.zoom = Number(zoomInput.value); redraw(); });
      const apply = R.button("Uygula", "primary", () => {
        const output = R.el("canvas");
        output.width = 1500;
        output.height = 500;
        draw(output, output.width / canvas.width);
        try {
          done(output.toDataURL("image/jpeg", 0.85).split(",")[1]);
          root.remove();
        } catch {
          toast("Bu görsel kırpılamadı; başka birini dene.");
        }
      });
      const actions = R.el("div", "modal-actions");
      actions.append(zoomInput, R.el("span", "spacer"), R.button("Vazgeç", "secondary", () => root.remove()), apply);
      card.append(R.el("h2", "", "Kapağı kırp"), canvas, actions);
      root.append(card);
      document.body.append(root);
      redraw();
    };
    image.src = source;
  }

  /** Keşfet has no editor of its own: the profile is edited in Settings. */
  function editInSettings() {
    window.showSection("settings");
    editProfile(() => window.WiqoReloadProfile?.());
  }

  async function openMine() {
    const request = showTab("mine");
    showDiscover();
    const host = $("discover-mine");
    const current = beginPanel(host);
    host.replaceChildren(R.el("p", "muted small", "Yükleniyor…"));
    let answer;
    try {
      answer = await call("GET", "missions/me");
    } catch (error) {
      if (current() && request === viewRequest) host.replaceChildren(R.el("p", "error small", error.message));
      return;
    }
    if (!current() || request !== viewRequest) return;
    state.me = answer.profile;
    state.categories = answer.categories;
    state.needs = answer.needs;
    state.covers = answer.covers || state.covers;
    if (answer.profile.username) { await renderProfile(host, answer.profile.username); return; }
    const card = R.el("div", "card profile-form");
    card.append(R.el("h3", "", "Keşfet profilin"), R.el("p", "muted small", "Keşfet'te paylaşmak ve yorum yazmak için bir kullanıcı adı gerekir."),
      R.button("Profili düzenle", "primary small", editInSettings));
    host.replaceChildren(card);
  }

  const NOTICE_FILTERS = [["all", "Tümü"], ["unread", "Okunmamış"], ["activity", "Etkileşimler"], ["people", "Kişiler"], ["problems", "Problemler"]];
  const NOTICE_SYMBOLS = { like: "heart", comment: "bubble.left", reply: "bubble.left", post_reply: "bubble.left", follow: "person.crop.circle", copy: "doc.on.doc",
    remix: "arrow.clockwise", published: "sparkles", succeeded: "checkmark.circle.fill", need: "person.2", idea: "sparkle", solution: "wand.and.stars", verdict: "checkmark" };
  const inbox = { filter: "all", notices: [], next: null };

  /**
   * Gelen kutusu: what waits for the person's yes on this computer first (answered right here), then what people did on Keşfet, newest first in time
   * sections, many people doing the same on one day as one row. Filters narrow it; each row reads, unreads or deletes itself; the head clears read or
   * everything.
   */
  async function openNotices() {
    const request = showTab("notices");
    showDiscover();
    const host = $("discover-notices");
    const group = ["activity", "people", "problems"].includes(inbox.filter) ? inbox.filter : "";
    try {
      const answer = await call("GET", `missions/notifications${group ? `?group=${group}` : ""}`);
      const waiting = await invoke("pending_approvals").catch(() => []);
      if (request !== viewRequest) return;
      inbox.notices = answer.notices;
      inbox.next = answer.next ?? null;
      drawNotices(host, waiting);
      if (answer.unread && !group) await call("POST", "missions/notifications/read");
      refreshUnread();
    } catch (error) {
      if (request === viewRequest) host.replaceChildren(R.el("p", "error small", error.message));
    }
  }

  function drawNotices(host, waiting = []) {
    const ids = (notice) => notice.ids || [notice.id];
    const redraw = () => drawNotices(host, waiting);
    const head = R.el("div", "notice-head");
    const filters = R.el("div", "chips");
    const unread = inbox.notices.filter((notice) => !notice.read).length;
    filters.append(...NOTICE_FILTERS.map(([id, text]) => R.button(id === "unread" && unread ? `${text} ${unread}` : text, `chip ${inbox.filter === id ? "on" : ""}`, () => {
      const reload = id !== "unread" && ["activity", "people", "problems", "all"].includes(id) && id !== inbox.filter;
      inbox.filter = id;
      if (reload || (id === "all")) openNotices(); else redraw();
    })));
    const tools = R.el("div", "mission-actions");
    tools.append(
      R.button("Tümünü okundu say", "link small", async () => {
        const list = inbox.notices.filter((notice) => !notice.read).flatMap(ids);
        if (!list.length) return;
        inbox.notices.forEach((notice) => { notice.read = true; });
        redraw();
        try { await call("POST", "missions/notifications/read", { ids: list }); } catch (error) { showError(error.message); }
      }),
      R.button("Okunanları temizle", "link small", async () => {
        try { await call("POST", "missions/notifications/delete", { which: "read" }); inbox.notices = inbox.notices.filter((notice) => !notice.read); redraw(); } catch (error) { showError(error.message); }
      }),
      R.button("Tümünü temizle", "link small danger-text", async () => {
        if (!window.confirm("Keşfet bildirimlerinin hepsi silinsin mi? İşlerin ve paylaşımların olduğu gibi kalır.")) return;
        try { await call("POST", "missions/notifications/delete", { which: "all" }); inbox.notices = []; redraw(); } catch (error) { showError(error.message); }
      }));
    head.append(filters, R.el("span", "spacer"), tools);
    const parts = [head];

    if (waiting.length && inbox.filter === "all") {
      const list = R.el("ul", "list");
      for (const question of waiting) {
        const row = R.el("li", "unread");
        const yes = R.button("Onayla", "primary small", async () => { await invoke("answer_approval", { id: question.id, allow: true }); row.remove(); });
        const no = R.button("Reddet", "secondary small", async () => { await invoke("answer_approval", { id: question.id, allow: false }); row.remove(); });
        row.append(R.el("span", "recipe-main notice", question.detail || question.kind), yes, no);
        list.append(row);
      }
      const box = R.el("div", "card");
      box.append(list);
      parts.push(R.el("h3", "settings-head", "Seni bekleyenler"), box);
    }

    const visible = inbox.notices.filter((notice) => inbox.filter !== "unread" || !notice.read);
    const now = Date.now();
    const today = new Date().toDateString();
    const sectionOf = (notice) => {
      if (!notice.read && inbox.filter !== "unread") return "Yeni";
      const at = new Date(`${notice.createdAt.replace(" ", "T")}Z`);
      if (at.toDateString() === today) return "Bugün";
      return now - at.getTime() < 7 * 86_400_000 ? "Bu hafta" : "Daha önce";
    };
    for (const title of ["Yeni", "Bugün", "Bu hafta", "Daha önce"]) {
      const items = visible.filter((notice) => sectionOf(notice) === title);
      if (!items.length) continue;
      const list = R.el("ul", "list notice-list");
      for (const notice of items) {
        const row = R.el("li", notice.read ? "" : "unread");
        const face = R.el("button", "notice-face");
        face.type = "button";
        face.append(notice.actor.username ? avatar({ username: notice.actor.username, displayName: notice.actor.displayName, avatar: notice.actor.avatar }, "big") : R.el("span", "avatar big", "?"));
        const badge = R.el("span", "notice-badge");
        badge.append(icon(NOTICE_SYMBOLS[notice.kind] || "bell", 10));
        face.append(badge);
        face.onclick = () => { if (notice.actor.username) openProfile(notice.actor.username); };
        const main = R.el("button", "recipe-main notice notice-main");
        main.type = "button";
        const words = R.el("div");
        words.append(R.el("div", "", notice.text));
        if (notice.preview) words.append(R.el("div", "muted small", `“${notice.preview}”`));
        words.append(R.el("div", "muted small", new Date(`${notice.createdAt.replace(" ", "T")}Z`).toLocaleString("tr-TR", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })));
        main.append(words);
        main.onclick = async () => {
          if (!notice.read) { notice.read = true; row.className = ""; call("POST", "missions/notifications/read", { ids: ids(notice) }).catch(() => undefined); }
          if (notice.mission) openMission(notice.mission.code);
          else if (notice.post) openProblem(notice.post.code);
          else if (notice.actor.username) openProfile(notice.actor.username);
        };
        const actions = R.el("span", "notice-actions");
        if (notice.kind === "follow" && !notice.others && notice.actor.username) {
          actions.append(R.button(notice.actor.following ? "Takipte" : "Takip et", notice.actor.following ? "secondary small" : "primary small", async () => {
            const on = !notice.actor.following;
            try { await call(on ? "POST" : "DELETE", `missions/users/${encodeURIComponent(notice.actor.username)}/follow`); notice.actor.following = on; redraw(); } catch (error) { showError(error.message); }
          }));
        }
        const toggle = R.button("", "icon-button", async () => {
          notice.read = !notice.read;
          redraw();
          try { await call("POST", notice.read ? "missions/notifications/read" : "missions/notifications/unread", { ids: ids(notice) }); } catch (error) { showError(error.message); }
        });
        toggle.title = notice.read ? "Okunmadı yap" : "Okundu say";
        toggle.append(icon(notice.read ? "circle" : "checkmark", 14));
        const remove = R.button("", "icon-button", async () => {
          inbox.notices = inbox.notices.filter((item) => item.id !== notice.id);
          redraw();
          try { await call("POST", "missions/notifications/delete", { ids: ids(notice) }); } catch (error) { showError(error.message); }
        });
        remove.title = "Sil";
        remove.append(icon("trash", 14));
        actions.append(toggle, remove);
        row.append(face, main, actions);
        list.append(row);
      }
      const card = R.el("div", "card");
      card.append(list);
      parts.push(R.el("h3", "settings-head", title), card);
    }
    if (inbox.next && inbox.filter !== "unread") {
      const more = R.button("Daha eskiler", "secondary small", async () => {
        more.disabled = true;
        const request = viewRequest;
        const filter = inbox.filter;
        const query = new URLSearchParams({ before: inbox.next });
        if (["activity", "people", "problems"].includes(filter)) query.set("group", filter);
        try {
          const answer = await call("GET", `missions/notifications?${query}`);
          if (request !== viewRequest || filter !== inbox.filter) return;
          const known = new Set(inbox.notices.map((notice) => notice.id));
          inbox.notices.push(...answer.notices.filter((notice) => !known.has(notice.id)));
          inbox.next = answer.next ?? null;
          redraw();
        } catch (error) { if (request === viewRequest && filter === inbox.filter) showError(error.message); }
        finally { more.disabled = false; }
      });
      parts.push(more);
    }
    if (parts.length === 1) {
      parts.push(R.el("p", "muted small", { unread: "Okunmamış bir şey yok.", activity: "İşlerine gelen beğeni, yorum ve değerlendirmeler burada görünür.",
        people: "Seni takip edenler ve takip ettiklerinin paylaşımları burada görünür.", problems: "Problemlerine gelen destek, fikir ve çözümler burada görünür." }[inbox.filter] || "Gelen kutun boş."));
    }
    host.replaceChildren(...parts);
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
  /**
   * "Wiqo'da paylaş": a post is made from work Wiqo finished, never from a blank page. The last finished task of the recent chats and the saved
   * recipes are listed; choosing one opens the dialog filled from that work.
   */
  async function shareFromWork() {
    const root = R.el("div", "modal");
    const card = R.el("div", "modal-card share-work");
    const list = R.el("div", "share-work-list");
    const status = R.el("p", "muted small", "Yükleniyor…");
    const actions = R.el("div", "modal-actions");
    actions.append(R.el("span", "spacer"), R.button("Kapat", "secondary", () => root.remove()));
    card.append(R.el("h2", "", "Wiqo'da paylaş"), status, list, actions);
    root.append(card);
    document.body.append(root);
    const snippet = (text) => (String(text || "").split("\n").map((line) => line.replace(/^[#*>\-`| ]+/, "").trim()).find((line) => line.length > 3) || "").slice(0, 120);
    const row = (title, line, onPick) => {
      const button = R.button("", "share-work-row", () => { root.remove(); onPick(); });
      button.type = "button";
      button.append(R.el("div", "w-title", title), R.el("div", "muted small", line));
      return button;
    };
    const [tasks, recipes] = await Promise.all([finishedTasks().catch(() => []), invoke("skills_list").then((value) => value.skills || value.recipes || []).catch(() => [])]);
    if (tasks.length) list.append(R.el("h3", "", "Tamamlanan görevler"), ...tasks.map((task) => row(task.title, snippet(task.result), () => publish({ mode: "task", request: task.request, result: task.result }))));
    if (recipes.length) list.append(R.el("h3", "", "Kayıtlı işlerin"), ...recipes.slice(0, 10).map((recipe) => row(recipe.name, snippet(recipe.prompt), () => publish({ mode: "recipe", recipe }))));
    status.textContent = tasks.length || recipes.length ? "" : "Paylaşılacak bir iş yok. Wiqo'ya bir iş yaptır; bitince buradan paylaşırsın.";
  }

  /** The last finished ask of each recent chat: a request and an answer of some substance. */
  async function finishedTasks() {
    const conversations = ((await invoke("chat_conversations")).conversations || []).slice(0, 10);
    const found = await Promise.all(conversations.map(async (conversation) => {
      try {
        const messages = ((await invoke("chat_messages", { conversation: conversation.id })).messages || []).filter((item) => item.role === "user" || item.role === "assistant");
        let answer = -1;
        for (let index = messages.length - 1; index >= 0; index -= 1) if (messages[index].role === "assistant" && String(messages[index].content || "").trim().length >= 80) { answer = index; break; }
        if (answer < 0) return null;
        let ask = -1;
        for (let index = answer - 1; index >= 0; index -= 1) if (messages[index].role === "user") { ask = index; break; }
        const request = ask >= 0 ? String(messages[ask].content || "").trim() : "";
        if (request.length < 8) return null;
        return { title: conversation.title || request.slice(0, 60), request, result: messages[answer].content };
      } catch { return null; }
    }));
    return found.filter(Boolean);
  }

  async function publish(source) {
    const dialog = $("mission-dialog");
    const field = (name) => dialog.querySelector(`[name="${name}"]`);
    const findingsBox = $("mission-findings");
    findingsBox.replaceChildren();
    findingsBox.hidden = true;
    $("mission-error").hidden = true;
    $("mission-dialog-title").textContent = { task: "Wiqo'da paylaş", recipe: "İşi paylaş", edit: "Paylaşımı düzenle", remix: "Kendi versiyonunu paylaş" }[source.mode];
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
    // A new post is shared to Keşfet by default; a draft from a chat (task) is filled as private below.
    fill({ visibility: "public" });
    renderImages(source.mode === "edit" ? source.mission : null);
    if (source.mode === "edit") fill(source.mission);
    if (source.mode === "recipe") fill({ ...source.recipe, visibility: source.recipe.visibility === "private" ? "link" : source.recipe.visibility });
    if (source.mode === "remix") fill({ ...source.mission, name: `${source.mission.name} (remix)`, preview: "", visibility: "public" });
    dialog.hidden = false;

    // The server's proof of the finished work (backend social/workProof.ts), sent with the save so the post may go out.
    let workToken = null;
    if (source.mode === "task") {
      const save = $("mission-save");
      save.disabled = true;
      save.textContent = "Taslak yazılıyor…";
      try {
        const answer = await call("POST", "missions/draft", { request: source.request, result: source.result });
        fill({ ...answer.draft, description: answer.draft.description || `Wiqo ile hazırladığım ${answer.draft.name}. Sen de kendi versiyonunu oluşturabilirsin.`, visibility: "public" });
        workToken = answer.workToken || null;
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
        needs: [...$("mission-needs").querySelectorAll("input:checked")].map((box) => box.value), visibility, ...(workToken ? { workToken } : {})
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
        if (mission.url) await copyShareLink(mission.url, mission.visibility === "public" ? "Keşfet'te yayında. " : "Kaydedildi. ");
        else toast("Kayıtlı işlerine kaydedildi.");
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
      displayImage(picture, image.url);
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

  async function openPendingDiscoverLink() {
    await window.WiqoReady?.catch(() => {});
    const link = await invoke("take_discover_link").catch(() => null);
    if (link?.kind === "mission") openMission(link.code);
    else if (link?.kind === "problem") openProblem(link.code);
  }

  const TAB_OPENERS = { feed: () => load(), search, notices: openNotices, mine: openMine };
  for (const button of document.querySelectorAll(".rail-item")) button.onclick = () => TAB_OPENERS[button.dataset.tab]();
  $("discover-new").onclick = shareFromWork;
  $("discover-side-close").onclick = closeSide;
  $("discover-more").onclick = () => load(true);
  $("discover-search-input").addEventListener("input", (event) => {
    clearTimeout(searchTimer);
    state.q = event.target.value.trim();
    searchTimer = setTimeout(search, 300);
  });
  window.addEventListener("wiqo-section", (event) => { if (event.detail === "discover" && state.tab === "feed") load(); });
  window.addEventListener("wiqo-signed-in", () => { refreshUnread(); openPendingDiscoverLink(); });
  window.__TAURI__.event.listen("discover-link", openPendingDiscoverLink).then(openPendingDiscoverLink).catch(() => {});

  window.WiqoDiscover = {
    editProfile,
    open: openMission,
    openProblem,
    publishTask: (request, result) => publish({ mode: "task", request, result }),
    publishRecipe: (recipe) => publish({ mode: "recipe", recipe }),
  };
})();
