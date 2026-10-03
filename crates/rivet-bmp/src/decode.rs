//! Reading a BMP.

use crate::{Error, Limits, Result};

/// Which information header the file has, by its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    /// `BITMAPCOREHEADER` / OS/2 1.x (12 bytes; 16-bit dimensions, 3-byte
    /// palette entries).
    Core,
    /// OS/2 2.x `BITMAPINFOHEADER2` (16 to 64 bytes; the fields past the
    /// stated size are zero). Its compression 3 is Huffman 1D and 4 is
    /// RLE24.
    Os2,
    /// `BITMAPINFOHEADER` (40 bytes).
    Info,
    /// The undocumented 52-byte header with RGB masks.
    V2,
    /// The undocumented 56-byte header with RGBA masks.
    V3,
    /// `BITMAPV4HEADER` (108 bytes).
    V4,
    /// `BITMAPV5HEADER` (124 bytes).
    V5,
}

/// How the pixels are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// `BI_RGB`: uncompressed.
    Rgb,
    /// `BI_RLE8`.
    Rle8,
    /// `BI_RLE4`.
    Rle4,
    /// `BI_BITFIELDS`: uncompressed with channel masks.
    Bitfields,
    /// `BI_ALPHABITFIELDS`: masks including alpha.
    AlphaBitfields,
    /// `BI_JPEG`: an embedded JPEG (not decoded).
    Jpeg,
    /// `BI_PNG`: an embedded PNG (not decoded).
    Png,
    /// OS/2 Huffman 1D (not decoded).
    Huffman1D,
    /// OS/2 RLE24.
    Rle24,
    /// Anything else, by value.
    Other(u32),
}

/// What the headers say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Rows stored top first (a negative height).
    pub top_down: bool,
    /// Bits per pixel.
    pub bits_per_pixel: u16,
    /// The pixel storage.
    pub compression: Compression,
    /// The information header.
    pub header: HeaderKind,
    /// Palette entries read (0 for direct colour without a palette).
    pub palette_len: usize,
    /// Red, green, blue and alpha masks in effect for 16 and 32-bit images.
    pub masks: Option<[u32; 4]>,
    /// Horizontal and vertical resolution in pixels per metre (0 when not
    /// given).
    pub pixels_per_meter: (i32, i32),
    /// The V4/V5 colour space type (`LCS_sRGB`, `PROFILE_EMBEDDED`, …), as
    /// stored.
    pub color_space: Option<u32>,
}

/// A decoded image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes of RGBA, rows top to bottom.
    pub rgba: Vec<u8>,
    /// Whether the format carries alpha (an alpha mask), or RLE delta codes
    /// left pixels undefined (which this decoder makes transparent).
    pub has_alpha: bool,
    /// An ICC profile embedded through a V5 header.
    pub icc_profile: Option<Vec<u8>>,
    /// The headers.
    pub info: Info,
}

const LCS_PROFILE_EMBEDDED: u32 = u32::from_be_bytes(*b"MBED");

fn u16_at(d: &[u8], at: usize) -> Result<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(Error::Truncated)
}

fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(Error::Truncated)
}

/// The parsed headers plus where things are.
struct Layout {
    info: Info,
    palette: Vec<[u8; 3]>,
    pixels_at: usize,
    icc: Option<Vec<u8>>,
}

/// Read the headers.
pub fn read_info(data: &[u8]) -> Result<Info> {
    Ok(parse(data)?.info)
}

