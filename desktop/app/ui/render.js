// How a reply looks: its markdown (sections, tables, code, callouts and the `wiqo:` component blocks of DESIGN.md §7) and every card the server
// sends (the backend's contracts/copilotWidgets.ts, the same set the iPhone draws). Everything untrusted goes in as text; the only HTML this
// file writes is the escaped markdown below and the sandboxed frame of an inline app.
(() => {
  const { invoke } = window.__TAURI__.core;
  const icon = (...args) => window.WiqoIcons.icon(...args);

  // ----- small builders -----

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }

  function button(label, kind, onclick) {
    const node = el("button", kind || "secondary", label);
    node.type = "button";
    node.onclick = onclick;
    return node;
  }

  function pill(text, tone) {
    return el("span", `pill-status ${tone || "neutral"}`, text);
  }

  function card(title, subtitle, icon) {
    const box = el("div", "w-card");
    if (title || subtitle) {
      const head = el("div", "w-head");
      if (icon) {
        const box = el("span", "w-icon");
        box.append(window.WiqoIcons.icon(icon, 15));
        head.append(box);
      }
      const text = el("div", "w-titles");
      if (title) text.append(el("div", "w-title", title));
      if (subtitle) text.append(el("div", "w-sub", subtitle));
      head.append(text);
      box.append(head);
    }
    return box;
  }

  function openLink(url) {
    invoke("open_link", { url }).catch(() => {});
  }

  function domainOf(url) {
    try {
      return new URL(url).hostname.replace(/^www\./, "");
    } catch {
      return url;
    }
  }

  function toneOf(status) {
    const value = String(status || "").toLowerCase();
    if (/(complete|done|success|ok|active|aktif|tamam|verified|on)/.test(value)) return "success";
    if (/(fail|error|denied|cancel|hata|iptal)/.test(value)) return "danger";
    if (/(run|pending|wait|progress|bekle|çalış)/.test(value)) return "warning";
    return "neutral";
  }

  const STATUS_LABELS = {
    completed: "Tamamlandı", running: "Çalışıyor", pending: "Bekliyor", failed: "Başarısız", denied: "Reddedildi", skipped: "Atlandı",
    cancelled: "Durduruldu", waiting: "Bekliyor", active: "Aktif", paused: "Duraklatıldı", unknown: "Belirsiz",
    verified: "Tamamlandı", waiting_device: "PC bekleniyor", awaiting_confirmation: "Onay bekliyor",
  };

  function statusLabel(status) {
    return STATUS_LABELS[String(status || "").toLowerCase()] || status || "";
  }

  function date(text, options) {
    const value = new Date(text);
    return isNaN(value) ? String(text || "") : value.toLocaleString("tr-TR", options || { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  }

  function copyButton(text) {
    const node = button("", "icon-button", async () => {
      try {
        await navigator.clipboard.writeText(text);
        node.replaceChildren(icon("checkmark", 13));
        setTimeout(() => node.replaceChildren(icon("doc.on.doc", 13)), 1400);
      } catch { /* clipboard off */ }
    });
    node.title = "Kopyala";
    node.append(icon("doc.on.doc", 13));
    return node;
  }

  // ----- markdown -----

  function escape(text) {
    return String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  }

  // Inline: code, bold, italic, links (opened in the browser), bare addresses. Input is escaped first.
  function inline(text) {
    const codes = [];
    let html = escape(text).replace(/`([^`]+)`/g, (_, code) => {
      codes.push(code);
      return `\u0000${codes.length - 1}\u0000`;
    });
    html = html
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
      .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a data-url="$2">$1</a>')
      .replace(/(^|[\s(])(https?:\/\/[^\s<)]+)/g, '$1<a data-url="$2">$2</a>');
    return html.replace(/\u0000(\d+)\u0000/g, (_, index) => `<code>${codes[Number(index)]}</code>`);
  }

  function wire(node) {
    for (const link of node.querySelectorAll("a[data-url]")) {
      link.href = "#";
      link.onclick = (event) => {
        event.preventDefault();
        openLink(link.dataset.url);
      };
    }
    return node;
  }

  function codeCard(code, language) {
    const box = el("div", "code-card");
    const head = el("div", "code-head");
    head.append(el("span", "overline", language || "kod"), copyButton(code));
    const pre = el("pre");
    pre.append(el("code", "", code));
    box.append(head, pre);
    return box;
  }

  function tableCard(header, rows, total) {
    const box = el("div", "w-card table-card");
    const scroll = el("div", "table-scroll");
    const table = el("table");
    const head = el("tr");
    for (const cell of header) head.append(el("th", "", cell));
    table.append(head);
    for (const row of rows) {
      const line = el("tr");
      header.forEach((_, index) => line.append(el("td", "", row[index] ?? "")));
      table.append(line);
    }
    scroll.append(table);
    box.append(scroll);
    if (total && total > rows.length) box.append(el("div", "w-foot", `${rows.length} / ${total} satır`));
    return box;
  }

  function callout(text, tone) {
    const box = el("div", `callout ${tone || "info"}`);
    const mark = el("span", "callout-icon");
    mark.append(icon(tone === "warning" ? "exclamationmark.triangle" : tone === "assumption" ? "questionmark.circle" : "info.circle", 15));
    box.append(mark);
    const body = el("div", "callout-body");
    body.innerHTML = inline(text);
    box.append(wire(body));
    return box;
  }

  // `wiqo:<kind>` blocks (DESIGN.md §7); an unreadable block shows as JSON code.
  function component(kind, json) {
    let data;
    try {
      data = JSON.parse(json);
    } catch {
      return codeCard(json, `wiqo:${kind}`);
    }
    switch (kind) {
      case "metrics": {
        const box = el("div", "w-card metrics");
        if (data.title) box.append(el("div", "w-title", data.title));
        const row = el("div", "metric-row");
        for (const item of (data.items || []).slice(0, 3)) {
          const metric = el("div", "metric");
          metric.append(el("div", "overline", item.label), el("div", "metric-value", item.value));
          if (item.hint) metric.append(pill(item.hint, item.tone || "neutral"));
          row.append(metric);
        }
        box.append(row);
        return box;
      }
      case "compare": {
        const row = el("div", "compare");
        for (const plan of (data.plans || []).slice(0, 3)) {
          const box = el("div", `w-card plan${plan.recommended ? " recommended" : ""}`);
          if (plan.recommended) box.append(el("span", "plan-flag", "ÖNERİLEN"));
          box.append(el("div", "w-sub", plan.name));
          if (plan.detail) box.append(el("div", "w-sub small", plan.detail));
          const price = el("div", "plan-price", plan.price);
          if (plan.unit) price.append(el("span", "w-sub", ` ${plan.unit}`));
          box.append(price);
          const list = el("ul", "checks");
          for (const point of (plan.points || []).slice(0, 6)) list.append(el("li", "", point));
          box.append(list);
          row.append(box);
        }
        return row;
      }
      case "calendar": return calendar(data.month, data.selected, data.marked || []);
      case "list": return listCard(data.title, (data.rows || []).slice(0, 12).map((row) => ({ title: row.title, subtitle: row.subtitle, badge: row.badge, tone: row.tone })));
      case "callout": return callout(data.text || "", data.tone);
      case "images": {
        const row = el("div", "image-row");
        for (const item of (data.items || []).slice(0, 6)) {
          if (!String(item.src || "").startsWith("https://")) continue;
          const tile = el("button", "image-card");
          tile.type = "button";
          tile.onclick = () => openLink(item.src);
          const image = el("img");
          image.src = item.src;
          image.alt = item.title || "";
          image.loading = "lazy";
          tile.append(image);
          const caption = el("div", "image-caption");
          if (item.label) caption.append(el("div", "overline", item.label));
          caption.append(el("div", "", item.title || ""));
          tile.append(caption);
          row.append(tile);
        }
        return row;
      }
      default: return codeCard(json, `wiqo:${kind}`);
    }
  }

  function calendar(month, selected, marked) {
    const [year, monthIndex] = String(month || "").split("-").map(Number);
    const first = new Date(year || new Date().getFullYear(), (monthIndex || new Date().getMonth() + 1) - 1, 1);
    const box = el("div", "w-card calendar");
    box.append(el("div", "w-title", first.toLocaleDateString("tr-TR", { month: "long", year: "numeric" })));
    const grid = el("div", "cal-grid");
    for (const name of ["Pt", "Sa", "Ça", "Pe", "Cu", "Ct", "Pa"]) grid.append(el("div", "cal-name", name));
    const offset = (first.getDay() + 6) % 7;
    for (let index = 0; index < offset; index++) grid.append(el("div", "cal-day out"));
    const days = new Date(first.getFullYear(), first.getMonth() + 1, 0).getDate();
    const key = (day) => `${first.getFullYear()}-${String(first.getMonth() + 1).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
    for (let day = 1; day <= days; day++) {
      const cell = el("div", "cal-day", day);
      if (key(day) === selected) cell.classList.add("on");
      if (marked.includes(key(day))) cell.classList.add("marked");
      grid.append(cell);
    }
    box.append(grid);
    return box;
  }

  function listCard(title, rows, options = {}) {
    const box = card(title, options.subtitle, options.icon);
    box.classList.add("list-card");
    const list = el("div", "rows");
    const limit = options.limit || 5;
    rows.forEach((row, index) => {
      const line = el(row.url ? "button" : "div", "row");
      if (row.url) {
        line.type = "button";
        line.onclick = () => openLink(row.url);
      }
      if (index >= limit) line.hidden = true;
      const text = el("div", "row-text");
      text.append(el("div", "row-title", row.title));
      if (row.subtitle) text.append(el("div", "row-sub", row.subtitle));
      line.append(text);
      if (row.meta) line.append(el("span", "row-meta", row.meta));
      if (row.badge) line.append(pill(row.badge, row.tone || toneOf(row.badge)));
      list.append(line);
    });
    box.append(list);
    if (rows.length > limit) {
      box.append(button(`Daha fazla (${rows.length - limit})`, "more", (event) => {
        for (const line of list.children) line.hidden = false;
        event.target.remove();
      }));
    }
    return box;
  }

  // Markdown → blocks. Three or more `##` sections become accordion cards (the first open unless `streaming`).
  function markdown(text, streaming) {
    const root = el("div", "md");
    const lines = String(text || "").replace(/\r/g, "").split("\n");
    let sections = [];
    let current = { title: null, nodes: [] };
    const flushSection = () => {
      if (current.title !== null || current.nodes.length) sections.push(current);
    };
    let paragraph = [];
    const flushParagraph = () => {
      if (!paragraph.length) return;
      const p = el("p");
      p.innerHTML = paragraph.map(inline).join("<br>");
      current.nodes.push(wire(p));
      paragraph = [];
    };
    for (let index = 0; index < lines.length; index++) {
      const line = lines[index];
      const fence = line.match(/^\s*```\s*([\w:-]*)\s*$/);
      if (fence) {
        flushParagraph();
        const body = [];
        let closed = false;
        for (index++; index < lines.length; index++) {
          if (/^\s*```\s*$/.test(lines[index])) {
            closed = true;
            break;
          }
          body.push(lines[index]);
        }
        const language = fence[1];
        if (language.startsWith("wiqo:")) {
          // A component block shows only once it is complete.
          if (closed) current.nodes.push(component(language.slice(5), body.join("\n")));
        } else {
          current.nodes.push(codeCard(body.join("\n"), language));
        }
        continue;
      }
      const heading = line.match(/^(#{1,4})\s+(.*)$/);
      if (heading) {
        flushParagraph();
        if (heading[1].length <= 2) {
          flushSection();
          current = { title: heading[2], nodes: [] };
        } else {
          const h = el("h4");
          h.innerHTML = inline(heading[2]);
          current.nodes.push(wire(h));
        }
        continue;
      }
      if (/^\s*\|.*\|\s*$/.test(line) && index + 1 < lines.length && /^\s*\|?\s*:?-{2,}/.test(lines[index + 1])) {
        flushParagraph();
        const cells = (row) => row.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => cell.trim());
        const header = cells(line);
        const rows = [];
        for (index += 2; index < lines.length && /^\s*\|.*\|\s*$/.test(lines[index]); index++) rows.push(cells(lines[index]));
        index--;
        current.nodes.push(tableCard(header, rows));
        continue;
      }
      if (/^\s*>\s?/.test(line)) {
        flushParagraph();
        const quote = [];
        for (; index < lines.length && /^\s*>\s?/.test(lines[index]); index++) quote.push(lines[index].replace(/^\s*>\s?/, ""));
        index--;
        const body = quote.join(" ");
        const tone = /^(uyarı|dikkat)\s*:/i.test(body) ? "warning" : /^varsayım\s*:/i.test(body) ? "assumption" : "info";
        current.nodes.push(callout(body, tone));
        continue;
      }
      const item = line.match(/^(\s*)([-*•]|\d+[.)])\s+(.*)$/);
      if (item) {
        flushParagraph();
        const ordered = /\d/.test(item[2]);
        const list = el(ordered ? "ol" : "ul");
        for (; index < lines.length; index++) {
          const next = lines[index].match(/^(\s*)([-*•]|\d+[.)])\s+(.*)$/);
          if (!next) {
            if (/^\s{2,}\S/.test(lines[index]) && list.lastChild) {
              list.lastChild.innerHTML += `<br>${inline(lines[index].trim())}`;
              continue;
            }
            break;
          }
          const li = el("li");
          if (next[1].length >= 2) li.className = "nested";
          const checkbox = next[3].match(/^\[( |x)\]\s+(.*)$/i);
          li.innerHTML = checkbox ? `${checkbox[1].trim() ? "☑" : "☐"} ${inline(checkbox[2])}` : inline(next[3]);
          list.append(li);
        }
        index--;
        current.nodes.push(wire(list));
        continue;
      }
      if (/^\s*(---|\*\*\*)\s*$/.test(line)) {
        flushParagraph();
        current.nodes.push(el("hr"));
        continue;
      }
      if (!line.trim()) {
        flushParagraph();
        continue;
      }
      paragraph.push(line);
    }
    flushParagraph();
    flushSection();
    const titled = sections.filter((section) => section.title !== null);
    if (titled.length >= 3) {
      sections.forEach((section, position) => {
        if (section.title === null) {
          root.append(...section.nodes);
          return;
        }
        const details = el("details", "accordion");
        if (streaming || position === sections.indexOf(titled[0])) details.open = true;
        const summary = el("summary");
        summary.innerHTML = inline(section.title);
        details.append(wire(summary));
        const body = el("div", "accordion-body");
        body.append(...section.nodes);
        details.append(body);
        root.append(details);
      });
    } else {
      for (const section of sections) {
        if (section.title !== null) {
          const h = el("h3");
          h.innerHTML = inline(section.title);
          root.append(wire(h));
        }
        root.append(...section.nodes);
      }
    }
    return root;
  }

  // ----- cards -----

  // The same symbols the iPhone's weather card uses (CopilotResultWidgetView).
  const WEATHER_SYMBOL = (code) => {
    if (code === 0) return "sun.max.fill";
    if ([1, 2].includes(code)) return "cloud.sun.fill";
    if (code === 3) return "cloud.fill";
    if ([45, 48].includes(code)) return "cloud.fog.fill";
    if (code >= 51 && code <= 57) return "cloud.drizzle.fill";
    if ((code >= 61 && code <= 67) || (code >= 80 && code <= 82)) return "cloud.rain.fill";
    if ((code >= 71 && code <= 77) || (code >= 85 && code <= 86)) return "cloud.snow.fill";
    if (code >= 95) return "cloud.bolt.rain.fill";
    return "cloud.fill";
  };
  const WEATHER = (code, size = 18) => icon(WEATHER_SYMBOL(code), size);
  const deg = (value) => (value === null || value === undefined ? "–" : `${Math.round(value)}°`);

  function weather(widget) {
    const box = card(widget.location || "Hava durumu", widget.current?.condition);
    box.classList.add("weather");
    if (widget.current) {
      const now = el("div", "weather-now");
      const big = el("span", "weather-icon");
      big.append(WEATHER(widget.current.weatherCode, 30));
      now.append(big, el("span", "weather-temp", deg(widget.current.temperatureC)));
      const facts = el("div", "w-sub");
      const parts = [];
      if (widget.current.feelsLikeC !== null && widget.current.feelsLikeC !== undefined) parts.push(`Hissedilen ${deg(widget.current.feelsLikeC)}`);
      if (widget.current.humidityPercent !== null && widget.current.humidityPercent !== undefined) parts.push(`Nem %${Math.round(widget.current.humidityPercent)}`);
      if (widget.current.windKmh !== null && widget.current.windKmh !== undefined) parts.push(`Rüzgâr ${Math.round(widget.current.windKmh)} km/s`);
      facts.textContent = parts.join(" · ");
      now.append(facts);
      box.append(now);
    }
    if (widget.hourly?.length) {
      const hours = el("div", "weather-hours");
      for (const hour of widget.hourly.slice(0, 12)) {
        const cell = el("div", "hour");
        const glyph = el("div", "hour-icon");
        glyph.append(WEATHER(hour.weatherCode, 15));
        cell.append(el("div", "w-sub", date(hour.time, { hour: "2-digit", minute: "2-digit" })), glyph, el("div", "", deg(hour.temperatureC)));
        if (hour.rainChancePercent) cell.append(el("div", "rain", `%${Math.round(hour.rainChancePercent)}`));
        hours.append(cell);
      }
      box.append(hours);
    }
    if (widget.days?.length) {
      const days = el("div", "rows");
      for (const day of widget.days.slice(0, 7)) {
        const row = el("div", "row");
        const name = el("div", "row-text");
        name.append(el("div", "row-title", date(day.date, { weekday: "long", day: "numeric", month: "short" })), el("div", "row-sub", day.condition));
        const glyph = el("span", "weather-icon small");
        glyph.append(WEATHER(day.weatherCode, 16));
        row.append(glyph, name, el("span", "row-meta", `${deg(day.minC)} / ${deg(day.maxC)}`));
        days.append(row);
      }
      box.append(days);
    }
    return box;
  }

  function sources(widget) {
    const row = el("div", "sources");
    for (const source of (widget.sources || []).slice(0, 12)) {
      const chip = button("", "source", () => openLink(source.url));
      chip.title = source.title || source.url;
      const dot = el("span", "source-dot");
      dot.append(icon("safari", 12));
      chip.append(dot, el("span", "", domainOf(source.url)));
      row.append(chip);
    }
    return row;
  }

  // CopilotResultWidgetView's artifact symbols.
  const FILE_ICONS = { pdf: "doc.text", document: "doc.text", image: "photo", spreadsheet: "tablecells", table: "tablecells", chart: "chart.bar", code: "chevron.left.forwardslash.chevron.right", diagram: "sparkle" };

  function artifactCard(artifact, ctx) {
    const id = artifact.artifactId || artifact.id;
    const name = artifact.title || artifact.filename || "Dosya";
    const mime = artifact.mimeType || "";
    const box = el("div", "w-card artifact");
    if (mime.startsWith("image/")) {
      const frame = el("div", "artifact-image");
      frame.append(el("div", "spinner"));
      box.append(frame);
      invoke("artifact_preview", { id }).then((file) => {
        const image = el("img");
        image.src = `data:${file.mimeType};base64,${file.base64}`;
        image.alt = name;
        frame.replaceChildren(image);
      }).catch(() => frame.replaceChildren(el("div", "w-sub", "Önizleme alınamadı.")));
    }
    const row = el("div", "artifact-row");
    const kind = artifact.variant || artifact.kind || (mime.includes("pdf") ? "pdf" : mime.startsWith("image/") ? "image" : mime.includes("sheet") || mime.includes("csv") ? "spreadsheet" : "document");
    const badge = el("span", "file-badge");
    badge.append(icon(FILE_ICONS[kind] || "doc.text", 18));
    row.append(badge);
    const text = el("div", "row-text");
    text.append(el("div", "row-title", name));
    const size = artifact.byteCount ? `${(artifact.byteCount / 1024 / 1024 >= 1 ? (artifact.byteCount / 1024 / 1024).toFixed(1) + " MB" : Math.max(1, Math.round(artifact.byteCount / 1024)) + " KB")}` : "";
    if (size || artifact.summary) text.append(el("div", "row-sub", [artifact.summary, size].filter(Boolean).join(" · ")));
    row.append(text);
    const save = async (open) => {
      try {
        const path = await invoke("artifact_save", { id, filename: artifact.filename || name, open });
        ctx.toast?.(open ? "Açıldı." : `İndirilenler'e kaydedildi: ${path.split("/").pop()}`);
      } catch (error) {
        ctx.toast?.(String(error), "bad");
      }
    };
    row.append(button("Aç", "secondary small", () => save(true)), button("İndir", "secondary small", () => save(false)));
    box.append(row);
    if (artifact.tablePreview) box.append(tableCard(artifact.tablePreview.columns, artifact.tablePreview.rows, artifact.tablePreview.totalRows));
    return box;
  }

  function stepList(steps) {
    const list = el("ol", "step-list");
    for (const step of steps) {
      const item = el("li", `step ${toneOf(step.status)}`);
      item.append(el("span", "step-dot"));
      const text = el("div", "row-text");
      text.append(el("div", "row-title", step.title || step.summary || "Adım"));
      const sub = step.message || step.expectedResult;
      if (sub) text.append(el("div", "row-sub", sub));
      item.append(text, pill(statusLabel(step.status), toneOf(step.status)));
      list.append(item);
    }
    return list;
  }

  // The plan card counts its own steps (the server sends the steps, not a total): verified ones are done, the bar follows them.
  function taskPlan(widget) {
    const steps = widget.planSteps || [];
    const done = steps.filter((step) => step.status === "verified").length;
    const failed = steps.some((step) => step.status === "failed");
    const box = card(widget.title || "Görev adımları", [failed ? "Bakman gereken adım var" : done === steps.length && steps.length ? "Tamamlandı" : "Çalışıyor", steps.length ? `${done}/${steps.length}` : ""].filter(Boolean).join(" · "));
    if (steps.length) {
      const bar = el("div", "progress");
      const fill = el("div", "progress-fill");
      fill.style.width = `${Math.round((done / steps.length) * 100)}%`;
      bar.append(fill);
      box.append(bar);
    }
    box.append(stepList(steps));
    return box;
  }

  function approval(widget, ctx) {
    const box = card(widget.title || "Onay gerekiyor", widget.detail || widget.summary, "lock.shield");
    box.classList.add("approval");
    if (widget.expiresAt) box.append(el("div", "w-sub small", `Son: ${date(widget.expiresAt)}`));
    const actions = el("div", "w-actions");
    const answer = async (approved) => {
      actions.replaceChildren(el("span", "w-sub", approved ? "Onaylanıyor…" : "Reddediliyor…"));
      try {
        const result = await invoke("chat_confirm", { confirmation: { id: widget.confirmationId, details: widget.details || {} }, approved });
        actions.replaceChildren(pill(approved ? "Onaylandı" : "Reddedildi", approved ? "success" : "neutral"));
        if (result?.result?.text) box.append(el("div", "w-sub", result.result.text));
        for (const artifact of result?.result?.artifacts || []) box.append(artifactCard(artifact, ctx));
      } catch (error) {
        actions.replaceChildren(el("span", "error small", String(error)));
      }
    };
    actions.append(button("Reddet", "secondary", () => answer(false)), button("Onayla", "primary", () => answer(true)));
    box.append(actions);
    return box;
  }

  function clarification(widget, ctx) {
    const box = card(widget.question || "Bir şey sormam gerekiyor");
    box.classList.add("question");
    const options = el("div", "options");
    for (const option of widget.options || []) {
      options.append(button(option, "option", () => {
        for (const other of options.children) other.disabled = true;
        ctx.answer?.(option, widget.taskId, widget.questionId);
      }));
    }
    box.append(options);
    if (ctx.latest) box.append(el("div", "w-sub small", "Ya da cevabını aşağıya yaz."));
    return box;
  }

  function suggestions(widget, ctx) {
    if (!ctx.latest) return null;
    const row = el("div", "suggestions");
    for (const text of (widget.suggestions || []).slice(0, 4)) row.append(button(text, "suggestion", () => ctx.send?.(text)));
    return row;
  }

  function listWidget(widget) {
    const ICONS = { mail: "envelope", calendar: "calendar", drive: "folder", notion: "doc.text", contacts: "person.crop.circle", files: "folder", tasks: "checklist" };
    const rows = (widget.items || []).map((item) => ({
      title: item.title,
      subtitle: [item.subtitle, item.detail].filter(Boolean).join(" · ") || (item.start ? date(item.start) : ""),
      meta: item.meta,
      badge: item.badge,
      url: item.url,
    }));
    const subtitle = widget.total && widget.total > rows.length ? `${rows.length} / ${widget.total}` : widget.detail;
    return listCard(widget.title || "Liste", rows, { subtitle, icon: ICONS[widget.domain] || "list.bullet" });
  }

  function mapWidget(widget) {
    const title = widget.title || (widget.mode === "route" ? `${widget.origin || "Konumun"} → ${widget.destination}` : widget.query || widget.location || "Harita");
    const box = card(title, widget.detail || (widget.near ? `${widget.near} yakınında` : ""), "map");
    let url = widget.url;
    if (!url) {
      const q = encodeURIComponent;
      url = widget.mode === "route"
        ? `https://maps.apple.com/?saddr=${q(widget.origin || "")}&daddr=${q(widget.destination || "")}&dirflg=${widget.transport === "walking" ? "w" : widget.transport === "transit" ? "r" : "d"}`
        : `https://maps.apple.com/?q=${q(widget.query || widget.location || title)}${widget.near ? `&near=${q(widget.near)}` : ""}`;
    }
    const actions = el("div", "w-actions");
    actions.append(button("Haritada aç", "primary", () => openLink(url)));
    box.append(actions);
    return box;
  }

  function sparkline(series) {
    if (!series || series.length < 2) return null;
    const min = Math.min(...series);
    const max = Math.max(...series);
    const points = series.map((value, index) => `${(index / (series.length - 1)) * 80},${24 - ((value - min) / (max - min || 1)) * 22}`).join(" ");
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 80 26");
    svg.setAttribute("class", `spark ${series[series.length - 1] >= series[0] ? "up" : "down"}`);
    const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    line.setAttribute("points", points);
    svg.append(line);
    return svg;
  }

  function timer(widget, stopwatch) {
    const box = card(widget.title || (stopwatch ? "Kronometre" : "Zamanlayıcı"), widget.subtitle, "timer");
    box.classList.add("timer");
    const face = el("div", "timer-face");
    let total = stopwatch ? 0 : Math.round(widget.seconds || 0);
    let left = total;
    let running = false;
    let handle = null;
    const show = () => {
      const value = Math.max(0, left);
      const h = Math.floor(value / 3600);
      const m = Math.floor((value % 3600) / 60);
      const s = value % 60;
      face.textContent = `${h ? `${h}:` : ""}${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
    };
    const toggle = button("Başlat", "primary", () => {
      running = !running;
      toggle.textContent = running ? "Duraklat" : "Devam";
      clearInterval(handle);
      if (running) {
        handle = setInterval(() => {
          left += stopwatch ? 1 : -1;
          show();
          if (!stopwatch && left <= 0) {
            clearInterval(handle);
            running = false;
            toggle.textContent = "Başlat";
            face.classList.add("done");
            try { new Notification("Wiqo", { body: `${widget.title || "Zamanlayıcı"} bitti.` }); } catch { /* notifications off */ }
          }
        }, 1000);
      }
    });
    const reset = button("Sıfırla", "secondary", () => {
      clearInterval(handle);
      running = false;
      left = stopwatch ? 0 : total;
      toggle.textContent = "Başlat";
      face.classList.remove("done");
      show();
    });
    show();
    const actions = el("div", "w-actions");
    actions.append(reset, toggle);
    box.append(face, actions);
    return box;
  }

  function smallCard(widget, ctx) {
    switch (widget.kind) {
      case "timer": return timer(widget, false);
      case "stopwatch": return timer(widget, true);
      case "quotes": {
        const box = card(widget.title || "Piyasa", widget.subtitle, "chart.bar");
        const rows = el("div", "rows");
        for (const quote of widget.quotes || []) {
          const row = el("div", "row");
          const text = el("div", "row-text");
          text.append(el("div", "row-title", quote.symbol), el("div", "row-sub", quote.name));
          row.append(text);
          const spark = sparkline(quote.series);
          if (spark) row.append(spark);
          const value = el("div", "quote-value");
          value.append(el("div", "row-title", `${Number(quote.price).toLocaleString("tr-TR", { maximumFractionDigits: 4 })}${quote.unit ? ` ${quote.unit}` : ""}`));
          if (quote.changePercent !== null && quote.changePercent !== undefined) {
            value.append(pill(`${quote.changePercent >= 0 ? "+" : ""}${quote.changePercent.toFixed(2)}%`, quote.changePercent >= 0 ? "success" : "danger"));
          }
          row.append(value);
          rows.append(row);
        }
        box.append(rows);
        return box;
      }
      case "score": {
        const box = card(widget.title || "Skorlar", widget.subtitle, "sportscourt");
        const rows = el("div", "rows");
        for (const match of widget.matches || []) {
          const row = el("div", "row match");
          const score = match.homeScore === null || match.homeScore === undefined ? "–" : `${match.homeScore} – ${match.awayScore}`;
          row.append(el("div", "team", match.home), el("div", "score", score), el("div", "team away", match.away));
          const meta = el("div", "row-sub match-meta", [match.league, match.status, match.when ? date(match.when) : ""].filter(Boolean).join(" · "));
          const wrap = el("div", "match-wrap");
          wrap.append(row, meta);
          rows.append(wrap);
        }
        box.append(rows);
        return box;
      }
      case "translation": {
        const box = card(widget.title || "Çeviri", [widget.sourceLang, widget.targetLang].filter(Boolean).join(" → "), "character.bubble");
        box.append(el("div", "w-sub translation-original", widget.original || ""));
        const result = el("div", "translation-result", widget.translated);
        const actions = el("div", "w-actions");
        actions.append(copyButton(widget.translated || ""));
        box.append(result, actions);
        return box;
      }
      case "calc": {
        const box = card(widget.title || "Hesap", "", "function");
        box.append(el("div", "w-sub mono", widget.expression || ""), el("div", "metric-value", `= ${widget.result}`));
        return box;
      }
      case "recipe": {
        const facts = [widget.servings ? `${widget.servings} kişilik` : "", widget.minutes ? `${widget.minutes} dk` : ""].filter(Boolean).join(" · ");
        const box = card(widget.title || "Tarif", facts || widget.subtitle, "fork.knife");
        box.append(el("div", "overline", "Malzemeler"));
        const ingredients = el("ul", "checks");
        for (const item of widget.ingredients || []) ingredients.append(el("li", "", item));
        box.append(ingredients, el("div", "overline", "Yapılışı"));
        const steps = el("ol", "recipe-steps");
        for (const step of widget.directions || []) {
          const li = el("li", "", step.text);
          if (step.seconds) li.append(" ", pill(`${Math.round(step.seconds / 60)} dk`, "neutral"));
          steps.append(li);
        }
        box.append(steps);
        return box;
      }
      case "trip": {
        const box = card(widget.title || "Gezi planı", widget.subtitle, "airplane");
        for (const day of widget.tripDays || []) {
          const details = el("details", "accordion inner");
          details.open = true;
          details.append(el("summary", "", day.title));
          const rows = el("div", "rows");
          for (const item of day.items || []) {
            const row = el("div", "row");
            row.append(el("span", "row-meta time", item.time || ""));
            const text = el("div", "row-text");
            text.append(el("div", "row-title", item.title));
            const sub = [item.place, item.note].filter(Boolean).join(" · ");
            if (sub) text.append(el("div", "row-sub", sub));
            row.append(text);
            rows.append(row);
          }
          details.append(rows);
          box.append(details);
        }
        return box;
      }
      case "media": {
        const box = card(widget.title || widget.query, widget.service || widget.subtitle, "play.rectangle");
        const url = widget.url || `https://www.youtube.com/results?search_query=${encodeURIComponent(widget.query || "")}`;
        const actions = el("div", "w-actions");
        actions.append(button("Aç", "primary", () => openLink(url)));
        box.append(actions);
        return box;
      }
      case "checklist": {
        const box = card(widget.title || "Yapılacaklar", widget.subtitle, "checklist");
        const list = el("div", "checklist");
        for (const item of widget.checklist || []) {
          const label = el("label", "check");
          const input = el("input");
          input.type = "checkbox";
          input.onchange = () => label.classList.toggle("done", input.checked);
          label.append(input, el("span", "", item));
          list.append(label);
        }
        box.append(list);
        return box;
      }
      case "contact": return card(widget.title || widget.contactQuery, "Rehberdeki kişiler telefonda açılır.", "person.crop.circle");
      default: return card(widget.title || "Kart", widget.subtitle || widget.detail);
    }
  }

  // An inline app: its page runs in a sandboxed frame with no access to this window, the same kit the iPhone injects, and no network.
  const kit = {};
  async function kitSource(name) {
    if (!(name in kit)) kit[name] = await fetch(`inline/${name}.js`).then((response) => response.text()).catch(() => "");
    return kit[name];
  }
  const INLINE_POLICY = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob: https://api.wiqoai.com; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

  function inlineApp(widget) {
    const box = card(widget.title, widget.summary);
    box.classList.add("inline-app");
    if (!widget.html) {
      box.append(el("div", "w-sub", "Bu uygulama telefondaki Wiqo'da açılır."));
      return box;
    }
    const frame = el("iframe", "inline-frame");
    frame.setAttribute("sandbox", "allow-scripts");
    frame.style.height = `${Math.min(Math.max(Number(widget.height) || 360, 160), 900)}px`;
    box.append(frame);
    (async () => {
      const libs = new Set(widget.libs || []);
      if (libs.has("templates")) libs.add("chart");
      const names = ["core", ...["chart", "confetti", "templates"].filter((name) => libs.has(name))];
      let data = "null";
      try {
        data = widget.data ? JSON.stringify(JSON.parse(widget.data)) : "null";
      } catch { /* malformed data: none */ }
      const sources = await Promise.all(names.map(kitSource));
      const safe = (code) => code.replace(/<\/script/gi, "<\\/script");
      const scripts = [`window.__WIQO_STATE__={};window.__WIQO_DATA__=${safe(data)};`, ...sources].map((code) => `<script>${safe(code)}</script>`).join("");
      const meta = `<meta http-equiv="Content-Security-Policy" content="${INLINE_POLICY}">`;
      const html = String(widget.html);
      const head = html.match(/<head[^>]*>/i);
      frame.srcdoc = head ? html.replace(head[0], `${head[0]}${meta}${scripts}`) : `<!doctype html><html><head>${meta}${scripts}</head><body>${html}</body></html>`;
    })();
    return box;
  }

  function undoCard(widget, kind, ctx) {
    const box = card(widget.title || "Değişiklik yapıldı", widget.summary || (widget.fileCount ? `${widget.fileCount} dosya` : ""), "arrow.uturn.backward");
    const actions = el("div", "w-actions");
    actions.append(button("Geri al", "secondary", async () => {
      actions.replaceChildren(el("span", "w-sub", "Geri alınıyor…"));
      try {
        actions.replaceChildren(pill(await invoke("undo_record", { kind, id: widget.undoId }), "success"));
      } catch (error) {
        actions.replaceChildren(el("span", "error small", String(error)));
      }
    }));
    box.append(actions);
    return box;
  }

  function widget(item, ctx) {
    if (!item || typeof item !== "object") return null;
    switch (item.type) {
      case "weather": return weather(item);
      case "sources": return sources(item);
      case "artifact": return artifactCard(item, ctx);
      case "task_plan": return taskPlan(item);
      case "task_result": {
        const box = card(item.title || "Görev", statusLabel(item.taskStatus || item.status));
        if (item.summary || item.detail) box.append(markdown(item.summary || item.detail));
        return box;
      }
      case "approval": return approval(item, ctx);
      case "clarification": return clarification(item, ctx);
      case "suggestions": return suggestions(item, ctx);
      case "error": {
        const box = callout(item.message || "Bir hata oluştu.", "warning");
        if (item.retryable && ctx.latest && ctx.retry) box.append(button("Tekrar dene", "secondary small", () => ctx.retry()));
        return box;
      }
      case "list": return listWidget(item);
      case "table": return tableCard(item.columns || [], item.rows || [], item.totalRows);
      case "map": return mapWidget(item);
      case "card": return smallCard(item, ctx);
      case "inline_app": return inlineApp(item);
      case "terminal_undo": return undoCard(item, "terminal", ctx);
      case "file_undo": return undoCard(item, "file", ctx);
      case "act_handoff": return card(item.title || item.projectName || "Act projesi", item.summary || "Bu iş telefondaki Wiqo'nun Act bölümünde devam eder.", "hammer");
      // A quiet action seen instead of read ("Görev eklendi · Süt al"): the icon says what kind, the check says it is done.
      case "receipt": {
        const icons = { task: "checklist", goal: "sparkle", memory: "doc.text", undo: "arrow.uturn.backward", automation: "clock.badge.checkmark" };
        const box = card(item.title || "Tamamlandı", item.detail, icons[item.family] || "checkmark");
        box.classList.add("w-receipt");
        box.append(pill("Tamam", "success"));
        return box;
      }
      case "created_resource": {
        const box = card(item.title || (item.resourceType === "agent" ? "Ajan" : "Otomasyon"), item.scheduleLabel || item.detail, item.resourceType === "agent" ? "sparkle" : "clock.badge.checkmark");
        const meta = el("div", "w-actions");
        if (item.status) meta.append(pill(statusLabel(item.status), toneOf(item.status)));
        if (item.nextRunAt) meta.append(el("span", "w-sub small", `Sonraki: ${date(item.nextRunAt)}`));
        box.append(meta);
        return box;
      }
      case "code_change": {
        const box = card(item.title || "Kod değişikliği", item.summary);
        const meta = el("div", "w-actions");
        if (item.projectVersion) meta.append(pill(`Sürüm ${item.projectVersion}`, "neutral"));
        if (item.verified !== undefined && item.verified !== null) meta.append(pill(item.verified ? "Doğrulandı" : "Doğrulanmadı", item.verified ? "success" : "warning"));
        box.append(meta);
        return box;
      }
      case "agent_task": case "project_task": case "deferred_task": case "act_task": {
        const box = card(item.title || item.name || "Görev", item.summary || item.detail);
        if (item.status) box.append(pill(statusLabel(item.status), toneOf(item.status)));
        return box;
      }
      default: return null;
    }
  }

  window.WiqoRender = { markdown, widget, artifactCard, stepList, pill, el, button, toneOf, statusLabel };
})();
