# questions for the spec side

## plugin integration

1. **Answered:** see `a2dp-integration.md` §1.1–§1.3 (layout, bit values, R1 vs R2 detection) and §1.5 (what to send to the Px7 S2e). Implemented in `spa-codec-aptx-adaptive/src/caps.rs`: `select_config` gives the §1.5 bytes for the Px7. What our own `fill_caps` should advertise is still open (item 9 below). Originally: A2DP capability layout after the 6-byte vendor header, field meanings, which bits the source should set in its own caps, and how to pick a configuration from the sink's caps.
2. **Answered:** yes, 40 bytes; see `a2dp-integration.md` §1.5 (recommended SetConfiguration). Is the configuration the source sends back the same length as the sink's caps? PipeWire only offers a codec when `select_config` returns exactly `caps_size` bytes.
3. **Partly answered:** each packet carries an 8-byte header and a 16-bit-swapped payload, and an RTP header is used; see `a2dp-integration.md` §7. Whether the 8 bytes go into the RTP timestamp/SSRC or follow a normal RTP header is still open (see "from a2dp-integration" Q1). Do media packets carry an RTP header (12 bytes, like aptX HD), any extra payload header, or neither? The plugin implements both readings behind `session::FRAMING` in `spa-codec-aptx-adaptive` (overridable at runtime with `APTX_ADAPTIVE_FRAMING=header-in-rtp|header-after-rtp`, see `scripts/dev-replay.sh`), defaulting to the spec's hypothesis (`HeaderInRtp`: header bytes 0..3 as RTP timestamp, 4..7 as SSRC).
4. **Answered:** interleaved 24-bit right-justified, sign-extended in 32-bit containers (S24_32LE) at 48 kHz stereo; 672 samples per channel per packet for HQ level 3. See `a2dp-integration.md` §3.4 and §4.1. PCM input: sample format and bit depth the encoder expects (currently S24_32LE interleaved stereo at 48 kHz), and samples per channel per encoder frame.
5. **Partly answered:** the encoder emits one 656-byte packet per 14 ms period at 364 kbps, and the packet size is capped by MTU − 8; see `a2dp-integration.md` §4 and §5. How codec packets map onto A2DP/RTP packets depends on "from a2dp-integration" Q1. How many encoder frames go into one A2DP packet, and how that relates to the MTU and the target bitrate.
6. Encoder delay in samples, for `get_delay`.
7. **Partly answered:** `abr_process` is a no-op for now; the deficit rule runs per packet in `aptx_adaptive::Stream`. The reference adapts the bitrate from the sample deficit (§5 step 7) and from controller level feedback (§8.2), not from socket backlog. Which backlog signal a Linux source should map onto the quality level is still open. Bitrate adaptation inputs: what `abr_process` should react to (socket backlog in bytes) and which encoder parameter it changes.

## from a2dp-integration

