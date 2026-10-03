//! Test support for the rivet image codecs: where the public corpora are, a
//! small PNG and PNM reader for their reference renderings, a deterministic
//! random number generator, and ways of damaging files.
//!
//! The PNG reader exists only to read the corpora's reference images; it
//! uses the TIFF crate's inflate (included by path, so the test support does
//! not depend on a codec crate it helps test).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

#[path = "../../rivet-tiff/src/flate/inflate.rs"]
#[allow(dead_code)]
mod inflate;

/// The directory holding the corpora `tools/fetch_corpora.py` fetches:
/// `$RIVET_IMAGE_CORPORA`, else `corpora/` at the workspace root. `None`
/// (with a note on stderr) when it does not exist.
pub fn corpora() -> Option<PathBuf> {
    let dir = std::env::var_os("RIVET_IMAGE_CORPORA")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpora"));
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("corpora not found at {} — run tools/fetch_corpora.py; skipping", dir.display());
        None
    }
}

/// One corpus's directory (`bmpsuite`, `libtiff-pics`, `libtiff-test`,
/// `gif-suite`), if fetched.
pub fn corpus(name: &str) -> Option<PathBuf> {
    let d = corpora()?.join(name);
    if d.is_dir() {
        Some(d)
    } else {
        eprintln!("corpus {name} not found at {}; skipping", d.display());
        None
    }
}

/// A small xorshift generator, so damaged inputs are the same every run.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeded (a zero seed is replaced).
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// A random byte.
    pub fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

/// Damaged versions of `data`: every truncation at a spread of lengths, and
/// `flips` copies with a few random bytes changed (some in the first 64
/// bytes, where headers live).
pub fn damaged(data: &[u8], seed: u64, flips: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let n = data.len();
    let mut cuts: Vec<usize> = (0..n.min(80)).collect();
    let step = (n / 60).max(1);
    cuts.extend((80..n).step_by(step));
    for c in cuts {
        out.push(data[..c].to_vec());
    }
    let mut rng = Rng::new(seed);
    for k in 0..flips {
        let mut d = data.to_vec();
        if d.is_empty() {
            break;
        }
        let changes = 1 + rng.below(8);
        for _ in 0..changes {
            let at = if k % 2 == 0 { rng.below(d.len().min(64)) } else { rng.below(d.len()) };
            d[at] = rng.byte();
        }
        out.push(d);
    }
    out
}

/// A decoded reference image: 8-bit RGBA, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub data: Vec<u8>,
}

