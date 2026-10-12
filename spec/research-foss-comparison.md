# aptX Adaptive A2DP: our caps handling vs public FOSS (clean-side cross-check)

Status: clean-side research, 2026-10-12. Public open-licence sources only. Facts are restated in our own words and no code is copied. Compared against `crates/spa-codec-aptx-adaptive/src/caps.rs`, `spec/a2dp-integration.md` §1 and §7A, `spec/research-a2dp-caps-public.md`, and the btmon capture in `spec/QUESTIONS.md`.

## Sources

| id | what | URL | licence |
|---|---|---|---|
| A | AOSPA `packages_modules_Bluetooth`, branch `calcite` (current). Files: `system/stack/a2dp/a2dp_vendor_aptx_adaptive{.cc,_encoder.cc}`, `system/stack/include/a2dp_vendor_aptx_adaptive{.h,_constants.h}`, plus the callers in `system/bta/av/bta_av_aact.cc`, `system/btif/src/btif_av.cc`, `system/btif/co/bta_av_co.cc`, `system/stack/avdt/avdt_scb_act.cc`, `system/audio_hal_interface/{qti_hidl/a2dp_encoding.cc, aidl/a2dp/codec_status_aidl.cc}` | https://github.com/AOSPA/android_packages_modules_Bluetooth/tree/calcite/system/stack/a2dp | aptX Adaptive files: BSD-3-Clause-style, "Copyright (c) 2018, The Linux Foundation", with an explicit "no patent licence" clause. Surrounding AOSP files: Apache-2.0. |
| B | AOSPA `android_vendor_qcom_opensource_commonsys_system_bt` (older CAF commonsys, R2.2-era) | https://github.com/AOSPA/android_vendor_qcom_opensource_commonsys_system_bt | same Linux Foundation BSD header |
| C | AOSPA `android_hardware_qcom_bt` (oldest, R1/R2.0-era) | https://github.com/AOSPA/android_hardware_qcom_bt | same |
| D | Evervolv `android_vendor_qcom_opensource_system_bt` (R2.1-era CAF) | https://github.com/Evervolv/android_vendor_qcom_opensource_system_bt | same |
| E | bluez-alsa `src/shared/bluetooth-a2dp.h`: caps struct only, no aptX Adaptive codec | https://github.com/arkq/bluez-alsa/blob/master/src/shared/bluetooth-a2dp.h | MIT |
| F | bluez-alsa issue #242 "AptX Adaptive support": no technical content, the maintainer only says a codec library is needed | https://github.com/arkq/bluez-alsa/issues/242 | n/a |

Avoided on purpose: repos with "proprietary" in the name that GitHub code search returned (for example `*/android_vendor_qcom_proprietary`, `bthost_ipc_qti`). None of them were opened.

**Main caveat for everything below.** In A, the aptX Adaptive host "encoder" is a stub: every encoder callback returns at once. Real phones always run aptX Adaptive as **SoC offload**. The host only negotiates caps and then sends a vendor HCI command, and the Bluetooth controller does the encoding, RTP packetisation and any sideband. So no FOSS source shows the on-air media packet format for aptX Adaptive.

---

## 1. Caps layout and offsets

Same in all sources. The offsets are from the first vendor-id byte ("body"), as in our spec.

- LOSC is 42: media type, codec type 0xFF, then 40 body bytes. The parser in A **requires LOSC == 42 exactly**. Ours requires at least 40 body bytes and ignores the rest. That's compatible, and the Px7 sends exactly 40.
- Body 6: rate is the high 5 bits (mask 0xF8) and source type the low 3 bits (mask 0x07). Body 7: channel mode, mask 0x3F. Matches ours.
- The 18-byte "aptx_data" at body 8..25 is: TTP LL lo/hi, HQ lo/hi, TWS lo/hi, then reserved (body 14), extension version (15), features u32 LE (16..19), setup prefs ×4 (20..23), and EOC 0x00 0xAA (24..25). Body 26..39 is reserved and zero. The struct has no padding: the u32 lands on struct offset 8. **Matches ours.**
- R1 layout. A and B have an "R1 caps" constant whose aptx_data is the default TTPs, then `00` reserved, then `00 AA` at body 15..16, then zeros. E (bluez-alsa) models only this short form: 6 TTP bytes plus a 3-byte "eoc" field, 17 body bytes in total. **Matches ours** (`R1_DEFAULT_DATA`, and the Px7 caps). Confidence high.

