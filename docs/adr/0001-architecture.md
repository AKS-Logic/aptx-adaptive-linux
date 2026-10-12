# 0001 - architecture

## goal

Play audio from Linux straight to an aptX Adaptive headset (target: Bowers & Wilkins PX7 S2e) over A2DP, without a USB dongle.

## clean room

The codec is built from written specs only. The reverse engineering happens in a separate repo, `aptx-re-quarantine`, which is never read by anyone implementing here. What crosses over:

- prose specs (`specs/*.md`) and numeric tables (`specs/tables/*`)
- black-box test vectors (PCM in, bitstream out)

These get copied into `spec/` and `tests/vectors/` in this repo. Implementers must not open the quarantine repo, decompiled output or the vendor binaries, or run IDA.

## layout

- `crates/aptx-adaptive` - pure Rust encoder, no I/O and no unsafe. Takes PCM, returns A2DP media payloads.
- `crates/spa-codec-aptx-adaptive` - `cdylib` that implements the PipeWire bluez5 media codec plugin ABI (`SPA_VERSION_BLUEZ5_CODEC_MEDIA` 16, PipeWire 1.6.x) and wraps the encoder.

## pipewire integration without patching pipewire

PipeWire 1.6 has two hardcoded limits:

- it loads codec plugins from a fixed list of filenames (`bluez5/libspa-codec-bluez5-<name>.so`)
- it only offers codecs whose `id` is in `enum spa_bluetooth_audio_codec`

Neither list has aptX Adaptive. So for now:

- the plugin is built as `libspa-codec-bluez5-lc3plus.so` with factory name `api.codec.bluez5.media.lc3plus`. LC3plus HR isn't installed on the target machine, so the slot is free.
- the codec uses `id = SPA_BLUETOOTH_AUDIO_CODEC_LC3PLUS_HR`, `name = "aptx_adaptive"` and description "aptX Adaptive"
- it's installed to `~/.local/lib/spa-0.2/bluez5/`. Systemd user drop-ins set `SPA_PLUGIN_DIR=~/.local/lib/spa-0.2:<system plugindir from pkg-config>` for `wireplumber.service`, where the bluez5 monitor and codec loader run, and for `pipewire.service`. PipeWire walks `SPA_PLUGIN_DIR` as a colon-separated list, and the variable replaces the default path, so the system dir must be in it.
- `codec_order` in `codec-loader.c` ranks `LC3PLUS_HR` below aptX HD, aptX and AAC, so it never wins automatic selection. Pick the aptX Adaptive profile by hand, or set a `bluez5.codecs` list that leaves out the codecs ranked above it.
- if WirePlumber's `monitor.bluez.properties` sets `bluez5.codecs`, the list must include `"aptx_adaptive"`, or the codec is never registered

Later: send PipeWire a patch upstream that adds a proper codec ID and loader entry, then drop the hijack.

## scope

1. A2DP source, aptX Adaptive R2.x (whatever the PX7 S2e advertises), 48 kHz stereo, HQ mode
2. low-latency mode and bitrate adaptation (`abr_process`)
3. 96 kHz, and R3/R4 if the headset supports them

No decoder and no speech/BLE profiles.
