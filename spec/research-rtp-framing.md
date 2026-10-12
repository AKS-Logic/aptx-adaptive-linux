# aptX Adaptive A2DP media packet framing: public research

Question (spec/a2dp-integration.md §7.2, QUESTIONS.md): are the encoder stage's 8 header bytes
(`TTP_lo TTP_hi period×4 cfg 00 00 00 AE|AD`) the RTP **timestamp + SSRC** fields, or a payload
prefix that follows a normal 12-byte RTP header?

Everything below comes from openly licensed source or public documents. It is written in my own
words and no code was copied. Date: 2026-10-12.

## Conclusion

**Best-supported layout:** a 12-byte RTP header in which the packetiser itself supplies only
`V/P/X/CC`, `M/PT` and the sequence number (4 bytes). The encoder's 8 bytes then fill the
**timestamp** field (bytes 0..3) and the **SSRC** field (bytes 4..7). The swapped codec payload
follows directly after. On air the SSRC is `0x000000AE` for HQ and `0x000000AD` for LL.

**Confidence: medium.** No open-licence source shows the packetiser or an on-air capture. The
conclusion rests on the converging indirect evidence below, so an HCI snoop of an Android source
is still needed to settle it. The alternative layout, a 12-byte RTP header followed by an 8-byte
prefix, is not ruled out, but no public source supports it.

Implementation advice: keep both `Framing` variants in `crates/spa-codec-aptx-adaptive`. Default
to header-in-RTP, and flip the default only if a capture contradicts it.

## Evidence

### E1. The stack says aptX Adaptive uses an RTP header with the marker bit clear

- In the CAF/AOSPA Bluetooth stack, the codec hook "uses RTP header" returns **true** for aptX
  Adaptive. For classic aptX the same hook returns false unless SCMS-T is active (classic aptX
  carries no RTP header).
