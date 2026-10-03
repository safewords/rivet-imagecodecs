//! Writing a GIF89a from RGBA frames.

use crate::decode::Disposal;
use crate::quantize::{Histogram, Mapper};
use crate::{Error, Result, lzw};

/// Where each frame's colours come from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PaletteMode {
    /// Each frame gets its own local colour table, built from the pixels it
    /// changes: exact when they have few enough colours, else median cut.
    #[default]
    PerFrame,
    /// One global colour table for every frame (at most 255 colours: one
    /// index is kept for transparency). [`crate::quantize::Histogram`] can
    /// build it from all the frames.
    Fixed(Vec<[u8; 3]>),
}

/// How to write a GIF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeOptions {
    /// The NETSCAPE2.0 loop count: `None` writes no extension (viewers play
    /// once), `Some(0)` loops forever, `Some(n)` stores `n`.
    pub loop_count: Option<u16>,
    /// Floyd–Steinberg error diffusion when a frame has more colours than
    /// its palette. Never applied to frames that fit exactly.
    pub dither: bool,
    /// The most colours a frame's palette may have, 2 ..= 256 (one goes to
    /// transparency when a frame needs it).
    pub max_colors: u16,
    /// Pixels with alpha below this are transparent; the rest are opaque.
    /// GIF has no partial transparency.
    pub alpha_threshold: u8,
    /// The palette strategy.
    pub palette: PaletteMode,
    /// Crop each frame to what changed from the one before and make
    /// unchanged pixels inside the crop transparent (so they show through).
    /// Off, every frame is written whole.
    pub differencing: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            loop_count: None,
            dither: false,
            max_colors: 256,
            alpha_threshold: 128,
            palette: PaletteMode::PerFrame,
            differencing: true,
        }
    }
}

/// Encode one still image (`width × height × 4` bytes of RGBA).
pub fn encode(width: u16, height: u16, rgba: &[u8], options: &EncodeOptions) -> Result<Vec<u8>> {
    let mut e = Encoder::new(width, height, options.clone())?;
    e.add_frame(rgba, 0)?;
    e.finish()
}

/// Writes an animated (or still) GIF frame by frame.
///
/// Each frame is held until the next arrives, because how it is disposed of
/// depends on what follows: when the next frame needs pixels to become
/// transparent, this one is cleared ("restore to background") after it is
/// shown; otherwise it stays and the next frame draws only what changed.
///
/// ```
/// let red = [255, 0, 0, 255].repeat(4);
/// let blue = [0, 0, 255, 255].repeat(4);
/// let options = gif::EncodeOptions { loop_count: Some(0), ..Default::default() };
/// let mut e = gif::Encoder::new(2, 2, options)?;
/// e.add_frame(&red, 50)?;
/// e.add_frame(&blue, 50)?;
/// let bytes = e.finish()?;
/// let anim = gif::decode(&bytes)?;
/// assert_eq!(anim.frames[1].rgba, blue);
/// assert_eq!(anim.loop_count, Some(0));
/// # Ok::<(), gif::Error>(())
/// ```
pub struct Encoder {
    width: usize,
    height: usize,
    options: EncodeOptions,
    out: Vec<u8>,
    /// The canvas a viewer holds before the pending frame draws, in the
    /// frames' own colours (transparent pixels are all zero).
    base: Vec<u8>,
    pending: Option<(Vec<u8>, u16)>,
    written: usize,
    fixed: Option<Mapper>,
}

