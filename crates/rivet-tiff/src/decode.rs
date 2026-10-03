//! Reading TIFF pages: fields, strips and tiles, decompression, predictors,
//! sample unpacking and photometric interpretation.

use crate::ccitt::{self, Scheme};
use crate::ifd::{self, Format, Ifd, tag};
use crate::{
    ColorType, Compression, Error, Image, Limits, PageInfo, Photometric, Planar, Result, SampleFormat, Samples,
};
use crate::{lzw, packbits};

/// A TIFF file: its directories, read once, decoded page by page.
///
/// ```
/// let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
/// let bytes = tiff::encode(2, 2, tiff::PixelFormat::Rgb8, tiff::SampleData::U8(&rgb), &Default::default())?;
/// let file = tiff::Tiff::new(&bytes)?;
/// assert_eq!(file.page_count(), 1);
/// let image = file.decode(0)?;
/// assert_eq!(image.samples, tiff::Samples::U8(rgb.to_vec()));
/// # Ok::<(), tiff::Error>(())
/// ```
pub struct Tiff<'a> {
    data: &'a [u8],
    format: Format,
    ifds: Vec<Ifd>,
    limits: Limits,
}

impl<'a> Tiff<'a> {
    /// Read the header and every directory, with the default [`Limits`].
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Self::with_limits(data, Limits::default())
    }

    /// Read the header and every directory within `limits`.
    ///
    /// A directory chain that loops, or points outside the file after the
    /// first page, ends there.
    pub fn with_limits(data: &'a [u8], limits: Limits) -> Result<Self> {
        let (format, mut offset) = ifd::header(data)?;
        let mut ifds = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while offset != 0 && seen.insert(offset) {
            if ifds.len() >= limits.max_pages {
                break;
            }
            match ifd::read_ifd(data, format, offset) {
                Ok((d, next)) => {
                    ifds.push(d);
                    offset = next;
                }
                Err(e) if ifds.is_empty() => return Err(e),
                Err(_) => break,
            }
        }
        if ifds.is_empty() {
            return Err(Error::Invalid("no image file directory".into()));
        }
        Ok(Self { data, format, ifds, limits })
    }

    /// Whether the file is BigTIFF.
    pub fn is_big_tiff(&self) -> bool {
        self.format.big_tiff
    }

    /// Whether the file is big-endian (`MM`).
    pub fn is_big_endian(&self) -> bool {
        self.format.big_endian
    }

    /// How many pages (directories in the main chain) the file has.
    pub fn page_count(&self) -> usize {
        self.ifds.len()
    }

    /// What page `index`'s fields say.
    pub fn info(&self, index: usize) -> Result<PageInfo> {
        let ifd = self.ifds.get(index).ok_or_else(|| Error::Invalid(format!("no page {index}")))?;
        page_info(self.data, self.format, ifd)
    }

    /// Decode page `index`.
    pub fn decode(&self, index: usize) -> Result<Image> {
        let ifd = self.ifds.get(index).ok_or_else(|| Error::Invalid(format!("no page {index}")))?;
        let info = page_info(self.data, self.format, ifd)?;
        Page { data: self.data, f: self.format, ifd, info, limits: self.limits }.decode()
    }
}

/// What the first page's fields say.
pub fn read_info(data: &[u8]) -> Result<PageInfo> {
    Tiff::new(data)?.info(0)
}

/// Decode the first page with the default [`Limits`].
pub fn decode(data: &[u8]) -> Result<Image> {
    decode_with_limits(data, Limits::default())
}

/// Decode the first page within `limits`.
pub fn decode_with_limits(data: &[u8], limits: Limits) -> Result<Image> {
    Tiff::with_limits(data, limits)?.decode(0)
}

/// Decode every page with the default [`Limits`].
pub fn decode_pages(data: &[u8]) -> Result<Vec<Image>> {
    let t = Tiff::new(data)?;
    (0..t.page_count()).map(|i| t.decode(i)).collect()
}