fn parse(data: &[u8]) -> Result<Layout> {
    if data.len() < 2 || &data[..2] != b"BM" {
        return Err(Error::Invalid("not a BMP (no BM signature)".into()));
    }
    let off_bits = u32_at(data, 10)? as usize;
    let hsize = u32_at(data, 14)? as usize;
    let h = 14usize;
    let header = match hsize {
        12 => HeaderKind::Core,
        40 => HeaderKind::Info,
        52 => HeaderKind::V2,
        56 => HeaderKind::V3,
        108 => HeaderKind::V4,
        124 => HeaderKind::V5,
        16..=64 => HeaderKind::Os2,
        _ => return Err(Error::Invalid(format!("an information header of {hsize} bytes"))),
    };
    if data.len() < h + hsize {
        return Err(Error::Truncated);
    }
    // A field of the (possibly shortened) header, or zero past its end.
    let field32 = |off: usize| -> u32 { if off + 4 <= hsize { u32_at(data, h + off).unwrap_or(0) } else { 0 } };
    let field16 = |off: usize| -> u16 { if off + 2 <= hsize { u16_at(data, h + off).unwrap_or(0) } else { 0 } };

    let (width, height, top_down, bits, compression_raw);
    if header == HeaderKind::Core {
        width = u32::from(u16_at(data, h + 4)?);
        height = u32::from(u16_at(data, h + 6)?);
        top_down = false;
        bits = u16_at(data, h + 10)?;
        compression_raw = 0;
    } else {
        let w = field32(4) as i32;
        let hh = field32(8) as i32;
        if header == HeaderKind::Os2 {
            // OS/2 dimensions are unsigned.
            width = w as u32;
            height = hh as u32;
            top_down = false;
        } else {
            if w < 0 {
                return Err(Error::Invalid(format!("a negative width ({w})")));
            }
            width = w as u32;
            top_down = hh < 0;
            height = hh.unsigned_abs();
        }
        bits = field16(14);
        compression_raw = field32(16);
    }
    let compression = match (compression_raw, header) {
        (0, _) => Compression::Rgb,
        (1, _) => Compression::Rle8,
        (2, _) => Compression::Rle4,
        (3, HeaderKind::Os2) => Compression::Huffman1D,
        (4, HeaderKind::Os2) => Compression::Rle24,
        (3, _) => Compression::Bitfields,
        (4, _) => Compression::Jpeg,
        (5, _) => Compression::Png,
        (6, _) => Compression::AlphaBitfields,
        (v, _) => Compression::Other(v),
    };
    let pixels_per_meter = if header == HeaderKind::Core { (0, 0) } else { (field32(24) as i32, field32(28) as i32) };
    let colors_used = if header == HeaderKind::Core { 0 } else { field32(32) as usize };

    // Bit masks: inside V2+ headers, or following a 40-byte header.
    let mut at = h + hsize;
    let mut masks = None;
    match compression {
        Compression::Bitfields | Compression::AlphaBitfields => {
            let alpha_inline = compression == Compression::AlphaBitfields;
            masks = Some(if hsize >= 52 && header != HeaderKind::Os2 {
                [field32(40), field32(44), field32(48), if hsize >= 56 { field32(52) } else { 0 }]
            } else {
                let m = [
                    u32_at(data, at)?,
                    u32_at(data, at + 4)?,
                    u32_at(data, at + 8)?,
                    if alpha_inline { u32_at(data, at + 12)? } else { 0 },
                ];
                at += if alpha_inline { 16 } else { 12 };
                m
            });
        }
        Compression::Rgb => {
            // The default layouts: 5-5-5 and 8-8-8, the rest of the pixel
            // unused (`BITMAPINFOHEADER`: "the high byte in each DWORD is
            // not used"). A 56-byte, V4 or V5 header has an alpha mask
            // field whose description, unlike the colour masks', is not
            // limited to BI_BITFIELDS: when it names bits outside the
            // colour, those are the alpha. A 40-byte header has no alpha
            // mask, and its unused byte stays unused — opaque — even when
            // a writer filled it (some put alpha there without saying so;
            // reading it would make every BMP whose writer leaves garbage
            // or zeros there transparent).
            let alpha = if hsize >= 56 && header != HeaderKind::Os2 { field32(52) } else { 0 };
            masks = match bits {
                16 => Some([0x7C00, 0x03E0, 0x001F, alpha & 0x8000]),
                32 => Some([0x00FF_0000, 0x0000_FF00, 0x0000_00FF, alpha & 0xFF00_0000]),
                _ => None,
            };
        }
        _ => {}
    }

    // The palette.
    let entry = if header == HeaderKind::Core { 3 } else { 4 };
    let wanted = if bits <= 8 {
        let full = 1usize << bits;
        if colors_used == 0 || colors_used > full { full } else { colors_used }
    } else {
        // A palette with direct colour is only a hint for display on
        // palette devices; it is skipped.
        0
    };
    let skip_hint = if bits > 8 { colors_used.min(1 << 16) } else { 0 };
    // Older writers give fewer entries than the depth implies; the pixel
    // data's offset bounds the palette when it says so.
    let room = if off_bits > at { (off_bits - at) / entry } else { usize::MAX };
    let count = wanted.min(room).min(data.len().saturating_sub(at) / entry);
    let palette: Vec<[u8; 3]> = (0..count)
        .map(|i| {
            let p = at + i * entry;
            [data[p + 2], data[p + 1], data[p]]
        })
        .collect();
    let palette_end = at + (count + skip_hint) * entry;
    let pixels_at = if off_bits >= h + hsize && off_bits < data.len() { off_bits } else { palette_end.min(data.len()) };

    let color_space = matches!(header, HeaderKind::V4 | HeaderKind::V5).then(|| field32(56));
    let mut icc = None;
    if header == HeaderKind::V5 && color_space == Some(LCS_PROFILE_EMBEDDED) {
        let off = field32(112) as usize;
        let size = field32(116) as usize;
        if let Some(p) = (h + off).checked_add(size).and_then(|end| data.get(h + off..end)) {
            icc = Some(p.to_vec());
        }
    }

    if width == 0 || height == 0 {
        return Err(Error::Invalid(format!("a {width}x{height} image")));
    }
    let info = Info {
        width,
        height,
        top_down,
        bits_per_pixel: bits,
        compression,
        header,
        palette_len: palette.len(),
        masks,
        pixels_per_meter,
        color_space,
    };
    Ok(Layout { info, palette, pixels_at, icc })
}

