// Renders the README pictures from the real overlay UI (app/ui) with a mocked backend.
// Run after each release: node docs/branding/showcase/render.js
// Needs Node 18+ and Microsoft Edge. Writes to docs/branding:
//   showcase.png (top picture, also the Discord release post), shot-classic.png, shot-solo.png, shot-alert.png,
//   site-meter.png (aion2.bajubesi.org). Copy showcase.png and site-meter.png into the bot's
//   source/public as meter-showcase.png and daeva-meter.png.
const fs = require("fs"), os = require("os"), path = require("path");
const { execFileSync } = require("child_process");

const root = path.resolve(__dirname, "../../..");
const ui = path.join(root, "app/ui/").replace(/\\/g, "/");
const outDir = path.join(root, "docs/branding");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "daeva-showcase-"));
const version = JSON.parse(fs.readFileSync(path.join(root, "app/src-tauri/tauri.conf.json"), "utf8")).version;
const edge = ["C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"].find((p) => fs.existsSync(p));
if (!edge) throw new Error("Microsoft Edge not found");

// Made-up party, so no real player names end up in the README.
const now = Date.now() / 1000;
const P = (id, name, cls, dps, share, crit, back, me) => ({ id, name, class: cls, is_self: !!me, in_party: true, dmg: Math.round(dps * 212), dps, share, hits: 400, crit_rate: crit, back_rate: back, perfect_rate: 0.09, front_rate: 1 - back, double_rate: 0.17, cp: me ? 74100 : null, max_hit: 99000, max_hit_skill: "x", heal: 0, hps: 0, taken: 0, deaths: 0 });
const fight = { fight_id: 7, live: true, duration: 212, clock: "boss", clock_secs: 212, clock_target_name: "Watcher Kaira", total: 10.85e6,
  players: [P(2, "Seraphiel", "Cleric", 12900, 0.28, 0.31, 0.42, true), P(3, "Nyxara", "Sorcerer", 11800, 0.26, 0.28, 0.18), P(4, "Drakthar", "Gladiator", 9710, 0.21, 0.24, 0.55), P(5, "Kaelith", "Chanter", 6220, 0.14, 0.19, 0.08), P(6, "Vaelor", "Ranger", 5420, 0.12, 0.22, 0.37)],
  target: { id: 50, name: "Watcher Kaira", hp: 20.15e6, max_hp: 31e6, hp_pct: 0.65, berserk: 600, secs: 212, rank: "Heroic", family: "Cogni", dmg_taken: 10.85e6 } };
const status = { version, server: "Beritra", ping_ms: 79, odyle: 640, odyle_bonus: 40, self_class: "Cleric", self_name: "Seraphiel", devices: [1], game_process: true, servers: [1], game_idle: 1 };
const settings = { solo: { rank: true, chips: true, footer: true }, accent: "#d9a441", opacity: 0.94, scale: 1, rows: 8, columns: [], show_players: "auto", primary_metric: "dps", dps_clock: "boss", alerts: { enabled: true }, hotkeys: {} };
const next = [{ id: "spacetime_rift", short: "Rift", name: "Spacetime Rift", emoji: "🌀", starts: now - 240, ends: now + 1560, active: true }, { id: "shugo_festival", short: "Shugo", name: "Shugo Festival", emoji: "🎪", starts: now + 394, ends: now + 994, active: false }];

const base = `<base href="file:///${ui.replace(/ /g, "%20")}">`;
const withMock = (file, mock) => fs.readFileSync(ui + file, "utf8").replace("<head>", "<head>" + base).replace('<script src="common.js"></script>', `<script>window.__DAEVA_MOCK__=async(cmd)=>{${mock}return null;};</script><script src="common.js"></script>`);
const J = (v) => JSON.stringify(JSON.stringify(v));

function meter(layout) {
  const snap = { party: null, status, fight, ui: { layout, solo: settings.solo, show_target: true, opacity: 0.94, scale: 1, accent: "#d9a441", rows: 8, columns: [], primary_metric: "dps" }, alerts: { enabled: true, show_next: true, next } };
  return withMock("index.html", `if(cmd==="get_snapshot")return ${J(snap)};if(cmd==="get_settings")return JSON.parse(${J({ ...settings, layout })});if(cmd==="get_events")return JSON.stringify({catalog:[]});if(cmd==="update_status")return JSON.stringify({current:${JSON.stringify(version)}});if(cmd==="get_names")return JSON.stringify({lookup_on:true});`);
}
function alert() {
  const starts = Math.floor(now / 300) * 300 + 300;
  const al = { key: "shugo", id: "shugo_festival", title: "Shugo Festival", body: "Starts in 5 min", emoji: "🎪", kind: "soon", starts, raised: now };
  const a = { enabled: true, movable: false, anchor: "top", banner_secs: 600, size: "normal", keys: { shugo_festival: 21 }, recent: [al] };
  return withMock("toast.html", `if(cmd==="get_alerts")return ${J(a)};`);
}

function shot(name, html, w, h, scale = 2, clear = false) {
  const page = path.join(tmp, name + ".html");
  fs.writeFileSync(page, html);
  const png = path.join(tmp, name + ".png");
  execFileSync(edge, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--allow-file-access-from-files", ...(clear ? ["--default-background-color=00000000"] : []), `--force-device-scale-factor=${scale}`, "--virtual-time-budget=4000", `--window-size=${w},${h}`, `--screenshot=${png}`, "file:///" + page.replace(/\\/g, "/")], { stdio: "ignore" });
  return png;
}

(async () => {
  // Same fonts as aion2.bajubesi.org.
  for (const f of ["orbitron.woff", "rajdhani-medium.woff"]) {
    const r = await fetch("https://aion2.bajubesi.org/fonts/" + f);
    if (!r.ok) throw new Error("font download failed: " + f);
    fs.writeFileSync(path.join(tmp, f), Buffer.from(await r.arrayBuffer()));
  }
  // Window sizes: Classic 520x330, Solo 372x135 at its 1.28 zoom, the alert takes the meter's width.
  const shots = { classic: shot("classic", meter("classic"), 520, 330), solo: shot("solo", meter("solo"), 476, 178), alert: shot("alert", alert(), 520, 118) };
  for (const [k, p] of Object.entries(shots)) fs.copyFileSync(p, path.join(outDir, `shot-${k}.png`));

  const page = fs.readFileSync(path.join(__dirname, "showcase.html"), "utf8").replace(/\{\{version\}\}/g, version);
  fs.copyFileSync(shot("showcase", page, 1280, 680), path.join(outDir, "showcase.png"));

  // Tall picture for the meter section on aion2.bajubesi.org (transparent background).
  const stack = fs.readFileSync(path.join(__dirname, "site.html"), "utf8");
  fs.copyFileSync(shot("site", stack, 460, 690, 2, true), path.join(outDir, "site-meter.png"));
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log(`Rendered README pictures for ${version} into docs/branding`);
})().catch((e) => { console.error(e); process.exit(1); });
