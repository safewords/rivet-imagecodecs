//! A TIFF decoder and encoder, written from TIFF Revision 6.0 (Adobe, 1992),
//! the Adobe TIFF Technical Notes (Deflate, the floating-point predictor),
//! the BigTIFF extension and ITU-T T.4 / T.6 for fax coding.
//!
//! **Decoding** reads classic and BigTIFF files in either byte order;
//! strips and tiles; no compression, PackBits, LZW (including the
//! pre-6.0 "old-style" bit order), Deflate (both codes, 8 and 32946),
//! CCITT modified Huffman, Group 3 (1D and 2D) and Group 4; horizontal and
//! floating-point predictors; 1 to 32-bit integer samples (unsigned or
//! signed) and 16, 32 and 64-bit floats; bilevel, grey, RGB, palette,
//! CMYK and YCbCr (subsampled or not) pixels, with associated or
//! unassociated alpha, chunky or planar; every page of a multi-page file.
//! JPEG-compressed TIFF (old- or new-style) is refused for now.
//!
//! Pixels come back in the file's own precision ([`Samples`]: 8 or 16-bit
//! integers or `f32`), with [`Image::to_rgba8`] for display. Orientation
//! and an ICC profile are reported, not applied.
//!
//! **Encoding** writes grey, grey + alpha, RGB and RGBA at 8 or 16 bits and
//! 32-bit float, uncompressed, PackBits, LZW or Deflate, with the
//! horizontal (or floating-point) predictor, one page or many, classic or
//! BigTIFF, either byte order.
//!
//! DEFLATE itself comes from rivet's PNG crate (`rivet-png`), written the
//! same clean-room way.
//!
//! Malformed input never panics; sizes are checked against [`Limits`]
//! before anything is allocated, and decompression never produces more
//! than the declared image needs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod ccitt;
mod decode;
mod encode;
mod ifd;
mod lzw;
mod packbits;
mod par;

use std::fmt;

pub use decode::{Tiff, decode, decode_pages, decode_with_limits, read_info};
pub use encode::{EncodeOptions, Encoder, PixelFormat, SampleData, encode};

/// Why a TIFF could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The data ended before a structure the image needs.
    Truncated,
    /// Not a TIFF, or one that breaks the format.
    Invalid(String),
    /// Valid, but uses something this crate does not implement (JPEG
    /// compression, CIE L*a*b*, LogLuv, …).
    Unsupported(String),
    /// Larger than the [`Limits`] allow.
    LimitExceeded(String),
    /// The encoder was given input it cannot write.
    BadInput(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("TIFF data is truncated"),
            Error::Invalid(m) => write!(f, "invalid TIFF: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported TIFF: {m}"),
            Error::LimitExceeded(m) => write!(f, "TIFF exceeds limits: {m}"),
            Error::BadInput(m) => write!(f, "cannot encode TIFF: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Bounds a decode stays within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most pixels a page (or a tile) may have.
    pub max_pixels: u64,
    /// The most bytes of samples a page may hold while it is decoded.
    pub max_alloc: u64,
    /// The most pages read from the directory chain.
    pub max_pages: usize,
}

impl Default for Limits {
    /// 100 megapixels, 4 GiB of samples, 65 536 pages.
    fn default() -> Self {
        Self { max_pixels: 100_000_000, max_alloc: 4 << 30, max_pages: 65_536 }
    }
}

/// How pixel values are to be read (PhotometricInterpretation, 262).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Photometric {
    WhiteIsZero,
    BlackIsZero,
    Rgb,
    Palette,
    TransparencyMask,
    /// Usually CMYK (InkSet 1).
    Separated,
    YCbCr,
    CieLab,
    IccLab,
    ItuLab,
    LogL,
    LogLuv,
    Other(u16),
}

impl Photometric {
    fn from_code(v: u16) -> Self {
        match v {
            0 => Self::WhiteIsZero,
            1 => Self::BlackIsZero,
            2 => Self::Rgb,
            3 => Self::Palette,
            4 => Self::TransparencyMask,
            5 => Self::Separated,
            6 => Self::YCbCr,
            8 => Self::CieLab,
            9 => Self::IccLab,
            10 => Self::ItuLab,
            32844 => Self::LogL,
            32845 => Self::LogLuv,
            v => Self::Other(v),
        }
    }
}

