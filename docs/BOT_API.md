# Daeva bot ingest API

When a player turns on **Settings → Share with Daeva bot**, the meter `POST`s JSON to the configured endpoint. This is the contract the Daeva bot and portal implement. It feeds the "Top DPS today" and "Most active players" cards and the rare-loot announcements.

## Request

```
POST <endpoint>
Content-Type: application/json
Authorization: Bearer <token>        (if the player set one)
```

The meter sends from its WebView, whose origin is `http://tauri.localhost`, so the endpoint must answer CORS preflight:

```
Access-Control-Allow-Origin: http://tauri.localhost
Access-Control-Allow-Methods: POST, OPTIONS
Access-Control-Allow-Headers: Content-Type, Authorization
```

Return any `2xx` on success. A network error or `5xx`/`408`/`429` is retried up to 5 times, 3 seconds apart, and then dropped; any other `4xx` is dropped straight away.

The Daeva bot lives at `https://aion2.bajubesi.org/api/meter`, which is the meter's default endpoint. Members get a token with `/meter link` in Discord.

## Bodies

### Fight

```json
{
  "type": "fight",
  "meter": "daeva-meter",
  "meter_version": "0.1.0",
  "demo": false,
  "record": {
    "format": "daeva-meter/fight@1",
    "fight_id": 12,
    "started_unix": 1760001234.5,
    "duration": 77.25,
    "clock": "boss",
    "clock_secs": 71.5,
    "clock_target": 90003,
    "clock_npc": 2400419,
    "clock_target_name": "Frost Wyrm Matriarch",
    "clock_counted": 0.998,
    "clock_watched": true,
    "gaps": 0,
    "total": 1563000,
    "dps": 20233,
    "boss_killed": true,
    "zone": "Zone 110010000",
    "self_name": "DewaPutih",
    "target": {"id": 90003, "name": "Frost Wyrm Matriarch", "dmg_taken": 1563000, "hp_pct": 0, "max_hp": 1563000},
    "players": [
      {"id": 1001, "name": "DewaPutih", "class": "Gladiator", "is_self": true,
       "dmg": 341800, "dps": 4410, "share": 0.219, "hits": 77, "crit_rate": 0.34,
       "back_rate": 0.12, "perfect_rate": 0.05, "max_hit": 12300, "max_hit_skill": "Shockwave",
       "heal": 0, "hps": 0, "taken": 2690, "deaths": 0}
    ],
    "details": [
      {"id": 1001, "name": "DewaPutih", "class": "Gladiator", "dmg": 341800, "dps": 4410,
       "active_time": 75.0, "hits": 77, "max_hit": 12300, "heal": 0, "taken": 2690,
       "skills": [{"id": 11051, "name": "Shockwave", "dmg": 112000, "dps": 1450, "share": 0.33,
                   "hits": 14, "crit_rate": 0.5, "back_rate": 0.0, "perfect_rate": 0.0,
                   "avg": 8000, "max": 12300, "min": 5200, "dot_hits": 0}],
       "targets": [{"id": 90003, "name": "Frost Wyrm Matriarch", "dmg": 341800}],
       "timeline": [0, 5120, 3980]}
    ],
    "loot": [],
    "deaths": [{"t": 41.2, "target": "Mirelle", "killer": "Frost Wyrm Matriarch"}]
  }
}
```

With scope **Only me** (the default), `players` and `details` contain only the sender's row, and `deaths` is left out. With **Everyone in the fight**, all rows are included.

`timeline` holds damage per second from the start of the fight.

### Connection test

**Settings → Share with Daeva bot → Test connection** sends:

```json
{"type": "ping", "meter": "daeva-meter"}
```

`200` means the token works; `401` means it's unknown.

### Loot

```json
{
  "type": "loot",
  "meter": "daeva-meter",
  "meter_version": "0.1.0",
  "demo": false,
  "record": {"unix": 1760001300.1, "who": "DewaPutih", "is_self": true,
             "item_id": 100000901, "item": "Some Greatsword", "grade": 4,
             "grade_name": "Eternal", "count": 1}
}
```

Loot is only sent when the player enables it and the item meets their minimum grade.

`clock` is the DPS clock: `"boss"` (boss fight), `"personal"` (each player's own first-to-last hit) or `"fight"` (whole fight; also used when no boss or elite is involved). `clock_secs` is the time DPS was divided by.

## What the Daeva bot counts

- Real fights only: `demo: true` is ignored.
- Fights of 20 s to 30 min against a **boss or elite** monster. The bot looks up `target.npc_id` in its own gaming.tools data; Common or unknown monsters don't count.
- DPS is recomputed as damage ÷ duration. From meter 0.3.7, when `clock` is `"boss"`, players' `dmg` is their damage on all enemies during the boss fight (first hit on the boss → kill, adds included) and the bot divides by `clock_secs` and ranks the fight under `clock_npc` / `clock_target_name`. Damage far above `target.max_hp` is refused (`422`).
- **Verified ✓**: the boss was killed, `gaps` (dropped packets, decoder resyncs or stall restarts during the fight) is 0, and either `clock_counted` (damage seen on the boss ÷ its max HP) is at least 0.97 or `clock_watched` is true (meter 0.3.9+: the meter's first health reading of the boss was at least 95% of its max HP, so it saw the whole kill). The second covers open-world bosses, where the game only sends the hits of players near you.
- Each character belongs to one Discord member (`409` if someone else uploads it).
- With "Everyone in the fight", other players appear only if they have linked their own meter.
- Several members uploading the same kill (same target, start within 15 s) count once.

Responses: `200 {"ok":true,"counted":n}`, `202 {"ok":true,"counted":0,"ignored":"reason"}`, or `4xx {"ok":false,"error":"…"}`.

## Suggested bot behaviour

- **Ignore `demo: true`**, or keep it in a test channel.
- **De-duplicate party fights:** several members may upload the same kill. Key on `(target.name, round(started_unix / 10))` plus the player set.
- **Daily cards:** highest single-fight DPS per class, total damage, number of fights (most active), and boss kills.
- **Tokens:** issue one per Discord user with a `/meter link` command, so uploads map to members and can be revoked.
