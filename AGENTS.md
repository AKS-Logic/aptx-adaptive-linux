# aptx-linux

Clean-room aptX Adaptive encoder in Rust, plus a PipeWire bluez5 codec plugin. See `docs/adr/0001-architecture.md`.

## spec and impl (hard)

Two folders, two owners, two kinds of content. Do not mix them.

- **`spec/` is the behaviour contract, owned by the project owner.** It says what the software
  is supposed to do, in domain language: modes and what each optimises, the flows a user goes
  through, observable expectations, acceptance scenarios, non-goals. It never contains file
  names, function names, byte layouts, service units, environment variable plumbing, or our own
  measurements. Read `spec/README.md`. Changes there express intent, and they come before code.
- **`impl/` is the implementation record, owned by the agent.** It holds everything about how
  the behaviour is produced: architecture, the protocol reference corpus, design decisions with
  evidence, task lists, verification commands. Read `impl/README.md` and, before changing code,
  `impl/governance/constitution.md` — it is binding, including the pre-commit gates.
- A behaviour is agreed as a `REQ-nnn` in `spec/features/NNN-slug/spec.md` before it is
  implemented. A task in `impl/features/NNN-slug/tasks.md` cites the requirement it satisfies.
  Behaviour with no requirement is not agreed, even if the code already does it.
- When behaviour changes: update `spec/` first, then `impl/`, then code.
- Record measurements in `impl/` with how they were obtained. Telemetry is not audibility; say
  which one a claim rests on.

## clean room rules (hard)

- Implement only from `impl/protocol/` (the reference corpus) and `tests/vectors/`.
- Never read, list or grep the `aptx-re-quarantine` repo (wherever it's checked out) or any vendor `.so` / decompiled output. Don't use IDA.
- The reconstructed codec tables live in `crates/aptx-adaptive/bundled_tables.rs` (see `LEGAL.md`). `crates/aptx-adaptive/build.rs` can regenerate and hash-verify them from a user-supplied vendor `.so` when `APTX_ADAPTIVE_SO` is set; that build script is the only thing that touches the file. Never hexdump it, and never print, log, test against or commit new table values.
- If a protocol fact is ambiguous or wrong, write the question in `impl/protocol/QUESTIONS.md` and move on. The coordinator gets it answered by the dirty side.
- Public sources are fine: PipeWire source, BlueZ, the A2DP spec, aptX classic/HD in ffmpeg or libfreeaptx.

## dev

- tools come from mise: `mise install`, `mise run check`
- the build needs `libaptXAdaptiveEnc.so` (README "install"). mise points `APTX_ADAPTIVE_SO` at `.scratch/libaptXAdaptiveEnc.so`; override with an absolute path.
- `crates/aptx-adaptive`: safe Rust only. Bit-exact against the vectors.

## scratch

- temporary and working files go in `.scratch/` in the repo (gitignored), never `/tmp`. Never commit them.
