# 0001 - architecture

## goal

Play audio from Linux straight to an aptX Adaptive headset over A2DP, without a USB dongle, on stock BlueZ and PipeWire.

## status

Works on the Bowers & Wilkins Px7 S2e, HQ mode, 48 kHz stereo, with stock BlueZ 5.87. The Px7 S2e is an **R1** sink (no capability extension, spec §1.5), so the encoder runs at decoder compatibility level R1: HQ table index 0, 364 kbps, 14 ms / 656-byte packets. No other headset has been tried.

## clean room

The codec is built from written specs only. The reverse engineering happens in a separate repo, `aptx-re-quarantine`, which is never read by anyone implementing here. What crosses over:

- prose specs and numeric tables, copied into `spec/`
- black-box test vectors (PCM in, bitstream out), copied into `tests/vectors/`

Implementers must not open the quarantine repo, decompiled output or the vendor binaries, or run IDA. Ambiguities go in `spec/QUESTIONS.md`.

## layout

- `crates/aptx-adaptive` - pure Rust encoder, no I/O and no unsafe. Takes PCM, returns codec packets. Bit-exact against the vectors.
- `crates/spa-codec-aptx-adaptive` - `cdylib` that implements the PipeWire bluez5 media codec plugin ABI (`SPA_VERSION_BLUEZ5_CODEC_MEDIA` 16, PipeWire 1.6.x) and wraps the encoder.

## A2DP configuration

- **Source type 2** (spec §7E). Config byte 6 carries the rate in bits 3..7 and the source type in bits 0..2. Source type 0 needs link-clock timestamps from the controller, which a host encoder can't supply; the sink accepts it and stays silent. Source type 2 makes the sink time playback itself. For the Px7 at 48 kHz, byte 6 = `0x12`. Bit 0x01 is sink-only and never set.
- **Target latency** (spec §7G). With source type 2 the sink uses config byte 10 (TTP HQ, 2 ms units) as its target latency. We echo the sink's TTP bytes, except that a zero there is replaced with `0x64` (200 ms); a 0 ms target makes the sink stutter after a minute or two. LL0 (byte 8, ms) is raised to at least `0x50` (80 ms) to absorb host jitter (§7H). The Px7's config is `d7 00 00 00 ad 00 12 02 50 6a 64 af 64 4b 00 00 aa` + zeros. The sink's Delay Report after Start reads 200 ms when this is right.
- **Drift** is the sink's job: with a proper target it trims its rate itself. The TTP adjust (timestamp high word) stays 0. The plugin logs a drift line every 10 s for diagnosis only.

## RTP framing

Per spec §7D and §7F:

- one codec packet per RTP packet, directly after a plain 12-byte RTP header; no extra in-band header
- `80 60` (V=2, PT 96), sequence +1 per packet
- timestamp is constant: low 16 bits = samples per channel in the packet (`0x000002A0` for 14 ms at 48 kHz), high 16 bits = TTP adjust (0). A running clock makes the sink think every packet holds far too much audio.
- SSRC selects the latency mode and stays constant within a mode: `0xA3` HQ, `0xA1`/`0xA2` LL0/LL1, `0xA4` TWS. It changes exactly when the encoder switches between HQ and LL (§7H); we only send `0xA3` and `0xA1` (no sideband, so LL stays LL0, §7K).
- payload is the codec packet with each 16-bit word byte-swapped

## HQ and low latency

