# aptx-adaptive-for-linux

This is a research project which shows that AI is capable of integrating audio
technologies and demonstrates the AI capability in clean room implementation of 
Bluetooth codecs.

aptX Adaptive for Linux contains a Rust encoder and PipeWire Bluetooth codec plugin.
It streams audio from a Linux desktop to aptX Adaptive headphones over stock
BlueZ and PipeWire, with no USB dongle and no external codec files. 
This project was tested with Bose Quiet Comfort Ultra (gen 1).
It may or may not work directly with other headphones; some simple tweaks
made by AI agent may allow it to work with other headphones and machines,
demonstrating AI's capability even more.

**Clean-room notice:** this is an independent clean-room reimplementation. No
vendor source code, SDK, headers, NDA material, or vendor binary is included
or redistributed in this repository; all original code was written by the
project. See [`LEGAL.md`](LEGAL.md) for the full statement.

## what it can do

**Three listening modes on one headset.** HQ, adaptive and low latency appear as three
card profiles, and you can switch between them while playing:

| card profile | mode | what it optimises |
|---|---|---|
| `a2dp-sink-aptx_adaptive_hq` | **HQ** | quality: pinned at the top bitrate row, 427 kbps / 12 ms |
| `a2dp-sink-aptx_adaptive` | **adaptive** | scales: starts at 204 kbps and climbs to 427 kbps as the link allows |
| `a2dp-sink` | **LL** | latency: 10 ms frames, 348-byte payload, ~270 kbps |

The plain `a2dp-sink` profile is the one a fresh connection uses, and with this layout it is
the LL profile. Which name gets the plain form is assigned by PipeWire rather than by this
plugin, so check `pactl list cards` before scripting a profile switch. WirePlumber remembers
the last profile you picked for a headset.

HQ never steps its ladder — it holds level 5 and accepts the extra latency. Adaptive starts
at the bottom row and steps up when the connection allows, dropping back when it does not;
every change applies from the next packet. LL is the lowest latency this headset can reach:
the table also holds 8 ms / 384-sample rows, but they need a sink that advertises
*enhanced LL*, which the Bose does not.

**Switching.** Pick another card profile in GNOME/KDE settings, pavucontrol or pwvucontrol.
On the tested headset the link stays up across all three; A2DP still renegotiates, so a
short chime is normal. For a change that does not renegotiate at all, use the in-band §7K
path — the node's quality property, or the optional control file described below.

**Sample rates on R2 headsets:** 44.1, 48 and 96 kHz; 96 kHz in HQ only, 44.1 kHz in HQ or
LL. R1 headsets stay at 48 kHz, which is the default.

**Works alongside the system aptX plugin.** In the default slot it borrows PipeWire's aptX
codec ids, gives aptX Adaptive headsets the new codec on connect, and passes aptX, aptX HD
and aptX-LL through to everything else unchanged.

**Other ranking slots** (`ldac`, `lc3plus_hr`) if you want aptX Adaptive to take a different
position in the codec preference order.

**Self-check logging.** The negotiated ladder row is logged once per stream and every bitrate
change gets one line; an optional drift line every 10 s is available with
`APTX_ADAPTIVE_DRIFT_LOG=1`. There is also a WirePlumber guard that recovers a headset which
configured aptX HD under an aptX Adaptive profile. Nothing writes log files: the lines go to
stderr and land in the systemd journal, which is size-capped and self-vacuuming.

MIT licensed, self-contained build.

## status

**Works:** Bose Quiet Comfort Ultra (gen 1), 48 kHz stereo, all three modes, audibly and
with measured frame timing:

| mode | measured on air |
|---|---|
| LL | 360-byte wire frames every **10.0 ms**, jitter σ 0.01 ms |
| HQ | 668-byte wire frames every **12.0 ms**, `level 5` held, no ladder steps |
| adaptive | 672-byte frames at high level, stepping `2 → 3 → 4 → 5` |

Steady streaming and switching between all three profiles produced no disconnects on the
tested headset. SPA unsent backlog stayed in single-digit to low-double-digit bytes while
streaming, rather than queueing.

