//! Writing a BMP.

use std::collections::HashMap;

use crate::{Error, Result};

/// The pixel format to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 24-bit BGR with a `BITMAPINFOHEADER`; alpha is dropped.
    Rgb24,
    /// 32-bit BGRA with a `BITMAPV4HEADER` and explicit bit masks, alpha
    /// included.
    Rgba32,
    /// 8-bit palette (`BITMAPINFOHEADER` and up to 256 colours). The image
    /// must have 256 distinct colours or fewer; alpha is dropped.
    Indexed8,
}

/// Options beyond the pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Store rows top first (a negative height) instead of bottom first.
    pub top_down: bool,
    /// Resolution in pixels per metre; the default 2835 is 72 dpi.
    pub pixels_per_meter: (i32, i32),
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { top_down: false, pixels_per_meter: (2835, 2835) }
    }
}

/// Encode `width × height × 4` bytes of RGBA.
pub fn encode(width: u32, height: u32, rgba: &[u8], format: Format) -> Result<Vec<u8>> {
    encode_with_options(width, height, rgba, format, EncodeOptions::default())
}

/// [`encode`] with options.
pub fn encode_with_options(
    width: u32,
    height: u32,
    rgba: &[u8],
    format: Format,
    options: EncodeOptions,
) -> Result<Vec<u8>> {
    check(width, height)?;
    let n = width as usize * height as usize;
    if rgba.len() != n * 4 {
        return Err(Error::BadInput(format!("{} bytes for {width}x{height} RGBA", rgba.len())));
    }
    match format {
        Format::Rgb24 => {
            let w = width as usize;
            let stride = (w * 3).div_ceil(4) * 4;
            let mut out = headers(width, height, 24, 0, &[], stride * height as usize, options, None);
            for r in 0..height as usize {
                let y = if options.top_down { r } else { height as usize - 1 - r };
                for p in rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
                    out.extend_from_slice(&[p[2], p[1], p[0]]);
                }
                out.resize(out.len() + stride - w * 3, 0);
            }
            Ok(out)
        }
        Format::Rgba32 => {
            let w = width as usize;
            let masks = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];
            let mut out = headers(width, height, 32, 3, &[], w * 4 * height as usize, options, Some(masks));
            for r in 0..height as usize {
                let y = if options.top_down { r } else { height as usize - 1 - r };
                for p in rgba[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
                    out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            }
            Ok(out)
        }
        Format::Indexed8 => {
            let mut map: HashMap<[u8; 3], u8> = HashMap::new();
            let mut palette = Vec::new();
            let mut indices = Vec::with_capacity(n);
            for p in rgba.as_chunks::<4>().0 {
                let c = [p[0], p[1], p[2]];
                let i = match map.get(&c) {
                    Some(&i) => i,
                    None => {
                        if palette.len() == 256 {
                            return Err(Error::BadInput("more than 256 colours for an 8-bit palette".into()));
                        }
                        palette.push(c);
                        map.insert(c, (palette.len() - 1) as u8);
                        (palette.len() - 1) as u8
                    }
                };
                indices.push(i);
            }
            write_indexed(width, height, &palette, &indices, 8, options)
        }
    }
}

/// Encode a palette image: `indices` (one byte each, below
/// `palette.len()`) at 1, 4 or 8 bits per pixel.
pub fn encode_indexed(
    width: u32,
    height: u32,
    palette: &[[u8; 3]],
    indices: &[u8],
    bits: u16,
    options: EncodeOptions,
) -> Result<Vec<u8>> {
    check(width, height)?;
    if !matches!(bits, 1 | 4 | 8) {
        return Err(Error::BadInput(format!("{bits} bits per pixel (1, 4 or 8)")));
    }
    if palette.is_empty() || palette.len() > 1 << bits {
        return Err(Error::BadInput(format!("{} palette colours at {bits} bits", palette.len())));
    }
    if indices.len() != width as usize * height as usize {
        return Err(Error::BadInput(format!("{} indices for {width}x{height}", indices.len())));
    }
    if let Some(&i) = indices.iter().find(|&&i| usize::from(i) >= palette.len()) {
        return Err(Error::BadInput(format!("index {i} outside a {}-colour palette", palette.len())));
    }
    write_indexed(width, height, palette, indices, bits, options)
}

fn write_indexed(
    width: u32,
    height: u32,
    palette: &[[u8; 3]],
    indices: &[u8],
    bits: u16,
    options: EncodeOptions,
) -> Result<Vec<u8>> {
    let w = width as usize;
    let stride = (w * usize::from(bits)).div_ceil(32) * 4;
    let mut out = headers(width, height, bits, 0, palette, stride * height as usize, options, None);
    let per = 8 / usize::from(bits);
    for r in 0..height as usize {
        let y = if options.top_down { r } else { height as usize - 1 - r };
        let mut row = vec![0u8; stride];
        for (x, &i) in indices[y * w..(y + 1) * w].iter().enumerate() {
            let shift = 8 - usize::from(bits) * (x % per + 1);
            row[x / per] |= i << shift;
        }
        out.extend_from_slice(&row);
    }
    Ok(out)
}

fn check(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(Error::BadInput(format!("a {width}x{height} image")));
    }
    Ok(())
}

/// File header, information header (40 bytes, or a V4 header when masks are
/// given) and palette.
#[allow(clippy::too_many_arguments)]
fn headers(
    width: u32,
    height: u32,
    bits: u16,
    compression: u32,
    palette: &[[u8; 3]],
    image_size: usize,
    options: EncodeOptions,
    masks: Option<[u32; 4]>,
) -> Vec<u8> {
    let info_size: u32 = if masks.is_some() { 108 } else { 40 };
    let offset = 14 + info_size as usize + palette.len() * 4;
    let total = offset + image_size;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&u32::try_from(total).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(&info_size.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    let h = height as i32;
    out.extend_from_slice(&(if options.top_down { -h } else { h }).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&u32::try_from(image_size).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&options.pixels_per_meter.0.to_le_bytes());
    out.extend_from_slice(&options.pixels_per_meter.1.to_le_bytes());
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    if let Some(m) = masks {
        for v in m {
            out.extend_from_slice(&v.to_le_bytes());
        }
        // LCS_sRGB; the endpoints and gammas that follow are unused with it.
        out.extend_from_slice(&u32::from_be_bytes(*b"sRGB").to_le_bytes());
        out.extend_from_slice(&[0; 36 + 12]);
    }
    for c in palette {
        out.extend_from_slice(&[c[2], c[1], c[0], 0]);
    }
    out
}
