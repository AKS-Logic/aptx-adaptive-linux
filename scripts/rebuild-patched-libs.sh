#!/bin/bash
# Rebuild and reinstall the locally patched system pieces, then verify them.
#
# Run after a `pacman -Syu` that touched pipewire or bluez: NoUpgrade in
# /etc/pacman.conf preserves our files, but a new pipewire/bluez ABI still
# needs a matching rebuild. Also run it if scripts/check-patched-libs.sh
# reports DRIFT.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
audio=$(cd "$here/.." && pwd)

echo "== pipewire bluez5 plugin (patched codec-loader + codec type-info) =="
ninja -C "$audio/pipewire/_build" spa/plugins/bluez5/libspa-bluez5.so
sudo install -m 755 "$audio/pipewire/_build/spa/plugins/bluez5/libspa-bluez5.so" \
	/usr/lib/spa-0.2/bluez5/libspa-bluez5.so

echo "== bluez bluetoothd (task-60 abort/reconfiguration patches) =="
if [ -d "$audio/bluez" ]; then
	make -C "$audio/bluez" src/bluetoothd -j"$(nproc)"
	tmp=$(mktemp)
	strip -o "$tmp" "$audio/bluez/src/bluetoothd"
	sudo install -m 755 "$tmp" /usr/lib/bluetooth/bluetoothd
	rm -f "$tmp"
else
	echo "no $audio/bluez checkout; leaving bluetoothd alone" >&2
fi

echo "== aptx adaptive codec plugin =="
( cd "$here" && cargo build --release -p spa-codec-aptx-adaptive )
install -m 755 "$here/target/release/libspa_codec_aptx_adaptive.so" \
	"$HOME/.local/lib/spa-0.2/bluez5/libspa-codec-bluez5-aptx.so"
install -m 755 "$here/target/release/libspa_codec_aptx_adaptive.so" \
	"$HOME/.local/lib/spa-0.2/bluez5/libspa-codec-bluez5-aptx_adaptive_hq.so"

echo "== verify =="
"$here/scripts/check-patched-libs.sh"

echo
echo "Now restart the stack:"
echo "  sudo systemctl restart bluetooth"
echo "  systemctl --user restart wireplumber"
