#!/usr/bin/env bash
# DEV ONLY. Installs the plugin with the replay core: instead of encoding, it sends the
# pre-encoded packets of a file (default tests/vectors/r2/sine1k.r1.bin: 1 kHz sine, -6 dBFS,
# looping, or e.g. packets captured from a phone) with the real caps, framing (spec §7D, §7F,
# §7H), RTP and timing.
#
#   scripts/dev-replay.sh [vector|/path/to/packets.bin|--no-replay]
#
# A bare name is looked up in tests/vectors/r2/. --no-replay installs the same dev build but
# runs the real encoder on the PCM.
#
# Optional, passed through to the plugin if set:
#   APTX_ADAPTIVE_SCMST=1       put the 1-byte SCMS-T header (default 0x02, copy free) after
#                               the RTP header. Only for a sink configured with SCMS-T, which
#                               stock bluetoothd never does
#   APTX_ADAPTIVE_SCMST_BYTE=0x00  the SCMS-T byte to send instead
#
# Doesn't restart anything or touch the headset; it prints the commands for that.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
replay=1
vector=${1:-sine1k.r1.bin}
case $vector in
--no-replay) replay= ;;
-*)
	echo "usage: $0 [vector|/path/to/packets.bin|--no-replay]" >&2
	exit 2
	;;
*/*) ;;
*) vector=$repo/tests/vectors/r2/$vector ;;
esac
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf

[[ -z $replay || -f $vector ]] || {
	echo "error: $vector is missing" >&2
	exit 1
}

CARGO_FEATURES=dev-replay "$repo/scripts/install-plugin.sh" >/dev/null

mkdir -p "$(dirname "$dropin")"
cat >"$dropin" <<EOF
# DEV ONLY, written by scripts/dev-replay.sh. Delete this file (or run install-plugin.sh) to go back to the normal build.
[Service]
EOF
if [[ -n $replay ]]; then
	vector=$(realpath "$vector")
	echo "Environment=\"APTX_ADAPTIVE_REPLAY=$vector\"" >>"$dropin"
	core="replay of ${vector##*/}"
else
	core="real encoder"
fi
extra=
for var in APTX_ADAPTIVE_SCMST APTX_ADAPTIVE_SCMST_BYTE; do
	if [[ -n ${!var-} ]]; then
		echo "Environment=\"$var=${!var}\"" >>"$dropin"
		extra+=", $var=${!var}"
	fi
done
echo "wrote $dropin ($core$extra)"
if command -v systemctl >/dev/null; then
	systemctl --user daemon-reload
fi

cat <<EOF

# (a) restart, then check the plugin logged the dev settings
systemctl --user restart wireplumber pipewire pipewire-pulse
journalctl --user -u wireplumber -b --no-pager | grep aptx-adaptive | tail -n 5

# (b) switch the headset to aptX Adaptive
card=\$(pactl list cards short | awk '/bluez_card\./ {print \$2; exit}'); echo "\$card"
pactl list cards | grep -A40 "Name: \$card" | grep a2dp-sink
# a2dp-sink-aptx_adaptive in the lc3plus_hr slot; in the default ldac slot it is a2dp-sink
pactl set-card-profile "\$card" a2dp-sink-aptx_adaptive || pactl set-card-profile "\$card" a2dp-sink

# (c) play. With replay the plugin ignores the PCM, so silence keeps the stream running and you
#     should hear the replayed packets (a steady 1 kHz tone for the default vector). With
#     --no-replay, play real audio instead of /dev/zero. Ctrl-C to stop.
sink=\$(pactl list sinks short | awk '/bluez_output\./ {print \$2; exit}'); echo "\$sink"
pw-play --target "\$sink" --raw --format s32 --rate 48000 --channels 2 - </dev/zero
journalctl --user -u wireplumber -u pipewire -b --no-pager --since -2min | grep -i 'bluez\|a2dp\|aptx' | tail -n 30

# (d) undo everything (plugin, drop-ins and this dev-replay drop-in)
"$repo/scripts/install-plugin.sh" --uninstall
systemctl --user restart wireplumber pipewire pipewire-pulse
# after the undo the stock order is back, so a2dp-sink is aptX HD again
pactl set-card-profile "\$(pactl list cards short | awk '/bluez_card\./ {print \$2; exit}')" a2dp-sink

# The drift line (every 10 s while streaming) is in the wireplumber journal:
#   journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
EOF
