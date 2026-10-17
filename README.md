# Daeva Meter

An open, community-run damage meter for **AION 2**, from Bajubesi Network.

It reads the game's network traffic with Npcap and shows live DPS, damage share, crits and per-skill breakdowns in a small overlay that stays on top of the game. It **never touches the game client**: no injection, no memory reading and no packets sent.

> **Status: Phase 1.** The meter, overlay, history, research tools and demo mode are complete. The AION 2 packet definitions (`definitions/aion2.json`) are **not decoded yet**, so until Phase 2 the meter can't show real fights. Use demo mode to try the overlay, and Research mode to record captures for decoding.

> **Fair warning:** NCSoft's terms of service generally prohibit third-party tools. Passive packet readers are hard to detect, but they are not sanctioned. Use it at your own risk.

## Features

- Live overlay that stays on top, with click-through, adjustable opacity and scale, and class colours. It shows rank, DPS or damage, % share, the target's HP and "killed" status.
- Each fight starts automatically and ends when you stop dealing damage or the main target dies. Summon and pet damage counts for its owner.
- **Details** window for each player: DPS timeline, plus a per-skill table with damage, share, hits, crit %, back-attack %, average and max.
- **History** window: this session's fights plus saved fights on disk (kept for 30 days by default).
- **Copy summary**: a Discord-ready table on the clipboard, from a button or `Ctrl+Shift+C`.
- **Global hotkeys** that work while the game has focus. All of them can be rebound.
- **Privacy mode** hides other players' names for streams and screenshots.
- **Share with the Daeva bot** (optional, off by default) sends finished fights and rare loot to your community's bot for leaderboards and Discord cards. See [docs/BOT_API.md](docs/BOT_API.md).
- **Research mode** records game-server traffic, with timestamped markers, for protocol decoding.
- **Definitions-driven decoder**: the protocol lives in a JSON file, so after a game patch you replace that file instead of the app. See [docs/DEFINITIONS.md](docs/DEFINITIONS.md).

### Default hotkeys

| Action | Keys |
|---|---|
| Toggle click-through | `Ctrl+Shift+L` |
| End current fight | `Ctrl+Shift+R` |
| Hide / show overlay | `Ctrl+Shift+H` |
| Copy summary | `Ctrl+Shift+C` |
| Research marker | `Ctrl+Shift+M` |
| Report a problem (saves a zip to the Desktop) | `Ctrl+Shift+P` |

## When something looks wrong

Press **Ctrl+Shift+P** (or Settings → Report a problem). The meter saves `DaevaMeter-report-<time>.zip` on your Desktop: the last 3 minutes of game-server traffic plus what the meter showed. Nothing else on your PC is included. If the meter notices that game traffic has stopped decoding (typical after a game patch), a yellow warning appears at the bottom of the overlay; click it to save the same report.

## Installing (players)

1. Install **Npcap** from <https://npcap.com/#download> with the default options. Leave "Restrict Npcap driver's access to Administrators only" **unticked**. Otherwise you'll have to run the meter as administrator.
2. Run `Daeva Meter_x.y.z_x64-setup.exe`. The installer isn't code-signed yet, so Windows SmartScreen may warn you. Choose **More info → Run anyway**.
3. Start AION 2, then Daeva Meter. The dot in the top-left turns green while it's capturing.

The game must run in **borderless/windowed** mode for the overlay to show above it.

## Building

The project has two parts:

| Path | What it is | Builds on |
|---|---|---|
| `crates/meter-core` | Capture parsing, TCP reassembly, protocol decoder, combat engine. No dependencies. | any OS |
| `crates/research` | `daeva-research` CLI for decoding captures | any OS |
| `app/` | Windows app (Tauri 2 + WebView2), with the overlay UI in `app/ui` | Windows |

**Automatic:** push to GitHub and the included workflow (`.github/workflows/build.yml`) tests the core, builds the Windows installer and `daeva-research.exe`, and uploads them as build artifacts. Push a tag such as `v0.1.0` to publish a GitHub Release.

**On your PC** (Windows 10/11):

```powershell
# one-time: Rust (https://rustup.rs) and Node.js 20+
cargo test --workspace                 # core + research
cd app
npx @tauri-apps/cli@2 dev              # run with hot reload
npx @tauri-apps/cli@2 build            # installer -> app/src-tauri/target/release/bundle/nsis/
```

## Naming monsters and skills

The game sends monster template numbers, never names (the client reads names from its own files). So the meter builds its own name list, typed in once by a player:

- An unnamed target shows **✎ name it** on the overlay. Type the name from the in-game target frame and press Enter.
- Unnamed skills show **✎** in the Details window.
- **Settings → Monster & skill names** lists everything met without a name (with max HP, zone and when it was seen) and everything already named.

Unknown monsters and skills are first looked up automatically on the [AION 2 Global Database](https://aion2.gaming.tools) (aion2.gaming.tools), whose ids match the game's, with the kind permission of its team: one page per unknown id, at most one request every 3 seconds, cached in `gaming-tools-names.json`. The 🔎 buttons open the matching page. It can be turned off in Settings.

Names you type are saved in `my-names.json` in the data folder. They take effect at once, fix older fights, survive updates, and override `names.json`. One skill name covers every rank and variant of that skill. This file is the start of the community's own database.

## Where things live on a player's PC

`%APPDATA%\org.bajubesi.daevameter\`

- `settings.json`
- `my-names.json`: monster and skill names you gave (your own database; keep it)
- `gaming-tools-names.json`: names fetched from aion2.gaming.tools (cache)
- `unnamed.json`: monsters and skills met without a name, waiting to be named
- `history\fight-*.json`: one file per finished fight (the same format the bot receives)
- `research\capture-*.pcap` and `*.markers.json`
- `definitions\`: put an updated `aion2.json` or `names.json` here to override the bundled copy

## Roadmap

- **Phase 2:** decode AION 2 framing, damage, actors and names, skills, deaths, loot and zones from research captures. Build the `names.json` skill/NPC tables.
- **Phase 3:** buff uptime, a death recap, a boss timeline, field-boss alerts, auto-update, and the Daeva bot API and portal pages.

## Credits

Monster and skill names: [AION 2 Global Database](https://aion2.gaming.tools) by gaming.tools, used with permission. AION 2 and its names belong to NCSOFT.

## Licence

MIT. Not affiliated with or endorsed by NCSoft. AION is a trademark of NCSoft Corporation.
