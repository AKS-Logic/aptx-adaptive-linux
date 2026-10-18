//! OTA transport framing.
//!
//! The 8-byte transport header was recovered from the reference converter
//! (`slimbus2aptx-adaptive3.exe`) and confirmed against live captures of a
//! real phone→headset stream; the research harness parser
//! (`aptx-adaptive-stream.c`, openaptx research PR #12) documents it
//! field-by-field:
//!
//! ```text
//! byte 0..1 : TTP counter (u16 LE)
//! byte 2    : period (units of 0.25 ms: 100 = 25 ms, 60 = 15 ms)
//! byte 3    : packet type (indexes PAYLOAD_SIZES)
//! byte 4    : channel mode (0x00 stereo, 0x80, 0xa0, 0xc0)
//! byte 5..6 : session id low bytes
//! byte 7    : version — 0xae = R2, 0xad = R3, 0xaf = R2.2
//! ```
//!
//! followed by a fixed-size payload (`PAYLOAD_SIZES`).
//!
//! SPDX-License-Identifier: MIT

/// Payload sizes per packet type (transport values from the reference
/// converter, not codec-derived).
pub const PAYLOAD_SIZES: [u16; 9] = [348, 656, 140, 152, 560, 760, 960, 348, 980];

/// OTA version bytes.
pub const VERSION_R2: u8 = 0xae;
/// R2.2 (Lossless-capable).
pub const VERSION_R2_2: u8 = 0xaf;
/// R3.
pub const VERSION_R3: u8 = 0xad;

/// Channel modes accepted by the reference decoder.
pub const CHANNEL_MODE_STEREO: u8 = 0x00;
/// Channel mode 0x80 (decoder id 1).
pub const CHANNEL_MODE_0X80: u8 = 0x80;
/// Channel mode 0xa0 (decoder id 4).
pub const CHANNEL_MODE_0XA0: u8 = 0xa0;
/// Channel mode 0xc0 (decoder id 5).
pub const CHANNEL_MODE_0XC0: u8 = 0xc0;

/// A complete OTA packet: 8-byte header + payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtaPacket {
    /// u16 little-endian TTP counter.
    pub ttp: u16,
    /// Period in 0.25 ms units.
    pub period: u8,
    /// Packet type (payload size selector).
    pub packet_type: u8,
    /// Channel mode byte.
    pub channel_mode: u8,
    /// Session id low bytes (bytes 5..6).
    pub session: u16,
    /// Version byte (byte 7).
    pub version: u8,
    /// Fixed-size payload.
    pub payload: Vec<u8>,
}

impl OtaPacket {
    /// Serialize to wire bytes (header + payload).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + self.payload.len());
        v.extend_from_slice(&self.ttp.to_le_bytes());
        v.push(self.period);
        v.push(self.packet_type);
        v.push(self.channel_mode);
        v.extend_from_slice(&self.session.to_le_bytes());
        v.push(self.version);
        v.extend_from_slice(&self.payload);
        v
    }

    /// Parse wire bytes; returns the packet (payload from the size table).
    pub fn parse(data: &[u8]) -> Option<OtaPacket> {
        if data.len() < 8 {
            return None;
        }
        let packet_type = data[3];
        let payload_size = *PAYLOAD_SIZES.get(packet_type as usize)? as usize;
        if data.len() < 8 + payload_size {
            return None;
        }
        Some(OtaPacket {
            ttp: u16::from_le_bytes([data[0], data[1]]),
            period: data[2],
            packet_type,
            channel_mode: data[4],
            session: u16::from_le_bytes([data[5], data[6]]),
            version: data[7],
            payload: data[8..8 + payload_size].to_vec(),
        })
    }
}

/// The RTP header used on the A2DP transport (12 bytes, standard L2CAP/RTP
/// framing observed in captures: `80 60 ...`).
pub fn rtp_header(seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = 0x80; // v=2
    h[1] = 0x60; // marker + payload type 96
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ota_roundtrip() {
        let pkt = OtaPacket {
            ttp: 3900,
            period: 100,
            packet_type: 1,
            channel_mode: CHANNEL_MODE_STEREO,
            session: 0,
            version: VERSION_R2,
            payload: vec![0xAB; 656],
        };
        let bytes = pkt.to_bytes();
        assert_eq!(&bytes[..8], &[0x3c, 0x0f, 100, 1, 0x00, 0x00, 0x00, 0xae]);
        let back = OtaPacket::parse(&bytes).unwrap();
        assert_eq!(back, pkt);
    }
}
