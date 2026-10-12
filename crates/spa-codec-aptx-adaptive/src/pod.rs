//! Just enough SPA pod serialisation to build a raw-audio `EnumFormat` object.

use crate::ffi::{
    SPA_FORMAT_AUDIO_CHANNELS, SPA_FORMAT_AUDIO_FORMAT, SPA_FORMAT_AUDIO_POSITION,
    SPA_FORMAT_AUDIO_RATE, SPA_FORMAT_MEDIA_SUBTYPE, SPA_FORMAT_MEDIA_TYPE, SPA_MEDIA_SUBTYPE_RAW,
    SPA_MEDIA_TYPE_AUDIO, SPA_TYPE_ARRAY, SPA_TYPE_ID, SPA_TYPE_INT, SPA_TYPE_OBJECT,
    SPA_TYPE_OBJECT_FORMAT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawFormat {
    pub format: u32,
    pub rate: u32,
    pub positions: &'static [u32],
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_ne_bytes());
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }

    fn prop_header(&mut self, key: u32) {
        self.u32(key);
        self.u32(0);
    }

    fn prop_scalar(&mut self, key: u32, type_: u32, value: u32) {
        self.prop_header(key);
        self.u32(4);
        self.u32(type_);
        self.u32(value);
        self.pad();
    }

    fn prop_id_array(&mut self, key: u32, values: &[u32]) {
        self.prop_header(key);
        self.u32(8 + 4 * values.len() as u32);
        self.u32(SPA_TYPE_ARRAY);
        self.u32(4);
        self.u32(SPA_TYPE_ID);
        for &v in values {
            self.u32(v);
        }
        self.pad();
    }
}

/// Serialises `Format` object `id` with fixed format, rate, channel count and positions,
/// the same shape `spa_format_audio_raw_build` produces.
#[must_use]
pub fn raw_audio_format(id: u32, f: &RawFormat) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(192));
    w.u32(0);
    w.u32(SPA_TYPE_OBJECT);
    w.u32(SPA_TYPE_OBJECT_FORMAT);
    w.u32(id);
    w.prop_scalar(SPA_FORMAT_MEDIA_TYPE, SPA_TYPE_ID, SPA_MEDIA_TYPE_AUDIO);
    w.prop_scalar(SPA_FORMAT_MEDIA_SUBTYPE, SPA_TYPE_ID, SPA_MEDIA_SUBTYPE_RAW);
    w.prop_scalar(SPA_FORMAT_AUDIO_FORMAT, SPA_TYPE_ID, f.format);
    w.prop_scalar(SPA_FORMAT_AUDIO_RATE, SPA_TYPE_INT, f.rate);
    w.prop_scalar(
        SPA_FORMAT_AUDIO_CHANNELS,
        SPA_TYPE_INT,
        f.positions.len() as u32,
    );
    w.prop_id_array(SPA_FORMAT_AUDIO_POSITION, f.positions);
    let body = (w.0.len() - 8) as u32;
    w.0[..4].copy_from_slice(&body.to_ne_bytes());
    w.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::{
        SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_S24_32_LE,
        SPA_PARAM_ENUM_FORMAT,
    };

    /// Output of `spa_pod_builder_add` from the 1.6.9 headers for the same format,
    /// `SPA_PARAM_EnumFormat` (3), dumped once with a throwaway C program.
    const REFERENCE: [u8; 168] = [
        0xa0, 0, 0, 0, 0x0f, 0, 0, 0, 0x03, 0, 0x04, 0, 0x03, 0, 0, 0, //
        0x01, 0, 0, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x03, 0, 0, 0, //
        0x01, 0, 0, 0, 0, 0, 0, 0, 0x02, 0, 0, 0, 0, 0, 0, 0, //
        0x04, 0, 0, 0, 0x03, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, //
        0x01, 0, 0x01, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x03, 0, 0, 0, //
        0x07, 0x01, 0, 0, 0, 0, 0, 0, 0x03, 0, 0x01, 0, 0, 0, 0, 0, //
        0x04, 0, 0, 0, 0x04, 0, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0, 0, //
        0x04, 0, 0x01, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0, 0, 0, //
        0x02, 0, 0, 0, 0, 0, 0, 0, 0x05, 0, 0x01, 0, 0, 0, 0, 0, //
        0x10, 0, 0, 0, 0x0d, 0, 0, 0, 0x04, 0, 0, 0, 0x03, 0, 0, 0, //
        0x03, 0, 0, 0, 0x04, 0, 0, 0,
    ];

    #[test]
    #[cfg(target_endian = "little")]
    fn matches_spa_pod_builder() {
        let pod = raw_audio_format(
            SPA_PARAM_ENUM_FORMAT,
            &RawFormat {
                format: SPA_AUDIO_FORMAT_S24_32_LE,
                rate: 48_000,
                positions: &[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR],
            },
        );
        assert_eq!(pod, REFERENCE);
    }
}