/// Decode with the default [`Limits`].
pub fn decode(data: &[u8]) -> Result<Image> {
    decode_with_limits(data, Limits::default())
}

/// Decode within `limits`.
pub fn decode_with_limits(data: &[u8], limits: Limits) -> Result<Image> {
    let Layout { info, palette, pixels_at, icc } = parse(data)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let pixels = u64::from(info.width) * u64::from(info.height);
    if pixels > limits.max_pixels {
        return Err(Error::LimitExceeded(format!("{}x{} is over the limit of {} pixels", w, h, limits.max_pixels)));
    }
    let bits = info.bits_per_pixel;
    let src = &data[pixels_at..];
    let mut rgba = vec![0u8; w * h * 4];
    let mut has_alpha = false;

    // Row `r` of storage is row `y` of the picture.
    let flip = |r: usize| if info.top_down { r } else { h - 1 - r };

    match info.compression {
        Compression::Rle8 | Compression::Rle4 | Compression::Rle24 => {
            let expected = match info.compression {
                Compression::Rle8 => 8,
                Compression::Rle4 => 4,
                _ => 24,
            };
            if bits != expected {
                return Err(Error::Invalid(format!("{:?} with {bits} bits per pixel", info.compression)));
            }
            if info.top_down {
                return Err(Error::Invalid("a run-length coded image stored top-down".into()));
            }
            has_alpha = rle(src, info.compression, w, h, &palette, &mut rgba);
        }
        Compression::Rgb | Compression::Bitfields | Compression::AlphaBitfields => {
            if !matches!(bits, 1 | 2 | 4 | 8 | 16 | 24 | 32) {
                return Err(if bits == 64 {
                    Error::Unsupported("64 bits per pixel".into())
                } else {
                    Error::Invalid(format!("{bits} bits per pixel"))
                });
            }
            if info.compression != Compression::Rgb && !matches!(bits, 16 | 32) {
                return Err(Error::Invalid(format!("bit masks with {bits} bits per pixel")));
            }
            let row_bits = w.checked_mul(usize::from(bits)).ok_or(Error::Truncated)?;
            let stride = row_bits.div_ceil(32) * 4;
            let needed = stride.checked_mul(h).ok_or(Error::Truncated)?;
            if src.len() < needed {
                // The last row may lack its padding.
                let last = row_bits.div_ceil(8);
                if src.len() < stride * (h - 1) + last {
                    return Err(Error::Truncated);
                }
            }
            let masks = info.masks.map(|m| m.map(Channel::new));
            if let Some(m) = &masks {
                has_alpha = m[3].bits > 0;
            }
            for r in 0..h {
                let row = &src[r * stride..(r * stride + stride).min(src.len())];
                let out = &mut rgba[flip(r) * w * 4..(flip(r) + 1) * w * 4];
                match bits {
                    1 | 2 | 4 | 8 => {
                        let per = 8 / usize::from(bits);
                        let mask = ((1u16 << bits) - 1) as u8;
                        for x in 0..w {
                            let byte = row[x / per];
                            let shift = 8 - usize::from(bits) * (x % per + 1);
                            let i = usize::from((byte >> shift) & mask);
                            let [r, g, b] = palette.get(i).copied().unwrap_or([0, 0, 0]);
                            out[x * 4..x * 4 + 4].copy_from_slice(&[r, g, b, 255]);
                        }
                    }
                    24 => {
                        for x in 0..w {
                            let p = &row[x * 3..x * 3 + 3];
                            out[x * 4..x * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                        }
                    }
                    _ => {
                        let Some(m) = &masks else {
                            return Err(Error::Invalid("no bit masks".into()));
                        };
                        let bytes = usize::from(bits / 8);
                        for x in 0..w {
                            let v = if bytes == 2 {
                                u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]))
                            } else {
                                u32::from_le_bytes([row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]])
                            };
                            let a = if m[3].bits > 0 { m[3].get(v) } else { 255 };
                            out[x * 4..x * 4 + 4].copy_from_slice(&[m[0].get(v), m[1].get(v), m[2].get(v), a]);
                        }
                    }
                }
            }
        }
        Compression::Jpeg => return Err(Error::Unsupported("an embedded JPEG".into())),
        Compression::Png => return Err(Error::Unsupported("an embedded PNG".into())),
        Compression::Huffman1D => return Err(Error::Unsupported("OS/2 Huffman 1D compression".into())),
        Compression::Other(v) => return Err(Error::Unsupported(format!("compression {v}"))),
    }
    Ok(Image { width: info.width, height: info.height, rgba, has_alpha, icc_profile: icc, info })
}

