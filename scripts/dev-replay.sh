#!/usr/bin/env bash
# DEV ONLY. Installs the plugin with the replay core: instead of encoding, it sends the
# pre-encoded packets of tests/vectors/r2/<vector> (default sine1k.r1.bin: 1 kHz sine,
# -6 dBFS, looping) with the real caps, framing, RTP and timing. Lets us test negotiation and
# framing on a headset before the codec core exists.
#
#   scripts/dev-replay.sh [header-after-rtp|ttp-clock|ttp-clock-offset|header-in-rtp|header-in-rtp-swapped] [vector]
#
# Framings (default header-after-rtp):
#   header-after-rtp       spec §7C: normal RTP header (PT 96, timestamp = sample count, +672
#                          per packet; SSRC 0), then the 8-byte header, then the payload
#   ttp-clock              RTP timestamp = µs clock from 0, +14000 per packet; SSRC 00 00 00 AE (spec §7B)
#   ttp-clock-offset       same, clock starts at the TTP offset (203000 µs on the Px7)
#   header-in-rtp          old: constant timestamp 1F 63 38 01, SSRC 00 00 00 AE
#   header-in-rtp-swapped  header-in-rtp with each 4-byte half reversed (§7A.2 option b)
#
# A/B switches (spec §7A.3), passed through to the plugin if set:
#   APTX_ADAPTIVE_PT=<0..127>   RTP payload type (default 96)
#   APTX_ADAPTIVE_NOSWAP=1      don't 16-bit swap the payload
# e.g. APTX_ADAPTIVE_PT=0 scripts/dev-replay.sh ttp-clock-offset sine1k.r2.bin
#
# Doesn't restart anything or touch the headset; it prints the commands for that.
set -euo pipefail

framing=${1:-header-after-rtp}
case $framing in
ttp-clock | ttp-clock-offset | header-in-rtp | header-after-rtp | header-in-rtp-swapped) ;;
*)
	echo "usage: $0 [header-after-rtp|ttp-clock|ttp-clock-offset|header-in-rtp|header-in-rtp-swapped] [vector]" >&2
	exit 2
	;;
esac

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
vector=$repo/tests/vectors/r2/${2:-sine1k.r1.bin}
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
extra=
for var in APTX_ADAPTIVE_PT APTX_ADAPTIVE_NOSWAP; do
	if [[ -n ${!var-} ]]; then
		echo "Environment=\"$var=${!var}\"" >>"$dropin"
		extra+=", $var=${!var}"
	fi
done
echo "wrote $dropin (framing $framing, vector ${vector##*/}$extra)"
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

# To try another framing, vector or switch, re-run e.g.
#   APTX_ADAPTIVE_PT=0 $0 ttp-clock-offset sine1k.r1.bin
# and redo (a)-(c).
EOF