1. Does the RTP packetiser map the 8-byte header into the RTP timestamp and SSRC fields, or send it as a payload prefix after a normal 12-byte RTP header? Needs an over-the-air capture from an Android source. (`a2dp-integration.md` §7.2)
2. What exactly is the subtracted "buffered audio" term in the TTP adjustment? Does a sink tolerate a constant TTP? (§7.1)
3. What are the OTA sample-rate bit values for 44.1 and 96 kHz? What does source-type bit 0x01 mean? What do the Px7's TTP bytes `37 6a 00 af 64 4b` mean, and in what units? (§1.2, §1.5)
4. What does the create-parameter flag mean? The reference always sets it to 1. (§3.1)
5. Does the Px7 accept the echoed aptx_data block, or does it need the R1 defaults `50 64 64 64 ff ff`? (§1.5)
6. Does any sink depend on the start-up fade-in? (§5 step 2)
7. What is the exact bitrate formula when the packet size is capped by the MTU? (§4)
8. §1.5 says to echo "body 8..16" (TTP bytes plus the R1 end marker), §1.4 says an R2-capable source echoes body 8..25. We echo 8..16 for R1 caps and 8..25 for R2 caps, zero after. For an R1 sink, should the reserved bytes 17..39 also be echoed if non-zero, or zeroed?
9. Our own `fill_caps` (as an A2DP source endpoint): the spec only covers what to send as SetConfiguration. We advertise 48 kHz, stereo | joint stereo, source type 0 and the public R1 block `50 64 64 64 ff ff 00 00 aa`. Is that right, and should a source advertise source type 2 or the R2 extension block?
10. §3.4 asks for an output window of the packet size "plus at least 64 bytes of slack after it", but also caps the window at 2016 bytes, while §2 lets the packet budget reach 2008 (2008 + 64 > 2016). Is the 2016 limit on the packet size or on packet size + slack? And does the encoder ever write into the slack (i.e. can an encoder packet be longer than the table size)?
11. §5 step 5: if a packet is due and the flush call still produces nothing, what then? We emit nothing and keep the counters, so the next block triggers another flush.
12. §4: when the MTU caps the packet size, does the period (and so samples per packet) stay the same? We keep the period and only lower size and bitrate.
13. §7.2 HeaderInRtp hypothesis: the RTP timestamp field then carries TTP/period/cfg instead of a sample clock, so a sink can't use it for timing. Does the sink rely on RTP timestamps at all? Also which RTP payload type: we use 96 like aptX HD, with marker bit 0. And with the header in RTP, does the spec's MTU − 8 budget become MTU − 12?
14. The R1 cfg byte in the §4.1 table is the same for idx 0/1/3 ("656 / 1"). Is cfg really independent of the decoder level, given R1 and R2 bitstreams differ (config id 0 vs 3)?

## live test 2026-10-12 (Px7 S2e, replaying sine1k.r1 vector packets, other source devices disconnected)

- control: the stock aptX HD codec played a 1 kHz test tone fine
- header-in-rtp (8 header bytes as RTP timestamp + SSRC, payload type 96): headset stays connected, no disconnect sound, **silence**
- header-after-rtp (12-byte RTP header, then the 8 header bytes): headset plays its **disconnect sound** and stays silent, though the BT link stays up
- conclusion: the transport layout is probably header-in-rtp, but the sink isn't decoding the payload. Open: whether the 16-bit swap direction is right, the TTP/header values, a start-of-stream sync or resync requirement, and payload type.

## from codec-core

1. Config 4 (R2 low latency): the 128-tap prototype in `tables/filterbank_8band_prototype_128.json` and its predictor mode are unverified. Not needed for the Px7 S2e, but needed for R2 LL sinks. (§1, §3.2)
2. Config 0 with the low-power create option set would use 12-tap coefficients with a 16-long delay line and different read positions. Can this happen via the normal wrapper, and if so what are the exact read positions? (§3.1)
3. 4-band allocation header bit 4 ("create-parameter channel config equals 2"): never observed. What channel config sets it, and does it change anything else in the stream? (§7.2)
4. **Answered:** see `codec-core.md` §8.1 (frame-size rule, padding and carry histories, `P_est`) and §8.1a (rate check). Constant-bitrate mode with create flag 0 is still open as "from codec-core allocator" item 3. Originally: last-frame size policy (§8.1): how is the bits-per-block estimate computed, what exactly is the "padding history capped at 30", and what is the second constant-bitrate estimate? Is constant-bitrate mode really tied to the create flag being 0?
5. **Answered:** see `codec-core.md` §8.2 to §8.6 (bit pool, max-bits table thresholds, scale factors, distribution and digital black), now verified byte for byte; tables in `spec/tables/allocator.json`. Originally: bit allocator (§8.2): unverified. Exact bit-pool formula from bitrate, bitrate thresholds for picking the `max_bits_*` table, scale-factor definition, and the digital-silence path (which gives 4/4/4/3 on config 0). Needed only to match the reference byte for byte without supplied allocations.
6. **Partly answered:** the per-packet flush call codes the partial frame at whatever even block count it holds and closes the packet (`codec-core.md` §8.0, §8.1). A final drain at end of stream is still not covered. Originally: end-of-stream flush: how is the final partial frame/packet closed? (§9)
7. Mono, joint stereo and metadata chunks are not covered; is any of these ever negotiated with an R1 sink?

## live test round 2 (2026-10-12, §7A.3 variants, sine1k vectors)

Each variant was installed, wireplumber/pipewire restarted, the headset reconnected, then 12 s streamed.

