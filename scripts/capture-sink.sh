#!/usr/bin/env bash
# DEV ONLY. Make this machine pose as the B&W Px7 S2e (aptX Adaptive sink) so a phone streams
# to it, and record what the phone sends into .scratch/phone-capture/. See capture-sink.py.
#
#   scripts/capture-sink.sh
#
# Stops wireplumber while running (so PipeWire's own sink endpoints don't compete) and
# restarts it on exit. Restores the adapter alias, discoverable and pairable state on exit.
set -euo pipefail
cd "$(dirname "$0")/.."

ALIAS="Px7 S2e capture"
show() { bluetoothctl show | sed -n "s/^\\s*$1: //p" | head -n1; }
old_alias=$(show Alias)
old_pairable=$(show Pairable)
old_discoverable=$(show Discoverable)

cleanup() {
	trap - EXIT INT TERM
	echo
	echo "== restoring adapter and wireplumber"
	[ "$old_discoverable" = yes ] || bluetoothctl discoverable off >/dev/null || true
	[ "$old_pairable" = yes ] || bluetoothctl pairable off >/dev/null || true
	[ -n "$old_alias" ] && bluetoothctl system-alias "$old_alias" >/dev/null || true
	systemctl --user start wireplumber || true
	echo "wireplumber: $(systemctl --user is-active wireplumber)"
}
trap cleanup EXIT INT TERM

cat <<EOF
=====================================================================
 aptX Adaptive phone capture
=====================================================================
 1. Turn the Px7 S2e headset OFF so it doesn't grab the audio.
 2. On the phone: Settings > Bluetooth > pair new device, pick
    "$ALIAS". Pairing is auto-accepted here; confirm on the phone.
 3. In the phone's Bluetooth device settings, make sure HD audio /
    aptX Adaptive is on. (Developer options > Bluetooth audio codec
    shows what's in use.)
 4. Play some music on the phone. The chosen codec + config bytes are
    printed below and saved, packets go to .scratch/phone-capture/.
 5. Ctrl-C here when done. Then on the phone, "forget" the device.
=====================================================================
EOF

echo "== stopping wireplumber (restarted on exit)"
systemctl --user stop wireplumber

echo "== adapter: alias '$ALIAS', pairable, discoverable"
bluetoothctl power on >/dev/null
bluetoothctl system-alias "$ALIAS" >/dev/null
bluetoothctl pairable on >/dev/null
bluetoothctl discoverable-timeout 0 >/dev/null
bluetoothctl discoverable on >/dev/null

uv run scripts/capture-sink.py || true
