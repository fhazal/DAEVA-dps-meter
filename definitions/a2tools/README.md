# A2Tools monster and skill names

`names.json` holds monster and skill names (id → name) taken from
[A2Tools](https://github.com/taengu/A2Tools-DPS-Meter) by taengu
(`src/data/i18n/npcs/en.json` and `src/data/i18n/skills/en.json`, commit
`2555901, 25 Oct 2026`). It only contains the ids the AION 2 Global Database (gaming.tools)
doesn't name, such as zone and dungeon variants (Vakron Sky Island 23008xx,
open-world 24009xx) and newer class skills (Upward Strike, Piercing Strike,
Elementalist spirit attacks). Internal names (test, unused, `<DNT>`) are left out.

Used with taengu's permission under **MIT terms**, like the rest of Daeva Meter
([A2Tools issue #118](https://github.com/taengu/A2Tools-DPS-Meter/issues/118)).
The names themselves are NCSOFT's game text; taengu's work is the compilation
(merging the CN/KR and Global data, and the corrections). Thank you, taengu!
Names we find missing or wrong go back to A2Tools as an issue or PR.
Daeva Meter reads this file as data; gaming.tools and our own
`definitions/names.json` win where they also have a name.