- PT 0, header-in-rtp: chime
- PT 96, header option b (halves reversed): chime
- PT 96, header-in-rtp, R2 bitstream: chime
- PT 96, header-in-rtp, no payload swap: chime
- control, PT 96, header-in-rtp, R1 bitstream (the round 1 layout): chime this time; round 1 was silent with no chime
- second stream on the same connection, no restart: silence, no chime

Conclusion: the chime doesn't separate layouts. It seems tied to the restart and reconnect, not to the packet bytes, so none of the variants can be ruled in or out yet. We need ground truth: a capture of what a phone sends, a btmon capture of what we send, or a reference decoder to check our bytes against.

## btmon capture of our own stream (2026-10-12)

- AVDTP: SetConfiguration on SEID 9 with `10 02 37 6a 00 af 64 4b 00 00 aa` + zeros was accepted, and so were Open and Start. No delay report, no vendor command, no reject.
- Media channel L2CAP MTU 895. Packets go out every 14 ms as 12 + 656 = 668 bytes, and the controller completes them.
- On-air RTP header: `80 60 SS SS 1f 63 38 01 00 00 00 ae`, i.e. V=2, PT 96, the sequence number incrementing by 1, then the 8 header bytes verbatim. The payload is the reference packet byte-swapped in 16-bit units, exactly as intended.
- The headset still plays silence. Signalling, transport and our own packetisation all behave as designed, so the problem is in the content the sink expects: the header values, the RTP mapping, or the bitstream/config. This needs the dirty side (receiver analysis) or a phone capture to settle.

## todo once audio works

- Make aptX Adaptive the default for aptX Adaptive sinks locally. A WirePlumber rule that sets the device's default profile to `a2dp-sink-aptx_adaptive`, installed by `scripts/install-plugin.sh`, is enough. Upstreaming is parked; the patch is on branch `upstream-adr` (PR #5, closed).

## live test round 3: ttp-clock (2026-10-12)

- btmon confirms the on-air RTP: `80 60 seq seq <ts BE> 00 00 00 ae`, with the timestamp stepping by 14000 (0x36b0) per packet from 0. AVDTP Start was accepted, the headset stayed connected through 12 s of streaming, and played **silence**.
- ttp-clock with PT 0: inconclusive. The headset was already dropping the link repeatedly.
- The repeated drops came from WirePlumber saving `a2dp-sink-aptx_adaptive` as the device's default profile, so every reconnect went straight into a non-working stream. Switching back to `a2dp-sink` (aptX HD) fixed it. **Live tests must restore `a2dp-sink` when they finish.** The install script should never leave aptX Adaptive as the saved default until it works.
- An advancing µs timestamp alone doesn't fix the silence.

## live test round 4: §7C layout, fair test (2026-10-12)

- Normal RTP header (PT 96, sample-count timestamp, SSRC 0), then `1f 63 38 01 00 00 00 ae`, then the swapped sine1k.r1 packets.
- Setup, then 30 s settle, then 12 s playback. The headset stayed connected, then was restored to aptX HD.
- Result: **silence** (listener fairly but not fully sure there was no disconnect).
- So every layout tested gives silence when tested fairly. Still not done fairly: ttp-clock + PT 0.

## from codec-core implementation

