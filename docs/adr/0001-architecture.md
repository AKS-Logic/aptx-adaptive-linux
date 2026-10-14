# 0001 - architecture

## goal

Play audio from Linux straight to an aptX Adaptive headset over A2DP, without a USB dongle, on stock BlueZ and PipeWire.

## status

Works on the Bowers & Wilkins Px7 S2e in HQ and LL at 48 kHz stereo, with stock BlueZ 5.87. The Px7 S2e is an **R1** sink (no capability extension, spec §1.5). HQ starts on table index 0, level 3, at 364 kbps with 14 ms / 656-byte packets. The live ABR test reached level 4, 427 kbps, with 12 ms / 656-byte packets. LL runs at 265 kbps with 10 ms / 348-byte packets. Runtime HQ → LL → HQ switching, LL on connect, and card-profile switches are recorded in [the live test log](../../spec/QUESTIONS.md#current-status-2026-10-14). The user also confirmed profile selection is tested on October 15. No other headset has been tried.

## clean room

The codec is built from written specs only. The reverse engineering happens in a separate repo, `aptx-re-quarantine`, which is never read by anyone implementing here. What crosses over:

- prose specs, copied into `spec/`
- black-box test vectors (PCM in, bitstream out), copied into `tests/vectors/`
- for the numeric tables, a locator spec rather than the values (`spec/table-extraction.md`): symbol names, layouts, transforms and a SHA-256 of each table

The repo holds no table values. `crates/aptx-adaptive/build.rs` reads them at build time from the user's own copy of the vendor encoder library, `libaptXAdaptiveEnc.so`, named by `APTX_ADAPTIVE_SO`. It follows the spec mechanically (ELF symbols, fixed offsets, the hashes) and fails the build unless every table matches its hash. The build needs that file; without it nothing compiles. mise and `scripts/install-plugin.sh` default it to `.scratch/libaptXAdaptiveEnc.so`, and CI downloads it from the `APTX_ADAPTIVE_SO_URL` secret.

Implementers must not open the quarantine repo, decompiled output or the vendor binaries, or run IDA. The build script is the only thing that reads the vendor library, and it is written from `spec/table-extraction.md` alone. Nobody inspects the file or prints the tables. Ambiguities go in `spec/QUESTIONS.md`.

## layout

- `crates/aptx-adaptive` - pure Rust encoder, no I/O and no unsafe. Takes PCM, returns codec packets. Bit-exact against the vectors.
- `crates/spa-codec-aptx-adaptive` - `cdylib` that implements the PipeWire bluez5 media codec plugin ABI (`SPA_VERSION_BLUEZ5_CODEC_MEDIA` 16, PipeWire 1.6.x) and wraps the encoder.

## A2DP configuration

- **Sample rates.** R1 remains 48 kHz. R2 supports 44.1 kHz HQ/LL and 96 kHz HQ through host selection and PipeWire's rate preference. The source advertisement remains 48 kHz in the R1 layout. The new paths are covered by reference packets and compiled callbacks, not hardware playback. See [ADR 0005](0005-sample-rates.md) for rate selection and the R3 guard.

- **Source type 2** (spec §7E). Config byte 6 carries the rate in bits 3..7 and the source type in bits 0..2. Source type 0 needs link-clock timestamps from the controller, which a host encoder can't supply; the sink accepts it and stays silent. Source type 2 makes the sink time playback itself. For the Px7 at 48 kHz, byte 6 = `0x12`. Bit 0x01 is sink-only and never set.
- **Capabilities** (spec §7J). `fill_caps` advertises 48 kHz, source type 2, stereo and joint stereo, and an R1 block. `select_config` detects R2 from extension byte 15 = 1 and sends the source's own R2 block. R1 configurations echo bytes 8..25 and zero 26..39, with the latency replacements below. Whether a source should advertise the R2 extension remains open in `spec/QUESTIONS.md`. R2 negotiation is covered by tests, not by hardware.
- **Target latency** (spec §7G). With source type 2 the sink uses config byte 10 (TTP HQ, 2 ms units) as its target latency. For R1 we echo the sink's TTP bytes, except that a zero HQ byte is replaced with `0x64` (200 ms). A 0 ms target makes the sink stutter after a minute or two. LL0 (byte 8, ms) is raised to at least `0x50` (80 ms) to absorb host jitter (§7H). The Px7's config is `d7 00 00 00 ad 00 12 02 50 6a 64 af 64 4b 00 00 aa` + zeros. Delay Reports confirmed 200 ms in HQ and 80 ms in LL with a test bluetoothd advertising A2DP 1.3. Stock BlueZ 5.87 advertises A2DP 1.4, for which the Px7 omits Delay Reporting. Playback still works.
- **Drift** is the sink's job: with a proper target it trims its rate itself. The TTP adjust (timestamp high word) stays 0. The plugin logs a drift line every 10 s for diagnosis only.

## RTP framing

Per spec §7D and §7F:

- one codec packet per RTP packet, directly after a plain 12-byte RTP header; no extra in-band header
- `80 60` (V=2, PT 96), sequence +1 per packet
- timestamp is not a running clock. Low 16 bits = samples per channel in the packet (`0x000002A0` for 14 ms at 48 kHz), high 16 bits = TTP adjust (0). It changes with the packet period after a mode or level change. A running clock makes the sink think every packet holds far too much audio.
- SSRC selects the latency mode and stays constant within a mode: `0xA3` HQ, `0xA1`/`0xA2` LL0/LL1, `0xA4` TWS. It changes exactly when the encoder switches between HQ and LL (§7H); we only send `0xA3` and `0xA1` (no sideband, so LL stays LL0, §7K).
- payload is the codec packet with each 16-bit word byte-swapped

## HQ and low latency

- **LL bitstream** (spec §7J, codec-core §10.D, §10.F). An R1 sink gets the HQ bitstream (config 0) on LL row 0: 265 kbps / 10 ms / 348-byte packets. An R2 sink gets config 4 on LL row 1: 270 kbps / 10 ms / 348 bytes. At 48 kHz both use timestamp `0x000001E0`; R2 at 44.1 kHz uses `0x000001B9`. SSRC is `0xA1`. Config 1 (low-power) is never given to a sink. Both are byte-exact through the stream loop against the parity vectors `c0_48k_ll`, `c0_44k_ll`, `c4_48k_i1_L3` and `c4_44k_i1_L3`. LL at 96 kHz is refused.
- **Switching** is the codec property `SPA_PROP_quality` (0 = HQ, 1 = LL), LDAC style. The two card profiles and install-time `APTX_ADAPTIVE_DEFAULT_MODE=hq|ll` choose the starting mode (ADR 0003). Only the HQ endpoint owner reads the legacy device setting `bluez5.a2dp.aptx-adaptive.mode = hq|ll`. A runtime choice lives in the node's props and is lost on reconnect.
- **Switch sequence** (§7K), with packets flowing throughout: a 4800-frame raised-cosine fade-out, 900 ms of silence, a 4800-frame fade-in. About 200 ms into the silence the encoder is reset in place (level 3, LL0, loop counters zeroed), and the packet size, period, timestamp and SSRC change from the next packet. Asking for the old mode mid-switch reverses it. Every stream also starts with a 20 ms mute and a 4800-frame fade-in (§7I). Curve: 0.5·(1 − cos(π(j+1)/4801)), truncated toward zero.
- **Block size.** `media-sink.c` reads `get_block_size` only at transport start and never calls `get_interval` for A2DP. It times packets from consumed blocks and the negotiated rate. The block uses the gcd of every reachable HQ and LL packet period in milliseconds, including ABR levels, converted to whole PCM frames. At the normal MTU it is 96 frames for R1/48 kHz, 48 for R2/48 kHz, 44 for R2/44.1 kHz and 96 for R2/96 kHz. Exact interleaved packet periods remain separate and can be odd. See ADR 0005.
- **Reported delay.** `media-sink.c` calls `get_delay` only when the transport starts, so the encoder delay it reports doesn't follow a later HQ↔LL switch. R1 is unaffected (config 0 both ways, 90 frames). On an R2 sink it stays at its initial value: start in HQ and it reads 255 frames (config 3) while in LL, start in LL and it reads 0 (config 4's delay isn't measured) while in HQ.
- **Real-time safety.** `update_props` runs on the data thread. Both rows are worked out at init, the encoder's buffers are sized for the largest packet up front and reset in place, and the props are atomics. The switch path doesn't allocate, free, lock or log (tests check this with a counting allocator). Refused switches are counted and shown in the drift log line. After a refused switch, Props reports the profile actually running.
- **LL deficit** (§7K, codec-core §10.C): LL never carries a deficit; it's zeroed after every packet. The reference's extra flush there never produces anything, so it isn't made. The HQ 25 % bitrate drop clamps at 200 kbps (§7J).

## adaptive bitrate

PipeWire's `reduce_bitpool` and `increase_bitpool` callbacks step to the next level with a different bitrate (spec §7L). R1 HQ reaches levels 2..4. R2 HQ reaches 2..5 on index 1 at 44.1/48 kHz and index 5 at 96 kHz. R1 LL has one bitrate, so the callbacks leave it at level 3. A level change applies at a packet boundary without a fade or encoder reset. Requests wait during a mode switch, whose reset restores level 3. `abr_process` only records drift and backlog. It does not drive level changes.

The stream matches all four `tests/vectors/levels/` runs byte for byte. The October 13 live Px7 test confirmed an automatic 3 → 4 increase and minutes at 427 kbps without backlog growth. Congestion-driven reduction did not occur in that test and remains covered only by synthetic tests and vectors.

An October 15 allocator probe through the compiled plugin found one 80-byte loader TLS allocation on the first ABR log write, also present at the 48 kHz base. Warm callbacks allocate nothing. Unit allocation tests suppress those log writes, so they do not prove cold callback behaviour. This remains open in `spec/QUESTIONS.md`.

## PipeWire integration without patching PipeWire

PipeWire 1.6 loads codec plugins from a fixed list of filenames and only offers codecs whose `id` is in `enum spa_bluetooth_audio_codec`. Neither has aptX Adaptive, so the plugin takes over an existing plugin file and borrows existing codec ids. The default since ADR 0004 is the aptX file: the plugin replaces `libspa-codec-bluez5-aptx.so`, takes the aptX HD and aptX ids, and passes the real aptX codecs through. The original setup below is still available as the `ldac` and `lc3plus_hr` slots:

- built as `libspa-codec-bluez5-lc3plus.so`, factory `api.codec.bluez5.media.lc3plus`, `name = "aptx_adaptive"`, description "aptX Adaptive". It shadows a real LC3plus plugin if one is installed.
- `id` is `SPA_BLUETOOTH_AUDIO_CODEC_LDAC` with `APTX_ADAPTIVE_SLOT=ldac`, or `SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR` with `lc3plus_hr` (set by `scripts/install-plugin.sh` in the drop-ins). See ADR 0002.
- installed to `~/.local/lib/spa-0.2/bluez5/`. Systemd user drop-ins set `SPA_PLUGIN_DIR=~/.local/lib/spa-0.2:<system plugindir>` for `wireplumber.service` (where the codec loader runs) and `pipewire.service`. The variable replaces the default path, so the system dir must be in it.
- **Ranking.** `codec_order` in `codec-loader.c` decides which codec BlueZ configures on connect. In the `aptx` slot aptX Adaptive takes aptX HD's rank, below LDAC only (ADR 0004). In the LDAC slot aptX Adaptive ranks above aptX HD, aptX and AAC, so it's configured directly and is the plain `a2dp-sink` profile unless the headset also has LDAC. That collision is nondeterministic (ADR 0002). In the `lc3plus_hr` slot it ranks below them. A supported higher-ranked codec is set up first, and switching to aptX Adaptive by hand costs a codec-change chime. Headsets that also have LDAC should use the default `aptx` slot or `lc3plus_hr` (ADR 0004).
- if `monitor.bluez.properties` sets `bluez5.codecs`, it must include both `"aptx_adaptive"` and `"aptx_adaptive_ll"` to expose both profiles.

Later: send PipeWire a patch that adds a proper codec ID and loader entry, then drop the hijack.

## limitations and untested paths

- R2 sinks in general are untested
- the R2.1 switch-to-LL silence of 800 blocks (§7K): R2 switches use 900, like R1
- the config 4 encoder delay isn't measured, so `get_delay` reports 0 in R2 LL
- playback on R2 hardware at 44.1 or 96 kHz, including fractional-rate pacing
- the R3 bitstream used by some sinks at 44.1 kHz; those sinks fall back to another supported rate
- congestion-driven bitrate reduction has not been observed on hardware
- the WirePlumber guard for headset-initiated aptX or aptX HD has not been tested on a headset that initiates configuration (ADR 0004)

No decoder and no speech/BLE profiles.
