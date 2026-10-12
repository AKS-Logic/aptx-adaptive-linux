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

## answers to "from the spec allocator implementation" (2026-10-13)

1. §8.1a step 4: answered. The correct rule is `m = A_new` if `A_new > nominal`, else `B_new`. The `max()` in the spec was a typo; spec fixed. If "always B_new" (the `min` that was implemented) matches the vectors, check that A accumulates `W + pad` and B only `W`, each with its own history. A reference check value: config 0 48k noise, packet 3, A = 349063 and B = 347103. The implementation must use the spec rule.
2. §8.1, n = 48..63: answered. 64 blocks, keep the packet open; the table had a typo. Spec fixed.
4. Carry bound: answered, see §8.8. The reference has no limit and drops nothing. Don't drop frames; pick the bitrate from the packet size instead.
5. Packet too small: answered, see §8.8. The encoder is never told the packet size at create; per call it rejects windows ≤ 11 bytes or > 2016 bytes.

## live test round 5 (2026-10-13, best-evidenced layout, with btmon)

- Layout on air (confirmed in the capture): `80 60 <seq> <ts +672/packet> 00 00 00 00 | 1f 63 38 01 00 00 00 ae | 656-byte swapped packet`, 676 bytes every 14 ms. Real encoder output (silence input) from the merged encoder, not the replay vector.
- AVDTP SetConfiguration, Open and Start were accepted. 25 s of playback after a 30 s settle.
- The headset played its **chime** during playback. The capture shows **no** link drop, no AVDTP Suspend/Abort/Close from the headset, no L2CAP disconnect, and no ACL data from the headset at all during playback: it stayed completely silent on the link. So the chime is the headset's own audio prompt, not a disconnect. It may be the "connected"/codec prompt, or a decoder error prompt.
- Headset remote features (from the reconnect): page 0 `ff fe 8f fe db ff 5b 87`, page 1 `03`, max page 2. BlueZ never sends Read Remote Version Information, so the remote LMP version and manufacturer still need `hcitool info` (not installed) or a raw HCI command.

## live test round 6: replaying the phone's own packets (2026-10-13)

- Replayed the 2609 codec packets captured from the OnePlus 15, in exactly the phone's on-air layout (§7D, 668-byte packets, `80 60`, µs TTP timestamp, SSRC `00 00 00 ae`), every 14 ms, to the Px7 S2e: **silence**. btmon confirms 548+ packets went out after AVDTP Start was accepted.
- So the on-air bytes are not the problem: a stream the phone itself produced is silent when it comes from this host.
- The headset closed the stream itself during one run (AVDTP Close from the sink, then "Remote User Terminated Connection"). The "disconnect chime" happens around reconnects, before any audio.

### new lead: SCMS-T content protection

- The Px7 S2e's aptX Adaptive endpoint (SEID 9) advertises **Content Protection: SCMS-T** in Get All Capabilities.
- Our SetConfiguration only includes Media Transport + Media Codec. It never configures content protection, so no SCMS-T header byte goes in the media payload.
- An earlier capture also suggested the offload start config carries an SCMS-T flag. If the sink needs SCMS-T configured, each media packet then carries a 1-byte content-protection header (between the RTP header and the codec payload) and the sink may mute without it.
- To test: add Content Protection SCMS-T (type 0x0002) to SetConfiguration, and prepend the SCMS-T byte (0x00 = copy allowed) to every payload.
- Correction (public sources): the CP header byte is bit 0 = L-bit, bit 1 = Cp-bit (Bluetooth Assigned Numbers, A/V content protection, note 3). Android's `AVDT_CP_SCMS_COPY_FREE` is **0x02**; 0x00 is `COPY_NEVER`. The dev switch `APTX_ADAPTIVE_SCMST=1` sends 0x02 by default; `APTX_ADAPTIVE_SCMST_BYTE=0x00` tries the other.

## phone to real Px7 S2e: bugreport findings (2026-10-13)

The HCI snoop file in the bugreport was empty (16 bytes): the vendor snoop logger failed an SELinux check. The phone's logcat and Bluetooth dumpsys still show:

- **No content protection.** The phone configures the Px7's aptX Adaptive SEP with protocol service capabilities mask `0x102`, which is Media Transport + Delay Reporting. **Not SCMS-T.** So the SCMS-T lead is very likely wrong; the phone plays fine without it.
- **Delay reporting is configured,** and the sink reports 200 ms (2000 in 1/10 ms). BlueZ already negotiates Delay Reporting when both sides offer it; check our SetConfiguration includes it.
- Stream MTU 883. The offload start adds "AVDTP media header" into the stream MTU (895 = 883 + 12): a plain 12-byte RTP header, same as §7D.
- Offload parameters for this sink: sample rate 48k, 24-bit, sink buffering min/max 20/100 ms for LL, HQ and TWS, TTP bytes `37 6a 00 af 64 4b` (the sink's own), encoder "Mode" 4266 (0x10AA), "mode 4096" (0x1000) sent to the controller as aptX Adaptive mode, input mode 0, R2.1 supported true.
- Interop: the phone checks `OplusAvrcpUseA2dpOnly` against the Px7 (no match).
- "Don't use RTP header: false", so RTP is used.

## phone to real Px7 S2e: real HCI log via the OnePlus log tool (2026-10-13)

Exported with the OnePlus log tool (`*#800#` → Bluetooth → transmission). The A2DP media itself is offloaded and isn't in the log, but the whole setup is:

- **Get All Capabilities for SEID 9 (aptX Adaptive), as the Px7 answers the phone:** Media Transport, Media Codec `19 0a 37 6a 00 af 64 4b 00 00 aa…`, Content Protection SCMS-T, **and Delay Reporting**. When BlueZ on our host asks, the Px7 answers with **no Delay Reporting**. So the headset advertises differently to us.
- **The phone's SetConfiguration to SEID 9:** Media Transport + Media Codec `10 02 37 6a 00 af 64 4b 00 00 aa` + zeros (identical to ours) + **Delay Reporting**. No content protection.
- After Open, the Px7 sends a Delay Report of 200 ms.
- Media L2CAP channel: the phone asks for MTU 1024 and the Px7 for MTU 895, with flush timeout 65535 both ways. The phone then sends HCI Flow Specification (outgoing, guaranteed) and Sniff Subrating.
- At Start the phone sends its controller a vendor command (OCF 0x000a, 66 bytes) with the codec config. That's offload only, and goes to its own chip, not the headset.

Leading hypothesis: the Px7's aptX Adaptive SEP only reports Delay Reporting to some sources. Possibly it depends on the AVDTP version we advertise in SDP, or on something else in the source's identity or the request. Either way, the phone configures Delay Reporting and we don't. Next: find why the Px7 omits Delay Reporting for us (compare our SDP A2DP source record and AVDTP version with the phone's), and test forcing Delay Reporting into our SetConfiguration with the patched bluetoothd.

## A2DP version: the headset gates Delay Reporting on it (2026-10-13)

- BlueZ's A2DP source SDP record says A2DP **1.4**. The phone's says **1.3**. The Px7 reads this before AVDTP Discover.
- With bluetoothd patched to advertise 1.3: the headset now includes **Delay Reporting on every SEP** (it had omitted it from all of them for us). Our SetConfiguration now includes Delay Reporting, the headset sends a Delay Report after Open, and the AVDTP sequence matches the phone's.
- The A2DP 1.3 run streamed 1498 packets (real encoder, phone framing, every 14 ms) after Start was accepted: **still silent.** The headset played its chime when the profile switched (Close of the aptX HD stream at SEID 7, then reconfigure to SEID 9).
- So A2DP 1.3 fixes the capability difference but not the silence on its own. Still to check from source-diff.md: AVRCP play status ("playing" after Start) with absolute volume, and configuring aptX Adaptive first instead of switching from aptX HD.

## combined test (2026-10-13) and a user observation

- A2DP 1.3 + aptX Adaptive configured directly (only SBC + aptx_adaptive offered, no aptX HD first) + mpris-proxy with mpv playing a 1 kHz tone + absolute volume (SetAbsoluteVolume sent): **still silent**. AVRCP play status was still reported as PAUSED during the stream. Delay Reporting was configured.
- **User observation:** with a Qualcomm-based USB aptX Adaptive dongle, the Px7 S2e **never worked at 48 kHz**; the dongle had to be forced to **96 kHz**. If that's a property of the headset rather than the dongle, our 48 kHz R1 stream may be exactly the case the headset doesn't play. But the phone plays at 48 kHz (its SetConfiguration says 48k), so it may be the dongle's own behaviour. To check: whether the Px7 advertises 96k at all (caps byte 6 = 0x19, while 96k is 0xA0 per the public header, so probably not), and what bit 0x01 of byte 6 means.

