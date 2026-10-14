# aptX Adaptive parity vectors

Output of the reference encoder, driven with the reference source's per-packet loop (described below). These cover the table rows, sample rates and configs that `tests/vectors/r2/` does not. What each run checks is in `spec/codec-core.md` §10.

All 44 runs are listed in `runs.txt`, one per line, with the fields `name level profile rate bitrate period_ms pkt_bytes chunk_frames ll_loop`.

## inputs (`mix<rate>.s32le`)

- 1.000 s at the given rate: `mix44100.s32le` (44100 frames), `mix48000.s32le` (48000 frames), `mix96000.s32le` (96000 frames).
- 2 channels, interleaved L,R. Each sample is a little-endian int32 holding a **24-bit value, right-justified and sign-extended**, as in `tests/vectors/r2/`.
- Content, with n = number of frames, fs = sample rate and t = frame index:

| frames | L | R |
|---|---|---|
| 0 ≤ t < ⌊0.4·n⌋ | noise | noise |
| ⌊0.4·n⌋ ≤ t < ⌊0.7·n⌋, with u = t − ⌊0.4·n⌋ | round(4194303·sin(2π·1000·u/fs)) | round(8388607·(0.25·sin(2π·1000·u/fs) + 0.125·sin(2π·7000·u/fs))), plus 0.125·sin(2π·30000·u/fs) inside the bracket at 96 kHz only |
| ⌊0.7·n⌋ ≤ t < ⌊0.85·n⌋ | 0 | 0 |
| ⌊0.85·n⌋ ≤ t < n | quiet noise | 0 |

- **Noise:** the 32-bit LCG `s = s·1664525 + 1013904223 (mod 2^32)`, seed s₀ = 1, one state shared by the whole file. Each sample uses the state after its update.
  - First segment: L then R each take one update, and the sample is `(int32)s >> 8` (arithmetic shift).
  - Last segment: only L takes an update, and the sample is `(int32)s >> 20`. The state carries over from the first segment.
- `round` rounds half to even. The sines are computed in IEEE double precision.
- MD5: `mix44100` 7492916afaee69327324d00d29510b14, `mix48000` 7026e025f03b0f00c823682067c8e8f5, `mix96000` ec10c3fea498bed68dfbe3e1942ac584.

## encoder parameters

Create parameters common to every run: secondary profile 0, channel config 4 (stereo), create flag 1 (`spec/a2dp-integration.md` §3.1). Per run:

- **level:** the sink's decoder compatibility level, R1 or R2 (`spec/codec-core.md` §1).
- **profile:** the create-parameter profile. The reference source sends **3** (HQ) for an R1 sink even in LL mode, and 6 only for LL with an R2 sink (`spec/codec-core.md` §10.D). Profile 1 is included to show the low-power config 1.
- **rate, bitrate:** the create sample rate and bitrate. The same bitrate is the "table bitrate" of the packet loop.
- **cfg / byte 1:** the config the encoder reports, and byte 1 of the first packet (frame type, plus 0x80 if that packet is padded).
- **P:** the period in interleaved samples, `floor(rate · 2 · period_ms / 1000)` (`spec/codec-core.md` §10.A). At 44.1 kHz P can be odd (1587, 1411, 1323, 1675).
- **Lp:** the packet size in bytes (the output window).
- **chunk:** input frames per encode call. The output depends on the chunk size (see "chunk dependence" below), so it is part of the vector.
- **ll:** 1 = the LL variant of the packet loop (step 5 below).
- **packets:** packets produced from the 1 s input. **not forced:** packets that came out of an encode call with input, without a flush.

