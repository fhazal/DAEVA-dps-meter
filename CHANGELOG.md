# Changelog

## 0.5.0

A new Solo layout, settings with tabs, and a long list of fixes from a day of real dungeon and open-world testing (0.4.5 was never released: its fixes are here too).

- **Solo layout: just you.** A small card the size of a game window, for players who only want their own numbers. On top, the boss with its HP left, the fight clock, the enrage countdown and HP %. Below, your card: class crest, name, combat power, your rank and share in the party, your live DPS in big numbers, and five chips (crit, double, perfect, front and back hits). At the bottom, Odyle, party DPS and ping. The frame (the same thin line as Classic), the frame around your class icon and the share line are in your class colour. Switch with the new panels button on the meter (either layout) or **Ctrl+Shift+O**; each layout keeps its own window size. Click your card for your full breakdown.
- **Party panel.** In a party and not fighting yet (the lobby, the quick-join queue filling up), the meter shows who's there: class, name, 👑 leader, server (short name, Asia servers), gear score and combat power, and in a dungeon room who has clicked **Ready** (a green stripe) and who hasn't (red), with *2/4 ready* on top and the room's title. It follows people joining and leaving, gives way to the DPS table as soon as you fight, and comes back when the line-up changes. In Solo it's one line under your card.
- **Settings with tabs.** General, Layout, Overlay look, Fights, Event alerts, Hotkeys, Daeva bot and Capture down the side, instead of one long page, plus a search box that finds any setting by name. The window opens on the tab you used last. Every setting is still there, just grouped.
- **Layout tab:** pick Classic or Solo, and choose what the Solo card shows (rank and share, the five chips, the bottom line).
- **Players listed: Auto (new default).** The meter follows the game's party list: in a dungeon, a dungeon room or a party at a world boss it lists you and your group; on your own it lists everyone fighting your target. Nothing to switch in Settings. If you had the old default (*Everyone*), you're moved to Auto once; any other choice is kept.
- **On your own, the card shows what you're fighting now:** the last monster you used a skill on, so a mob you turn to next to a big monster shows straight away (your damage-over-time ticks don't pull it back). In a party the card stays on the boss.
- **Farming next to a big monster counts the mobs.** On your own, when you mix a big monster (a Heroic, say) with mobs, most of the damage going into the mobs, the DPS now uses the whole fight; it used to count only the time on the big monster, so your mob damage didn't move the number. Boss fights (in a party, or solo on a boss) keep the boss-fight clock.
- **Other players stop counting when you stop.** A player who helped on your mobs kept being added for the monsters they hit after you'd finished, while your fight's clock stood still, so their DPS shot up.
- **A mob dying isn't a boss kill.** When a mob that took more of your damage than a living boss died, the fight was marked *killed*, which read (and would upload to the DPS boards) as a kill of the boss. Only the boss's own death counts now.
- **No minute-long wait after farming next to a big monster.** A living boss keeps a fight open up to a minute (untargetable phases); on your own that now only applies while the boss is what you last hit, so after killing mobs next to one the fight ends in a few seconds as usual.
- **The events line shows what's happening now:** live events ending within the hour come first (an invasion that just started), then the next to start; events open for hours (Arena of Tactics) go last. Up to three fit.
- **After a boss, the card follows your next monster.** Once a boss died (or, on your own, when you left a boss: no skill of yours on it for 10 s; your damage-over-time ticks don't count), the meter kept adding the monsters you hit next to the boss fight, so the card stayed on the boss for minutes. Hitting a new monster now starts a new fight.
- **Summons stay off the party list.** An Elementalist's spirits could show up as a row of *Player #…* lines (under 0.5% each) in boss fights.
- **"World Boss" only with strangers.** A full dungeon party (five players) was enough to label a boss *World Boss*; it now takes players from outside your party.
- **Names never disappear on a narrow meter.** Extra columns (Crit, Perf, HPS…) now drop one by one, from the right, before a name gets squeezed out, and the boss card leaves out the family rather than running past its edge.
- **Size with the mouse:** hold **Ctrl** and turn the mouse wheel over the meter to make it bigger or smaller (5% steps, remembered). New installs start at 90%.
- **Health above 99% shows a decimal** (*99.7%*), so a big monster you're working on doesn't look stuck at 100%.
- **A finished fight keeps its health bar.** After a fight ended, the card showed the monster's health right now: a training dummy that came back, or a boss that reset after a wipe, showed *100%* next to the old fight time. It now shows the health the monster had when the fight ended.
- **Boss fights count from the pull, even if you arrive late.** When your party started a boss before you got there, the meter ignored their hits until you hit the boss yourself (a rule meant to keep strangers out in the open world), so the start of the fight was missing: in one Phantasmal Lakshmi run the meter began at 89% HP. In a party, hits on a boss or elite by your party now count from the first one the game sends. Other monsters, and bosses when you're not in a party, work as before.
- **Dungeon-room members who aren't Ready yet are seen.** In a dungeon room the game marks each member as Ready or not; members not yet Ready were skipped, so the meter knew only part of the party (and Solo said *#1 of 2*). Assassins are now recognised by their party-list number too.
- **Open-world parties too.** The meter only read the party list the game sends in dungeons, so a party out in the world (two friends at a field boss, say) was never seen. It now also reads the list the game sends whenever you join a party or someone joins or leaves it, anywhere.
- **Leaving a party is noticed**, in the open world and in dungeons. The meter used to keep your old party until the game sent a new list, so former members could still count as your party.
- **Solo shows the monster's rank by colour:** in place of the monster picture, the monster icon in its rank colour (grey Common, blue Elite, orange Heroic, pink-red Legendary/Boss), the same colours as the Classic card.
- **Easier testing and bug reports:** Ctrl+Shift+P saves a report quietly (a soft chime and *Report saved* on the meter) instead of opening the Desktop over the game. Reports now cover the last 5 minutes and include what the meter showed each second and what it noticed: party joins and leaves, zone changes, each fight's start and end, kills and the boss card. A research recording marks those by itself, and **Ctrl+Shift+B** adds *⚑ bug here* with one press, nothing to type (it was Ctrl+Shift+M, which is Discord's mute; moved once for everyone who had it).
- **Clearer hotkey message:** when another program already uses one of the meter's hotkeys (Discord uses Ctrl+Shift+M, for example), Save names it and the Hotkeys tab gets a red dot, instead of *Saved, but some hotkeys failed*.
- **Test alerts check themselves:** *Show a test alert* now says whether the banner really appeared, and where.
- **Alert banners all read the same way:** the event name on top, and the line below says what is happening (*Starts in 5 min* or *Started · runs 30 min*). Long names like *Dimensional Invasion in 5 min* no longer get cut off with "…".
- **Ready stripes like the meter rows:** the party panel's Ready colours (green Ready, red not yet, gold leader) and the open Settings tab now use the same short rounded bar as your row on the meter, instead of a line that bent around the corners.
- **Immortal Gartua banners:** one *Starts in 5 min* banner per spawn, even when the estimate moves a little afterwards (it used to show a second one), and *Spawning now* when it's due instead of *runs 15 min*: a world boss has no set length.
- **Banners fit on one line:** Gartua's line reads *Beritra · estimate* (it was cut off when the banner stands on its own), and long events show hours: Arena of Tactics *runs 3 h* instead of *runs 180 min*.
- **The installer has the Daeva Meter icon** instead of the default setup icon.
- **Window buttons like on a Mac:** red quits, yellow minimises, green switches between Classic and Solo (the symbols show when you point at them). In Classic they're at the top left, in Solo at the end of the bottom line. The status light now sits after the name.

