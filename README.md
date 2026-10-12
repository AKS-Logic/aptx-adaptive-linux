# aptx-linux

aptX Adaptive for Linux via PipeWire, clean-room Rust. Work in progress.

```
mise install
mise run check
mise run install-plugin
```

While streaming, the plugin logs one `aptx-adaptive: drift: …` line to stderr (the wireplumber journal) every 10 s, release builds included, for diagnosing stutter. `APTX_ADAPTIVE_DRIFT_LOG=0` in the service's environment silences it.