| name | level | profile | rate | bitrate | ms | P | Lp | chunk | ll | cfg | packets | not forced | byte 1 (pkt 0) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `c0_48k_L1` | R1 | 3 | 48000 | 281000 | 18 | 1728 | 656 | 48 | 0 | 0 | 55 | 0 | 81 |
| `c0_48k_L2` | R1 | 3 | 48000 | 318000 | 16 | 1536 | 656 | 48 | 0 | 0 | 62 | 0 | 81 |
| `c0_48k_L3` | R1 | 3 | 48000 | 364000 | 14 | 1344 | 656 | 48 | 0 | 0 | 71 | 0 | 81 |
| `c0_48k_L4` | R1 | 3 | 48000 | 427000 | 12 | 1152 | 656 | 48 | 0 | 0 | 83 | 0 | 81 |
| `c0_44k_L1` | R1 | 3 | 44100 | 281000 | 18 | 1587 | 656 | 44 | 0 | 0 | 55 | 0 | 81 |
| `c0_44k_L2` | R1 | 3 | 44100 | 318000 | 16 | 1411 | 656 | 44 | 0 | 0 | 62 | 0 | 81 |
| `c0_44k_L3` | R1 | 3 | 44100 | 364000 | 14 | 1234 | 656 | 44 | 0 | 0 | 71 | 0 | 81 |
| `c0_44k_L4` | R1 | 3 | 44100 | 427000 | 12 | 1058 | 656 | 44 | 0 | 0 | 83 | 0 | 81 |
| `c0_48k_qhs_L1` | R1 | 3 | 48000 | 110000 | 50 | 4800 | 760 | 48 | 0 | 0 | 27 | 27 | 01 |
| `c0_48k_qhs_L2` | R1 | 3 | 48000 | 235000 | 25 | 2400 | 760 | 48 | 0 | 0 | 40 | 0 | 81 |
| `c0_48k_qhs_L3` | R1 | 3 | 48000 | 290000 | 20 | 1920 | 760 | 48 | 0 | 0 | 50 | 0 | 81 |
| `c0_48k_qhs_L4` | R1 | 3 | 48000 | 350000 | 16 | 1536 | 760 | 48 | 0 | 0 | 62 | 0 | 81 |
| `c0_48k_qhs_L5` | R1 | 3 | 48000 | 430000 | 13 | 1248 | 760 | 48 | 0 | 0 | 76 | 0 | 81 |
| `c0_48k_L3_chunk13` | R1 | 3 | 48000 | 364000 | 14 | 1344 | 656 | 13 | 0 | 0 | 71 | 0 | 81 |
| `c0_44k_L3_chunk441` | R1 | 3 | 44100 | 364000 | 14 | 1234 | 656 | 441 | 0 | 0 | 71 | 25 | 01 |
| `c0_48k_ll` | R1 | 3 | 48000 | 265000 | 10 | 960 | 348 | 48 | 1 | 0 | 100 | 0 | 81 |
| `c0_44k_ll` | R1 | 3 | 44100 | 265000 | 10 | 882 | 348 | 44 | 1 | 0 | 91 | 1 | 01 |
| `c1_48k_ll` | R1 | 1 | 48000 | 265000 | 10 | 960 | 348 | 48 | 1 | 1 | 100 | 0 | 82 |
| `c1_44k_ll` | R1 | 1 | 44100 | 265000 | 10 | 882 | 348 | 44 | 1 | 1 | 91 | 1 | 02 |
| `c3_96k_i5_L1` | R2 | 3 | 96000 | 128000 | 40 | 7680 | 656 | 96 | 0 | 3 | 25 | 0 | 83 |
| `c3_96k_i5_L2` | R2 | 3 | 96000 | 255000 | 20 | 3840 | 656 | 96 | 0 | 3 | 50 | 0 | 83 |
| `c3_96k_i5_L3` | R2 | 3 | 96000 | 340000 | 15 | 2880 | 656 | 96 | 0 | 3 | 66 | 0 | 83 |
| `c3_96k_i5_L4` | R2 | 3 | 96000 | 420000 | 12 | 2304 | 656 | 96 | 0 | 3 | 83 | 0 | 83 |
| `c3_96k_i5_L5` | R2 | 3 | 96000 | 646000 | 8 | 1536 | 656 | 96 | 0 | 3 | 125 | 1 | 03 |
| `c3_96k_i6_L1` | R2 | 3 | 96000 | 142000 | 40 | 7680 | 760 | 96 | 0 | 3 | 25 | 0 | 83 |
| `c3_96k_i6_L2` | R2 | 3 | 96000 | 284000 | 20 | 3840 | 760 | 96 | 0 | 3 | 50 | 0 | 83 |
| `c3_96k_i6_L3` | R2 | 3 | 96000 | 460000 | 16 | 3072 | 960 | 96 | 0 | 3 | 62 | 0 | 83 |
| `c3_96k_i6_L4` | R2 | 3 | 96000 | 570000 | 13 | 2496 | 960 | 96 | 0 | 3 | 76 | 0 | 83 |
| `c3_96k_i6_L5` | R2 | 3 | 96000 | 820000 | 9 | 1728 | 960 | 96 | 0 | 3 | 111 | 0 | 83 |
| `c3_48k_i1_L1` | R2 | 3 | 48000 | 104000 | 50 | 4800 | 656 | 48 | 0 | 3 | 20 | 0 | 83 |
| `c3_48k_i1_L2` | R2 | 3 | 48000 | 204000 | 25 | 2400 | 656 | 48 | 0 | 3 | 40 | 0 | 03 |
| `c3_44k_i1_L3` | R2 | 3 | 44100 | 270000 | 19 | 1675 | 656 | 44 | 0 | 3 | 52 | 0 | 03 |
| `c3_44k_i1_L5` | R2 | 3 | 44100 | 427000 | 12 | 1058 | 656 | 44 | 0 | 3 | 83 | 1 | 03 |
| `c3_96k_r1_L3` | R1 | 3 | 96000 | 364000 | 14 | 2688 | 656 | 96 | 0 | 3 | 71 | 0 | 83 |
| `c4_48k_i1_L1` | R2 | 6 | 48000 | 220000 | 12 | 1152 | 348 | 48 | 1 | 4 | 83 | 1 | 04 |
| `c4_48k_i1_L3` | R2 | 6 | 48000 | 270000 | 10 | 960 | 348 | 48 | 1 | 4 | 100 | 1 | 04 |
| `c4_48k_i2_L1` | R2 | 6 | 48000 | 290000 | 15 | 1440 | 560 | 48 | 1 | 4 | 66 | 1 | 04 |
| `c4_48k_i2_L3` | R2 | 6 | 48000 | 320000 | 12 | 1152 | 560 | 48 | 1 | 4 | 83 | 0 | 84 |
| `c4_48k_i2_L4` | R2 | 6 | 48000 | 320000 | 10 | 960 | 560 | 48 | 1 | 4 | 100 | 0 | 84 |
| `c4_48k_i3` | R2 | 6 | 48000 | 214000 | 13 | 1248 | 348 | 48 | 1 | 4 | 77 | 1 | 04 |
| `c4_48k_i4_L3` | R2 | 6 | 48000 | 320000 | 13 | 1248 | 560 | 48 | 1 | 4 | 76 | 0 | 04 |
| `c4_48k_i5` | R2 | 6 | 48000 | 300000 | 8 | 768 | 348 | 48 | 1 | 4 | 125 | 1 | 04 |
| `c4_44k_i1_L3` | R2 | 6 | 44100 | 270000 | 10 | 882 | 348 | 44 | 1 | 4 | 94 | 1 | 04 |
| `c4_44k_i2_L1` | R2 | 6 | 44100 | 290000 | 15 | 1323 | 560 | 44 | 1 | 4 | 62 | 1 | 84 |

