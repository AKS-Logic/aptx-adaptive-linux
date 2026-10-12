# aptX Adaptive A2DP integration (R1 / R2.x, source side)

Status: clean-side spec, 2026-10-12.

This describes what an A2DP source must do around the aptX Adaptive encoder: capability negotiation, encoder configuration, the per-packet loop, packet framing, and the adaptive behaviour driven by the sink. The internals of the encoder (the bitstream itself) are covered separately.

The capability layout in §1 also draws on the public Linux Foundation / CodeAurora Android Bluetooth sources; see `research-a2dp-caps-public.md`. Everything else was observed in the behaviour of the reference encoder and confirmed by the reference vectors in `tests/vectors/r2/` where noted.

Anything marked **UNCERTAIN** was inferred and has not been confirmed by running the reference.

---

## 0. Big picture

On a reference Android source the flow is:

1. The Bluetooth host negotiates the over-the-air (OTA) codec capability element (§1).
2. The negotiated result picks an encoder profile (HQ or LL), sample rate, channel configuration and decoder compatibility level.
3. A quality level selects a row from built-in tables (§4). The row gives the bitrate, packet period, samples per packet and packet size.
4. PCM is fed to the encoder, which produces one compressed packet per period (§5).
5. Each packet is byte-swapped in 16-bit units and given an 8-byte header (§7).
6. A separate packetiser adds the RTP framing. The encoder stage itself writes **no** RTP header (§7.2).
7. Sideband messages from the sink drive level and mode changes (§8). They cover HQ↔LL switches, RSSI, link count, Wi-Fi activity and QHS.

For a Linux source talking to an R1 sink such as the B&W Px7 S2e, the minimum is:

- the OTA configuration from §1.5;
- a fixed HQ level 3: 48 kHz, 364 kbps, 14 ms period, 672 samples per channel per packet, 656-byte packets;
- an encoder at decoder compatibility level R1 ("1.1 sink");
- the 16-bit byte swap;
- the 8-byte header;
- an RTP header.

---

## 1. OTA codec capability element (vendor 0x000000D7, codec 0x00AD)

### 1.1 Layout

LOSC is 42: a media-type byte, the codec type 0xFF, then 40 bytes. The offsets below count from the first vendor-id byte (the "body"). There are two public layouts. The capability-extension block only exists from R2 on.

| body off | R2.x layout | R1 layout |
|---|---|---|
| 0..3 | vendor id 0x000000D7, little-endian | same |
| 4..5 | codec id 0x00AD, little-endian | same |
| 6 | bits 7..3 (mask 0xF8): sample-rate bits. Bits 2..0 (mask 0x07): "source type" | same |
| 7 | channel-mode bits (mask 0x3F) | same |
| 8 | TTP LL byte 0 | same |
| 9 | TTP LL byte 1 | same |
| 10 | TTP HQ byte 0 | same |
| 11 | TTP HQ byte 1 | same |
| 12 | TTP TWS byte 0 | same |
| 13 | TTP TWS byte 1 | same |
| 14 | reserved (0) | reserved (0) |
| 15 | **capability-extension version** (1 for R2) | **end marker byte 0 = 0x00** |
| 16..19 | supported-features word, u32 little-endian | 16 = **end marker byte 1 = 0xAA**, then zero |
| 20..23 | setup preferences 1..4 | zero |
| 24, 25 | end marker 0x00, 0xAA | zero |
| 26..39 | reserved, zero | zero |

How to tell the two layouts apart:

- **R1:** body[15] is 0x00 and body[16] is 0xAA.
- **R2:** body[15] is 0x01, and the end marker 0x00 0xAA sits at bytes 24..25.

**Check body[15] before reading the features word.** On an R1 sink the features word would read as 0x000000AA, which would decode as "sink 2.2 decoder + split TX + …". That reading is wrong.

### 1.2 Bit values

**Channel mode** (byte 7). These values come from public sources.

| bit | meaning |
|---|---|
| 0x01 | mono |
| 0x02 | stereo |
| 0x04 | TWS stereo |
| 0x08 | joint stereo |
| 0x10 | TWS mono |
| 0x20 | TWS+ |

The public source prefers channel modes in this order:

1. TWS+
2. stereo
3. TWS stereo
4. joint stereo
5. mono
6. TWS mono

A configuration must have exactly one channel-mode bit set.

**Sample rate** (byte 6, mask 0xF8):

- **0x10 means 48 kHz.** All public sources agree on this.
- The public LineageOS 18.1 constants header gives **0x08 = 44.1 kHz** and **0xA0 = 96 kHz**.
- Another public variant uses 0x40 for 44.1 kHz. (`research-a2dp-caps-public.md` lists the variants slightly differently again.)
- **UNCERTAIN:** the reference encoder only ever sees a rate value already decoded by the host, so it cannot settle this.
- If 0x08 is 44.1 kHz, that fits: the Px7's aptX classic endpoint advertises both 48 and 44.1 kHz.
- Only the 48 kHz bit matters for this project.

**Source type** (byte 6, mask 0x07):

- Public values are 0x00, 0x02, 0x04 and 0x06. A public comment labels them "AVRCP custom / Q2Q / NQ2Q / reserved".
- The public software source sends 0x02. The public hardware-offload source sends 0x00.
- The encoder does not use this field.

**Features word** (R2 only, u32 little-endian, so body byte 16 is the low byte). Only its two low bytes affect the encoder. Below they are called **features byte 0** (body 16) and **features byte 1** (body 17).

Bits of features byte 0:

| bit | meaning |
|---|---|
| 0 | TWM (true wireless mirroring) support |
| 1 | sink is R2.1 |
| 2 | split TX |
| 3 | split ACK |
| 4 | source 2.2 encoder |
| 5 | source 2.2 decoder |
| 6 | sink 2.2 encoder |
| 7 | sink 2.2 decoder |

Fields of features byte 1:

- **Bits 0..4: RSSI adjustment.** Values 0..15 are positive and clamped to at most 10. Values 16..31 are negative: sign-extend the low 4 bits and clamp the result to at least −10.
- **Bits 5..7: "ELL latency adjustment".** Used in §7.1.

Public feature words, for reference:

| version | features word |
|---|---|
| R2.0 | 0x0F000000 |
| R2.1 | 0x0F000003 |
| R2.2 | 0x0F000017 |

### 1.3 Sink version and encoder behaviour (confirmed)

The sink version decides the encoder's **decoder compatibility level**. That level changes the bitstream (see §3).

| condition | sink version | decoder compatibility level |
|---|---|---|
| no extension, i.e. R1 (extension version byte is 0) | 1.1 | **R1 ("1.1 sink")** |
| extension present, features bit 1 clear | 2.0 | **R2** |
| extension present, features bit 1 set | 2.1 | **R2** |
| extension present, features bits 1 and 7 both set | 2.2 | R2, but see the note below |

For a 2.2 sink, the reference switches to a separate R3 encoder (not covered by this spec) **only if** the sample rate is 44.1 kHz. Otherwise it stays on the R2 encoder.

The level choice itself only looks at the extension version byte: R2 if it equals 1, R1 for any other value.

An R1 sink also has these effects:

- the encoder channel configuration is forced to stereo, whatever channel mode was negotiated;
- TWM and split TX are off;
- the HQ table index is 0, or 2 with QHS (§4).

### 1.4 What the TTP bytes are

The six TTP bytes (body 8..13) never reach the encoder, so they cannot change the encoded bitstream. The reference instead configures its encoder with fixed min/max **sink buffer** values {20, 50, 20, 50, 20, 50} (LL, HQ, TWS pairs); see §7.1.

Nothing in the encoder reveals the units of the TTP bytes.

What the public Android source does with them:

- When its encoder supports R2, it copies the sink's 18-byte "aptx_data" block (body 8..25) **verbatim** into its SetConfiguration.
- When its encoder is R1-only, it sends the fixed R1 block `50 64 64 64 ff ff 00 00 aa`.

The public defaults are LL 0x50/0x64 (80/100), HQ 0x64/0x64 and TWS 0xFF/0xFF. That fits a "latency target low/high in ms" reading. **UNCERTAIN.**

### 1.5 Px7 S2e: decoding its caps, and what to send back

The Px7 S2e advertises these caps (40 bytes):

`d7 00 00 00 ad 00 19 0a 37 6a 00 af 64 4b 00 00 aa 00 … 00`

| body | value | decoding |
|---|---|---|
| 0..3 | d7 00 00 00 | vendor 0x000000D7 |
| 4..5 | ad 00 | codec 0x00AD |
| 6 | 0x19 | Rate bits 0x18: 48 kHz (0x10) plus 44.1 kHz (0x08 per the LineageOS header, **UNCERTAIN**). Source-type bits 0x01: an odd value not in the public list, possibly an "AVRCP custom supported" flag. **UNCERTAIN**, and not used by the encoder. |
| 7 | 0x0a | stereo (0x02) + joint stereo (0x08) |
| 8, 9 | 37 6a | TTP LL 55 / 106 |
| 10, 11 | 00 af | TTP HQ 0 / 175 |
| 12, 13 | 64 4b | TTP TWS 100 / 75 |
| 14 | 00 | reserved |
| 15 | 00 | **end marker byte 0**: this is the R1 layout, with no capability extension |
| 16 | aa | **end marker byte 1** |
| 17..39 | 00 | reserved |

About body bytes 8..13 (`37 6a 00 af 64 4b`):

- They are vendor-tuned TTP values, not the public defaults.
- Their units, and the odd HQ 0/175 and TWS 100/75 pairs, are unexplained. **UNCERTAIN.**
- They do not affect the encoder bitstream (§1.4).

**Cross-check against `research-a2dp-caps-public.md`:**

1. Its tentative "R1 caps, no extension" reading is **correct**. It follows that the Px7 gets decoder compatibility level **R1** ("1.1 sink"), HQ table index 0, and a stereo channel configuration.
2. Its decoding of byte 6 needs one correction. The 0x01 is **not** a rate bit. The public parser masks the rate with 0xF8, so 0x01 lands in the source-type field.
3. Its reading of bytes 8..13 is right: they are the TTP pairs, just with non-default values.