**Offline-tested:** R2 stereo at 44.1 kHz in HQ or LL, and 96 kHz in HQ. Playback on an R2
headset is unverified — no sink advertising 96 kHz was available.

**Untested:** congestion-driven bitrate reduction on hardware, multi-hour stability, and
every other headset.

### community hardware report

**Audio-Technica ATH-TWX9MK2 + Steam Deck:** successfully tested on SteamOS 3.9.3
(build 20261015.100), PipeWire 1.6.8, WirePlumber 0.5.18 and BlueZ 5.87.

Current `main` was tested at commit
`42ae0e22678f3d74f1efc5014cb703c072ecc0ac`.

The plugin was installed in the `lc3plus_hr` slot using the stock SteamOS
PipeWire and BlueZ packages; the project's patched PipeWire/BlueZ system
components were not installed. WirePlumber was restricted to the A2DP source
role and to `sbc`, `aptx`, `aptx_adaptive` and `aptx_adaptive_ll`; HFP was
disabled after initial pairing/A2DP connection attempts proved unreliable with
the normal set of Bluetooth roles enabled.

aptX Adaptive playback works correctly. The adaptive bitrate was observed
moving between 270, 340 and 427 kbps according to link conditions.

aptX Adaptive Low Latency also negotiates successfully. PipeWire reports
`api.bluez5.codec = "aptx_adaptive_ll"` and a fresh LL stream reports:

```text
aptx-adaptive: row profile=Ll period=10ms bitrate=270kbps payload=348B
```

The ATH-TWX9MK2 therefore appears to use the normal 10 ms LL mode rather than
the optional 8 ms enhanced-LL mode.

One compatibility issue was found with the current default ST0 transport.
With `APTX_ADAPTIVE_ST0=1` present in the WirePlumber process environment,
the LL profile negotiates successfully but playback on the ATH-TWX9MK2 is
severely corrupted, consisting mainly of static-like fragments and popping
from both earbuds.

Removing only `APTX_ADAPTIVE_ST0`, restarting PipeWire/WirePlumber,
reconnecting the earbuds and selecting the same LL profile restores clean
playback immediately. No other codec or WirePlumber setting was changed for
this A/B test.

The working configuration therefore uses the current codec implementation with
ST0 disabled for this headset.

Standard aptX was also verified independently, helping separate general
Bluetooth/A2DP behaviour from aptX Adaptive codec negotiation.

## requirements

Tested with PipeWire 1.6.9, BlueZ 5.87 and WirePlumber 0.5; the plugin requires PipeWire
1.6.x. The headset must support aptX Adaptive.

The three-profile layout needs two locally patched, package-owned system components. They
are not optional for `a2dp-sink-aptx_adaptive_hq` to exist at all:

1. **PipeWire's bluez5 SPA plugin** (`/usr/lib/spa-0.2/bluez5/libspa-bluez5.so`):
   - `codec-loader.c` gains the `aptx_adaptive_hq` entry in its plugin table, otherwise the
     HQ plugin file is never opened;
   - `type-info.h` gains the `0x303` codec row named `aptx_adaptive_hq`, otherwise card
     profiles — which are enumerated by codec id — can never include HQ.
2. **BlueZ's `bluetoothd`**: keeps the session on an unanswered Abort and recovers from a
   failed reconfiguration, so a profile change does not kill the link.

Both files are package-owned, so they are pinned with `NoUpgrade` in `/etc/pacman.conf`;
pacman then keeps our build and writes the package copy as `.pacnew`. Pinning does not
prevent ABI drift: after any `pipewire` or `bluez` upgrade, check and rebuild with the two
scripts below.

The changes themselves are tracked as diffs in [`patches/`](patches/README.md), against the
exact commits we built and installed — PipeWire `8fa27ca` (1.6.9) and BlueZ `dd9d5ba` (5.87).
One PipeWire patch and three BlueZ patches; no upstream source is included. Each set was
verified by applying it to a clean checkout of its base and comparing every modified file
against our build tree: 10/10 for PipeWire, 7/7 for BlueZ. The BlueZ patches must be applied
in order — `0003` carries changes the two named fixes do not cover, and skipping it yields a
binary that differs from ours.

