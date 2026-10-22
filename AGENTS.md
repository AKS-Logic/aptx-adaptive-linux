# aptx-linux

Clean-room aptX Adaptive encoder in Rust, plus a PipeWire bluez5 codec plugin. See `docs/adr/0001-architecture.md`.

## spec-driven development (hard)

- This repo is developed spec-first. Read `spec/README.md` for the layout and workflow,
  and `spec/constitution.md` for the rules every change must satisfy.
- Behaviour is agreed in a `spec/features/NNN-slug/spec.md` requirement (`REQ-nnn`) before
  it is implemented. A task cites the requirement it satisfies; a behaviour with no
  requirement is not agreed even if the code already does it.
- Each feature directory holds `spec.md` (what/why, EARS requirements, Given/When/Then
  acceptance), `plan.md` (how), `research.md` (decisions and evidence), `tasks.md`
  (ordered, traceable work), `quickstart.md` (hardware verification).
- The `spec/*.md` files at the root are the reference corpus (protocol, encoder, research).
  Do not move them into a feature directory.
- Record measurements as evidence, with how they were obtained. Telemetry is not
  audibility; say which one a claim rests on.

## clean room rules (hard)

- Implement only from `spec/` and `tests/vectors/`.
- Never read, list or grep the `aptx-re-quarantine` repo (wherever it's checked out) or any vendor `.so` / decompiled output. Don't use IDA.
- The reconstructed codec tables live in `crates/aptx-adaptive/bundled_tables.rs` (see `LEGAL.md`). `crates/aptx-adaptive/build.rs` can regenerate and hash-verify them from a user-supplied vendor `.so` when `APTX_ADAPTIVE_SO` is set; that build script is the only thing that touches the file. Never hexdump it, and never print, log, test against or commit new table values.
- If the spec is ambiguous or wrong, write the question in `spec/QUESTIONS.md` and move on. The coordinator gets it answered by the dirty side.
- Public sources are fine: PipeWire source, BlueZ, the A2DP spec, aptX classic/HD in ffmpeg or libfreeaptx.

## dev

- tools come from mise: `mise install`, `mise run check`
- the build needs `libaptXAdaptiveEnc.so` (README "install"). mise points `APTX_ADAPTIVE_SO` at `.scratch/libaptXAdaptiveEnc.so`; override with an absolute path.
- `crates/aptx-adaptive`: safe Rust only. Bit-exact against the vectors.

## scratch

- temporary and working files go in `.scratch/` in the repo (gitignored), never `/tmp`. Never commit them.
