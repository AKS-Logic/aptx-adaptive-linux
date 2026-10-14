# aptX Adaptive R1/R2 core encoder: bitstream and signal path

Status: clean-side spec, 2026-10-12. Batch 5 corrections and the config 4 specification (2026-10-13) are in §10, which supersedes earlier sections where they differ.

- **Verification:** every stage marked **[VERIFIED]** was modelled independently and compared bit for bit against the reference encoder, using intermediate values captured between stages.
- **Test vectors:** `tests/vectors/stages/` (see its README).
- **Tables:** not in the repo. The build extracts them from the user's copy of the vendor library and checks each against a SHA-256 (`table-extraction.md`). This document names a table by its id there, such as `quantiser.log_table`; §3 of that spec gives each id's layout and the Rust constant it becomes.
- **Companion spec:** `a2dp-integration.md` covers the wrapper, the A2DP config, packet timing, the 16-bit byte swap and the 8-byte header. This document starts where that one stops: PCM samples go into the encoder, raw packets come out.

Notation and arithmetic conventions are in §0. Anything not confirmed by execution is marked **UNCERTAIN**.

---

## 0. Conventions

- Integers are two's complement.
- `lo32(v)`: wrap v to signed 32 bits. `lo64(v)`: wrap to signed 64 bits.
- `sat32(v)`, `sat24(v)`: clamp to [−2^31, 2^31−1] and [−2^23, 2^23−1]. `sat64` likewise for 64 bits.
- `a ⊕ b = sat32(a+b)` and `a ⊖ b = sat32(a−b)` (saturating 32-bit add and subtract).
- `>>` is an arithmetic right shift unless stated otherwise.
- **`R64(v)`** ("round high word"): `sat32( (sat64(v) + 2^31) >> 32 )`, where v is a 64-bit value.
- **`CR(v, n)`** (convergent rounding by 2^n, round half to even): `t = ((v >> (n−1)) + 1) >> 1`. If `v mod 2^n == 2^(n−1)` exactly (an exact half), clear bit 0 of t. CR23 means n = 23.
- **`CR1(v)`, `CR8(v)`:** the same convergent rounding by 2 and by 256, applied to a 32-bit value. The result is 32-bit.
- **`RND1(v)`** = `(v + 1) >> 1` (round half up), on a 32-bit value.
- `sext24(v)`: keep the low 24 bits of v and sign-extend them.
- Wherever a 64-bit rounded result is later used as 32 bits, take `lo32` first and saturate after. In normal operation values are always in range.

Bit writing is MSB first. A field "v in n bits" puts bit n−1 of v first. Bytes are filled from bit 7 down to bit 0.

---

## 1. Configurations and what the Px7 S2e gets

The encoder supports four music configurations, numbered 0, 1, 3 and 4 (config 2 is speech and out of scope). A config is chosen from the A2DP profile (HQ / LL) and the sink's decoder compatibility level (R1 or R2, `a2dp-integration.md` §1.3). "Frame type" is the value the encoder writes in packet byte 1 (§7.1).

| profile | decoder level | sample rate | config | frame type | filterbank | subbands/ch | samples/block | predictor mode |
|---|---|---|---|---|---|---|---|---|
| HQ (profile 0, 3 or 5) | R1 | 44.1/48k | **config 0** | 1 | 4-band tree QMF, 16 taps (§3.1) | 4 | 4 | normal (§6) |
| LL (profile 1 always; profile 2/6 with R1; but see §10.D: the source never uses these for an R1 sink) | R1 | 44.1/48k | **config 1** | 2 | 4-band tree QMF, 12 taps | 4 | 4 | low-power (§6.3) |
| HQ | R2 | 44.1/48k | **config 3** | 3 | 8-band cosine-modulated, 256-tap prototype (§3.2) | 8 | 8 | normal |
| HQ | any | 88.2/96k | **config 3** | 3 | same as above | 8 | 8 | normal, some bands' predictor off |
| LL (profile 2/6) | R2 | 44.1/48k | **config 4** | 4 | cosine-modulated, 128-tap prototype (§10.F) | 8 | 8 | normal |

The companion spec establishes that the **Px7 S2e is an R1 sink**. That means:

- **config 0** for HQ — verified bit-exact end to end.
- **config 1** for low latency — verified bit-exact end to end.

Config 3 (R2 sinks) is also verified, at 44.1, 48 and 96 kHz. Config 4 is verified at 44.1 and 48 kHz (§10.F). **Correction (§10.D):** an R1 sink in LL gets config 0 on the LL table row, not config 1.

**How the config is selected:**

1. Two decoder compatibility words `D0, D1` are turned into capability masks: `C0 = ((2·D0) & 0xE) | 1` and `C1 = (2·D1) & 0xE`. An R1 sink gives masks (1, 0); an R2 sink gives (7, 6).
2. HQ tries the candidate indices [2, 0] in that order. LL tries [3, 1].
3. On the first pass, the first index whose bit is set in `C0 & C1` wins. If none matches, a second pass uses `C0` alone.
4. Index 0 → config 0, index 1 → config 1, index 2 → config 3, index 3 → config 4.
5. HQ at 88.2/96 kHz goes straight to config 3. Profile 1 goes straight to config 1. Profile 4 is the speech configuration (config 2), which is out of scope.

Encoder state is per channel. Stereo is coded as two independent channels in the same frame; joint stereo was never seen enabled. Wherever "band index i" ranges over a block, the order is: channel 0, bands 0..S−1, then channel 1, bands 0..S−1. S is the number of subbands per channel.

Input samples are int32. The wrapper supplies 24-bit audio right-justified (its Q31 sample shifted right by 8). The encoder takes whatever int32 it is given; both 24-bit and full-scale 32-bit inputs were verified.

A **block** is one subband sample per band per channel, so 2·S values. It consumes S input samples per channel. A **frame** is a run of blocks (64 or fewer) that share one bit allocation.

---

## 2. Per-block processing order

For every block, for each channel independently:

1. **Analysis.** Run the filterbank on S new input samples to get S subband values `x[i]` (§3).
2. **Quantise.** For each band, quantise `x[i]` against the prediction `p[i]` left by the previous block, using the frame's allocation `B[i]`. This gives the code `q[i]` and the reconstructed difference `r[i]` (§5).
3. **Dither.** In normal mode only, update the dither seed and the dither values for the next block (§5.5).
4. **Predict.** Run each band's predictor on `r[i]`, which produces `p[i]` for the next block (§6).

The codes `q` of all blocks in the frame, plus the frame's allocation, are then entropy-coded into packets (§7).

**[VERIFIED]** on these runs, with reference-supplied allocations and frame sizes; every code, reconstruction, prediction and packet byte was identical:

- config 0 at 48 and 44.1 kHz: 24-bit sine, 24-bit noise, full-scale 32-bit noise, silence, impulse; bitrates 364/427 kbps
- config 1 at 48 kHz
- config 3 at 44.1, 48, 88.2 and 96 kHz

---

## 3. Analysis filterbanks

### 3.1 Four-band tree QMF (configs 0 and 1) [VERIFIED]

This is the same tree structure as aptX classic. Two-band stages are cascaded: one outer stage, then two inner stages.

- **Coefficients:** `filterbank_4band_tree.*`. Config 0 uses `outer_16` and `inner_16` (n = 16 taps). Config 1 uses `outer_12` and `inner_12` (n = 12).
- **State:** each 2-band stage keeps two delay lines, A and B, each holding the last n samples. All state starts at zero.

**A 2-band stage** takes two inputs (a, b):

1. Push a into delay line A and b into delay line B.
2. `P = Σ_{k=0}^{n−1} c[k] · A(age k)`. Age 0 is the sample just pushed.
3. `Q = Σ_{k=0}^{n−1} c[k] · B(age n−1−k)`. Note the reversed order: c[n−1] multiplies the newest sample of B.
4. Both sums are exact (64-bit or wider).
5. `p = R(P)` and `q = R(Q)`, where `R(v) = sat24( lo32(CR23(v)) )`: convergent rounding by 2^23, then a 24-bit clamp.
6. The outputs are **sum** = `sat24(p + q)` and **diff** = `sat24(q − p)`.

**Per channel and per block**, with input samples s0..s3 in time order:

