# 0002 - configure aptX Adaptive first on connect

**Status: default superseded by [ADR 0004](0004-aptx-slot.md).** The default slot is now `aptx`, which takes the aptX HD and aptX ids and passes PipeWire's real aptX plugin through, with no LDAC collision. The `ldac` slot described here still works with `APTX_ADAPTIVE_SLOT=ldac`, unchanged. Where this ADR says "default", read "the `ldac` slot". The analysis of how BlueZ picks the first codec still holds.

## problem

On connect the host configured aptX HD (Set Configuration on the Px7's SEID 7). WirePlumber then restored the saved `a2dp-sink-aptx_adaptive` profile, so PipeWire closed that stream and reconfigured SEID 9. The headset plays its disconnect chime on every such switch (`spec/QUESTIONS.md`, "clean-path test").

## how the first codec is chosen (BlueZ 5.87, PipeWire 1.6.9, WirePlumber 0.5)

- **BlueZ** picks it, before PipeWire or WirePlumber know about the device. `a2dp_select_capabilities` → `a2dp_select_eps` (`profiles/audio/a2dp.c`) walks the local source endpoints in registration order, keeps those with a matching remote SEP, and moves the `LastUsed` pair to the front. Then it calls `SelectConfiguration` on the first one.
- The registration order is PipeWire's codec list order: `GetManagedObjects` in `bluez5-dbus.c` iterates `monitor->media_codecs`, which `codec-loader.c` has sorted with `codec_order` (LC3, LDAC, aptX HD, aptX, AAC, Opus G, LC3plus HR, ...).
- `LastUsed` doesn't help on 5.87: `load_remote_sep`'s hex loop (`i < size && i >= 2`) never runs, so every cached endpoint is dropped and the log says `Unable to load LastUsed: rseid 9 not found` on each connect. Even if it worked, it would only follow the last stream.
- PipeWire's `endpoint_select_configuration` has no device context (its own FIXME), so no per-device setting reaches this point. `bluez5.profile` only knows `off`, `headset-head-unit` and `asha-sink`.
- WirePlumber's profile hooks (`find-stored-profile`, `find-preferred-profile`, `device.profile.priority.rules`, `bluetooth.profile-preference`) run on `select-profile`, after the transport exists. Whatever they choose, if it isn't the codec BlueZ already set up, it's a switch. The hook names come from the installed WirePlumber 0.5.18 Lua scripts. That they run too late is from our own btmon captures (Set Configuration of aptX HD, then Close and a reconfigure to aptX Adaptive), not from reading WirePlumber's C source.

So no WirePlumber rule or device property can make the first configuration aptX Adaptive. The options left are the codec order itself, or `bluez5.codecs`.

## decision

The plugin claims `SPA_BLUETOOTH_AUDIO_CODEC_LDAC` (8) as its `id` by default, instead of `LC3PLUS_HR` (13). `codec_order` ranks LDAC second, above aptX HD, so our endpoint is registered before aptX HD and BlueZ configures aptX Adaptive directly on any device that has it. Devices without aptX Adaptive have no matching remote SEP, BlueZ skips our endpoint, and nothing changes for them.

The file name and factory stay `libspa-codec-bluez5-lc3plus.so` / `api.codec.bluez5.media.lc3plus` (ADR 0001); only the `id` moves. The id is chosen at factory init from `APTX_ADAPTIVE_SLOT=ldac|lc3plus_hr`, which `scripts/install-plugin.sh` writes into the wireplumber/pipewire drop-ins. When the plugin is loaded as the LC3plus file, an unset variable still means `ldac`, and an unknown value logs an error and falls back to `ldac`. (Since ADR 0004 the installer's default is `aptx`, which uses the aptX file instead.)

A process that loads the LC3plus file **without** our drop-in gets the LDAC slot silently, since that needs no variable. That covers anything that loads the bluez5 monitor outside `wireplumber.service`/`pipewire.service` but still finds our plugin, e.g. a `SPA_PLUGIN_DIR` set some other way. This is intended: the default is the behaviour we want. Only the `lc3plus_hr` choice depends on the drop-in reaching the process.

To go back to the old ranking, or to remove everything:

```
APTX_ADAPTIVE_SLOT=lc3plus_hr scripts/install-plugin.sh   # old ranking
scripts/install-plugin.sh --uninstall                     # plugin, drop-ins, dev-replay drop-in
systemctl --user restart wireplumber pipewire
```

## trade-offs

- **Profile names change.** PipeWire gives the bare `a2dp-sink` name to the best-ranked codec. With the LDAC slot, `a2dp-sink` is aptX Adaptive and aptX HD becomes `a2dp-sink-aptx_hd`. A saved `a2dp-sink-aptx_adaptive` no longer exists, so WirePlumber falls back to the best profile, which is `a2dp-sink`: aptX Adaptive again, no switch. Switching back to `lc3plus_hr` makes a saved `a2dp-sink` mean aptX HD.
- **aptX Adaptive wins automatically.** At the time of this decision, stutter and LL support were still open. Later tests confirmed clean HQ playback and runtime HQ → LL → HQ switching (`spec/QUESTIONS.md`, October 13 live tests). Anyone who wants the separate aptX HD profile uses the `lc3plus_hr` or `ldac` slot. The default `aptx` slot replaces that profile on Adaptive headsets (ADR 0004).
- **LDAC collision, and it isn't deterministic.** `libspa-codec-bluez5-ldac.so` is installed here, and both codecs then have id 8. They register as separate endpoints (`ldac`, `aptx_adaptive`), but `bluez5-device.c` keys profiles and the codec property by id (`get_supported_media_codec`, "skip endpoint aliases"). So on a headset with **both** LDAC and aptX Adaptive, only the first of the two in the codec list gets a profile, and BlueZ configures that one on connect. `codec_order_cmp` breaks ties between equal ids by comparing the `media_codec` pointers, which live in two different `.so` files. With ASLR their relative addresses change on each load, so which codec wins can change every time WirePlumber restarts.
  - The plugin can't detect this and back off. `select_config` and `check_caps` only see the caps of one remote endpoint, never the device's other endpoints, so it can't tell whether the headset also has LDAC.
  - The Px7 S2e has no LDAC, so nothing collides for it. **For a headset with both codecs, use the `aptx` slot (ADR 0004, now the default) or `lc3plus_hr`**, neither of which shares an id with LDAC. Leaving `ldac` out of `bluez5.codecs` also works, but removes LDAC everywhere. `install-plugin.sh` prints a note when it installs the `ldac` slot next to an installed LDAC plugin.
- **Type info.** `spa_type_bluetooth_audio_codec` names id 8 `ldac`, so tools that print the `bluetoothAudioCodec` prop id by type name (e.g. `pw-dump` Props) say `ldac`. `api.bluez5.codec`, the profile names and descriptions use our `name`/`description` and say `aptx_adaptive` / "aptX Adaptive".
- **Latency fallback.** The only other id-specific path, `transport_get_delay`'s default when the device sends no delay report, is 125 ms for both LDAC and LC3plus HR.

## alternatives

- WirePlumber rule: not possible, see above.
- The LC3 id (`SPA_BLUETOOTH_AUDIO_CODEC_LC3`) ranks even higher, but PipeWire treats it as the LE Audio codec. `is_media_codec_enabled` always enables it, whatever `bluez5.codecs` says. BAP code paths default to it (e.g. the BAP profile's initial codec in `bluez5-device.c`), and `transport_get_delay` gives it a 40 ms latency fallback instead of 125 ms. Borrowing it would mix our A2DP codec into LE Audio handling.
- LHDC ranks above aptX HD on some other stacks, but PipeWire 1.6.9 has no LHDC id in `enum spa_bluetooth_audio_codec` or `codec_order`, so there's nothing to borrow.
- `bluez5.codecs` without `aptx_hd`, `aptx`, `aac`: works with stock ids, but takes those codecs away from every device, not only aptX Adaptive ones.
- Patching BlueZ (`LastUsed`, or a preferred-endpoint property) or PipeWire (a real codec id ranked above aptX HD): the proper fix, and part of the upstream PipeWire patch ADR 0001 plans. Out of scope for a user install.
