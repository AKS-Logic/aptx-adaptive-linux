#!/usr/bin/env bash
# DEV ONLY. Installs the plugin with the replay core: instead of encoding, it sends the
# pre-encoded packets of tests/vectors/r2/<vector> (default sine1k.r1.bin: 1 kHz sine,
# -6 dBFS, looping) with the real caps, framing, RTP and timing. Lets us test negotiation and
# framing on a headset before the codec core exists.
#
#   scripts/dev-replay.sh [phone|ttp-clock-offset|header-after-rtp|header-in-rtp|header-in-rtp-swapped] [vector|--no-replay]
#
# --no-replay installs the same dev build and switches but runs the real encoder on the PCM.
#
# Framings (default phone):
#   phone                  spec §7D, what a Snapdragon phone sends: RTP 80 60, timestamp = µs
#                          clock from 0, +14000 per packet; SSRC 00 00 00 AE; then the payload
#   ttp-clock-offset       old: same, clock starts at the TTP offset (203000 µs on the Px7)
#   header-after-rtp       old, spec §7C: normal RTP header (PT 96, timestamp = sample count,
#                          +672 per packet; SSRC 0), then the 8-byte header, then the payload
#   header-in-rtp          old: constant timestamp 1F 63 38 01, SSRC 00 00 00 AE
#   header-in-rtp-swapped  header-in-rtp with each 4-byte half reversed (§7A.2 option b)
#
# A/B switches (spec §7A.3), passed through to the plugin if set:
#   APTX_ADAPTIVE_PT=<0..127>   RTP payload type (default 96)
#   APTX_ADAPTIVE_NOSWAP=1      don't 16-bit swap the payload
#   APTX_ADAPTIVE_SCMST=1       put the 1-byte SCMS-T header (default 0x02, copy free) after
#                               the RTP header. Only for a sink configured with SCMS-T, which
#                               stock bluetoothd never does (see .scratch/scmst-test.sh)
#   APTX_ADAPTIVE_SCMST_BYTE=0x00  the SCMS-T byte to send instead
#   APTX_ADAPTIVE_SOURCE_TYPE=0|2  config byte 6 source type (spec §7E, default 2)
#   APTX_ADAPTIVE_SSRC=0x..     the whole RTP SSRC (default 0x000000a3, HQ, spec §7F)
#   APTX_ADAPTIVE_TS=samples-per-packet|us|samples  RTP timestamp: samples per channel in the
#                               packet, 0x000002a0 every packet (default, §7F), or a clock in
#                               µs (+14000) or samples (+672)
#   APTX_ADAPTIVE_TTP_ADJUST=off|fixed:<ms>|auto  §7F latency adjust in the timestamp's high
#                               16 bits: 0 (default), a constant signed ms, or following the
#                               measured drift (±1 ms per 10 s, max ±50; host fast → negative)
# e.g. APTX_ADAPTIVE_PT=0 scripts/dev-replay.sh ttp-clock-offset sine1k.r2.bin
#
# Doesn't restart anything or touch the headset; it prints the commands for that.
set -euo pipefail

framing=${1:-phone}
case $framing in
phone | ttp-clock-offset | header-in-rtp | header-after-rtp | header-in-rtp-swapped) ;;
*)
	echo "usage: $0 [phone|ttp-clock-offset|header-after-rtp|header-in-rtp|header-in-rtp-swapped] [vector|--no-replay]" >&2
	exit 2
	;;
esac

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
replay=1
if [[ ${2-} == --no-replay ]]; then
	replay=
fi
vector=$repo/tests/vectors/r2/${2:-sine1k.r1.bin}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf

[[ -z $replay || -f $vector ]] || {
	echo "error: $vector is missing" >&2
	exit 1
}

# install-plugin.sh removes any old replay drop-in; ours is written after it.
CARGO_FEATURES=dev-replay "$repo/scripts/install-plugin.sh" >/dev/null

mkdir -p "$(dirname "$dropin")"
cat >"$dropin" <<EOF
# DEV ONLY, written by scripts/dev-replay.sh. Delete this file (or run install-plugin.sh) to go back to the normal build.
[Service]
Environment="APTX_ADAPTIVE_FRAMING=$framing"
EOF
if [[ -n $replay ]]; then
	echo "Environment=\"APTX_ADAPTIVE_REPLAY=$vector\"" >>"$dropin"
	core="vector ${vector##*/}"
else
	core="real encoder"
fi
extra=
for var in APTX_ADAPTIVE_PT APTX_ADAPTIVE_NOSWAP APTX_ADAPTIVE_SCMST APTX_ADAPTIVE_SCMST_BYTE \
	APTX_ADAPTIVE_SOURCE_TYPE APTX_ADAPTIVE_SSRC APTX_ADAPTIVE_TS APTX_ADAPTIVE_TTP_ADJUST; do
	if [[ -n ${!var-} ]]; then
		echo "Environment=\"$var=${!var}\"" >>"$dropin"
		extra+=", $var=${!var}"
	fi
done
echo "wrote $dropin (framing $framing, $core$extra)"
if command -v systemctl >/dev/null; then
	systemctl --user daemon-reload
fi

cat <<EOF

# (a) restart, then check the plugin logged the dev settings
systemctl --user restart wireplumber pipewire pipewire-pulse
journalctl --user -u wireplumber -b --no-pager | grep aptx-adaptive | tail -n 5

# (b) switch the Px7 to aptX Adaptive
card=\$(pactl list cards short | awk '/bluez_card\./ {print \$2; exit}'); echo "\$card"
pactl list cards | grep -A40 "Name: \$card" | grep a2dp-sink
pactl set-card-profile "\$card" a2dp-sink-aptx_adaptive

# (c) play. With replay the plugin ignores the PCM, so silence keeps the stream running and you
#     should hear a steady 1 kHz tone. With --no-replay, play real audio instead of /dev/zero.
#     Ctrl-C to stop.
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
#   APTX_ADAPTIVE_SOURCE_TYPE=0 APTX_ADAPTIVE_TS=samples $0 phone --no-replay
#   APTX_ADAPTIVE_TTP_ADJUST=auto $0 phone --no-replay
# The drift line (every 10 s while streaming) is in the wireplumber journal:
#   journalctl --user -u wireplumber -f | grep 'aptx-adaptive: drift'
# and redo (a)-(c).
EOF
