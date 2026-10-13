# 0004 - slot `aptx`: replace PipeWire's aptX plugin and pass it through

Supersedes ADR 0002's default (`ldac`). The `ldac` and `lc3plus_hr` slots still exist and behave as before.

## problem

The `ldac` slot (ADR 0002) gets aptX Adaptive configured on connect, but it shares id `LDAC` with the real LDAC codec. On a headset with both, only one of them gets a profile, and which one depends on ASLR. The `lc3plus_hr` slot avoids the clash but never wins on connect.

## decision

A third slot, `APTX_ADAPTIVE_SLOT=aptx`, is the new default:

- The plugin is installed as `~/.local/lib/spa-0.2/bluez5/libspa-codec-bluez5-aptx.so`. `SPA_PLUGIN_DIR` puts the user dir first, so PipeWire loads ours instead of the system aptX plugin.
- The aptX Adaptive endpoint owner takes id `SPA_BLUETOOTH_AUDIO_CODEC_APTX_HD` and the alias (ADR 0003) takes `SPA_BLUETOOTH_AUDIO_CODEC_APTX`. With default mode `hq` the owner is `aptx_adaptive` and the alias `aptx_adaptive_ll`. With `ll` it's the other way round.
- At factory init (once per process) the plugin loads the real system `libspa-codec-bluez5-aptx.so` and exports its codecs after ours. Ours come first, then `aptx_hd`, `aptx`, `aptx_ll` ×2, `aptx_ll_duplex` ×2 (PipeWire 1.6.9).
- Codec names stay as they are: ours are `aptx_adaptive` / `aptx_adaptive_ll`, the real ones keep `aptx`, `aptx_hd`, `aptx_ll`…, so `bluez5.codecs` still selects by name.

`ldac` and `lc3plus_hr` keep the LC3plus file and their ids (LDAC or LC3plus HR, alias MPEG).

### which slot runs

`codec-loader.c` loads `bluez5/libspa-codec-bluez5-<name>` and asks for factory `api.codec.bluez5.media.<name>` (L78-91, L180-198). The plugin exports both `api.codec.bluez5.media.aptx` and `api.codec.bluez5.media.lc3plus`. The factory PipeWire asks for tells it which file it was loaded as:

- **aptX file:** always the `aptx` slot. A different `APTX_ADAPTIVE_SLOT` logs an error and is ignored.
- **LC3plus file:** `ldac` or `lc3plus_hr` from the variable, `ldac` if it's unset (as before). `aptx` or a bad value logs an error and uses `ldac`.

So a stale drop-in can't make one file act as the other. `scripts/install-plugin.sh` installs exactly one of the two files and removes the other. `--uninstall` removes both.

## why this works (PipeWire 1.6.9)

### one profile per id, over the codecs the device supports

`bluez5-device.c` builds one A2DP profile per codec **id**. `get_supported_media_codec` (L227) takes the first entry with that id in `supported_codecs`, the codecs this device's remote endpoints accept (`spa_bt_device_get_supported_media_codecs`, `bluez5-dbus.c` L2868). The device's codec PropInfo skips later entries with an id it has already seen ("skip endpoint aliases", L2668).

- **Adaptive headset:** our entries are supported, and they sort before the real `aptx_hd` / `aptx` (below). So the `APTX_HD` and `APTX` profiles are ours, and the real aptX HD and aptX have no profile.
- **Headset without Adaptive:** our `select_config` rejects its caps (vendor/codec id mismatch), so our entries aren't in `supported_codecs`. The first `APTX_HD` and `APTX` entries are the real ones, so profiles, names and priorities are exactly as with the stock plugin.

### why the copy

`codec_order_cmp` (`codec-loader.c` L67-76) sorts by `codec_order` and breaks ties between equal ids by comparing the `media_codec` **pointers** (L75). If our entries and the real ones lived in two `.so` files, their order would depend on where ASLR put each library: the same coin flip ADR 0002 has for LDAC.