1. Outer stage on (s0, s1) gives (sum0, diff0). Outer stage again on (s2, s3) gives (sum1, diff1). The same outer-stage state is used both times, sequentially.
2. Inner-low stage on (sum0, sum1) gives (band0, band1) = (sum, diff).
3. Inner-high stage on (diff0, diff1) gives (band3, band2) = (sum, diff).
4. The quantiser input is `x[band] = band_value << 8`, a plain 32-bit shift.

**UNCERTAIN:** config 0 created with its low-power option set would use the 12-tap coefficients, a 16-long delay line and different read positions. The normal wrapper never sets this option.

### 3.2 Eight-band cosine-modulated filterbank (config 3) [VERIFIED]

- **Tables:** prototype `filterbank_8band_prototype_256.coef` (256 int32 values `P[0..255]`). Modulation constants `filterbank_8band_modulation.frontend` and `.dct`.
- **State:** the last 256 input samples of the channel, all zero at start.

**Per block** (8 new samples per channel):

**1. Push the samples.** Push the 8 samples in time order. `h(m)` is the sample of age m (m = 0 is the newest).

**2. Polyphase.** For i = 0..15:

- `acc = −Σ_{k=0}^{15} P[i+16k] · h(i+16k)`. Accumulate in 64 bits with wraparound.
- `u[i] = R64( lo64(acc · 64) )`.

**3. Cosine-modulation front end.** There are four groups. Each group g has fields `(a, b, c, d, od, Kd, os, Ks)`; in `filterbank_8band_modulation.frontend[g]` these are `u_add = [a, b]`, `u_sub = [c, d]`, `diff_out = od`, `diff_const = Kd`, `sum_out = os`, `sum_const = Ks`. For each group:

- `A = u[a] ⊕ u[b]`
- `C = u[c] ⊖ u[d]`
- `S = A ⊕ C`
- `D = A ⊖ C`
- `o[od] = R64( sat64(2 · D · Kd) )`
- `o[os] = R64( sat64(2 · S · Ks) )`

The constants are **unsigned** 32-bit values: the product is the signed sample times the unsigned constant, exact in 64 bits.

**4. 8-point DCT.** Apply this to `o[0..7]`, in place.

Define `M(v, K) = R64( lo64(2 · v · K) )`. The constants K1..K7 are the signed 32-bit values `filterbank_8band_modulation.dct` c1..c7, in that order.

With inputs x0..x7 = o[0..7]:

| step | formula |
|---|---|
| sums | `s04 = x0⊕x4`, `s26 = x2⊕x6`, `s15 = x1⊕x5`, `s37 = x3⊕x7` |
| odd products | `A = M(x0⊖x4, K1)`, `B = M(x1⊖x5, K2)`, `C = M(x3⊖x7, K3)`, `D = M(x2⊖x6, K4)` |
| partial sums | `AD = A⊕D`, `BC = B⊕C`, `S0 = s04⊕s26`, `S1 = s15⊕s37` |
| y0 | `S0 ⊕ S1` |
| y1 | `AD ⊕ BC` |
| y2 | `G ⊕ H`, where `G = M(s04⊖s26, K5)` and `H = M(s15⊖s37, K6)` |
| y4 | `M(S0⊖S1, K7)` |
| y3 | `EF ⊕ (EF ⊖ y1)`, where `E = M(A⊖D, K5)`, `F = M(B⊖C, K6)`, `EF = E⊕F` |
| y5 | `I ⊕ (I ⊖ y3)`, where `I = M(AD⊖BC, K7)` |
| y6 | `L ⊕ (L ⊖ y2)`, where `L = M(G⊖H, K7)` |
| y7 | `T ⊕ (T ⊖ y5)`, where `J = M(E⊖F, K7)` and `T = J ⊕ (J ⊖ EF)` |

Then `o[k] = y_k`.

**5. Post-processing.**

- `o[0] = R64( lo64(o[0] · 2^31) )`, which is effectively a rounded halving.
- Then for k = 1..7 in order: `o[k] = o[k] ⊖ o[k−1]`. Each step uses the already-updated `o[k−1]`.

**6. Output.** Subband k (k = 0 is the lowest band) is `x[k] = o[k]`, already in the 32-bit quantiser domain.

The filterbank adds no extra delay or latency compensation. The encoder reports latency 0.

---

## 4. Quantiser input

For each band, the quantiser sees:

- `x`: a 32-bit subband value;
- `p`: the 24-bit-domain prediction from the previous block's predictor. It is 0 for the first block and for bands whose predictor is disabled.

The difference is formed as `d = x ⊖ lo32(p · 256)`.

---

## 5. Non-uniform adaptive quantiser [VERIFIED]

**Tables:** for each table size Bt = 2..9 there are:

- `T` (`quantiser.tables.<Bt>.threshold`): thresholds;
- `Dt` (`quantiser.tables.<Bt>.dither`): dither;
- `Inc` (`quantiser.tables.<Bt>.logdelta_incr`): log-delta increments.

All three have 2^(Bt−1)+2 entries, the last a zero sentinel. Allocations above 9 bits use the Bt = 9 tables plus raw LSBs. There is also `quantiser.log_table` (32 entries).

**State.**

- Per band: `step` (initially 8), `ld` (log-delta, initially 244), `dith` (initially 0).
- Per channel: `seed` (initially 0).

**Modes.**

- **Normal** mode applies to configs 0 and 3.
- **Low-power** mode applies to config 1. It drops dither and changes the threshold (§5.2) and the reconstruction (§5.3).

For each band, the frame allocation `B ∈ {0, 2..16}` is used. 1 never occurs.

### 5.1 Zero allocation

If `B = 0`:

- `q = 0` and `r = 0`;
- `step = 8` and `ld = 244`;
- the low-power carry (§5.2) is not touched.

### 5.2 Code decision

**Setup.**

- `Bt = min(B, 9)` and `E = max(B − 9, 0)`.
- Use the tables `T`, `Dt`, `Inc` for Bt.
- `a = lo32(|d|)` (the absolute value of −2^31 stays −2^31), and `A = a · 2^11`. All products in this section are 64-bit.

**1. Coarse binary search.**

- Start with `idx = 0`.
- For `h = 2^(Bt−2), 2^(Bt−3), …, 1`: if `A − T[idx+h] · step ≥ 0`, add h to idx.
- Then set `L = T[idx]`, `H = T[idx+1]`, `dL = Dt[idx]`, `dH = Dt[idx+1]`, `code = idx`, `R = 0`.

**2. Extension, only if E > 0.**

Repeat E times:

1. Remember `(Lp, Hp) = (L, H)`.
2. `M = L + ((H − L) >> 1)`.
3. If `A − M · step < 0`: `code = 2·code`, `R = 2·R`, `H = M`.
4. Otherwise: `code = 2·code + 1`, `R = 2·R + 1`, `L = M`.

Afterwards:

- Restore `(L, H) = (Lp, Hp)`, i.e. the interval *before* the last split.
- `dH = dH − ( lo32( lo32(mask · (−1 − R)) · (dH − dL) ) >> E )`, where `mask = 2^E − 1`.

**3. Decision threshold v.**

In **normal** mode, with `δ = dith`:

1. `g = sat24(dL − dH)`
2. `e = sat24(lo32(CR23(δ · δ)))`
3. `off = sat24(lo32(CR23((2^21 − e) · g)))`
4. `thr = sat24(off + H + L)`
5. `V = thr · 2^23 + (d ≥ 0 ? δ·(H−L) : −δ·(H−L))`
6. `v = sat24(lo32(CR23(V)))`

In **low-power** mode:

- `v = sat24(carry + H + L)`.
- `carry` is the `ld` most recently produced in this block, by any band of either channel, in processing order. It is 0 at the start of each block.

**4. Final code.**

1. If `a · 2^12 − v · step < 0`, decrement `code` by 1.
2. If `d < 0`, set `code = ~code` (that is, −code−1).
3. The result is `q`.

### 5.3 Reconstruction

1. `c = q >> E` (arithmetic shift).
2. `j = c + 1` if c ≥ 0, otherwise `j = −c`.
3. `ρ = T[j]`.
4. If E > 0:
   - `σ = −1` if q < 0, otherwise 0
   - `m9 = mask & (σ XOR q)`
   - `m6 = mask & ~m9`
   - `ρ = ρ − ( lo32((T[j] − T[j−1]) · m6) >> E )`
5. If c < 0, negate ρ.
6. Normal mode with B ≤ 9 only: `ρ = sat24(lo32(CR23(Dt[j] · δ + ρ · 2^23)))`.
7. `r = sat24( lo32( (step · ρ) >> 19 ) )`.

