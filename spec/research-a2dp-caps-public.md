# aptX Adaptive A2DP caps - public research

Source: CodeAurora / Linux Foundation BSD-licensed Android Bluetooth stack (`a2dp_vendor_aptx_adaptive*.{h,cc}`, Copyright 2018 The Linux Foundation). These are facts only. Don't copy any code from there; write your own and compare afterwards.

## element layout (LOSC = 42, so 40 bytes after media type + codec type)

| offset in vendor body | field |
|---|---|
| 0..3 | vendor id 0x000000D7 LE |
| 4..5 | codec id 0x00AD LE |
| 6 | sample rate bits OR source type bits |
| 7 | channel mode bits |
| 8 | TTP low-latency min (ttp_ll_0) |
| 9 | TTP low-latency max (ttp_ll_1) |
| 10 | TTP high-quality min (ttp_hq_0) |
| 11 | TTP high-quality max (ttp_hq_1) |
| 12 | TTP TWS min (ttp_tws_0) |
| 13 | TTP TWS max (ttp_tws_1) |
| 14 | reserved |
| 15 | capability extension version (R2+) |
| 16..19 | supported features u32 (R2+) |
| 20..23 | setup preferences 1..4 (R2+) |
| 24, 25 | end-of-caps marker 0x00, 0xAA |
| rest | reserved, zero |

Public constants (some conflict between sources, so treat as UNCERTAIN until the dirty-side spec confirms them):

- channel mode: mono 0x01, stereo 0x02, TWS stereo 0x04, joint stereo 0x08, TWS mono 0x10, TWS+ 0x20
- sample rate: 48000 = 0x10. One source says 44100 = 0x08 and 96000 = 0x20; another says 44100 = 0x40 and 96000 = 0xA0.
- source type (low bits of byte 6): 0x00, 0x02, 0x04, 0x06
- capability extension version 0x01. Feature words: R2.0 0x0F000000, R2.1 0x0F000003, R2.2 0x0F000017.
- the source's default TTPs: LL 0x50/0x64, HQ 0x64/0x64, TWS 0xFF/0xFF

## Px7 S2e caps decoded against this layout (tentative)

`19 0a 37 6a 00 af 64 4b 00 00 aa 00 ...`

- 0x19: 48k (0x10) plus 0x08 (44.1k?), plus 0x01, which is unknown
- 0x0a: stereo plus joint stereo
- bytes 8..13 `37 6a 00 af 64 4b` don't look like the default TTP min/max pairs. The meaning is UNCERTAIN; the dirty side is checking.
- `00 00 aa` lands at body offsets 14..16. If the extension block is missing, that reads as reserved, then end marker 0x00 0xAA. That would mean the headset advertises R1-style caps with no R2 extension. UNCERTAIN.