**Recommended SetConfiguration** (40 bytes, the same length as the sink's caps):

```
d7 00 00 00 ad 00 10 02 37 6a 00 af 64 4b 00 00 aa 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```

- byte 6 = 0x10: 48 kHz, source type 0;
- byte 7 = 0x02: stereo;
- body 8..16: the sink's own aptx_data, echoed back.

Fallbacks if the sink rejects it (**UNCERTAIN** which one it prefers):

- (a) set byte 6 to 0x12, i.e. source type 2, as Android software sources do;
- (b) replace body 8..13 with the public R1 defaults `50 64 64 64 ff ff`.

Do not send 0x0a for the channel mode. It has two bits set.

---

## 2. Encoder configuration derived from negotiation

This section lists the settings the reference derives from the negotiated configuration, and how it handles each one.

**Sample rate.** 48000, 44100 or 96000 Hz. An unrecognised rate falls back to 48 kHz. 96 kHz also changes the table index (§4) and blocks LL.

**Channel mode → encoder channel configuration.** The encoder takes a channel-configuration code:

| negotiated mode | channel config code | channels |
|---|---|---|
| stereo, joint stereo, or TWS stereo | 4 | 2 |
| mono | 0 | 1 |
| dual mono | 1 | 2 |
| split-TX variant (R2 only) | 3 | 2 |
| TWS+ / earbud | rejected | |
| anything else | 4 | 2 |

Joint stereo is encoded with the same configuration (4) as stereo. Dual mono and TWS block LL and cap the quality level (§4). For an R1 sink the code is always 4 (§1.3).

**Profile.** HQ uses encoder profile code **3** and LL uses profile code **6**. The reference picks the profile from the user's HQ/LL choice, using this rule on a 16-bit mode value (the same bits appear in sideband message 5, §8.1):

- If (mode & 0x3000) == 0x1000: **HQ**.
- Otherwise, if mode & 0x2000 is set: **LL**. The exceptions fall back to HQ: 96 kHz, and dual mono or TWS stereo.
- Any other value: HQ.

**MTU.** If the link MTU is known, the packet budget is clamp(MTU − 8, 128, 2008) bytes; the 8 bytes leave room for the header in §7. The default budget is 2008. See §4 for how it limits the packet size.

**Sink buffering.** The reference uses min/max sink-buffer values of 20/50 for each of LL, HQ and TWS. Only the HQ and TWS minimums are actually used: a value of 0 keeps the default, a value above 256 becomes 20, and the result is clamped to the maximum, which stays at its internal default of 200 whatever is configured. These feed the TTP clamp in §7.1.

**Stereo↔mono fade.** A cross-fade length of 1..256 is accepted; the reference uses 255. It only matters for TWM stereo↔dual-mono transitions.

**On every (re)configuration** the source should:

1. set the quality level to 3;
2. recompute the latency state (§8.3);
3. look up the table row (§4);
4. re-create the encoder with the new parameters.

**Legacy bitrate ladder.** An older control path snaps a requested level onto this ladder. Its effect on the active tables is **UNCERTAIN**, and a Linux source does not need it.

| kbps | ms | samples/ch |
|---|---|---|
| 262 | 20 | 960 |
| 276 | 19 | 912 |
| 291 | 18 | 864 |
| 308 | 17 | 816 |
| 328 | 16 | 768 |
| 349 | 15 | 720 |
| 374 | 14 | 672 |
| 403 | 13 | 624 |
| 437 | 12 | 576 |

**Input PCM.** The reference accepts 44.1, 48 or 96 kHz, 1 or 2 channels, as 24- or 32-bit fixed point. It converts everything to the encoder's input format (§3).

---

## 3. Encoder interface behaviour

### 3.1 Create parameters

The encoder is created with these parameters:

| parameter | values |
|---|---|
| profile | 3 = HQ, 6 = LL. The reference encoder also accepts 0 and 5 (both treated as HQ) and 1, 2, 4 (speech profiles, out of scope). |
| secondary profile | 0 |
| channel config | 4 = stereo, 0 = mono, 1 = dual mono, 3 = split-TX variant |
| sample rate in Hz | 48000 |
| bitrate in bps | from the table in §4 |
| flag | always 1. Its meaning is **UNCERTAIN**. |
| decoder compatibility level | R1 ("1.1 sink") or R2, from §1.3 |

Creating an encoder with a sample rate or bitrate of 0 fails.

### 3.2 Bitstream variant ("config id")

The encoder derives a config id from the create parameters. It selects the bitstream variant; what each variant means belongs to the bitstream spec.

- For HQ at 48 kHz the config id is **0 at level R1** and **3 at level R2** (observed in the reference vectors).
- The visible effect is in the first packet bytes: R1 packets start `00 81 a1 …` and R2 packets start `00 83 a1 …`.

### 3.3 Lifecycle

1. Create the encoder with the parameters above.
2. To change profile, rate or channels, destroy it and create it again with the new parameters. There is no in-place change for these.
3. Encode packets as described in §3.4 and §5.
4. After every packet, apply the bitrate rule from §5. The encoder's set-bitrate operation clamps the requested value to a [min, max] range it derived at create time.

### 3.4 Encode call

**Input.** Interleaved L,R 32-bit signed integers. Each holds a **24-bit value, right-justified and sign-extended** (range −8388608..8388607). The reference gets 32-bit (Q31) samples from its audio framework and does an arithmetic shift right by 8. One call takes any amount of input and reports how many samples it consumed. Unconsumed samples must be kept and put in front of the next call's input.

**Output window.** The caller passes a window equal to the packet size from §4, plus at least 64 bytes of slack after it. The window must be between 12 and 2016 bytes.

**Behaviour:**

- When enough samples are buffered, the call writes one packet.
- Otherwise it consumes all the input, produces nothing, and reports "need more input". That is not an error.
- A **flush** call (no input) finishes and emits the packet currently being built.

In the reference vectors:

- Every HQ packet came out at exactly the packet size (656 bytes). The encoder fills or pads to the window, so the stream is effectively constant bitrate at the table size.
- The output does not depend on how the input is chunked: 1 ms and 14 ms chunks give byte-identical results.

---

## 4. Mode and level tables

Each table row holds:

- the bitrate in bps;
- the period in ms;
- PCM samples per channel per packet;
- the maximum packet size in bytes;
- a "cfg" byte, which goes into header byte 3 (§7).

Quality levels run from 1 to 7; there is no level 0. The default level is **3**.

The packet size is rounded down to a multiple of 4. If the result is bigger than the MTU budget from §2:

- the packet size becomes the budget, rounded down to a multiple of 4;
- the bitrate is recomputed from that size minus 25 kbps, roughly size·8/period − 25000. The exact formula is **UNCERTAIN**.

### 4.1 HQ table index (profile 3)

- **R1 sink:** index = 2 × QHS. So index 0 normally, or 2 when the controller reports QHS.
- **R2 sink:** index = 1 + QHS + 2 × TWM + 4 × (96 kHz).

Table rows, written as bitrate / period (ms) / samples per channel:

| idx | L1 | L2 | L3 | L4 | L5..L7 | packet bytes / cfg |
|---|---|---|---|---|---|---|
| 0 (R1) | 281k / 18 / 864 | 318k / 16 / 768 | **364k / 14 / 672** | 427k / 12 / 576 | 427k / 12 / 576 | 656 / 1 |
| 1, 3 | 104k / 50 / 2400 | 204k / 25 / 1200 | 270k / 19 / 912 | 340k / 15 / 720 | 427k / 12 / 576 | 656 / 1 |
| 2 (R1+QHS), 4 | 110k / 50 / 2400 | 235k / 25 / 1200 | 290k / 20 / 960 | 350k / 16 / 768 | 430k / 13 / 624 | 760 / 5 |
| 5 (96k) | 128k / 40 / 3840 | 255k / 20 / 1920 | 340k / 15 / 1440 | 420k / 12 / 1152 | 646k / 8 / 768 | 656 / 1 |
| 6 (96k + QHS) | 142k / 40 / 3840 | 284k / 20 / 1920 | 460k / 16 / 1536 | 570k / 13 / 1248 | 820k / 9 / 864 | L1–L2: 760 / 5; L3 and up: 960 / 6 |
| 7 | 255k / 20 / 1920 | 255k / 20 / 1920 | 340k / 15 / 1440 | 390k / 10 / 960 | 510k / 10 / 960 | 656 / 1 |
| 8 | 284k / 20 / 1920 | 284k / 20 / 1920 | 460k / 16 / 1536 | 570k / 13 / 1248 | 650k / 10 / 960 | L1–L2: 760 / 5; L3 and up: 960 / 6 |

HQ level caps:

- A 96 kHz link carrying 48 kHz content is capped at level 3.
- Dual mono or TWS is forced to level 1 at 96 kHz, and capped at level 2 otherwise.

### 4.2 LL table (profile 6)

- **R1 sink:** index = 2 × QHS.
- **R2 sink:** index = 1 + QHS + 2 × TWM + 4 × latency state, where the latency state is 0 or 1 (§8.3).

| idx | rows, level 1→7 (bitrate / ms / samples per channel) | packet bytes / cfg |
|---|---|---|
| 0 | 265k / 10 / 480 at every level | 348 / 0 |
| 1 | 220k / 12 / 576 (L1–2), then 270k / 10 / 480 | 348 / 0 |
| 2 | 290k / 15 / 720 (L1–2), 320k / 12 / 576 (L3), then 320k / 10 / 480 | 560 / 4 |
| 3, 7 | 214k / 13 / 624 | 348 / 0 |
| 4, 8 | 290k / 15 / 720 (L1–2), then 320k / 13 / 624 | 560 / 4 |
| 5 | 300k / 8 / 384 | 348 / 0 |
| 6 | 300k / 8 / 384 | 348 / 7 |

### 4.3 Derived values

- **Header period byte** = period_ms × 4, truncated to 8 bits.
- **Header mode byte** = **0xAE** for HQ, **0xAD** for LL.
- **Interleaved samples per packet** = ⌊period_ms × rate × channels / 1000⌋. That is 1344 for 14 ms of 48 kHz stereo.
- **Split TX:** with channel config 3 and split TX enabled, the sample count, packet size, period and period byte are all doubled.

---

## 5. Per-packet loop

The reference feeds input in 1 ms chunks. The chunk size does not change the output (§3.4), so a Linux source can feed whatever its audio graph delivers.

The loop keeps two counters, both in interleaved samples: the number **consumed** by the encoder since the last packet, and a carried **deficit** that starts at 0. For each block of input:

1. **Convert.** Append the new input to the carried-over PCM and convert it to the encoder's 24-bit format (§3.4).
2. **Fade (optional).** The reference may apply a fade-in at start-up, a mute during a profile change, or a stereo↔mono ramp. It uses raised-cosine curves, 0.5·(1 − cos):
   - a 4800-step curve with one step per sample pair, giving a 100 ms fade at 48 kHz;
   - a 48-step curve for the stereo↔mono ramp.

   A clean encoder may skip all of this. **UNCERTAIN** whether sinks depend on the start-up fade.
3. **Encode.** Pass all the buffered input to the encoder, with an output window of the table packet size.
4. **Account.** Add the samples consumed to the consumed counter. Keep the unconsumed samples for next time.
5. **Check the period.** A packet is due once consumed ≥ samples per packet + deficit.
   - If no packet is due and the encoder produced no bytes, emit nothing for this block.
   - If a packet is due but the encoder has not produced one yet, make a flush call to force it out.
   - Then set deficit = deficit + samples per packet − consumed, and reset consumed to 0.
6. **Frame.** Byte-swap the packet in 16-bit units and put the 8-byte header in front of it (§7).
7. **Bitrate.** If the deficit is above 96, set the bitrate to table_bitrate − ⌊table_bitrate / 4⌋. Otherwise set it to table_bitrate.
8. **Pending changes.** If a level or mode change is pending:
   - recompute the table row and set its bitrate;
   - if the profile changed, re-create the encoder with the new parameters and reset both counters.
9. **TTP adjustment.** Every 700 packets, the latency-target adjustment in §8.3 runs.

`tests/vectors/r2/` reproduces steps 1, 3, 4, 5 and 7 exactly. It applies no fade, adds no header and does no swap.

---

## 6. Reference vector results

Full details are in `tests/vectors/r2/README.md`.

Configuration: 48 kHz stereo, HQ, table index 0 at level 3, 364 kbps, 14 ms period, 672 samples per channel, 656-byte packets.

- One second of input gives 71 packets of exactly 656 bytes each, at both decoder compatibility levels R1 and R2.
- Each test signal gives a distinct output, and the output is the same every run.
- Changing the input chunk size does not change the output.
- R1 packets begin `00 81 a1 d?`; R2 packets begin `00 83 a1 d?`.
- In steady state the encoder does not emit a packet on its own. The flush call at the period boundary is what forces each packet out.

---

## 7. Output framing: 8-byte header plus swapped payload

For each packet, the encoder stage outputs **8 header bytes** followed by the encoder packet, byte-swapped in 16-bit units (bytes 2k and 2k+1 exchanged).

| byte | contents |
|---|---|
| 0 | TTP adjustment, low byte |
| 1 | TTP adjustment, high byte |
| 2 | period × 4 (0x38 = 56 for 14 ms) |
| 3 | cfg byte from the table (1 for HQ index 0) |
| 4 | split-TX byte: normally 0x00. For split TX it is 0x80 or 0xA0 (left/right), and 0xC0 for a deferred right-channel packet. |
| 5 | 0 |
| 6 | 0 |
| 7 | 0xAE (HQ) or 0xAD (LL) |

For the Px7 configuration the header is `TT TT 38 01 00 00 00 AE`.

### 7.1 TTP adjustment

The value is a u16 in units of 8 µs. It is computed in microseconds and then divided by 8.

**HQ:**

1. Start from 4000 × HQ latency target − (period + 1) × 1000 − 62000 µs. The HQ latency target defaults to 70.
2. Subtract a further term: the duration of the audio buffered so far, in µs. Its exact operands are **UNCERTAIN**.
3. Clamp the result to the range 1000 × HQ min sink buffer + 10000 … 4000 × HQ max sink buffer + 10000 µs. With the defaults (min 20, max 200, §2) that is 30000 … 810000 µs.

Ignoring the uncertain term, the HQ value is 203000 µs, so the header field is about 25375 (0x631F).

**LL:**

1. Start from 1000 × LL latency target − (period + 1) × 1000 − 12000 µs. The LL latency target defaults to 69.
2. Subtract the same buffered-audio term as HQ.
3. Clamp using the LL min/max sink buffer (5 / 200).

**Adjustments for both:**

- On an R2.2 sink only, the value is floored at (30 + 2 × (ELL latency adjustment & 0xF)) ms. The ELL adjustment comes from features byte 1 (§1.2).
- When TWM is active, 1250 is added.

### 7.2 Where RTP comes from

The encoder stage writes no RTP header. The public Qualcomm stack says aptX Adaptive *uses* an RTP header, so a downstream packetiser must add one. That packetiser has not been analysed.

**Hypothesis (UNCERTAIN; needs an Android HCI snoop to verify):** the 8 header bytes are side information that the packetiser folds into the RTP header.

- The reference's own diagnostics describe bytes 4..7 as the most-significant SSRC byte of the packet header. That suggests they become the RTP **SSRC**, with byte 4 as the most significant byte.
  - The SSRC would then be 0x000000AE for HQ and 0x000000AD for LL, which would act as an in-band mode marker.
- By the same logic, bytes 0..3 (TTP, period, cfg) would become the RTP timestamp field.
- The alternative is that all 8 bytes are sent as a payload prefix after an ordinary 12-byte RTP header.

The 16-bit swap is applied before the packet leaves the encoder stage, so the bytes on air are the **swapped** encoder output.

---

## 8. Mode switching, sideband feedback and latency state

None of this is needed for a Linux source talking to an R1 sink (§8.4). It is recorded for completeness.

### 8.1 Sideband feedback from the sink

Each sideband message has an id, a length and a value.

| id | meaning | effect |
|---|---|---|
| 5 | encoder-mode feedback (u16) | See "Mode switch" below. |
| 6 | link RSSI (s8) | Smoothed as rssi = 0.1 × raw + 0.9 × rssi. |
| 7 | number of links | The first value byte is the BT link count. If the length is greater than 1, the second value byte is the BLE link count. |
| 8 | Wi-Fi 2.4 GHz busy | Bit 0. |
| 14 | grouped settings | See "Id 14 sub-ids" below. |

**Mode switch** (id 5):

- If (v & 0x3000) == 0x1000: switch to HQ (profile 3).
- If v & 0x2000: switch to LL (profile 6). This is refused for dual mono, TWS, or 96 kHz.
- A switch starts a mute and fade (lasting 900 and 4800 samples) and marks a profile change as pending (§5 step 8).

**Id 14 sub-ids:**

| sub-id | meaning |
|---|---|
| 1 | page scan / BLE scan |
| 2 | QHS available. Changes the table index; the LL latency targets go from 94/119 to 82/107. |
| 3 | value stored, use unknown |
| 4 | sink battery level, mapped through a 16-entry table (255, 7, 14 … 100) |

### 8.2 Bitrate level feedback

The controller can request a quality level from 1 to 7. The source sets the quality level to it, with these adjustments:

- level 1 is treated as 2;
- in LL, the level is capped at 3, or at 4 when RSSI ≥ −55 / −60 dBm;
- on an R2.1 sink that started in TWM, level ≥ 3 triggers the TWM→stereo transition.

### 8.3 Latency state

The latency state is 0, 1 or 3. It is recomputed whenever any of these change: profile, QHS, Wi-Fi, scans, number of links, RSSI against an "ELL RSSI" threshold, or level.

- State 1 is the "enhanced LL" path, which only R2 sinks use.
- Every 700 packets, the source raises or lowers the LL latency target by 1 ms, within [base, base + 50]. The decision depends on RSSI thresholds (−70, −60 and −55 dBm) and on the link conditions.

### 8.4 Linux source with an R1 sink

None of this sideband exists unless the Bluetooth controller supplies it. Recommended setup: fixed HQ profile, level 3, table index 0, and no mode switching.

---

## 9. Open questions

1. Does the RTP packetiser map the 8-byte header into the RTP timestamp and SSRC fields, or send it as a payload prefix? This needs an over-the-air capture from an Android source.
2. What exactly is the subtracted term in the TTP adjustment? And does a sink tolerate a constant TTP?
3. What are the OTA sample-rate bit values for 44.1 and 96 kHz? What does source-type bit 0x01 mean? What do the Px7's TTP bytes `37 6a 00 af 64 4b` mean, and in what units?
4. What does the create-parameter flag mean? The reference always sets it to 1.
5. Does the Px7 accept the echoed aptx_data block, or does it need the R1 defaults `50 64 64 64 ff ff`?

## 7A. Transport re-check after the first live test (2026-10-12)

This supersedes §7 where they disagree. None of it has been verified over the air.

### 7A.1 What the source does to each codec packet

- The 16-bit byte swap is unconditional on the normal (non-split-TX) path: codec bytes `b0 b1 b2 b3 …` go on air as `b1 b0 b3 b2 …`. Sink-side evidence agrees: the sink reads the frame type from the first payload byte after undoing the swap, and an unswapped packet would put 0x00 there, which is invalid. The live test used the swapped form, so the swap direction is very likely right.
- The 8-byte header is written byte by byte in this order:

| byte | value |
|---|---|
| 0 | TTP adjustment, low byte |
| 1 | TTP adjustment, high byte |
| 2 | period in ms × 4 |
| 3 | table cfg byte |
| 4 | 0x00 (0x80/0xA0/0xC0 only for split-TX left/right/deferred, which we don't do) |
| 5, 6 | 0x00 |
| 7 | 0xAE for HQ, 0xAD for LL |

- TTP adjustment, R1 HQ: `4000·70 − (period_ms + 1)·1000 − 62000` µs, minus the duration of any sample deficit, clamped to `[30000, 810000]` µs, then divided by 8. With no deficit at 14 ms that is 203000 µs → 25375 → bytes `1F 63`. If TWM is active, 1250 is added before the bytes are written (not our case).
- For the Px7 S2e the header is `1F 63 38 01 00 00 00 AE` on every packet.
- No RTP header is built at this layer. A later stage maps the 8 bytes into the RTP header.
- There is no special first packet, sync word, resync marker or metadata packet for R1/R2. A freshly created encoder's first packet is an ordinary packet, and every packet carries an absolute bit allocation, so a decoder can lock on to any packet.
- A 100 ms raised-cosine fade-in is applied to the PCM at start. It doesn't change the packet format.

### 7A.2 RTP mapping (best inference)

- A newer generation of the same vendor's codec builds its own RTP header: byte 0 = 0x80 (V=2), byte 1 = 0x00 (marker clear, **payload type 0**), bytes 2..3 = sequence number big-endian, +1 per packet. The timestamp and SSRC fields carry codec side information (TTP and mode), not a sample clock or a random SSRC.
- For R1/R2 the best inference is that the 8 header bytes are copied **verbatim** into RTP octets 4..11 (option a): `80 PT sq sq 1F 63 38 01 00 00 00 AE`. The alternative (option b) is that each 4-byte half is reversed: `01 38 63 1F AE 00 00 00`. Option a is favoured.
- Payload type: no R1/R2 value is known. 0 is the only value seen in this vendor's aptX Adaptive RTP code; Android's generic vendor-codec path uses 96.

### 7A.3 Things to try for "accepted but silent", most likely first

1. Payload type 0 instead of 96.
2. Header option b (each 4-byte half reversed).
3. The R2 vector (`sine1k.r2.bin`, cfg 3 bitstream) with the same framing. That checks whether the sink actually expects R2 despite the R1 caps.
4. Unswapped payload, to rule the swap out completely.
5. Check that the sequence number increases by exactly 1 per packet and that packets go out every 14 ms, not in bursts.
6. Check the 664-byte payload (8 + 656) plus the 12-byte RTP header fits the negotiated MTU.

## 7B. RTP timestamp and payload type (receiver analysis, 2026-10-12)

From analysing a newer-generation receiver of the same vendor's codec. That receiver can't decode R1/R2 bitstreams, so this is inferred for the Px7 S2e, not proven:

- RTP header: 12 bytes, no CSRCs, then the swapped payload. The first payload byte is the frame type. That rules out putting the 8-byte header after the RTP header.
- The sequence number and timestamp are big-endian on air. The timestamp is a **time-to-play clock in µs**:
  - the first packet after a reset anchors it;
  - a sequence jump of more than 1, or a timestamp change of more than 150000, resets the decoder;
  - a packet more than about one frame late is dropped.
- With a constant timestamp (our current `1F 63 38 01`), every packet after the first looks late and gets dropped. That matches "accepted, no error, silence".
- **Fix to try:** the timestamp advances by the packet duration in µs (14000 per 672-sample packet at 48 kHz) from any start value. Possibly the 203 ms TTP offset (header bytes 0..1 × 8 µs) is added once to the start. UNCERTAIN which.
- SSRC: the last byte is a mode/control byte, so `00 00 00 AE` is consistent. A constant value is fine.
- Payload type: that receiver only accepts **0**. The earlier PT 0 live test kept the constant timestamp, so it didn't rule PT 0 out.

## 7C. Packet layout reconsidered (2026-10-12)

New analysis questions §7A/§7B. The best available evidence for an aptX Adaptive stream over ordinary Bluetooth from a vendor source is a **normal** RTP header followed by the 8-byte header, then the payload:

- `80 60` (V=2, PT 96), sequence +1 per packet, timestamp = sample count (+672 per packet at 48 kHz), SSRC 0
- then the 8-byte header (`1f 63 38 01 00 00 00 ae` for the Px7 case)
- then the 16-bit byte-swapped codec packet. For a 656-byte packet the A2DP payload is 676 bytes in total.

Also:
- header byte 3 is a packet-type index into the packet-size list {348, 656, 140, 152, 560, 760, 960, 348, 980}. Our table cfg bytes are consistent with it (1 = 656).
- the "header-after-RTP" layout was only ever tested in runs where the restart caused a disconnect chime, so it hasn't actually been ruled out.

## 7D. Ground truth: a Snapdragon phone streaming to an R1 sink (2026-10-13)

Captured from a OnePlus 15 (Snapdragon) streaming aptX Adaptive to this machine, which advertised the Px7 S2e's exact caps. 2609 packets over about 33 s. This supersedes §7A to §7C.

- **SetConfiguration** from the phone: `d7 00 00 00 ad 00 10 02 37 6a 00 af 64 4b 00 00 aa` + zeros. That's exactly what our `select_config` sends, so negotiation is confirmed.
- **Every media packet is 668 bytes:** a 12-byte RTP header followed directly by the 656-byte codec packet. There is **no separate 8-byte header** in the payload.
- **RTP header:** `80 60` (V=2, PT 96, marker clear), sequence +1 per packet, then a 32-bit timestamp, then SSRC `00 00 00 ae`.
  - The SSRC carries the HQ/LL mode byte (`ae` = HQ) in its last byte, with the other three bytes zero.
  - The **timestamp is a time-to-play clock in µs**, big-endian. It advances by the packet's audio duration: 14000 µs for 14 ms packets, and 12000 µs in stretches where the phone sends 12 ms packets. The steps also jitter by ±1 µs. The start value is arbitrary (`0xbddc3048` in the capture).
- **Payload:** the 16-bit byte-swapped codec packet, exactly as specified. The first bytes on air are `81 00 d0 a1 …`, which unswap to the codec's `00 81 a1 d0`.
- **So the 8-byte header's fields map into RTP like this:** the TTP/period/cfg bytes aren't sent at all; the TTP clock replaces them in the timestamp field. Byte 7 (`ae`/`ad`) becomes the low byte of the SSRC, and bytes 4..6 (zero) its high bytes.
- The phone switched between 14 ms and 12 ms packets during the stream, so its encoder changed table rows on the fly. Both are valid for an R1 sink.

## 7E. Source type: the sink's timing mode (2026-10-13)

Codec element byte 6 carries the sample rate in bits 3..7 and a **source type** in bits 0..2.

- **Source type 0** puts a Qualcomm sink in a timing mode where the RTP timestamp is the absolute time-to-play on the Bluetooth link clock. Only a source whose controller writes link-clock timestamps can meet that. A Snapdragon phone with offload uses this mode, which is why its configuration has byte 6 = `0x10`.
- **Source type 2 (bit 0x02 set)** tells the sink to time playback itself and use the RTP timestamp only relatively. A host-side (software) encoder must use this.
- For the Px7 S2e at 48 kHz that makes byte 6 = **`0x12`**: `d7 00 00 00 ad 00 12 02 37 6a 00 af 64 4b 00 00 aa` + zeros.
- Bit 0x01 is a sink-side flag. A source must not set it.
- With source type 2, the SSRC's low byte may select the latency mode: `a1`/`a2` LL, `a3` HQ, `a4` TWS (UNCERTAIN; the HQ marker used so far is `ae`).
- The RTP timestamp unit in this mode isn't known. Try µs (+14000 per 14 ms packet) first, then samples (+672).

This supersedes the earlier recommendation of byte 6 = `0x10`, which is correct only for sources whose controller supplies link-clock timestamps.

## 7F. RTP fields with source type 2 (2026-10-13)

With source type 2 (§7E), the sink reads the RTP header like this:

- **Timestamp:** not a clock. Bits 0..15 = **samples per channel in this packet** (672 for a 14 ms packet at 48 kHz). Bits 16..31 = a signed latency adjustment in ms (0 normally). So every 14 ms packet carries the same timestamp, `0x000002A0`. The sink plays the first packet at arrival time + target latency, and each later packet at the previous packet's play time + its sample count / sample rate. A running counter, in samples or in µs, makes every packet claim far too much audio. The sink's buffer then fills and it takes data at well below real time, which comes out crackly, slow to start and slow to stop.
- **SSRC:** the full 32-bit value selects the latency mode. `0x000000A1` LL0, `0x000000A2` LL1, **`0x000000A3` HQ**, `0x000000A4` TWS. Keep it constant; changing it mid-stream resets the sink's timing.
- **Target latency:** from config bytes 8..11 (LL0 ms, LL1 ms, HQ in 2 ms units, TWS in 2 ms units). Out-of-range values become the minimum, so the Px7's HQ byte `0x00` gives 200 ms. That's fine.
- **Sequence number:** +1 per packet.
- **Payload:** one codec packet per RTP packet, directly after the 12-byte header.

UNCERTAIN: an older generation of the sink reads the timestamp as plain µs. Test results so far fit the newer reading above.

## 7G. Target latency with source type 2: don't send 0 (2026-10-13)

Correction to §7F: "Out-of-range values become the minimum, so the Px7's HQ byte `0x00` gives 200 ms" is **wrong**. With source type 2 the sink uses config byte 10 (TTP HQ, in 2 ms units) as its **target latency**, and `0x00` gives a **0 ms** target.

- Evidence: in every source-type-2 capture the Px7's Delay Report is 200 ms before Start and **0.0 ms** about 0.7 s after Start. The Delay Report carries the sink's active target.
- With a 0 ms target, the sink's only headroom is whatever the stream start accidentally gives it. Its rate trim slowly removes that headroom, and once it falls below the link's arrival jitter (30–45 ms), late-packet resets start: an audible stutter after roughly 80–110 s, worse under link or host jitter.
- **Fix:** with source type 2, set TTP HQ (body byte 10) to `0x64` (100 × 2 ms = **200 ms**). For the Px7 the config becomes `d7 00 00 00 ad 00 12 02 37 6a 64 af 64 4b 00 00 aa` + zeros. Check: the Delay Report after Start should read 200.0 ms.
- More generally, for source type 2 don't echo a zero latency byte from the sink's caps: replace a 0 with a sensible default (HQ 200 ms, `0x64`). Bytes 8/9 are LL in ms (the Px7's `37` = 55 ms is non-zero, so fine). Bytes 12/13 are TWS in 2 ms units.
- Drift: with a proper target the sink corrects source/sink clock drift itself. A rate trim driven by early/late arrival against TTP, up to ±0.5 %, settles in about 50 s. A source shouldn't try to correct drift: keep the timestamp's high word (TTP adjust) at 0. Changing it doesn't reset playback, but it changes the target and makes the sink speed up or slow down.
- Avoid a hole or burst at stream start if possible. It doesn't break anything with a proper target, but it costs headroom.

## 7H. Verified rules per profile and rate (2026-10-13)

Checked against primary sources. This supersedes earlier statements where they differ.

**Sample-rate bits (byte 6, bits 3..7).** 0x08 = 44.1 kHz, 0x10 = 48 kHz, 0x20 = 96 kHz, 0x40 = 192 kHz. (0x80 is a variant used only by newer sinks.) The "0x40 = 44.1 kHz" and "0xA0 = 96 kHz" readings in §1.2 are wrong for this sink family. With source type 2, byte 6 is therefore **44.1 kHz → `0x0A`, 48 kHz → `0x12`, 96 kHz → `0x22`**.

**§7E corrections.** The SSRC is matched as a whole 32-bit value (not its low byte), and the timestamp rules are as in §7F, not "used relatively". `0xAE`/`0xAD` aren't valid SSRCs with source type 2.

**Timestamp low 16 bits** = samples per channel in that packet, counted at the negotiated rate. **Never 0**: a zero makes the sink read the field as a time value instead. Examples:

| profile / row | period | low 16 bits |
|---|---|---|
| HQ 48 kHz | 14 ms (672) | `0x02A0` |
| LL 48 kHz | 10 ms (480) | `0x01E0` |
| LL 48 kHz | 8 ms (384) | `0x0180` |
| HQ 96 kHz | 15 ms (1440) | `0x05A0` |
| HQ 96 kHz | 40 ms (3840) | `0x0F00` |

**SSRC.** `0x000000A3` HQ, `0x000000A1` LL0, `0x000000A2` LL1, `0x000000A4` TWS. LL0 is the sink's "2.4 GHz Wi-Fi off" latency and LL1 is "2.4 GHz Wi-Fi on". The SSRC **must change** exactly when the encoder switches between HQ and LL. Each change costs one audible re-anchor in the sink; that's expected. Keeping the old SSRC leaves the sink at the old target latency.

**TTP adjust (high 16 bits).** A signed value in ms, absolute: target = SSRC latency + adjust. Changing it doesn't reset playback: the sink slides to the new target using its ±0.5 % rate trim. **A source should send 0.** The sink corrects drift itself, so a source-side drift loop duplicates that work.

**Target latency** comes from the bytes the source writes in SetConfiguration (body 8..11), not from the sink's caps. Headphone ranges are LL0 55–200 ms, LL1 75–230 ms, HQ 200–500 ms, TWS 200–500 ms. Out-of-range values become the minimum. The Px7 config we send (`37 6a 64 af`) decodes as LL0 55, LL1 106, HQ 200, TWS 350 ms. A host source should consider **LL0 ≥ 80 ms** (`0x50`) to absorb host and link jitter. Body bytes 12..13 aren't read.

**Unverified.** Packet sizing and sample counts at 44.1 kHz; the rule for choosing TWS; the exact LL0/LL1 switching policy; whether payloads larger than 668 bytes are accepted with source type 2.

## 7I. Encoder delay, start and end of stream (verified 2026-10-13)

**Encoder delay** (filterbank, encode plus a matched decode), measured with impulses. It doesn't depend on position, bitrate or packet size:

| config | rate | delay |
|---|---|---|
| 0 (HQ, R1) | 48 kHz | 90 samples (1.875 ms) = 2 × (3 × 16 − 3) |
| 1 (LL, R1) | 48 kHz | 66 samples (1.375 ms) = 2 × (3 × 12 − 3) |
| 3 (HQ, R2) | 48 kHz | 255 samples (5.3 ms) = 256 − 1 |
| 3 (HQ, R2) | 96 kHz | 255 samples (2.66 ms) |

Packet buffering adds up to one period (672 frames for HQ 48 kHz, 480 for LL, 1152 at 96 kHz). The reference reports 0 to its audio framework. A source may report the filterbank delay, plus one period if it counts the packet wait as encoder delay. (Correction to codec-core §3.2: "latency 0" is what gets reported, not the real delay.)

**Start of stream (reference behaviour, optional for a source).** Whenever the encoder is created or reconfigured, including on an HQ↔LL switch:
1. **Hard mute** for 20 ms: 20 input calls of 1 ms each are encoded as zeros.
2. **Raised-cosine fade-in** over 4800 frames: gain = 0.5·(1 − cos(π·(j+1)/4801)) for frame j = 0…4799. Same gain on both channels, each product truncated toward zero. That's 100 ms at 48 kHz and 50 ms at 96 kHz.

The bitstream carries no sign of the fade and the first packet is an ordinary packet, so a source can skip both. The "CVR mute ramp" mentioned earlier isn't part of start-up: it's the stereo↔mono ramp.

**HQ↔LL switch fade.** The profile-switch fade uses the same 4800-frame table read backwards (a fade-out), then the start-of-stream sequence above on the new encoder. Batch 2 may refine this.

**End of stream.** The reference discards the final partial period and sends nothing special: no flush, no padding, no end marker. A source sends nothing at stop; AVDTP Suspend/Close ends the stream. Optionally, pad with zeros to the next period boundary plus the filterbank delay, so the last audio plays out. The library's flush call codes an even number of buffered blocks (≥ 2) into one full-size padded packet; an odd last block, or less than one block, is never coded; with nothing pending it emits nothing.

## 7J. Verified caps, tables and MTU rules (2026-10-13)

Checked against primary sources. This supersedes earlier statements where they differ.

**LL with an R1 sink uses the HQ bitstream.** The reference creates the encoder in LL mode (profile 6, the low-power path) only for **R2** sinks. With an R1 sink (like the Px7) it creates an **HQ encoder (bitstream config 0)** and runs it on the **LL table row**: 265 kbps, 10 ms, 480 samples per channel, 348-byte packets, LL marking (SSRC per §7H). Bitstream config 1 is never produced for an R1 sink. (codec-core §1: config 1 is used only when the reference runs the low-power path, which an R1 sink never gets.)

**What a source advertises (fill_caps), and byte 6.** The low three bits of byte 6 are independent flags:
- 0x01: set by the **sink** only. A source must never set it.
- 0x02: the sink times playback itself. A host encoder must set this.
- 0x04: custom AVRCP commands supported.

A source always sends its own source-type bits (0x02), never the sink's. Source type doesn't depend on the rate or on R1/R2. Advertising 0 would make any sink-initiated stream silent. The §1.5 recommendation of 0x10 is stale: use 0x12 at 48 kHz.

**R2 sinks.** The source sends **its own** 18-byte block, not the sink's: TTP `50 64 64 64 ff ff`, extension version 1, setup preferences `02 03 03 03`. The features low byte is the AND of both sides' features. For **R1** sinks, the sink's body 8..25 is echoed back with zero-latency bytes replaced (§7G), and body 26..39 is zeroed.

**Sample rate.** For R1 sinks the encoder always runs at **48 kHz**, whatever was negotiated. Rate bits as in §7H. Never send 0x40 for 44.1 kHz (sinks read it as 192 kHz).

**Samples per packet** = period × rate. The table's samples column is only right at 48 kHz (96 kHz for the 96 kHz rows). For example, a 14 ms packet at 44.1 kHz is 617 samples per channel.

**MTU rule** (corrects §4).
- The encoder's MTU is already net of the 12-byte RTP header, so the packet budget = **L2CAP MTU − 20** (875 for the Px7's 895).
- The cap only applies to **HQ** rows. When a row's packet is larger than the budget, the new bitrate = ⌊budget·8000 / period_ms⌋ − 25000, computed from the budget **before** rounding to a multiple of 4. The **period stays the same**.

**Bitrate clamps** for set-bitrate in configs 0, 1 and 4: [100000·n, 300000·n] (n = channels), with a bit-pool cap of 210000·n for config 4. The 25 % deficit drop clamps at the floor: in stereo, 265000 → 200000, not 198750.

**Tables.** All HQ and LL rows (indices 0–8, levels 1–7), packet sizes and cfg bytes are confirmed. The default level is 3. Level caps apply in HQ only. The cfg byte is a packet-size class, the same for R1 and R2. R2 detection: extension byte == 1. In LL the latency state starts at 0, and latency state 3 selects the same table as state 0.

**Other.**
- Channel config 3 (split TX) only happens at 96 kHz with an R2 sink that supports it.
- The R2.1 RSSI and latency-adjust fields are only read for R2.1 sinks.
- TTP bytes: LL0 ms, LL1 ms, HQ in 2 ms units, TWS in 2 ms units.
- The reference's sink-buffer minimum is 20 (the default).
