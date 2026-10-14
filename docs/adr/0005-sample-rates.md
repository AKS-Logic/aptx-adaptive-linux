# 0005 - negotiated sample rates

## problem

The encoder matches reference packets at 44.1 and 96 kHz, but the PipeWire plugin accepts only 48 kHz. Extra rate bits alone would leave the PCM format, encoder resets and bitrate-table selection inconsistent. At 44.1 kHz a packet period can contain an odd number of interleaved samples, while PipeWire must consume whole stereo frames.

## decision

Use the existing `aptx_adaptive::Config` as the stream contract. Caps validation constructs an HQ Config because the A2DP configuration bytes do not choose the runtime mode. Raw format derives from its rate and channels. Init applies the requested profile and passes the Config to Session. Alternate-profile rows and resets change only `Config.profile`, preserving rate and channels.

R2 stereo supports 44.1 kHz in HQ or LL, and 96 kHz in HQ. The reference refuses LL at 96 kHz (`codec-core.md` section 10.F), so this plugin does too. R1 stays at 48 kHz under the latest wrapper rule (`a2dp-integration.md` section 7J). Lower-level encoder vectors at other R1-labelled rates remain valid library tests, not permission to negotiate those rates with an R1 sink.

## rate selection

The normal connection stays at 48 kHz. PipeWire's existing `media_codec_audio_info.rate` supplies a preference, configured through the BlueZ monitor:

```conf
monitor.bluez.properties = {
	bluez5.default.rate = 96000
}
```

Put this in a WirePlumber configuration fragment, restart WirePlumber and reconnect. Use 44100 for 44.1 kHz. The preference also affects other Bluetooth codecs. It does not set the PipeWire graph rate or increase the resolution of source audio.

An exact supported preference wins. Otherwise select a supported rate in order 48, 44.1, 96 kHz. Null or unsupported preferences use that fallback. The source writes the verified rate bits 0x08, 0x10 or 0x20 with host source type 0x02.

Keep `fill_caps` at the existing 48 kHz-only R1 layout until its advertisement compatibility question is answered. Host selection receives the raw remote caps and may choose another supported rate for an R2 sink. Before intersecting features, exclude 44.1 kHz when an extension-1 sink sets decoder feature bit 7. This conservatively avoids the out-of-scope R3 encoder, including a lone bit 7 until its meaning is clarified. Keep the required source-owned R2 block and features intersection.

Configuration-only callbacks cannot recover feature bits erased by that intersection. They do not establish the physical peer's revision. Do not add a process-wide last-selected-caps cache or encode provenance in reserved bytes. The unanswered R3 and advertisement questions remain in `spec/QUESTIONS.md`.

## rows and input blocks

Derive the high-rate table flag centrally from Config. R2 HQ at 96 kHz uses index 5, not the 48 kHz index 1. QHS, TWM, split TX and enhanced LL remain outside this change.

PCM block size stays fixed throughout a session. Compute the GCD of reachable packet periods in milliseconds, then convert that duration to whole PCM frames. This preserves existing 48 kHz blocks and gives 44-frame input at 44.1 kHz and 96-frame input at 96 kHz. Include every reachable bitrate level and mode when finding that GCD.

Keep the coded packet period separately in exact interleaved samples from `period_interleaved.json`, including odd values. Never round that period to make a PCM block divide it. Stream already retains unconsumed input. PipeWire schedules from actual consumed block frames and the negotiated rate, so a 44-frame block is not treated as exactly one millisecond.

The negotiated rate also drives mode resets, RTP sample counts, delay and drift. Retain the supplied nominal RTP rule `floor(Pi / channels)`, zero adjustment and mode SSRC. Fractional-rate timing questions go to the coordinator rather than an invented timestamp scheme. Replay must not return half-frame PCM blocks at an odd period.

## alternatives and synthesis

Two independent design sketches and a separate judge considered a private checked negotiated-stream enum and direct use of Config. Config is the selected base. It already holds the required stream fields; a second runtime model cannot recover missing peer provenance. Caps enforces the plugin's narrower rate policy before returning Config. Keep the first sketch's explicit boundary validation and raw-caps R3 guard.

Model the Domain keeps one stream description. Boundary Discipline places byte and PCM checks in caps. Laziness Protocol avoids a duplicate model, a new codec-specific setting and unrelated public Stream API changes. Generic capacity or preparation changes require a reproduced feature defect, not speculative cleanup.

A GCD of truncated interleaved periods can yield half a stereo frame at 44.1 kHz. A GCD of rounded frame periods changes packet accounting. Both are rejected. A runtime rate property is also rejected because rate changes require a new transport.

## verification

Exercise the exported media codec callbacks through the compiled shared library, from selection and raw format to init, PCM consumption, RTP fields and payload bytes. Compare public Stream output with the supplied reference packets. Test rate-preserving mode switches, bitrate changes, 96 kHz LL refusal, fixed whole-frame blocks, bounded input and no data-thread allocations.

Independent exported-callback verification also compares nonzero, spec-faded PCM packets against reference-anchored encoder output and probes actual allocator calls. Warm callbacks allocate nothing. The first ABR log write triggers a pre-existing 80-byte loader TLS allocation on this host, which unit tests suppress. That cold-start limitation is recorded in QUESTIONS rather than claimed fixed by this change.

R2 headset playback at these rates remains unverified. Px7/R1 playback and its tested HQ/LL switch remain unchanged. Config-3 delay at 44.1 kHz retains the existing 255-frame assumption; config-4 delay remains unmeasured. The 44.1 kHz LL vector's packet-count discrepancy is recorded as an open timing question, not resolved by payload parity.
