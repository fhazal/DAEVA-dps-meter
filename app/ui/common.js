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
  // Event artwork from gaming.tools' activities data (mirrored by the Daeva
  // server). Events without a fitting picture keep their emoji.
  const EVENT_ART = {
    shugo_festival: "/images/ui/resource/texture/icon_fieldevent/thumbnail/ut_fieldevent_thumbnail_defence.webp",
    dimensional_invasion: "/images/ui/resource/texture/icon/ut_marker_battlefield.webp",
    artifact_siege: "/images/ui/resource/texture/icon/ut_marker_abyssartifact_neutral.webp",
    watcher_kaira: "/images/ui/resource/texture/icon/ut_marker_monster_boss.webp",
    executor_bosses: "/images/ui/resource/texture/icon/ut_marker_monster_boss.webp",
    guardian_nahma: "/images/ui/resource/texture/icon/ut_marker_monster_boss.webp",
    arena_of_tactics: "/images/ui/resource/texture/icon/ut_marker_dungeon.webp",
    gartua: "/images/ui/resource/texture/portrait/portrait_256/ut_256_mob_gartua_01.webp",
  };
  // Icons shipped with the meter: always shown, even with game art off.
  const EVENT_LOCAL = {
    spacetime_rift: "spacetime_rift",
    shugo_festival: "shugo_festival",
    dimensional_invasion: "dimensional_invasion",
    artifact_siege: "artifact_siege",
    arena_of_tactics: "arena_of_tactics",
    daily_reset: "reset",
    weekly_reset: "reset",
    watcher_kaira: "field_boss",
    executor_bosses: "field_boss",
    guardian_nahma: "field_boss",
  };
  function eventIcon(id, emoji, size) {
    const px = size || 14;
    const local = EVENT_LOCAL[id];
    const src = local ? `img/events/${local}.webp` : art.on && art.base && EVENT_ART[id] ? art.base + EVENT_ART[id] : null;
    const fallback = `<span class="ev-emoji">${esc(emoji || "🔔")}</span>`;
    if (!src) return fallback;
    // If the picture is missing, swap back to the emoji.
    return `<img class="gt-ic ev-ic" src="${esc(src)}" width="${px}" height="${px}" alt="" data-e="${esc(emoji || "🔔")}" onerror="this.outerHTML='<span class=&quot;ev-emoji&quot;>'+this.dataset.e+'</span>'">`;
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

  // Window border in your class colour, shared by every window (remembered).
  function applyClassBorder(cls) {
    try { if (cls) localStorage.setItem("daeva.selfClass", cls); else cls = localStorage.getItem("daeva.selfClass"); } catch {}
    const sh = document.querySelector(".shell");
    if (sh) sh.style.setProperty("--cls", cls ? classColor(cls) : "");
  }
  function applyTheme(ui) {
    applyClassBorder(null);
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

  // Icons: Lucide (lucide.dev, ISC licence), drawn on its 24-unit grid and
  // scaled into our 16-unit buttons. Lucide is the project's icon set.
  const ICONS = {
    close: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></g>',
    min: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M5 12h14"/></g>',
    // Download tray from Lucide (lucide.dev, ISC licence), 24-unit grid.
    download: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M12 15V3"/><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/></g>',
    // Globe from Lucide (lucide.dev, ISC licence), 24-unit grid: open-world bosses.
    globe: '<g transform="scale(0.6667)" stroke-width="2.4"><circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/></g>',
    // Cog wheel from Lucide (lucide.dev, ISC licence), drawn on a 24-unit grid.
    gear: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/></g>',
    history: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></g>',
    reset: '<g transform="scale(0.6667)" stroke-width="2.4"><circle cx="12" cy="12" r="10"/><rect x="9" y="9" width="6" height="6" rx="1"/></g>',
    copy: '<g transform="scale(0.6667)" stroke-width="2.4"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></g>',
    lock: '<g transform="scale(0.6667)" stroke-width="2.4"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></g>',
    unlock: '<g transform="scale(0.6667)" stroke-width="2.4"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 9.9-1"/></g>',
    flask: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M14 2v6a2 2 0 0 0 .245.96l5.51 10.08A2 2 0 0 1 18 22H6a2 2 0 0 1-1.755-2.96l5.51-10.08A2 2 0 0 0 10 8V2"/><path d="M6.453 15h11.094"/><path d="M8.5 2h7"/></g>',
    metric: '<g transform="scale(0.6667)" stroke-width="2.4"><path d="M3 3v16a2 2 0 0 0 2 2h16"/><path d="M18 17V9"/><path d="M13 17V5"/><path d="M8 17v-3"/></g>',
    // Panels from Lucide (lucide.dev, ISC licence), 24-unit grid: switch layout.
    layout: '<g transform="scale(0.6667)" stroke-width="2.4"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 9h18"/><path d="M9 21V9"/></g>',
  };
  // Red quit, yellow minimise, green switch layout (`to`: the other layout).
  function lights(to) {
    const g = to === "solo" ? "Switch to Solo: just you" : "Switch to Classic: the whole party";
    return `<span class="lights hide-locked"><button type="button" class="lt r" data-lt="close" title="Quit Daeva Meter"><svg viewBox="0 0 8 8"><path d="M2 2l4 4M6 2L2 6"/></svg></button>`
      + `<button type="button" class="lt y" data-lt="min" title="Minimise"><svg viewBox="0 0 8 8"><path d="M1.6 4h4.8"/></svg></button>`
      + `<button type="button" class="lt g" data-lt="layout" data-to="${to}" title="${g} (Ctrl+Shift+O)"><svg viewBox="0 0 8 8"><path d="M2.2 5.8L5.8 2.2M3.4 2.2h2.4v2.4"/></svg></button></span>`;
  }
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

  // Window dragging by title bar. Not Tauri's data-tauri-drag-region: that
  // maximises the window on double-click. A single press drags; double-clicks
  // do nothing; nothing drags while the meter is locked.
  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.detail !== 1 || !T) return;
    if (!e.target.closest || !e.target.closest("[data-drag]")) return;
    if (e.target.closest("button, input, select, textarea, a")) return;
    if (document.body.classList.contains("locked")) return;
    try { T.window.getCurrentWindow().startDragging(); } catch (err) { console.warn(err); }
  });
  document.addEventListener("dblclick", (e) => {
    if (e.target.closest && e.target.closest("[data-drag]")) e.preventDefault();
  });

  window.Daeva = { call, callJSON, fmt, pct, dur, when, esc, classColor, classIcon, skillIcon, npcIcon, eventIcon, loadArt, art, applyTheme, applyClassBorder, toast, icon, lights, winAction, params, wireTitlebar };
})();
