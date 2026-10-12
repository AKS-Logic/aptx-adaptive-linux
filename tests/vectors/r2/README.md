# aptX Adaptive R2.x encoder vectors

Output of the reference encoder, driven the way a reference source drives it (see `spec/a2dp-integration.md` §5–§6).

## inputs (`*.s32le`)

- 48000 frames (1.000 s), 2 channels, interleaved L,R.
- Each sample is a little-endian signed 32-bit integer holding a **24-bit value, right-justified and sign-extended** (range −8388608..8388607). This is the encoder's input format. A source that has Q31 samples gets it by an arithmetic shift right by 8.

| file | content |
|---|---|
| `silence.s32le` | all zeros |
| `sine1k.s32le` | L = R = round(4194303 · sin(2π·1000·n/48000)), so −6 dBFS, n = 0..47999 |
| `noise.s32le` | 32-bit LCG `s = s·1664525 + 1013904223 (mod 2^32)` with seed s₀ = 1. For each frame, L is taken first and then R. Each sample is `(int32)s >> 8` (arithmetic shift), using the state *after* the update. |

## encoder parameters (all files)

- create parameters: profile 3 (HQ), secondary profile 0, channel config 4 (stereo), sample rate 48000, bitrate 364000 bps, flag 1 (spec §3.1).
- decoder compatibility level, given by the file name:
  - `*.r1.*`: level **R1** ("1.1 sink"). This is what the B&W Px7 S2e needs.
  - `*.r2.*`: level **R2** (R2.x sink).
  - The encoder's config id is **0** at R1 and **3** at R2 (spec §3.2).
- packet timing (HQ table index 0, level 3): period 14 ms, 672 samples per channel per packet (1344 interleaved), packet size 656 bytes. These are the defaults for an R1 sink with no QHS.
- input was fed in blocks of 672 frames. The output is byte-identical with 48-frame (1 ms) blocks.

## packet loop

The encoder takes any amount of input, consumes what it can, and either writes one packet or reports that it needs more input. A flush call with no input forces out the packet currently being built.

The loop keeps a **consumed** counter and a carried **deficit**, both in interleaved samples, both starting at 0. For each input block:

1. Pass all buffered input to the encoder. The output window is 656 bytes, with 64 bytes of slack after it. Keep any unconsumed input for the next call, and add the samples consumed to the consumed counter.
2. A packet is due once consumed ≥ 1344 + deficit. If no packet is due and the encoder produced nothing, go on to the next block.
3. If a packet is due but the encoder has not produced one yet, make a flush call to force it out. Every packet in these vectors is forced this way, except packet 0 of `noise.r2`.
4. Once a packet is out, set deficit = deficit + 1344 − consumed and reset consumed to 0.
5. Set the bitrate for the next packet: 364000 − 364000/4 if the deficit is above 96, otherwise 364000. In these vectors the deficit never goes above 96.

## outputs

- `*.bin`: the raw codec packets, concatenated in order, exactly as the encoder wrote them.
  - There is **no** 8-byte header in them.
  - There is **no** 16-bit byte swap applied.
  - Every packet is exactly 656 bytes. That gives 71 packets per 1 s of input, 46576 bytes total.
  - R1 packets begin `00 81 a1 d?`; R2 packets begin `00 83 a1 d?`.
- `*.sizes`: one line per packet, `index bytes forced`. `forced = 1` means the packet came out through the flush call.

To get what goes to the Bluetooth side, take each 656-byte packet, swap the bytes in every 16-bit word, and put the 8-byte header from spec §7 in front of it (`TT TT 38 01 00 00 00 AE` for this configuration). RTP framing is separate (spec §7.2).