/// One channel of a bit-field layout.
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    bits: u32,
}

impl Channel {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Self { mask: 0, shift: 0, bits: 0 };
        }
        let shift = mask.trailing_zeros();
        // The span from the lowest to the highest set bit (a mask with gaps
        // is read as if it had none).
        let bits = 32 - mask.leading_zeros() - shift;
        Self { mask, shift, bits }
    }

    /// The channel's value scaled to 0 ..= 255 (rounded).
    fn get(&self, v: u32) -> u8 {
        if self.bits == 0 {
            return 0;
        }
        let max = (1u64 << self.bits) - 1;
        let x = u64::from((v & self.mask) >> self.shift);
        ((x * 255 + max / 2) / max) as u8
    }
}

/// Run-length decoding (RLE8, RLE4 and OS/2 RLE24). Pixels the codes skip
/// stay transparent; returns whether any did. Codes that run past a row or
/// the image are clipped; data that ends early leaves the rest undefined.
fn rle(src: &[u8], kind: Compression, w: usize, h: usize, palette: &[[u8; 3]], rgba: &mut [u8]) -> bool {
    let (mut x, mut row) = (0usize, 0usize);
    let colour = |i: usize| -> [u8; 3] { palette.get(i).copied().unwrap_or([0, 0, 0]) };
    let mut put = |x: usize, row: usize, c: [u8; 3]| {
        if x < w && row < h {
            let y = h - 1 - row;
            let at = (y * w + x) * 4;
            rgba[at..at + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    };
    let mut i = 0;
    while i + 1 < src.len() && row < h {
        let (n, v) = (usize::from(src[i]), src[i + 1]);
        i += 2;
        if n > 0 {
            match kind {
                Compression::Rle8 => {
                    let c = colour(usize::from(v));
                    for k in 0..n {
                        put(x + k, row, c);
                    }
                }
                Compression::Rle4 => {
                    let (hi, lo) = (colour(usize::from(v >> 4)), colour(usize::from(v & 15)));
                    for k in 0..n {
                        put(x + k, row, if k % 2 == 0 { hi } else { lo });
                    }
                }
                _ => {
                    // RLE24: a count, then a 3-byte BGR value (the first
                    // byte of which is `v`).
                    let Some(&[g, r]) = src.get(i..i + 2).map(|s| [s[0], s[1]]).as_ref() else { break };
                    i += 2;
                    for k in 0..n {
                        put(x + k, row, [r, g, v]);
                    }
                }
            }
            x += n;
            continue;
        }
        match v {
            0 => {
                x = 0;
                row += 1;
            }
            1 => break,
            2 => {
                let Some(d) = src.get(i..i + 2) else { break };
                x += usize::from(d[0]);
                row += usize::from(d[1]);
                i += 2;
            }
            n => {
                let n = usize::from(n);
                let bytes = match kind {
                    Compression::Rle8 => n,
                    Compression::Rle4 => n.div_ceil(2),
                    _ => n * 3,
                };
                let Some(run) = src.get(i..i + bytes) else { break };
                for k in 0..n {
                    let c = match kind {
                        Compression::Rle8 => colour(usize::from(run[k])),
                        Compression::Rle4 => {
                            let b = run[k / 2];
                            colour(usize::from(if k % 2 == 0 { b >> 4 } else { b & 15 }))
                        }
                        _ => [run[k * 3 + 2], run[k * 3 + 1], run[k * 3]],
                    };
                    put(x + k, row, c);
                }
                x += n;
                // Absolute runs are padded to a 16-bit boundary.
                i += bytes + bytes % 2;
            }
        }
    }
    rgba.as_chunks::<4>().0.iter().any(|p| p[3] == 0)
}