Name prefixes: `c0_*` config 0 (`_qhs_` for HQ table index 2, `_ll` for the R1 LL row as the reference source runs it), `c1_*` config 1, `c3_96k_i5_*` / `c3_96k_i6_*` the 96 kHz rows, `c3_48k_i1_*` / `c3_44k_i1_*` the R2 HQ index-1 rows (level 1 exercises the alternate flag, `spec/codec-core.md` §10.E), `c3_96k_r1_L3` an R1 sink at 96 kHz, `c4_*` config 4 (`iN` = LL table index N). Row values are from `spec/a2dp-integration.md` §4.1 and §4.2.

Notes on the rows:

- **HQ levels 5–7** for table index 0 use the same row as level 4 (427k / 12 ms), so `*_L4` covers them.
- **`c0_48k_qhs_L1`:** 110000 is below the configs 0/1 create clamp of 200000 (100000·nch), so the encoder runs at 200000 (`spec/codec-core.md` §10.G). After the first packet the deficit is above 96, so the loop asks for 110000 − 27500 = 82500, which is clamped to 200000 as well. Every packet in this file comes out without a flush (27 of 27).
- Byte 1 of packet 0 is shown so the frame type can be checked at a glance. 0x80 set means packet 0 ended with padding.

## packet loop (all counters in interleaved samples)