## final software-side test (2026-10-13): everything combined, still silent

- Patched bluetoothd advertising A2DP 1.3 (the headset then offers Delay Reporting; our SetConfiguration includes it; the headset sends a Delay Report).
- The phone's link settings applied on the connection before Start: Write Link Policy 0 (no sniff, no role switch) and Flow Specification (outgoing, guaranteed), both successful.
- The phone's own captured aptX Adaptive packets (2609 packets, about 36 s) replayed in the phone's exact on-air layout, 14 ms pacing; 3208 packets went out after Start was accepted.
- The Px7 S2e stayed connected and silent. It played its disconnect chime right after connecting, before any audio.

Conclusion: every AVDTP, SDP, AVRCP, L2CAP, HCI link-policy and media-byte difference we could find between the working phone and this host has now been matched. What's left is the controller (Realtek RTL8922 vs Qualcomm), below what the host can control. Next: test with a Qualcomm-based HCI controller (e.g. QCNFA765/WCN6855 or QCA6174A M.2 card in an M.2-to-USB adapter), keeping A2DP 1.3.

## the "disconnect chime right after connecting" (2026-10-13)

The user notes this chime isn't normal headset behaviour. What the captures show around it:

- The Bluetooth link is never dropped at the HCI level when the chime plays in the middle of a connection.
- In fin2, both mid-test disconnects came from the **host**: KDE's Bluetooth applet (plasmashell, D-Bus :1.52) called `Device1.Disconnect`, then `Device1.Connect`, about 2 s after a stream was suspended. The user wasn't clicking it at those times. Something in KDE (bluedevil / the audio applet) is cycling the device, probably when the PipeWire node disappears or errors. That gives a real disconnect/reconnect, hence the chime.
- The connection sequence after connecting is near-identical to the phone's: SDP, RFCOMM/HFP, AVDTP signalling, AVCTP, and the headset's own SDP queries (PSM 1). The phone opens PSM 25 (AVDTP) before the headset queries it, and we do the same. One difference: the phone opens AVDTP signalling directly, while our host first configures **aptX HD (SEID 7)**, then Closes it and reconfigures to SEID 9 (Adaptive). With the stock aptX HD default, the headset briefly runs an aptX HD stream before switching.
- Hypothesis for the chime: the HD → Adaptive reconfigure (Close of SEID 7, Set Configuration on SEID 9) makes the headset tear down and rebuild its audio path, and it plays its "disconnected" prompt while doing so. That would fit "chime right after connecting" (when PipeWire immediately switches to the profile saved as default) and "chime when the profile switches".
- To test: make aptX Adaptive the **first and only** codec configured on connect (the earlier combined test did this with `bluez5.codecs = [sbc, aptx_adaptive]`; that run was also silent, and whether it chimed wasn't recorded), and stop KDE from cycling the device (e.g. `systemctl --user stop plasma-kded6` or disable bluedevil during the test).

## clean-path test (2026-10-13): the chime explained, still silent

- With only SBC + aptX Adaptive offered (no aptX HD set up first) and KDE's bluedevil module unloaded, connecting gave the headset's normal **connect chime only**, no disconnect chime. So the "disconnect chime right after connecting" comes from the HD → Adaptive codec switch (Close of SEID 7, then reconfigure to SEID 9) on connect. The plugin should make sure aptX Adaptive is set up directly when it's the chosen profile.
- Same clean connection with A2DP 1.3: SetConfiguration SEID 9 + Delay Reporting, Open, Start, then a 1 kHz tone through mpv for 25 s. **Still silent.**

## source type 2 implemented (2026-10-13)

- Per §7E, `select_config` now sends source type 2: byte 6 = the chosen rate bit | 0x02, with the sink's bits 0..2 cleared. For the Px7 S2e: `d7 00 00 00 ad 00 12 02 37 6a 00 af 64 4b 00 00 aa` + zeros. `validate_config` accepts source type 0 or 2 and only checks bits 3..7 for the rate.
- Dev switches (`dev-replay` builds only) for A/B on the headset: `APTX_ADAPTIVE_SOURCE_TYPE=0|2`, `APTX_ADAPTIVE_SSRC=0x..` (whole SSRC), `APTX_ADAPTIVE_TS=us|samples`. `scripts/dev-replay.sh phone --no-replay` sets them while running the real encoder.
- Still open from §7E: the RTP timestamp unit with source type 2, and whether the SSRC low byte selects the latency mode (`a1`..`a4` vs the `ae` we send).

## BREAKTHROUGH (2026-10-13 07:32): source type 2 → audible output

With SetConfiguration byte 6 = `0x12` (source type 2, spec §7E), A2DP 1.3, phone framing (µs timestamps, SSRC `00 00 00 ae`), and the real encoder on a 1 kHz tone, the Px7 S2e **produced sound**: "crackly morse code". The headset now decodes our stream; what remains is timing or quality. Next: SSRC (`a3`), timestamp unit (samples), and packet pacing (the drops after packet 25).

### follow-up: why it's crackly (2026-10-13)

btmon, time from sending each media ACL packet to the controller's Number of Completed Packets:

| run | source type | completion latency median / p90 |
|---|---|---|
| a13b, fin2 (before §7E) | 0 | 20 / 30 ms (normal; the headset silently ignored the audio) |
| st2 (first source-type-2 run, µs timestamps) | 2 | **310 / 430 ms** |
| ts-samples (sample-count timestamps) | 2 | 42 / 79 ms |
| aptX HD baseline, same session | n/a | 22 / 33 ms, clean audio |

- With source type 2 the headset **accepts and decodes** the stream, but it takes the media far more slowly than it arrives. The controller's buffers (8 ACL buffers) fill up, so PipeWire's socket backs up and drops packets (sequence gaps; 511 of an expected 1596 packets went out in 22 s). That gives the crackling, the slow start and the slow stop.
- With µs timestamps it was far worse than with sample-count timestamps. So the sink uses the timestamp to pace itself: a timestamp that runs too fast or too slow makes the sink hold back or drain slowly, which stalls the link at the baseband level.
- With samples (+672 per 14 ms packet), the stall shrinks but doesn't go away. Candidates: the expected timestamp clock rate is something else (e.g. 1/90 kHz or 1/32 kHz units?); the SSRC should be `a3` in this mode instead of `ae`; or the TTP bytes in our config are too small for a non-Q2Q source.
- Next: try SSRC `a3`, then the timestamp at other rates; watch the completion latency in btmon. Target: about 20 ms like aptX HD.

### SSRC a3 + sample timestamps (2026-10-13, round 2 of source type 2)

- Still crackly. The user heard a short stretch where it kept up, at very low quality.
- Our host sent only about 23 packets/s instead of 71.4 (14 ms period). PipeWire drops packets when `send()` returns EAGAIN, so 2 of every 3 periods are skipped. The cause is the slow media completion at the controller: median 78 ms, p90 267 ms in the last session, against about 20 ms for aptX HD and for our earlier (ignored) source-type-0 streams.
- The headset also set up an eSCO link at one point. HFP was active in this session: 2543 SCO frames, all before the final AVDTP Start. Not a factor in the final session.
- So the headset is consuming aptX Adaptive at roughly a third of real time. Plausible causes, to test next:
  1. **The sink paces its intake from the RTP timestamp, in units we don't match.** µs made it much worse (310 ms completion); samples (+672 per packet) is better but not right. Next candidates: 1/90 kHz? `ms·4`? Timestamps in the encoder's sample rate but per channel times 2 (+1344)?
  2. **Packet size / bitrate:** the sink may expect a packet rate matching its own table for source type 2 (maybe a different period, e.g. 20 ms at 279 kbps).
  3. **TTP bytes:** the config echoes the sink's `37 6a 00 af 64 4b`. For a non-Q2Q source the public defaults are `50 64 64 64 ff ff`; a TTP of 0x00 for HQ low may make the sink underrun and back-pressure.

## §7F RTP fields implemented (2026-10-13)

- Default RTP header with source type 2 is now §7F: timestamp = `adjust_ms << 16 | samples per channel in this packet` with adjust 0 (`00 00 02 a0` on every 672-sample packet), SSRC `0x000000a3` (HQ; `0x000000a1` if the profile were LL0), constant for the session. Sequence +1, swapped payload right after the 12-byte header. First packets: `80 60 00 01 00 00 02 a0 00 00 00 a3`, `80 60 00 02 00 00 02 a0 00 00 00 a3`, …
- The old `00 00 00 ae` SSRC (header bytes 4..7) and the µs clock are no longer the default. Dev switches (`dev-replay`): `APTX_ADAPTIVE_TS=samples-per-packet|us|samples` (the last two are the old running clocks), `APTX_ADAPTIVE_SSRC=0x..` still overrides; non-default values are logged at init.
- Still open: the §7F UNCERTAIN older-sink reading of the timestamp as µs, and whether the latency adjustment should ever be non-zero.

## WORKING (2026-10-13 07:51): aptX Adaptive plays cleanly on the Px7 S2e

- Default plugin build: source type 2 (byte 6 `0x12`, §7E), RTP timestamp `0x000002A0` every packet and SSRC `0x000000A3` (§7F), real encoder, 364 kbps / 14 ms / 656-byte packets.
- btmon: 71.2 packets/s, 5 missing in 25 s, controller completion median 20 ms / p90 31 ms (same as aptX HD). The user hears it as smooth, quality comparable to aptX HD.
- **Works with stock BlueZ 5.87 (A2DP 1.4 advertised).** The A2DP 1.3 patch isn't needed: it only made the sink offer Delay Reporting. The silence was entirely the source type.
- Remaining: the HQ/LL mode switch, automatic codec selection, the encoder delay (`get_delay`), and keeping aptX HD from being set up first on connect (codec switch chime).

## stutter report (2026-10-13 ~08:00)

- The user reported stutter that came and went ("started fine, got worse, sometimes gets a bit better") and later cleared up, possibly system load.
- A btmon capture during it shows our side as perfect: 71.4 packets/s, no sequence gaps, completion median 20 ms / p90 31 ms, 0 ppm drift of send times against the 14 ms audio clock.
- Hypothesis to guard against anyway: clock drift between the host's audio clock and the sink's. With source type 2 the sink advances TTP purely by the sample count (§7F), with only a ±0.5 % rate trim. A sustained source/sink rate mismatch moves the sink's latency towards its min/max window and triggers TTP resets, which are audible glitches. The phone's packets show small adjustments (12/14 ms steps, ±1 µs jitter).
- Planned mitigation: drive the timestamp's high 16 bits (signed TTP adjust, ms) from a slow estimate of host/sink drift, e.g. from socket backlog (`abr_process` unsent bytes) and/or the sink's Delay Reports, keeping the sink's latency centred. Needs a spec answer: units and safe range of the TTP adjust, and whether changing it causes a reset (the dirty-side note says each change calls "source changed", which re-sets the target latency; check whether that glitches).

## stutter capture 2 (2026-10-13 ~08:06): a slow drift

- 118 s btmon capture. The user heard the stutter start at about 100–105 s.
- Send side clean the whole time: exactly 357 packets per 5 s, completion median 19–20 ms / p90 30 ms, no response or events from the sink at all.
- One gap at stream start: sequence 25 → 33 (7 packets, 98 ms, lost about 0.4 s in). It's the same "drop after packet 25" seen in earlier captures: probably the encoder/socket startup in PipeWire. It's a one-off, but the sink gets a 98 ms hole. Worth fixing separately.
- **Host audio rate against the capture clock: +80 ppm.** We send audio about 80 µs/s faster than real time, as measured by the kernel. If the sink's clock matches the kernel's, the sink's buffer grows by about 8 ms in 100 s.
- So the onset at about 100 s matches a slow drift crossing a latency limit after roughly 8–10 ms of creep. That's a tight window, which suggests the sink's max−target margin is small, or the 98 ms start hole already put the sink near a limit. The ±0.5 % `sp_adjust` trim (dirty-side note) should absorb 80 ppm in principle, so it may be too slow or not driven by buffer level.
- Test: the TTP-adjust correction (drift PR), plus not losing packets at stream start.
