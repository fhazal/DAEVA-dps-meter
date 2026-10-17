# Decoding the AION 2 protocol (Phase 2)

This is the workflow for turning research captures into `definitions/aion2.json`.

## 1. Record good captures (player)

In the meter, open the **flask icon (Research mode)**:

1. Stand at a training dummy. Use borderless/windowed mode so the overlay stays visible.
2. Name the session (e.g. `gladiator-dummy`) and press **Start recording**.
3. **Before** each action, add a marker with what you're about to do. Afterwards, add the **exact number** the game showed. For example:
   - `skill 1 — Ferocious Strike 12,345`
   - `crit — Ferocious Strike 21,880 crit`
   - `auto attack 1,204`
   - `took damage 3,410 from dummy`, `heal 5,600 on self`
   - `loot picked up — <item name>`, `zone change`
4. Record 3–5 of each action. Slow and clean is better than fast and messy.
5. Press **Stop & save**, then **Open folder**, and send the `.pcap` and `.markers.json` files.

Only traffic to and from the detected game server is recorded. MAC addresses are stripped, and nothing from your browser or other apps is included.

Useful extra captures:
- **Login to character select to world**: names, self ID and classes.
- **Party dungeon run**: multiple players, summons, deaths.
- **Field boss**: HP updates and the boss death.

## 2. Decode (developer)

Build the toolkit (`cargo build --release -p research`) or use `daeva-research.exe` from the CI artifacts.

```text
daeva-research flows   cap.pcap                       # find the game flow (biggest non-443)
daeva-research dump    cap.pcap --flow 0 --limit 2048 # eyeball the start of the stream
daeva-research framing cap.pcap --flow 0              # rank candidate header layouts
```

Paste the best `framing` into `aion2.json`, then:

```text
daeva-research ops    cap.pcap --defs aion2.json                     # opcode histogram
daeva-research near   cap.pcap --defs aion2.json --markers cap.markers.json
daeva-research search cap.pcap --value 12345 --value 21880 --markers cap.markers.json
daeva-research frames cap.pcap --defs aion2.json --op 0x1a2b --markers cap.markers.json
```

- `near` lists the opcodes that spike right after each marker. Damage packets stand out quickly.
- `search` finds a damage number in every common encoding (u16/u32/u64 in both byte orders, f32, varint, zigzag) and shows the bytes around it plus the nearest marker. The encoding that matches tells you the field type. The bytes around it usually include the source and target IDs and the skill ID.
- `frames` dumps every frame of one opcode, with its decoded fields once you've described them.

If `search` finds nothing anywhere, the payload is compressed or encrypted. Look for a header flag that toggles on large frames, and try `compressed_if` with `zlib` or `lz4`.

Iterate: describe a message in `aion2.json`, re-run `frames`, compare the values with the markers, and repeat. When damage matches the in-game numbers:

```text
daeva-research decode cap.pcap --defs aion2.json --names names.json --out fights.json
```

This runs the entire meter offline and prints the same fight summaries the overlay would show.

## 3. Ship it

Commit `definitions/aion2.json` and `names.json`. Players get the update either from a new installer or by dropping the file into their definitions folder and pressing **Reload definitions**.
