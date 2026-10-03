//! Writing TIFF files.

use crate::ifd::{tag, ty};
use crate::{Compression, Error, Result, lzw, packbits};

/// The pixel layout of an image to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum PixelFormat {
    Gray8,
    Gray16,
    GrayAlpha8,
    GrayAlpha16,
    Rgb8,
    Rgb16,
    Rgba8,
    Rgba16,
    Gray32F,
    Rgb32F,
    Rgba32F,
}

impl PixelFormat {
    /// Samples per pixel.
    pub fn channels(self) -> usize {
        match self {
            Self::Gray8 | Self::Gray16 | Self::Gray32F => 1,
            Self::GrayAlpha8 | Self::GrayAlpha16 => 2,
            Self::Rgb8 | Self::Rgb16 | Self::Rgb32F => 3,
            Self::Rgba8 | Self::Rgba16 | Self::Rgba32F => 4,
        }
    }

    fn bits(self) -> usize {
        match self {
            Self::Gray8 | Self::GrayAlpha8 | Self::Rgb8 | Self::Rgba8 => 8,
            Self::Gray16 | Self::GrayAlpha16 | Self::Rgb16 | Self::Rgba16 => 16,
            Self::Gray32F | Self::Rgb32F | Self::Rgba32F => 32,
        }
    }

    fn float(self) -> bool {
        self.bits() == 32
    }

    fn alpha(self) -> bool {
        matches!(self, Self::GrayAlpha8 | Self::GrayAlpha16 | Self::Rgba8 | Self::Rgba16 | Self::Rgba32F)
    }

    fn rgb(self) -> bool {
        self.channels() >= 3
    }
}

/// Samples to write, interleaved, rows top to bottom.
#[derive(Debug, Clone, Copy)]
#[allow(missing_docs)]
pub enum SampleData<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    F32(&'a [f32]),
}

/// How to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions {
    /// [`Compression::None`], [`Compression::PackBits`],
    /// [`Compression::Lzw`] or [`Compression::Deflate`].
    pub compression: Compression,
    /// Apply the horizontal predictor (or, for floats, the floating-point
    /// predictor) before LZW or Deflate.
    pub predictor: bool,
    /// Deflate level, 0 ..= 9.
    pub deflate_level: u8,
    /// The uncompressed size a strip aims at; strips hold whole rows.
    pub strip_bytes: usize,
    /// Write BigTIFF (64-bit offsets). Files over 4 GiB are written as
    /// BigTIFF whatever this says.
    pub big_tiff: bool,
    /// Write big-endian (`MM`) instead of little-endian (`II`).
    pub big_endian: bool,
}

impl Default for EncodeOptions {
    /// LZW with the predictor, 64 KiB strips, classic little-endian TIFF.
    fn default() -> Self {
        Self {
            compression: Compression::Lzw,
            predictor: true,
            deflate_level: 6,
            strip_bytes: 1 << 16,
            big_tiff: false,
            big_endian: false,
        }
    }
}

/// Encode one page.
pub fn encode(
    width: u32,
    height: u32,
    format: PixelFormat,
    data: SampleData,
    options: &EncodeOptions,
) -> Result<Vec<u8>> {
    let mut e = Encoder::new(*options)?;
    e.add_page(width, height, format, data)?;
    e.finish()
}

struct EncodedPage {
    width: u32,
    height: u32,
    format: PixelFormat,
    rows_per_strip: u32,
    strips: Vec<Vec<u8>>,
}

/// Writes a TIFF of one or more pages.
///
/// ```
/// let gray: Vec<u16> = (0..64).map(|i| i * 1000).collect();
/// let mut e = tiff::Encoder::new(Default::default())?;
/// e.add_page(8, 8, tiff::PixelFormat::Gray16, tiff::SampleData::U16(&gray))?;
/// e.add_page(8, 8, tiff::PixelFormat::Gray16, tiff::SampleData::U16(&gray))?;
/// let bytes = e.finish()?;
/// let pages = tiff::decode_pages(&bytes)?;
/// assert_eq!(pages.len(), 2);
/// assert_eq!(pages[1].samples, tiff::Samples::U16(gray));
/// # Ok::<(), tiff::Error>(())
/// ```
pub struct Encoder {
    options: EncodeOptions,
    pages: Vec<EncodedPage>,
}

