# aptx-adaptive-for-linux

aptX Adaptive for Linux: a Rust encoder and PipeWire Bluetooth codec plugin.
It streams audio from a Linux desktop to aptX Adaptive headphones over stock
BlueZ and PipeWire, with no USB dongle and no external codec files. This
reverse-engineering project is made to support the Bose Quiet Comfort Ultra
(gen 1) and may not work directly with other headphones; some simple tweaks
may allow an AI agent to adapt it to yours.

**Clean-room notice:** this is an independent clean-room reimplementation. No
vendor source code, SDK, headers, NDA material, or vendor binary is included
or redistributed in this repository; all original code was written by the
project. See [`LEGAL.md`](LEGAL.md) for the full statement.

## what it can do

- **Two listening modes on one headset.** High-quality (HQ) and Low Latency
  (LL) appear as two card profiles. Choose which one connects by default at
  install time, and switch either way while playing without reconnecting.
- **Adaptive bitrate in HQ.** The encoder starts at level 3 (364 kbps) and
  steps up to 427 kbps when the link allows it, dropping back to 318 kbps
  when it doesn't. Changes apply from the next packet with no gap or fade.
- **44.1, 48 and 96 kHz** on R2 headsets: 96 kHz in HQ only, 44.1 kHz in HQ
  or LL. R1 headsets stay at 48 kHz. The default is 48 kHz.
- **Works alongside the system aptX plugin.** In the default slot it borrows
  PipeWire's aptX codec ids, gives aptX Adaptive headsets the new codec on
  connect, and passes aptX, aptX HD and aptX-LL through to everything else
  unchanged.
- **Other ranking slots** (`ldac`, `lc3plus_hr`) if you want aptX Adaptive to
  take a different position in the codec preference order.
- **Live mode switching** through the card profile or the node's `quality`
  property, with a short fade. WirePlumber remembers the profile per headset.
- **Self-check logging.** A drift line every 10 s, a log line for every
  bitrate change, and a WirePlumber guard that recovers a headset which
  configured aptX HD under an aptX Adaptive profile.
- **MIT licensed**, self-contained build.

## status

- **Works:** Bose Quiet Comfort Ultra (gen 1), 48 kHz stereo, HQ and low
  latency. HQ starts at 364 kbps and reached 427 kbps in a live test; LL runs
  at 265 kbps. Runtime HQ ↔ LL switching, LL on connect and card-profile
  switches have been tested.
- **Offline-tested:** R2 stereo at 44.1 kHz in HQ or LL, and 96 kHz in HQ.
  Playback on an R2 headset is unverified.
- **Untested:** congestion-driven bitrate reduction on hardware, and every
  other headset.

## requirements

Tested with PipeWire 1.6.9, BlueZ 5.87 and WirePlumber 0.5; the plugin
requires PipeWire 1.6.x. The headset must support aptX Adaptive. Building
needs `mise` (it provides Rust, Python and uv), `pkg-config` with the libspa
development files, and `python3`.

## install

```sh
mise install
mise run install-plugin
systemctl --user restart wireplumber pipewire
```

This builds the plugin, installs it under
`~/.local/lib/spa-0.2/bluez5/`, and adds systemd user drop-ins so PipeWire
and WirePlumber find it. In the default slot it also installs a small
WirePlumber guard script that recovers a headset which configures aptX HD
under an aptX Adaptive profile. Re-running it is safe; only one plugin file
is ever installed.

By default the plugin is installed as PipeWire's **aptX plugin** and loads the
system one behind it, so headsets without aptX Adaptive behave exactly as
before. The other install slots use the LC3plus HR plugin file instead:

| slot | what it does |
|---|---|
| `aptx` (default) | aptX Adaptive takes the aptX HD and aptX ids; aptX Adaptive headsets get it on connect, LDAC is untouched |
| `ldac` | aptX Adaptive takes LDAC's id, ranking above aptX HD |
| `lc3plus_hr` | aptX HD (or aptX/AAC) on connect; aptX Adaptive is picked by hand |

```sh
APTX_ADAPTIVE_SLOT=ldac mise run install-plugin         # or lc3plus_hr
systemctl --user restart wireplumber pipewire
```

## choosing the mode on connect

HQ is the default. To connect in low latency instead:

```sh
APTX_ADAPTIVE_DEFAULT_MODE=ll mise run install-plugin
systemctl --user restart wireplumber pipewire
```

Use `hq` to switch back. Both modes are always available as card profiles.
With default `hq` the plain `a2dp-sink` profile is HQ and the low-latency
profile is `a2dp-sink-aptx_adaptive_ll`; with default `ll` it is the other
way round.

## switching while playing

Pick the other card profile in GNOME/KDE settings, `pavucontrol` or
`pwvucontrol`. The stream reconfigures, so expect a short gap and a headset
chime. To switch without reconfiguring, set the node's `quality` property:

```sh
id=$(pw-cli ls Node | awk '/^\tid /{id=$2} /node.name = "bluez_output\./{print id+0; exit}')
pw-cli set-param "$id" Props '{ quality = 1 }'    # low latency
pw-cli set-param "$id" Props '{ quality = 0 }'    # HQ
```

The change survives until the headset disconnects; switch the default mode
if you want it on every connection.

## choosing a sample rate

The default is 48 kHz. To prefer another rate on an R2 headset, put this in
`~/.config/wireplumber/wireplumber.conf.d/aptx-adaptive-rate.conf`, restart
WirePlumber and reconnect:

```conf
monitor.bluez.properties = {
	bluez5.default.rate = 96000
}
```

Use `44100` or `48000` instead. An unavailable preference falls back to a
supported rate, preferring 48 kHz. This is a codec preference, not the
PipeWire graph rate, and it does not increase the resolution of the source
audio.

## checking it is working

While audio plays, the plugin logs a drift line every 10 s:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
```

Every bitrate change is logged once, too:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: level'
```

## if the headset switches to the call profile

That is WirePlumber's headset-profile autoswitch, not this plugin. With
`bluetooth.autoswitch-to-headset-profile` on (the default), recording from
the headset microphone moves it to HFP. To keep it on A2DP:

```sh
wpctl settings --save bluetooth.autoswitch-to-headset-profile false
```

## uninstall

```sh
scripts/install-plugin.sh --uninstall
systemctl --user restart wireplumber pipewire
```

This removes the plugin, the WirePlumber guard script and configuration, and
the systemd drop-ins. It is safe to run twice.

## legal

This repository is an independent, clean-room reimplementation of aptX
Adaptive and its A2DP transport for Linux. It contains no vendor source code,
SDK, headers, confidential documents or vendor binaries; all original code and
documentation was written by the project, and reconstructed numeric data is
included only for interoperability. The full legal notes — including sources
of information, jurisdiction and limitation of liability — are in
[`LEGAL.md`](LEGAL.md).

Trademarks:

- `aptX`, `aptX Adaptive`, `aptX HD`, `aptX Low Latency` and related marks are
  trademarks of Qualcomm Incorporated.
- Bluetooth and the Bluetooth word mark are trademarks of the Bluetooth SIG,
  Inc.
- PipeWire, WirePlumber, BlueZ and Linux are trademarks or registered
  trademarks of their respective owners.
- All other trademarks are the property of their respective owners.

This project is not affiliated with, sponsored by or endorsed by Qualcomm or
the Bluetooth SIG.

All original code and documentation is MIT-licensed; see
[`LICENSE`](LICENSE).
