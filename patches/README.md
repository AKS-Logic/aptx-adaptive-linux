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
| [`pipewire/0001-bluez5-aptx-adaptive.patch`](pipewire/0001-bluez5-aptx-adaptive.patch) | PipeWire git master, commit `a9dbcf677` | all **11** patched files identical to the build tree |
| [`bluez/0001-aptx-adaptive-additions.patch`](bluez/0001-aptx-adaptive-additions.patch) | BlueZ git master, commit `20ffd285b` | **8** modified files reproduce the build tree identically |
| [`bluez/0002-hfp-hf-remember-last-codec.patch`](bluez/0002-hfp-hf-remember-last-codec.patch) | same, applied on top of 0001 | HFP HF keeps the last negotiated codec |

BlueZ `0001` already contains the AVDTP unanswered-Abort handling and the A2DP
reconfiguration recovery; they are part of the same tree, so they are not separate files.
The PipeWire set now also carries two HFP fixes: a wideband codec wins over CVSD when both
are offered, and an idle SLC no longer activates the HFP profile (which made a headset
announce "call ended" on the first playback).

## Why the PipeWire patch exists

Without it the third profile cannot exist at all, and a profile switch can kill the link:

| File | Lines | Reason |
|---|---|---|
| `spa/include/spa/param/bluetooth/audio.h` | +8 | register the aptX Adaptive codec ids in the codec enum |
| `spa/include/spa/param/bluetooth/type-info.h` | +6 | **the `0x303` row** — card profiles are enumerated by codec id, so without a row here the HQ profile can never be listed |
| `spa/plugins/bluez5/codec-loader.c` | +27 | plugin table entry for `aptx_adaptive_hq`, so the loader opens that file |
| `spa/plugins/bluez5/bluez5-device.c` | +12 | profile construction for the extra codec ids |
| `spa/plugins/bluez5/bluez5-dbus.c` | +88 | endpoint/transport handling around reconfiguration |
| `spa/plugins/bluez5/defs.h` | +2 | codec-id definitions used by the files above |
| `spa/plugins/bluez5/a2dp-codec-aptx.c` | +8 | accept the Adaptive codec id in the combined aptX plugin |
| `spa/plugins/bluez5/media-sink.c` | +369 | sink-side transport path, including the `aptxdiag` write-active line |
| `spa/plugins/bluez5/media-source.c` | +152 | source-side transport path |
| `spa/plugins/bluez5/backend-native.c` | +18 | wideband codec preferred over CVSD; no HFP profile activation without a call |
| `spa/plugins/bluez5/meson.build` | +3 | build wiring for the above |

The single most important hunk is the `type-info.h` row. It was the actual reason HQ never
appeared while the codec was otherwise accepted by the loader.

## Why the BlueZ patches exist

A profile change makes BlueZ send AVDTP `Close`/`Abort`. Some sinks (the tested Bose among
them) ignore the Abort, so upstream BlueZ waits out its timeout and tears the link down.
`0001` carries the AVDTP unanswered-Abort handling, the A2DP reconfiguration recovery, and
the aptX Adaptive endpoint/transport additions. `0002` makes HFP HF keep the codec it last
negotiated instead of falling back to CVSD on every connect.

## Applying them

The installed binaries are pinned with `NoUpgrade` in `/etc/pacman.conf`, so pacman never
overwrites them — it writes the package copy as `.pacnew`. To build them from scratch:

```sh
# PipeWire
git clone https://gitlab.freedesktop.org/pipewire/pipewire.git
cd pipewire && git checkout a9dbcf677      # git master
git apply /path/to/patches/pipewire/0001-bluez5-aptx-adaptive.patch
meson setup _build && ninja -C _build spa/plugins/bluez5/libspa-bluez5.so
sudo install -m755 _build/spa/plugins/bluez5/libspa-bluez5.so \
    /usr/lib/spa-0.2/bluez5/libspa-bluez5.so

# BlueZ
git clone https://git.kernel.org/pub/scm/bluetooth/bluez.git
cd bluez && git checkout 20ffd285b         # git master; apply 0001..0002 in order
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
