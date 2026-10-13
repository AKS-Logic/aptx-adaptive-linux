# Mid-stream quality-level change vectors

Output of the reference encoder driven with the parity packet loop (`tests/vectors/parity/README.md`, ll = 0) plus the level step from `spec/a2dp-integration.md` §7L.

All runs use the same setup:

- R1 sink, create profile 3, channel config 4 and create flag 1;
- HQ table index 0, starting at level 3 (364000 bps, 14 ms);
- a 656-byte packet window at every level.

The input is `tests/vectors/parity/mix48000.s32le` or `mix44100.s32le`.

| name | rate | chunk (frames) | 1→2 mapping | schedule `packet:level` | packets |
|---|---|---|---|---|---|
| `lv_48k_ref` | 48000 | 48 | on | `10:2,20:1,30:2,40:3,50:4` | 70 |
| `lv_48k_raw1` | 48000 | 48 | off (level 1 row used) | `10:2,20:1,30:2,40:3,50:4` | 68 |
| `lv_44k_ref` | 44100 | 44 | on | `10:2,20:1,30:2,40:3,50:4` | 70 |
| `lv_48k_fast` | 48000 | 48 | on | `1:4,2:3,3:2,4:3,5:4,6:2,7:4,9:3,11:2,12:4,13:3` | 71 |

`k:L` means the level request is applied between packet k−1 and packet k, so packet k is the first packet produced after the change. With the 1→2 mapping on, a request for 1 is treated as 2, as in the reference.

## files

- `<name>.bin`: raw codec packets, concatenated, each 656 bytes, with no RTP header and no 16-bit swap.
- `<name>.sizes`: one line per packet, `index bytes forced level P deficit`. P and the deficit are in interleaved samples, and the deficit is the value **after** this packet's update.

## loop step (per packet, after it is emitted)

1. `deficit = deficit + P − consumed`, `consumed = 0`.
2. If a level request is pending, look up the row for the new level:
   - if the encoder's current bitrate (the last value given to the set-bitrate call) differs from the row bitrate, set the bitrate to the row bitrate and switch P to the new row's period;
   - otherwise change nothing; P stays the same.
3. Set the bitrate to `deficit > 96 ? row − row/4 : row` (integer division), as in every packet.

A level change doesn't reset the deficit or any encoder state.
