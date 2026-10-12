# Per-stage reference vectors: aptX Adaptive R1/R2 core encoder

Every value here was produced by the reference encoder and captured at the boundary between stages, so each number is what the real encoder computed. Each directory holds one encoder run, starting from a freshly created encoder.

Specification: `spec/codec-core.md`. Section numbers (§) below refer to that document.

| dir | config | rate | params | input |
|---|---|---|---|---|
| `cfg0_48k_noise` | config 0 (R1 sink, 4-band tree filterbank) | 48000 | bitrate 364000, packet 656 B | Gaussian noise, σ = 0.25·2^23 |
| `cfg0_48k_sine` | config 0 | 48000 | 364000 / 656 | L: 1 kHz at 0.3 FS, R: 3.1 kHz at 0.2 FS (24-bit) |
| `cfg1_48k_noise` | config 1 (R1 low latency, profile 1, R1 sink; "low-power" mode) | 48000 | 279000 / 328 | noise as above (30 packets) |
| `cfg3_48k_noise` | config 3 (R2 sink, 8-band cosine-modulated filterbank) | 48000 | 420000 / 660 | noise as above |
| `cfg3_48k_sine` | config 3 | 48000 | 420000 / 660 | sine as above |
| `cfg3_96k_noise` | config 3 | 96000 | 420000 / 660 | noise as above |

Create parameters common to every run:

- profile = 3 (HQ)
- channel config = 4 (stereo)
- create flag = 1 (the "flag" in `a2dp-integration.md` §3.1)

Each run produced 24 packets, except cfg1, which produced 30. cfg1 was created with profile = 1 instead of 3. The encoder was called once per packet with all remaining input. The output window was exactly `packet_len` bytes.

## Files (all little-endian int32 unless stated)

Notation:

- `S` = subbands per channel (4 for configs 0 and 1, 8 for config 3).
- `N` = blocks. One block is one subband sample per band per channel, i.e. `S` input samples per channel.
- Per-block band order is ch0 band0..S-1, then ch1 band0..S-1.

| file | shape | content |
|---|---|---|
| `pcm_in.s32` | N·S·2 | Interleaved L,R input PCM, exactly the samples that were consumed. These are 24-bit values in an int32. |
| `analysis_out.s32` | N × 2S | Filterbank output, which is the same as the quantiser input `x` (§3, §4). |
| `pred_in.s32` | N × 2S | Prediction given to the quantiser for that block (24-bit domain). This is the predictor output of the previous block (§4). |
| `quant_codes.s32` | N × 2S | Signed quantiser codes `q` (§5). |
| `quant_recon.s32` | N × 2S | Reconstructed difference `r` (§5.3), which is the predictor input. |
| `pred_hist_in.s32` | N × 2S | Same as `quant_recon` (kept for clarity). |
| `pred_out.s32` | N × 2S | Predictor output after that block (§6), which is the next block's `pred_in`. Bands whose predictor is disabled output 0. In cfg1, only channel-0 bands are predicted. |
| `frames.json` | list | One entry per bit-allocation frame: `packet` (index of the packet the frame went into), `blocks` (number of blocks in the frame), `bits` (2S allocations). |
| `packets.bin` | bytes | All packets concatenated. Each is `packet_len` bytes. These are raw encoder output, before any byte swap or A2DP header. |
| `meta.json` | | packet_len, n_packets, blocks, subbands_per_channel |

Frame blocks are consecutive. Frame k covers blocks `sum(blocks[0..k-1])` to `sum(blocks[0..k]) - 1`.

## What a clean implementation should reproduce

1. `pcm_in` → `analysis_out`. The filterbank is stateful and starts at zero.
2. With `analysis_out` and the `bits` of each block's frame, run the quantiser (§5) and predictor (§6) in a loop. This must give identical `quant_codes`, `quant_recon` and `pred_out`. All state starts at its reset value.
3. With `frames.json` (`bits`, `blocks`, `packet`) and `quant_codes`, the packer (§7) must give identical `packets.bin`.

Bit allocation (§8) is an encoder-side choice that is **not** specified yet. That is why `bits` is supplied as an input. A decoder needs no particular allocator, so any allocator gives a valid stream. Matching these vectors byte for byte, however, requires using the supplied allocations.

Note: in `cfg3_96k_noise` the input is 24-bit noise like the other runs (its range is −2^23..2^23−1). The 4-bit packet header carries sample-rate code 6 (96 kHz), not 2 (48 kHz).