So all entries must come from one object with ours at lower addresses. The plugin keeps a single `#[repr(C)]` static, `passthrough::Table`, that holds our two entries followed by an array of `media_codec`. Each real entry is copied **by value** into that array. Field order inside one struct guarantees ours are lower. The copies keep every pointer they had: callbacks, strings, `info`, and `duplex_codec`, which still points at the real `aptx_ll_msbc`.

Copying is only safe if the real code never identifies a codec by its address. In `a2dp-codec-aptx.c` (1.6.9) it doesn't:

- `codec_is_hd` compares `codec->vendor` (L54-58).
- `codec_is_ll` compares `codec->id` (L60-64).
- `fill_caps` and `select_config_ll` read `codec->duplex_codec` (L92, L168).
- There's no `==` against `&a2dp_codec_aptx*` anywhere.

In `bluez5-*.c`, codec pointers are compared only with other pointers taken from the same exported list: `preferred_codec`, `t->media_codec`, `supported_codecs`. All of those are our copies, since PipeWire never sees the originals.

### loading the real plugin

On the first `factory_init`:

1. **Find it.** Look in each directory of `SPA_PLUGIN_DIR`, colon-separated as PipeWire splits it, for `bluez5/libspa-codec-bluez5-aptx.so`. If the variable is unset, look in PipeWire's own default dir, which `build.rs` takes from `pkg-config --variable=plugindir libspa-0.2` (`/usr/lib/spa-0.2` if pkg-config fails). Our own file is skipped by device and inode, found with `dladdr` on `spa_handle_factory_enum`.
2. **Load it.** `dlopen(RTLD_NOW | RTLD_LOCAL)`, find factory `api.codec.bluez5.media.aptx`, and init a handle with the same `info` and `support` array we were given. The real `impl_init` ignores them (`media-codecs.c` L190-214).
3. **Check it.** The `Bluez5CodecMedia` interface version must be 16. PipeWire bumps it whenever `struct media_codec` changes (comment at `media-codecs.h` L23-30), so the version is what guarantees the layout we copy. The content checks catch a plugin that has the version but not the contents we expect:
   - every entry has a name, is A2DP, has the vendor codec type, and has `select_config`, `enum_config`, `init` and `deinit`;
   - `aptx_hd` and `aptx` are present, with their ids and vendor/codec ids, `fill_caps` and `encode`.

   A factory whose description is ours, i.e. another copy of this plugin, is refused rather than nested.
4. **Keep it.** On success nothing calls `dlclose` or `clear`, because the copies point into the real library. The real library stays loaded for the life of the process. The real plugin's handle (64 bytes in 1.6.9; anything outside `size_of::<spa_handle>()..=4096` is refused) is leaked on purpose. `codec-loader.c` only unloads our `.so` when the bluez5 monitor is torn down (`free_media_codecs`, L233-242). If it's loaded again in the same process, its statics start fresh and it loads the real plugin again. So each reload of our `.so` leaks one small handle and adds one `dlopen` reference to the real library. On failure the handle is freed and the library closed.

`dlopen`, `dlsym` and `dladdr` come from libc on glibc ≥ 2.34. The plugin also links `libdl` (`#[link(name = "dl")]`), which is an empty stub there and the real thing on older glibc, so both work.

If any step fails, the plugin logs `aptx-adaptive: aptX/aptX HD unavailable: <reason>` at load and exports only its own two entries. On success it logs `aptx-adaptive: aptX passthrough: 6 codecs from <path>`.

The real plugin doesn't log and has no `set_log` (1.6.9), so nothing is lost by its log topic not being registered.

## consequences

