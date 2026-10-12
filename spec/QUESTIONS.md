# questions for the spec side

## plugin integration

1. **Answered:** see `a2dp-integration.md` §1.1–§1.3 (layout, bit values, R1 vs R2 detection) and §1.5 (what to send to the Px7 S2e). Implemented in `spa-codec-aptx-adaptive/src/caps.rs`: `select_config` gives the §1.5 bytes for the Px7. What our own `fill_caps` should advertise is still open (item 9 below). Originally: A2DP capability layout after the 6-byte vendor header, field meanings, which bits the source should set in its own caps, and how to pick a configuration from the sink's caps.
2. **Answered:** yes, 40 bytes; see `a2dp-integration.md` §1.5 (recommended SetConfiguration). Is the configuration the source sends back the same length as the sink's caps? PipeWire only offers a codec when `select_config` returns exactly `caps_size` bytes.
3. **Partly answered:** each packet carries an 8-byte header and a 16-bit-swapped payload, and an RTP header is used; see `a2dp-integration.md` §7. Whether the 8 bytes go into the RTP timestamp/SSRC or follow a normal RTP header is still open (see "from a2dp-integration" Q1). Do media packets carry an RTP header (12 bytes, like aptX HD), any extra payload header, or neither? The plugin implements both readings behind `session::FRAMING` in `spa-codec-aptx-adaptive`, defaulting to the spec's hypothesis (`HeaderInRtp`: header bytes 0..3 as RTP timestamp, 4..7 as SSRC).
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
