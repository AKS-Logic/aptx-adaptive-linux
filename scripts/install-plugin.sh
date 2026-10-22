#!/usr/bin/env bash
# Builds the plugin and installs it for the current user, as PipeWire's aptX plugin file
# (slot aptx, docs/adr/0004-aptx-slot.md) or its LC3plus HR plugin file (slots ldac and
# lc3plus_hr, docs/adr/0001-architecture.md). Only one of the two is ever installed.
# Slot aptx also installs a WirePlumber script (wireplumber/aptx-adaptive-guard.lua) into
# ${XDG_DATA_HOME:-~/.local/share}/wireplumber/scripts/aptx-linux and a fragment enabling it into
# ${XDG_CONFIG_HOME:-~/.config}/wireplumber/wireplumber.conf.d; other slots remove them.
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
# APTX_ADAPTIVE_SLOT (optional, default aptx) picks the plugin file and the codec ids the
# plugin claims:
#   aptx        replaces PipeWire's aptX plugin and passes its codecs through; aptX Adaptive
#               takes the aptX HD and aptX ids, so an aptX Adaptive headset gets it on
#               connect and LDAC is left alone (docs/adr/0004-aptx-slot.md)
#   ldac        borrows the LDAC id, ranked above aptX HD (docs/adr/0002-codec-autoselect.md).
#               Collides with LDAC on headsets that have both.
#   lc3plus_hr  aptX HD (or aptX, AAC) on connect, aptX Adaptive picked by hand
# APTX_ADAPTIVE_DEFAULT_MODE (optional, default hq) picks the mode a fresh connection gets
# (docs/adr/0003-ll-profile.md):
#   hq  the aptX Adaptive profile on connect, Low Latency picked by hand
#   ll  the aptX Adaptive (Low Latency) profile on connect, HQ picked by hand
# Re-run with other values to switch; only the drop-ins change.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
user_plugin_dir=$HOME/.local/lib/spa-0.2
aptx_dest=$user_plugin_dir/bluez5/libspa-codec-bluez5-aptx.so
# HQ is exported by its own factory and must live in its own file: a third
# endpoint inside one plugin file is not registered by BlueZ.
aptx_hq_dest=$user_plugin_dir/bluez5/libspa-codec-bluez5-aptx_adaptive_hq.so
lc3plus_dest=$user_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so
units=(wireplumber.service pipewire.service)
dev_dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf
# WirePlumber 0.5 reads conf.d fragments from the config dir and scripts from the data dir.
wp_conf_dir=$config_home/wireplumber/wireplumber.conf.d
wp_script_dir=${XDG_DATA_HOME:-$HOME/.local/share}/wireplumber/scripts/aptx-linux
guard_conf=$wp_conf_dir/90-aptx-adaptive-guard.conf
guard_script=$wp_script_dir/aptx-adaptive-guard.lua

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

remove_guard() {
	remove "$guard_conf"
	remove "$guard_script"
	for dir in "$wp_script_dir" "$wp_conf_dir" "$(dirname "$wp_script_dir")" \
		"$(dirname "$(dirname "$wp_script_dir")")" "$(dirname "$wp_conf_dir")"; do
		if [[ -d $dir ]] && rmdir "$dir" 2>/dev/null; then
			echo "removed empty $dir"
		fi
	done
}

case ${1-} in
--uninstall)
	remove "$aptx_dest"
	remove "$aptx_hq_dest"
	remove "$lc3plus_dest"
	for unit in "${units[@]}"; do
		remove "$config_home/systemd/user/$unit.d/aptx-adaptive.conf"
	done
	remove "$dev_dropin"
	remove_guard
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

slot=${APTX_ADAPTIVE_SLOT:-aptx}
case $slot in
aptx)
	dest=$aptx_dest
	other_dest=$lc3plus_dest
	;;
ldac | lc3plus_hr)
	dest=$lc3plus_dest
	other_dest=$aptx_dest
	;;
*)
	echo "error: APTX_ADAPTIVE_SLOT=$slot is not aptx, ldac or lc3plus_hr" >&2
	exit 2
	;;
esac
default_mode=${APTX_ADAPTIVE_DEFAULT_MODE:-hq}
case $default_mode in
hq | ll) ;;
*)
	echo "error: APTX_ADAPTIVE_DEFAULT_MODE=$default_mode is not hq or ll" >&2
	exit 2
	;;
esac

if ! system_plugin_dir=$(pkg-config --variable=plugindir libspa-0.2) || [[ -z $system_plugin_dir ]]; then
	echo "error: pkg-config can't find libspa-0.2's plugindir (install the libspa dev package)" >&2
	exit 1
fi

if [[ $slot == aptx ]]; then
	if [[ ! -e $system_plugin_dir/bluez5/libspa-codec-bluez5-aptx.so ]]; then
		echo "warning: $system_plugin_dir has no aptX plugin to pass through; aptX and aptX HD" >&2
		echo "         will be unavailable (docs/adr/0004-aptx-slot.md)" >&2
	fi
