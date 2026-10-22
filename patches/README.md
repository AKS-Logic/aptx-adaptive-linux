# Patches to system components

Our codec plugin needs two package-owned system components to behave differently, and BlueZ
needs two fixes. This directory carries those changes as **patch files only** — no upstream
source trees, no binaries, no build output. Everything here is a diff against the released
source of a public project.

Every patch here is generated from a source tree we actually built and installed, and each
was verified by applying it to a clean checkout of its base commit and comparing the result
against that tree byte for byte.

| Patch | Base | Verification |
|---|---|---|
| [`pipewire/0001-bluez5-aptx-adaptive.patch`](pipewire/0001-bluez5-aptx-adaptive.patch) | PipeWire **1.6.9**, commit `8fa27ca` (installed package `1:1.6.9-1`) | all **10** patched files identical to the build tree |
| [`bluez/0001-avdtp-Keep-the-session-on-an-unanswered-Abort.patch`](bluez/0001-avdtp-Keep-the-session-on-an-unanswered-Abort.patch) | BlueZ `5.87` per `configure.ac`, commit `dd9d5ba` (installed package `5.87-2.1`) | first of three; see below |
| [`bluez/0002-a2dp-Recover-from-a-failed-reconfiguration.patch`](bluez/0002-a2dp-Recover-from-a-failed-reconfiguration.patch) | same | second of three |
| [`bluez/0003-aptx-adaptive-additions.patch`](bluez/0003-aptx-adaptive-additions.patch) | same, **applied on top of 0001 and 0002** | together the three reproduce all **7** modified files identically |

`0001`/`0002` are the named fixes; `0003` carries the remaining changes our tree has beyond
them. Skipping `0003` yields a `bluetoothd` that applies cleanly but does not match what we
built — the failure mode this directory exists to avoid.

## Why the PipeWire patch exists

Without it the third profile cannot exist at all, and a profile switch can kill the link:

| File | Lines | Reason |
|---|---|---|
| `spa/include/spa/param/bluetooth/audio.h` | +8 | register the aptX Adaptive codec ids in the codec enum |
| `spa/include/spa/param/bluetooth/type-info.h` | +6 | **the `0x303` row** — card profiles are enumerated by codec id, so without a row here the HQ profile can never be listed |
| `spa/plugins/bluez5/codec-loader.c` | +35 | plugin table entry for `aptx_adaptive_hq`, so the loader opens that file |
| `spa/plugins/bluez5/bluez5-device.c` | +29 | profile construction for the extra codec ids |
| `spa/plugins/bluez5/bluez5-dbus.c` | +116 | endpoint/transport handling around reconfiguration |
| `spa/plugins/bluez5/defs.h` | +10 | codec-id definitions used by the files above |
| `spa/plugins/bluez5/a2dp-codec-aptx.c` | +8 | accept the Adaptive codec id in the combined aptX plugin |
| `spa/plugins/bluez5/media-sink.c` | +372 | sink-side transport path, including the `aptxdiag` write-active line |
| `spa/plugins/bluez5/media-source.c` | +154 | source-side transport path |
| `spa/plugins/bluez5/meson.build` | +3 | build wiring for the above |

The single most important hunk is the `type-info.h` row. It was the actual reason HQ never
appeared while the codec was otherwise accepted by the loader.

## Why the BlueZ patches exist

A profile change makes BlueZ send AVDTP `Close`/`Abort`. Some sinks (the tested Bose among
them) ignore the Abort, so upstream BlueZ waits out its timeout and tears the link down.
`0001` keeps the session alive when the Abort goes unanswered, `0002` recovers a failed
reconfiguration instead of dropping the link, and `0003` is the rest of what our build tree
carries.

## Applying them

The installed binaries are pinned with `NoUpgrade` in `/etc/pacman.conf`, so pacman never
overwrites them — it writes the package copy as `.pacnew`. To build them from scratch:

```sh
# PipeWire
git clone https://gitlab.freedesktop.org/pipewire/pipewire.git
cd pipewire && git checkout 1.6.9
git apply /path/to/patches/pipewire/0001-bluez5-aptx-adaptive.patch
meson setup _build && ninja -C _build spa/plugins/bluez5/libspa-bluez5.so
sudo install -m755 _build/spa/plugins/bluez5/libspa-bluez5.so \
    /usr/lib/spa-0.2/bluez5/libspa-bluez5.so

# BlueZ
git clone https://git.kernel.org/pub/scm/bluetooth/bluez.git
cd bluez && git checkout dd9d5ba      # 5.87; apply 0001..0003 in order
for p in /path/to/patches/bluez/*.patch; do git apply "$p" || break; done
./bootstrap-configure && make -j"$(nproc)"
sudo install -m755 src/bluetoothd /usr/lib/bluetooth/bluetoothd
```

Then verify and record the result:

```sh
scripts/check-patched-libs.sh     # expects PASS
```

`scripts/rebuild-patched-libs.sh` does the build-and-install half from local checkouts; use
it after a `pipewire` or `bluez` upgrade, or when the check reports `DRIFT`.

## Maintenance notes

- These patches are written against **exact versions**. A newer PipeWire or BlueZ almost
  certainly needs a rebase; a clean `git apply --check` is the test, not an expectation.
- After applying, confirm the patched result still matches what the installed binary was
  built from — a patch that applies but produces different behaviour is worse than a failure.
- Keep `type-info.h`'s `0x303` row and `codec-loader.c`'s table entry in step with the codec
  ids the plugin exports. Those two facts are what make the third profile visible.

## Licensing

These are modifications of upstream projects and stay under their licenses: PipeWire is MIT,
BlueZ is LGPL-2.1-or-later. The patch files are distributed here only as the minimal
difference needed for interoperability with our plugin. No upstream source, no vendor code,
and no binary from either project is included in this repository.