fn page_info(data: &[u8], f: Format, ifd: &Ifd) -> Result<PageInfo> {
    let uint = |t: u16| ifd.uint(data, f, t);
    let width = uint(tag::IMAGE_WIDTH).ok_or_else(|| Error::Invalid("no ImageWidth".into()))?;
    let height = uint(tag::IMAGE_LENGTH).ok_or_else(|| Error::Invalid("no ImageLength".into()))?;
    let width = u32::try_from(width).map_err(|_| Error::Invalid(format!("width {width}")))?;
    let height = u32::try_from(height).map_err(|_| Error::Invalid(format!("height {height}")))?;
    let samples_per_pixel = uint(tag::SAMPLES_PER_PIXEL).unwrap_or(1).min(u64::from(u16::MAX)) as u16;
    let bps = ifd.uints(data, f, tag::BITS_PER_SAMPLE).unwrap_or_else(|| vec![1]);
    let bits_per_sample: Vec<u16> = bps.iter().map(|&b| b.min(u64::from(u16::MAX)) as u16).collect();
    let compression = Compression::from_code(uint(tag::COMPRESSION).unwrap_or(1) as u16);
    let photometric = match uint(tag::PHOTOMETRIC) {
        Some(v) => Photometric::from_code(v as u16),
        None if matches!(compression, Compression::CcittRle | Compression::Group3 | Compression::Group4) => {
            Photometric::WhiteIsZero
        }
        None if samples_per_pixel >= 3 => Photometric::Rgb,
        None => Photometric::BlackIsZero,
    };
    let sample_format = match ifd.uint(data, f, tag::SAMPLE_FORMAT).unwrap_or(1) {
        1 => SampleFormat::Uint,
        2 => SampleFormat::Int,
        3 => SampleFormat::Float,
        v => SampleFormat::Other(v as u16),
    };
    let planar = if uint(tag::PLANAR_CONFIGURATION) == Some(2) && samples_per_pixel > 1 {
        Planar::Separate
    } else {
        Planar::Chunky
    };
    // Tiles are normally located by TileOffsets; some pre-6.0 writers
    // (SGI's among them) put tile locations in StripOffsets, recognisable
    // by there being one per tile.
    let tile = match (uint(tag::TILE_WIDTH), uint(tag::TILE_LENGTH)) {
        (Some(tw), Some(th)) if tw > 0 && th > 0 => {
            let planes = if planar == Planar::Separate { u64::from(samples_per_pixel) } else { 1 };
            let tiles = u64::from(width).div_ceil(tw) * u64::from(height).div_ceil(th) * planes;
            let strips = ifd.entries.get(&tag::STRIP_OFFSETS).map(|e| e.count);
            (ifd.entries.contains_key(&tag::TILE_OFFSETS) || strips == Some(tiles))
                .then(|| (u32::try_from(tw).unwrap_or(0), u32::try_from(th).unwrap_or(0)))
        }
        _ => None,
    };
    let rows_per_strip = uint(tag::ROWS_PER_STRIP).map_or(height, |r| r.min(u64::from(height)) as u32).max(1);
    let extra_samples = ifd
        .uints(data, f, tag::EXTRA_SAMPLES)
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.min(u64::from(u16::MAX)) as u16)
        .collect();
    let resolution = match (
        ifd.floats(data, f, tag::X_RESOLUTION).and_then(|v| v.first().copied()),
        ifd.floats(data, f, tag::Y_RESOLUTION).and_then(|v| v.first().copied()),
    ) {
        (Some(x), Some(y)) => Some((x, y, uint(tag::RESOLUTION_UNIT).unwrap_or(2) as u16)),
        _ => None,
    };
    let page_number = ifd.uints(data, f, tag::PAGE_NUMBER).and_then(|v| match v[..] {
        [a, b, ..] => Some((a as u16, b as u16)),
        _ => None,
    });
    Ok(PageInfo {
        width,
        height,
        bits_per_sample,
        samples_per_pixel,
        sample_format,
        photometric,
        compression,
        planar,
        tile,
        rows_per_strip,
        predictor: uint(tag::PREDICTOR).unwrap_or(1) as u16,
        orientation: uint(tag::ORIENTATION).filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16,
        extra_samples,
        icc_profile: ifd.bytes(data, tag::ICC_PROFILE).map(<[u8]>::to_vec),
        resolution,
        subfile_type: uint(tag::NEW_SUBFILE_TYPE).unwrap_or(0) as u32,
        page_number,
    })
}

