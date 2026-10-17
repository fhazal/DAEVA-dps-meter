// Shared helpers for every Daeva Meter window.
(function () {
  const T = window.__TAURI__;
  const mock = window.__DAEVA_MOCK__;

  async function call(cmd, args) {
    if (mock) return mock(cmd, args || {});
    if (!T) throw new Error("Tauri API unavailable");
    return T.core.invoke(cmd, args || {});
  }
  async function callJSON(cmd, args) {
    const r = await call(cmd, args);
    return typeof r === "string" ? JSON.parse(r) : r;
  }

  function fmt(n) {
    n = Number(n) || 0;
    const a = Math.abs(n);
    if (a >= 1e9) return (n / 1e9).toFixed(2) + "B";
    if (a >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (a >= 1e4) return (n / 1e3).toFixed(1) + "k";
    if (a >= 1000) return (n / 1e3).toFixed(2) + "k";
    return Math.round(n).toString();
  }
  function pct(x, digits) {
    return ((Number(x) || 0) * 100).toFixed(digits == null ? 0 : digits) + "%";
  }
  function dur(s) {
    s = Math.max(0, Math.floor(Number(s) || 0));
    const m = Math.floor(s / 60);
    const h = Math.floor(m / 60);
    const ss = String(s % 60).padStart(2, "0");
    return h ? `${h}:${String(m % 60).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
  }
  function when(unix) {
    if (!unix) return "";
    const d = new Date(unix * 1000);
    const today = new Date();
    const t = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    return d.toDateString() === today.toDateString() ? t : d.toLocaleDateString([], { day: "numeric", month: "short" }) + " " + t;
  }
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  // Class colours (AION 2 classes plus a fallback hash for unknown ids).
  const CLASS_COLORS = {
    gladiator: "#C46B36",    // Burnished Bronze
    templar: "#4387D6",      // Guardian Blue
    assassin: "#A86CE0",     // Shadow Violet
    ranger: "#49A879",       // Forest Green
    sorcerer: "#E56A3D",     // Arcane Ember
    elementalist: "#43B7C7", // Spirit Cyan (Spiritmaster)
    spiritmaster: "#43B7C7",
    cleric: "#E6C65A",       // Holy Gold
    chanter: "#D985B5",      // Mystic Rose
  };
  // Original line-art class glyphs (16x16, stroke = currentColor).
  const CLASS_ICONS = {
    gladiator: '<path d="M3 13l7-7M10 6l2.5-2.5L13 3l-.5.5L10 6"/><path d="M4.5 9.5l2 2M2.5 13.5l1-1"/><path d="M8 3.5l1 1M11.5 7l1 1"/>',
    templar: '<path d="M8 2l5 2v4c0 3-2.2 5-5 6-2.8-1-5-3-5-6V4z"/><path d="M8 5v6M6 7.5h4"/>',
    assassin: '<path d="M3 13l6.5-6.5M9.5 6.5L13 3l-.6 3.4z"/><path d="M5 9.5l1.5 1.5M10 13l1.5-1.5L13 13"/>',
    ranger: '<path d="M4 2.5c4.5 2 4.5 9 0 11"/><path d="M4 2.5v11"/><path d="M2.5 8h10M10.5 6l2 2-2 2"/>',
    sorcerer: '<path d="M8 2.5c2 2.2 3.5 3.8 3.5 6a3.5 3.5 0 0 1-7 0c0-1.6.9-2.7 1.8-3.5.1 1.2.7 2 1.4 2.2C7.4 5.5 7.6 3.9 8 2.5z"/>',
    elementalist: '<path d="M8 8a2 2 0 1 1 2-2c0 2.8-2.2 5-5 5"/><path d="M8 8a2 2 0 1 0-2 2c2.8 0 5-2.2 5-5" opacity=".7"/><circle cx="8" cy="8" r="6"/>',
    spiritmaster: '<path d="M8 8a2 2 0 1 1 2-2c0 2.8-2.2 5-5 5"/><circle cx="8" cy="8" r="6"/>',
    cleric: '<path d="M8 2v12M4.5 5.5h7"/><circle cx="8" cy="5.5" r="2.2"/>',
    chanter: '<circle cx="8" cy="8" r="5.5"/><circle cx="8" cy="8" r="2.5"/><path d="M8 2.5v2.5M8 11v2.5M2.5 8H5M11 8h2.5"/>',
  };
  // ---- game art from aion2.gaming.tools (used with permission) ----
  // Images belong to NCSOFT; served by gaming.tools. Built-in SVGs are the
  // fallback whenever art is off or an image fails.
  const art = { on: false, base: "", icons: {}, sprites: {} };
  async function loadArt() {
    try {
      const a = await callJSON("get_icons");
      art.on = !!a.on; art.base = a.base || ""; art.icons = a.icons || {};
    } catch { art.on = false; }
  }
  if (!mock) { loadArt(); setInterval(loadArt, 60 * 1000); }
  function skillKey(id) {
    id = Number(id) || 0;
    if (id >= 2147483648) id -= 2147483648; // DoT entries carry the top bit
    return id >= 10000000 && id < 20000000 ? Math.floor(id / 10000) * 10000 : id;
  }
  const imgTag = (path, px, cls, title) => `<img class="gt-ic ${cls || ""}" src="${esc(art.base + path)}" width="${px}" height="${px}" loading="lazy" alt="" ${title ? `title="${esc(title)}"` : ""} onerror="this.remove()">`;
  function skillIcon(id, size) {
    if (!art.on) return "";
    const raw = Number(id) >= 2147483648 ? Number(id) - 2147483648 : Number(id);
    const p = art.icons["s" + raw] || art.icons["s" + skillKey(id)];
    return p ? imgTag(p, size || 18, "sk-ic") : "";
  }
  function npcIcon(tid, size) {
    if (!art.on || !tid) return "";
    const p = art.icons["n" + tid];
    return p ? imgTag(p, size || 28, "npc-ic") : "";
  }
  function classSprite(key) {
    const sp = art.sprites[key];
    if (sp) return sp.ok ? sp : null;
    const s2 = (art.sprites[key] = { ok: false });
    const img = new Image();
    img.onload = () => {
      const w = img.naturalWidth, h = img.naturalHeight;
      if (!w || !h) return;
      // Sprite sheets hold several square states; show the first one.
      s2.size = w > h ? "auto 100%" : h > w ? "100% auto" : "100% 100%";
      s2.url = img.src;
      s2.ok = true;
    };
    img.src = `${art.base}/images/ui/resource/sprite/common/ut_common_icon_class_${key}_sprite.webp`;
    return null;
  }
  function classIcon(cls, size) {
    const key = String(cls || "").toLowerCase();
    const px = size || 14;
    if (art.on && CLASS_COLORS[key]) {
      const sp = classSprite(key);
      if (sp) return `<span class="cls-icon gt-cls" title="${esc(cls)}" style="display:inline-block;flex:none;width:${px}px;height:${px}px;background:url('${sp.url}') 0 0 / ${sp.size} no-repeat"></span>`;
    }
    const path = CLASS_ICONS[key];
    if (!path) return "";
    return `<svg class="cls-icon" viewBox="0 0 16 16" width="${px}" height="${px}" style="color:${classColor(cls)};flex:none" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-label="${key}">${path}</svg>`;
  }
  function classColor(cls, name) {
    const key = String(cls || "").toLowerCase();
    if (CLASS_COLORS[key]) return CLASS_COLORS[key];
    let h = 0;
    for (const c of String(cls || name || "?")) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    return `hsl(${h % 360} 55% 62%)`;
  }

  function applyTheme(ui) {
    if (!ui) return;
    const r = document.documentElement.style;
    if (ui.accent) {
      r.setProperty("--accent", ui.accent);
      r.setProperty("--accent-soft", hexAlpha(ui.accent, 0.18));
      r.setProperty("--self", hexAlpha(ui.accent, 0.14));
    }
    if (ui.opacity != null) r.setProperty("--bg-alpha", String(Math.min(1, Math.max(0.2, ui.opacity))));
    if (ui.scale) document.body.style.zoom = String(ui.scale);
  }
  function hexAlpha(hex, a) {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex || "");
    if (!m) return `rgba(217,164,65,${a})`;
    const v = parseInt(m[1], 16);
    return `rgba(${v >> 16},${(v >> 8) & 255},${v & 255},${a})`;
  }

  let toastTimer;
  function toast(msg) {
    let el = document.querySelector(".toast");
    if (!el) {
      el = document.createElement("div");
      el.className = "toast";
      document.body.appendChild(el);
    }
    el.textContent = msg;
    el.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.classList.remove("show"), 2200);
  }

  const ICONS = {
    close: '<path d="M4 4l8 8M12 4l-8 8"/>',
    min: '<path d="M4 8h8"/>',
    gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M3.6 12.4l1.1-1.1M11.3 4.7l1.1-1.1"/>',
    history: '<path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9"/><path d="M2.5 2.5v2.2h2.2"/><path d="M8 5v3.2l2 1.3"/>',
    reset: '<path d="M3 8a5 5 0 1 1 1.5 3.6"/><path d="M3 12V8.8h3.2"/>',
    copy: '<rect x="5" y="5" width="8" height="8" rx="1.5"/><path d="M3 10.5V3.5A.5.5 0 0 1 3.5 3h7"/>',
    lock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.3"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/>',
    unlock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1.3"/><path d="M5.5 7V5a2.5 2.5 0 0 1 4.8-1"/>',
    flask: '<path d="M6 2h4M6.5 2v4L3 13a1 1 0 0 0 .9 1.4h8.2A1 1 0 0 0 13 13L9.5 6V2"/><path d="M4.6 10h6.8"/>',
    metric: '<path d="M3 13V9M6.5 13V5M10 13V7.5M13.5 13V3"/>',
  };
  function icon(name) {
    return `<svg viewBox="0 0 16 16">${ICONS[name] || ""}</svg>`;
  }

  async function winAction(action) {
    try { await call("win_action", { action }); } catch (e) { console.warn(e); }
  }

  async function params() {
    try { return await callJSON("window_params"); } catch { return {}; }
  }

  function wireTitlebar(root) {
    (root || document).querySelectorAll("[data-win]").forEach((b) => {
      b.addEventListener("click", (e) => { e.stopPropagation(); winAction(b.dataset.win); });
    });
  }

  window.Daeva = { call, callJSON, fmt, pct, dur, when, esc, classColor, classIcon, skillIcon, npcIcon, loadArt, art, applyTheme, toast, icon, winAction, params, wireTitlebar };
})();
