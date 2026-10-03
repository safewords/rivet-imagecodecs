//! A GIF decoder and encoder, written from the GIF89a specification
//! (CompuServe, 1990; the W3C-hosted text).
//!
//! **Decoding** reads GIF87a and GIF89a: LZW image data, global and local
//! colour tables, interlaced images, transparency, the four disposal methods,
//! and the NETSCAPE2.0 / ANIMEXTS1.0 loop extension. Every image is
//! composited onto the logical screen and handed back as a full-canvas RGBA
//! frame with its delay — what a viewer shows, not the raw sub-image.
//! Frames are produced one at a time by [`Decoder`], or all at once by
//! [`decode`].
//!
//! **Encoding** takes RGBA frames: colours are reduced to a palette (exactly,
//! when a frame has 256 colours or fewer; otherwise by median cut, with
//! optional Floyd–Steinberg dithering), unchanged areas between frames are
//! cropped away or made transparent, and the loop count is written as a
//! NETSCAPE2.0 extension. See [`Encoder`] and [`encode`].
//!
//! Malformed input never panics; sizes are checked against [`Limits`] before
//! anything is allocated.
//!
//! ```
//! let rgba = [255u8, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 0, 0, 0, 0];
//! let bytes = gif::encode(2, 2, &rgba, &gif::EncodeOptions::default()).unwrap();
//! let anim = gif::decode(&bytes).unwrap();
//! assert_eq!(anim.frames[0].rgba, rgba);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod decode;
mod encode;
mod lzw;
pub mod quantize;

use std::fmt;

pub use decode::{Animation, Decoder, Disposal, Frame, Info, decode, decode_with_limits, read_info};
pub use encode::{EncodeOptions, Encoder, PaletteMode, encode};

/// Why a GIF could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The data ended before a structure the image needs.
    Truncated,
    /// The data is not a GIF, or breaks the format in a way that leaves
    /// nothing sensible to show.
    Invalid(String),
    /// Valid, but uses something this crate does not implement.
    Unsupported(String),
    /// Larger than the [`Limits`] allow.
    LimitExceeded(String),
    /// The encoder was given input it cannot write (wrong buffer size, a
    /// zero dimension, …).
    BadInput(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("GIF data is truncated"),
            Error::Invalid(m) => write!(f, "invalid GIF: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported GIF: {m}"),
            Error::LimitExceeded(m) => write!(f, "GIF exceeds limits: {m}"),
            Error::BadInput(m) => write!(f, "cannot encode GIF: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A `Result` with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Bounds a decode stays within, so a small file cannot make the decoder
/// allocate or work without bound (a "decompression bomb").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The most pixels the logical screen, or any one image, may have.
    pub max_pixels: u64,
    /// The most frames [`decode`] collects (the streaming [`Decoder`] counts
    /// them too).
    pub max_frames: usize,
    /// The most bytes [`decode`] may hold across all frames it returns.
    pub max_total_bytes: u64,
}

impl Default for Limits {
    /// 100 megapixels, 100 000 frames, 2 GiB of frames.
    fn default() -> Self {
        Self { max_pixels: 100_000_000, max_frames: 100_000, max_total_bytes: 2 << 30 }
    }
}

impl Limits {
    /// No limits beyond what the format itself allows (65 535 × 65 535).
    pub fn none() -> Self {
        Self { max_pixels: u64::MAX, max_frames: usize::MAX, max_total_bytes: u64::MAX }
    }
}