Building needs `mise` (it provides Rust, Python and uv), `pkg-config` with the libspa
development files, and `python3`.

## install

```sh
mise install
mise run install-plugin
systemctl --user restart wireplumber pipewire
```

This builds the plugin and installs **one** file under `~/.local/lib/spa-0.2/bluez5/`,
which exports all three endpoints, and writes systemd user drop-ins. In the default slot it
installs a small WirePlumber guard script that recovers a headset which configured aptX HD
under an aptX Adaptive profile. Re-running it is safe.

The drop-ins carry one setting that is not cosmetic:

```conf
Environment="APTX_ADAPTIVE_ST0=1"
```

Without it the plugin sends the §7F latency-mode SSRC (`0xA3`) together with a constant
sample-count RTP timestamp. The stream negotiates and the transport starts, but the sink
cannot schedule it: it decodes nothing and the link eventually times out. With ST0 the
plugin sends SSRC `0xAE` and a microsecond timestamp that advances by the frame period.

By default the plugin is installed as PipeWire's **aptX plugin** and loads the
system one behind it, so headsets without aptX Adaptive behave exactly as before. The other
install slots use the LC3plus HR plugin file instead:

| slot | what it does |
|---|---|
| `aptx` (default) | aptX Adaptive takes the aptX HD and aptX ids; aptX Adaptive headsets get it on connect, LDAC is untouched |
| `ldac` | aptX Adaptive takes LDAC's id, ranking above aptX HD |
| `lc3plus_hr` | aptX HD (or aptX/AAC) on connect; aptX Adaptive is picked by hand |

```sh
APTX_ADAPTIVE_SLOT=ldac mise run install-plugin         # or lc3plus_hr
systemctl --user restart wireplumber pipewire
```

## keeping the patched stack healthy

```sh
scripts/check-patched-libs.sh      # PASS, or DRIFT with the hashes that differ
scripts/rebuild-patched-libs.sh    # rebuild and reinstall everything patched
```

The check compares the installed `libspa-bluez5.so` and codec plugin against their builds,
confirms `bluetoothd` is still the patched binary, and verifies the link-clock publisher.
Run it after any upgrade; run the rebuild script if it reports drift.

## choosing the mode on connect

All three profiles are always available. Which one the headset lands on at connect is set by
`APTX_ADAPTIVE_DEFAULT_MODE`, and the adaptive profile's starting level follows
`APTX_ADAPTIVE_START_LOW` (already on in the drop-ins):

```sh
APTX_ADAPTIVE_DEFAULT_MODE=ll mise run install-plugin   # or hq (default)
systemctl --user restart wireplumber pipewire
```

## switching while playing

Through the desktop, by picking another card profile. Through the node's quality property,
without touching A2DP negotiation:

```sh
id=$(pw-cli ls Node | awk '/^\tid /{id=$2} /node.name = "bluez_output\./{print id+0; exit}')
pw-cli set-param "$id" Props '{ quality = 1 }'    # low latency
pw-cli set-param "$id" Props '{ quality = 0 }'    # HQ
```

Or, optionally, in-band by file. Set `APTX_ADAPTIVE_MODE_FILE` in the drop-in (for example
`/run/user/1000/aptx-mode`) and write one of `hq`, `adaptive` or `ll` into it:

```sh
echo adaptive > /run/user/1000/aptx-mode
```

The mode file is inert unless that variable is set, so it can never fight the card profile
you picked in the panel.

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

The plugin logs the ladder row it selected once per stream, and one line per bitrate change:

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: row'
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: level'
```

Those two are always on and cost almost nothing: a handful of lines per connection. The
periodic drift line — backlog, drift and current level, every 10 s — is off by default
because it is the only thing that would write continuously while audio plays. Turn it on when
diagnosing a silent or stuttering stream, by adding to the drop-in and restarting:

```conf
Environment="APTX_ADAPTIVE_DRIFT_LOG=1"
```

```sh
journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
```

On the tested headset, LL reports `period=10ms bitrate=270kbps payload=348B`, and HQ holds
`level 5` with no `level` lines at all.

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

This removes the plugin file, the WirePlumber guard script and configuration, and
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
