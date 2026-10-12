#!/usr/bin/env bash
# DEV ONLY. Installs the plugin with the replay core: instead of encoding, it sends the
# pre-encoded packets of tests/vectors/r2/sine1k.r1.bin (1 kHz sine, -6 dBFS, looping) with
# the real caps, framing, RTP and timing. Lets us test negotiation and framing on a headset
# before the codec core exists.
#
#   scripts/dev-replay.sh [header-in-rtp|header-after-rtp]
#
# Doesn't restart anything or touch the headset; it prints the commands for that.
set -euo pipefail

framing=${1:-header-in-rtp}
case $framing in
header-in-rtp | header-after-rtp) ;;
*)
	echo "usage: $0 [header-in-rtp|header-after-rtp]" >&2
	exit 2
	;;
esac

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
vector=$repo/tests/vectors/r2/sine1k.r1.bin
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf

[[ -f $vector ]] || {
	echo "error: $vector is missing" >&2
	exit 1
}

# install-plugin.sh removes any old replay drop-in; ours is written after it.
CARGO_FEATURES=dev-replay "$repo/scripts/install-plugin.sh" >/dev/null

mkdir -p "$(dirname "$dropin")"
cat >"$dropin" <<EOF
# DEV ONLY, written by scripts/dev-replay.sh. Delete this file (or run install-plugin.sh) to go back to the encoder.
[Service]
Environment="APTX_ADAPTIVE_REPLAY=$vector"
Environment="APTX_ADAPTIVE_FRAMING=$framing"
EOF
echo "wrote $dropin (framing $framing)"
if command -v systemctl >/dev/null; then
	systemctl --user daemon-reload
fi

cat <<EOF

# (a) restart, then check the plugin logged replay mode
systemctl --user restart wireplumber pipewire pipewire-pulse
journalctl --user -u wireplumber -b --no-pager | grep aptx-adaptive | tail -n 5

# (b) switch the Px7 to aptX Adaptive
card=\$(pactl list cards short | awk '/bluez_card\./ {print \$2; exit}'); echo "\$card"
pactl list cards | grep -A40 "Name: \$card" | grep a2dp-sink
pactl set-card-profile "\$card" a2dp-sink-aptx_adaptive

# (c) play: the plugin ignores the PCM, so silence is enough to keep the stream running.
#     You should hear a steady 1 kHz tone. Ctrl-C to stop.
sink=\$(pactl list sinks short | awk '/bluez_output\./ {print \$2; exit}'); echo "\$sink"
pw-play --target "\$sink" --raw --format s32 --rate 48000 --channels 2 - </dev/zero
journalctl --user -u wireplumber -u pipewire -b --no-pager --since -2min | grep -i 'bluez\|a2dp\|aptx' | tail -n 30

# (d) undo everything
rm -f "$dropin"
rm -f "$config_home/systemd/user/wireplumber.service.d/aptx-adaptive.conf" "$config_home/systemd/user/pipewire.service.d/aptx-adaptive.conf"
rm -f "$HOME/.local/lib/spa-0.2/bluez5/libspa-codec-bluez5-lc3plus.so"
systemctl --user daemon-reload
systemctl --user restart wireplumber pipewire pipewire-pulse
pactl set-card-profile "\$(pactl list cards short | awk '/bluez_card\./ {print \$2; exit}')" a2dp-sink

# To try the other framing, re-run: $0 $([[ $framing == header-in-rtp ]] && echo header-after-rtp || echo header-in-rtp)
# and redo (a)-(c).
EOF
