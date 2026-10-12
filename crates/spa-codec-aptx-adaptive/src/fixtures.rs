//! Test fixtures. Shared with `tests/dlopen.rs`.

use crate::ids::CAPS_LEN;

/// `spec/px7s2e-a2dp-caps.md`
pub const PX7_S2E: [u8; CAPS_LEN] = [
    0xd7, 0, 0, 0, 0xad, 0, 0x19, 0x0a, 0x37, 0x6a, 0, 0xaf, 0x64, 0x4b, 0, 0, 0xaa, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// `spec/a2dp-integration.md` §1.5 with source type 2 (§7E): the `SetConfiguration` we send.
pub const PX7_S2E_CONFIG: [u8; CAPS_LEN] = [
    0xd7, 0, 0, 0, 0xad, 0, 0x12, 0x02, 0x37, 0x6a, 0, 0xaf, 0x64, 0x4b, 0, 0, 0xaa, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