- **LL bitstream** (spec §7J, codec-core §10.D, §10.F). An R1 sink gets the HQ bitstream (config 0) on LL row 0: 265 kbps / 10 ms / 348-byte packets. An R2 sink gets config 4 on LL row 1: 270 kbps / 10 ms / 348 bytes. Both use timestamp `0x000001E0` and SSRC `0xA1`. Config 1 (low-power) is never given to a sink. Both are byte-exact through the stream loop against the parity vectors `c0_48k_ll`, `c0_44k_ll`, `c4_48k_i1_L3` and `c4_44k_i1_L3`.
- **Switching** is the codec property `SPA_PROP_quality` (0 = HQ, 1 = LL), LDAC style, plus the device setting `bluez5.a2dp.aptx-adaptive.mode = hq|ll` for the start. A runtime choice lives in the node's props, so it resets to the setting on reconnect.
- **Switch sequence** (§7K), with packets flowing throughout: a 4800-frame raised-cosine fade-out, 900 ms of silence, a 4800-frame fade-in. About 200 ms into the silence the encoder is reset in place (level 3, LL0, loop counters zeroed), and the packet size, period, timestamp and SSRC change from the next packet. Asking for the old mode mid-switch reverses it. Every stream also starts with a 20 ms mute and a 4800-frame fade-in (§7I). Curve: 0.5·(1 − cos(π(j+1)/4801)), truncated toward zero.
- **Block size.** `media-sink.c` reads `get_block_size` only at transport start and never calls `get_interval` for A2DP; it times packets from blocks × block size. So the block is the gcd of the HQ and LL packet periods, and media-sink's timing stays right across a switch: 2 ms (96 frames) for an R1 sink (14 and 10 ms), 1 ms (48 frames) for an R2 sink (19 and 10 ms). 1 ms is the reference's own chunk. Encoding a block costs far less than its duration even at 1 ms, so the smaller R2 block is fine.
- **Reported delay.** `media-sink.c` calls `get_delay` only when the transport starts, so the encoder delay it reports doesn't follow a later HQ↔LL switch. R1 is unaffected (config 0 both ways, 90 frames). On an R2 sink it stays at its initial value: start in HQ and it reads 255 frames (config 3) while in LL, start in LL and it reads 0 (config 4's delay isn't measured) while in HQ.
- **Real-time safety.** `update_props` runs on the data thread. Both rows are worked out at init, the encoder's buffers are sized for the largest packet up front and reset in place, and the props are atomics. The switch path doesn't allocate, free, lock or log (tests check this with a counting allocator). Refused switches are counted and shown in the drift log line. After a refused switch, Props reports the profile actually running.
- **LL deficit** (§7K, codec-core §10.C): LL never carries a deficit; it's zeroed after every packet. The reference's extra flush there never produces anything, so it isn't made. The HQ 25 % bitrate drop clamps at 200 kbps (§7J).

## PipeWire integration without patching PipeWire

PipeWire 1.6 loads codec plugins from a fixed list of filenames and only offers codecs whose `id` is in `enum spa_bluetooth_audio_codec`. Neither has aptX Adaptive, so the plugin takes over the LC3plus HR plugin file and borrows an existing codec id:

- built as `libspa-codec-bluez5-lc3plus.so`, factory `api.codec.bluez5.media.lc3plus`, `name = "aptx_adaptive"`, description "aptX Adaptive". It shadows a real LC3plus plugin if one is installed.
- `id` is `SPA_BLUETOOTH_AUDIO_CODEC_LDAC` by default, or `SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR` with `APTX_ADAPTIVE_SLOT=lc3plus_hr` (set by `scripts/install-plugin.sh` in the drop-ins). See ADR 0002.
- installed to `~/.local/lib/spa-0.2/bluez5/`. Systemd user drop-ins set `SPA_PLUGIN_DIR=~/.local/lib/spa-0.2:<system plugindir>` for `wireplumber.service` (where the codec loader runs) and `pipewire.service`. The variable replaces the default path, so the system dir must be in it.
- **ranking:** `codec_order` in `codec-loader.c` decides which codec BlueZ configures on connect. In the default LDAC slot aptX Adaptive ranks above aptX HD, aptX and AAC, so it's configured directly and is the plain `a2dp-sink` profile. In the `lc3plus_hr` slot it ranks below them and never wins automatic selection: the higher-ranked codec is set up first, and switching to aptX Adaptive by hand costs a codec-change chime. Headsets that also have LDAC should use the `lc3plus_hr` slot (ADR 0002).
- if `monitor.bluez.properties` sets `bluez5.codecs`, it must include `"aptx_adaptive"`.

Later: send PipeWire a patch that adds a proper codec ID and loader entry, then drop the hijack.

## not supported yet

- LL hasn't been tried on hardware yet; R2 sinks in general are untested
- the R2.1 switch-to-LL silence of 800 blocks (§7K): R2 switches use 900, like R1
- the config 4 encoder delay isn't measured, so `get_delay` reports 0 in R2 LL
- 44.1 kHz and 96 kHz
- bitrate adaptation (`abr_process` only feeds the drift log)

No decoder and no speech/BLE profiles.
