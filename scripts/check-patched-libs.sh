#!/bin/bash
# Verify the locally patched system libraries are the ones we built.
#
# These files are pinned with `NoUpgrade` in /etc/pacman.conf so pacman keeps
# them across upgrades (the package copy lands as .pacnew). Pinning stops the
# overwrite but not ABI drift: after a pipewire or bluez upgrade the pinned
# binary may no longer match the rest of the stack, so run this and
# scripts/rebuild-patched-libs.sh after any upgrade.
set -u
here=$(cd "$(dirname "$0")/.." && pwd)
fail=0

same() { # label installed built
	if [ ! -e "$2" ]; then printf '%-22s MISSING installed %s\n' "$1" "$2"; fail=1; return; fi
	if [ ! -e "$3" ]; then printf '%-22s no build at %s\n' "$1" "$3"; fail=1; return; fi
	a=$(sha256sum "$2" | cut -d' ' -f1)
	b=$(sha256sum "$3" | cut -d' ' -f1)
	if [ "$a" = "$b" ]; then printf '%-22s OK   %s\n' "$1" "${a:0:16}"
	else printf '%-22s DRIFT installed=%s build=%s\n' "$1" "${a:0:16}" "${b:0:16}"; fail=1; fi
}

same "spa bluez5 plugin" /usr/lib/spa-0.2/bluez5/libspa-bluez5.so \
	"$here/../pipewire/_build/spa/plugins/bluez5/libspa-bluez5.so"
same "aptx codec plugin" "$HOME/.local/lib/spa-0.2/bluez5/libspa-codec-bluez5-aptx.so" \
	"$here/target/release/libspa_codec_aptx_adaptive.so"

# bluetoothd is stripped on install, so compare presence and build availability.
stock=$(ls /usr/lib/bluetooth/bluetoothd.stock-* 2>/dev/null | head -1)
if [ -n "$stock" ]; then
	if cmp -s /usr/lib/bluetooth/bluetoothd "$stock"; then
		printf '%-22s STOCK (patched build not installed)\n' "bluetoothd"; fail=1
	else
		printf '%-22s patched (differs from %s)\n' "bluetoothd" "$(basename "$stock")"
	fi
else
	printf '%-22s no stock backup to compare; size %s\n' "bluetoothd" \
		"$(stat -c%s /usr/lib/bluetooth/bluetoothd)"
fi

if grep -q 'monotonic_us' "$HOME/.local/bin/aptx-linkclock-inner.sh" 2>/dev/null &&
   grep -q 'sleep 0.2' "$HOME/.local/bin/aptx-linkclock-inner.sh"; then
	printf '%-22s OK (pair format, 5 Hz)\n' "linkclock publisher"
else
	printf '%-22s DRIFT (expected "<monotonic_us> <clock>" and sleep 0.2)\n' "linkclock publisher"; fail=1
fi

echo
if [ "$fail" -eq 0 ]; then echo "patched stack: PASS"; else echo "patched stack: DRIFT (run scripts/rebuild-patched-libs.sh)"; fi
exit "$fail"
