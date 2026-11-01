//! Row-streaming for a certified, single full-page arithmetic generic region.
//! Context update adapted from hayro-jbig2 0.3.1; see THIRD_PARTY_NOTICES.md.
//! Unsupported segment programs use the general decoder, never partial output.
use crate::filter::jbig2_arith::{ArithmeticDecoder, ArithmeticDecoderContext};
use alloc::vec;

pub(crate) struct Program<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    typical_prediction: bool,
}

fn take<'a>(data: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let result = data.get(..n)?;
    *data = &data[n..];
    Some(result)
}
fn number(data: &mut &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(take(data, 4)?.try_into().ok()?))
}

/// Count a blank prefix in both lookahead rows, eight bytes at a time.
#[inline]
fn blank_prefix(a: &[u8], b: &[u8]) -> usize {
    let mut offset = 0;
    while offset + 8 <= a.len() {
        let bits = u64::from_le_bytes(a[offset..offset + 8].try_into().unwrap())
            | u64::from_le_bytes(b[offset..offset + 8].try_into().unwrap());
        if bits != 0 {
            return offset + bits.trailing_zeros() as usize / 8;
        }
        offset += 8;
    }
    while offset < a.len() && (a[offset] | b[offset]) == 0 {
        offset += 1;
    }
    offset
}

impl<'a> Program<'a> {
    /// Complete header scan before decoding: no globals, references, layering,
    /// refinement, stripes, unknown lengths, nonstandard templates, or offsets.
    pub(crate) fn probe(mut input: &'a [u8], width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || width > 65535 || height > 65535 {
            return None;
        }
        let mut page = false;
        let mut result = None;
        let mut previous = None;
        let mut ended = false;
        while !input.is_empty() {
            let id = number(&mut input)?;
            if previous.is_some_and(|p| id <= p) {
                return None;
            }
            previous = Some(id);
            let flags = take(&mut input, 1)?[0];
            let references = take(&mut input, 1)?[0];
            if references >> 5 != 0 {
                return None;
            }
            let association = if flags & 64 != 0 {
                number(&mut input)?
            } else {
                take(&mut input, 1)?[0] as u32
            };
            let length = number(&mut input)? as usize;
            let data = take(&mut input, length)?;
            match flags & 63 {
                48 if !page && result.is_none() && association == 1 => {
                    if data.len() != 19
                        || u32::from_be_bytes(data[..4].try_into().ok()?) as usize != width
                        || u32::from_be_bytes(data[4..8].try_into().ok()?) as usize != height
                        // Zero-filled page, default OR; the region may override
                        // the operator. No refinements, colour, or auxiliary planes.
                        || data[16] & !0x41 != 0
                        || data[17..] != [0, 0]
                    {
                        return None;
                    }
                    page = true;
                }
                38 | 39 if page && result.is_none() && !ended && association == 1 => {
                    if data.len() < 28
                        || u32::from_be_bytes(data[..4].try_into().ok()?) as usize != width
                        || u32::from_be_bytes(data[4..8].try_into().ok()?) as usize != height
                        || data[8..16] != [0; 8]
                        // On a pristine zero-filled page, OR, XOR and REPLACE
                        // all produce exactly this single full-page region.
                        || !matches!(data[16], 0 | 2 | 4)
                        || data[17] & !8 != 0
                        || data[18..26] != [3, 255, 253, 255, 2, 254, 254, 254]
                        || !data.ends_with(&[0xff, 0xac])
                    {
                        return None;
                    }
                    result = Some(Self {
                        data: &data[26..],
                        width,
                        height,
                        typical_prediction: data[17] & 8 != 0,
                    });
                }
                49 if result.is_some() && !ended && association == 1 && data.is_empty() => {
                    ended = true
                }
                51 if result.is_some()
                    && association == 0
                    && data.is_empty()
                    && input.is_empty() => {}
                _ => return None,
            }
        }
        result
    }

    /// The only full-resolution storage is three packed scanlines. Every
    /// entropy decision is preserved; repeated-row and blank-span shortcuts are exact.
    pub(crate) fn decode(&self, mut row: impl FnMut(&[u8], bool)) {
        let stride = self.width.div_ceil(8);
        let mut prev2 = vec![0u8; stride + 1];
        let mut prev1 = prev2.clone();
        let mut current = prev2.clone();
        let mut contexts = vec![ArithmeticDecoderContext::default(); 65536];
        let mut decoder = ArithmeticDecoder::new(self.data);
        let mut typical = false;
        for _ in 0..self.height {
            if self.typical_prediction {
                typical ^= decoder.read_bit(&mut contexts[0x9b25]) != 0;
            }
            if typical {
                current.copy_from_slice(&prev1);
            } else {
                let mut above2 = (prev2[0] as u32) << 6;
                let mut above1 = prev1[0] as u32;
                let mut context = (above2 & 0xf800) | (above1 & 0x7f0);
                let mut byte_index = 0;
                while byte_index < stride {
                    above2 = (above2 << 8) | ((prev2[byte_index + 1] as u32) << 6);
                    above1 = (above1 << 8) | prev1[byte_index + 1] as u32;
                    let count = (self.width - byte_index * 8).min(8);
                    if context == 0 && above1 == 0 && above2 == 0 {
                        // A zero context stays zero while both lookahead rows
                        // are blank. Check the arithmetic budget first, then
                        // inspect only the bytes that can actually be skipped.
                        let available = (decoder.zero_run_limit(contexts[0]) / 8) as usize;
                        let end_limit = (self.width / 8).min(byte_index + available);
                        if end_limit > byte_index {
                            let end = byte_index
                                + 1
                                + blank_prefix(
                                    &prev1[byte_index + 2..end_limit + 1],
                                    &prev2[byte_index + 2..end_limit + 1],
                                );
                            if decoder.try_zero_run(contexts[0], ((end - byte_index) * 8) as u32) {
                                current[byte_index..end].fill(0);
                                byte_index = end;
                                continue;
                            }
                        }
                    }
                    let mut byte = 0;
                    for k in 0..count {
                        let bit = 7 - k;
                        let pixel = decoder.read_bit(&mut contexts[context as usize]);
                        byte |= (pixel as u8) << bit;
                        context = ((context & 0x7bf7) << 1)
                            | pixel
                            | ((above2 >> bit) & 0x800)
                            | ((above1 >> bit) & 0x10);
                    }
                    current[byte_index] = byte;
                    byte_index += 1;
                }
            }
            row(&current[..stride], typical);
            core::mem::swap(&mut prev2, &mut prev1);
            core::mem::swap(&mut prev1, &mut current);
        }
    }
}
