# Quickstart: verifying the three profiles

Everything here is a command an operator can run. Each step states what to observe; a
mismatch is a spec violation, not a nuisance.

## 1. Confirm the patched stack

```bash
cd ~/aks-infra/audio/aptx-linux
./scripts/check-patched-libs.sh
```

Expect:

```
spa bluez5 plugin      OK   <hash>
aptx codec plugin      OK   <hash>
bluetoothd             patched (differs from bluetoothd.stock-0610)
linkclock publisher    OK (pair format, 5 Hz)

patched stack: PASS
```

`DRIFT` means run `./scripts/rebuild-patched-libs.sh`, then restart bluetooth and
wireplumber.

## 2. Confirm the three profiles exist

```bash
pactl list cards | sed -n '/bluez_card.AC_BF_71_CB_3E_C1/,$p' | grep 'codec aptX'
```

Expect exactly three:

```
a2dp-sink:                   codec aptX Adaptive HQ
a2dp-sink-aptx_adaptive:     codec aptX Adaptive
a2dp-sink-aptx_adaptive_ll:  codec aptX Adaptive LL
```

## 3. Listen to each profile

Pick a profile in the desktop audio panel (or `pactl set-card-profile`), play something,
and confirm sound. Switch through all three.

Expect: audio on every profile, and the headset **not** announcing a disconnect.

Verify the link never dropped:

```bash
bluetoothctl info AC:BF:71:CB:3E:C1 | grep Connected
sudo journalctl -u bluetooth --since '-5min' | grep -c dev_disconnected
```

Expect `Connected: yes` and zero disconnects for the switches you just made.
(A restart or plugin reinstall in the same window will show up here — re-measure instead of
concluding.)

## 4. Confirm the level policy

Watch the plugin's own telemetry while playing:

```bash
journalctl --user -u wireplumber -f | grep -E 'level|drift'
```

| Profile | Expect |
|---|---|
| HQ (`a2dp-sink`) | `level 5` in the drift line, **no** `level x -> y` transitions |
| adaptive (`a2dp-sink-aptx_adaptive`) | `3 -> 2` at open, then `2 -> 3 -> 4 -> 5` |
| LL (`..._ll`) | no ladder movement; low, stable level |

## 5. Confirm the wire format

Capture frames for one profile at a time:

```bash
sudo timeout 8 btmon > /tmp/cap.log
grep -o 'len [0-9]*' /tmp/cap.log | sort | uniq -c | sort -rn | head -3
```

| Profile | Dominant wire size | Codec payload |
|---|---|---|
| LL | `364` | 348 bytes |
| HQ | `668` | 656 bytes |
| adaptive (high) | `672` | 660 bytes |

Backlog should sit in the tens of bytes while streaming:

```bash
journalctl --user -u wireplumber --since '-1min' | grep drift | tail -1
```

Thousands of unsent bytes means the sink is not draining the stream — check the RTP envelope
(SSRC `0xAE`, timestamps advancing) before suspecting the encoder.

## 6. Optional — in-band mode change (no reconfiguration)

Only active when the environment names a control file:

```bash
# in the wireplumber/pipewire drop-in:
#   Environment="APTX_ADAPTIVE_MODE_FILE=/run/user/1000/aptx-mode"

echo ll > /run/user/1000/aptx-mode         # lowest latency
echo adaptive > /run/user/1000/aptx-mode   # scaling
echo hq > /run/user/1000/aptx-mode         # pinned quality
```

The plugin logs `in-band mode -> <mode>` and switches without touching A2DP negotiation.
Without `APTX_ADAPTIVE_MODE_FILE`, this file is ignored entirely and the card profile is
authoritative.

## 7. Gates before committing

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets
./scripts/check-patched-libs.sh
```