1. **Answered:** yes. See `codec-core.md` §7.1 (corrected) and §8.0: the {8, 16, 32, 48, 64} set only applies when the frame-size rule closes the packet; a flush closes the last frame at whatever even count is buffered. Analysis only ever takes an even number of blocks, so an odd block is never left at flush; it stays unconsumed. Originally: §7.1 says the last frame of a packet has 8, 16, 32, 48 or 64 blocks. In `tests/vectors/r2/*.r1.bin` (the flush-driven loop the Px7 path uses) every packet is 64 + 64 + **40** blocks: 672 samples / 4 = 168 blocks, all coded into the packet, padded, no continuation. The stage vectors (encoder called once per packet with all remaining input) do follow the {8..64} rule. So it looks like a flush closes the last frame at whatever even count is buffered. Is that right, and is the {8, 16, 32, 48, 64} set only the non-flush policy (§8.1)? What happens to an odd block left at flush?
2. `a2dp-integration.md` §3.2/§6 and `tests/vectors/r2/README.md` say R1 packets begin `00 81`. That's only true when the packet is padded (0x80 flag, §7.1). Unpadded packets start `00 01`, as in `stages/cfg0_*`. The r2 vectors happen to be padded throughout.
3. §5.2 step 2 (extension), "restore (L, H) to the interval before the last split": we read this as the interval at the start of the last iteration. That matches the 10–16-bit bands in `cfg3_48k_sine`, but they're the only coverage. Confirm.
4. §5.3 low-power quirk: the top band keeps its previous r. Does it still update `step`/`ld` (and so the low-power carry) from the new code? We do, and cfg1 matches, but cfg1 never gives band 3 a non-zero allocation in two consecutive blocks with B ≥ 3, so this is thinly covered.
5. **Answered:** yes. The largest 4-band ceiling is 9 (`codec-core.md` §8.3), so ρ ≤ 8 always. Originally: §7.2 4-band absolute coding has no escapes, so reduced allocations above 8 can't be coded with tables A/B (no codes past nibble 8). Is the config 0/1 cap really 9 bits (`max_bits_4band_max`), so ρ ≤ 8 always?
6. Encoder delay for `get_delay` (old Q6): the filterbank adds group delay even though §3.2 says "latency 0". Is 0 what the reference reports to the stack, regardless of the actual filter delay?
7. **Partly answered:** the reference limits carry-over indirectly through the frame-size rule (`codec-core.md` §8.1: carry and smoothed carry are held back from the budget). Whether it has any hard cap or ever drops audio is still open as "from codec-core allocator" item 6. Originally: encoder-side choice, not in the spec: a packet may push at most a quarter of its payload (`(Lp − 3)/4` bytes) into the next packet's continuation. Past that a frame falls back to the cheapest allocation, then to fewer blocks, and anything left is dropped (counted, logged by the plugin). Without some cap the continuation can grow without bound when packets are small. What does the reference do: is there a carry limit, and does it ever drop audio?
8. Contradiction: §7.1 byte 2 is `Lp/4 − 3` in 8 bits, so the largest packet it can describe is 1032 bytes, but `a2dp-integration.md` §2 clamps the packet budget to 128..2008 and §3.4 allows windows up to 2016. We cap packets at 1032. Does the reference clamp too, or does byte 2 mean something else for bigger packets?

## from codec-core allocator

1. `codec-core.md` §8.7: the stage-vector cross-check reports 126 packets reproduced from raw PCM, which is five runs (four 24-packet runs plus the 30-packet config 1 run), but `tests/vectors/stages/` has six runs. Which stage run was not reproduced from raw PCM, and does it match?
2. §8 verification covers 48 and 96 kHz only. Is the frame-size rule and allocator also confirmed at 44.1 and 88.2 kHz (`rate_idx` 2, and the 88.2 kHz use of the 96 kHz max-bits tables)?
3. §8.1: what exactly is the constant-bitrate clamp with create flag 0, and is that flag otherwise meaningful? (Also `a2dp-integration.md` §3.1 / "from a2dp-integration" Q4.)
4. §8.1a and §8.2: is the create-time `P_est` computed with the §8.2 clamp applied? The set-bitrate clamp (configs 0/1 up to 300000·nch, config 3 up to 500000·nch) is narrower than the pool clamp (350000·nch, 550000·nch); is that intended, and does a create bitrate above the set-bitrate limit stay unclamped until the first set-bitrate call?
5. §8.2 alternate flag (config 3, 2 channels, R ≤ 179999) and §8.6 step 5.4 (extra bits when the digital-black total is 1 to 6) are untested. Can either happen with any wrapper table row, including after the three-quarter bitrate drop of the wrapper loop?
6. Carry-over bound (follows "from codec-core implementation" item 7): beyond the frame-size rule holding back carry, does the reference cap the continuation length or drop audio when a frame overflows by more than the next packet can absorb? Our implementation currently caps carry at a quarter of the payload; with §8 implemented, should that cap be removed?
7. §8.4 offset tables: the channel-config-2 ("stereo") tables and the mono path (`r = 2R` in §8.3) are not covered by any verified run. What channel config produces them, and is mono's 2R correct?
8. §8.2 config 4 clamp range [100000·nch, 210000·nch] is unverified, as is the rest of §8 for config 4.
