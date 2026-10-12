#!/usr/bin/env bash
# Builds the plugin and installs it for the current user in the LC3plus HR slot
# (docs/adr/0001-architecture.md). Safe to re-run. Doesn't restart anything.
# Needs cargo, pkg-config with libspa-0.2, and python3.
# CARGO_FEATURES (optional) is passed to cargo build --features, e.g. dev-replay.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
user_plugin_dir=$HOME/.local/lib/spa-0.2
# SPA_PLUGIN_DIR replaces the built-in search path, so the system dir must be right.
if ! system_plugin_dir=$(pkg-config --variable=plugindir libspa-0.2) || [[ -z $system_plugin_dir ]]; then
	echo "error: pkg-config can't find libspa-0.2's plugindir (install the libspa dev package)" >&2
	exit 1
fi
dest=$user_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so

if [[ -e $system_plugin_dir/bluez5/libspa-codec-bluez5-lc3plus.so ]]; then
	echo "warning: $system_plugin_dir has a real LC3plus plugin; ours shadows it" >&2
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
"
for unit in wireplumber.service pipewire.service; do
	dropin=$config_home/systemd/user/$unit.d/aptx-adaptive.conf
	if [[ -f $dropin ]] && [[ $(<"$dropin")$'\n' == "$dropin_body" ]]; then
		continue
	fi
	mkdir -p "$(dirname "$dropin")"
	printf '%s' "$dropin_body" >"$dropin"
	echo "wrote $dropin"
done

# Left by scripts/dev-replay.sh. A normal install goes back to the encoder.
dev_dropin=$config_home/systemd/user/wireplumber.service.d/aptx-adaptive-dev-replay.conf
if [[ -e $dev_dropin ]]; then
	rm -f "$dev_dropin"
	echo "removed dev replay drop-in $dev_dropin"
fi

if command -v systemctl >/dev/null; then
	systemctl --user daemon-reload
fi

cat <<'EOF'
restart to load it: systemctl --user restart wireplumber pipewire

PipeWire ranks this codec (in the LC3plus HR slot) below aptX HD, aptX and AAC, so pick
the aptX Adaptive A2DP profile by hand (pavucontrol, or pactl set-card-profile).
If monitor.bluez.properties sets bluez5.codecs, it must include "aptx_adaptive".
EOF
