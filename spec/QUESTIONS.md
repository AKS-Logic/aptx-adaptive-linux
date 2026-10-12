# questions for the spec side

## plugin integration

1. A2DP capability layout after the 6-byte vendor header (vendor 0x000000D7, codec 0x00AD): field meanings, which bits the source should set in its own caps, and how to pick a configuration from the sink's caps. The plugin currently echoes the remote caps unchanged.
2. Is the configuration the source sends back the same length as the sink's caps? PipeWire only offers a codec when `select_config` returns exactly `caps_size` bytes.
3. Do media packets carry an RTP header (12 bytes, like aptX HD), any extra payload header, or neither? Currently assumed RTP with PT 96, SSRC 1, timestamp in samples.
4. PCM input: sample format and bit depth the encoder expects (currently S24_32LE interleaved stereo at 48 kHz), and samples per channel per encoder frame.
5. How many encoder frames go into one A2DP packet, and how that relates to the MTU and the target bitrate.
6. Encoder delay in samples, for `get_delay`.
7. Bitrate adaptation inputs: what `abr_process` should react to (socket backlog in bytes) and which encoder parameter it changes.