impl Encoder {
    /// An encoder with no pages yet.
    pub fn new(options: EncodeOptions) -> Result<Self> {
        if !matches!(
            options.compression,
            Compression::None | Compression::PackBits | Compression::Lzw | Compression::Deflate
        ) {
            return Err(Error::BadInput(format!("writing {:?} compression", options.compression)));
        }
        if options.deflate_level > 9 {
            return Err(Error::BadInput(format!("Deflate level {}", options.deflate_level)));
        }
        Ok(Self { options, pages: Vec::new() })
    }

    /// Compress and keep a page.
    pub fn add_page(&mut self, width: u32, height: u32, format: PixelFormat, data: SampleData) -> Result<()> {
        if width == 0 || height == 0 {
            return Err(Error::BadInput(format!("a {width}x{height} page")));
        }
        let ch = format.channels();
        let n = (width as usize).checked_mul(height as usize).and_then(|p| p.checked_mul(ch));
        let n = n.ok_or_else(|| Error::BadInput("page too large".into()))?;
        let be = self.options.big_endian;
        // Samples as file bytes.
        let bytes: Vec<u8> = match (data, format.bits(), format.float()) {
            (SampleData::U8(v), 8, false) if v.len() == n => v.to_vec(),
            (SampleData::U16(v), 16, false) if v.len() == n => {
                v.iter().flat_map(|&s| if be { s.to_be_bytes() } else { s.to_le_bytes() }).collect()
            }
            (SampleData::F32(v), 32, true) if v.len() == n => {
                v.iter().flat_map(|&s| if be { s.to_be_bytes() } else { s.to_le_bytes() }).collect()
            }
            _ => {
                return Err(Error::BadInput(format!(
                    "sample data does not match {format:?} at {width}x{height} ({n} samples)"
                )));
            }
        };
        let row_bytes = width as usize * ch * format.bits() / 8;
        let rows_per_strip = (self.options.strip_bytes / row_bytes).clamp(1, height as usize) as u32;
        let mut strips = Vec::new();
        for chunk in bytes.chunks(row_bytes * rows_per_strip as usize) {
            let mut raw = chunk.to_vec();
            let compressing = matches!(self.options.compression, Compression::Lzw | Compression::Deflate);
            if self.options.predictor && compressing {
                if format.float() {
                    float_predict(&mut raw, row_bytes, width as usize * ch, ch, be);
                } else {
                    horizontal_predict(&mut raw, row_bytes, ch, format.bits() / 8, be);
                }
            }
            strips.push(match self.options.compression {
                Compression::None => raw,
                Compression::Lzw => lzw::encode(&raw),
                Compression::Deflate => rpng::deflate::zlib_compress(&raw, self.options.deflate_level),
                _ => {
                    let mut out = Vec::new();
                    for row in raw.chunks(row_bytes) {
                        packbits::encode_row(row, &mut out);
                    }
                    out
                }
            });
        }
        self.pages.push(EncodedPage { width, height, format, rows_per_strip, strips });
        Ok(())
    }

    /// Write the file.
    pub fn finish(self) -> Result<Vec<u8>> {
        if self.pages.is_empty() {
            return Err(Error::BadInput("no pages".into()));
        }
        let data_size: u64 = self.pages.iter().flat_map(|p| &p.strips).map(|s| s.len() as u64 + 1).sum();
        let big = self.options.big_tiff || data_size > u64::from(u32::MAX) - (1 << 24);
        let mut w = Writer { out: Vec::new(), big, be: self.options.big_endian };
        w.out.extend_from_slice(if w.be { b"MM" } else { b"II" });
        if big {
            w.u16(43);
            w.u16(8);
            w.u16(0);
        } else {
            w.u16(42);
        }
        // Where the first directory's offset goes.
        let mut link = w.out.len();
        w.offset(0);
        let total = self.pages.len();
        for (index, page) in self.pages.iter().enumerate() {
            let mut offsets = Vec::new();
            for s in &page.strips {
                w.align();
                offsets.push(w.out.len() as u64);
                w.out.extend_from_slice(s);
            }
            let counts: Vec<u64> = page.strips.iter().map(|s| s.len() as u64).collect();
            w.align();
            let at = w.out.len() as u64;
            w.patch(link, at);
            link = w.directory(page, &offsets, &counts, &self.options, index, total);
        }
        Ok(w.out)
    }
}

