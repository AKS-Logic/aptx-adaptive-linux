# Research: Three aptX Adaptive profiles

Decisions made while building this feature, the alternative rejected, and the evidence.
All measurements were taken on the operator's hardware (ribcage QA Ultra, `AC:BF:71:CB:3E:C1`)
with `btmon` and the plugin's own telemetry.

## R1 — The silence was the RTP envelope, not the encoder

**Symptom**: the profile negotiated, the transport started, audio was silent.

**Measurement**: 9 media frames in a 14 s window; RTP timestamp `912` then a constant `720`
for every following packet; SSRC `0xA3`; PipeWire unsent backlog pinned at ~22 000 bytes.

**Cause**: `session.rs::st0_mode()` keys on `APTX_ADAPTIVE_ST0`. The service drop-in never
set it, so the plugin took the §7F latency-mode branch: SSRC `0xA3` and a constant
sample-count timestamp. A sink cannot schedule audio whose time-to-play never advances and
regresses once, so it stopped acknowledging and BlueZ reported a connection timeout.

**Decision**: enable ST0 (`APTX_ADAPTIVE_ST0=1`). Under ST0 the plugin sends SSRC `0xAE`
and microsecond timestamps that advance by the frame period.

**Evidence after the change**: 1417 frames in the same window; SSRC `0xAE`; deltas
`+12000 µs` each; backlog `0/23/1336` bytes; zero disconnects.

**Rejected alternative**: keep §7F mode and make the timestamp constant-only. That is
self-consistent but does not match the source type we advertise, and it produced silence.

## R2 — HQ needs its own *codec id row*, not its own file

> Update: the separate HQ plugin file was later removed. It existed because the first
> three-endpoint attempt failed — but that failure was the missing `0x303` type-info row
> (R4), not the single-file layout. With the row in place, one plugin file exports all three
> endpoints and the second file is unnecessary. See R14.

Original decision, superseded:

**Attempt**: three entries in one plugin file sharing one endpoint, each with a distinct id.
**Result**: only two profiles appeared; the third was never registered.

**Decision**: export HQ from a dedicated factory (`api.codec.bluez5.media.aptx_adaptive_hq`)
and install the same binary as `libspa-codec-bluez5-aptx_adaptive_hq.so`, mirroring the
pattern that already produced a working extra profile.

## R3 — The loader has a hardcoded plugin table

`spa/plugins/bluez5/codec-loader.c` iterates a fixed `plugins[]` array of basenames. A new
file is never opened unless it is listed there.

**Evidence**: `/proc/<wireplumber pid>/maps` never contained the HQ `.so`; after adding
`MEDIA_CODEC_FACTORY_LIB("aptx_adaptive_hq")` and rebuilding, the file loads and logs
`1 codec(s) accepted`.

**Decision**: patch the table in our PipeWire build. This is why the loader is part of the
patched-stack set.

## R4 — Codec id `0x303` had no type-info row

Card profiles are enumerated by walking `spa_type_bluetooth_audio_codec` **by codec id**.
The table had rows for `0x301` (adaptive), `0x302` (LL), `0x304` (ultra) but **none for
`0x303`**, even though `SPA_BLUETOOTH_AUDIO_CODEC_APTX_ADAPTIVE_HD`/`_HIRES` exists in the
enum.

**Decision**: add a row in `type-info.h` named `aptx_adaptive_hq`. Without it no profile can
exist for that id, no matter what the plugin exports.

## R5 — Naming: HQ, not HD

The specification calls encoder profile 3 **HQ** and profile 6 **LL**. "aptX HD" is a
separate codec (vendor `0x24`). Naming our third profile "HD" invited confusion with a
different codec, so it is `aptx_adaptive_hq` throughout: profile name, endpoint, allow-list,
priority rules.

## R6 — Level policy belongs to the entry, not the process

