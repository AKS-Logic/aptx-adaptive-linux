# aptx-linux

aptX Adaptive for Linux: a clean-room Rust encoder plus a PipeWire Bluetooth codec plugin. It plays audio from your computer straight to an aptX Adaptive headset, with stock BlueZ and PipeWire and no USB dongle.

**Clean-room notice:** this is an independent reverse-engineering project.
No vendor source code, SDK, NDA material, or vendor binary was received, used,
or copied. All code was written by the project and all constants were
independently reconstructed from reverse engineering and black-box measurement
— see [`LEGAL.md`](LEGAL.md).

## status

- **Works:** Bowers & Wilkins Px7 S2e, 48 kHz stereo, HQ and low latency. HQ starts at 364 kbps and reached 427 kbps through adaptive bitrate in a live test. LL runs at 265 kbps. Runtime HQ → LL → HQ switching, LL on connect, and card-profile switches have been tested ([test status](spec/QUESTIONS.md#current-status-2026-10-14)).
- **Untested:** congestion-driven bitrate reduction on hardware, and every other headset, including R2 sinks.
- **Offline-tested:** R2 stereo at 44.1 kHz in HQ or LL, and 96 kHz in HQ. Reference packets and compiled plugin callbacks pass; playback on an R2 headset remains unverified. R1 headsets, including the Px7, stay at 48 kHz.

Needs PipeWire 1.6.x. Design notes are in [docs/adr/](docs/adr/).

## install

You need `mise` (it provides Rust, Python and uv: run `mise install`) and
`pkg-config` with the libspa development headers. **No vendor library is needed
to build:** the reconstructed codec tables are included as Rust source in
`crates/aptx-adaptive/bundled_tables.rs` (see [`LEGAL.md`](LEGAL.md)).

Optionally you can regenerate and hash-verify those tables from your own copy of
the vendor encoder library. Set `APTX_ADAPTIVE_SO` to an absolute path of
`libaptXAdaptiveEnc.so` and the build extracts from it instead, failing unless
every table matches [spec/table-extraction.md](spec/table-extraction.md) §5.
To get such a copy, from a Snapdragon phone with aptX Adaptive over `adb`:

```sh
mkdir -p .scratch
adb pull /vendor/dsp/adsp/libaptXAdaptiveEnc.so .scratch/
```

Or take `adsp/libaptXAdaptiveEnc.so` out of the `dsp` partition image in the phone's full OTA (`payload.bin`). Only `libaptXAdaptiveEnc.so` works; `libaptXAdaptiveEnc3.so` and `…Enc4.so` don't. Point `APTX_ADAPTIVE_SO` at the absolute path of the copy you want to verify against.

```sh
mise install
mise run install-plugin
systemctl --user restart wireplumber pipewire
```

This builds the plugin, installs it to `~/.local/lib/spa-0.2/bluez5/`, and adds systemd user drop-ins so WirePlumber and PipeWire find it. In the default slot it also installs a WirePlumber script, `~/.local/share/wireplumber/scripts/aptx-linux/aptx-adaptive-guard.lua`, enabled by `~/.config/wireplumber/wireplumber.conf.d/90-aptx-adaptive-guard.conf` (see below). Re-running it is safe.

By default the plugin is installed as PipeWire's **aptX plugin** (`libspa-codec-bluez5-aptx.so`). It loads the real system aptX plugin and passes aptX, aptX HD and aptX-LL through unchanged, so headsets without aptX Adaptive work exactly as before. The other install slots use the LC3plus HR plugin file instead (see below). Only one file is ever installed; switching slots removes the other.

If the real aptX plugin can't be loaded (not installed, or from another PipeWire version), aptX and aptX HD are unavailable while ours is installed. WirePlumber's log says why:

```sh
journalctl --user -u wireplumber | grep 'aptx-adaptive: aptX'
# aptx-adaptive: aptX passthrough: 6 codecs from /usr/lib/spa-0.2/bluez5/libspa-codec-bluez5-aptx.so
```

## codec selection

The install slot (`APTX_ADAPTIVE_SLOT`) decides which PipeWire codec ids aptX Adaptive borrows, and so where it ranks:

| slot | plugin file | aptX Adaptive on connect | notes |
|---|---|---|---|
| `aptx` (default) | aptX | yes, unless the headset has LDAC | replaces aptX HD and aptX on aptX Adaptive headsets only ([ADR 0004](docs/adr/0004-aptx-slot.md)) |
| `ldac` | LC3plus HR | yes, unless the headset also has LDAC | shares LDAC's id; on headsets with both codecs, the selected codec can change between restarts ([ADR 0002](docs/adr/0002-codec-autoselect.md)) |
| `lc3plus_hr` | LC3plus HR | no, pick it by hand | below aptX HD, aptX and AAC |

**`aptx` slot (default).** aptX Adaptive takes PipeWire's aptX HD and aptX codec ids. PipeWire makes one profile per id, using the codecs the headset supports, so:

- On a headset with aptX Adaptive, the aptX HD and aptX profiles become aptX Adaptive (HQ and low latency). The separate aptX HD and aptX profiles disappear; that's intended. aptX-LL, AAC and SBC are unchanged.
- On a headset without aptX Adaptive, nothing changes: aptX HD and aptX are the real ones.
- LDAC is untouched. LDAC ranks above aptX HD, so **a headset with both LDAC and aptX Adaptive connects on LDAC**. Pick aptX Adaptive by hand there (`a2dp-sink-aptx_adaptive`, or `…_ll`).
- `pw-dump` shows the codec ids as `aptx_hd` (aptX Adaptive's connect profile) and `aptx` (the other mode). These are only borrowed ids. `api.bluez5.codec` and the profile names say `aptx_adaptive`.
- Some headsets set up the codec themselves and may pick aptX HD. PipeWire then labels the card with the aptX Adaptive profile while aptX HD actually runs. This slot installs a small WirePlumber script that notices this and switches to aptX Adaptive once per connection. You may hear one codec-change chime. Check its log line:

  ```sh
  journalctl --user -u wireplumber | grep 're-selecting it via'
  ```

  To fix it by hand, pick the other aptX Adaptive profile, then the one you want. Picking the same profile again does nothing. This hasn't been tested on such a headset yet ([ADR 0004](docs/adr/0004-aptx-slot.md), "risks").

Other slots:

```sh
APTX_ADAPTIVE_SLOT=ldac mise run install-plugin         # the old default
APTX_ADAPTIVE_SLOT=lc3plus_hr mise run install-plugin   # aptX HD on connect
systemctl --user restart wireplumber pipewire
```

In the `lc3plus_hr` slot aptX Adaptive ranks below aptX HD, aptX and AAC, so pick it after connecting (the headset chimes when the codec changes):

```sh
pactl list cards short          # find the bluez_card.… name
pactl set-card-profile <card> a2dp-sink-aptx_adaptive
```

If your WirePlumber config sets `bluez5.codecs`, add `"aptx_adaptive"` and `"aptx_adaptive_ll"` to it, or the codec and its low-latency profile won't show up.

## sample rates

The default is 48 kHz. To prefer 44.1 or 96 kHz on a compatible R2 headset, add a WirePlumber fragment such as `~/.config/wireplumber/wireplumber.conf.d/aptx-adaptive-rate.conf`:

```conf
monitor.bluez.properties = {
	bluez5.default.rate = 96000
}
```

Use `44100` for 44.1 kHz or `48000` to restore the default. Restart WirePlumber and reconnect the headset:

```sh
systemctl --user restart wireplumber
```

This is a preference for all Bluetooth codecs, not a PipeWire graph-rate setting. An unavailable preference falls back to a supported rate, preferring 48 kHz. It does not increase the resolution of source audio.

R1 headsets remain at 48 kHz. At 96 kHz, LL is unavailable and an initial LL request falls back to HQ. The node's `quality` property reports HQ, although the selected card profile can still have the LL label. Choose 44.1 or 48 kHz for LL. Sinks advertising the R3 decoder feature cannot negotiate 44.1 kHz until that bitstream is specified.

The source's advertised capabilities remain the compatible 48 kHz R1 layout. Other rates use host selection against the remote R2 capabilities. R2.2 compatibility and fractional-rate playback timing remain open questions in [the sample-rate design](docs/adr/0005-sample-rates.md).

## HQ or low latency

HQ and low latency are two card profiles. Profile selection has been tested on the Px7 S2e. These tools expose the profiles:

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
| `aptx` or `ldac` slot, default mode `hq` | `a2dp-sink` | `a2dp-sink-aptx_adaptive_ll` |
| `aptx` or `ldac` slot, default mode `ll` | `a2dp-sink-aptx_adaptive` | `a2dp-sink` |
| `aptx` slot, headset also has LDAC | `a2dp-sink-aptx_adaptive` | `a2dp-sink-aptx_adaptive_ll` |
| `lc3plus_hr` slot, either mode | `a2dp-sink-aptx_adaptive` | `a2dp-sink-aptx_adaptive_ll` |

The full labels are "High Fidelity Playback (A2DP Sink, codec aptX Adaptive)" and "High Fidelity Playback (A2DP Sink, codec aptX Adaptive (Low Latency))".

With the default mode's profile there's a single Bluetooth setup on connect, and the label matches what's running. In the `lc3plus_hr` slot aptX HD is set up on connect whatever the mode, and in the `aptx` slot a headset with LDAC gets LDAC (see above).

WirePlumber remembers the profile you pick per headset and restores it on the next connect. Switching profile reconfigures the Bluetooth stream: there's a gap of a second or two, and the Px7 S2e plays its chime. So if you usually want the other mode, change the default mode instead of switching every time. `pw-dump` shows the non-default profile's codec id as `aptx` (`mpeg` in the `ldac` and `lc3plus_hr` slots); that's only a borrowed id, see [ADR 0003](docs/adr/0003-ll-profile.md) and [ADR 0004](docs/adr/0004-aptx-slot.md).

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

Low latency has been hardware-tested on the Px7 S2e (aptX Adaptive R1) at 265 kbps. The R2 path starts at 270 kbps and is covered by vectors, not by a real headset test. GUIs don't show per-codec properties for any codec. The `quality` switch is `pw-cli` only.

## check it's working

While audio plays, the plugin logs a line every 10 s:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
```

To silence it, set `APTX_ADAPTIVE_DRIFT_LOG=0` in WirePlumber's service environment. The line includes the current quality level (`level 3`). If a mode switch was refused (e.g. low latency when the link's MTU is too small for it), the line ends with `refused mode switches N`.

## adaptive bitrate

In HQ mode the plugin changes the quality level as the link allows. When PipeWire can't send a packet because the socket is full, it asks the codec to back off (at most every ½ s), and the plugin drops one level. After a second with nothing queued, PipeWire asks it to go up again, and the plugin goes up one level. On a Px7 S2e (R1) the levels are 2 (318 kbps, 16 ms), 3 (364 kbps, 14 ms, the default) and 4 (427 kbps, 12 ms). The change applies from the next packet with no gap or fade. Switching between HQ and low latency puts the level back to 3. Low latency on an R1 headset has one bitrate at every level, so it never changes.

In the [October 13 live ABR test](spec/QUESTIONS.md#adaptive-bitrate-live-2026-10-13-1314-after-25), PipeWire raised HQ from level 3 to 4 after one second. It stayed at 427 kbps for minutes without backlog growth. Walking around did not trigger a reduction, so reductions remain covered by tests and vectors only.

Each change is logged once:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: level'
# aptx-adaptive: level 3 -> 2 (364k -> 318k)
```

If the headset offers Delay Reporting, run `sudo btmon` and start playback. The Px7's target is 200.0 ms in HQ and 80.0 ms in LL. Both were confirmed with a test bluetoothd advertising A2DP 1.3. With stock BlueZ 5.87 advertising A2DP 1.4, the Px7 omits Delay Reporting, but audio works in both modes. A 0 ms HQ target can cause stutter after a minute or two.

## headset keeps switching to the call profile

If the headset drops from aptX Adaptive to "Headset Head Unit" (HFP, 16 kHz mono), that's WirePlumber, not this plugin. With `bluetooth.autoswitch-to-headset-profile` on (the default), every headset's microphone is always listed as an input. As soon as any app records from it (a browser, a call app, or a fallback when your usual mic is unplugged), WirePlumber switches the headset to HFP, and it switches back a few seconds after recording stops.

WirePlumber 0.5 only has this as a global setting; there's no per-device switch. To turn it off and keep the change:

```sh
wpctl settings --save bluetooth.autoswitch-to-headset-profile false
```

The headset microphone then only appears when you pick the "Headset Head Unit" profile by hand, e.g. for a call. Turn it back on with `true`.

## uninstall

```sh
scripts/install-plugin.sh --uninstall
systemctl --user restart wireplumber pipewire
```

This removes the plugin (under either file name), the WirePlumber script and its config fragment, and the drop-ins. It's safe to run twice. Directories the install created (`~/.local/lib/spa-0.2/…`, `${XDG_CONFIG_HOME:-~/.config}/systemd/user/….d`, `…/wireplumber/…`) are removed only if empty, so `~/.local/lib` and `${XDG_CONFIG_HOME:-~/.config}/systemd/user` themselves are left in place.

## clean room

The codec is written only from prose specs and black-box test vectors in `spec/` and `tests/vectors/`. Nobody working in this repo reads the vendor binaries or the reverse-engineering notes they came from. The reconstructed numeric tables are included as Rust data in `crates/aptx-adaptive/bundled_tables.rs`; an optional build-time extractor can regenerate and hash-verify them from a vendor library copy following [spec/table-extraction.md](spec/table-extraction.md). See [`LEGAL.md`](LEGAL.md), [AGENTS.md](AGENTS.md) and [ADR 0001](docs/adr/0001-architecture.md).