elif [[ -e $system_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so ]]; then
	echo "warning: $system_plugin_dir has a real LC3plus plugin; ours shadows it" >&2
fi
if [[ $slot == ldac && -e $system_plugin_dir/bluez5/libspa-codec-bluez5-ldac.so ]]; then
	echo "note: the LDAC plugin is installed. If a headset supports both LDAC and aptX Adaptive," >&2
	echo "      install with the default APTX_ADAPTIVE_SLOT=aptx (docs/adr/0004-aptx-slot.md)" >&2
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
remove "$other_dest"
if [[ $slot == aptx ]]; then
	install -Dm644 "$target_dir/release/libspa_codec_aptx_adaptive.so" "$aptx_hq_dest"
	echo "installed $aptx_hq_dest"
else
	remove "$aptx_hq_dest"
fi

if [[ $slot == aptx ]]; then
	# Recovers a headset that configured the real aptX HD under our profile (ADR 0004).
	install -Dm644 "$repo/wireplumber/aptx-adaptive-guard.lua" "$guard_script"
	install -Dm644 "$repo/wireplumber/90-aptx-adaptive-guard.conf" "$guard_conf"
	echo "installed $guard_script"
	echo "installed $guard_conf"
else
	remove_guard
fi

# APTX_ADAPTIVE_ST0=1 is not optional: without it the plugin sends the §7F
# latency-mode SSRC (0xA3) with a constant sample-count timestamp, which a sink
# cannot schedule — the stream negotiates and then plays silence.
# APTX_ADAPTIVE_START_LOW=1 makes the adaptive profile begin at the lowest
# bitrate row and climb; HQ pins its own level in code.
dropin_body="[Service]
Environment=\"SPA_PLUGIN_DIR=$user_plugin_dir:$system_plugin_dir\"
Environment=\"APTX_ADAPTIVE_SLOT=$slot\"
Environment=\"APTX_ADAPTIVE_DEFAULT_MODE=$default_mode\"
Environment=\"APTX_ADAPTIVE_ST0=1\"
Environment=\"APTX_ADAPTIVE_START_LOW=1\"
MemoryDenyWriteExecute=no
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

if [[ -e $dev_dropin ]]; then
	rm -f "$dev_dropin"
	echo "removed dev replay drop-in $dev_dropin"
fi

daemon_reload

echo "restart to load it: systemctl --user restart wireplumber pipewire"
echo
hq_profile=a2dp-sink-aptx_adaptive
ll_profile=a2dp-sink-aptx_adaptive_ll
if [[ $slot != lc3plus_hr && $default_mode == hq ]]; then
	hq_profile=a2dp-sink
elif [[ $slot != lc3plus_hr ]]; then
	ll_profile=a2dp-sink
fi
case $slot in
aptx)
	cat <<'EOF'
Codec slot: aptx. The plugin replaces PipeWire's aptX plugin and passes aptX, aptX HD and
aptX-LL through. On a headset with aptX Adaptive, aptX Adaptive takes the aptX HD and aptX
profiles; headsets without it keep aptX HD and aptX as before. LDAC is untouched and still
ranks first, so a headset with both connects on LDAC. A WirePlumber script switches an
aptX Adaptive headset that set itself up on the real aptX HD over to aptX Adaptive.
EOF
	;;
ldac)
	cat <<'EOF'
Codec slot: ldac. PipeWire ranks aptX Adaptive above aptX HD, aptX and AAC, so BlueZ
configures it directly on connect when the headset has it, and aptX HD becomes
a2dp-sink-aptx_hd. A headset that also has LDAC may get either (docs/adr/0002).
To go back to the default: APTX_ADAPTIVE_SLOT=aptx scripts/install-plugin.sh, then restart.
EOF
	;;
*)
	cat <<'EOF'
Codec slot: lc3plus_hr. PipeWire ranks aptX Adaptive below aptX HD, aptX and AAC, so aptX
HD is configured on connect. Pick an aptX Adaptive profile by hand (KDE, pavucontrol, or
pactl set-card-profile); each switch makes the headset chime.
EOF
	;;
esac
cat <<EOF
Default mode: $default_mode (APTX_ADAPTIVE_DEFAULT_MODE=hq|ll). Profiles:
  a2dp-sink                    aptX Adaptive HQ (pinned, quality over latency)
  a2dp-sink-aptx_adaptive      aptX Adaptive (starts low, scales with the link)
  a2dp-sink-aptx_adaptive_ll   aptX Adaptive LL (lowest latency)
If monitor.bluez.properties sets bluez5.codecs, it must include "aptx_adaptive",
"aptx_adaptive_ll" and "aptx_adaptive_hq".
EOF
