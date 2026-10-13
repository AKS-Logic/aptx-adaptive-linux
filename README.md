# aptx-linux

aptX Adaptive for Linux: a clean-room Rust encoder plus a PipeWire Bluetooth codec plugin. It plays audio from your computer straight to an aptX Adaptive headset, with stock BlueZ and PipeWire and no USB dongle.

## status

- **Works:** Bowers & Wilkins Px7 S2e, high-quality mode, 48 kHz stereo (364 kbps).
- **Untested:** every other headset.
- **Not yet:** low-latency mode, 44.1 or 96 kHz.

Needs PipeWire 1.6.x. Design notes are in [docs/adr/](docs/adr/).

## install

You need `mise`, `pkg-config` with the libspa development headers, and `python3`.

```sh
mise install
mise run install-plugin
systemctl --user restart wireplumber pipewire
```

This builds the plugin, installs it to `~/.local/lib/spa-0.2/bluez5/`, and adds systemd user drop-ins so WirePlumber and PipeWire find it. Re-running it is safe.

The plugin is installed as PipeWire's LC3plus HR plugin, so it replaces a real LC3plus plugin if you have one.

## codec selection

By default the plugin borrows PipeWire's LDAC codec id, which ranks above aptX HD, aptX and AAC. A headset with aptX Adaptive gets it as soon as it connects, as the plain `a2dp-sink` profile; aptX HD is still there as `a2dp-sink-aptx_hd`. See [ADR 0002](docs/adr/0002-codec-autoselect.md).

**If your headset also supports LDAC**, the two share that id, and which one you get can change between restarts. Install in the LC3plus HR slot instead:

```sh
APTX_ADAPTIVE_SLOT=lc3plus_hr mise run install-plugin
systemctl --user restart wireplumber pipewire
```

In that slot aptX Adaptive ranks below aptX HD, aptX and AAC, so pick it after connecting (the headset chimes when the codec changes):

```sh
pactl list cards short          # find the bluez_card.… name
pactl set-card-profile <card> a2dp-sink-aptx_adaptive
```

If your WirePlumber config sets `bluez5.codecs`, add `"aptx_adaptive"` to it, or the codec won't show up.

## check it's working

While audio plays, the plugin logs a line every 10 s:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
```

To silence it, set `APTX_ADAPTIVE_DRIFT_LOG=0` in WirePlumber's service environment.

To confirm the headset's latency target is right, run `sudo btmon` and start playback. The headset's AVDTP Delay Report after Start should read 200.0 ms. If it reads 0 ms, expect stutter after a minute or two.

## uninstall

```sh
scripts/install-plugin.sh --uninstall
systemctl --user restart wireplumber pipewire
```

This removes the plugin and the drop-ins. It's safe to run twice.

## clean room

The codec is written only from prose specs and black-box test vectors in `spec/` and `tests/vectors/`. Nobody working in this repo reads the vendor binaries or the reverse-engineering notes they came from. See [AGENTS.md](AGENTS.md).