- The "use RTP marker bit" hook returns **false** for aptX Adaptive.
- The "build codec header" hook does nothing. The host adds no codec-specific payload header
  (unlike SBC's 1-byte frame count).
- Sources:
  - https://github.com/AOSPA/android_packages_modules_Bluetooth/blob/HEAD/system/stack/a2dp/a2dp_vendor_aptx_adaptive.cc
  - https://github.com/AOSPA/android_vendor_qcom_opensource_commonsys_system_bt/blob/HEAD/stack/a2dp/a2dp_vendor_aptx_adaptive.cc
- Licence: BSD-3-Clause, "Copyright (c) 2018, The Linux Foundation".

### E2. The host never builds aptX Adaptive media packets; the SoC does

- In the same stacks, every aptX Adaptive encoder entry point is a stub that returns early ("running
  in offload mode"). These include init, feeding, send_frames and the frame size, which returns 0.
  There is no software encoder path.
- So the RTP header described in E1 is never written by the host's AVDTP layer for this codec. The
  ADSP encoder → COP packetiser → Bluetooth SoC chain produces it.
- Consequence: the host-side defaults of the AVDTP write path describe what *would* happen for a
  software codec, not what goes on air for aptX Adaptive:
  - `m_pt = 0x60`, i.e. PT 96;
  - SSRC = `codec_info[1] | codec_info[2]` (media type | codec type, so 0xFF for vendor codecs);
  - the timestamp the encoder supplies.
- Sources:
  - https://github.com/AOSPA/android_packages_modules_Bluetooth/blob/HEAD/system/stack/a2dp/a2dp_vendor_aptx_adaptive_encoder.cc
  - https://github.com/AOSPA/android_packages_modules_Bluetooth/blob/HEAD/system/stack/avdt/avdt_scb_act.cc (SSRC generation, RTP header build)
  - https://github.com/AOSPA/android_packages_modules_Bluetooth/blob/HEAD/system/bta/av/bta_av_aact.cc (`m_pt` 0x60, marker handling)
- Licence: BSD-3-Clause (Linux Foundation) plus Apache-2.0 (AOSP/Broadcom).

### E3. The offload start command sizes the media packet as MTU + 12 bytes

- The Qualcomm vendor "A2DP offload start" HCI command (codec type 10 = aptX Adaptive) carries
  the L2CAP CID, the ACL handle, `mtu + AVDT_MEDIA_HDR_SIZE` (12) and a TTP of 150. It then carries
  the raw codec_info.
- The host explicitly adds exactly one 12-byte media header to the MTU it hands the controller. No
  extra 8-byte allowance appears anywhere.
- This is weak evidence: the encoder has its own MTU parameter (`mtulink0`, below), so the 8 bytes
  could be counted inside the encoder's budget either way. It is still consistent with "8 bytes +
  4 packetiser bytes = 12-byte RTP header".
- Source: `bta_av_qti_offload_req` / `bta_av_vendor_offload_start` in the files above. Licence: as E2.

### E4. Field-size arithmetic

- RTP fixed header = 1 (V/P/X/CC) + 1 (M/PT) + 2 (seq) + **4 (timestamp) + 4 (SSRC)** = 12 bytes.
- The encoder emits exactly 8 bytes. Bytes 4..7 are `00 00 00 AE|AD`, which reads as a 32-bit
  big-endian value 0xAE/0xAD.
- The spec notes that the reference's own diagnostics call these bytes the most-significant SSRC
  byte of the packet header.
- Read as a payload prefix, three of the eight bytes would be always-zero padding. Read as an SSRC,
  they are just the high bytes of a small identifier.
- This is a structural argument, not a source.

### E5. Mode is a stream property, set by the host through a vendor command, not by AVDTP

- HQ/LL/ULL are encoded as `0x1000` / `0x2000` / `0x4000` (ULL for 16-bit source: `0x5000`), with
  mask `0x7000`.
  - Defined in https://github.com/AOSPA/android_vendor_qcom_opensource_commonsys_system_bt/blob/HEAD/btif/include/btif_av.h
  - Also in the AOSP AIDL enum `AptxMode` (HIGH_QUALITY 0x1000, LOW_LATENCY 0x2000, ULTRA_LOW_LATENCY 0x4000): https://android.googlesource.com/platform/hardware/interfaces/+/refs/heads/main/bluetooth/audio/aidl/android/hardware/bluetooth/audio/AptxMode.aidl (Apache-2.0)
- The source switches mode mid-stream by sending a Qualcomm vendor HCI sub-command
  (`VS_QHCI_ENCODER_MODE_CHANGE` = 0x13) to its own controller, carrying the 16-bit mode.
  - This happens right after offload start, and again on a user or app request routed through
    `codec_specific_4`.
  - No AVDTP reconfigure is sent to the sink.
  - The sink must therefore learn about the switch **in-band**, which fits a per-packet marker such
    as SSRC 0xAE/0xAD.
  - Source: `bta_av_update_enc_mode` in https://github.com/AOSPA/android_vendor_qcom_opensource_commonsys_system_bt/blob/HEAD/bta/av/bta_av_aact.cc and `btif_update_source_codec` in `btif/src/btif_av.cc` of the same repo (BSD-3-Clause / Apache-2.0).
- The same encoder mode values (HQ 4096, LL 8192) appear as the encoder's "primary profile" in
  AudioReach (next item).

### E6. AudioReach (ADSP) APIs: an encoder then a COP packetiser, with no RTP knobs exposed

- https://github.com/AudioReach/audioreach-graphservices/blob/HEAD/spf/api/modules/aptx_adaptive_encoder_api.h
  - Encoder init has an MTU (`mtulink0`, default 675) and min/max sink buffering per mode. Min is
    in 1 ms units and max in 4 ms units, with defaults 20 and 50 (=200 ms). That matches
    spec §2's 20/200.
  - It also has a profile (HQ 0x1000 / LL 0x2000) and the sink capability bytes.
  - The profile parameter carries TTP offsets: LL0/LL1 in 1 ms units (defaults 60/80) and HQ0/HQ1
    in 4 ms units (default 50).
- `cop_v2_packetizer_api.h` and `cop_packetizer_cmn_api.h` in the same directory: the COP v2
  packetiser only takes an output media format and a stream-id → BT channel map. It sits in the
  encoder's subgraph and "packetizes raw bitstream into Compressed-over-PCM".
  - It has **no** RTP, SSRC, PT or timestamp parameters.
  - So either the header fields arrive from the encoder (the 8 bytes) or the BT SoC adds them.
    Neither reading supports host-configurable SSRC/PT.
- No COP packetiser *implementation* is published in `AudioReach/audioreach-engine`. Only the gate
  and other framework modules mention COP.
- Licence: BSD-3-Clause (SPDX tag in each header; "Copyright (c) Qualcomm Technologies, Inc."). Headers only, facts
  only.

### E7. The RTP timestamp is not a sample clock

- If bytes 0..3 are the RTP timestamp, they hold the following, not a running sample count:
  - TTP adjustment in 8 µs units (u16, little-endian in the encoder output);
  - period×4;
  - the cfg byte.
- None of the public sources show an aptX Adaptive RTP timestamp that advances by samples per
  packet. Compare RFC 7310 (aptX over RTP on IP), where the timestamp is the sample clock: that
  RFC covers classic/enhanced aptX on IP, not Bluetooth A2DP.
- A sink therefore cannot use the timestamp for drift recovery the usual way. The public TTP
  parameters (AIDL `AptxAdaptiveTimeToPlay`, low/high per LL/HQ/TWS; the CIE's TTP bytes) show that
  the sink schedules playback from TTP values.
- **UNCERTAIN:** the byte order of the TTP within the RTP timestamp field on air. The encoder emits
  it LE. If the packetiser copies the bytes verbatim, a parser reading a BE timestamp would see it
  byte-swapped.

### E8. Other open projects

- No open-source project implements aptX Adaptive transmission.
  - [libopenaptx](https://github.com/pali/libopenaptx) and [arkq/openaptx](https://github.com/arkq/openaptx) only do classic/HD.
  - PipeWire, bluez-alsa and BlueZ have no aptX Adaptive media path. BlueZ's `btmon-a2dp.rst` decodes
    capabilities only.
  - The Wireshark `packet-btavdtp.c` dissector knows aptX/aptX HD caps but has no aptX Adaptive
    media dissector.
- [SeiyaFunaokaJP/A2DP-Windows-Bridge](https://github.com/SeiyaFunaokaJP/A2DP-Windows-Bridge) (MIT) detects 0xD7/0x00AD but explicitly refuses to
  select it ("no open encoder").
- [santiquiroz/openwinblue](https://github.com/santiquiroz/openwinblue) `driver/src/l2cap_stream.c` (GPL-3.0, 2026) lumps aptX Adaptive with
  classic aptX as "no RTP header". This contradicts E1. It looks like an unverified assumption,
  since the project has no aptX Adaptive encoder, so it should not be trusted.
- No public btsnoop/pcap of aptX Adaptive media, no XDA/head-fi analysis of the header, and no
  patent text describing SSRC-as-mode turned up in searches.

### Excluded material (disclosed, not used)

GitHub code-search previews also surfaced strings from several repos that appear to redistribute
Qualcomm's proprietary **headset ADK** and BT firmware log decoders. They were not opened or relied
on because they have no open licence. Those previews mention sink-side handling of aptX Adaptive
"stream id"s via the RTP SSRC, with the low-latency value 0xAD, HQ 0xAE and a lossless 0xAF. This
agrees with the conclusion, but it is **not citable** under the project's rules. The coordinator
may want the dirty side to confirm independently.

## Answers to the sub-questions

| item | finding | confidence |
|---|---|---|
| Layout | 12-byte RTP header; the 8 encoder bytes are timestamp (0..3) and SSRC (4..7); no extra prefix | medium |
| SSRC | 0x000000AE HQ, 0x000000AD LL (from the encoder bytes, BE on air). 0xAF is plausibly lossless; not relevant to R1 | medium |
| RTP timestamp | carries TTP/period/cfg, not a sample clock | medium (follows from the layout) |
| Payload type | not exposed anywhere public for the offload path. The AOSP host default for A2DP is 0x60 (96, dynamic); use 96 | low |
| Marker bit | clear (aptX Adaptive's `useRtpHeaderMarkerBit` = false) | high for the host path, assumed the same on air |
| Sequence number | standard, incrementing per packet (packetiser-owned) | assumed |
| Codec payload header | none (`BuildCodecHeader` is a no-op) | high |
| HQ↔LL signalled in-band? | Yes, implicitly: the source switches mode via a vendor HCI command to its own controller with no AVDTP renegotiation, so the sink must detect it per packet. The SSRC byte is the only candidate in the known header. For a fixed-HQ Linux source to an R1 sink (spec §8.4) the SSRC is always 0xAE. | medium |

## Suggested follow-ups

- Capture `btsnoop_hci.log` from a Snapdragon phone streaming aptX Adaptive (HQ, then a game for
  LL). Check bytes 4..11 of the AVDTP media packet (RTP timestamp + SSRC) against the encoder's
  8 bytes.
  - Because of offload, the ACL media packets may not appear in the snoop. Some Qualcomm builds log
    them only with the "Bluetooth HCI snoop: full" option. An over-the-air sniffer is the fallback.
- Add a QUESTIONS.md item for the dirty side: the byte order of the TTP bytes within the RTP
  timestamp field on air, and the PT value.