## 2. Sample-rate bits

| source | 44.1k | 48k | 88.2k | 96k | 192k |
|---|---|---|---|---|---|
| A (current), B, crDroid | 0x40 | 0x10 | – | 0xA0 (two bits) | – |
| C (oldest) | 0x08 | 0x10 | 0x20 | 0x30 | 0x40 |
| D (Evervolv) | 0x08 | 0x10 | – | 0xA0 | – |
| E (bluez-alsa) | 0x08 (field bit 0) | 0x10 | 0x20 | – | 0x40 |

- Every source agrees that 48 kHz is 0x10. We use only that bit, so we are safe. Confidence high.
- 44.1 kHz is disputed. A also defines a reserved 0x80 and treats 0x10|0x80 as 48 kHz. 0x08 = 44.1 kHz has three independent sources (C, D, E). It also fits the Px7 advertising 0x18, as its aptX classic endpoint advertises 44.1 kHz too.
- A note on A's own logic: when the sink isn't R2.2 it strips 44.1 kHz from its own caps, so 44.1 kHz only appears with R2.2. Under A's 0x40 definition, the Px7's 0x08 bit would be "unknown".
- Verdict: the spec's reading (0x08 = 44.1k, UNCERTAIN) is probably right for R1-era sinks. 0x40 looks like a later change, made when 44.1 kHz became an R2.2 feature. Medium confidence. It doesn't matter for us.
- How rates are picked: when both sides allow 44.1 kHz, A's "best" picker prefers it over 48 kHz. Its default config, and the usual audio config, ask for 48 kHz first, though. For the Px7 the result is 48 kHz either way, so no difference from ours.

## 3. Channel-mode bits and choice

- The bits are the same everywhere: mono 0x01, stereo 0x02, TWS 0x04, joint stereo 0x08, TWS mono 0x10, TWS+ 0x20 (E stops at TWS mono). They match ours.
- How A chooses:
  1. It ANDs its own caps with the sink's caps.
  2. For a stereo request it prefers TWS+, then stereo, then joint stereo, then TWS stereo. (The fallback "best" order puts TWS stereo before joint stereo.)
- The Px7 offers 0x0a, so A picks **stereo 0x02**, the same as ours. Our order (stereo, then joint stereo) is a subset of A's.
- A rejects a configuration unless exactly one channel bit is set. It does **not** check that exactly one rate bit is set; that check is commented out. Our `validate_config` is stricter, which is fine for a source.

## 4. R1 vs R2 detection

| | rule |
|---|---|
| A (current) and B | R1 if the sink's extension-version byte (body 15) **== 0**; otherwise R2 |
| ours (`decoder_level`) | R2 if body 15 **== 1**; otherwise R1 |
| C (oldest) | doesn't look at the sink at all. It decides from whether the local controller supports "aptxadaptiver2" |

- The two rules differ only when body 15 is 2 or more. A would treat that as R2 and we treat it as R1.
- The rest of our spec (§1.3) says the reference *encoder* uses "== 1", so ours follows the encoder. A's rule is the host-side one.
- Both rules give R1 for the Px7 (body 15 = 0x00). Confidence high that the Px7 is R1.
- A also sets R2.2 only if features bit 0x80 ("sink decoder R2.2") is set **and** the extension version is 1. That matches our §1.3.

## 5. What the source sends in SetConfiguration

### 5.1 To an R1 sink (the Px7 case)

