# A2Tools monster and skill names (GPL-3.0)

`names.json` holds monster and skill names (id → name) taken from
[A2Tools](https://github.com/taengu/A2Tools-DPS-Meter) by taengu
(`src/data/i18n/npcs/en.json` and `src/data/i18n/skills/en.json`, commit
`2555901, 25 Oct 2026`). It only contains the ids the AION 2 Global Database (gaming.tools)
doesn't name, such as zone and dungeon variants (Vakron Sky Island 23008xx,
open-world 24009xx) and newer class skills (Upward Strike, Piercing Strike,
Elementalist spirit attacks). Internal names (test, unused, `<DNT>`) are left out.

This folder is licensed under the **GNU General Public License v3** (see
[LICENSE](LICENSE)), like A2Tools itself. The rest of Daeva Meter stays under
the MIT licence. Daeva Meter reads this file as data; gaming.tools and our own
`definitions/names.json` win where they also have a name.
