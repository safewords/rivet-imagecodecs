//! A BMP (device-independent bitmap) decoder and encoder, written from
//! Microsoft's documentation of the format (`BITMAPFILEHEADER`,
//! `BITMAPCOREHEADER`, `BITMAPINFOHEADER`, `BITMAPV4HEADER`,
//! `BITMAPV5HEADER`, the RLE4/RLE8 bitmap compression description and
//! `BI_BITFIELDS`) and IBM's OS/2 2.x `BITMAPINFOHEADER2`.
//!
//! **Decoding** reads 1, 2, 4 and 8-bit palette images, 16, 24 and 32-bit
//! direct colour (default layouts and arbitrary `BI_BITFIELDS` /
//! `BI_ALPHABITFIELDS` masks, alpha included), RLE8, RLE4 and OS/2 RLE24,
//! bottom-up and top-down rows, the OS/2 1.x and 2.x headers and the Windows
//! 40, 52, 56, 108 and 124-byte headers, with an embedded ICC profile from a
//! V5 header. Output is 8-bit RGBA.
//!
//! **Encoding** writes 24-bit (`BITMAPINFOHEADER`), 32-bit with alpha
//! (`BITMAPV4HEADER` with bit masks) and 1, 4 or 8-bit palette images.
//!
//! Malformed input never panics; the pixel count is checked against
//! [`Limits`] before anything is allocated.
//!
//! ```
//! let rgba = [10u8, 20, 30, 255, 40, 50, 60, 128];
//! let bytes = bmp::encode(2, 1, &rgba, bmp::Format::Rgba32).unwrap();
//! let img = bmp::decode(&bytes).unwrap();
//! assert_eq!((img.width, img.height), (2, 1));
//! assert_eq!(img.rgba, rgba);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod decode;
mod encode;

use std::fmt;

pub use decode::{Compression, HeaderKind, Image, Info, decode, decode_with_limits, read_info};
pub use encode::{EncodeOptions, Format, encode, encode_indexed, encode_with_options};

/// Why a BMP could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The data ended before the image did.
    Truncated,
    /// Not a BMP, or one that breaks the format.
    Invalid(String),
    /// Valid, but uses something this crate does not implement (embedded
    /// JPEG or PNG, OS/2 Huffman 1D, CMYK, 64-bit pixels).
    Unsupported(String),
    /// Larger than the [`Limits`] allow.
    LimitExceeded(String),
    /// The encoder was given input it cannot write.
    BadInput(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("BMP data is truncated"),
            Error::Invalid(m) => write!(f, "invalid BMP: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported BMP: {m}"),
            Error::LimitExceeded(m) => write!(f, "BMP exceeds limits: {m}"),
            Error::BadInput(m) => write!(f, "cannot encode BMP: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Bounds a decode stays within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most pixels an image may have.
    pub max_pixels: u64,
}

impl Default for Limits {
    /// 100 megapixels.
    fn default() -> Self {
        Self { max_pixels: 100_000_000 }
    }
}
