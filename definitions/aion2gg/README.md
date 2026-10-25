# aion2.gg boss data

**[Boss data](https://aion2.gg)** by aion2.gg, shared with Daeva Meter by the
aion2.gg admin, who allows a copy in the meter as long as this credit and link
stay with it.

- `boss-fights.json`: boss id → name, stagger seconds and enrage seconds.
  Daeva Meter uses the enrage times for its Frenzy countdown where our own
  community list (`definitions/names.json`, "berserk") has no entry; ours wins
  where both list a boss.
- `boss-skills.json`: boss skill id → name. Stored for later; not shown yet.

This copy is only a fallback for the first start. The meter downloads the
current files from the Daeva server (aion2.bajubesi.org/api/aion2gg/) once a
day, and a downloaded copy replaces this one.

Please don't reuse these files outside Daeva Meter: ask aion2.gg.