struct Page<'a> {
    data: &'a [u8],
    f: Format,
    ifd: &'a Ifd,
    info: PageInfo,
    limits: Limits,
}

/// Geometry of one chunk (strip or tile) as stored.
struct Chunk {
    /// Index into the offset arrays.
    index: usize,
    /// Plane (0 for chunky data).
    plane: usize,
    /// Top-left pixel in the image.
    x0: usize,
    y0: usize,
    /// Pixels per row and rows, as stored (tiles are always whole).
    width: usize,
    rows: usize,
}

impl Page<'_> {
    fn decode(self) -> Result<Image> {
        let info = &self.info;
        let (w, h) = (info.width as usize, info.height as usize);
        if w == 0 || h == 0 {
            return Err(Error::Invalid(format!("a {w}x{h} image")));
        }
        let pixels = u64::from(info.width) * u64::from(info.height);
        if pixels > self.limits.max_pixels {
            return Err(Error::LimitExceeded(format!(
                "{}x{} is over the limit of {} pixels",
                info.width, info.height, self.limits.max_pixels
            )));
        }
        let spp = usize::from(info.samples_per_pixel);
        if spp == 0 {
            return Err(Error::Invalid("SamplesPerPixel 0".into()));
        }
        let bps = info.bits_per_sample[0];
        if info.bits_per_sample.iter().any(|&b| b != bps) {
            return Err(Error::Unsupported(format!("mixed BitsPerSample {:?}", info.bits_per_sample)));
        }
        let bps = usize::from(bps);
        match info.sample_format {
            SampleFormat::Uint | SampleFormat::Int if (1..=32).contains(&bps) => {}
            SampleFormat::Float if matches!(bps, 16 | 32 | 64) => {}
            SampleFormat::Other(v) => return Err(Error::Unsupported(format!("SampleFormat {v}"))),
            _ => return Err(Error::Unsupported(format!("{bps}-bit {:?} samples", info.sample_format))),
        }
        match info.compression {
            Compression::None
            | Compression::CcittRle
            | Compression::Group3
            | Compression::Group4
            | Compression::Lzw
            | Compression::Deflate
            | Compression::PackBits => {}
            other => return Err(Error::Unsupported(format!("compression {other:?}"))),
        }
        let separate = info.planar == Planar::Separate;
        let planes = if separate { spp } else { 1 };
        let sp = if separate { 1 } else { spp };

        // YCbCr with subsampled chroma is stored in data units; those chunks
        // are expanded to full-resolution Y, Cb, Cr before anything else.
        let subsampling = if info.photometric == Photometric::YCbCr {
            let ss = self.ifd.uints(self.data, self.f, tag::YCBCR_SUBSAMPLING).unwrap_or_else(|| vec![2, 2]);
            let (sh, sv) = (ss.first().copied().unwrap_or(2), ss.get(1).copied().unwrap_or(2));
            if !matches!(sh, 1 | 2 | 4) || !matches!(sv, 1 | 2 | 4) {
                return Err(Error::Invalid(format!("YCbCrSubSampling {sh},{sv}")));
            }
            if (sh, sv) == (1, 1) {
                None
            } else if separate || spp != 3 || bps != 8 {
                return Err(Error::Unsupported("subsampled YCbCr other than 8-bit chunky".into()));
            } else {
                Some((sh as usize, sv as usize))
            }
        } else {
            None
        };

        // Image buffers: one per plane, rows padded to bytes.
        let row_bytes = (w * sp * bps).div_ceil(8);
        let plane_bytes = row_bytes.checked_mul(h).ok_or_else(|| Error::LimitExceeded("image size".into()))?;
        let total = (plane_bytes as u64).saturating_mul(planes as u64);
        if total > self.limits.max_alloc {
            return Err(Error::LimitExceeded(format!("{total} bytes of samples")));
        }
        let mut raw: Vec<Vec<u8>> = (0..planes).map(|_| vec![0u8; plane_bytes]).collect();

        let chunks = self.chunks(w, h, planes)?;
        let (offsets, counts) = self.offsets_and_counts()?;
        let mut first_error = None;
        for c in &chunks {
            let Some(&offset) = offsets.get(c.index) else { continue };
            let stored = match self.chunk(c, offset, counts.get(c.index).copied(), sp, bps, subsampling) {
                Ok(s) => s,
                Err(e) => {
                    first_error.get_or_insert(e);
                    continue;
                }
            };
            // Copy the chunk's rows into the image.
            let chunk_row = (c.width * sp * bps).div_ceil(8);
            let x_byte = c.x0 * sp * bps / 8;
            if !(c.x0 * sp * bps).is_multiple_of(8) {
                return Err(Error::Unsupported("tiles that do not start on a byte boundary".into()));
            }
            let len = chunk_row.min(row_bytes.saturating_sub(x_byte));
            let plane = &mut raw[c.plane];
            for r in 0..c.rows {
                let y = c.y0 + r;
                if y >= h {
                    break;
                }
                let src = &stored[r * chunk_row..r * chunk_row + len];
                plane[y * row_bytes + x_byte..y * row_bytes + x_byte + len].copy_from_slice(src);
            }
        }
        if let Some(e) = first_error {
            return Err(e);
        }
        self.interpret(&raw, row_bytes, sp, bps)
    }

    /// Strip or tile layout.
    fn chunks(&self, w: usize, h: usize, planes: usize) -> Result<Vec<Chunk>> {
        let mut out = Vec::new();
        match self.info.tile {
            Some((tw, th)) => {
                let (tw, th) = (tw as usize, th as usize);
                if tw == 0 || th == 0 {
                    return Err(Error::Invalid(format!("tiles of {tw}x{th}")));
                }
                if (tw as u64) * (th as u64) > self.limits.max_pixels {
                    return Err(Error::LimitExceeded(format!("{tw}x{th} tiles")));
                }
                let (across, down) = (w.div_ceil(tw), h.div_ceil(th));
                for p in 0..planes {
                    for ty in 0..down {
                        for tx in 0..across {
                            out.push(Chunk {
                                index: (p * down + ty) * across + tx,
                                plane: p,
                                x0: tx * tw,
                                y0: ty * th,
                                width: tw,
                                rows: th,
                            });
                        }
                    }
                }
            }
            None => {
                let rps = self.info.rows_per_strip as usize;
                let n = h.div_ceil(rps);
                for p in 0..planes {
                    for s in 0..n {
                        out.push(Chunk {
                            index: p * n + s,
                            plane: p,
                            x0: 0,
                            y0: s * rps,
                            width: w,
                            rows: rps.min(h - s * rps),
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    fn offsets_and_counts(&self) -> Result<(Vec<u64>, Vec<u64>)> {
        let (ot, ct) = if self.info.tile.is_some() && self.ifd.entries.contains_key(&tag::TILE_OFFSETS) {
            (tag::TILE_OFFSETS, tag::TILE_BYTE_COUNTS)
        } else {
            (tag::STRIP_OFFSETS, tag::STRIP_BYTE_COUNTS)
        };
        let offsets = self
            .ifd
            .uints(self.data, self.f, ot)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::Invalid("no strip or tile offsets".into()))?;
        let counts = self.ifd.uints(self.data, self.f, ct).unwrap_or_default();
        Ok((offsets, counts))
    }

    /// Read, decompress and un-predict one chunk; returns its rows packed
    /// at `chunk.width` pixels.
    fn chunk(
        &self,
        c: &Chunk,
        offset: u64,
        count: Option<u64>,
        sp: usize,
        bps: usize,
        subsampling: Option<(usize, usize)>,
    ) -> Result<Vec<u8>> {
        let info = &self.info;
        let chunk_row = (c.width * sp * bps).div_ceil(8);
        let expected = chunk_row * c.rows;
        // Stored size: the subsampled layout has its own.
        let stored_size = match subsampling {
            Some((sh, sv)) => c.width.div_ceil(sh) * c.rows.div_ceil(sv) * (sh * sv + 2),
            None => expected,
        };
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(self.data.len());
        let len = match count {
            Some(n) => usize::try_from(n).unwrap_or(usize::MAX),
            None if info.compression == Compression::None => stored_size,
            None => usize::MAX,
        };
        let mut src = &self.data[start..start.saturating_add(len).min(self.data.len())];
        let reversed;
        if self.ifd.uint(self.data, self.f, tag::FILL_ORDER) == Some(2) {
            reversed = src.iter().map(|b| b.reverse_bits()).collect::<Vec<u8>>();
            src = &reversed;
        }

        let mut buf = vec![0u8; stored_size];
        match info.compression {
            Compression::None => {
                let n = src.len().min(stored_size);
                buf[..n].copy_from_slice(&src[..n]);
            }
            Compression::PackBits => {
                packbits::decode(src, &mut buf);
            }
            Compression::Lzw => {
                lzw::decode(src, &mut buf).map_err(|_| Error::Invalid("an LZW code outside the table".into()))?;
            }
            Compression::Deflate => {
                // A last strip may hold whole RowsPerStrip rows; allow that
                // much (and a little) before calling it a bomb.
                let limit = stored_size.max(chunk_row * info.rows_per_strip as usize).saturating_add(1 << 16);
                let out = rpng::deflate::Inflater::new()
                    .limit(limit)
                    .check_adler(false)
                    .zlib(src)
                    .map_err(|e| Error::Invalid(format!("Deflate data: {e}")))?;
                let n = out.data.len().min(stored_size);
                buf[..n].copy_from_slice(&out.data[..n]);
            }
            Compression::CcittRle | Compression::Group3 | Compression::Group4 => {
                if bps != 1 || sp != 1 {
                    return Err(Error::Invalid("fax compression for other than bilevel data".into()));
                }
                let scheme = match info.compression {
                    Compression::CcittRle => Scheme::Mh,
                    Compression::Group3 => {
                        let opts = self.ifd.uint(self.data, self.f, tag::T4_OPTIONS).unwrap_or(0);
                        if opts & 2 != 0 {
                            return Err(Error::Unsupported("T.4 uncompressed mode".into()));
                        }
                        Scheme::G3 { two_d: opts & 1 != 0 }
                    }
                    _ => {
                        let opts = self.ifd.uint(self.data, self.f, tag::T6_OPTIONS).unwrap_or(0);
                        if opts & 2 != 0 {
                            return Err(Error::Unsupported("T.6 uncompressed mode".into()));
                        }
                        Scheme::G4
                    }
                };
                let (rows, _good, err) = ccitt::decode(src, scheme, c.width, c.rows);
                if let Some(e) = err {
                    return Err(match e {
                        ccitt::FaxError::Uncompressed => Error::Unsupported("fax uncompressed mode".into()),
                        e => Error::Invalid(format!("fax data: {e:?}")),
                    });
                }
                buf = rows;
                // The fax decoder gives 1 for black; BlackIsZero stores 0.
                if info.photometric == Photometric::BlackIsZero {
                    for b in &mut buf {
                        *b = !*b;
                    }
                }
            }
            _ => unreachable!("checked above"),
        }

        if let Some((sh, sv)) = subsampling {
            if info.predictor != 1 {
                return Err(Error::Unsupported("a predictor with subsampled YCbCr".into()));
            }
            return Ok(expand_ycbcr(&buf, c.width, c.rows, sh, sv));
        }

        match info.predictor {
            1 => {}
            2 => self.horizontal(&mut buf, c.width, c.rows, sp, bps)?,
            3 => {
                if info.sample_format != SampleFormat::Float {
                    return Err(Error::Invalid("the floating-point predictor on integer samples".into()));
                }
                self.floating(&mut buf, c.width, c.rows, sp, bps)?;
            }
            p => return Err(Error::Unsupported(format!("Predictor {p}"))),
        }
        Ok(buf)
    }

    /// Undo horizontal differencing (Predictor 2): each sample was stored
    /// as its difference from the same sample of the pixel to its left.
    fn horizontal(&self, buf: &mut [u8], width: usize, rows: usize, sp: usize, bps: usize) -> Result<()> {
        let bytes = bps / 8;
        if !matches!(bps, 8 | 16 | 32 | 64) {
            return Err(Error::Unsupported(format!("Predictor 2 with {bps}-bit samples")));
        }
        let row_len = width * sp * bytes;
        let be = self.f.big_endian;
        for r in 0..rows {
            let row = &mut buf[r * row_len..(r + 1) * row_len];
            match bytes {
                1 => {
                    for i in sp..row.len() {
                        row[i] = row[i].wrapping_add(row[i - sp]);
                    }
                }
                _ => {
                    let get = |row: &[u8], i: usize| -> u64 {
                        let b = &row[i * bytes..(i + 1) * bytes];
                        let mut v = 0u64;
                        for k in 0..bytes {
                            let byte = if be { b[k] } else { b[bytes - 1 - k] };
                            v = (v << 8) | u64::from(byte);
                        }
                        v
                    };
                    let put = |row: &mut [u8], i: usize, v: u64| {
                        let b = &mut row[i * bytes..(i + 1) * bytes];
                        for k in 0..bytes {
                            let shift = 8 * (bytes - 1 - k);
                            let byte = (v >> shift) as u8;
                            if be {
                                b[k] = byte;
                            } else {
                                b[bytes - 1 - k] = byte;
                            }
                        }
                    };
                    let mask = if bytes == 8 { u64::MAX } else { (1u64 << (8 * bytes)) - 1 };
                    for i in sp..width * sp {
                        let v = get(row, i).wrapping_add(get(row, i - sp)) & mask;
                        put(row, i, v);
                    }
                }
            }
        }
        Ok(())
    }

    /// Undo the floating-point predictor (Predictor 3, Adobe Photoshop
    /// TIFF Technical Note 3): each row's bytes were split into planes,
    /// most significant byte first, and differenced bytewise; the result
    /// is written back in the file's byte order.
    fn floating(&self, buf: &mut [u8], width: usize, rows: usize, sp: usize, bps: usize) -> Result<()> {
        let bytes = bps / 8;
        if !matches!(bps, 16 | 32 | 64) {
            return Err(Error::Unsupported(format!("Predictor 3 with {bps}-bit samples")));
        }
        let n = width * sp;
        let row_len = n * bytes;
        let mut tmp = vec![0u8; row_len];
        for r in 0..rows {
            let row = &mut buf[r * row_len..(r + 1) * row_len];
            for i in sp..row_len {
                row[i] = row[i].wrapping_add(row[i - sp]);
            }
            for i in 0..n {
                for k in 0..bytes {
                    // Byte k of the big-endian value.
                    let byte = row[k * n + i];
                    let at = if self.f.big_endian { k } else { bytes - 1 - k };
                    tmp[i * bytes + at] = byte;
                }
            }
            row.copy_from_slice(&tmp);
        }
        Ok(())
    }

    /// Turn the image's packed samples into an [`Image`].
    fn interpret(&self, raw: &[Vec<u8>], row_bytes: usize, sp: usize, bps: usize) -> Result<Image> {
        let info = &self.info;
        let (w, h) = (info.width as usize, info.height as usize);
        let spp = usize::from(info.samples_per_pixel);
        let be = self.f.big_endian;
        // Sample `s` of pixel (x, y), as an unsigned integer of `bps` bits.
        let read = |x: usize, y: usize, s: usize| -> u64 {
            let (plane, idx) = if raw.len() > 1 { (s, x) } else { (0, x * sp + s) };
            let row = &raw[plane][y * row_bytes..(y + 1) * row_bytes];
            match bps {
                8 => u64::from(row[idx]),
                16 | 24 | 32 | 64 => {
                    let n = bps / 8;
                    let b = &row[idx * n..idx * n + n];
                    let mut v = 0u64;
                    for k in 0..n {
                        v = (v << 8) | u64::from(if be { b[k] } else { b[n - 1 - k] });
                    }
                    v
                }
                _ => {
                    // Packed most significant bit first, whatever the byte
                    // order.
                    let bit = idx * bps;
                    let mut v = 0u64;
                    for k in 0..bps {
                        let p = bit + k;
                        v = (v << 1) | u64::from((row[p / 8] >> (7 - p % 8)) & 1);
                    }
                    v
                }
            }
        };

        let alpha_extra = info.extra_samples.first().copied().filter(|&e| e == 1 || e == 2);
        let premultiplied = alpha_extra == Some(1);
        let (color, from): (ColorType, Vec<usize>) = match info.photometric {
            Photometric::WhiteIsZero | Photometric::BlackIsZero => {
                if alpha_extra.is_some() && spp >= 2 {
                    (ColorType::GrayAlpha, vec![0, 1])
                } else {
                    (ColorType::Gray, vec![0])
                }
            }
            Photometric::Rgb | Photometric::YCbCr => {
                if spp < 3 {
                    return Err(Error::Invalid(format!("{:?} with {spp} samples per pixel", info.photometric)));
                }
                if alpha_extra.is_some() && spp >= 4 {
                    (ColorType::Rgba, vec![0, 1, 2, 3])
                } else {
                    (ColorType::Rgb, vec![0, 1, 2])
                }
            }
            Photometric::Separated => {
                let inks = self.ifd.uint(self.data, self.f, tag::INK_SET).unwrap_or(1);
                if inks != 1 || spp < 4 {
                    return Err(Error::Unsupported(format!("separated data with InkSet {inks} and {spp} samples")));
                }
                if alpha_extra.is_some() && spp >= 5 {
                    (ColorType::Cmyka, vec![0, 1, 2, 3, 4])
                } else {
                    (ColorType::Cmyk, vec![0, 1, 2, 3])
                }
            }
            Photometric::Palette => return self.palette(read, bps),
            other => return Err(Error::Unsupported(format!("PhotometricInterpretation {other:?}"))),
        };

        let n = w * h * from.len();
        let invert = info.photometric == Photometric::WhiteIsZero;
        let signed = info.sample_format == SampleFormat::Int;
        let max = if bps >= 64 { u64::MAX } else { (1u64 << bps) - 1 };
        // An integer sample as unsigned (signed ones offset by half the
        // range), and white-is-zero turned around.
        let norm = |v: u64, s: usize| -> u64 {
            let v = if signed { v ^ (1u64 << (bps - 1)) } else { v };
            if invert && s == 0 { max - v } else { v }
        };
        let samples = match info.sample_format {
            SampleFormat::Float => {
                let mut out = Vec::with_capacity(n);
                for y in 0..h {
                    for x in 0..w {
                        for (k, &s) in from.iter().enumerate() {
                            let v = read(x, y, s);
                            let f = match bps {
                                16 => half_to_f32(v as u16),
                                32 => f32::from_bits(v as u32),
                                _ => f64::from_bits(v) as f32,
                            };
                            out.push(if invert && k == 0 { 1.0 - f } else { f });
                        }
                    }
                }
                Samples::F32(out)
            }
            _ if bps <= 8 => {
                let mut out = Vec::with_capacity(n);
                for y in 0..h {
                    for x in 0..w {
                        for &s in &from {
                            let v = norm(read(x, y, s), s);
                            out.push(if bps == 8 { v as u8 } else { ((v * 255 + max / 2) / max) as u8 });
                        }
                    }
                }
                Samples::U8(out)
            }
            _ => {
                let mut out = Vec::with_capacity(n);
                for y in 0..h {
                    for x in 0..w {
                        for &s in &from {
                            let v = norm(read(x, y, s), s);
                            out.push(if bps == 16 {
                                v as u16
                            } else {
                                ((u128::from(v) * 65535 + u128::from(max / 2)) / u128::from(max)) as u16
                            });
                        }
                    }
                }
                Samples::U16(out)
            }
        };
        let samples = if info.photometric == Photometric::YCbCr { self.ycbcr_to_rgb(samples, &from)? } else { samples };
        Ok(Image { width: info.width, height: info.height, color, samples, premultiplied, info: info.clone() })
    }

    fn palette(&self, read: impl Fn(usize, usize, usize) -> u64, bps: usize) -> Result<Image> {
        let info = &self.info;
        if bps > 16 || info.sample_format != SampleFormat::Uint {
            return Err(Error::Invalid(format!("a palette image with {bps}-bit samples")));
        }
        let map = self
            .ifd
            .uints(self.data, self.f, tag::COLOR_MAP)
            .ok_or_else(|| Error::Invalid("a palette image with no ColorMap".into()))?;
        let entries = 1usize << bps;
        if map.len() < 3 * entries {
            return Err(Error::Invalid(format!("a ColorMap of {} values for {bps}-bit indices", map.len())));
        }
        let (w, h) = (info.width as usize, info.height as usize);
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let i = read(x, y, 0) as usize;
                for c in 0..3 {
                    out.push(map[c * entries + i] as u16);
                }
            }
        }
        Ok(Image {
            width: info.width,
            height: info.height,
            color: ColorType::Rgb,
            samples: Samples::U16(out),
            premultiplied: false,
            info: info.clone(),
        })
    }

    /// YCbCr to RGB (TIFF 6.0 §21): ReferenceBlackWhite scales the codes,
    /// YCbCrCoefficients give the luma weights.
    fn ycbcr_to_rgb(&self, samples: Samples, from: &[usize]) -> Result<Samples> {
        let Samples::U8(mut v) = samples else {
            return Err(Error::Unsupported("YCbCr other than 8-bit".into()));
        };
        let coef = self.ifd.floats(self.data, self.f, tag::YCBCR_COEFFICIENTS).unwrap_or_default();
        let (lr, lg, lb) = match coef[..] {
            [r, g, b, ..] if g != 0.0 => (r, g, b),
            _ => (0.299, 0.587, 0.114),
        };
        let rbw = self.ifd.floats(self.data, self.f, tag::REFERENCE_BLACK_WHITE).unwrap_or_default();
        let rbw = if rbw.len() >= 6 { rbw } else { vec![0.0, 255.0, 128.0, 255.0, 128.0, 255.0] };
        let scale =
            |v: u8, black: f64, white: f64, range: f64| (f64::from(v) - black) * range / (white - black).max(1e-9);
        for px in v.chunks_mut(from.len()) {
            let y = scale(px[0], rbw[0], rbw[1], 255.0);
            let cb = scale(px[1], rbw[2], rbw[3], 127.0);
            let cr = scale(px[2], rbw[4], rbw[5], 127.0);
            let r = cr * (2.0 - 2.0 * lr) + y;
            let b = cb * (2.0 - 2.0 * lb) + y;
            let g = (y - lb * b - lr * r) / lg;
            px[0] = r.round().clamp(0.0, 255.0) as u8;
            px[1] = g.round().clamp(0.0, 255.0) as u8;
            px[2] = b.round().clamp(0.0, 255.0) as u8;
        }
        Ok(Samples::U8(v))
    }
}

/// Expand subsampled YCbCr data units (Y block, then Cb, then Cr) to
/// full-resolution chunky Y, Cb, Cr, repeating each chroma sample over its
/// block.
fn expand_ycbcr(units: &[u8], width: usize, rows: usize, sh: usize, sv: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * rows * 3];
    let ux = width.div_ceil(sh);
    let unit = sh * sv + 2;
    for (u, block) in units.chunks(unit).enumerate() {
        if block.len() < unit {
            break;
        }
        let (bx, by) = (u % ux * sh, u / ux * sv);
        let (cb, cr) = (block[sh * sv], block[sh * sv + 1]);
        for j in 0..sv {
            for i in 0..sh {
                let (x, y) = (bx + i, by + j);
                if x < width && y < rows {
                    let at = (y * width + x) * 3;
                    out[at..at + 3].copy_from_slice(&[block[j * sh + i], cb, cr]);
                }
            }
        }
    }
    out
}

/// IEEE 754 binary16 to f32.
fn half_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1F);
    let frac = u32::from(h & 0x3FF);
    let bits = match (exp, frac) {
        (0, 0) => sign,
        (0, f) => {
            // Subnormal: renormalise.
            let shift = f.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((f << shift) & 0x3FF) << 13)
        }
        (31, 0) => sign | 0x7F80_0000,
        (31, f) => sign | 0x7F80_0000 | (f << 13),
        (e, f) => sign | ((e + 112) << 23) | (f << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::half_to_f32;

    #[test]
    fn half_floats() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x7BFF), 65504.0);
        assert_eq!(half_to_f32(0x0001), 2.0f32.powi(-24));
        assert_eq!(half_to_f32(0x03FF), 2.0f32.powi(-14) * (1023.0 / 1024.0));
        assert!(half_to_f32(0x7C00).is_infinite());
        assert!(half_to_f32(0x7E00).is_nan());
    }
}
