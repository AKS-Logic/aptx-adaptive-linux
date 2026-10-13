# aptx-linux

aptX Adaptive for Linux: a clean-room Rust encoder plus a PipeWire Bluetooth codec plugin. It plays audio from your computer straight to an aptX Adaptive headset, with stock BlueZ and PipeWire and no USB dongle.

## status

- **Works:** Bowers & Wilkins Px7 S2e, high-quality mode, 48 kHz stereo (364 kbps).
- **Untested:** low-latency mode on real hardware, including picking the low-latency profile in KDE/pavucontrol; every other headset.
- **Not yet:** 44.1 or 96 kHz.

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

By default the plugin borrows PipeWire's LDAC codec id, which ranks above aptX HD, aptX and AAC. A headset with aptX Adaptive gets it as soon as it connects, as the plain `a2dp-sink` profile; aptX HD is still there as `a2dp-sink-aptx_hd`. There's no `a2dp-sink-aptx_adaptive` profile in this slot, so if WirePlumber saved that one earlier it falls back to `a2dp-sink`. See [ADR 0002](docs/adr/0002-codec-autoselect.md).

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

If your WirePlumber config sets `bluez5.codecs`, add `"aptx_adaptive"` and `"aptx_adaptive_ll"` to it, or the codec and its low-latency profile won't show up.

## HQ or low latency

The default is HQ. Low latency is a separate card profile, so you can pick it anywhere profiles are listed:

| profile (`pactl list cards`) | shown as |
|---|---|
| `a2dp-sink` (`a2dp-sink-aptx_adaptive` in the `lc3plus_hr` slot) | High Fidelity Playback (A2DP Sink, codec aptX Adaptive) |
| `a2dp-sink-aptx_adaptive_ll` | High Fidelity Playback (A2DP Sink, codec aptX Adaptive (Low Latency)) |

- **KDE:** System Settings → Sound, the "Profile:" box under the headset; or the tray's volume applet, right-click the headset → Profiles.
- **pavucontrol:** Configuration tab, the headset's Profile list. The Codec list next to it shows "aptX Adaptive (Low Latency)" too.
- **command line:** `pactl set-card-profile <card> a2dp-sink-aptx_adaptive_ll`

WirePlumber remembers the profile per headset. Switching profile reconfigures the Bluetooth stream, so there's a gap of a second or two, and the Px7 S2e plays its chime. On connect the headset always starts in the main profile, so a saved low-latency profile costs one extra switch (and chime) each time. To start in low latency without that, use the `bluez5.a2dp.aptx-adaptive.mode` setting below instead. `pw-dump` shows the low-latency profile's codec id as `mpeg`; that's only a borrowed id, see [ADR 0003](docs/adr/0003-ll-profile.md).

To switch without the reconfigure, set the codec's `quality` property on the Bluetooth output node (0 = HQ, 1 = low latency), the same way LDAC's quality is set. This works in either profile:

```sh
id=$(pw-cli ls Node | awk '/^\tid /{id=$2} /node.name = "bluez_output\./{print id+0; exit}')
pw-cli enum-params "$id" PropInfo                 # "aptX Adaptive mode": 0 "HQ", 1 "Low latency"
pw-cli set-param "$id" Props '{ quality = 1 }'    # low latency
pw-cli set-param "$id" Props '{ quality = 0 }'    # HQ
pw-cli enum-params "$id" Props                    # what's actually running
```

You can also find the `bluez_output.…` node id with `wpctl status`. The audio fades out, goes silent for just under a second, and fades back in. The headset's latency mode changes during the silence.

**A `quality` change doesn't survive a reconnect.** To start the main profile in low latency every time, add a WirePlumber drop-in, e.g. `~/.config/wireplumber/wireplumber.conf.d/aptx-adaptive.conf`, then run `systemctl --user restart wireplumber`:

```
monitor.bluez.rules = [
  {
    matches = [ { device.name = "~bluez_card." } ]
    actions = { update-props = { bluez5.a2dp.aptx-adaptive.mode = "ll" } }
  }
]
```

Low latency works on both headset generations: the Px7 S2e (aptX Adaptive R1) gets 265 kbps, and newer (R2) headsets get the R2 low-latency codec at 270 kbps. GUIs don't show per-codec properties for any codec; the `quality` switch is `pw-cli` only.

## check it's working

While audio plays, the plugin logs a line every 10 s:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
```

To silence it, set `APTX_ADAPTIVE_DRIFT_LOG=0` in WirePlumber's service environment. If a mode switch was refused (e.g. low latency when the link's MTU is too small for it), the line ends with `refused mode switches N`.

To confirm the headset's latency target is right, run `sudo btmon` and start playback. The headset's AVDTP Delay Report after Start should read 200.0 ms. If it reads 0 ms, expect stutter after a minute or two.

## uninstall

```sh
scripts/install-plugin.sh --uninstall
systemctl --user restart wireplumber pipewire
```

This removes the plugin and the drop-ins. It's safe to run twice. Directories the install created (`~/.local/lib/spa-0.2/…`, `${XDG_CONFIG_HOME:-~/.config}/systemd/user/….d`) are removed only if empty, so `~/.local/lib` and `${XDG_CONFIG_HOME:-~/.config}/systemd/user` themselves are left in place.

## clean room

The codec is written only from prose specs and black-box test vectors in `spec/` and `tests/vectors/`. Nobody working in this repo reads the vendor binaries or the reverse-engineering notes they came from. See [AGENTS.md](AGENTS.md).
