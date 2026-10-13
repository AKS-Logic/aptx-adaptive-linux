use crate::ffi::{
    SPA_CHOICE_ENUM, SPA_FORMAT_AUDIO_CHANNELS, SPA_FORMAT_AUDIO_FORMAT, SPA_FORMAT_AUDIO_POSITION,
    SPA_FORMAT_AUDIO_RATE, SPA_FORMAT_MEDIA_SUBTYPE, SPA_FORMAT_MEDIA_TYPE, SPA_MEDIA_SUBTYPE_RAW,
    SPA_MEDIA_TYPE_AUDIO, SPA_PROP_INFO_DESCRIPTION, SPA_PROP_INFO_ID, SPA_PROP_INFO_LABELS,
    SPA_PROP_INFO_TYPE, SPA_TYPE_ARRAY, SPA_TYPE_CHOICE, SPA_TYPE_ID, SPA_TYPE_INT,
    SPA_TYPE_OBJECT, SPA_TYPE_OBJECT_FORMAT, SPA_TYPE_OBJECT_PROP_INFO, SPA_TYPE_OBJECT_PROPS,
    SPA_TYPE_STRING, SPA_TYPE_STRUCT,
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

    fn int(&mut self, v: i32) {
        self.u32(4);
        self.u32(SPA_TYPE_INT);
        self.u32(v.cast_unsigned());
        self.pad();
    }

    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32 + 1);
        self.u32(SPA_TYPE_STRING);
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
        self.pad();
    }

    fn begin(&mut self, type_: u32) -> usize {
        let at = self.0.len();
        self.u32(0);
        self.u32(type_);
        at
    }

    fn end(&mut self, at: usize, size: usize) {
        let size = (size - at - 8) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_ne_bytes());
    }

    fn object(&mut self, type_: u32, id: u32) -> usize {
        let at = self.begin(SPA_TYPE_OBJECT);
        self.u32(type_);
        self.u32(id);
        at
    }
}

#[must_use]
pub fn enum_prop_info(
    id: u32,
    key: u32,
    description: &str,
    current: i32,
    labels: &[(i32, &str)],
) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(256));
    let obj = w.object(SPA_TYPE_OBJECT_PROP_INFO, id);
    w.prop_scalar(SPA_PROP_INFO_ID, SPA_TYPE_ID, key);
    w.prop_header(SPA_PROP_INFO_DESCRIPTION);
    w.string(description);
    w.prop_header(SPA_PROP_INFO_TYPE);
    let choice = w.begin(SPA_TYPE_CHOICE);
    w.u32(SPA_CHOICE_ENUM);
    w.u32(0);
    w.u32(4);
    w.u32(SPA_TYPE_INT);
    for v in std::iter::once(current).chain(labels.iter().map(|l| l.0)) {
        w.u32(v.cast_unsigned());
    }
    let end = w.0.len();
    w.end(choice, end);
    w.pad();
    w.prop_header(SPA_PROP_INFO_LABELS);
    let labels_at = w.begin(SPA_TYPE_STRUCT);
    for (v, label) in labels {
        w.int(*v);
        w.string(label);
    }
    let end = w.0.len();
    w.end(labels_at, end);
    let end = w.0.len();
    w.end(obj, end);
    w.0
}

#[must_use]
pub fn int_props(id: u32, key: u32, value: i32) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(40));
    let obj = w.object(SPA_TYPE_OBJECT_PROPS, id);
    w.prop_header(key);
    w.int(value);
    let end = w.0.len();
    w.end(obj, end);
    w.0
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let bytes = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

