//! Results from decoding filtered data streams.

mod ascii_85;
pub(crate) mod ascii_hex;
#[cfg(feature = "images")]
mod ccitt;
#[cfg(feature = "images")]
mod dct;
#[cfg(feature = "images")]
mod jbig2;
mod jbig2_arith;
mod jbig2_stream;
#[cfg(feature = "images")]
mod jpx;
mod lzw_flate;
mod png;
mod run_length;

use crate::object::Dict;
use crate::object::Name;
use crate::object::dict::keys::*;
use crate::object::stream::{DecodeFailure, FilterResult, ImageDecodeParams};
use core::cell::Cell;
use core::ops::Deref;

/// The most bytes one filter may decode a stream to. A stream that would
/// decode to more fails, and `take_oversized` reports it.
pub const MAX_DECODED_BYTES: usize = 256 << 20;

std::thread_local! {
    static OVERSIZED: Cell<bool> = const { Cell::new(false) };
}

/// Whether a stream failed on this thread for exceeding `MAX_DECODED_BYTES`
/// since the last call. Clears the flag.
pub fn take_oversized() -> bool {
    OVERSIZED.replace(false)
}

/// A data filter.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Filter {
    /// ASCII hexadecimal encoding.
    AsciiHexDecode,
    /// ASCII base-85 encoding.
    Ascii85Decode,
    /// Lempel-Ziv-Welch (LZW) compression.
    LzwDecode,
    /// DEFLATE compression (zlib/gzip).
    FlateDecode,
    /// Run-length encoding compression.
    RunLengthDecode,
    /// CCITT Group 3 or Group 4 fax compression.
    CcittFaxDecode,
    /// JBIG2 compression for bi-level images.
    Jbig2Decode,
    /// JPEG (DCT) compression.
    DctDecode,
    /// JPEG 2000 compression.
    JpxDecode,
    /// Encryption filter.
    Crypt,
}

impl Filter {
    fn debug_name(&self) -> &'static str {
        match self {
            Self::AsciiHexDecode => "ascii_hex",
            Self::Ascii85Decode => "ascii_85",
            Self::LzwDecode => "lzw",
            Self::FlateDecode => "flate",
            Self::RunLengthDecode => "run-length",
            Self::CcittFaxDecode => "ccit_fax",
            Self::Jbig2Decode => "jbig2",
            Self::DctDecode => "dct",
            Self::JpxDecode => "jpx",
            Self::Crypt => "crypt",
        }
    }

    pub(crate) fn from_name(name: Name<'_>) -> Option<Self> {
        match name.deref() {
            ASCII_HEX_DECODE | ASCII_HEX_DECODE_ABBREVIATION => Some(Self::AsciiHexDecode),
            ASCII85_DECODE | ASCII85_DECODE_ABBREVIATION => Some(Self::Ascii85Decode),
            LZW_DECODE | LZW_DECODE_ABBREVIATION => Some(Self::LzwDecode),
            FLATE_DECODE | FLATE_DECODE_ABBREVIATION => Some(Self::FlateDecode),
            RUN_LENGTH_DECODE | RUN_LENGTH_DECODE_ABBREVIATION => Some(Self::RunLengthDecode),
            CCITTFAX_DECODE | CCITTFAX_DECODE_ABBREVIATION => Some(Self::CcittFaxDecode),
            JBIG2_DECODE => Some(Self::Jbig2Decode),
            DCT_DECODE | DCT_DECODE_ABBREVIATION => Some(Self::DctDecode),
            JPX_DECODE => Some(Self::JpxDecode),
            CRYPT => Some(Self::Crypt),
            _ => {
                warn!("unknown filter: {}", name.as_str());

                None
            }
        }
    }

    pub(crate) fn apply(
        &self,
        data: &[u8],
        params: &Dict<'_>,
        #[cfg_attr(not(feature = "images"), allow(unused))] image_params: &ImageDecodeParams,
        expected: Option<usize>,
    ) -> Result<FilterResult<'static>, DecodeFailure> {
        // An expected size (an image's raw samples) bounds the output, and
        // extra data is dropped. Otherwise one byte past the cap fails.
        let expected = expected.filter(|&n| n <= MAX_DECODED_BYTES);
        let limit = expected.unwrap_or(MAX_DECODED_BYTES + 1);
        let res = match self {
            Self::AsciiHexDecode => ascii_hex::decode(data)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::Ascii85Decode => ascii_85::decode(data)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::RunLengthDecode => run_length::decode(data, limit)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::LzwDecode => lzw_flate::lzw::decode(data, params, limit)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            Self::FlateDecode => lzw_flate::flate::decode(data, params, limit, expected)
                .map(FilterResult::from_data)
                .ok_or(DecodeFailure::StreamDecode),
            #[cfg(feature = "images")]
            Self::DctDecode => {
                dct::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::CcittFaxDecode => {
                ccitt::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::Jbig2Decode => {
                jbig2::decode(data, params, image_params).ok_or(DecodeFailure::ImageDecode)
            }
            #[cfg(feature = "images")]
            Self::JpxDecode => jpx::decode(data, image_params).ok_or(DecodeFailure::ImageDecode),
            #[cfg(not(feature = "images"))]
            Self::DctDecode | Self::CcittFaxDecode | Self::Jbig2Decode | Self::JpxDecode => {
                warn!("image decoding is not supported (enable the `images` feature)");
                Err(DecodeFailure::ImageDecode)
            }
            _ => Err(DecodeFailure::StreamDecode),
        };
        let res = match res {
            Ok(result) if result.data.len() > MAX_DECODED_BYTES => {
                OVERSIZED.set(true);
                Err(DecodeFailure::StreamDecode)
            }
            res => res,
        };

        if res.is_err() {
            warn!("failed to apply filter {}", self.debug_name());
        }

        res
    }
}

