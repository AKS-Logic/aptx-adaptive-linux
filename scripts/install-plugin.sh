#!/usr/bin/env bash
# Builds the plugin and installs it for the current user as PipeWire's LC3plus HR plugin
# file (docs/adr/0001-architecture.md), claiming the codec id picked by APTX_ADAPTIVE_SLOT.
# Safe to re-run. Doesn't restart anything.
#
#   scripts/install-plugin.sh               install or update
#   scripts/install-plugin.sh --uninstall   remove the plugin and every drop-in we write
#
# --uninstall removes directories the install created only if they're empty, and leaves
# ~/.local/lib and ${XDG_CONFIG_HOME:-~/.config}/systemd/user in place.
#
# Needs cargo, pkg-config with libspa-0.2, and python3 (not for --uninstall).
# CARGO_FEATURES (optional) is passed to cargo build --features, e.g. dev-replay.
# APTX_ADAPTIVE_SLOT (optional, default ldac) picks the codec id the plugin claims
# (docs/adr/0002-codec-autoselect.md):
#   ldac        ranked above aptX HD, so a headset with aptX Adaptive gets it on connect
#   lc3plus_hr  aptX HD (or aptX, AAC) on connect, aptX Adaptive picked by hand. Use this
#               for headsets that also support LDAC.
# Re-run with the other value to switch; only the drop-ins change.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
user_plugin_dir=$HOME/.local/lib/spa-0.2
dest=$user_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so
units=(wireplumber.service pipewire.service)
# Left by scripts/dev-replay.sh.
dev_dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf

daemon_reload() {
	if command -v systemctl >/dev/null; then
		systemctl --user daemon-reload
	fi
}

remove() {
	if [[ -e $1 ]]; then
		rm -f "$1"
		echo "removed $1"
	fi
}

case ${1-} in
--uninstall)
	remove "$dest"
	for unit in "${units[@]}"; do
		remove "$config_home/systemd/user/$unit.d/aptx-adaptive.conf"
	done
	remove "$dev_dropin"
	# Only directories we may have created, and only if nothing else is in them.
	dirs=()
	for unit in "${units[@]}"; do
		dirs+=("$config_home/systemd/user/$unit.d")
	done
	for dir in "${dirs[@]}" "$user_plugin_dir/bluez5" "$user_plugin_dir"; do
		if [[ -d $dir ]] && rmdir "$dir" 2>/dev/null; then
			echo "removed empty $dir"
		fi
	done
	daemon_reload
	echo "restart to unload it: systemctl --user restart wireplumber pipewire"
	exit 0
	;;
"") ;;
*)
	echo "usage: $0 [--uninstall]" >&2
	echo '  --uninstall leaves ~/.local/lib and ${XDG_CONFIG_HOME:-~/.config}/systemd/user in place' >&2
	exit 2
	;;
esac

slot=${APTX_ADAPTIVE_SLOT:-ldac}
case $slot in
ldac | lc3plus_hr) ;;
*)
	echo "error: APTX_ADAPTIVE_SLOT=$slot is not ldac or lc3plus_hr" >&2
	exit 2
	;;
esac

# SPA_PLUGIN_DIR replaces the built-in search path, so the system dir must be right.
if ! system_plugin_dir=$(pkg-config --variable=plugindir libspa-0.2) || [[ -z $system_plugin_dir ]]; then
	echo "error: pkg-config can't find libspa-0.2's plugindir (install the libspa dev package)" >&2
	exit 1
fi

if [[ -e $system_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so ]]; then
	echo "warning: $system_plugin_dir has a real LC3plus plugin; ours shadows it" >&2
fi
if [[ $slot == ldac && -e $system_plugin_dir/bluez5/libspa-codec-bluez5-ldac.so ]]; then
	echo "note: the LDAC plugin is installed. If a headset supports both LDAC and aptX Adaptive," >&2
	echo "      install with APTX_ADAPTIVE_SLOT=lc3plus_hr (docs/adr/0002-codec-autoselect.md)" >&2
fi

features=()
if [[ -n ${CARGO_FEATURES:-} ]]; then
	features=(--features "$CARGO_FEATURES")
fi
cargo build --release --manifest-path "$repo/Cargo.toml" -p spa-codec-aptx-adaptive "${features[@]}"
target_dir=$(cargo metadata --manifest-path "$repo/Cargo.toml" --format-version 1 --no-deps |
	python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
install -Dm644 "$target_dir/release/libspa_codec_aptx_adaptive.so" "$dest"
echo "installed $dest"

# The bluez5 monitor, and so the codec loader, runs in wireplumber. pipewire gets the same
# setting in case bluez5 is ever loaded there (e.g. a pipewire.conf.d bluez context).
dropin_body="[Service]
Environment=\"SPA_PLUGIN_DIR=$user_plugin_dir:$system_plugin_dir\"
Environment=\"APTX_ADAPTIVE_SLOT=$slot\"
"
for unit in "${units[@]}"; do
	dropin=$config_home/systemd/user/$unit.d/aptx-adaptive.conf
	if [[ -f $dropin ]] && [[ $(<"$dropin")$'\n' == "$dropin_body" ]]; then
		continue
	fi
	mkdir -p "$(dirname "$dropin")"
	printf '%s' "$dropin_body" >"$dropin"
	echo "wrote $dropin"
done

# A normal install goes back to the encoder.
if [[ -e $dev_dropin ]]; then
	rm -f "$dev_dropin"
	echo "removed dev replay drop-in $dev_dropin"
fi

daemon_reload

echo "restart to load it: systemctl --user restart wireplumber pipewire"
echo
if [[ $slot == ldac ]]; then
	cat <<'EOF'
Codec slot: ldac. PipeWire ranks aptX Adaptive above aptX HD, aptX and AAC, so BlueZ
configures it directly on connect when the headset has it. It's then the plain a2dp-sink
profile, and aptX HD becomes a2dp-sink-aptx_hd.
To go back: APTX_ADAPTIVE_SLOT=lc3plus_hr scripts/install-plugin.sh, then restart.
EOF
else
	cat <<'EOF'
Codec slot: lc3plus_hr. PipeWire ranks aptX Adaptive below aptX HD, aptX and AAC, so aptX
HD is configured on connect. Pick the a2dp-sink-aptx_adaptive profile by hand (pavucontrol,
or pactl set-card-profile); each switch makes the headset chime.
EOF
fi
echo 'Low latency is the a2dp-sink-aptx_adaptive_ll profile (KDE, pavucontrol, pactl).'
echo 'If monitor.bluez.properties sets bluez5.codecs, it must include "aptx_adaptive", and'
echo '"aptx_adaptive_ll" for the low-latency profile.'
