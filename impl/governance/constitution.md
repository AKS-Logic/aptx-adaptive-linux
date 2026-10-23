# Constitution

Immutable rules for this project. Every change, plan and task must comply. If a rule
blocks a change, the rule wins and the conflict is recorded in `QUESTIONS.md`.

## Article I — Provenance and clean room

1. Implementation is written **only** from the reference corpus in `impl/protocol/` and the
   vectors in `tests/vectors/`.
2. Vendor binaries, decompiler output, and the quarantine tree are **not** sources.
   They are never read, listed, grepped or cited while implementing.
3. `crates/aptx-adaptive/build.rs` is the only code that may touch a vendor `.so`, and
   only to regenerate and hash-verify `bundled_tables.rs` when `APTX_ADAPTIVE_SO` is set.
4. Vendor `.so`, table dumps, captures and vectors are **never committed**. `impl/` notes and
   `tests/vectors/` are working data, not publications.
5. Public sources are allowed: PipeWire, BlueZ, A2DP, aptX classic/HD in ffmpeg or
   libfreeaptx.

## Article II — Bit-exactness

1. `crates/aptx-adaptive` is safe Rust only, and must stay bit-exact against
   `tests/vectors/`.
2. A change that alters encoder output requires a vector test that proves the new output
   is intended, not merely different.
3. Wire-level claims (frame size, SSRC, RTP timestamp, caps bytes) are evidenced by a
   test or a captured trace, never by inspection alone.

## Article III — Spec-first delivery

1. No behaviour is implemented that a `spec.md` requirement does not describe.
2. Every requirement has an ID; every task cites the requirement it satisfies.
3. A task is complete only when its acceptance scenario is proven by a test or a
   documented hardware observation.
4. `spec.md` changes are reviewed before the code that follows them.

## Article IV — Honest state

1. Telemetry is not audibility. A stream that produces correctly-sized frames with a
   drained backlog is evidence of framing, not proof of sound; say which one is known.
2. Do not attribute a failure to a cause that has not been isolated. Measurements taken
   during a restart, reinstall or reconnect are contaminated and must be re-taken.
3. When a prior claim is contradicted by evidence, correct it explicitly and record the
   correction in `research.md`.

## Article V — Patched system components

1. Locally patched, package-owned files are pinned with `NoUpgrade` in
   `/etc/pacman.conf`: `usr/lib/spa-0.2/bluez5/libspa-bluez5.so` and
   `usr/lib/bluetooth/bluetoothd`.
2. Pinning prevents overwrite, not staleness. After any `pipewire` or `bluez` upgrade,
   `scripts/check-patched-libs.sh` must report `PASS`; if it reports `DRIFT`, run
   `scripts/rebuild-patched-libs.sh`.
3. Only one codec plugin file is active per endpoint, and the installed plugin must hash
   match `target/release/libspa_codec_aptx_adaptive.so`.

## Article VI — Gates before commit

All of the following must pass, with the output quoted in the change description:

| Gate | Command | Required result |
|---|---|---|
| Build | `cargo build --workspace` | success |
| Tests | `cargo test --workspace` | all suites pass |
| Format | `cargo fmt --check` | clean |
| Lint | `cargo clippy --workspace --all-targets` | no new warnings in changed crates |
| Patched stack | `scripts/check-patched-libs.sh` | `PASS` |
| Hardware | `impl/features/*/quickstart.md` | acceptance scenarios observed |

A change that cannot pass a gate is reported as blocked with the failing output, not
described as done.