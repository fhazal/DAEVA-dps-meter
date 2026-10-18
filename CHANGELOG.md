# Changelog

## 0.3.2

- **New app icon:** Daeva wings around a gold crystal, on the meter's navy tile. A bolder version of the same drawing is used at 16–24 px so it stays clear in the taskbar and title bar. (Windows may show the old icon on a desktop shortcut until the next restart.)

## 0.3.1

- **Update notice:** when a newer Daeva Meter is published, a line under the meter says so; **Download** opens the release page, ✕ hides that version. Settings → Data → Updates shows your version and has *Check now*. The meter asks GitHub for the latest release at start-up and every 6 hours; nothing about you is sent, and it never downloads or installs anything by itself.
- **New icons:** every button now uses the [Lucide](https://lucide.dev) icon set for a consistent look; *End fight* has a clearer stop icon (it looked like History before).

## 0.3.0 — first public release

The first public build of Daeva Meter for AION 2 (Windows 10/11).

**Damage meter**
- Live DPS, damage and share for you and your party, with skill breakdowns (crit, back, perfect, double, multi-hit, parry) and fight history.
- Summons and spirits are counted under the player who summoned them.
- "Players listed" filter: everyone, your party, or only you.

**Target card**
- Monster name, health (HP / max · %), "Kill in" estimate and "Killed in" time.
- Monster type (Cogni, Natura, Fera, Varian, Special) and rank colour, using data from the AION 2 Global Database by gaming.tools (with permission).

**Your character**
- Odyle energy and Shugo Festival keys under the meter, remembered between sessions.
- Your name and server read from the game; switching characters is handled.

**Events**
- Banners and sounds before AION 2 events, with the next events shown under the meter (same schedule as the Daeva Discord bot).

**Overlay**
- Lock (Ctrl+Shift+L) makes the meter click-through and immovable; only the lock button stays clickable.
- Status light: green while fighting, pulsing amber when idle, steady amber when there's no game traffic.
- Ping in the footer (green under 100 ms, amber to 179 ms, red from 180 ms). Works through tunnels such as Mudfish.
- "Teleport or change zone once" notice when the meter was started mid-zone.
- Report a problem (Ctrl+Shift+P) saves the last 3 minutes of game traffic for bug reports. Research tools for testers are under Settings → Data → Show research tools.

**Known limitations**
- Monsters that were already near you when the meter started show as *Unknown monster* until you teleport or change zone.
- On a brand-new install, monster pictures, types and ranks can take a minute or two to appear while the monster data downloads.
- After a game patch the meter may need an updated definitions file before it reads damage again.