impl Encoder {
    /// Start a GIF of the given screen size; writes the header.
    pub fn new(width: u16, height: u16, options: EncodeOptions) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::BadInput(format!("a {width}x{height} image")));
        }
        if !(2..=256).contains(&options.max_colors) {
            return Err(Error::BadInput(format!("max_colors {} is outside 2..=256", options.max_colors)));
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"GIF89a");
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        let fixed = match &options.palette {
            PaletteMode::PerFrame => {
                // No global table; colour resolution 8 bits.
                out.extend_from_slice(&[0x70, 0, 0]);
                None
            }
            PaletteMode::Fixed(p) => {
                if p.is_empty() || p.len() > 255 {
                    return Err(Error::BadInput(format!("a fixed palette of {} colours (1 ..= 255)", p.len())));
                }
                let bits = table_bits(p.len() + 1);
                out.extend_from_slice(&[0xF0 | (bits - 1), 0, 0]);
                write_table(&mut out, p, bits);
                Some(Mapper::new(p.clone()))
            }
        };
        if let Some(n) = options.loop_count {
            out.extend_from_slice(&[0x21, 0xFF, 11]);
            out.extend_from_slice(b"NETSCAPE2.0");
            out.extend_from_slice(&[3, 1]);
            out.extend_from_slice(&n.to_le_bytes());
            out.push(0);
        }
        let (w, h) = (usize::from(width), usize::from(height));
        Ok(Self { width: w, height: h, options, out, base: vec![0; w * h * 4], pending: None, written: 0, fixed })
    }

    /// Add a frame: `width × height × 4` bytes of RGBA, shown for `delay_cs`
    /// hundredths of a second.
    pub fn add_frame(&mut self, rgba: &[u8], delay_cs: u16) -> Result<()> {
        if rgba.len() != self.width * self.height * 4 {
            return Err(Error::BadInput(format!(
                "a frame of {} bytes for a {}x{} screen (expected {})",
                rgba.len(),
                self.width,
                self.height,
                self.width * self.height * 4
            )));
        }
        let threshold = self.options.alpha_threshold;
        let mut frame = rgba.to_vec();
        for px in frame.as_chunks_mut::<4>().0 {
            if px[3] < threshold {
                px.copy_from_slice(&[0; 4]);
            } else {
                px[3] = 255;
            }
        }
        if let Some((prev, delay)) = self.pending.take() {
            self.write_frame(&prev, delay, Some(&frame));
        }
        self.pending = Some((frame, delay_cs));
        Ok(())
    }

    /// Write the last frame and the trailer, and return the file.
    pub fn finish(mut self) -> Result<Vec<u8>> {
        let Some((last, delay)) = self.pending.take() else {
            return Err(Error::BadInput("no frames".into()));
        };
        self.write_frame(&last, delay, None);
        self.out.push(0x3B);
        Ok(self.out)
    }

    fn write_frame(&mut self, target: &[u8], delay: u16, next: Option<&[u8]>) {
        let (w, h) = (self.width, self.height);
        let differs = |i: usize| target[i * 4..i * 4 + 4] != self.base[i * 4..i * 4 + 4];
        let whole = self.written == 0 || !self.options.differencing;

        // The rectangle to draw, and whether pixels the next frame needs
        // transparent force this one to be cleared after it is shown.
        let mut bbox = Bbox::default();
        if whole {
            bbox.add(0, 0);
            bbox.add(w - 1, h - 1);
        }
        let mut clear = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let goes_clear = next.is_some_and(|n| n[i * 4 + 3] == 0 && target[i * 4 + 3] != 0);
                clear |= goes_clear;
                if !whole && (goes_clear || differs(i)) {
                    bbox.add(x, y);
                }
            }
        }
        let (x0, y0, x1, y1) = bbox.get().unwrap_or((0, 0, 1, 1));
        let (rw, rh) = (x1 - x0, y1 - y0);
        let disposal = if clear {
            Disposal::Background
        } else if next.is_none() && self.written == 0 {
            Disposal::Unspecified
        } else {
            Disposal::Keep
        };

        // Classify the pixels of the rectangle: transparent (shows what is
        // underneath: either nothing, or the same colour already there) or
        // a colour to draw.
        let mut draw = vec![false; rw * rh];
        let mut needs_transparent = false;
        for y in 0..rh {
            for x in 0..rw {
                let i = (y0 + y) * w + x0 + x;
                let opaque = target[i * 4 + 3] != 0;
                let d = opaque && (!self.options.differencing || self.written == 0 || differs(i));
                draw[y * rw + x] = d;
                needs_transparent |= !d;
            }
        }

        // The palette.
        let (mapper, transparent, local) = match &self.fixed {
            Some(m) => (m.clone(), m.palette().len() as u8, false),
            None => {
                let mut hist = Histogram::new();
                for y in 0..rh {
                    for x in 0..rw {
                        if draw[y * rw + x] {
                            let i = ((y0 + y) * w + x0 + x) * 4;
                            hist.add([target[i], target[i + 1], target[i + 2]]);
                        }
                    }
                }
                let max = usize::from(self.options.max_colors) - usize::from(needs_transparent);
                let palette = if hist.distinct() == 0 { vec![[0, 0, 0]] } else { hist.palette(max) };
                let t = palette.len() as u8;
                (Mapper::new(palette), t, true)
            }
        };
        let mut mapper = mapper;

        let mut indices = vec![transparent; rw * rh];
        let dither = self.options.dither;
        let mut err_cur = vec![[0i32; 3]; rw + 2];
        let mut err_next = vec![[0i32; 3]; rw + 2];
        for y in 0..rh {
            for x in 0..rw {
                if !draw[y * rw + x] {
                    continue;
                }
                let i = ((y0 + y) * w + x0 + x) * 4;
                let src = [target[i], target[i + 1], target[i + 2]];
                if !dither {
                    indices[y * rw + x] = mapper.index(src);
                    continue;
                }
                let e = err_cur[x + 1];
                let want: [i32; 3] = std::array::from_fn(|a| (i32::from(src[a]) + ((e[a] + 8) >> 4)).clamp(0, 255));
                let idx = mapper.index(want.map(|v| v as u8));
                indices[y * rw + x] = idx;
                let got = mapper.palette()[usize::from(idx)];
                for a in 0..3 {
                    let d = want[a] - i32::from(got[a]);
                    err_cur[x + 2][a] += d * 7;
                    err_next[x][a] += d * 3;
                    err_next[x + 1][a] += d * 5;
                    err_next[x + 2][a] += d;
                }
            }
            std::mem::swap(&mut err_cur, &mut err_next);
            err_next.fill([0; 3]);
        }

        // Graphic Control Extension.
        let animated = self.written > 0 || next.is_some();
        if needs_transparent || animated || delay != 0 {
            let flags = (disposal.bits() << 2) | u8::from(needs_transparent);
            self.out.extend_from_slice(&[0x21, 0xF9, 4, flags]);
            self.out.extend_from_slice(&delay.to_le_bytes());
            self.out.extend_from_slice(&[if needs_transparent { transparent } else { 0 }, 0]);
        }

        // Image descriptor, table and data.
        self.out.push(0x2C);
        for v in [x0, y0, rw, rh] {
            self.out.extend_from_slice(&(v as u16).to_le_bytes());
        }
        let table_len = if local { mapper.palette().len() + usize::from(needs_transparent) } else { usize::from(transparent) + 1 };
        let bits = table_bits(table_len);
        if local {
            self.out.push(0x80 | (bits - 1));
            write_table(&mut self.out, mapper.palette(), bits);
        } else {
            self.out.push(0);
        }
        let min_size = bits.max(2);
        self.out.push(min_size);
        let code = lzw::encode(min_size, &indices);
        for chunk in code.chunks(255) {
            self.out.push(chunk.len() as u8);
            self.out.extend_from_slice(chunk);
        }
        self.out.push(0);

        // What the viewer holds afterwards.
        self.base.copy_from_slice(target);
        if disposal == Disposal::Background {
            for y in y0..y1 {
                self.base[(y * w + x0) * 4..(y * w + x1) * 4].fill(0);
            }
        }
        self.written += 1;
    }
}

#[derive(Default)]
struct Bbox(Option<(usize, usize, usize, usize)>);

impl Bbox {
    fn add(&mut self, x: usize, y: usize) {
        self.0 = Some(match self.0 {
            None => (x, y, x + 1, y + 1),
            Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
        });
    }

    fn get(&self) -> Option<(usize, usize, usize, usize)> {
        self.0
    }
}

/// Bits per index for a table of `len` entries (1 ..= 8).
fn table_bits(len: usize) -> u8 {
    let mut bits = 1u8;
    while (1usize << bits) < len {
        bits += 1;
    }
    bits
}

fn write_table(out: &mut Vec<u8>, palette: &[[u8; 3]], bits: u8) {
    for i in 0..(1usize << bits) {
        out.extend_from_slice(&palette.get(i).copied().unwrap_or([0; 3]));
    }
}
