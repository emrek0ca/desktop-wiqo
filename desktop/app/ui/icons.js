// Icons: the iPhone app's SF Symbols. On macOS the system draws each symbol (Rust `symbol` command) and the page uses it as a mask, so it takes
// the text colour; elsewhere a close SVG stands in. `icon("magnifyingglass", 15)` returns an element; `[data-symbol]` elements in the page
// are filled on load.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const cache = new Map();

  // Stand-ins in the SF Symbols spirit (24×24, 1.8 stroke) for systems without SF Symbols.
  const SVG = {
    "magnifyingglass": '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m15.5 15.5 5 5"/>',
    "square.and.pencil": '<path d="M12 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-6"/><path d="M18.4 3.6a2 2 0 1 1 2.8 2.8L12 15.6l-3.6.8.8-3.6z"/>',
    "sidebar.left": '<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M9 4v16"/>',
    "sidebar.right": '<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M15 4v16"/>',
    "laptopcomputer": '<rect x="5" y="5" width="14" height="10" rx="1.5"/><path d="M3 18h18"/>',
    "clock.arrow.circlepath": '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l3 2"/>',
    "iphone": '<rect x="7" y="2.5" width="10" height="19" rx="2.5"/><path d="M11 18.5h2"/>',
    "gearshape": '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
    "ellipsis": '<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>',
    "arrow.up": '<path d="M12 19V5"/><path d="m5 12 7-7 7 7"/>',
    "stop.fill": '<rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor"/>',
    "plus": '<path d="M12 5v14M5 12h14"/>',
    "doc.on.doc": '<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V5a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h2"/>',
    "arrow.clockwise": '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>',
    "checkmark": '<path d="m5 12 5 5 9-10"/>',
    "checkmark.circle.fill": '<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>',
    "exclamationmark.circle.fill": '<circle cx="12" cy="12" r="9"/><path d="M12 7v6M12 16.5v.5"/>',
    "exclamationmark.triangle": '<path d="M10.3 4 2.8 17a2 2 0 0 0 1.7 3h15a2 2 0 0 0 1.7-3L13.7 4a2 2 0 0 0-3.4 0z"/><path d="M12 9v4M12 16.5v.5"/>',
    "info.circle": '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 7.5v.5"/>',
    "chevron.down": '<path d="m6 9 6 6 6-6"/>',
    "chevron.right": '<path d="m9 6 6 6-6 6"/>',
    "location": '<path d="M12 21s-7-6.2-7-11.5A7 7 0 0 1 19 9.5C19 14.8 12 21 12 21z"/><circle cx="12" cy="9.5" r="2.5"/>',
    "chevron.left": '<path d="m15 6-6 6 6 6"/>',
    "eye": '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
    "eye.slash": '<path d="M2 12s3.5-7 10-7c2 0 3.7.6 5.1 1.5M22 12s-3.5 7-10 7c-2 0-3.7-.6-5.1-1.5"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2M3 3l18 18"/>',
    "xmark": '<path d="M6 6l12 12M18 6 6 18"/>',
    "envelope": '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3 7 9 6 9-6"/>',
    "calendar": '<rect x="3" y="5" width="18" height="16" rx="2"/><path d="M3 10h18M8 3v4M16 3v4"/>',
    "folder": '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    "doc.text": '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M9 13h6M9 17h6"/>',
    "person.crop.circle": '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="10" r="3"/><path d="M6.5 18a6 6 0 0 1 11 0"/>',
    "map": '<path d="m9 4-6 2v14l6-2 6 2 6-2V4l-6 2z"/><path d="M9 4v14M15 6v14"/>',
    "play.rectangle": '<rect x="3" y="5" width="18" height="14" rx="3"/><path d="m10 9 5 3-5 3z"/>',
    "timer": '<circle cx="12" cy="13" r="8"/><path d="M12 9v4l2 2M9 2h6"/>',
    "photo": '<rect x="3" y="5" width="18" height="14" rx="2"/><circle cx="9" cy="10" r="2"/><path d="m21 16-5-5-9 8"/>',
    "tablecells": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 10h18M3 15h18M9 4v16M15 4v16"/>',
    "chart.bar": '<path d="M4 20V10M10 20V4M16 20v-7M22 20H2"/>',
    "chevron.left.forwardslash.chevron.right": '<path d="m8 7-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/>',
    "sparkle": '<path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z"/>',
    "clock.badge.checkmark": '<circle cx="11" cy="12" r="8"/><path d="M11 8v4l2 2"/><path d="m16 19 2 2 4-4"/>',
    "arrow.uturn.backward": '<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/>',
    "link": '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
    "safari": '<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>',
    "lock.shield": '<path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/><rect x="9" y="11" width="6" height="5" rx="1"/><path d="M10 11V9.5a2 2 0 0 1 4 0V11"/>',
    "hammer": '<path d="m14 6 4 4-9 9-4-4z"/><path d="m13 5 3-2 5 5-2 3"/>',
    "laptopcomputer.and.iphone": '<rect x="2" y="5" width="13" height="9" rx="1.5"/><path d="M1 17h14"/><rect x="17" y="8" width="6" height="11" rx="1.5"/>',
    "exclamationmark.bubble": '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.5A8 8 0 1 1 21 12z"/><path d="M12 8v4M12 15.5v.5"/>',
    "rectangle.portrait.and.arrow.right": '<path d="M14 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h8"/><path d="M10 12h11M18 9l3 3-3 3"/>',
    "questionmark.circle": '<circle cx="12" cy="12" r="9"/><path d="M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .9-1 1.6v.6M12 17v.5"/>',
    "list.bullet": '<path d="M9 6h12M9 12h12M9 18h12"/><circle cx="4" cy="6" r="1"/><circle cx="4" cy="12" r="1"/><circle cx="4" cy="18" r="1"/>',
    "bolt": '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
    "cpu": '<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3"/>',
    "terminal": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M12 15h5"/>',
    "dollarsign.circle": '<circle cx="12" cy="12" r="9"/><path d="M15 9a3 2.5 0 0 0-3-1.5c-1.7 0-3 .9-3 2s1.3 1.7 3 2 3 .9 3 2-1.3 2-3 2a3 2.5 0 0 1-3-1.5M12 6v12"/>',
    "sportscourt": '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M12 5v14"/><circle cx="12" cy="12" r="2.5"/>',
    "character.bubble": '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.5A8 8 0 1 1 21 12z"/><path d="m9 15 3-7 3 7M10 13h4"/>',
    "function": '<path d="M14 4h-1a3 3 0 0 0-3 3v10a3 3 0 0 1-3 3H6M7 11h7M15 12l5 6M20 12l-5 6"/>',
    "fork.knife": '<path d="M7 3v8M5 3v5a2 2 0 0 0 4 0V3M7 11v10M17 21V3c-2 1-3 4-3 7h3"/>',
    "airplane": '<path d="M10 15 3 13l1-2 6 1V6a2 2 0 0 1 4 0v6l6-1 1 2-7 2v4l2 2-1 1-3-1-3 1-1-1 2-2z"/>',
    "checklist": '<path d="m3 6 2 2 3-3M3 13l2 2 3-3M11 6h10M11 13h10M11 19h10"/>',
    "sun.max.fill": '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    "cloud.sun.fill": '<path d="M12 4v1M5.6 6.6l.7.7M3 13h1M18.4 6.6l-.7.7"/><path d="M8 13a4 4 0 0 1 7.5-2A3.5 3.5 0 1 1 17 18H8a3 3 0 0 1 0-6"/>',
    "cloud.fill": '<path d="M7 18a4 4 0 0 1-.6-8A6 6 0 0 1 18 9.5 4.3 4.3 0 0 1 17.5 18z"/>',
    "cloud.fog.fill": '<path d="M7 14a4 4 0 0 1-.6-8A6 6 0 0 1 18 5.5 4.3 4.3 0 0 1 17.5 14z"/><path d="M4 18h16M6 21h12"/>',
    "cloud.drizzle.fill": '<path d="M7 15a4 4 0 0 1-.6-8A6 6 0 0 1 18 6.5 4.3 4.3 0 0 1 17.5 15z"/><path d="M8 18v1M12 18v1M16 18v1"/>',
    "cloud.rain.fill": '<path d="M7 14a4 4 0 0 1-.6-8A6 6 0 0 1 18 5.5 4.3 4.3 0 0 1 17.5 14z"/><path d="m8 17-1 3M12 17l-1 3M16 17l-1 3"/>',
    "cloud.snow.fill": '<path d="M7 14a4 4 0 0 1-.6-8A6 6 0 0 1 18 5.5 4.3 4.3 0 0 1 17.5 14z"/><path d="M8 18h.01M12 20h.01M16 18h.01"/>',
    "cloud.bolt.rain.fill": '<path d="M7 14a4 4 0 0 1-.6-8A6 6 0 0 1 18 5.5 4.3 4.3 0 0 1 17.5 14z"/><path d="m12 15-2 4h3l-2 4"/>',
  };

  function fallback(span, name) {
    const body = SVG[name] || SVG["sparkle"];
    span.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;
  }

  function fill(span) {
    const name = span.dataset.symbol;
    const size = Number(span.dataset.size || 16);
    const weight = Number(span.dataset.weight || 0);
    span.style.width = span.style.height = `${size}px`;
    if (!invoke) return fallback(span, name);
    const key = `${name}|${size}|${weight}`;
    if (!cache.has(key)) cache.set(key, invoke("symbol", { name, size, weight }).catch(() => null));
    cache.get(key).then((url) => {
      if (!url) return fallback(span, name);
      span.classList.add("masked");
      span.style.webkitMaskImage = span.style.maskImage = `url("${url}")`;
    });
  }

  // Weights as NSFontWeight: regular 0, medium .23, semibold .3.
  function icon(name, size = 16, weight = 0, className = "") {
    const span = document.createElement("span");
    span.className = `sf ${className}`.trim();
    span.dataset.symbol = name;
    span.dataset.size = size;
    span.dataset.weight = weight;
    span.setAttribute("aria-hidden", "true");
    fill(span);
    return span;
  }

  function hydrate(root = document) {
    for (const span of root.querySelectorAll("[data-symbol]:not(.sf-ready)")) {
      span.classList.add("sf", "sf-ready");
      fill(span);
    }
  }

  window.WiqoIcons = { icon, hydrate };
  document.addEventListener("DOMContentLoaded", () => hydrate());
  hydrate();
})();
