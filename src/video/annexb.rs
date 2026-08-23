//! AVCC (length-prefixed) to Annex-B (start-code prefixed) conversion.
//!
//! An mp4 stores each H.264 access unit as a run of `[length][NAL payload]`
//! pairs, where the length field is 1, 2 or 4 bytes wide and the width is
//! declared once in the `avcC` box. Decoders that take a raw bitstream —
//! openh264 among them — want Annex-B instead: every NAL preceded by a
//! `00 00 00 01` start code, with the sequence and picture parameter sets
//! ahead of the first keyframe.
//!
//! Everything here is a pure function over bytes, which matters because this
//! is the first code a corrupt or truncated file reaches. A length field that
//! claims more bytes than remain, a zero-length NAL and a truncated tail are
//! all normal inputs to this module, not bugs: it stops at the damage and
//! returns what it could read, rather than indexing past the end.

/// Annex-B start code. Four bytes rather than three: some decoders only detect
/// parameter sets behind the long form.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// H.264 NAL unit types this module needs to recognise.
const NAL_TYPE_IDR: u8 = 5;
const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

/// The parameter sets from an `avcC` box, already in Annex-B form.
#[derive(Clone, Debug, Default)]
pub struct ParameterSets {
    /// `nal_length_size` from `avcC`: 1, 2 or 4.
    pub length_size: usize,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl ParameterSets {
    /// `length_size_minus_one` is the low two bits of the `avcC` field; the
    /// upper bits are reserved and are set to 1 in practice (0xFF for a
    /// 4-byte length), so they must be masked off rather than trusted.
    pub fn new(length_size_minus_one: u8, sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Self {
        Self {
            length_size: ((length_size_minus_one & 0b11) + 1) as usize,
            sps,
            pps,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sps.is_empty() && self.pps.is_empty()
    }

    /// SPS and PPS as one Annex-B blob, to hand a decoder before the first
    /// frame of a GOP.
    pub fn to_annexb(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in self.sps.iter().chain(self.pps.iter()) {
            if nal.is_empty() {
                continue;
            }
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(nal);
        }
        out
    }
}

fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1F)
}

/// Convert one AVCC sample into Annex-B, appending into `out`.
///
/// `out` is cleared first. Parameter sets are inserted ahead of the first NAL
/// when the sample contains an IDR and does not already carry its own — which
/// is the usual shape, since an mp4 keeps them in `avcC` rather than in-band.
///
/// Returns false when the sample yielded no NAL at all (empty or damaged
/// beyond the first length field); the caller should treat that as "no frame
/// here" and move on rather than feeding the decoder an empty buffer.
pub fn sample_to_annexb(sample: &[u8], params: &ParameterSets, out: &mut Vec<u8>) -> bool {
    out.clear();
    let length_size = params.length_size.clamp(1, 4);

    // First pass: does this access unit contain an IDR, and does it already
    // carry parameter sets in-band? Cheap enough to walk twice, and it keeps
    // the emit pass from having to rewind.
    let mut has_idr = false;
    let mut has_inband_params = false;
    for nal in NalUnits::new(sample, length_size) {
        match nal_type(nal) {
            Some(NAL_TYPE_IDR) => has_idr = true,
            Some(NAL_TYPE_SPS) | Some(NAL_TYPE_PPS) => has_inband_params = true,
            _ => {}
        }
    }
    if has_idr && !has_inband_params && !params.is_empty() {
        out.extend_from_slice(&params.to_annexb());
    }

    let mut emitted = false;
    for nal in NalUnits::new(sample, length_size) {
        if nal.is_empty() {
            continue;
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        emitted = true;
    }
    emitted
}

/// Iterator over the length-prefixed NAL units of one AVCC sample.
///
/// Stops at the first length field that does not fit in what remains, so a
/// truncated sample yields its intact leading NALs and nothing else.
struct NalUnits<'a> {
    rest: &'a [u8],
    length_size: usize,
}

impl<'a> NalUnits<'a> {
    fn new(sample: &'a [u8], length_size: usize) -> Self {
        Self {
            rest: sample,
            length_size,
        }
    }
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.len() < self.length_size {
            self.rest = &[];
            return None;
        }
        let (len_bytes, after_len) = self.rest.split_at(self.length_size);
        let nal_len = len_bytes.iter().fold(0usize, |acc, b| {
            acc.saturating_mul(256).saturating_add(*b as usize)
        });
        if nal_len > after_len.len() {
            // Truncated or corrupt: take nothing further rather than reading
            // past the sample.
            self.rest = &[];
            return None;
        }
        let (nal, rest) = after_len.split_at(nal_len);
        self.rest = rest;
        Some(nal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ParameterSets {
        ParameterSets::new(0xFF, vec![vec![0x67, 0x42, 0x00]], vec![vec![0x68, 0xCE]])
    }

    fn avcc(nals: &[&[u8]], length_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            let len = nal.len();
            for i in (0..length_size).rev() {
                out.push(((len >> (i * 8)) & 0xFF) as u8);
            }
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn length_size_comes_from_the_low_two_bits() {
        assert_eq!(ParameterSets::new(0xFF, vec![], vec![]).length_size, 4);
        assert_eq!(ParameterSets::new(0x03, vec![], vec![]).length_size, 4);
        assert_eq!(ParameterSets::new(0xFC, vec![], vec![]).length_size, 1);
        assert_eq!(ParameterSets::new(0xFD, vec![], vec![]).length_size, 2);
    }

    #[test]
    fn a_non_idr_sample_becomes_start_code_prefixed_nals() {
        // NAL type 1 = non-IDR slice.
        let sample = avcc(&[&[0x41, 0xAA, 0xBB], &[0x41, 0xCC]], 4);
        let mut out = Vec::new();
        assert!(sample_to_annexb(&sample, &params(), &mut out));
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x41, 0xAA, 0xBB, 0, 0, 0, 1, 0x41, 0xCC]
        );
    }