**Low-power quirk:** only bands 0..S−2 compute a new r. The top band (band S−1 = band 3 in config 1) keeps the r it had in the previous block. That value is 0 if the band had B = 0 at any point, since §5.1 writes r = 0.

### 5.4 Step adaptation (all modes)

1. `ld = clamp( sat24(lo32(CR23(8350720 · ld + Inc[j] · 2^23))), 0, 6399 − 256·B )`. Here j is the index from §5.3, Inc is read as int16, and B is the full allocation, not Bt.
2. `step = log_table[(ld >> 3) & 31] >> (24 − B − (ld >> 8))`.

### 5.5 Dither generation (normal mode only)

This runs after all bands of a channel are coded in the block. q0 and q1 are that channel's band-0 and band-1 codes.

1. `seed = lo32( (((4·q0) & 12) | (q1 & 3)) · 256 + 16·seed )`
2. `P = (seed >> 8) · 10368886` (exact signed 64-bit). `Pl = P mod 2^32`.
3. `V = ((Pl >> 22) & 3) | ( ((⌊(P mod 2^64) / 2^24⌋ + Pl) & 0x3FFFFF) << 2 )`
4. For band k of this channel: `dith = sext24( (V << s_k) mod 2^24 )`. Here `s_k = 2k` for 8 bands and `s_k = 5k` for 4 bands.

These dither values apply to the **next** block.

---

## 6. Sign-sign LMS predictor [VERIFIED]

There is one predictor per band, per channel. It is a pole-zero (two poles, N zeros) adaptive predictor with sign-sign updates.

**Zero (FIR) tap counts N**, per band, the same for each channel:

- configs 0 and 1: `predictor_taps.4band`.
- config 3 at 44.1/48k: `predictor_taps.8band_48k`.
- config 3 at 88.2/96k: `predictor_taps.8band_96k`. A count of 0 disables that band's predictor: its output is 0 and its state is never touched.

**State per band** (all at reset):

| variable | meaning | initial value |
|---|---|---|
| `s0`, `s1` | sign history | 1, 1 |
| `a1`, `a2` | pole coefficients | 0, 0 |
| `b[0..N−1]` | zero coefficients | 0 |
| history | last N inputs | 0 |
| `y1` | previous pole input | 0 |
| `z` | previous zero-predictor output | 0 |
| `pr` | prediction | 0 |

### 6.1 Per sample

The input is `x = r`, the quantiser reconstruction (§5.3).

**1. Sign and pole adaptation.**

- `t = z + x`, using the previous `z`.
- Choose the sign terms:
  - t < 0: `cur = −1`, `p1 = −s0`, `p2 = −s1`.
  - t > 0: `cur = 1`, `p1 = s0`, `p2 = s1`.
  - t = 0: `cur = 1`, `p1 = 0`, `p2 = 0`.
- Then `s1 = s0` and `s0 = cur`.
- Update the poles. All of the following are 32-bit operations, using the **old** a1 and a2:
  - `w = CR8(255 · a1 + 12582912 · p1)`
  - `k = clamp(CR1(−p1 · a1), −2^20, 2^20)`
  - `a2' = clamp( CR8( ((16 · (k + p2 · 2^19)) & ~0xFF) + 254 · a2 ), −3145728, 3145728 )`
  - `a1' = clamp(w, a2' − 3932160, 3932160 − a2')`
  - Store a1', a2'.

**2. Pole part.**

- `y = sat24(pr + x)`, using the previous `pr`.
- `pole = lo32( (y1 · a2 + a1 · y) >> 21 )`, using the new a1 and a2.
- Then `y1 = y`.

**3. Zero coefficient update.**

- `e = 0` if x = 0, otherwise `e = sign(x) · 2^23`.
- Let `hist[k]` be the input of age k+1, for k = 0..N−1. `hist[0]` is the previous input.
- For each k: `term = −e` if `hist[k] < 0`, otherwise `e`.
- Then `b[k] = sext24( b[k] + CR8(lo32(term − b[k])) )`.

**4. Zero prediction.**

- `acc = b[0]·x + Σ_{k=1}^{N−1} b[k] · hist[k−1]`, exact in 64 bits.
- So each updated coefficient k multiplies the sample one step *newer* than the one that gave its update sign.
- `z = sat24( lo32(acc >> 22) )`.

**5. Prediction.**

- `pr = sat24( z + sat24(RND1(pole)) )`.
- Push x into the history.

**6. Output.** `pr` is the `p` used by the quantiser in the next block (§4).

### 6.2 Normal mode

All enabled bands of both channels run every block.

### 6.3 Low-power mode (config 1)

The arithmetic is the same, but **only channel 0, bands 0..3** run. Every channel-1 band's prediction is always 0 and its state never changes. This was confirmed against the reference, even though it looks like an implementation quirk.

---

## 7. Bitstream: packets, frames, allocation and codewords [VERIFIED]

The encoder emits fixed-size packets of `Lp` bytes. Lp is set by the wrapper, must be a multiple of 4, and in practice is 328 to 880. There is one container per packet.

### 7.1 Packet layout

| byte | content |
|---|---|
| 0 | 0x00 |
| 1 | frame type (config 0 → 1, config 1 → 2, config 3 → 3, config 4 → 4), OR 0x80 if the packet ends with padding |
| 2 | Lp/4 − 3 (for example 656 → 0xA1, 660 → 0xA2, 328 → 0x4F) |
| 3 … Lp−1 | payload: `Cap = Lp − 3` bytes |

The payload is one MSB-first bit sequence:

1. **Continuation.** Let C be the number of bytes carried over from the previous packet (0 for the first packet). Write C as Exp-Golomb order 0: with `v = C + 1` and `L = bitlength(v)`, write L−1 zero bits, then v in L bits. C = 0 is the single bit `1`. Then write the C carried bytes, 8 bits each, unchanged.
2. **Frames.** One or more frames follow, each written directly after the previous one with no alignment. For each frame:
   - the bit allocation. The first frame in the packet uses absolute coding with a 4-bit header (§7.2). Later frames in the same packet use differential coding against the previous frame (§7.3).
   - the frame's codewords (§7.4).
3. **End.** Zero-pad the last partial byte.

If the total exceeds Cap bytes, the first Cap bytes go into this packet and the remaining bytes are the next packet's continuation.

**Padding.** If the payload is shorter than Cap, the remaining `Pd = Cap − len` bytes are filled as follows, and byte 1 gets bit 0x80:

- the first pad byte is 0x80, the rest are 0x00;
- then, with `t = Pd − 1`, the last `⌊t/255⌋` pad bytes are 0xFF, and the byte just before them is `t mod 255`.

When Pd = 1, the single pad byte is 0x00.

**Frames per packet (encoder policy, see §8.0 and §8.1).**

- Every frame except the packet's last has exactly 64 blocks.
- The last frame has any even number of blocks from 2 to 64. Either the frame-size rule (§8.1) chose 8, 16, 32, 48 or 64 and closed the packet, or the period-end flush call coded a partial frame (for example 40 blocks at 364 kbps / 656 B, which is what every packet in `tests/vectors/r2/*.r1.bin` ends with).
- A frame's bits may run past the packet end. The overflow is the next packet's continuation.

The earlier statement that the last frame is always 8, 16, 32, 48 or 64 blocks was incomplete: that set only applies when the encoder closes the packet itself.

A decoder cannot read the block count of the last frame directly. It is implied by where the data ends, or by the zero-allocation escape (§7.2). An encoder must therefore close each packet's last frame exactly where the payload ends (packet end or continuation end). The policy that picks the last frame's size from the remaining byte budget is described in §8. It is **not** needed for decodability.

### 7.2 Absolute allocation (first frame of each packet)

**Reduced allocation.** `ρ(B) = B − 1` if B > 1, otherwise B. So 0 → 0, 2 → 1, 3 → 2, …, 16 → 15.

**4-bit header**, written first:

| config | header value |
|---|---|
| config 3 / config 4 | sample-rate code (44.1k → 0, 48k → 2, 88.2k → 4, 96k → 6), plus 8 if the stream has 2 channels |
| config 0 / config 1 | (48k ? 2 : 0), plus 1 for low-power (config 1), plus 4 if the create-parameter channel config equals 2 (UNCERTAIN: never seen; stereo is 4), plus 8 if 2 channels |

Observed headers: 0xA for configs 0 and 3 at 48 kHz stereo; 0xB for config 1.