State: an input buffer of unconsumed samples (empty), `consumed = 0`, `deficit = 0`. The encoder is created once with the parameters above. Then, for each chunk of `chunk` frames of the input file, in order:

1. Append the chunk to the input buffer. Make one encode call with the whole buffer. The output window is Lp bytes, with 64 bytes of slack after it. Remove the consumed samples from the front of the buffer; keep the rest.
2. Add the number of samples consumed (interleaved) to `consumed`. If `consumed < P + deficit` and the call produced no packet, go to the next chunk.
3. Otherwise a packet is due. If the call produced no packet, make a flush call (no input) to force it out; that packet has "forced" = 1.
4. `deficit = deficit + P − consumed`, then `consumed = 0`.
5. **ll = 1 only:** if `deficit ≠ 0`, set `deficit = 0`. (The reference source also makes one more flush call here, into the same output window. It never produced bytes or changed encoder state in any run, so it can be left out.)
6. Call set-bitrate with `bitrate − floor(bitrate / 4)` if `deficit > 96`, otherwise with `bitrate`.

The file length is not a whole number of chunks for 44-frame chunks at 44.1 kHz (12 frames left) or for `c0_48k_L3_chunk13` (4 frames left). How that tail is fed, and whether anything happens after the last chunk, is not stated by the source of these vectors (`spec/QUESTIONS.md`, "from batch 5"). The packet counts in the table are what came out.

With ll = 0 this is the `tests/vectors/r2/` loop, except that P is given directly in interleaved samples, so it can be odd at 44.1 kHz.

## outputs

- `<name>.bin`: the raw encoder packets, concatenated in order, exactly as written. Every packet is exactly Lp bytes. There is no 8-byte header and no 16-bit byte swap.
- `<name>.sizes`: one line per packet, `index bytes forced`. `forced = 1` means the packet came out through the flush call in step 3.

## chunk dependence

The output depends on the chunk size (`spec/codec-core.md` §10.B). The encoder analyses only an even number of whole blocks from what it is given, and the loop only checks the period between encode calls, so where the chunks end changes when a period is found to be over and how many blocks the flushed frame holds. The model in `spec/codec-core.md` reproduces this exactly for every chunk size tried; it is defined behaviour, not noise. But a vector is only valid with its stated chunk size.

- At 44.1 kHz P is never a whole number of block pairs, so the chunk size always matters. `c0_44k_L3` (chunk 44) and `c0_44k_L3_chunk441` differ from packet 0. The reference source's 1 ms chunk at 44.1 kHz is 44 frames, which every other 44.1 kHz run uses.
- At 48 kHz, chunks of 5, 7, 9, 16, 24, 48, 56, 96, 112, 336 and 672 frames gave identical output on this signal, but chunks of 13 and 100 frames did not, both here and on `tests/vectors/r2/noise.s32le`. `c0_48k_L3_chunk13` is such a case: it **differs** from `c0_48k_L3` (first difference at byte offset 1209, in packet 1). So a 48 kHz chunk size is not interchangeable either; use the stated one.
