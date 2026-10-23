# Implementation Plan: Three aptX Adaptive profiles

**Feature**: `001-three-adaptive-profiles`
**Behaviour**: [spec/features/001-three-adaptive-profiles/spec.md](../../../spec/features/001-three-adaptive-profiles/spec.md)
**Date**: 2026-10-22

## Summary

Expose the existing aptX Adaptive encoder as three card profiles with distinct bitrate
policies, and make the RTP envelope conform to the negotiated source type so a sink can
actually schedule the stream. HQ ships as its own plugin file because a third endpoint
inside one plugin file is never registered by BlueZ.

## Technical Context

| Item | Value |
|---|---|
| Language | Rust 2024, MSRV 1.99 (`crates/*`) |
| Codec plugin | `spa-codec-aptx-adaptive` (cdylib, SPA bluez5 media codec) |
| Encoder | `aptx-adaptive` crate (safe Rust, table-driven, bit-exact vs vectors) |
| Host stack | PipeWire 1.6.9 (patched), WirePlumber, BlueZ 5.87 (patched `bluetoothd`) |
| Sink target | ribcage QA Ultra (R1, 48 kHz, stereo + joint stereo, no 96 kHz bit) |
| Testing | `cargo test --workspace` (unit + dlopen integration), `tests/vectors/` |
| Hardware verification | `btmon` frame captures, `pactl`, `journalctl`, `scripts/check-patched-libs.sh` |
| Platform | Linux, PipeWire bluez5 SPA plugin ABI |

### Constraints

- The A2DP codec id space is fixed: `0x301` adaptive, `0x302` LL, `0x303` Hi-Res/HQ.
  Anything not present in `spa_type_bluetooth_audio_codec` cannot become a card profile.
- One plugin file yields one set of endpoints; a third endpoint in the same file is not
  registered, hence the separate HQ plugin file.
- Source type 0 (ST0) forces SSRC `0xAE` and microsecond timestamps. Section §7F latency
  mode SSRCs (`0xA1`/`0xA3`) are used only when ST0 is off.
- PipeWire's `apply_level_now()` refuses while the stream holds its start mute, so a
  queued start level must be landed at the first packet boundary.

## Constitution Check

| Article | Gate | Status |
|---|---|---|
| I — clean room | no vendor artefact read or committed; tables via `bundled_tables.rs` | PASS |
| II — bit-exactness | `cargo test --workspace`; vectors unchanged | PASS |
| III — spec-first | requirements `REQ-001..012` precede implementation; tasks cite them | PASS |
| IV — honest state | audible confirmation obtained before claiming done | PASS |
| V — patched components | `NoUpgrade` pinning + `check-patched-libs.sh` | PASS |
| VI — gates | build, test, fmt, clippy, patched stack, hardware | PASS |

## Components affected

```text
crates/spa-codec-aptx-adaptive/
├── src/ids.rs            # HQ name/description/endpoint; factory name
├── src/ffi.rs            # SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE (0x301)
├── src/dev.rs            # Options: pin_level, start_level
├── src/session.rs        # level policy at open; pending-level landing; set_level_policy
├── src/lib.rs            # three-entry table, HQ-only factory, per-entry level policy,
│                         # optional in-band mode file
├── src/passthrough.rs    # Table3 (three of our endpoints + real plugin codecs)
└── tests/dlopen.rs       # factory/entry contract, HQ factory export

audio/pipewire (patched, outside this repo)
├── spa/plugins/bluez5/codec-loader.c                    # plugin table: + aptx_adaptive_hq
└── spa/include/spa/param/bluetooth/type-info.h          # 0x303 row -> aptx_adaptive_hq

system configuration
├── /etc/pacman.conf                                     # NoUpgrade for two patched files
├── ~/.config/systemd/user/*.service.d/aptx-adaptive.conf # ST0, slot, levels, sandbox
├── ~/.local/bin/aptx-linkclock-inner.sh                 # "<monotonic_us> <clock>", 5 Hz
└── ~/.config/wireplumber/wireplumber.conf.d/10-silencer-bluez.conf  # codec allow-list
```

## Approach

1. **Profiles as distinct endpoints.** Three entries, three ids, three endpoints. The
   shared file exports adaptive + LL; the HQ entry is exported by a dedicated factory and
   installed as a second copy of the same binary under its own filename.
2. **Level policy per entry.** `codec_init` reads the entry name and applies
   `pin_level` (HQ → 5, `abr = false`) or `start_level` (adaptive → 2 when
   `APTX_ADAPTIVE_START_LOW` is set). Default two-entry behaviour is unchanged, so the
   existing test contract still holds.
3. **Correct envelope.** ST0 must be enabled for the wire format to match what the sink
   expects; without it the packets carry a frozen sample-count timestamp.
4. **Nothing hidden.** The optional mode file cannot override the card profile unless
   `APTX_ADAPTIVE_MODE_FILE` is set.

## Risks

| Risk | Mitigation |
|---|---|
| Upstream upgrade overwrites patched libs | `NoUpgrade` + `check-patched-libs.sh` |
| Pinned lib becomes ABI-stale | `rebuild-patched-libs.sh` after upgrades |
| Third endpoint silently dropped again | HQ shipped as its own plugin file; loader table patched; `type-info` row added |
| Test contract churn | three-entry export gated; default remains two entries |

## Out of scope

96 kHz producer, panel-driven in-band switching, non-ribcage sinks as verified targets.