**Band pairs.** The bands are taken in pairs (band 2m, band 2m+1) over the ordered list ch0 bands, then ch1 bands. The tables are `alloc_absolute_huffman.table_a` (A) and `.table_b` (B). Which one codes pair m is entry m of `alloc_absolute_huffman.pair_table_8band` (8 subbands) or `.pair_table_4band` (4 subbands).

**Configs 3 / 4.** For each pair m, with α = ρ(B[2m]) and β = ρ(B[2m+1]):

1. Map each value to a nibble: the value itself if ≤ 7, otherwise 8.
2. Symbol = `(nibble_α << 4) | nibble_β`. Write its Huffman code from pair m's table.
3. Escapes: for each of α, β in that order whose value is > 7, append `value − 8` in 3 bits. All escape bits of the pair come together after the code, α's first.

After all pairs, if every B in the frame is 0, write `(frame_blocks − 1)` in 8 bits.

**Configs 0 / 1.** For each pair m:

- Symbol = `(α << 4) | β`, with no escapes.
- Pairs 0..3 (ch0 bands 0/1, ch0 2/3, ch1 0/1, ch1 2/3) each use pair m's table.
- There is no zero-allocation escape.

### 7.3 Differential allocation (later frames in the same packet)

Tables: `alloc_diff_huffman.tables`, `.table_index` and `.range`. Per band i, in list order:

- `v = ρ(B_new[i])`
- `pv = ρ(B_prev[i])`, from the immediately preceding frame
- table = `tables[table_index[pv]]`

**Configs 3 / 4:**

1. `(lo, hi) = range[pv]`.
2. `cl = min(max(v, lo), hi)`.
3. Write `table[(cl − pv) & 15]`.
4. Then:
   - if v ≤ lo, write `lo − v` in 3 bits;
   - else if v ≥ hi, write `v − hi` in 3 bits.

After all bands, if every B is 0, write `(frame_blocks − 1)` in 8 bits.

**Configs 0 / 1:** write `table[(v − pv) & 15]`. There is no clamping and no escape.

### 7.4 Codewords

Tables: `codeword_huffman.entries`, one record per allocation 0..16, each naming one of `codeword_huffman.tables.T1`..`T4` (or none) with its `pair_shift`, `raw_lsbs` and `msb_mask`. Allocation 1 is never allocated; its record is used only by the configs 0 / 1 exception below.

Blocks are taken in pairs (block 2n, block 2n+1) in time order; frames always have an even block count. For each pair, for each band i in list order with B[i] ≠ 0:

1. Let `q0`, `q1` be the codes of that band in the two blocks.
2. `Bc = B[i]`. **Configs 0 / 1 exception:** if `B[i] = 2` and i is the top band of its channel (i mod 4 = 3), use the entry for allocation 1 instead.
3. From the entry for Bc, take `(table, pair_shift, raw_lsbs, msb_mask)`.
4. `h0 = (q0 >> raw_lsbs) & msb_mask` and `h1 = (q1 >> raw_lsbs) & msb_mask`.
5. Write the Huffman code for symbol `(h0 << pair_shift) | h1`.
6. If `raw_lsbs > 0`, write the low raw_lsbs bits of q0, then the low raw_lsbs bits of q1.

The parameters for each allocation are those of its `codeword_huffman.entries` record.

---

## 8. Encoder-side decisions: frame sizing and bit allocation [VERIFIED]

These are encoder choices; a decoder does not depend on them. Following this section exactly takes a model from raw PCM to packets that are byte-identical to the reference encoder, with no allocations or frame sizes supplied from outside.

**Verification.** Every run below was byte-identical: 4,149 packets from the per-packet loop of `a2dp-integration.md` §5, plus 126 stage-vector packets driven as described in §8.7.

| config | runs |
|---|---|
| config 0 | `tests/vectors/r2/*.r1.bin` (noise, sine1k, silence; 364 kbps, 656 B). Five more HQ table rows on a mixed noise, tones and silence signal: 281k/864, 318k/768 and 427k/576 samples per packet at 656 B; 290k/960 and 430k/624 at 760 B. |
| config 1 | five LL table rows, 214k to 320k, at 348 and 560 B |
| config 3 | `tests/vectors/r2/*.r2.bin`, plus 420k/660 B and 500k/760 B at 48 kHz, and 420k/660 B and 570k/960 B at 96 kHz |

Only the `tests/vectors/` runs are available on the clean side; the other table rows were checked by the spec side and have no vectors here.

Input was fed in 48-, 672- and 1000-frame chunks. **Correction (§10.B):** the output does depend on chunking in general; a bit-exact test must use each vector's stated chunk size.

**Tables:** the `allocator.int8.offset_*` and `allocator.int32.max_bits_*` tables (`table-extraction.md` §3.9).

### 8.0 Why packets end "64 + 64 + 40"

The encoder does not decide the size of a packet's last frame on its own; the wrapper's **period** does. At 364 kbps / 656 B the wrapper supplies 672 samples per channel per packet, which is 168 four-sample blocks.

1. The frame-size rule (§8.1) asks for 64-block frames. The encoder codes 64, then another 64.
2. The third 64-block frame is only 40 blocks full when the period's input runs out.
3. The wrapper then makes the flush call (no input). The flush codes the **incomplete** frame with however many blocks it holds (40) and closes the packet.

So the last frame of a packet is one of two things:

- a frame sized by §8.1 (8, 16, 32, 48 or 64 blocks), which closes the packet itself; or
- whatever even number of blocks has been analysed when the flush arrives.

Any even count from 2 to 64 can therefore appear (§7.1). Analysis only ever takes an even number of blocks from the input (§8.1 step 2), so a flush never finds an odd block; any leftover input stays unconsumed and is passed again in the next call.

### 8.1 Encoder state machine (standard A2DP packet format, create flag = 1)

This describes the encoder with the create flag of `a2dp-integration.md` §3.1 set to 1, which is the only value the reference wrapper uses.

**State.**

| item | meaning | initial value |
|---|---|---|
| carry | bytes that overflowed the previous packet | none |
| `pad_hist` | smoothed padding, in bytes | 0 |
| `pad1`, `pad2` | padding byte counts of the last and second-last packets | 0, 0 |
| `ov_hist` | smoothed carry-over, in bytes | 0 |
| `ov_prev` | the previous packet's carry length | 0 |
| `P_est` | bit pool per block from the most recent allocation (§8.2) | the pool computed at create time from the configured bitrate |
| `nominal` | the bitrate set at create time or by the set-bitrate operation | the create bitrate |
| `target` | the rate-controlled bitrate (§8.1a) | `nominal` |
| `A`, `Ap`, `Bq`, `Bp` | rate-check filter state (§8.1a) | all equal to `nominal` |
| current frame | blocks analysed but not yet coded | none |
| current packet | the open packet's bit writer, or none | none |

**Encode call with input.** Repeat these steps:

1. Evaluate the frame-size rule (below) to get a requested block count `req` and a `close` flag. This is re-evaluated on **every** iteration, including when a frame is already partly analysed.
2. Analyse whole blocks into the current frame until it holds `req` blocks, taking only an even number of blocks from the available input. A block is S stereo sample frames, S = 4 or 8. If the input runs out first, return the number of samples consumed and no packet.
3. Code the frame (§8.2 to §8.6, then §5, §6 and §7). If no packet is open, open one first:
   1. Update the carry history: `ov_hist = (410·(ov_hist + ov_prev) + 205·len(carry)) >> 10`, then `ov_prev = len(carry)`.
   2. Start the payload with the continuation field and the carried bytes (§7.1).
   3. **After** the first frame of the new packet is coded, update the padding history: `pad2 = pad1`, `pad1 = padding of the packet just finished`, then `pad_hist = (410·(pad_hist + pad2) + 205·pad1) >> 10`. The ordering matters: the frame-size rule for that first frame still sees the old `pad_hist`, while the new `ov_hist` is already in place.
4. If `close` is set, or `floor(bits_written / 8) ≥ Lp − 3`, close the packet (§7.1 padding and carry split), run the rate check (§8.1a), and return the packet.

**Flush call (no input).** Code the current partial frame if it holds any blocks, then close the packet, run the rate check (§8.1a), and return the packet.

**Frame-size rule.** All quantities are in bytes, signed.

