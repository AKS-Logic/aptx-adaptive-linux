# 0003 - HQ and low latency as card profiles

## status

Implemented. The October 13 live test confirmed LL on connect in the `ldac` slot, one SetConfiguration, and both profiles listed in KDE. The October 14 test confirmed LL on connect and LL → HQ → LL card-profile switches in the default `aptx` slot. Runtime switching through the node's `quality` property was tested separately on October 13. The user confirmed profile selection is tested on October 15 ([test status](../../spec/QUESTIONS.md#current-status-2026-10-14)).

## problem

HQ or low latency is a codec node prop (`quality`, like LDAC's). No GUI shows codec node props. KDE (settings and tray), pavucontrol and pwvucontrol show **card profiles**, and pavucontrol also has a Codec combobox built from the device's `bluetoothAudioCodec` PropInfo. Both lists have one entry per codec **id**. So LL could only be reached with `pw-cli` or a WirePlumber setting, and the setting left the profile labelled plain "aptX Adaptive" while LL ran.

Users who want LL by default also want it **on connect**: one SetConfiguration, the profile labelled Low Latency, and no HQ → LL reconfigure (which on the Px7 S2e means the chime, ADR 0002).

## decision

The plugin exports two `media_codec` entries for the one `aptx_adaptive` endpoint, using the same pattern as PipeWire's `sbc` / `sbc_xq`:

| | HQ entry | LL entry |
|---|---|---|
| `name` | `aptx_adaptive` | `aptx_adaptive_ll` |
| `description` | aptX Adaptive | aptX Adaptive (Low Latency) |
| `endpoint_name` | `aptx_adaptive` | `aptx_adaptive` |
| streams start in | HQ, or the device's `bluez5.a2dp.aptx-adaptive.mode` if it's the owner | LL |

One of them is the **owner**: it has the slot's id (LDAC or LC3plus HR, ADR 0002; aptX HD in the `aptx` slot, ADR 0004) and `fill_caps`, and comes first. The other is the **alias**: id `SPA_BLUETOOTH_AUDIO_CODEC_MPEG` (3), or `SPA_BLUETOOTH_AUDIO_CODEC_APTX` (6) in the `aptx` slot, and `fill_caps = NULL`. Which one owns the endpoint is chosen when the plugin loads, from `APTX_ADAPTIVE_DEFAULT_MODE=hq|ll` (default `hq`; unknown values log an error and use `hq`). `scripts/install-plugin.sh` writes it into the same systemd drop-ins as `APTX_ADAPTIVE_SLOT`. All callbacks are shared. The `quality` prop works on both entries, and a reset (`set_props` with no param) goes back to the entry's own starting mode.

Only the HQ owner reads `bluez5.a2dp.aptx-adaptive.mode`. Every other entry starts in the mode its label says. The legacy setting can therefore make an HQ owner run LL under the plain "aptX Adaptive" label. `APTX_ADAPTIVE_DEFAULT_MODE=ll` selects LL on connect with the correct label.

At a negotiated 96 kHz, LL is unavailable regardless of the entry. Init falls back to HQ, and the node's `quality` property reports HQ without offering LL. The card-profile label remains the selected entry's label because the plugin cannot change PipeWire's profile metadata per transport. Use a 44.1 or 48 kHz preference when LL is wanted (ADR 0005).

### why connect can only bind the owner (PipeWire 1.6.9)

- BlueZ calls `SelectConfiguration` on the one registered endpoint. `endpoint_select_configuration` looks the codec up with `media_endpoint_to_codec(monitor, path, &sink, NULL)` (`bluez5-dbus.c` L795; the comment at L790 says BlueZ-initiated connections "always pick the default one").
- The transport is bound with `media_endpoint_to_codec(..., device->preferred_codec)` (L5342-5343). `preferred_codec` is set only by a codec switch (L4820, L4865) and cleared on every disconnect (`device_clear_sub`, L1836, called from L2061/L2108). On a fresh connection it's NULL.
- With no preferred codec, `media_endpoint_to_codec` skips every entry without `fill_caps` (L543) and takes the first remaining one with that endpoint name. That's the owner.

So the binding on connect is decided by which entry has `fill_caps`, and nothing else. Other candidates, checked:

- **`bluez5.codecs` order**: parsed into a dict and sorted by name (`spa_dict_qsort`, L7286). It only enables entries (`is_media_codec_enabled`, L579); order has no effect.
- **Device props / WirePlumber rules** (`device.profile` in `monitor.bluez.rules`, `device.profile.priority.rules`, a saved profile): these run in WirePlumber's `select-profile` hooks (`find-stored-profile`, `find-preferred-profile`, `find-best-profile`, the last reading `device.profile` at `find-best-profile.lua` L37). That happens after the transport exists. Choosing the alias there is a profile switch: `set_profile` → `spa_bt_device_ensure_media_codec` → a second `SetConfiguration`, i.e. the reconfigure and chime we're avoiding (ADR 0002 has the btmon captures). `bluez5.profile` only knows `off`, `headset-head-unit` and `asha-sink` (`bluez5-device.c` L1964).
- **Device settings in the plugin**: `select_config` and `fill_caps` see only the monitor's global `bluez5.*` settings, never the device's (`endpoint_select_configuration`'s FIXME at L799). And they can't change which entry gets bound anyway, only the config bytes, which are the same for both modes (below).
- **A config file setting the env var**: neither PipeWire 1.6.9 (`context.properties`, `context.spa-libs`, `context.modules`, `context.objects`, `context.exec`; `conf.c` L1413-1423) nor WirePlumber 0.5 has a section that sets the process environment before the bluez5 monitor loads the codecs. The env var has to come from the service environment, i.e. the systemd drop-in we already write.

Hence the swap happens in the plugin, at load time. `codec-loader.c` keeps both entries as long as only one has `fill_caps` (L139-148). `codec_order` sorts the owner's id ahead of the alias (L31-66, `qsort` at L228). The ids are LDAC or LC3plus HR ahead of MPEG in the original slots, and aptX HD ahead of aptX in the `aptx` slot (ADR 0004). The only thing that changes between modes is which of two static entries gets `fill_caps` and the slot id.

### the rest of the PipeWire / BlueZ side

- **One BlueZ endpoint**: codecs without `fill_caps` register none (`endpoint_should_be_registered`, `bluez5-dbus.c` L683-692), so BlueZ sees one `aptx_adaptive` endpoint in either mode and ADR 0002's connect order is unchanged.
- **Profiles** (`bluez5-device.c`): `EnumProfile` walks the ids in `spa_type_bluetooth_audio_codec` (L1868), and `build_profile` takes the first supported codec with that id. Each entry gets its own profile. The bare `a2dp-sink` name goes to the entry with the highest priority (`prio == 0`, L2100), the owner in the `ldac` slot. The alias is named `a2dp-sink-<name>`. In the `lc3plus_hr` slot aptX HD, aptX and AAC outrank both, so both get suffixed names. The owner has the higher priority, so WirePlumber's `find-best-profile` keeps it when nothing is saved. The device's `bluetoothAudioCodec` PropInfo skips aliases only by id (L2668), so pavucontrol's Codec combobox lists both.
- **Support check**: `media_codec_check_caps` calls the entry's `select_config`, which is the same function for both, so both are offered on exactly the same devices.
- **Switching**: `set_profile` → `spa_bt_device_ensure_media_codec` → `codec_switch_configure_a2dp` sets `preferred_codec` to the picked entry (L4820) and calls `SetConfiguration` on the remote SEP with the shared local endpoint. BlueZ 5.87's `a2dp_reconfig` (`profiles/audio/a2dp.c` L2038) closes any open stream and reconfigures without comparing caps. The configuration bytes are the same for HQ and LL, and the switch still happens. When the transport comes back, `media_endpoint_to_codec` picks the preferred entry, and `media-sink` calls `init_props` / `init` with that entry's pointer. The pointer is how the plugin knows the mode.
- **Original slots** have owner id 8 or 13 and alias id 3. The alias has no shipped MPEG codec to collide with, but the `ldac` owner can still collide with LDAC (ADR 0002). The `aptx` slot uses owner id 7 and alias id 6, with ordered aptX passthrough entries (ADR 0004).

### why MPEG for the alias

`bluez5-device.c` only enumerates ids that exist in `enum spa_bluetooth_audio_codec`, and every id there has a name. MPEG (MP3, A2DP codec type 1) is the only A2DP id no PipeWire 1.6.9 plugin implements. Its only id-specific code is the 125 ms latency fallback in `transport_get_delay` (`bluez5-dbus.c` L3701), the same value as LDAC and LC3plus HR. The other free-looking ids have problems:

- `SBC_XQ`, `FASTSTREAM*` and `APTX_LL_DUPLEX` are behind device quirk checks.
- `APTX_LL*`, `AAC_ELD` and `OPUS_*` are used by shipped plugins.
- `LC3` is LE Audio and always enabled.

## trade-offs

- **Profile names depend on the default mode.** In the `ldac` slot, and the `aptx` slot on a headset without LDAC, the owner is plain `a2dp-sink`. Default mode `hq` gives `a2dp-sink` (HQ) and `a2dp-sink-aptx_adaptive_ll`. Mode `ll` gives `a2dp-sink` (LL) and `a2dp-sink-aptx_adaptive`. Changing the default mode changes what a saved `a2dp-sink` means. A saved suffixed name stops existing, so WirePlumber falls back to the new default. In the `lc3plus_hr` slot, or the `aptx` slot on a headset with LDAC, both Adaptive profiles keep their suffixed names. A supported higher-ranked codec connects first. See the profile table in `README.md` and ADR 0004.
- **The default mode is not a WirePlumber setting.** It's an install-time env var, because nothing in the WirePlumber or PipeWire config reaches the codec loader (above). Changing it means re-running the installer, or editing the drop-in, and restarting wireplumber. A per-device default isn't possible: the owner is per process.
- **Cosmetic id.** In the original slots, tools that print the codec id by type name (`pw-dump` device Props, `bluetoothAudioCodec`) say `mpeg` for the alias and `ldac` or `lc3plus_hr` for the owner. The `aptx` slot uses `aptx` for the alias and `aptx_hd` for the owner (ADR 0004). `api.bluez5.codec`, the profile name, and the descriptions use the entry's own name. If PipeWire ever ships an MPEG plugin, it would collide with the original slots' alias id 3, like the LDAC collision in ADR 0002.
- **Picking the non-default profile reconfigures.** `set_profile` removes the nodes and releases the transport, and BlueZ closes and reconfigures the A2DP stream. The result is an audible gap, a new sink node (WirePlumber moves the streams back) and, on the Px7 S2e, probably the disconnect chime. A saved non-default profile costs that once per connect. The live `quality` switch (a fade, under a second of silence, no reconfigure) still works with `pw-cli`.
- **`bluez5.a2dp.aptx-adaptive.mode` is now only for the HQ owner.** It still works there, for compatibility, but the profile is then labelled plain "aptX Adaptive" while it runs LL. `APTX_ADAPTIVE_DEFAULT_MODE=ll` is the way to get LL on connect with the right label.
- **`bluez5.codecs`.** `is_media_codec_enabled` checks each entry's `name`. A custom list must name both `aptx_adaptive` and `aptx_adaptive_ll`. Leaving out the owner's name removes the endpoint, and the alias with it.
- **The level** (1-7) isn't a profile. Seven levels times two modes would be too much clutter; it stays out of the GUIs.

## alternatives

- **A WirePlumber rule or a default profile** (`device.profile`, priority rules): picks the LL profile, but only after the HQ transport exists, so it's a second SetConfiguration and a chime on every connect. That's the problem we're avoiding.
- **Routes or ports per mode**: `bluez5-device.c` has a fixed route set and a codec plugin can't add any.
- **A different config per mode**, so `select_config` could tell them apart: the SetConfiguration carries target latencies for every mode at once, and the mode is chosen per stream by the SSRC (spec `a2dp-integration.md` §7H, §7K). There's no mode field. And connect-time `SelectConfiguration` has no device context to choose from anyway.
- **Patching PipeWire** (a per-device preferred codec that survives disconnect, or `bluez5.codecs` order deciding the owner): the proper fix, and a candidate for the upstream patch ADR 0001 plans. Out of scope for a user install.
- **A plasmoid or tray app that sets the node prop**: live switching without a reconfigure, but a separate install that does nothing for pavucontrol or GNOME.