- **A and B:** if the sink's extension version is 0, the source copies the sink's **whole 18-byte aptx_data** (body 8..25) verbatim. Byte 6 = chosen rate | **the source's own source type**. Byte 7 = the one chosen channel bit. Body 26..39 is zeroed. No R2 extension is added; the source's own R1 TTP defaults are dropped.
- **C:** if the controller lacks R2, it sends the fixed R1 block `50 64 64 64 ff ff 00 00 aa`; otherwise it echoes the sink's 18 bytes.
- **Ours:** we echo body 8..16 (9 bytes) and zero the rest. On an R1 sink, body 17..25 are zeros anyway, so the bytes come out the same as A for the Px7.
- **So for the Px7, A would send** `d7 00 00 00 ad 00 10 02 37 6a 00 af 64 4b 00 00 aa` + zeros. That assumes an offload phone with source type 0 (see §6). **It is byte-for-byte what our btmon capture shows.** Confidence high that the SetConfiguration bytes are not the cause of the silence. The only possible change is byte 6 = 0x12 (source type 2, A's host-software source). Real phones use offload, so 0x10 is the more realistic value.

### 5.2 To an R2 sink: **discrepancy with our spec §1.4 and `caps.rs`**

Current A (and B) do **not** echo the sink's block for R2. They send the **source's own** aptx_data:

- the source's TTP defaults `50 64 64 64 ff ff`;
- reserved 0;
- extension version 1;
- the source's setup prefs `02 03 03 03`;
- EOC `00 aa`;
- a features word made from the sink's upper 24 bits plus a merged low byte. In that low byte, the high nibble is sink OR source and the low nibble is sink AND source. The source's own low byte is 0x00 for R2.0, 0x03 for R2.1 (+0x04 with split-TX) and 0x17 for R2.2.

Only the oldest source, C, echoes the sink's 18 bytes verbatim for R2. That is what our §1.4 and `select_config` do.

Verdict:

- The newer behaviour, own TTPs and prefs plus a merged features byte, is more likely what current phones send.
- For a source with no TWM, split-TX or 2.2 features, the merged low byte would be (sink & 0xF0), plus (sink & 0x0F & 0) = sink high nibble only.
- Echoing the sink's low nibble verbatim, as we do, claims source support for TWM, R2.1, split TX and split ACK whenever the sink offers them. That is risky.
- Medium confidence. It doesn't affect the Px7. Worth a QUESTIONS.md entry before any R2 sink is tested.

## 6. Source-type bits (byte 6, low 3 bits)

- The constants are 0x00, 0x02, 0x04 and 0x06. A commented-out block in the constants header gives the meaning:
  - a 1-bit "AVRCP custom unsupported/supported" flag (0/1);
  - a 1-bit Q2Q/NQ2Q flag;
  - a reserved bit.
- The numeric values (0, 2, 4, 6) suggest that bit 0 is the AVRCP-custom flag, bit 1 is Q2Q/NQ2Q, and bit 2 is reserved. That is our inference; no source states it.
- If so, the Px7's **0x01 means "sink supports custom AVRCP"**. The spec's "possibly AVRCP custom" guess is supported by this. Low-medium confidence.
- A's host-software source advertises and configures **0x02**. Every offload variant (R1, R2, R2.1, R2.2) uses **0x00**. Shipping phones use offload, so 0x00 is what they send, and ours matches.
- The config always carries the source's own type, never the sink's.

## 7. What the Android source advertises as its own caps (its SEP)

| field | offload R2.0 (A default) | offload R2.1 / R2.2 | host-software path | ours (`fill_caps`) |
|---|---|---|---|---|
| byte 6 | 0x10\|0xA0 (48k+96k), type 0 | +0x40 for 44.1k on R2.2 | 48k+96k, type 2 | 0x10 |
| byte 7 | 0x3E (stereo, TWS, joint stereo, TWS mono, TWS+) | 0x3E | 0x3E | 0x0A |
| aptx_data | TTP `50 64 64 64 ff ff`, 00, ext **01**, features 0x0F000000, prefs 02 03 03 03, EOC 00 AA | features 0x0F000003 (+0x04 split-TX) / 0x0F000017 | as R2.0 | R1 short form `50 64 64 64 ff ff 00 00 aa` |

- A also has an R1-only offload caps set (48k, 0x1E, R1 short form), but it is unreachable: the condition guarding it is hard-wired to false.
- Ours is an honest R1-only source. It only matters if a sink initiates configuration from our SEP. Fine as is.
- If the advertised extension version ever needs to say R2, copy A's R2.0 layout: features 0x0F000000, prefs 02 03 03 03.

## 8. TTP bytes: defaults and meaning

- The defaults agree everywhere: LL 0x50/0x64, HQ 0x64/0x64, TWS 0xFF/0xFF.
- **New fact (A, AIDL HAL path):** when A hands the negotiated config to the offload HAL, it converts each of the 6 TTP bytes to a **signed value by subtracting 128**. The older HIDL path passes the raw bytes.
  - So the TTP bytes look like signed, offset-128 values.
  - Defaults then become LL −48/−28, HQ −28/−28, TWS +127/+127.
  - The Px7 becomes LL −73/−22, HQ −128/+47, TWS −28/−53. The HQ value 0x00 (−128) may mean "minimum/unset".
  - The units are still unknown, and the encoder doesn't see these bytes (our §1.4).
  - Confidence medium that bias-128 is the intended encoding.
- The same HAL config always gives the encoder:
  - sink buffering {20,50,20,50,20,50}, which matches our §2;
  - input fade 0xFF (255), which matches §2;
  - input mode 0;
  - aptX mode 0x1000 (HQ) in the HIDL path, or the current HQ/LL mode in AIDL. This matches our §2 HQ rule.
  - It also gives an 11-byte "config stream": extension version, the 4 feature bytes, the 4 prefs, and the 2 EOC bytes. That is body 15..25 of the negotiated config. For an R1 sink that is `00 aa 00 00 00 00 00 00 00 00 00`, so the encoder side would also see R1.
  - All of this corroborates our spec.
- The legacy QTI offload-start command passes a fixed **TTP of 150** (presumably ms) for a non-TWS device, alongside the codec info. No TTP-to-header mapping is visible.

## 9. RTP / SSRC / PT / MTU for aptX Adaptive in Android

- Host generic media path (used by host-encoded codecs):
  - RTP byte 0 is 0x80;
  - the **payload type is 0x60 (96)**;
  - the marker bit is used only if the codec asks for it, and **aptX Adaptive says no** (`useRtpHeaderMarkerBit` is false);
  - the sequence number is +1 per packet;
  - the timestamp comes from the first 4 bytes of the buffer, which the codec module fills;
  - **SSRC = codec_info[1] | codec_info[2]**, i.e. media-type byte | codec-type byte = 0x00 | 0xFF = **0x000000FF** for any vendor codec.
  - A marks aptX Adaptive as using an RTP header (`UsesRtpHeader` → true) and its "build codec header" does nothing (no extra payload header).
- **But aptX Adaptive never goes through that path on Android**, because the encoder is stubbed. The host sends the controller one of three vendor HCI commands:
  - legacy QTI start (opcode sub-cmd, codec type **10**, "max latency 0", "**delay reporting 0**", SCMS-T flags, sample rate, ACL handle, remote L2CAP CID, **MTU + 12** for the AVDTP media header, stream start, TWS 0, TTP 150, then the full codec-info bytes);
  - generic AOSP v1 start (codec type, MTU, codec info);
  - v2 start (handle, CID, MTU, SCMS-T, opaque vendor params).

  None of them carries a payload type, SSRC or timestamp rule; the controller firmware decides those.
- Effect on our test:
  - PT 96 is consistent with Android's generic path, but that is **unconfirmed for aptX Adaptive**.
  - The **SSRC Android's host path would use (0x000000FF) differs from ours** (`00 00 00 AE`, from the "header in RTP" hypothesis). FOSS can't say what the SoC puts there.
  - The MTU handling (payload must fit peer MTU − 12) matches ours: 668 ≤ 895.
  - Confidence: high that FOSS cannot settle the RTP layout. A phone capture is still required.

## 10. Anything else Android sends around Start

From A's host code:

- **AVDTP to the sink:** only standard Discover, GetAllCapabilities, SetConfiguration, Open and Start. There are no vendor AVDTP signals for aptX Adaptive.
  - One special case: when the phone was the acceptor and the codec is aptX Adaptive, it **re-runs config selection after fetching the remote caps** and saves the new config. That is a reconfiguration path, not an extra message type.
  - Content protection: SCMS-T is added only if built in and active. Check that our btmon shows no CP and that the Px7 doesn't need it; it is SBC/AAC-agnostic and unlikely.
- **Delay reporting:** advertised if enabled (`BTA_AV_FEAT_DELAY_RPT`). The *sink* sends DelayReport and the source only consumes it. The source sends nothing. The offload command literally sets "delay reporting 0".
- **AVRCP:** the usual target, controller and vendor features. No aptX-Adaptive-specific AVRCP exchange appears in the host. The Px7's source-type bit 0x01 ("AVRCP custom supported"?) hints at one, but A never sends anything for it, since its own source type is 0 and the AVRCP-custom flag is clear.
- **Vendor HCI to the local controller, not over the air:**
  1. offload start (above);
  2. on a successful offload-start reply, `btif_av_update_codec_mode` sends an "encoder mode change" vendor command carrying HQ or LL (HQ when not gaming);
  3. at runtime, "update aptx data" vendor commands relay sub-band items from the user and framework.

  Whatever the controller then sends to the sink (LMP/L2CAP sideband, the §8.1 feedback channel) is in firmware and not visible.
- **Conclusion:** the host stack sends nothing extra to the sink that our source omits. If the Px7 waits for something before producing audio, it would come from the Qualcomm controller firmware, which no FOSS shows. Our evidence says nothing about whether such a firmware message exists. Medium confidence that nothing is required, given that the sink plays silence rather than rejecting.

---

## Summary of differences

| # | topic | ours | FOSS | which looks right | confidence |
|---|---|---|---|---|---|
| 1 | Px7 SetConfiguration bytes | `10 02` + echoed TTP + `00 00 aa` | identical (offload source type 0) | equal | high |
| 2 | R1 detection | ext == 1 → R2 | ext == 0 → R1 | same for real sinks; ours matches the encoder rule | high |
| 3 | R2 SetConfiguration aptx_data | echo the sink's 18 bytes | source's own TTP and prefs, merged features byte (only the oldest fork echoes) | FOSS current | medium |
| 4 | 44.1k bit | 0x08 (unused) | 0x08 (C, D, E) vs 0x40 (A, B) | 0x08 for R1-era sinks | medium |
| 5 | source type | 0 | 0 offload, 2 host-SW | 0 | medium-high |
| 6 | own SEP caps | R1 short form, 48k, 0x0A | R2 extension, 48+96k, 0x3E | ours fine for an R1-only encoder | high |
| 7 | TTP byte meaning | unknown | AIDL subtracts 128 (signed) | new lead | medium |
| 8 | RTP PT / SSRC | 96 / `000000AE` | host path 96 / `000000FF`; aptX Adaptive is really packetised in SoC firmware | unknown; SSRC differs | low |
| 9 | extra signalling before audio | none | none from the host; vendor HCI to the local controller only | equal on air from the host | medium |
| 10 | LOSC check | ≥ 40 body | == 42 | both fine | high |

### Suggested follow-ups (to `QUESTIONS.md` / live test)

- Try SSRC 0x000000FF, with the 8 header bytes left out of the RTP header. That needs a home for the header bytes: probably a payload prefix, but the earlier "header-after-rtp" test chimed, so this is low priority.
- Ask the dirty side whether the decoder side reads the RTP SSRC or timestamp at all, and whether a byte-6 source type of 0 vs 2 changes sink behaviour.
- Before testing an R2 sink, replace the R2 echo with the current-FOSS composition (§5.2).
