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

By default the plugin borrows PipeWire's LDAC codec id, which ranks above aptX HD, aptX and AAC. A headset with aptX Adaptive gets it as soon as it connects, as the plain `a2dp-sink` profile (HQ, or low latency, see below); aptX HD is still there as `a2dp-sink-aptx_hd`. There's no `a2dp-sink-aptx_adaptive` profile in this slot, so if WirePlumber saved that one earlier it falls back to `a2dp-sink`. See [ADR 0002](docs/adr/0002-codec-autoselect.md).

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

HQ and low latency are two card profiles, so you can pick either anywhere profiles are listed:

- **KDE:** System Settings → Sound, the "Profile:" box under the headset; or the tray's volume applet, right-click the headset → Profiles.
- **pavucontrol:** Configuration tab, the headset's Profile list. The Codec list next to it shows both too.
- **command line:** `pactl set-card-profile <card> <profile>`

Which one a headset gets **on connect** is set at install time:

```sh
APTX_ADAPTIVE_DEFAULT_MODE=ll mise run install-plugin   # low latency on connect
APTX_ADAPTIVE_DEFAULT_MODE=hq mise run install-plugin   # HQ on connect (the default)
systemctl --user restart wireplumber pipewire
```

The profile names depend on that choice, because PipeWire calls the connect-time profile plain `a2dp-sink`:

| | HQ, shown as "…codec aptX Adaptive)" | low latency, shown as "…codec aptX Adaptive (Low Latency))" |
|---|---|---|
| default mode `hq` | `a2dp-sink` | `a2dp-sink-aptx_adaptive_ll` |
| default mode `ll` | `a2dp-sink-aptx_adaptive` | `a2dp-sink` |
| `lc3plus_hr` slot, either mode | `a2dp-sink-aptx_adaptive` | `a2dp-sink-aptx_adaptive_ll` |

The full labels are "High Fidelity Playback (A2DP Sink, codec aptX Adaptive)" and "High Fidelity Playback (A2DP Sink, codec aptX Adaptive (Low Latency))".

With the default mode's profile there's a single Bluetooth setup on connect, and the label matches what's running. In the `lc3plus_hr` slot, aptX HD is set up on connect whatever the mode (see above).

WirePlumber remembers the profile you pick per headset and restores it on the next connect. Switching profile reconfigures the Bluetooth stream: there's a gap of a second or two, and the Px7 S2e plays its chime. So if you usually want the other mode, change the default mode instead of switching every time. `pw-dump` shows the non-default profile's codec id as `mpeg`; that's only a borrowed id, see [ADR 0003](docs/adr/0003-ll-profile.md).

The default mode is a systemd drop-in environment variable, not a WirePlumber setting. PipeWire binds a fresh connection to whichever codec entry owns the Bluetooth endpoint. That's fixed when the plugin loads, and nothing in the WirePlumber or PipeWire config reaches it (details in ADR 0003).

To switch without the reconfigure, set the codec's `quality` property on the Bluetooth output node (0 = HQ, 1 = low latency), the same way LDAC's quality is set. This works in either profile:

```sh
id=$(pw-cli ls Node | awk '/^\tid /{id=$2} /node.name = "bluez_output\./{print id+0; exit}')
pw-cli enum-params "$id" PropInfo                 # "aptX Adaptive mode": 0 "HQ", 1 "Low latency"
pw-cli set-param "$id" Props '{ quality = 1 }'    # low latency
pw-cli set-param "$id" Props '{ quality = 0 }'    # HQ
pw-cli enum-params "$id" Props                    # what's actually running
```

You can also find the `bluez_output.…` node id with `wpctl status`. The audio fades out, goes silent for just under a second, and fades back in. The headset's latency mode changes during the silence.

**A `quality` change doesn't survive a reconnect.** The older way to start in low latency, a WirePlumber setting, still works with default mode `hq`, but the profile is still labelled plain "aptX Adaptive" while it runs low latency. Prefer `APTX_ADAPTIVE_DEFAULT_MODE=ll`. With default mode `ll`, or in the non-default profile, the setting is ignored. For reference, the drop-in goes in e.g. `~/.config/wireplumber/wireplumber.conf.d/aptx-adaptive.conf`, then run `systemctl --user restart wireplumber`:

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

To silence it, set `APTX_ADAPTIVE_DRIFT_LOG=0` in WirePlumber's service environment. The line includes the current quality level (`level 3`). If a mode switch was refused (e.g. low latency when the link's MTU is too small for it), the line ends with `refused mode switches N`.

## adaptive bitrate

In HQ mode the plugin changes the quality level as the link allows. When PipeWire can't send a packet because the socket is full, it asks the codec to back off (at most every ½ s), and the plugin drops one level. After a second with nothing queued, PipeWire asks it to go up again, and the plugin goes up one level. On a Px7 S2e (R1) the levels are 2 (318 kbps, 16 ms), 3 (364 kbps, 14 ms, the default) and 4 (427 kbps, 12 ms). The change applies from the next packet with no gap or fade. Switching between HQ and low latency puts the level back to 3. Low latency on an R1 headset has one bitrate at every level, so it never changes.

Each change is logged once:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: level'
# aptx-adaptive: level 3 -> 2 (364k -> 318k)
```

To confirm the headset's latency target is right, run `sudo btmon` and start playback. The headset's AVDTP Delay Report after Start should read 200.0 ms. If it reads 0 ms, expect stutter after a minute or two.

## uninstall

```sh
scripts/install-plugin.sh --uninstall
systemctl --user restart wireplumber pipewire
```

This removes the plugin and the drop-ins. It's safe to run twice. Directories the install created (`~/.local/lib/spa-0.2/…`, `${XDG_CONFIG_HOME:-~/.config}/systemd/user/….d`) are removed only if empty, so `~/.local/lib` and `${XDG_CONFIG_HOME:-~/.config}/systemd/user` themselves are left in place.

## clean room

The codec is written only from prose specs and black-box test vectors in `spec/` and `tests/vectors/`. Nobody working in this repo reads the vendor binaries or the reverse-engineering notes they came from. See [AGENTS.md](AGENTS.md).