/// Horizontal differencing (Predictor 2), row by row.
fn horizontal_predict(buf: &mut [u8], row_bytes: usize, ch: usize, bytes: usize, be: bool) {
    for row in buf.chunks_mut(row_bytes) {
        if bytes == 1 {
            for i in (ch..row.len()).rev() {
                row[i] = row[i].wrapping_sub(row[i - ch]);
            }
        } else {
            let get = |row: &[u8], i: usize| {
                let b = [row[i * 2], row[i * 2 + 1]];
                if be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
            };
            let n = row.len() / 2;
            for i in (ch..n).rev() {
                let v = get(row, i).wrapping_sub(get(row, i - ch));
                let b = if be { v.to_be_bytes() } else { v.to_le_bytes() };
                row[i * 2..i * 2 + 2].copy_from_slice(&b);
            }
        }
    }
}

/// The floating-point predictor (Predictor 3): each row's `n` samples are
/// regrouped into byte planes, most significant first, then differenced
/// bytewise with a stride of one pixel (`ch` bytes).
fn float_predict(buf: &mut [u8], row_bytes: usize, n: usize, ch: usize, be: bool) {
    let mut tmp = vec![0u8; row_bytes];
    for row in buf.chunks_mut(row_bytes) {
        for i in 0..n {
            for k in 0..4 {
                let at = if be { k } else { 3 - k };
                tmp[k * n + i] = row[i * 4 + at];
            }
        }
        for i in (ch..row_bytes).rev() {
            tmp[i] = tmp[i].wrapping_sub(tmp[i - ch]);
        }
        row.copy_from_slice(&tmp);
    }
}

struct Writer {
    out: Vec<u8>,
    big: bool,
    be: bool,
}