/// Compression schemes (Compression, 259).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// 1.
    None,
    /// 2: CCITT modified Huffman run-length coding.
    CcittRle,
    /// 3: CCITT T.4 (Group 3 fax).
    Group3,
    /// 4: CCITT T.6 (Group 4 fax).
    Group4,
    /// 5.
    Lzw,
    /// 6: the TIFF 6.0 JPEG scheme, since withdrawn (not decoded).
    OldJpeg,
    /// 7: JPEG per TIFF Technical Note 2 (not decoded).
    Jpeg,
    /// 8 (Adobe) or 32946 (the earlier private code): zlib.
    Deflate,
    /// 32773.
    PackBits,
    /// Anything else, by code.
    Other(u16),
}

impl Compression {
    fn from_code(v: u16) -> Self {
        match v {
            1 => Self::None,
            2 => Self::CcittRle,
            3 => Self::Group3,
            4 => Self::Group4,
            5 => Self::Lzw,
            6 => Self::OldJpeg,
            7 => Self::Jpeg,
            8 | 32946 => Self::Deflate,
            32773 => Self::PackBits,
            v => Self::Other(v),
        }
    }

    fn code(self) -> u16 {
        match self {
            Self::None => 1,
            Self::CcittRle => 2,
            Self::Group3 => 3,
            Self::Group4 => 4,
            Self::Lzw => 5,
            Self::OldJpeg => 6,
            Self::Jpeg => 7,
            Self::Deflate => 8,
            Self::PackBits => 32773,
            Self::Other(v) => v,
        }
    }
}

/// PlanarConfiguration (284).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planar {
    /// 1: the samples of a pixel are together.
    Chunky,
    /// 2: each sample has its own plane.
    Separate,
}

/// SampleFormat (339).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 1: unsigned integers.
    Uint,
    /// 2: two's-complement signed integers.
    Int,
    /// 3: IEEE floating point.
    Float,
    /// Anything else (4, "undefined", and private values).
    Other(u16),
}

/// What a page's fields say.
#[derive(Debug, Clone, PartialEq)]
pub struct PageInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// BitsPerSample, one value per sample (or one for all).
    pub bits_per_sample: Vec<u16>,
    /// SamplesPerPixel.
    pub samples_per_pixel: u16,
    /// SampleFormat.
    pub sample_format: SampleFormat,
    /// PhotometricInterpretation (inferred when missing).
    pub photometric: Photometric,
    /// Compression.
    pub compression: Compression,
    /// PlanarConfiguration.
    pub planar: Planar,
    /// Tile width and length, for a tiled page.
    pub tile: Option<(u32, u32)>,
    /// RowsPerStrip, clipped to the height (for a stripped page).
    pub rows_per_strip: u32,
    /// Predictor: 1 none, 2 horizontal, 3 floating point.
    pub predictor: u16,
    /// Orientation, 1 ..= 8 (1, the default, is rows top to bottom, columns
    /// left to right). Not applied by the decoder.
    pub orientation: u16,
    /// ExtraSamples: 0 unspecified, 1 associated alpha, 2 unassociated.
    pub extra_samples: Vec<u16>,
    /// An ICC profile (tag 34675).
    pub icc_profile: Option<Vec<u8>>,
    /// X and Y resolution and their unit (1 none, 2 inch, 3 centimetre).
    pub resolution: Option<(f64, f64, u16)>,
    /// NewSubfileType bits (1: reduced-resolution, 2: page of many, 4:
    /// mask).
    pub subfile_type: u32,
    /// PageNumber: this page and the total (0 when unknown).
    pub page_number: Option<(u16, u16)>,
}

/// The channels of decoded samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorType {
    /// One channel, black to white.
    Gray,
    /// Grey and alpha.
    GrayAlpha,
    /// Red, green, blue (palette and YCbCr pages decode to this).
    Rgb,
    /// RGB and alpha.
    Rgba,
    /// Cyan, magenta, yellow, black (ink amounts: 0 is none).
    Cmyk,
    /// CMYK and alpha.
    Cmyka,
}

