# B&W Px7 S2e remote A2DP endpoints (BlueZ MediaEndpoint1, captured 2026-10-12)

Modalias bluetooth:v19B5p0030dFFFF

| sep | codec | caps (hex) | meaning |
|---|---|---|---|
| sep1 | 0x00 SBC | ff ff 02 35 | |
| sep3 | 0x02 AAC | c0 ff 8c 84 e2 00 | |
| sep5 | 0xFF vendor | 4f 00 00 00 01 00 32 | aptX classic (vendor 0x4F, codec 0x0001) 48k/44.1k stereo |
| sep7 | 0xFF vendor | d7 00 00 00 24 00 32 00 00 00 00 | aptX HD (0xD7, 0x0024) |
| sep9 | 0xFF vendor | d7 00 00 00 ad 00 19 0a 37 6a 00 af 64 4b 00 00 aa 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 | aptX Adaptive (0xD7, 0x00AD), 40 bytes total, 34 body bytes |