Levels must differ per profile, but the environment is per-service. **Decision**: derive the
policy from the entry name in `codec_init` — HQ pins level 5 with `abr = false`; the adaptive
entry starts at 2 only when `APTX_ADAPTIVE_START_LOW` is set, leaving the default two-entry
contract (level 3) untouched for the existing tests.

## R7 — A queued start level must land at a packet boundary

`Stream::apply_level_now()` returns false while the stream is in its start mute (`hold`),
so a level requested at open time was silently dropped and the adaptive profile started at
3. **Decision**: attempt to land a pending level at the top of `Session::encode`, where the
stream is past the hold. Observed afterwards: `level 3 -> 2` immediately, then `2 -> 3 -> 4 -> 5`.

## R8 — Link clock contract and poll rate

`aptx-linkclock-inner.sh` published a single piconet tick. The plugin's reader expects
`"<monotonic_us> <clock>"`; with one field the seed was taken as absolute and the
time-to-play was wrong.

**Decision**: publish both fields, and poll at 5 Hz instead of 50 Hz — the codec seeds once
per stream, and the 50 Hz rate flooded journal and `btmon` (279 → 21 `Get Clock Information`
events per capture window), which is what made on-air traces unreadable.

## R9 — Profile switching does not disconnect (correction)

An earlier claim in this session — "switching profiles always drops the headset" — was
**wrong**. The 20 `dev_disconnected` events counted in a 15-minute window coincided with
plugin reinstalls and WirePlumber restarts performed by the agent, not with profile switches.

**Evidence**: LL → adaptive and adaptive → HQ both produced zero new disconnects, with the
link `Connected: yes` and the sink running. The operator independently confirmed that all
three modes switch with no disconnect and audio plays on each.

**Consequence**: the three-profile design stands; the in-band §7K path remains available but
is not required for switching.

## R10 — Patched libraries are pinned, not frozen

`NoUpgrade` in `/etc/pacman.conf` keeps our `libspa-bluez5.so` and `bluetoothd` and writes
the package copies as `.pacnew`. Pinning does not prevent ABI drift, so
`scripts/check-patched-libs.sh` compares installed against build and
`scripts/rebuild-patched-libs.sh` restores a matching build.

## R11 — Measurement hygiene

`speaker-test` does not exist on this machine; several early "sustained playback" runs were
therefore silent no-ops. Sustained tests now use a generated 48 kHz stereo WAV played with
`pw-play`. Any measurement taken across a restart or reinstall is discarded and re-taken.

## R12 — LL is measurably the lowest latency this sink allows

**Question**: is the LL profile actually the lowest-latency configuration, or only labelled so?

**Measured on air** (3 s playback per profile, `btmon`, ACL timestamps):

| Profile | Wire size | Interval median | Jitter (sd) |
|---|---|---|---|
| LL | 360 B (348 B payload) | **10.0 ms** | 0.01 ms |
| HQ (level 5) | 672 B (656 B payload) | 12.0 ms | 0.01 ms |

**Selected row, logged at stream open**:

```text
aptx-adaptive: row profile=Ll period=10ms bitrate=270kbps payload=348B
```

That identifies `LL[1]` with quality ≥ 3 exactly. The index comes from
`tables::index(Profile::Ll, DecoderLevel::R2, link) = 1 + qhs + 2*twm + 4*enhanced_ll`:
this sink advertises neither QHS nor TWM nor enhanced LL, so the index is 1.

**Why 8 ms is not used**: the table does contain 8 ms / 384-sample rows (`LL[5]`, `LL[6]`,
300 kbps), but they are reachable only when the sink advertises **enhanced LL**, which
raises the index to 5. The ribcage QA Ultra does not, so 10 ms is the floor available on
this hardware — not a limitation of the implementation.

**Conclusion**: LL is genuinely low-latency here: 17% shorter period than HQ and about half
the air time per packet (360 B vs 672 B). A sink advertising enhanced LL would reach 8 ms
through the same code path.

## R13 — Logging: journal only, quiet by default

**Question raised**: does the self-check logging create log files, is it needed in a release
build, and does it clean up after itself?