impl ColorType {
    /// Samples per pixel.
    pub fn channels(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba | Self::Cmyk => 4,
            Self::Cmyka => 5,
        }
    }
}

/// Decoded samples, interleaved, rows top to bottom (in storage order:
/// [`PageInfo::orientation`] is not applied). Integer samples use the full
/// range of their type: 1 to 7-bit data is scaled to 8 bits, 9 to 15 and
/// 17 to 32-bit data to 16 bits, signed data is offset by half the range.
#[derive(Debug, Clone, PartialEq)]
pub enum Samples {
    /// 8-bit samples.
    U8(Vec<u8>),
    /// 16-bit samples (and palette colours, which TIFF stores at 16 bits).
    U16(Vec<u16>),
    /// Floating-point samples, nominally 0.0 to 1.0.
    F32(Vec<f32>),
}

/// A decoded page.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The channels.
    pub color: ColorType,
    /// The samples, `width × height × color.channels()` of them.
    pub samples: Samples,
    /// Whether alpha is associated (the colour is premultiplied by it).
    pub premultiplied: bool,
    /// The page's fields.
    pub info: PageInfo,
}

impl Image {
    /// [`to_rgba8`](Self::to_rgba8) turned upright by the page's
    /// Orientation: width, height and pixels.
    pub fn to_rgba8_upright(&self) -> (u32, u32, Vec<u8>) {
        orient_rgba8(self.width, self.height, &self.to_rgba8(), self.info.orientation)
    }

    /// The page as 8-bit RGBA: 16-bit samples rounded, floats clamped to
    /// 0.0 ..= 1.0, CMYK converted naively (`R = (1 - C)(1 - K)`),
    /// associated alpha divided out.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let ch = self.color.channels();
        let n = self.width as usize * self.height as usize;
        let to8: Box<dyn Fn(usize) -> u8 + '_> = match &self.samples {
            Samples::U8(v) => Box::new(move |i| v[i]),
            Samples::U16(v) => Box::new(move |i| ((u32::from(v[i]) * 255 + 32767) / 65535) as u8),
            Samples::F32(v) => Box::new(move |i| {
                let f = v[i];
                if f.is_nan() { 0 } else { (f.clamp(0.0, 1.0) * 255.0).round() as u8 }
            }),
        };
        let mut out = Vec::with_capacity(n * 4);
        for p in 0..n {
            let s = |c: usize| to8(p * ch + c);
            let (mut rgb, a) = match self.color {
                ColorType::Gray => ([s(0); 3], 255),
                ColorType::GrayAlpha => ([s(0); 3], s(1)),
                ColorType::Rgb => ([s(0), s(1), s(2)], 255),
                ColorType::Rgba => ([s(0), s(1), s(2)], s(3)),
                ColorType::Cmyk | ColorType::Cmyka => {
                    let k = 255 - u32::from(s(3));
                    let c = |v: u8| ((255 - u32::from(v)) * k / 255) as u8;
                    ([c(s(0)), c(s(1)), c(s(2))], if self.color == ColorType::Cmyka { s(4) } else { 255 })
                }
            };
            if self.premultiplied && a < 255 {
                for v in &mut rgb {
                    *v = if a == 0 {
                        0
                    } else {
                        ((u32::from(*v) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
                    };
                }
            }
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
        out
    }
}

/// Turn 8-bit RGBA pixels stored under TIFF Orientation `orientation`
/// (1 ..= 8; other values are treated as 1) upright. Returns the upright
/// width, height and pixels.
///
/// The Orientation values name where the stored first row and first column
/// belong: 1 top/left, 2 top/right, 3 bottom/right, 4 bottom/left, 5
/// left/top, 6 right/top, 7 right/bottom, 8 left/bottom.
pub fn orient_rgba8(width: u32, height: u32, rgba: &[u8], orientation: u16) -> (u32, u32, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let swap = matches!(orientation, 5..=8);
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (ux, uy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let src = (y * w + x) * 4;
            let dst = (uy * ow + ux) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    (ow as u32, oh as u32, out)
}
