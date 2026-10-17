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
    gladiator: "#e07a4f",
    templar: "#d9b44a",
    assassin: "#b56fd6",
    ranger: "#6fb55a",
    sorcerer: "#5aa2e0",
    spiritmaster: "#8f7ae0",
    cleric: "#e8e2c4",
    chanter: "#4fc1b0",
  };
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

  window.Daeva = { call, callJSON, fmt, pct, dur, when, esc, classColor, applyTheme, toast, icon, winAction, params, wireTitlebar };
})();
