# Changelog

## 0.3.7

- **Live numbers:** DPS, damage and share now count smoothly to each new value, like a digital dashboard, instead of jumping. Your DPS glows faintly green while it climbs.
- **DPS clock:** on a boss or elite, DPS now counts the **boss fight**: damage on every enemy (the boss, its adds and objects) from the first hit on the boss to the kill, divided by that time. This is how Details! and ACT count an encounter. Trash and walking before the pull no longer pull your DPS down; before, everything was divided by the whole fight, so it read lower than other meters. Without a boss, DPS uses the whole fight as before. Hover a DPS number to see the rule; **Settings → Fights → DPS clock** also offers *your time* and *whole fight*.
- **Bosses that heal themselves no longer inflate damage:** some bosses (Thamon) heal whenever they're hit, and the game reports that heal like a damage-over-time tick carrying the attacker's skill. The meter counted it as your damage (about 200k of one Thamon kill). It's now ignored, and Thamon matches the game's Combat Analysis within about 1%. (Game definitions 0.6.16.)
- **Untargetable phases pause the clock:** when nobody damages the boss for 10 seconds or more (barriers, untargetable phases, run-backs), that stretch counts neither time nor damage, so a Conquest boss like Vakron isn't dragged down by its barrier phase.
- **Sorcerer ground skills count for the Sorcerer:** Bittercold Wind, Fire Wall and Cold Storm are placed on the ground as objects, and each cast showed up as its own *Player #12345* row (with the Sorcerer's real DPS too low). They're now credited to the Sorcerer who cast them. (Game definitions 0.6.16.)
- **No hits after the kill:** a damage-over-time tick that lands after a monster died no longer counts, as in the game's Combat Analysis.
- **HP check:** when a boss or elite dies, its card shows **✓ 99.8% of HP counted**, proof that the meter saw every hit. If hits were missed it says so (**⚠ 92.1%**) instead of quietly showing a lower DPS. Saved fights also record any gaps in the captured traffic.
- **Extra columns:** choose up to 3 of Crit %, Back %, Perfect %, Hits, Max hit, Healing/s, Damage taken and Deaths in **Settings → Overlay**. They appear with a small header row.
- **Personal bests:** beat your best DPS on a boss (kills of 30 s or more) and a 🏆 line appears under the meter. History has a **You** column with 🏆 on your record fights, and each fight shows *Your best on this boss*.
- The copied Discord summary and the player detail window use the same clock as the overlay.

## 0.3.6

- **Boss objects no longer show as damage dealers:** objects a boss places, such as Faithful Rajit's **Recovery Totem**, used to get their own row in the meter (and lowered everyone's share %). The meter now knows they belong to the boss and leaves them out. (Game definitions 0.6.15.)

## 0.3.5

- **Unknown monster:** the boss card now always says **📍 Teleport away and back to identify** (hover for why). The game only names a monster when it first comes into range, and big bosses are announced from far away, so a meter started near a boss can't know which one it is until you leave the region and return.
- **Boss cards on a fresh install:** while the monster list (type, family and picture) downloads for the first time, the card says **Loading monster data…** and then fills in by itself, with no restart. A failed first download is retried after a minute instead of 30.
- **Faster start:** the saved monster list is used straight away at launch, even when it's due for its daily refresh. Before, cards could miss their type and picture for up to a minute while the refresh ran.
- **Settings → Data → Names** shows the monster data state: how many monsters and when it was updated, *downloading…*, or the error.
- **Test connection** results are now coloured: a green ✓ when your token works (with an amber reminder to Save), a red ✕ with advice when it doesn't. The `/meter link` hint is in gold.

## 0.3.4

- **DPS boards:** sharing now goes to the Daeva bot at aion2.bajubesi.org out of the box. Run `/meter link` in our Discord, paste the token in **Settings → Share with Daeva bot**, turn sharing on, and your boss and elite fights appear in *Highest DPS today* and *Class DPS today* on the site and in the daily Discord recap. Sharing stays off until you turn it on.
- **Test connection** button next to the token tells you straight away whether it works.
- Uploads the bot refuses (for example an old token) are no longer retried five times.

## 0.3.3

- Status light: a freshly opened meter now shows **idle** (pulsing amber) until your first fight, instead of "in combat" for the first 30 seconds.

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