- **Adaptive headsets lose the separate aptX HD and aptX profiles.** That's intended. On those headsets the `APTX_HD` and `APTX` profiles are aptX Adaptive (HQ and LL). aptX-LL, AAC and SBC keep their profiles. The device's Codec PropInfo (pavucontrol's Codec list) shows aptX Adaptive twice and no aptX HD / aptX.
- **Non-Adaptive headsets are unchanged.** Same profiles, same names, same codec code: it's the real plugin's functions running.
- **LDAC is freed.** Nothing shares its id any more, so the LDAC/aptX Adaptive coin flip of ADR 0002 is gone.
- **A headset with LDAC and aptX Adaptive now connects on LDAC.** `codec_order` ranks LDAC above aptX HD (`codec-loader.c` L35-36), so BlueZ configures LDAC first, and LDAC gets the plain `a2dp-sink` name. aptX Adaptive is `a2dp-sink-aptx_adaptive` and `a2dp-sink-aptx_adaptive_ll`, picked by hand. A headset without LDAC connects on aptX Adaptive, as in the `ldac` slot.
- **Profile names** on an Adaptive headset without LDAC are the same as in the `ldac` slot:
  - default mode `hq`: `a2dp-sink` (HQ) and `a2dp-sink-aptx_adaptive_ll`
  - default mode `ll`: `a2dp-sink` (LL) and `a2dp-sink-aptx_adaptive`
- **Latency fallback.** With no delay report, `transport_get_delay` (`bluez5-dbus.c` L3697-3706) uses 125 ms for `APTX_HD` and `APTX`, the same as for LDAC, MPEG and LC3plus HR. Nothing changes from the other slots.
- **Type names.** `spa_type_bluetooth_audio_codec` names id 7 `aptx_hd` and id 6 `aptx` (`type-info.h` L29-30). So `pw-dump`'s device Props print `aptx_hd` for the owner's profile and `aptx` for the alias's. `api.bluez5.codec`, profile names and descriptions use our own name and description (`aptx_adaptive`, "aptX Adaptive"). The same cosmetic issue as `ldac` / `mpeg` in ADRs 0002 and 0003.
- **Without the real plugin** (not installed, or a PipeWire with another codec ABI), aptX and aptX HD disappear for every headset while ours is installed, and the error says so. `install-plugin.sh` warns at install time if the system plugin dir has no aptX plugin.
- **Headset-initiated aptX HD is a regression** against the `ldac` slot, mitigated by a WirePlumber guard script that the installer adds for this slot. See "risks found in the source" below.
- **PipeWire upgrades.** A new PipeWire with codec ABI 16 and another set of aptX entries is passed through as is. A new ABI version makes the real plugin unavailable (logged) until this plugin is rebuilt. Our own entries would be rejected by the codec loader then anyway.

## risks found in the source

### regression: headset-initiated configuration on the real aptX HD

The device's current profile comes from the running transport's **id** (`this->props.codec = t->media_codec->id`, `bluez5-device.c` L1279). Suppose an aptX Adaptive headset configures the real `aptx_hd` endpoint itself, with BlueZ only passing the headset's SetConfiguration on to PipeWire.

- The transport's id is 7. On that headset id 7 is our aptX Adaptive owner profile, so the card shows "aptX Adaptive" while aptX HD runs. The sink node's `api.bluez5.codec` still says `aptx_hd`.
- WirePlumber then restores the saved profile, `a2dp-sink`, which is the same id. `set_profile` returns early when profile and id match (L1442-1447), so nothing reconfigures and the stream stays on aptX HD for the whole connection.

**In the `ldac` slot this recovered.** Our id was 8 there, and a headset-initiated aptX HD transport showed up as the real `a2dp-sink-aptx_hd` profile. WirePlumber's restore of the saved `a2dp-sink` (id 8) differed by id, so it reconfigured to aptX Adaptive at the cost of one chime. That's a regression of the `aptx` slot.

How likely it is:
- The Px7 S2e never initiated in our captures; the host always did. Host-initiated connects take our endpoint first, because it's registered first (ADR 0002), so they're unaffected.
- A OnePlus phone captured as an A2DP source did have its sink pick aptX HD remotely, so some headsets will initiate.
- The same state can come from a `bluez5.codecs` list that leaves out `aptx_adaptive` but keeps `aptx_hd`. In that case the guard doesn't fire, since our profile isn't offered at all.

Manual recovery: pick the other aptX Adaptive profile and then the wanted one back. Each step differs by id from the previous one, so each reconfigures, and the second lands on our entry. Picking the same profile again does nothing, and disconnecting and reconnecting from the host also works.

**Mitigation: a WirePlumber guard** (`wireplumber/aptx-adaptive-guard.lua`, installed for slot `aptx` only). It does the same thing automatically, once per connection:

- **Triggers:** a bluez5 `Audio/Sink` node with `api.bluez5.codec` = `aptx_hd` or `aptx` appears, or the device's `EnumProfile`/`Profile` params change. After 1.5 s, so WirePlumber's own profile restore has run, the guard checks the state.
- **Condition:** the current profile is A2DP (`index >> 16 == 2`, `get_index_from_profile`, `bluez5-device.c` L1883-1900), its codec id (`index & 0xffff`) matches the running real codec (7 for `aptx_hd`, 6 for `aptx`), and its description contains "aptX Adaptive" (our entry's description, put there by `build_profile`, L2104). A headset without aptX Adaptive never has our entries among its profiles (`get_supported_media_codec` only takes supported entries, L227-257), so its aptX HD profile reads "aptX HD" and is never touched.
- **Action:**
  1. Set `Profile` to index 0 (`off`). That releases the transports (L1449-1451).
  2. Set `Profile` back to the same index, with `save = false`.
  - The second set differs from `off`, so `set_profile` runs `spa_bt_device_ensure_media_codec` with every supported entry of that id in order (`get_media_codecs`, L207-225; `bluez5-dbus.c` L5092-5117). The first one is ours, so it reconfigures to aptX Adaptive.
  - It logs `… runs aptx_hd under profile 'a2dp-sink' (…aptX Adaptive); re-selecting it via 'off' to switch to aptX Adaptive` at info level, topic `s-aptx-adaptive`.
- **Rate limit:** one attempt per device per connection, keyed by the device's bound id and cleared on `device-removed`, so it can't loop even if the switch fails.

Why "off" and not the alternatives:
- **Device `Props` `bluetoothAudioCodec`:** it's keyed by id. `impl_set_param` looks up the id among the supported codecs and calls `set_profile(this, this->profile, codec_id, true)` (L3243-3250). With the same id that's the early return, so it's a no-op.
- **The other aptX Adaptive profile and back:** this works, but it's two codec reconfigures (two SetConfigurations, probably two chimes). The intermediate one starts a stream in the other mode.
- **Via `off`:** a transport release plus one SetConfiguration. It's the cheapest path through `set_profile` that ends in a codec switch.

Live test pending: no headset that initiates the configuration has been tried with the guard. Offline, `wireplumber/test-guard.lua` drives the hook with stubbed WirePlumber objects (run by `cargo test`), and the script loads and registers its hooks in WirePlumber 0.5.18.

### other

- **`bluez5.codecs`.** A list that names `aptx_hd` but not `aptx_adaptive` disables our entries by name, and the real aptX HD then gets its profile back on Adaptive headsets too. That's fine. A list with `aptx_adaptive` but without `aptx_hd` / `aptx` removes those codecs for non-Adaptive headsets, as it would with the stock plugin.
- **Stale LC3plus file (minor).** Say a previous `ldac` or `lc3plus_hr` install leaves `libspa-codec-bluez5-lc3plus.so` next to the new aptX file. PipeWire loads both, the aptX file first (`codec-loader.c` L184, L188).
  - The LC3plus copy sees `APTX_ADAPTIVE_SLOT=aptx` from the drop-in, logs that it doesn't fit, and runs the `ldac` slot. That adds a second `aptx_adaptive` owner and an MPEG alias.
  - The loader drops the second owner as a duplicate endpoint, since both have `fill_caps` (L139-148). It keeps the alias, which has none.
  - That orphan is an extra aptX Adaptive profile under id 3 (MPEG). Picking it switches to our endpoint with the alias's mode, so it works, but it has the same name and label as the real alias profile (e.g. two `a2dp-sink-aptx_adaptive_ll`).
  - `install-plugin.sh` prevents this by removing the other file on every install.

## alternatives

- **Keep the `ldac` slot as default:** loses LDAC on headsets with both codecs, nondeterministically.
- **Take the aptX ids without passing the real plugin through:** aptX and aptX HD would vanish for every headset.
- **Pass the real entries' original pointers through, instead of copies:** the tie-break would compare addresses in two libraries, which ASLR decides per load.
- **Wrap each real callback:** the copy already runs the real code unchanged, and nothing in it depends on the entry's address, so wrappers would add surface without fixing anything.
