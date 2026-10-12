# questions for the spec side

## plugin integration

1. **Answered:** see `a2dp-integration.md` §1.1–§1.3 (layout, bit values, R1 vs R2 detection) and §1.5 (what to send to the Px7 S2e). A2DP capability layout after the 6-byte vendor header (vendor 0x000000D7, codec 0x00AD): field meanings, which bits the source should set in its own caps, and how to pick a configuration from the sink's caps. The plugin currently echoes the remote caps unchanged.
2. **Answered:** yes, 40 bytes; see `a2dp-integration.md` §1.5 (recommended SetConfiguration). Is the configuration the source sends back the same length as the sink's caps? PipeWire only offers a codec when `select_config` returns exactly `caps_size` bytes.
3. **Partly answered:** each packet carries an 8-byte header and a 16-bit-swapped payload, and an RTP header is used; see `a2dp-integration.md` §7. Whether the 8 bytes go into the RTP timestamp/SSRC or follow a normal RTP header is still open (see "from a2dp-integration" Q1). Do media packets carry an RTP header (12 bytes, like aptX HD), any extra payload header, or neither? Currently assumed RTP with PT 96, SSRC 1, timestamp in samples.
4. **Answered:** interleaved 24-bit right-justified, sign-extended in 32-bit containers (S24_32LE) at 48 kHz stereo; 672 samples per channel per packet for HQ level 3. See `a2dp-integration.md` §3.4 and §4.1. PCM input: sample format and bit depth the encoder expects (currently S24_32LE interleaved stereo at 48 kHz), and samples per channel per encoder frame.
5. **Partly answered:** the encoder emits one 656-byte packet per 14 ms period at 364 kbps, and the packet size is capped by MTU − 8; see `a2dp-integration.md` §4 and §5. How codec packets map onto A2DP/RTP packets depends on "from a2dp-integration" Q1. How many encoder frames go into one A2DP packet, and how that relates to the MTU and the target bitrate.
6. Encoder delay in samples, for `get_delay`.
7. **Partly answered:** the reference adapts the bitrate from the sample deficit (§5 step 7) and from controller level feedback (§8.2), not from socket backlog. Which backlog signal a Linux source should map onto the quality level is still open. Bitrate adaptation inputs: what `abr_process` should react to (socket backlog in bytes) and which encoder parameter it changes.

## from a2dp-integration

1. Does the RTP packetiser map the 8-byte header into the RTP timestamp and SSRC fields, or send it as a payload prefix after a normal 12-byte RTP header? Needs an over-the-air capture from an Android source. (`a2dp-integration.md` §7.2)
2. What exactly is the subtracted "buffered audio" term in the TTP adjustment? Does a sink tolerate a constant TTP? (§7.1)
3. What are the OTA sample-rate bit values for 44.1 and 96 kHz? What does source-type bit 0x01 mean? What do the Px7's TTP bytes `37 6a 00 af 64 4b` mean, and in what units? (§1.2, §1.5)
4. What does the create-parameter flag mean? The reference always sets it to 1. (§3.1)
5. Does the Px7 accept the echoed aptx_data block, or does it need the R1 defaults `50 64 64 64 ff ff`? (§1.5)
6. Does any sink depend on the start-up fade-in? (§5 step 2)
7. What is the exact bitrate formula when the packet size is capped by the MTU? (§4)
