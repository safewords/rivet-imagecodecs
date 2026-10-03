//! The file structure: header, Image File Directories and field values
//! (TIFF 6.0 §2), and the BigTIFF variant (64-bit offsets, version 43).

use std::collections::BTreeMap;

use crate::{Error, Result};

/// Tags this crate reads or writes (TIFF 6.0 and its technical notes).
pub(crate) mod tag {
    pub const NEW_SUBFILE_TYPE: u16 = 254;
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const FILL_ORDER: u16 = 266;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const ORIENTATION: u16 = 274;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const X_RESOLUTION: u16 = 282;
    pub const Y_RESOLUTION: u16 = 283;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const T4_OPTIONS: u16 = 292;
    pub const T6_OPTIONS: u16 = 293;
    pub const RESOLUTION_UNIT: u16 = 296;
    pub const PAGE_NUMBER: u16 = 297;
    pub const SOFTWARE: u16 = 305;
    pub const PREDICTOR: u16 = 317;
    pub const COLOR_MAP: u16 = 320;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const INK_SET: u16 = 332;
    pub const EXTRA_SAMPLES: u16 = 338;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const YCBCR_COEFFICIENTS: u16 = 529;
    pub const YCBCR_SUBSAMPLING: u16 = 530;
    pub const REFERENCE_BLACK_WHITE: u16 = 532;
    pub const ICC_PROFILE: u16 = 34675;
}

/// Field types (TIFF 6.0 §2 and BigTIFF).
pub(crate) mod ty {
    pub const BYTE: u16 = 1;
    pub const ASCII: u16 = 2;
    pub const SHORT: u16 = 3;
    pub const LONG: u16 = 4;
    pub const RATIONAL: u16 = 5;
    pub const SBYTE: u16 = 6;
    pub const UNDEFINED: u16 = 7;
    pub const SSHORT: u16 = 8;
    pub const SLONG: u16 = 9;
    pub const SRATIONAL: u16 = 10;
    pub const FLOAT: u16 = 11;
    pub const DOUBLE: u16 = 12;
    pub const IFD: u16 = 13;
    pub const LONG8: u16 = 16;
    pub const SLONG8: u16 = 17;
    pub const IFD8: u16 = 18;
}

fn type_size(t: u16) -> Option<usize> {
    Some(match t {
        ty::BYTE | ty::ASCII | ty::SBYTE | ty::UNDEFINED => 1,
        ty::SHORT | ty::SSHORT => 2,
        ty::LONG | ty::SLONG | ty::FLOAT | ty::IFD => 4,
        ty::RATIONAL | ty::SRATIONAL | ty::DOUBLE | ty::LONG8 | ty::SLONG8 | ty::IFD8 => 8,
        _ => return None,
    })
}

/// Byte order and offset width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Format {
    pub(crate) big_endian: bool,
    pub(crate) big_tiff: bool,
}

impl Format {
    pub(crate) fn u16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.big_endian { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) }
    }

    pub(crate) fn u32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.big_endian { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    }

    pub(crate) fn u64(&self, b: &[u8]) -> u64 {
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        if self.big_endian { u64::from_be_bytes(a) } else { u64::from_le_bytes(a) }
    }
}

/// One directory entry, with its value bytes located (inline or at an
/// offset) but not yet interpreted.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) ty: u16,
    pub(crate) count: u64,
    /// Byte range of the value in the file.
    pub(crate) at: usize,
}

/// A parsed Image File Directory.
#[derive(Debug, Clone)]
pub(crate) struct Ifd {
    pub(crate) entries: BTreeMap<u16, Entry>,
}

/// The header: format and the first directory's offset.
pub(crate) fn header(data: &[u8]) -> Result<(Format, u64)> {
    if data.len() < 8 {
        return Err(Error::Invalid("not a TIFF (too short)".into()));
    }
    let big_endian = match &data[..2] {
        b"II" => false,
        b"MM" => true,
        _ => return Err(Error::Invalid("not a TIFF (no byte-order mark)".into())),
    };
    let mut f = Format { big_endian, big_tiff: false };
    match f.u16(&data[2..4]) {
        42 => Ok((f, u64::from(f.u32(&data[4..8])))),
        43 => {
            if data.len() < 16 {
                return Err(Error::Truncated);
            }
            if f.u16(&data[4..6]) != 8 || f.u16(&data[6..8]) != 0 {
                return Err(Error::Invalid("BigTIFF with an offset size other than 8".into()));
            }
            f.big_tiff = true;
            Ok((f, f.u64(&data[8..16])))
        }
        v => Err(Error::Invalid(format!("not a TIFF (version {v})"))),
    }
}

