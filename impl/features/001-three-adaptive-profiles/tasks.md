# Tasks: Three aptX Adaptive profiles

**Input**: behaviour [spec.md](../../../spec/features/001-three-adaptive-profiles/spec.md), design [design.md](./design.md), research [research.md](./research.md)

Format: `[ID] [P?] [Story] Description` — `[P]` can run in parallel, `[USn]` the user story.
Each task cites the requirement it satisfies. Tasks are complete only when their acceptance
scenario is proven (test or documented hardware observation).

## Phase 1 — Setup

- [x] T001 [US1,US2,US3] Add HQ identifiers (`aptx_adaptive_hq`, description, endpoint) and the
      `0x301` adaptive codec id — `crates/spa-codec-aptx-adaptive/src/ids.rs`, `src/ffi.rs` (REQ-001, REQ-002, REQ-003)
- [x] T002 [US1,US2,US3] Add a three-entry passthrough table so three of our entries can be
      exported alongside the stock plugin's codecs — `src/passthrough.rs` (REQ-003)
- [x] T003 [US2] Ship HQ from a dedicated factory `api.codec.bluez5.media.aptx_adaptive_hq`
      and export it from `spa_handle_factory_enum` — `src/lib.rs` (REQ-002, REQ-003)

## Phase 2 — Foundational

- [x] T004 [US2,US3] Add `pin_level` / `start_level` to `dev::Options` — `src/dev.rs` (REQ-004, REQ-005)
- [x] T005 [US2,US3] Apply the level policy at session open, and pin disables the ladder —
      `src/session.rs` (REQ-004, REQ-005)
- [x] T006 [US2,US3] Derive the policy per entry from the codec name in `codec_init`,
      gating the adaptive start level behind `APTX_ADAPTIVE_START_LOW` — `src/lib.rs` (REQ-004, REQ-005)
- [x] T007 [US3] Land a queued start level at the first packet boundary, because
      `apply_level_now()` refuses during the start mute — `src/session.rs` (REQ-005)
- [x] T008 [US1,US2,US3] Send SSRC `0xAE` with advancing microsecond timestamps by enabling
      ST0 in the service environment — drop-in `aptx-adaptive.conf` (REQ-007)
- [x] T009 [US1,US2,US3] Fall back to monotonic seeding when the clock file is absent or
      malformed; publish `"<monotonic_us> <clock>"` and poll at 5 Hz —
      `~/.local/bin/aptx-linkclock-inner.sh` (REQ-008)

## Phase 3 — Patched stack

- [x] T010 [US2] Add `aptx_adaptive_hq` to the loader's plugin table — `pipewire/spa/plugins/bluez5/codec-loader.c` (REQ-003)
- [x] T011 [US2] Add the `0x303` row to the codec type-info table so the profile can be
      enumerated — `pipewire/spa/include/spa/param/bluetooth/type-info.h` (REQ-002)
- [x] T012 [US2] Update the BlueZ allow-list and profile priorities to `aptx_adaptive_hq` —
      `~/.config/wireplumber/wireplumber.conf.d/10-silencer-bluez.conf` (REQ-001)
- [x] T013 [US5] Pin the patched package-owned files with `NoUpgrade` — `/etc/pacman.conf` (REQ-011)
- [x] T014 [US5] Add `scripts/check-patched-libs.sh` and `scripts/rebuild-patched-libs.sh` (REQ-011, REQ-012)

## Phase 4 — Control and safety

- [x] T015 [US3] Add optional in-band mode switching via a control file, inert unless
      `APTX_ADAPTIVE_MODE_FILE` is set — `src/lib.rs` (REQ-009)
- [x] T016 [US4] Verify profile switching keeps the link up; record the correction that an
      earlier disconnect attribution was wrong — see research R9 (REQ-010)
- [x] T017 [US2,US3] Remove the `ported` factory export (malformed-request source) and its
      obsolete test — `src/lib.rs`, `tests/dlopen_ported.rs` (REQ-001)

## Phase 5 — Verification

- [x] T018 [US1,US2,US3] Unit and integration tests for the entry contract and level policy —
      `tests/dlopen.rs`, `src/session.rs` (REQ-001..REQ-007)
- [x] T019 [US1] Hardware: LL sends 348-byte payloads (360 on the wire), backlog drained (REQ-001, REQ-007)
- [x] T020 [US2] Hardware: HQ sends 656-byte payloads (668 on the wire), `level 5` held, zero
      ladder transitions (REQ-004)
- [x] T021 [US3] Hardware: adaptive logs `3 -> 2` at open and `2 -> 3 -> 4 -> 5` (REQ-005, REQ-006)
- [x] T022 [US4] Hardware: switching all three profiles logs zero `dev_disconnected`; audio
      audible on each — operator confirmed (REQ-010)
- [x] T023 [US5] `scripts/check-patched-libs.sh` reports `PASS` (REQ-011)
- [x] T024 Gates: `cargo test --workspace` (7 suites), `cargo fmt --check` clean (Constitution VI)

## Open

- [x] T029 [US1] Verify LL is the lowest latency available: measured 10.0 ms / 360 B vs HQ
      12.0 ms / 672 B, and confirm the 8 ms row requires enhanced-LL support the sink lacks —
      research R12 (REQ-001, REQ-007)
- [ ] T025 Measure long-duration stability (multi-hour) and record backlog/frame counts
- [ ] T026 Exercise the 96 kHz path on hardware advertising bit 5 (blocked: no such sink available)
- [ ] T027 Verify a second sink model to widen the supported-target claim
- [ ] T028 Decide whether the in-band mode file should be surfaced in the desktop UI or stay CLI-only