1. Work out the space left and the amount to hold back:
   - If no packet is open: `space = Lp − 3 − len(carry)` and `hold = len(carry) + ov_hist`. The carry is deliberately counted **twice**; that is what the reference does.
   - If a packet is open: `space = Lp − 3 − floor(bits_written / 8)` and `hold = ov_hist`. Here bits_written includes the continuation field and the carried bytes.
2. `v = space − hold + min(pad_hist, 30)`.
3. If `v < 0`, the result is 8 blocks, close.
4. Otherwise let `x = 8·v`. If `target > nominal`, let `k = floor(nominal / (target − nominal))`, and if `k ≠ 0` add `floor(x / k)` to x.
5. `n = floor(x / P_est)`, and the result is:

   | n | blocks | close the packet? |
   |---|---|---|
   | ≥ 64 | 64 | no, keep it open |
   | ≤ 7 | 8 | yes |
   | 8 to 15 | 16 | yes |
   | 16 to 31 | 32 | yes |
   | 32 to 47 | 48 | yes |
   | 48 to 63 | 64 | no, keep it open |

**UNCERTAIN**, never active in the verified runs: with the create flag set to 0, a constant-bitrate clamp further limits the frame size. The reference wrapper always uses flag 1.

#### 8.1a Rate check

This runs once after every packet is closed (by the frame-size rule, by a full payload, or by a flush). Inputs:

- `W = floor(bits_written / 8) + 3 − carry_in`, where `carry_in` is the number of carried bytes this packet **started** with;
- `pad`, this packet's padding bytes;
- `nb`, the number of blocks coded into this packet.

All arithmetic here is unsigned 32-bit, and `>>` is a logical shift.

1. `q = floor(2·fs / (S·nb))`, then `pps = (q & 1) + (q >> 1)`. This is the packet rate per second, rounded.
2. `A_new = (410·(Ap + A) + 1640·(W + pad)·pps) >> 10`, then `Ap = A` and `A = A_new`.
3. `B_new = (410·(Bp + Bq) + 1640·W·pps) >> 10`, then `Bp = Bq` and `Bq = B_new`.
4. Choose `m`: if `A_new > nominal`, `m = A_new`; otherwise `m = B_new`. (An earlier revision wrapped the second case in `max()`; that was a typo.) This was checked against the reference on 24 consecutive rate checks of a config-0 stream; the alternatives "always `B_new`" and `max(A_new, B_new)` matched only 12 and 14 of them. The rules differ only when `A_new > nominal`, which happens only when `pad > 0` makes `A_new ≠ B_new`. If "always `B_new`" seems to match, check that A accumulates `W + pad` and B only `W`, each with its own two-step history (`A`/`Ap` and `Bq`/`Bp`).
5. `v = nominal − m + target`.
6. Set `target`:
   - if `nominal > v`: `target = nominal`;
   - otherwise, for configs 3 and 4: `target = min(v, (5·nominal) >> 2)`;
   - otherwise (configs 0 and 1): `target = v`.

`target` feeds the frame-size rule (above), the bit pool (§8.2) and the max-bits table choice (§8.3). `nominal` only feeds the frame-size rule and the clamps.

**Set-bitrate operation** (the wrapper calls it after every packet, `a2dp-integration.md` §5 step 7):

1. Clamp the requested bitrate b to the range derived at create time:
   - configs 0, 1 and 4: [100000·nch, 300000·nch];
   - config 3: [90000, 500000·nch].

   The create bitrate is clamped to the same range (§10.G).
2. Set `nominal = target = b` and `A = Ap = Bq = Bp = b`.

This resets the filter. Under the wrapper loop, therefore, the rate check only acts within one packet, and it changes the next packet's sizing only when the set-bitrate operation is not called in between.

The stage vectors (encoder called once per packet with all remaining input, no flush, no set-bitrate call) need §8.1a to reproduce. The `tests/vectors/r2` runs do not.

**Wrapper loop.** This is the per-packet loop of `a2dp-integration.md` §5, restated in terms of the calls above. **Correction (§10.A, §10.C):** P is the period in **interleaved** samples, `floor(period_ms · fs · 2 / 1000)`, which can be odd at 44.1 kHz, and in LL the deficit is zeroed after every packet.

1. Call encode with all buffered input.
2. If the period (P plus the deficit) has not been reached and no packet came out, wait for more input.
3. Otherwise:
   - if no packet came out, make the flush call;
   - add P minus the samples consumed to the deficit;
   - call set-bitrate with `table_bitrate − floor(table_bitrate / 4)` if the deficit is above 96 interleaved samples (48 sample frames), otherwise with the table bitrate.

### 8.2 Bit pool per block

Clamp the current **target** bitrate R (bit/s for the whole stream, §8.1a):

| config | clamp range |
|---|---|
| configs 0 and 1 | [100000·nch, 350000·nch] |
| config 3 | [90000, 550000·nch] |
| config 4 | [100000·nch, 210000·nch] (verified, §10.F) |

Here nch is the channel count of the group (2 for stereo). The lower clamp for config 3 is not scaled by nch. Note that this clamp is wider than the set-bitrate clamp of §8.1a, so it only matters when `target` has risen above `nominal`.

The pool, in bits per block for both channels together, is

`P = floor(R / fs)·S + floor((R mod fs)·S / fs)`.

Examples: config 0 at 364 kbps / 48 kHz gives 30; config 1 at 265 kbps gives 22; config 3 at 420 kbps gives 70.

**Alternate flag.** Config 3 with a 2-channel group and R ≤ 179999 sets an "alternate" flag; any other case clears it. The flag only changes the digital-black step (§8.6 step 5) and makes the 96 kHz allocator unusable. **Superseded by §10.E:** the flag is reachable (R2 HQ level-1 rows at 104, 128 and 142 kbps), is decided on the nominal bitrate, zeroes channel 1's scale factors and skips one digital-black step.

### 8.3 Max-bits table

Let `r` be the clamped target R of §8.2 (not the nominal rate), or 2R for mono. The tables are `allocator.int32.max_bits_*`; each has 2·S entries, channel 0 bands then channel 1 bands. The result is the per-band ceiling `maxb[i]`.

**S = 4 (configs 0 and 1):**

| r | table |
|---|---|
| ≤ 319999 | `max_bits_4band_320` |
| ≤ 369999 | `max_bits_4band_370` |
| otherwise | `max_bits_4band_max` |

`max_bits_4band_250` is present in the reference but never selected. Because the largest 4-band ceiling is 9, configs 0 and 1 never allocate more than 9 bits, so the reduced allocation in §7.2 is at most 8.

**S = 8 at 44.1 / 48 kHz:**

| r | table |
|---|---|
| ≤ 249999 | `max_bits_8band_250` |
| ≤ 319999 | `max_bits_8band_320` |
| ≤ 369999 | `max_bits_8band_370` |
| ≤ 419999 | `max_bits_8band_420` |
| ≤ 519999 | `max_bits_8band_520` |
| otherwise | `max_bits_8band_max` |

**S = 8 at 88.2 / 96 kHz:**

| r | table |
|---|---|
| ≤ 419999 | `max_bits_8band_96k_420` |
| ≤ 499999 | `max_bits_8band_96k_500` |
| ≤ 599999 | `max_bits_8band_96k_600` |
| ≤ 649999 | `max_bits_8band_96k_650` |
| otherwise | `max_bits_8band_96k_max` |

**Tonal override.** If the tonal flag (§8.4) is set, config 4 uses `max_bits_8band_420` (§10.F), and config 3 uses `max_bits_8band_max` at 44.1 / 48 kHz and `max_bits_8band_96k_max` at 88.2 / 96 kHz, whatever the rate. This override table is also the `maxb` used by §8.5 and §8.6.

### 8.4 Scale factors, tonal flag and bit need

Everything here is per frame, over all of the frame's blocks. Band index i runs over channel 0 bands 0..S−1, then channel 1 bands.

**Scale factor** `sf[i]`:

1. For every subband value x of band i in the frame (the filterbank output, §3), take `a = |x|` as an unsigned 32-bit value, so |−2^31| = 2^31.
2. Compute `j = min((a·2^24 + 2^31) >> 32, 2^31 − 1)`, using 64-bit arithmetic. This is `a/256`, rounded.
3. Start with `lim = 2` and `c = 0`. While `j > lim`, double `lim` and add 1 to `c`.
4. `sf[i]` is the largest c over all values in the band. It is 0 for a band of zeros or of very small values.

**Tonal flag** (S = 8 only; it is always clear when S = 4):