/// Receives runs of bilevel pixels and averages `fx` x `fy` blocks into 8-bit gray,
/// so a large bilevel image is never expanded to one byte per pixel.
pub(crate) struct ReduceSink {
    width: usize,
    fx: usize,
    fy: usize,
    out_width: usize,
    out_height: usize,
    x: usize,
    column: usize,
    left: usize,
    rows_in_block: usize,
    white: Vec<u32>,
    row_white: Vec<u32>,
    pub(crate) out: Vec<u8>,
}

impl ReduceSink {
    /// `None` unless the target is at most half the image in some direction.
    pub(crate) fn new(width: u32, height: u32, target: Option<(u32, u32)>) -> Option<Self> {
        let (tw, th) = target?;
        let (w, h) = (width as usize, height as usize);
        let fx = (w / tw.max(1) as usize).max(1);
        let fy = (h / th.max(1) as usize).max(1);
        if (fx < 2 && fy < 2) || w == 0 || h == 0 {
            return None;
        }
        let out_width = w.div_ceil(fx);
        let out_height = h.div_ceil(fy);
        Some(Self {
            width: w,
            fx,
            fy,
            out_width,
            out_height,
            x: 0,
            column: 0,
            left: fx,
            rows_in_block: 0,
            white: alloc::vec![0; out_width],
            row_white: alloc::vec![0; out_width],
            out: Vec::with_capacity(out_width * out_height),
        })
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.out_width as u32, self.out_height as u32)
    }

    #[inline]
    pub(crate) fn run(&mut self, white: bool, count: usize) {
        let mut count = count.min(self.width - self.x);
        self.x += count;
        while count > 0 {
            let take = count.min(self.left);
            if white {
                self.white[self.column] += take as u32;
            }
            count -= take;
            self.left -= take;
            if self.left == 0 {
                self.column += 1;
                self.left = self.fx;
            }
        }
    }

    /// A packed row (1 = black), optionally identical to the previous row.
    pub(crate) fn packed_row(&mut self, row: &[u8], repeated: bool) {
        if !repeated {
            for column in 0..self.out_width {
                let begin = column * self.fx;
                let end = ((column + 1) * self.fx).min(self.width);
                self.row_white[column] = (end - begin) as u32 - count_bits(row, begin, end);
            }
        }
        for (total, white) in self.white.iter_mut().zip(&self.row_white) {
            *total += white;
        }
        self.end_row();
    }

    pub(crate) fn end_row(&mut self) {
        self.x = 0;
        self.column = 0;
        self.left = self.fx;
        self.rows_in_block += 1;
        if self.rows_in_block == self.fy {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.out.len() >= self.out_width * self.out_height {
            return;
        }
        let rows = self.rows_in_block as u32;
        for (column, white) in self.white.iter_mut().enumerate() {
            let cols = (((column + 1) * self.fx).min(self.width) - column * self.fx) as u32;
            let n = rows * cols;
            self.out.push(((*white * 255 + n / 2) / n) as u8);
            *white = 0;
        }
        self.rows_in_block = 0;
    }

    /// The reduced image; rows never decoded are white.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.rows_in_block > 0 {
            self.flush();
        }
        self.out.resize(self.out_width * self.out_height, 0xFF);
        self.out
    }
}

fn count_bits(row: &[u8], mut start: usize, end: usize) -> u32 {
    let mut count = 0;
    while start < end && start % 8 != 0 {
        count += ((row[start / 8] >> (7 - start % 8)) & 1) as u32;
        start += 1;
    }
    for b in &row[start / 8..end / 8] {
        count += b.count_ones();
    }
    start = start.max(end / 8 * 8);
    while start < end {
        count += ((row[start / 8] >> (7 - start % 8)) & 1) as u32;
        start += 1;
    }
    count
}