/// Read the directory at `offset`; returns it and the next directory's
/// offset (0 for none).
pub(crate) fn read_ifd(data: &[u8], f: Format, offset: u64) -> Result<(Ifd, u64)> {
    let off = usize::try_from(offset).map_err(|_| Error::Truncated)?;
    let (count, entry_size, head) = if f.big_tiff {
        let b = data.get(off..off + 8).ok_or(Error::Truncated)?;
        (f.u64(b), 20usize, 8usize)
    } else {
        let b = data.get(off..off + 2).ok_or(Error::Truncated)?;
        (u64::from(f.u16(b)), 12usize, 2usize)
    };
    let count = usize::try_from(count).map_err(|_| Error::Truncated)?;
    let table_len = count.checked_mul(entry_size).ok_or(Error::Truncated)?;
    let table = data.get(off + head..off + head + table_len).ok_or(Error::Truncated)?;
    let mut entries = BTreeMap::new();
    for (i, e) in table.chunks(entry_size).enumerate() {
        let tag = f.u16(&e[0..2]);
        let typ = f.u16(&e[2..4]);
        let (n, inline_at, inline_len, value_off) = if f.big_tiff {
            (f.u64(&e[4..12]), 12, 8, f.u64(&e[12..20]))
        } else {
            (u64::from(f.u32(&e[4..8])), 8, 4, u64::from(f.u32(&e[8..12])))
        };
        // Unknown types are skipped, as the specification asks.
        let Some(size) = type_size(typ) else { continue };
        let Some(total) = n.checked_mul(size as u64) else { continue };
        let at = if total <= inline_len {
            off + head + i * entry_size + inline_at
        } else {
            match usize::try_from(value_off) {
                Ok(v) if v.checked_add(total as usize).is_some_and(|end| end <= data.len()) => v,
                // A value outside the file: the entry is unusable.
                _ => continue,
            }
        };
        entries.insert(tag, Entry { ty: typ, count: n, at });
    }
    let next_at = off + head + table_len;
    let next = if f.big_tiff {
        data.get(next_at..next_at + 8).map_or(0, |b| f.u64(b))
    } else {
        data.get(next_at..next_at + 4).map_or(0, |b| u64::from(f.u32(b)))
    };
    Ok((Ifd { entries }, next))
}

impl Ifd {
    /// The values of an integer field (BYTE, SHORT, LONG, LONG8, IFD
    /// types and their signed forms, read as unsigned).
    pub(crate) fn uints(&self, data: &[u8], f: Format, tag: u16) -> Option<Vec<u64>> {
        let e = self.entries.get(&tag)?;
        let size = type_size(e.ty)?;
        let n = usize::try_from(e.count).ok()?;
        let bytes = data.get(e.at..e.at + n.checked_mul(size)?)?;
        let v = match e.ty {
            ty::BYTE | ty::SBYTE | ty::UNDEFINED => bytes.iter().map(|&b| u64::from(b)).collect(),
            ty::SHORT | ty::SSHORT => bytes.chunks(2).map(|b| u64::from(f.u16(b))).collect(),
            ty::LONG | ty::SLONG | ty::IFD => bytes.chunks(4).map(|b| u64::from(f.u32(b))).collect(),
            ty::LONG8 | ty::SLONG8 | ty::IFD8 => bytes.chunks(8).map(|b| f.u64(b)).collect(),
            _ => return None,
        };
        Some(v)
    }

    pub(crate) fn uint(&self, data: &[u8], f: Format, tag: u16) -> Option<u64> {
        self.uints(data, f, tag)?.first().copied()
    }

    /// The values of a numeric field as `f64` (rationals divided out).
    pub(crate) fn floats(&self, data: &[u8], f: Format, tag: u16) -> Option<Vec<f64>> {
        let e = self.entries.get(&tag)?;
        let size = type_size(e.ty)?;
        let n = usize::try_from(e.count).ok()?;
        let bytes = data.get(e.at..e.at + n.checked_mul(size)?)?;
        Some(match e.ty {
            ty::RATIONAL => {
                bytes.chunks(8).map(|b| f64::from(f.u32(&b[..4])) / f64::from(f.u32(&b[4..]).max(1))).collect()
            }
            ty::SRATIONAL => bytes
                .chunks(8)
                .map(|b| f64::from(f.u32(&b[..4]) as i32) / f64::from((f.u32(&b[4..]) as i32).max(1)))
                .collect(),
            ty::FLOAT => bytes.chunks(4).map(|b| f64::from(f32::from_bits(f.u32(b)))).collect(),
            ty::DOUBLE => bytes.chunks(8).map(|b| f64::from_bits(f.u64(b))).collect(),
            ty::SSHORT => bytes.chunks(2).map(|b| f64::from(f.u16(b) as i16)).collect(),
            ty::SLONG => bytes.chunks(4).map(|b| f64::from(f.u32(b) as i32)).collect(),
            ty::SBYTE => bytes.iter().map(|&b| f64::from(b as i8)).collect(),
            _ => self.uints(data, f, tag)?.into_iter().map(|v| v as f64).collect(),
        })
    }

    /// The raw bytes of a field (for ICC profiles and ASCII).
    pub(crate) fn bytes<'a>(&self, data: &'a [u8], tag: u16) -> Option<&'a [u8]> {
        let e = self.entries.get(&tag)?;
        let size = type_size(e.ty)?;
        let n = usize::try_from(e.count).ok()?;
        data.get(e.at..e.at + n.checked_mul(size)?)
    }
}
