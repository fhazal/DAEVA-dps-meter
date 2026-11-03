<!-- Short release notes the Daeva Discord bot posts when a release is published.
     Update "version" and the lists with each release. -->
version: 0.6.2
tagline: Better pet tracking when you farm several pets at once.

## New
- 🐾 **Pet strip for several pets:** when a pet gets a +1, its strip shows for a few seconds in its own colour, even while the card is on a monster without a levelling pet. A pet not matched to a monster yet shows as "Pet Insight"
- 🧩 **Pets learned in mixed packs:** when different monsters die together, the meter works out which monster belongs to which pet over the next few kills

## Fixed
- 🥚 **Hatched monsters named:** monsters that hatch from eggs (Contaminated Swarm, Giant Ore Insectoid) show their names instead of "Unknown monster"
- 🎒 **Looting by hand:** without the auto-loot pet ticket, maxed pets are now learned right. A levelling monster no longer shows MAX, and a late +1 no longer jumps to another monster
- 👥 **Calmer party queue bar:** strangers joining and getting kicked from a filling room no longer make the bar jump 4/5 ↔ 5/5 every second
