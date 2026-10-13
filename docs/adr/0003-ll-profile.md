# 0003 - low latency as a card profile

## problem

HQ or low latency is a codec node prop (`quality`, like LDAC's). No GUI shows codec node props. KDE (settings and tray), pavucontrol and pwvucontrol show **card profiles**, and pavucontrol also has a Codec combobox built from the device's `bluetoothAudioCodec` PropInfo. Both lists have one entry per codec **id**. So LL could only be reached with `pw-cli` or a WirePlumber drop-in.

## decision

The plugin exports a second `media_codec`, a shared-endpoint alias of the main one, the same pattern PipeWire uses for `sbc` / `sbc_xq`:

| | main | alias |
|---|---|---|
| `name` | `aptx_adaptive` | `aptx_adaptive_ll` |
| `description` | aptX Adaptive | aptX Adaptive (Low Latency) |
| `id` | the slot's (LDAC or LC3plus HR, ADR 0002) | `SPA_BLUETOOTH_AUDIO_CODEC_MPEG` (3) |
| `endpoint_name` | `aptx_adaptive` | `aptx_adaptive` |
| `fill_caps` | yes | **NULL** |

All other fields and callbacks are the same. The alias is listed after the main entry in both slot lists. A stream created through the alias starts in LL. A stream created through the main entry starts in HQ, or in LL if the device sets `bluez5.a2dp.aptx-adaptive.mode = "ll"`. The `quality` prop works on both, so either profile can still be switched live with `pw-cli`. A reset (`set_props` with no param) goes back to the profile's own starting mode.

Checked against PipeWire 1.6.9 and BlueZ 5.87:

- **Loader** (`codec-loader.c`, `load_media_codecs_from`): an entry is dropped as a duplicate endpoint only if both it and the earlier one have `fill_caps`. The alias loads.
- **One BlueZ endpoint** (`bluez5-dbus.c`, `endpoint_should_be_registered`): codecs without `fill_caps` register no endpoint, so BlueZ sees only `aptx_adaptive`, and ADR 0002's connect order is unchanged.
- **Connect still picks the main entry**: `endpoint_select_configuration` and the transport setup use `media_endpoint_to_codec(..., preferred)`. With no preferred codec it skips entries without `fill_caps`, and `preferred_codec` is cleared on disconnect (`device_clear_sub`). A fresh connection therefore binds to the main entry.
- **Separate profile** (`bluez5-device.c`): `EnumProfile` walks the ids in `spa_type_bluetooth_audio_codec`, and `build_profile` takes the first supported codec with that id. The alias's id is its own, so it gets `a2dp-sink-aptx_adaptive_ll`. Its priority is lower than the main entry's because `codec_order` ranks MPEG below LDAC and LC3plus HR, so `find-best-profile` still prefers HQ. The device's `bluetoothAudioCodec` PropInfo skips aliases only by id, so pavucontrol's Codec combobox lists it too.
- **Support check**: `media_codec_check_caps` calls the alias's `select_config`, which is the same function, so the alias is offered on exactly the devices where the main entry is.
- **Switching**: `set_profile` → `spa_bt_device_ensure_media_codec` → `codec_switch_configure_a2dp` sets `preferred_codec` to the alias and calls `SetConfiguration` on the remote SEP with our shared local endpoint. BlueZ's `a2dp_reconfig` closes any open stream and reconfigures without comparing caps. The configuration bytes are the same for HQ and LL, which is fine because the switch still goes through. When the transport comes back, `media_endpoint_to_codec` picks the preferred entry, and `media-sink` calls `init_props` / `init` with the alias pointer. That pointer is how the plugin knows to start in LL.
- **Both slots**: the alias doesn't depend on the slot. With `APTX_ADAPTIVE_SLOT=lc3plus_hr` the main entry is id 13 and the alias id 3. `codec_order` puts them next to each other (LC3plus HR, then MPEG), both below aptX HD, aptX and AAC. Nothing conflicts.

### why MPEG

`bluez5-device.c` only enumerates ids that exist in `enum spa_bluetooth_audio_codec`, and every id there has a name. MPEG (MP3, A2DP codec type 1) is the only A2DP id no PipeWire 1.6.9 plugin implements. Its only id-specific code is the 125 ms latency fallback in `transport_get_delay`, the same as LDAC and LC3plus HR. The other free-looking ids have problems:

- `SBC_XQ`, `FASTSTREAM*` and `APTX_LL_DUPLEX` are behind device quirk checks.
- `APTX_LL*`, `AAC_ELD` and `OPUS_*` are used by shipped plugins.
- `LC3` is LE Audio and always enabled.

## trade-offs

- **Cosmetic id.** Tools that print the codec id by type name (`pw-dump` device Props, `bluetoothAudioCodec`) say `mpeg` for the LL profile, just as they say `ldac` for the main entry (ADR 0002). `api.bluez5.codec`, the profile name and the descriptions say `aptx_adaptive_ll` / "aptX Adaptive (Low Latency)". If PipeWire ever ships an MPEG plugin, both would have id 3 and only one gets a profile, like the LDAC collision in ADR 0002.
- **Switching profile reconfigures the stream.** `set_profile` removes the nodes, releases the transport, and BlueZ closes and reconfigures the A2DP stream. That means an audible gap, a new sink node (WirePlumber moves the streams back) and, on the Px7 S2e, probably the disconnect chime from ADR 0002. The live `quality` prop switch (a fade, under a second of silence, no reconfigure) is still there with `pw-cli` for anyone who wants to avoid that.
- **An extra reconfigure on connect for LL users.** BlueZ always configures the main entry on connect, and WirePlumber then restores a saved `a2dp-sink-aptx_adaptive_ll`, which is one more reconfigure (and chime). To start in LL without it, keep the main profile and set `bluez5.a2dp.aptx-adaptive.mode = "ll"`. The main profile then starts in LL too, and its description still says "aptX Adaptive", so the label doesn't show the mode.
- **`bluez5.codecs`.** `is_media_codec_enabled` checks each entry's `name`. A custom `bluez5.codecs` list must name `aptx_adaptive_ll` to get the profile. Naming it without `aptx_adaptive` gives a profile with no endpoint behind it, so it never appears.
- **The level** (1-7) isn't a profile. Seven levels times two modes would be too much clutter; it stays out of the GUIs.

## alternatives

- Routes or ports per mode: `bluez5-device.c` has a fixed route set and a codec plugin can't add any.
- A plasmoid or tray app that sets the node prop: live switching without a reconfigure, but a separate install that does nothing for pavucontrol or GNOME. Maybe later, on top of this.
- A different config per mode, so BlueZ's connect-time choice could already be LL: the SetConfiguration carries target latencies for every mode at once and the mode is chosen per stream by the SSRC (spec `a2dp-integration.md` §7H, §7K). There's no mode field to tell the two apart, and connect-time `SelectConfiguration` has no device context to choose from anyway.
