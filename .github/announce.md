<!-- Short release notes the Daeva Discord bot posts when a release is published.
     Update "version" and the lists with each release. -->
version: 0.3.8
tagline: A reliability update: the meter now recovers on its own if it loses track of the game, and Sorcerer ground skills count for the Sorcerer everywhere.

## New
- 🔄 **Self-repair**: if the meter stops reading the game (e.g. after leaving an event), it recovers by itself within about 15 seconds, no restart needed

## Fixed
- Damage not registering after leaving the Beritra air raid until the meter was restarted
- Sorcerer Bittercold Wind, Fire Wall and Cold Storm showing as "Player #" rows in the air raid and at open-world bosses
