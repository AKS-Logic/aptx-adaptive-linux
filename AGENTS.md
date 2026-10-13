# aptx-linux

Clean-room aptX Adaptive encoder in Rust, plus a PipeWire bluez5 codec plugin. See `docs/adr/0001-architecture.md`.

## clean room rules (hard)

- Implement only from `spec/` and `tests/vectors/`.
- Never read, list or grep the `aptx-re-quarantine` repo (wherever it's checked out) or any vendor `.so` / decompiled output. Don't use IDA.
- If the spec is ambiguous or wrong, write the question in `spec/QUESTIONS.md` and move on. The coordinator gets it answered by the dirty side.
- Public sources are fine: PipeWire source, BlueZ, the A2DP spec, aptX classic/HD in ffmpeg or libfreeaptx.

## dev

- tools come from mise: `mise install`, `mise run check`
- `crates/aptx-adaptive`: safe Rust only. Bit-exact against the vectors.

## scratch

- temporary and working files go in `.scratch/` in the repo (gitignored), never `/tmp`. Never commit them.