1. For each channel, let `mx` and `mn` be the largest and smallest sf among that channel's bands.
2. The flag is set if, for either channel, `mx − mn > 10` and at least 6 of its 8 bands have `sf < mx − 10`.

(The reference's rule is phrased as "more than 4 low bands sets the flag unless more than 2 bands are high"; with 8 bands it reduces to the rule above.)

**Offset table.** Let `rate_idx` be 0, 1, 2, 3 or 4 for fs ≤ 16000, ≤ 32000, ≤ 44100, ≤ 48000, or higher. The table depends on the create-parameter channel config (`a2dp-integration.md` §3.1). Normal stereo is channel config 4, and it uses the **dual-mono** tables.

| S | channel config | table (`allocator.int8.*`) | row start |
|---|---|---|---|
| 4 | 2 | `offset_4band_stereo` | 8·rate_idx |
| 4 | anything else | `offset_4band_dual_mono` | 8·rate_idx |
| 8 | 2 | `offset_8band_stereo` | 16·rate_idx |
| 8 | anything else, tonal flag set | `offset_8band_dual_mono_tone` | 16·rate_idx |
| 8 | anything else, tonal flag clear | `offset_8band_dual_mono` | 16·rate_idx |

Let `off[i]` be the table entry at row start + i. Each table holds a row only for the `rate_idx` values its size covers (`table-extraction.md` §3.9).

**Bit need:**

- If `sf[i] = 0`, `need[i] = −5`.
- Otherwise let `d = sf[i] − off[i]`. If d > 0, `need[i] = (d + 1) >> 1`; otherwise `need[i] = d`.
- Let `M = max(0, largest need[i])`.

### 8.5 Water level ("bit slice")

Inputs: the pool P (§8.2), `need[]` and `maxb[]`. Start with `L = M + 1`, `acc = 0` and `prev = 0`.

- If `M ≤ −12`, the slice is `L − 1` when P = 0, otherwise `L`. (With M clamped at 0 above, this branch cannot be taken; it is kept for completeness.)
- Otherwise repeat:
  1. Let `cnt` be the number of bands with `need[i] > L` and `maxb[i] + L − 1 > need[i]`, plus 2 for each band with `need[i] = L`.
  2. Add `prev` to `acc`.
  3. Stop if `cnt + acc ≥ P`, or if `L ≤ −10`.
  4. Otherwise set `prev = cnt`, subtract 1 from L, and repeat.
- After the loop, add `cnt` to `acc`. The slice is `sl = L − 1` if `acc ≠ P`, or `sl = L − 2` if `acc = P`.

### 8.6 Distribution

The output `out[i]` is the frame's allocation B[i] (§5, §7).

**1. Start.** Set every `out[i] = 0`, then `out[0] = 2` and, for 2 channels, `out[S] = 2`. Set `tot = 0`. Then for each band i in index order:

- if `need[i] ≥ sl + 2`, set `out[i] = min(need[i] − sl, maxb[i])`;
- set `out[i] = max(out[i], minb[i])`, where `minb` is all zeros for music;
- add `out[i]` to `tot`.

**2. First fill** (only if `tot < P`). For band k = 0..S−1, and within that for each channel in order (so channel 0 band k, then channel 1 band k):

- if `out > 1` and `maxb > out`, add 1 to `out` and to `tot`;
- otherwise, if `need = sl + 1`, `P > tot + 1` and `maxb > 0`, set `out = 2` and add 2 to `tot`. This can lower an existing value of `out` without adjusting `tot` for the old value; that is faithful to the reference.
- Stop as soon as `tot ≥ P`.

**3. Second fill** (only if still `tot < P`). Visit i = 0, 1, …, 2S−1, 0, 1, … for at most 32 visits in total:

- if `out[i] > 1` and `maxb[i] > out[i]`, add 1 to `out[i]` and to `tot`;
- otherwise, if `out[i] = 0` and `maxb[i] > 0`, set `out[i] = 2` and add 2 to `tot`;
- stop as soon as `tot ≥ P`.

**4. Trim** (only if `tot > P`). For i = 2S−1 down to 0: if `out[i] > 2`, subtract 1 from `out[i]` and from `tot`. Stop as soon as `tot ≤ P`. A single pass only.

**5. Digital black** (config 3 only):

1. Let `s0` and `s1` be the sums of `out[i]` over the channel 0 and channel 1 bands whose `sf[i] ≠ 0`.
2. Set `out[i] = 0` wherever `sf[i] = 0`. If `s0 + s1 = 0`, stop here (the frame is all zero, which uses the zero-allocation escape of §7.2).
3. With the alternate flag (§8.2) clear: if `s1 = 0`, set `out[S] = 2`; if `s0 = 0`, set `out[0] = 2`. Add these to the total.
4. **UNCERTAIN** (derived but never triggered in testing): if the total is now between 1 and 6, spend `7 − total` extra bits, first on bands with `out > 0`, raising each towards its `maxb`, then on all bands in index order. A band that would end on 1 is raised to 2, which costs one more bit.

`out[i]` is never 1. (The configs 0/1 codebook substitution in §7.4 is a bitstream detail and does not change the allocation.)

As a cross-check, these rules give silence allocations of 4/4/4/3 per channel on config 0 at 364 kbps (pool 30, slice −8, then the first fill) and all zero on config 3, matching what was observed.

### 8.7 Notes for implementers

- A frame's allocation uses only that frame's subband samples: the filterbank output for all of its blocks, computed before quantisation.
- `P_est` in the frame-size rule is the pool from the **previous** allocation (initially the create-time pool), so a bitrate change reaches frame sizing one frame late.
- Stage-vector `packets.bin` streams totalling 126 packets were also reproduced from raw PCM, driving the encoder with all remaining input per call, no flush and no set-bitrate call; this exercises the rate check (§8.1a). The spec side reports five stage runs, but `tests/vectors/stages/` has six; see QUESTIONS.md "from codec-core allocator" item 1.
- With §8 implemented, the `frames.json` allocations and block counts in the stage vectors become a check on the allocator rather than a required input.

### 8.8 Packets smaller than the bitrate needs

The reference has no special handling for this:

- It never limits carry-over and never drops frames. Carried bytes are subtracted from the free space the frame-size rule sees, so with little room it picks small frames (8 or 16 blocks) and closes packets early. The carry stays below about one frame. In reference runs at 364 kbps the carry never went above 102 bytes, even with 64-byte packets.
- The encoder consumes only the PCM it actually codes. The rest stays with the caller, so the caller's backlog grows every period when packets are too small. At 364 kbps the reference coded every block with 400-byte packets, but only about half with 200-byte packets and about an eighth with 64-byte packets.
- The rate check (§8.1a) raises the internal rate when packets come out short. The only reduction anywhere is the caller's cut to three quarters of the bitrate when the deficit grows (the wrapper loop in §8.1a).
- The encoder is never told the packet size when it's created, so it can't refuse an undersized one then. Per call, it rejects output windows of 11 bytes or less, consuming nothing, and windows over 2016 bytes. Anything in between behaves as above.
- The reference source never gets into this state, because it picks the bitrate from the packet size and period (the MTU rule in `a2dp-integration.md` §4), so its packets are always at least 328 bytes.

Recommendation: choose the bitrate from the packet size the same way, and treat a growing backlog as a configuration error rather than dropping frames.

---

## 9. What is verified

| stage | config 0 | config 1 | config 3 (48k) | config 3 (96k) |
|---|---|---|---|---|
| filterbank | ✔ | ✔ | ✔ | ✔ |
| quantiser + dither | ✔ | ✔ (no dither) | ✔ (incl. 10–16 bit bands) | ✔ |
| predictor | ✔ | ✔ (ch0 only) | ✔ | ✔ (some bands off) |
| allocation coding, absolute + diff | ✔ | ✔ | ✔ (incl. escapes, zero escape) | ✔ |
| codewords | ✔ | ✔ | ✔ | ✔ |
| packet header, continuation, padding | ✔ | ✔ | ✔ | ✔ |
| end to end, given allocations and frame sizes | ✔ (48k, 44.1k) | ✔ | ✔ (48k, 44.1k) | ✔ (96k, 88.2k) |
| bit allocation, frame sizing and rate check, raw PCM → packets (§8) | ✔ (r2 R1 vectors + 5 table rows) | ✔ (5 LL table rows) | ✔ (r2 R2 vectors + 2 rows) | ✔ (2 rows) |

Out of scope / not done:

- config 4 at 88.2/96 kHz (config 4 at 44.1/48 kHz is covered in §10.F)
- config 2 (speech)
- mono
- joint stereo
- metadata chunks
- the voice / true-wireless packet formats
- end-of-stream flush behaviour (the per-packet flush call of §8.1 is covered; a final drain at stream end is not)

---

## 10. Verified parity cases and corrections (batch 5)

Status: clean-side summary of a spec-side verification round, 2026-10-13. Where this section differs from §1–§9 it supersedes them.

**Method.** Each case was run on the reference encoder, driven by the packet loop of §10.A–§10.C. An independent model following §3–§8 plus the corrections below was compared with the output byte for byte: whole streams from raw PCM, with no reference-side state fed in. Every gap found is listed here with the smallest change that makes the model byte-exact on every vector.

**Result.** All 44 streams in `tests/vectors/parity/` (see its README), plus the 21 earlier regression runs, are byte-exact with these corrections.

### 10.0 Summary

| case | what was checked | verdict |
|---|---|---|
| 1a | config 0, 48 kHz, R1 sink, HQ table index 0, levels 1–4 (levels 5–7 use the level-4 row) | confirmed |
| 1b | config 0, 48 kHz, R1 sink with QHS, HQ table index 2, levels 1–5 (760-byte packets) | confirmed |
| 1c | config 0, 44.1 kHz, levels 1–4 | codec confirmed; packet loop needed the period rule (A) and the chunking rule (B) |
| 2a | config 1 on the R1 LL row (265 kbps / 10 ms / 348 B), 48 kHz | confirmed |
| 2b | the same at 44.1 kHz | needed the LL loop rule (C) |
| 2c | which config an R1 sink gets in LL | §1 was wrong: config 0, not config 1 (D) |
| 3 | config 3 on every 96 kHz row (HQ indices 5 and 6, levels 1–5), and an R1 sink at 96 kHz | confirmed for levels 2–5; level 1 (128k, 142k) needed the alternate flag (E) |
| 4 | config 4 (R2 low latency) | reachable; fully specified in F; byte-exact on 10 LL rows at 48 and 44.1 kHz |
| 5 | rate control and allocation away from level 3 | pool, max-bits and frame-size rules confirmed on every row; see E, G, H |

### 10.A Period length in interleaved samples

§8.1a's wrapper loop states the period as samples per channel per packet, and `a2dp-integration.md` §4 lists samples per channel per table row. Those numbers are for 48 kHz (96 kHz on the 96 kHz rows) and are wrong at 44.1 kHz.

**Rule.** Keep every counter of the packet loop in **interleaved** samples (both channels counted).

- The period is `Pi = floor(period_ms · fs · 2 / 1000)`. The reference computes this in single-precision floating point; for every table row at 44.1, 48 and 96 kHz that gives the same value as exact integer arithmetic.
- At 44.1 kHz `Pi` can be **odd**: 18 ms → 1587, 16 ms → 1411, 15 ms → 1323, 19 ms → 1675. Others: 14 ms → 1234, 12 ms → 1058, 10 ms → 882.
- A packet is due when `consumed ≥ Pi + deficit`. After the packet, `deficit = deficit + Pi − consumed`. `consumed` counts interleaved samples, so it is always even.
- The set-bitrate rule's "deficit above 96" is in the same units.
- The table's samples-per-channel column is **not** used for the period. Do not round `Pi` to whole stereo frames: using 793 frames (1586) instead of 1587 makes the 281 kbps / 18 ms row diverge at packet 26.

At 48 and 96 kHz `Pi` is always even and the deficit stays 0 in every vector, so this only shows at 44.1 kHz. It is consistent with `a2dp-integration.md` §4.3 ("interleaved samples per packet") and §7J ("samples per packet = period × rate").

### 10.B The output depends on input chunking

This replaces "The output does not depend on chunking" in §8 (and the matching sentences in `a2dp-integration.md` §3.4 and §5, and `tests/vectors/r2/README.md`).

The state machine of §8.1 is exact and reproduces every chunking tried. But two of its properties make the output depend on where the caller's chunks end:

1. In an encode call, the encoder analyses only a whole, **even** number of blocks out of the input it is offered (§8.1 step 2). Leftover input is not consumed and must be offered again in the next call.
2. The packet loop compares `consumed` with the period only **between** encode calls. So the chunk boundaries decide at which call a period is found to have ended, and therefore when the flush call happens and how many blocks the flushed frame holds.

Observed:

- At 44.1 kHz the output always depends on the chunk size, because `Pi` is never a whole number of block pairs. `c0_44k_L3` (44-frame chunks) and `c0_44k_L3_chunk441` (441-frame chunks) differ from packet 0.
- At 48 kHz, chunks of 5, 7, 9, 16, 24, 48, 56, 96, 112, 336 and 672 frames gave identical output on the parity signal. Chunks of 13 and 100 frames did not, both on the parity signal and on `tests/vectors/r2/noise.s32le`. `c0_48k_L3_chunk13` differs from `c0_48k_L3`.

**Rule.** The output depends on how the input is split into encode calls. The reference source feeds 1 ms chunks: 48 frames at 48 kHz, 44 frames at 44.1 kHz, 96 frames at 96 kHz. Each vector states its chunk size, and a bit-exact test must use that chunk size. For a live source this is not a correctness problem: any chunking produces a valid stream.

### 10.C LL packet loop: the deficit is zeroed after every packet

When the source runs in LL mode (any sink, any config), the loop has one extra step, after the deficit update and before the set-bitrate call:

- if the deficit is not 0, set it to 0.

So in LL the deficit is always 0 at the start of each period and the three-quarter bitrate cut never fires. HQ mode does not do this.

The reference also makes one more flush call at this point, on an output window that is already full. In every run it produced no bytes and changed no encoder state, so a clean implementation should not make that call.

At 48 kHz the LL deficit is always 0 anyway. At 44.1 kHz it matters: on the R1 LL row the loop gives 91 packets per second with the reset and 99 without, and the bytes differ from packet 2. This agrees with `a2dp-integration.md` §7K ("LL never carries a deficit").

### 10.D An R1 sink in LL gets config 0

This corrects the config-1 row of the §1 table. It agrees with `a2dp-integration.md` §7J.

- The source creates the encoder with the LL profile (6) **only** when the mode is LL **and** the sink is R2. Otherwise it uses the HQ profile (3), including LL with an R1 sink.
- An R1 sink in LL therefore gets **config 0** (frame type 1): the HQ algorithm, on the LL table row (265 kbps, 10 ms, 348-byte packets), with the LL loop of 10.C. The 8-byte header still carries the LL mode byte (0xAD) and the LL cfg byte.
- The source-side choice is UNCERTAIN in the sense that it couldn't be observed end to end on a sink. The resulting streams (`c0_48k_ll`, `c0_44k_ll`) are reproduced exactly by the config-0 rules.
- Config 1 (low-power, frame type 2) is what the encoder produces when created with profile 1. That is verified (`c1_48k_ll`, `c1_44k_ll`, and the five earlier LL rows), but the reference source does not appear to use it for A2DP.
- Creating with profile 2 or 6 and the R1 decoder level also reports config 1, but gives a **different** stream from profile 1: not the low-power stream, and not modelled. Do not use that combination.

Corrected §1 table row: "LL, R1 sink: config 0 (the source creates the encoder with the HQ profile); config 1 only if created with profile 1."

### 10.E The alternate flag at low config-3 rates

This replaces the "Alternate flag" paragraph of §8.2 and step 3 of §8.6 step 5. The path is reachable: R2 HQ table index 1 level 1 (104 kbps), index 5 level 1 (128 kbps) and index 6 level 1 (142 kbps) all use it.

**When it applies.** The flag is set for a frame when all of these hold, and is clear otherwise:

- the config is 3;
- the stream has 2 channels;
- the **nominal** bitrate (the create or set-bitrate value, not the rate-controlled target of §8.1a) is at most 179999.

It is evaluated for every frame. It is never set for config 4.

**What it changes.** Two things, both required:

1. **Channel 1 scale factors are zeroed.** After the tonal-flag decision (§8.4) and before bit need, every channel-1 scale factor `sf[S..2S−1]` is replaced by 0. Every channel-1 band therefore gets `need = −5`. The tonal flag itself is decided on the original scale factors.
2. **One digital-black step is skipped.** In §8.6 step 5:
   - sub-step 2 zeroes every band whose (replaced) scale factor is 0, which now includes all of channel 1;
   - sub-step 3 (setting `out[S] = 2` when channel 1's sum is 0, and `out[0] = 2` when channel 0's sum is 0) is **skipped** entirely;
   - sub-step 4 (the top-up when the total is 1 to 6) still applies.

Distribution steps 1–4 of §8.6 are unchanged. The net effect is that channel 1 is sent with an all-zero allocation in every frame at these rates.

Dropping either change breaks the 104k and 128k streams within a few packets. The 96 kHz allocator, which §8.2 called unusable with the flag, works with the same two changes.

### 10.F Config 4 (R2 low latency)

**Reachability.** An R2 sink in LL mode, at 44.1 or 48 kHz, gets the LL create profile (6) and the R2 decoder level, which selects config 4 (frame type 4). The source refuses LL at 96 kHz. Verified on LL table indices 1–5 at 48 kHz and indices 1–2 at 44.1 kHz (`c4_*` vectors), using the LL loop of 10.C.

Everything not listed here is as for config 3 at 44.1/48 kHz.

**1. Filterbank: 8-band, 128-tap prototype.** Use `filterbank_8band_prototype_128.coef` (128 values `P128[0..127]`), now verified identical to the reference's table. The channel state holds the last 128 input samples. §3.2 step 2 becomes, for i = 0..15:

- `acc = −Σ_{k=0}^{7} P128[i+16k] · h(i+16k)`, accumulated in 64 bits with wraparound;
- `u[i] = R64( lo64(acc · 64) )`, as before.

Steps 3–6 (cosine-modulation front end, 8-point DCT, post-processing, output) are unchanged. Verified on 10,192 stereo blocks of intermediate values.

**2. Quantiser (§5).** Normal mode with dither, exactly as config 3. The dither shifts are `s_k = 2k`.

**3. Predictor (§6).** Normal mode: both channels, all 8 bands run every block. Zero-tap counts `predictor_taps.8band_48k`, the same as config 3 at 44.1/48 kHz.

**4. Allocation (§8.2–§8.6).**

- Pool clamp: `[100000·nch, 210000·nch]` (the §8.2 row is correct). The pool formula is unchanged, with S = 8.
- Max-bits: the S = 8, 44.1/48 kHz threshold chain of §8.3.
- **Tonal override:** when the tonal flag is set, use `max_bits_8band_420`, not `max_bits_8band_max`. This table is also the `maxb` used by §8.5 and §8.6.
- Offset tables, scale factors, tonal flag, bit need, water level and distribution steps 1–4: as for config 3.
- **No digital-black step** (§8.6 step 5 is config 3 only) and **no alternate flag**.
- With these tables config 4 never allocates more than 9 bits per band at any rate a source can reach (target at most 5/4 × 320000).

**5. Rate check and set-bitrate (§8.1a).**

- The set-bitrate clamp is `[100000·nch, 300000·nch]`, as for configs 0 and 1.
- In step 6 of the rate check, config 4 takes the config-3 branch: `target = min(v, (5·nominal) >> 2)`. Read "for config 3" there as "for configs 3 and 4".
- The frame-size rule (§8.1) is unchanged.

**6. Bitstream (§7).**

- Packet byte 1 = 4, plus 0x80 if the packet ends with padding.
- **Absolute allocation (§7.2).** The 4-bit header is the sample-rate code (44.1 kHz → 0, 48 kHz → 2) plus 8 for 2 channels. Then the 8 band pairs, using the **configs 0/1 symbol coding** (symbol `(α << 4) | β`, no escapes) but the **8-band table choice** (`pair_table_8band`). There is **no** zero-allocation escape.
- **Differential allocation (§7.3).** The **configs 0/1 rule**: write `table[(v − pv) & 15]`, with no clamping, no escapes and **no** zero-allocation escape.
- **Codewords (§7.4).** As config 3, **without** the configs 0/1 top-band substitution.
- Packet layout, continuation and padding: unchanged.

Since the allocation never exceeds 9 bits, ρ is at most 8 and the escape-free absolute coding is always sufficient.

**Not covered:** config 4 at 88.2/96 kHz (the encoder accepts it, but the source never creates it), mono, and the LL rows with split-TX doubling.

Corrected §1 table row: "LL (profile 6), R2 sink, 44.1/48k: config 4, frame type 4, 8-band cosine-modulated filterbank with the 128-tap prototype, 8 subbands, 8 samples per block, normal predictor mode."

### 10.G Create-time bitrate clamp

The create bitrate is clamped at create time to the same range as set-bitrate (§8.1a):

| config | range |
|---|---|
| 0, 1, 4 | [100000·nch, 300000·nch] |
| 3 | [90000, 500000·nch] |

`nominal` and `target` start at the clamped value, and the create-time `P_est` is computed from it. Seen on the R1 + QHS level-1 row: 110000 is raised to 200000. In the parity runs the first packet was the same either way, but the clamped value is what the encoder reports.

### 10.H The three-quarter bitrate cut

The loop's "deficit above 96 → set bitrate − bitrate/4" step fired in only one run of the whole set, the R1 + QHS level-1 row. There it asked for 82500, which the set-bitrate clamp raises back to 200000, so it had no effect. With the period rule of 10.A and 1 ms chunks, the deficit stays at or below 96 on every other table row at every rate. The cut therefore has **no verified effect on any byte**; it only matters when the caller under-feeds the encoder. (Derived, not separately tested: in stereo the 200000 clamp floor makes the cut a no-op for configs 0, 1 and 4 whenever the table bitrate is at most 266666.)

### 10.I Confirmed unchanged

Across all 44 streams these needed no change: §3.1, §3.2 (256-tap), §4–§6, §7 for configs 0, 1 and 3, the 44.1 kHz allocation header codes (config 0: 0x8; config 3: 0x8), the 96 kHz predictor and max-bits chain (including `max_bits_8band_96k_650` at 646 kbps and `max_bits_8band_96k_max` at 820 kbps), §8.1 frame sizing with its carry and padding histories, §8.1a's A/B filters, and §8.2's pool formula. HQ at 96 kHz with an R1 sink gives config 3 (§1 step 5), checked at 364 kbps / 14 ms. This also answers "from codec-core allocator" item 2: the allocator and frame-size rule are confirmed at 44.1 kHz (`rate_idx` 2). 88.2 kHz is still not covered by a parity vector.

### 10.J Verification status per case

| case | vectors | status |
|---|---|---|
| A, period in interleaved samples | `c0_44k_L1`–`L4`, `c3_44k_i1_*`, `c4_44k_*` | byte-exact |
| B, chunk dependence | `c0_48k_L3_chunk13`, `c0_44k_L3_chunk441` (against `c0_48k_L3`, `c0_44k_L3`) | byte-exact with the stated chunk sizes |
| C, LL deficit reset | `c0_44k_ll`, `c1_44k_ll`, `c4_44k_*` | byte-exact; without the reset, 44.1 kHz LL diverges at packet 2 |
| D, R1 sink in LL → config 0 | `c0_48k_ll`, `c0_44k_ll` | byte-exact as config 0. The source's profile choice is from analysis of the source, not from running it |
| E, alternate flag | `c3_48k_i1_L1`, `c3_96k_i5_L1`, `c3_96k_i6_L1` | byte-exact; whether the sub-step-4 top-up fires in these runs is not reported |
| F, config 4 | `c4_48k_i1_L1` … `c4_44k_i2_L1` (10 runs) | byte-exact, 48 and 44.1 kHz |
| G, create clamp | `c0_48k_qhs_L1` | consistent; no byte depends on it in the vectors |
| H, three-quarter cut | `c0_48k_qhs_L1` | fires once, no effect after the clamp |
| config 0 / 3 / 1 rows | every other parity vector | byte-exact |

### 10.K Config 4 above 9 bits (verified 2026-10-13)

When config 4's target reaches 420000 (nominal ≥ 336000 after the 5/4 cap), the §8.3 chain would pick the 520 (or Max) max-bits table, which allows bands of more than 9 bits. Config 4 has no escapes, so such an allocation can't be coded. The reference encoder doesn't guard against this: with loud one-sided input it gives a band 10 bits, writes that band's allocation with zero bits, and produces an undecodable packet. **Rule for a correct encoder:** for config 4, whenever the chain would pick the 520 or Max table, use the 8-band 420 table instead. This matches the reference byte for byte on every packet before its first 10-bit band, and keeps every packet decodable.
