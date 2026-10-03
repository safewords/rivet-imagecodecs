//! Test support for the rivet image codecs: where the public corpora are,
//! PNG (through rivet-png) and PNM readers for their reference renderings, a
//! deterministic random number generator, and ways of damaging files.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

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

/// Read a PNG (through rivet-png) as 8-bit RGBA, 16-bit samples rounded.
pub fn read_png(bytes: &[u8]) -> Result<Rgba, String> {
    let png = rpng::decode(bytes).map_err(|e| e.to_string())?;
    Ok(Rgba { width: png.image.width, height: png.image.height, data: png.image.to_rgba8() })
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
