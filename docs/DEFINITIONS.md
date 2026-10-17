# Definitions file format

`definitions/aion2.json` describes the wire protocol. The meter loads it at startup. A copy in `%APPDATA%\org.bajubesi.daevameter\definitions\` overrides the bundled one, and **Settings → Reload definitions** applies changes without restarting. `//` line comments are allowed.

```jsonc
{
  "name": "AION 2 (global)",
  "version": "1.2.0",
  "process_names": ["aion2"],          // substring match on the .exe name
  "ports": [],                         // optional fixed server ports
  "framing": { ... },
  "messages": [ ... ]
}
```

## framing

How the TCP byte stream is split into frames.

```jsonc
"framing": {
  "header": ["length:u16le", "opcode:u16le"],  // read in order; must include "length"
  "length_counts": "frame",     // "frame" (whole frame) | "after_length" | "payload"
  "length_adjust": 0,           // added to the length value
  "min_frame": 2,
  "max_frame": 262144,
  "compressed_if": "flags&0x80",// optional: decompress the payload when true
  "compression": "zlib"         // zlib | deflate | lz4 (size-prefixed)
}
```

Header fields can be any of the field types below. Header values (such as `flags`) are visible to expressions in every message.

After capture loss, the framer **resyncs automatically**. It scans for an offset where several frames chain together and the first opcode is a known message.

## messages

```jsonc
{
  "opcode": "0x1a2b",        // number or hex string
  "name": "S_ATTACK_RESULT",
  "dir": "server",           // server | client | any
  "skip": 0,                 // bytes to skip before the fields
  "decompress": false,       // payload is compressed (uses framing.compression)
  "container": false,        // payload is itself a run of frames (bundles)
  "fields": [ ... ],
  "emit": { ... } | [ { ... }, ... ]
}
```

### Field types

Fields can be written compactly (`"name:type"`) or as objects.

| type | meaning |
|---|---|
| `u8` `u16le` `u16be` `u32le` `u32be` `u64le` `i32le` `f32le` | fixed-width numbers |
| `varint` / `svarint` | LEB128 unsigned / zigzag signed |
| `skip` + `len` (compact: `"_:skip4"`) | ignore N bytes |
| `bytes` + `len` (number, field name or `"rest"`) | raw bytes |
| `str` | varint length + UTF-8 |
| `wstr` | u16 length (in characters) + UTF-16LE |
| `wstrz` | null-terminated UTF-16LE |
| `repeat` + `count` (number, field name or `"rest"`) + `fields` | list of records |

Optional fields: `{"name": "pet_owner", "type": "varint", "if": "flags&0x10"}`.

### Expressions

These are used in `if`, `when` and `emit` values: `field`, `field&0x4`, `field>>8&0xff`, `field==3`, `field!=0`, `field+1`, `field*10`, numbers (`12`, `0x1f`) and strings (`'player'`). Lookups search the current repeat item, then the message, then the frame header.

### emit

This maps decoded fields to meter events. `each` runs the emit once for every item in a repeat list, and `when` filters items.

| event | keys |
|---|---|
| `damage` | `src` `tgt` `skill` `amount` + flags `crit` `back` `perfect` `parry` `block` `evade` `dot` `resist` (each truthy → flag), or raw `flags` |
| `heal` | `src` `tgt` `skill` `amount` `crit` |
| `death` | `target` `killer` |
| `actor` | `id` `name` `class` `kind` (`'player'`/`'npc'`/`'summon'` or 1/2/3) `npc_id` `owner` `max_hp` |
| `self` | `id` `name`: the local player's character |
| `hp` | `id` `hp` `max_hp` |
| `zone` | `zone` (also ends the current fight) |
| `loot` | `who` `item` `grade` `count` |
| `buff` | `src` `tgt` `buff` `applied` `duration_ms` |

## names.json

```json
{
  "classes": {"1": "Gladiator"},
  "skills":  {"1101": "Ferocious Strike"},
  "npcs":    {"2400001": "Training Golem"},
  "zones":   {"110010000": "Sanctum"},
  "grades":  {"4": "Eternal"},
  "items":   {"100000901": {"name": "Some Greatsword", "grade": 4}}
}
```

Skill lookups fall back from the exact ID to `id/10`, `id/100`, `id/1000` and `id/10000`, so you can name a skill once for all of its ranks.