#[must_use]
pub fn props_int(pod: &[u8], key: u32) -> Option<i32> {
    let size = u32_at(pod, 0)? as usize;
    let pod = pod.get(..size.checked_add(8)?)?;
    if u32_at(pod, 4)? != SPA_TYPE_OBJECT || u32_at(pod, 8)? != SPA_TYPE_OBJECT_PROPS {
        return None;
    }
    let mut at = 16;
    while at + 16 <= pod.len() {
        let (k, value_size, value_type) = (
            u32_at(pod, at)?,
            u32_at(pod, at + 8)? as usize,
            u32_at(pod, at + 12)?,
        );
        let body = at + 16;
        if body.checked_add(value_size)? > pod.len() {
            return None;
        }
        if k == key {
            return (value_type == SPA_TYPE_INT && value_size >= 4)
                .then(|| u32_at(pod, body).map(u32::cast_signed))
                .flatten();
        }
        at = body.checked_add(value_size.checked_next_multiple_of(8)?)?;
    }
    None
}

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

    const PROP_INFO_LL: [u8; 216] = [
        0xd0, 0, 0, 0, 0x0f, 0, 0, 0, 0x01, 0, 0x04, 0, 0x01, 0, 0, 0, //
        0x01, 0, 0, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x03, 0, 0, 0, //
        0x0d, 0x01, 0, 0, 0, 0, 0, 0, 0x07, 0, 0, 0, 0, 0, 0, 0, //
        0x13, 0, 0, 0, 0x08, 0, 0, 0, 0x61, 0x70, 0x74, 0x58, 0x20, 0x41, 0x64, 0x61, //
        0x70, 0x74, 0x69, 0x76, 0x65, 0x20, 0x6d, 0x6f, 0x64, 0x65, 0, 0, 0, 0, 0, 0, //
        0x03, 0, 0, 0, 0, 0, 0, 0, 0x1c, 0, 0, 0, 0x13, 0, 0, 0, //
        0x03, 0, 0, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0, 0, 0, //
        0x01, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, //
        0x04, 0, 0, 0, 0, 0, 0, 0, 0x48, 0, 0, 0, 0x0e, 0, 0, 0, //
        0x04, 0, 0, 0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
        0x03, 0, 0, 0, 0x08, 0, 0, 0, 0x48, 0x51, 0, 0, 0, 0, 0, 0, //
        0x04, 0, 0, 0, 0x04, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, //
        0x0c, 0, 0, 0, 0x08, 0, 0, 0, 0x4c, 0x6f, 0x77, 0x20, 0x6c, 0x61, 0x74, 0x65, //
        0x6e, 0x63, 0x79, 0, 0, 0, 0, 0,
    ];

    const PROPS_LL: [u8; 40] = [
        0x20, 0, 0, 0, 0x0f, 0, 0, 0, 0x02, 0, 0x04, 0, 0x02, 0, 0, 0, //
        0x0d, 0x01, 0, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0x04, 0, 0, 0, //
        0x01, 0, 0, 0, 0, 0, 0, 0,
    ];

    const PROPS_LATENCY_THEN_HQ: [u8; 64] = [
        0x38, 0, 0, 0, 0x0f, 0, 0, 0, 0x02, 0, 0x04, 0, 0x02, 0, 0, 0, //
        0x0e, 0, 0x01, 0, 0, 0, 0, 0, 0x08, 0, 0, 0, 0x05, 0, 0, 0, //
        0x05, 0, 0, 0, 0, 0, 0, 0, 0x0d, 0x01, 0, 0, 0, 0, 0, 0, //
        0x04, 0, 0, 0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    #[cfg(target_endian = "little")]
    fn prop_pods_match_spa_pod_builder() {
        use crate::ffi::{SPA_PARAM_PROP_INFO, SPA_PARAM_PROPS, SPA_PROP_QUALITY};
        let info = enum_prop_info(
            SPA_PARAM_PROP_INFO,
            SPA_PROP_QUALITY,
            "aptX Adaptive mode",
            1,
            &[(0, "HQ"), (1, "Low latency")],
        );
        assert_eq!(info, PROP_INFO_LL);
        assert_eq!(int_props(SPA_PARAM_PROPS, SPA_PROP_QUALITY, 1), PROPS_LL);
        assert_eq!(props_int(&PROPS_LL, SPA_PROP_QUALITY), Some(1));
        assert_eq!(props_int(&PROPS_LATENCY_THEN_HQ, SPA_PROP_QUALITY), Some(0));
        assert_eq!(props_int(&PROPS_LATENCY_THEN_HQ, 0x1_000e), None);
        assert_eq!(props_int(&PROPS_LL, 0x1_000e), None);
    }

    #[test]
    fn props_int_rejects_junk() {
        assert_eq!(props_int(&[], 0x10d), None);
        assert_eq!(props_int(&PROPS_LL[..39], 0x10d), None);
        let mut not_props = PROPS_LL;
        not_props[8] = 3;
        assert_eq!(props_int(&not_props, 0x10d), None);
        let mut huge = PROPS_LL;
        huge[24] = 0xff;
        huge[27] = 0xff;
        assert_eq!(props_int(&huge, 0x10d), None);
        let mut long = PROPS_LATENCY_THEN_HQ;
        long[24..28].copy_from_slice(&0xffff_fff0_u32.to_ne_bytes());
        assert_eq!(props_int(&long, 0x10d), None);
    }

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