**Facts**:
- No crate opens a file for logging. Every line goes to stderr and is captured by
  systemd-journald; the only file the project writes is the link-clock hand-off in
  `/run/user/1000/aptx-linkclock`, which is a single value replaced atomically, not a log.
- The journal is size-capped. With `SystemMaxUse` unset, journald defaults to 10% of the
  filesystem capped at 4 GiB; `journalctl --disk-usage` reported 4 G, i.e. the cap, so
  old entries are vacuumed as new ones arrive. It cannot grow without bound.
- Measured volume of the periodic drift line: 240 lines in one hour ≈ 45 KiB/h, ≈ 1.1 MiB/day
  if audio ran continuously.

**Decision**: the periodic drift line is **off by default** and enabled with
`APTX_ADAPTIVE_DRIFT_LOG=1`. The one-shot lines stay on in every build: the negotiated row at
stream open and one line per bitrate change — a handful per connection, and the only evidence
that explains a silent stream.

**Related fix**: a pinned profile used to open at level 3 and log
`level 3 -> 5 (427k -> 427k)` at the first packet, because the pin was applied twice. Pinned
profiles now open directly on their row and queue no level request, so HQ logs exactly one
line per stream and never logs a level change.

## R14 — One plugin file is enough

**Question raised**: why two copies of the same binary under different names?

**Measurement**: the two files had identical hashes — the same build installed twice, not two
versions. The stock `/usr/lib/.../aptx.so` is a *different* library, loaded on purpose for the
passthrough path.

**Test**: with the `0x303` type-info row present, a three-endpoint export from a single file
produced all three profiles (`aptX Adaptive`, `aptX Adaptive LL`, `aptX Adaptive HQ`) and each
opened its expected row (`HQ 12 ms/427k`, `adaptive 19 ms/270k`, `LL 10 ms/270k/348B`).

**Decision**: drop the second plugin file and its dedicated factory. The installer now
installs one file and sets `APTX_ADAPTIVE_THREE_PROFILES=1` to select the three-endpoint
export; the two-endpoint export remains the default in code because the plugin's own test
contract describes it.

**Naming**: the plain `a2dp-sink` profile name is assigned by PipeWire, not by us. With this
layout it lands on LL, and a `priorities` list in WirePlumber's device rules did not change
that (priorities stayed 135/134/133 and followed codec ordering). Documented rather than
fought; the user picks a profile and WirePlumber remembers it per headset.

## R15 — Source type must be chosen per headset

**Final evidence** (real music, current build, both values):

| Sink | `0x00` source-timed | `0x02` host-timed |
|---|---|---|
| Bose QC Ultra | clean | stream dies, headset restarts |
| MOMENTUM 5 (PR #16) | works (write succeeds) | stops reading after ~1 s |
| ATH-TWX9MK2 (AKS PR #2) | static / popping | clean |

**Measurement trap recorded**: a 4 s capture showed ~700 frames in *both* modes on the Bose,
and a 12 s capture showed 1775 frames in host mode, which suggested host timing was fine. Real
playback exposed the failure within seconds (drop, then headset restart). Short frame counts
are not a compatibility test; only sustained listening is.

**Implementation**: `select_config` receives the device settings (after patching
`filter_bluez_device_setting` to pass `api.bluez5.address`, and `get_device_codec_settings` to
return device settings for A2DP). Resolution order: `APTX_ADAPTIVE_SOURCE_TYPE_OVERRIDES`
(address list) → `APTX_ADAPTIVE_ST0` → `0x02`. The choice is cached for the packet path so the
codec element and the SSRC/timestamp agree.

## Open evidence gaps

- 96 kHz was never exercised on hardware: no available sink advertises bit 5.
- Only one sink model was validated. Other sinks are supported by the same code path but
  unverified.
- Long-duration stability (hours) was not measured; the observed steady-state backlog is
  tens of bytes and streams ran clean for the duration of each test.