/// Read a PNG (any colour type and bit depth, non-interlaced or Adam7) as
/// 8-bit RGBA; 16-bit samples keep their high byte.
pub fn read_png(bytes: &[u8]) -> Result<Rgba, String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("not a PNG".into());
    }
    let mut pos = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut idat = Vec::new();
    let mut plte: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let body = bytes.get(pos + 8..pos + 8 + len).ok_or("truncated chunk")?;
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes(body[0..4].try_into().unwrap());
                h = u32::from_be_bytes(body[4..8].try_into().unwrap());
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => plte = body.as_chunks::<3>().0.to_vec(),
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let raw = inflate::zlib_decompress(&idat, 1 << 30, false).map_err(|e| e.to_string())?;
    let channels = match ctype {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(format!("colour type {ctype}")),
    };
    let bpp_bits = channels * usize::from(depth);
    let bpp = bpp_bits.div_ceil(8).max(1);
    let mut data = vec![0u8; w as usize * h as usize * 4];
    // (x0, y0, dx, dy) per pass.
    let passes: &[(usize, usize, usize, usize)] = if interlace == 1 {
        &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut at = 0;
    for &(x0, y0, dx, dy) in passes {
        let pw = (w as usize).saturating_sub(x0).div_ceil(dx);
        let ph = (h as usize).saturating_sub(y0).div_ceil(dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = (pw * bpp_bits).div_ceil(8);
        let mut prev = vec![0u8; stride];
        for row in 0..ph {
            let filter = *raw.get(at).ok_or("short image data")?;
            let mut line = raw.get(at + 1..at + 1 + stride).ok_or("short image data")?.to_vec();
            at += 1 + stride;
            for i in 0..stride {
                let a = if i >= bpp { line[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                let p = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    4 => {
                        let pa = (i16::from(b) - i16::from(c)).abs();
                        let pb = (i16::from(a) - i16::from(c)).abs();
                        let pc = (i16::from(a) + i16::from(b) - 2 * i16::from(c)).abs();
                        if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }
                    }
                    _ => return Err(format!("filter {filter}")),
                };
                line[i] = line[i].wrapping_add(p);
            }
            let sample = |idx: usize| -> u16 {
                match depth {
                    16 => u16::from_be_bytes([line[idx * 2], line[idx * 2 + 1]]),
                    8 => u16::from(line[idx]),
                    d => {
                        let bit = idx * usize::from(d);
                        let byte = line[bit / 8];
                        let shift = 8 - usize::from(d) - bit % 8;
                        u16::from((byte >> shift) & ((1u8 << d) - 1))
                    }
                }
            };
            let to8 = |v: u16| -> u8 {
                match depth {
                    16 => (v >> 8) as u8,
                    8 => v as u8,
                    d => (u32::from(v) * 255 / ((1u32 << d) - 1)) as u8,
                }
            };
            for x in 0..pw {
                let px = match ctype {
                    0 => {
                        let v = sample(x);
                        let g = to8(v);
                        let transparent = trns.len() >= 2 && u16::from_be_bytes([trns[0], trns[1]]) == v;
                        [g, g, g, if transparent { 0 } else { 255 }]
                    }
                    2 => {
                        let s = [sample(x * 3), sample(x * 3 + 1), sample(x * 3 + 2)];
                        let transparent = trns.len() >= 6
                            && (0..3).all(|k| u16::from_be_bytes([trns[k * 2], trns[k * 2 + 1]]) == s[k]);
                        [to8(s[0]), to8(s[1]), to8(s[2]), if transparent { 0 } else { 255 }]
                    }
                    3 => {
                        let i = sample(x) as usize;
                        let c = plte.get(i).copied().unwrap_or([0; 3]);
                        [c[0], c[1], c[2], trns.get(i).copied().unwrap_or(255)]
                    }
                    4 => {
                        let g = to8(sample(x * 2));
                        [g, g, g, to8(sample(x * 2 + 1))]
                    }
                    _ => [to8(sample(x * 4)), to8(sample(x * 4 + 1)), to8(sample(x * 4 + 2)), to8(sample(x * 4 + 3))],
                };
                let (ox, oy) = (x0 + x * dx, y0 + row * dy);
                let o = (oy * w as usize + ox) * 4;
                data[o..o + 4].copy_from_slice(&px);
            }
            prev = line;
        }
    }
    Ok(Rgba { width: w, height: h, data })
}

/// A PNM image (P1–P6): samples as read (16-bit when maxval > 255),
/// `channels` 1 or 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pnm {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// 1 (P1, P2, P4, P5) or 3 (P3, P6).
    pub channels: u32,
    /// The declared maximum value (1 for bitmaps).
    pub maxval: u32,
    /// Samples, row by row. Bitmaps are 1 for black, 0 for white, as stored.
    pub samples: Vec<u16>,
}

/// Read a PNM file.
pub fn read_pnm(bytes: &[u8]) -> Result<Pnm, String> {
    let mut pos = 0;
    let mut token = || -> Result<String, String> {
        loop {
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] == b'#' {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                continue;
            }
            break;
        }
        let start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if start == pos {
            return Err("truncated PNM header".into());
        }
        Ok(String::from_utf8_lossy(&bytes[start..pos]).into_owned())
    };
    let magic = token()?;
    let num = |s: String| s.parse::<u32>().map_err(|e| e.to_string());
    let width = num(token()?)?;
    let height = num(token()?)?;
    let (channels, maxval) = match magic.as_str() {
        "P1" | "P4" => (1, 1),
        "P2" | "P5" => (1, num(token()?)?),
        "P3" | "P6" => (3, num(token()?)?),
        m => return Err(format!("PNM magic {m}")),
    };
    let n = (width * height * channels) as usize;
    let mut samples = Vec::with_capacity(n);
    match magic.as_str() {
        "P1" | "P2" | "P3" => {
            for _ in 0..n {
                samples.push(num(token()?)? as u16);
            }
        }
        _ => {
            let data = &bytes[pos + 1..];
            if magic == "P4" {
                let stride = (width as usize).div_ceil(8);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let b = *data.get(y * stride + x / 8).ok_or("short PNM")?;
                        samples.push(u16::from((b >> (7 - x % 8)) & 1));
                    }
                }
            } else if maxval > 255 {
                for i in 0..n {
                    samples.push(u16::from_be_bytes([
                        *data.get(i * 2).ok_or("short PNM")?,
                        *data.get(i * 2 + 1).ok_or("short PNM")?,
                    ]));
                }
            } else {
                samples.extend(data.get(..n).ok_or("short PNM")?.iter().map(|&b| u16::from(b)));
            }
        }
    }
    Ok(Pnm { width, height, channels, maxval, samples })
}