- **Party members named from the first boss:** in dungeons the game often sends a player's name late (minutes into the run) or not at all, so the first bosses showed *Player #6377* instead of your party, and those fights couldn't count for them on the Daeva bot's DPS boards. The party list the game sends as you enter has every member with a number that follows their class; the meter now matches each member to the fighter of that class as soon as they use a skill. When the game does send a name late, earlier fights of the same dungeon are filled in too. It never guesses: two members of the same class (two Gladiators) stay unnamed until the game names them, and strangers in the open world are never taken for party members.
- **Your own name instead of *You*:** if the meter was started after you logged in, the game hadn't told it your character's name, so your row said *You* for the whole session. It now takes your name from the party list (you are the party's only member of your class) and fills it into earlier fights too. In a party with two of your class it still says *You* until the game names you.
- **Dungeon-room members too:** a player who joined through a dungeon room ("Request to Join") was missing from the meter's copy of the party list, which also stopped the naming above. They're read now.
- **The boss card stays on the boss:** in a boss fight the card follows the boss even when your own hits (or a healer's damage-over-time) land on its adds; elite adds no longer take it over. Trash fights still follow what you hit last.
- **Odyle shows 0 when it is 0:** the game leaves a value out of its updates when it's zero, and the meter kept showing the old number (*15 (+35)* while the game showed *0 (+35)*).
- **Back from the Shugo Festival:** the monsters around you can stay *Unknown monster* until your next teleport (the game describes them in a way the meter doesn't read yet). The *Teleport or change zone once* notice now shows in that case too, instead of only before your first zone load.
- **Zone notices only when they help:** the *Teleport or change zone once* notice and the *Teleport away and back to identify* hint now show only while a monster you fight really can't be identified. Entering a dungeon (which the game doesn't announce as a new zone) used to leave the notice up for the whole run until you teleported in the open world; now it goes as soon as you fight monsters the meter knows.
- **New event and Odyle icons:** Spacetime Rift, Shugo Festival, Dimensional Invasion, Artifact Siege, Arena of Tactics and the field bosses (Kaira, Executors, Nahma) have their own icons on the next-events line and the alert banners, and Odyle has a new crystal in the footer. They ship with the meter, so they show even with game art turned off.
- **Your own names.json no longer hides new data:** a `names.json` in the meter's data folder (`%APPDATA%\org.bajubesi.daevameter\definitions`) used to replace the built-in one, so an older copy there hid newer data such as the enrage timers. The built-in names are now always loaded, and your file adds to them (and wins for the entries it has).

## 0.4.4

- **Lighter on your PC:** the overlay still checks for new numbers 4 times a second, but only redraws what actually changed. Idle or between fights it now does almost no work (on a test PC the meter's graphics use fell from about 0.5% to 0.05%), and in fights it redraws about half as much. DPS still counts up just as smoothly.
- **Calmer idle light:** when combat stops, the amber dot dims three times and then rests as a steady glow, instead of pulsing (and redrawing the overlay) for the rest of the session.
- **Lighter alert window:** with no banner on screen it checks for new alerts once a second and follows the meter at a gentler pace. Banners still appear well before each event.
- **Your server stays in the footer:** while idle the footer shows **● Beritra │ Idle · no combat for 30s** instead of replacing your server with the idle message. The server light is now **blue**, so it isn't confused with the green "in combat" dot.
- **Player list header:** a thin line under PLAYER · CRIT · PERF… that fades at both ends, and the figure the list is ranked by (DPS, DMG or DMG %) is shown in gold.
- **⏱ Enrage countdown:** on bosses with a known enrage time, the boss card counts down from your first hit, for example *⏱ Enrage 3:12*. It turns amber in the last minute and shows **🔥 Enraged** once the boss hits harder and faster. Starts with 14 dungeon bosses (most at 5:00; Kaldrix and Terminator Bargott 8:00, Eternal Ludra 10:00). The times are community-reported and may differ by difficulty; hover the countdown to see the rule.
- **Dungeon bosses fully recognised:** dungeon versions of a boss (such as Conquest Vakron and Thamon) have their own monster ids that the monster list doesn't include, so their card showed no picture, type or rank and offered *✎ name it*. The card now takes them from the boss with the same name: portrait, **Natura · Heroic** and the rank colour.

## 0.4.3

- **No more dark box behind event banners:** the banner's shadow was wider than the invisible window it sits in, so it was cut off at the edges and showed as a faint dark rectangle behind the banner over the game. The shadow is now a thin edge that fits inside the window: the banner sits cleanly on the game and still reads well.

## 0.4.2

- **Clearer column labels:** with extra columns turned on (crit, perfect, max and so on), the figures on the right each get their own label, centred over the number: **DPS**, **DMG** and **DMG %** (your share of the group's damage). Before, a single *DPS* label sat at the far right, above the percentage. The header is also a little brighter, so it reads over the game. With *Show healing* on, a **HEAL** column joins them.
- **🌐 World Boss instead of a warning:** at a busy open-world boss, players arrive at different times and the game only sends you the hits of players near you, so joining after the pull showed *⚠ 2.5% of HP counted*, which looked like a fault. With nothing lost, the card now says **🌐 World Boss** (hover for why). *🌐 All your hits seen* stays for bosses watched from full health, and ⚠ stays for real problems.
- **🏆 First record:** your first recorded kill on a boss (at least 30 s on it) now shows *🏆 First record on … · your best to beat next time*. Before, the 🏆 line only appeared when you beat an earlier best.
- **The 🏆 line stays 10 minutes**, through later fights; only a newer record replaces it (or ✕).

## 0.4.1

- **HP check right in the open world:** open-world monsters lose 4.5 times the damage the game shows (your damage numbers are right: the game's own Combat Analysis shows the same). The HP check compared the damage with the health bar as if 1:1, so a solo kill of a side-quest elite read *⚠ 23% of HP counted* although every hit was seen. It now measures how much health each hit really removes and allows for a steady factor: those fights show ✓, and the Beritra air raid boss reads about 94% instead of 21%. Fights where hits really go missing (busy world bosses) still say so. DPS and damage are unchanged.

## 0.4.0

- **Rows glide when someone overtakes:** when the ranking changes mid-fight, players slide to their new place instead of jumping, and the one moving up glows gold for a moment, so you can see who passed whom. Rows that keep their place don't move. Turned off when Windows' *Show animations* setting is off.
- **"Your time" ranks by DPS:** under *Settings → Fights → DPS clock → Your time*, each player's DPS uses their own time in the fight, so a late joiner could show a higher DPS than the row above. The list now ranks by the DPS shown there. With the default clocks nothing changes (damage and DPS order are the same).

## 0.3.9

- **World bosses no longer look like a fault:** at an open-world boss the game only sends you the hits of players near you, so the HP check read something like *⚠ 3.9% of HP counted* even though every one of your own hits was recorded. When the meter watched the boss from (nearly) full health to the kill with no data lost, the card now says **🌐 All your hits seen** instead (hover for why). Dungeon bosses keep *✓ 99.8% of HP counted*, and a real problem (joined late, data lost) still shows ⚠.
- **✅ on the DPS boards for world bosses too:** a fight is verified when the meter recorded the whole kill: seen from (nearly) full health to the kill with nothing lost, or every hit on the boss accounted for. Before, world-boss kills could never be verified. (Unverified fights always counted on the boards; ✅ is only a badge.)
- **"Killed" stays killed:** after a world boss died, a stray hit by another player on a different monster cancelled the kill, so the fight was saved and shared as not killed.
- Lost data during a fight now also counts a self-repair restart (0.3.8), and stays counted when a connection closes.

## 0.3.8

- **The meter recovers by itself when it stops reading the game:** on a server switch (leaving the Beritra air raid, for example) the meter could lose its place in the game's data and stay deaf until restarted: you kept fighting and nothing registered. It already had a self-repair, but your own outgoing messages kept it believing all was well. It now watches only what the server sends: if the server keeps sending but none of it can be read for 15 seconds, the meter restarts reading on its own, so at most about 15 seconds are lost. Reports record how often this happened.
- **More Sorcerer ground skills credited to the Sorcerer:** in some areas (the Beritra air raid, open-world bosses) Bittercold Wind, Fire Wall and Cold Storm arrive in a slightly different form that names the caster by id instead of by name, and still showed as *Player #12345* rows. They now count for the Sorcerer who cast them, including hits that went missing before. (Game definitions 0.6.17.)

## 0.3.7

- **Live numbers:** DPS, damage and share now count smoothly to each new value, like a digital dashboard, instead of jumping. Your DPS glows faintly green while it climbs.
- **DPS clock:** on a boss or elite, DPS now counts the **boss fight**: damage on every enemy (the boss, its adds and objects) from the first hit on the boss to the kill, divided by that time. This is how Details! and ACT count an encounter. Trash and walking before the pull no longer pull your DPS down; before, everything was divided by the whole fight, so it read lower than other meters. Without a boss, DPS uses the whole fight as before. Hover a DPS number to see the rule; **Settings → Fights → DPS clock** also offers *your time* and *whole fight*.
- **Bosses that heal themselves no longer inflate damage:** some bosses (Thamon) heal whenever they're hit, and the game reports that heal like a damage-over-time tick carrying the attacker's skill. The meter counted it as your damage (about 200k of one Thamon kill). It's now ignored, and Thamon matches the game's Combat Analysis within about 1%. (Game definitions 0.6.16.)
- **Untargetable phases pause the clock:** when nobody damages the boss for 10 seconds or more (barriers, untargetable phases, run-backs), that stretch counts neither time nor damage, so a Conquest boss like Vakron isn't dragged down by its barrier phase.
- **Sorcerer ground skills count for the Sorcerer:** Bittercold Wind, Fire Wall and Cold Storm are placed on the ground as objects, and each cast showed up as its own *Player #12345* row (with the Sorcerer's real DPS too low). They're now credited to the Sorcerer who cast them. (Game definitions 0.6.16.)
- **Party members mistaken for monsters:** if a party member's first action the meter saw was an item or basic skill, they could be taken for a monster for the rest of the session: their damage went missing and a Cleric's heals on them counted as damage. They're now recognised as players as soon as the game says so.
- **HP check counts every hit seen:** including hits from before you joined in (which still don't count towards anyone's DPS), so a boss someone else pulled no longer shows ⚠ 96%.
- **No hits after the kill:** a damage-over-time tick that lands after a monster died no longer counts, as in the game's Combat Analysis.
- **HP check:** when a boss or elite dies, its card shows **✓ 99.8% of HP counted**, proof that the meter saw every hit. If hits were missed it says so (**⚠ 92.1%**) instead of quietly showing a lower DPS. Saved fights also record any gaps in the captured traffic.
- **Move event alerts anywhere:** with the meter unlocked, hover a banner (a real one, or *Settings → Event alerts → Show a test alert*, which now stays 30 s) and drag it where you want it. The spot is kept as a share of the screen, so it stays in place at any resolution, and it's remembered per monitor. While the meter is locked (Ctrl+Shift+L) banners stay click-through, so they never catch a click in a fight. The fixed spots (above the meter by default) are still there.
- **Compact banners:** *Banner size → Compact* shows each alert as one slim line. Normal stays the default.
- **Hold banners during fights** (off by default): banners wait until the fight ends; the chime still plays.
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