    #[test]
    fn an_idr_sample_gets_the_parameter_sets_prepended() {
        let sample = avcc(&[&[0x65, 0x88]], 4);
        let mut out = Vec::new();
        assert!(sample_to_annexb(&sample, &params(), &mut out));
        assert_eq!(
            out,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0x00, // SPS
                0, 0, 0, 1, 0x68, 0xCE, // PPS
                0, 0, 0, 1, 0x65, 0x88, // IDR
            ]
        );
    }

    #[test]
    fn in_band_parameter_sets_are_not_duplicated() {
        let sample = avcc(&[&[0x67, 0x42, 0x00], &[0x68, 0xCE], &[0x65, 0x88]], 4);
        let mut out = Vec::new();
        assert!(sample_to_annexb(&sample, &params(), &mut out));
        // Exactly three start codes, not five.
        assert_eq!(out.windows(4).filter(|w| *w == [0, 0, 0, 1]).count(), 3);
    }

    #[test]
    fn shorter_length_fields_are_honoured() {
        let mut params = params();
        params.length_size = 2;
        let sample = avcc(&[&[0x41, 0x01, 0x02]], 2);
        let mut out = Vec::new();
        assert!(sample_to_annexb(&sample, &params, &mut out));
        assert_eq!(out, vec![0, 0, 0, 1, 0x41, 0x01, 0x02]);
    }

    #[test]
    fn a_length_longer_than_the_sample_stops_the_walk_without_panicking() {
        // Declares 100 bytes but only 2 follow.
        let sample = vec![0, 0, 0, 100, 0x41, 0xAA];
        let mut out = Vec::new();
        assert!(!sample_to_annexb(&sample, &params(), &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn a_truncated_tail_keeps_the_intact_leading_nals() {
        let mut sample = avcc(&[&[0x41, 0xAA, 0xBB]], 4);
        // A second NAL whose length field is itself cut short.
        sample.extend_from_slice(&[0, 0]);
        let mut out = Vec::new();
        assert!(sample_to_annexb(&sample, &params(), &mut out));
        assert_eq!(out, vec![0, 0, 0, 1, 0x41, 0xAA, 0xBB]);
    }

    #[test]
    fn empty_and_degenerate_samples_are_handled() {
        let mut out = Vec::new();
        assert!(!sample_to_annexb(&[], &params(), &mut out));
        assert!(out.is_empty());
        // A well-formed zero-length NAL contributes nothing.
        assert!(!sample_to_annexb(&[0, 0, 0, 0], &params(), &mut out));
        assert!(out.is_empty());
        // A one-byte sample is shorter than the length field itself.
        assert!(!sample_to_annexb(&[0x07], &params(), &mut out));
    }

    #[test]
    fn huge_length_fields_saturate_instead_of_overflowing() {
        let sample = vec![0xFF, 0xFF, 0xFF, 0xFF, 0x41];
        let mut out = Vec::new();
        assert!(!sample_to_annexb(&sample, &params(), &mut out));
    }
}