impl Writer {
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&if self.be { v.to_be_bytes() } else { v.to_le_bytes() });
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&if self.be { v.to_be_bytes() } else { v.to_le_bytes() });
    }

    fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&if self.be { v.to_be_bytes() } else { v.to_le_bytes() });
    }

    fn offset(&mut self, v: u64) {
        if self.big { self.u64(v) } else { self.u32(v as u32) }
    }

    fn patch(&mut self, at: usize, v: u64) {
        let b: Vec<u8> = if self.big {
            if self.be { v.to_be_bytes().to_vec() } else { v.to_le_bytes().to_vec() }
        } else if self.be {
            (v as u32).to_be_bytes().to_vec()
        } else {
            (v as u32).to_le_bytes().to_vec()
        };
        self.out[at..at + b.len()].copy_from_slice(&b);
    }

    fn align(&mut self) {
        if self.out.len() % 2 == 1 {
            self.out.push(0);
        }
    }

    /// Write a page's directory at the current (even) position; returns
    /// where its next-directory offset is.
    fn directory(
        &mut self,
        page: &EncodedPage,
        offsets: &[u64],
        counts: &[u64],
        options: &EncodeOptions,
        index: usize,
        total: usize,
    ) -> usize {
        let f = page.format;
        let ch = f.channels() as u64;
        let bits = f.bits() as u64;
        let long_ty = if self.big { ty::LONG8 } else { ty::LONG };
        // (tag, type, values); values are written in the type's size.
        let mut entries: Vec<(u16, u16, Vec<u64>)> = vec![
            (tag::NEW_SUBFILE_TYPE, ty::LONG, vec![if total > 1 { 2 } else { 0 }]),
            (tag::IMAGE_WIDTH, ty::LONG, vec![u64::from(page.width)]),
            (tag::IMAGE_LENGTH, ty::LONG, vec![u64::from(page.height)]),
            (tag::BITS_PER_SAMPLE, ty::SHORT, vec![bits; ch as usize]),
            (tag::COMPRESSION, ty::SHORT, vec![u64::from(options.compression.code())]),
            (tag::PHOTOMETRIC, ty::SHORT, vec![if f.rgb() { 2 } else { 1 }]),
            (tag::STRIP_OFFSETS, long_ty, offsets.to_vec()),
            (tag::SAMPLES_PER_PIXEL, ty::SHORT, vec![ch]),
            (tag::ROWS_PER_STRIP, ty::LONG, vec![u64::from(page.rows_per_strip)]),
            (tag::STRIP_BYTE_COUNTS, long_ty, counts.to_vec()),
            (tag::X_RESOLUTION, ty::RATIONAL, vec![72, 1]),
            (tag::Y_RESOLUTION, ty::RATIONAL, vec![72, 1]),
            (tag::PLANAR_CONFIGURATION, ty::SHORT, vec![1]),
            (tag::RESOLUTION_UNIT, ty::SHORT, vec![2]),
            (tag::SOFTWARE, ty::ASCII, b"rivet-tiff\0".iter().map(|&b| u64::from(b)).collect()),
        ];
        if total > 1 {
            entries.push((tag::PAGE_NUMBER, ty::SHORT, vec![index as u64, total as u64]));
        }
        let compressing = matches!(options.compression, Compression::Lzw | Compression::Deflate);
        if options.predictor && compressing {
            entries.push((tag::PREDICTOR, ty::SHORT, vec![if f.float() { 3 } else { 2 }]));
        }
        if f.alpha() {
            entries.push((tag::EXTRA_SAMPLES, ty::SHORT, vec![2]));
        }
        entries.push((tag::SAMPLE_FORMAT, ty::SHORT, vec![if f.float() { 3 } else { 1 }; ch as usize]));
        entries.sort_by_key(|e| e.0);

        let size = |t: u16| match t {
            ty::BYTE | ty::ASCII => 1,
            ty::SHORT => 2,
            ty::LONG => 4,
            _ => 8,
        };
        let (count_size, entry_size, inline) = if self.big { (8, 20, 8) } else { (2, 12, 4) };
        let start = self.out.len();
        let mut extra = start + count_size + entries.len() * entry_size + if self.big { 8 } else { 4 };
        let mut overflow: Vec<u8> = Vec::new();
        if self.big {
            self.u64(entries.len() as u64);
        } else {
            self.u16(entries.len() as u16);
        }
        let encode_values = |w: &Writer, t: u16, vals: &[u64]| -> Vec<u8> {
            let mut b = Vec::new();
            for &v in vals {
                match size(t) {
                    1 => b.push(v as u8),
                    2 => b.extend_from_slice(&if w.be { (v as u16).to_be_bytes() } else { (v as u16).to_le_bytes() }),
                    4 => b.extend_from_slice(&if w.be { (v as u32).to_be_bytes() } else { (v as u32).to_le_bytes() }),
                    _ if t == ty::RATIONAL => {
                        b.extend_from_slice(&if w.be { (v as u32).to_be_bytes() } else { (v as u32).to_le_bytes() })
                    }
                    _ => b.extend_from_slice(&if w.be { v.to_be_bytes() } else { v.to_le_bytes() }),
                }
            }
            b
        };
        for (t, typ, vals) in &entries {
            self.u16(*t);
            self.u16(*typ);
            // A RATIONAL is two LONGs: `vals` holds numerator, denominator.
            let count = if *typ == ty::RATIONAL { vals.len() / 2 } else { vals.len() };
            if self.big {
                self.u64(count as u64);
            } else {
                self.u32(count as u32);
            }
            let mut bytes = encode_values(self, *typ, vals);
            if bytes.len() <= inline {
                bytes.resize(inline, 0);
                self.out.extend_from_slice(&bytes);
            } else {
                let at = extra as u64;
                self.offset(at);
                if bytes.len() % 2 == 1 {
                    bytes.push(0);
                }
                extra += bytes.len();
                overflow.extend_from_slice(&bytes);
            }
        }
        let next = self.out.len();
        self.offset(0);
        self.out.extend_from_slice(&overflow);
        next
    }
}
