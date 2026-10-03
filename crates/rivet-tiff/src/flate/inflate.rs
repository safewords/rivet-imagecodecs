//! Inflate: the DEFLATE decompressor of RFC 1951, and the zlib wrapper of
//! RFC 1950 around it.
//!
//! Self-contained (std only) so the workspace's test support can include it
//! for reading reference PNGs.

use std::fmt;

/// Why a DEFLATE stream could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// The stream ended before its final block did.
    Truncated,
    /// A malformed header, block type, code table or distance.
    Invalid(&'static str),
    /// The output would exceed the caller's limit.
    TooLarge,
    /// The zlib Adler-32 check failed.
    Checksum,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InflateError::Truncated => f.write_str("deflate stream is truncated"),
            InflateError::Invalid(m) => write!(f, "invalid deflate stream: {m}"),
            InflateError::TooLarge => f.write_str("deflate output exceeds the limit"),
            InflateError::Checksum => f.write_str("zlib Adler-32 mismatch"),
        }
    }
}

impl std::error::Error for InflateError {}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn refill(&mut self) {
        while self.n <= 56 {
            let Some(&b) = self.data.get(self.pos) else { break };
            self.buf |= u64::from(b) << self.n;
            self.pos += 1;
            self.n += 8;
        }
    }

    fn need(&mut self, k: u32) -> Result<(), InflateError> {
        if self.n < k {
            self.refill();
            if self.n < k {
                return Err(InflateError::Truncated);
            }
        }
        Ok(())
    }

    fn take(&mut self, k: u32) -> Result<u32, InflateError> {
        if k == 0 {
            return Ok(0);
        }
        self.need(k)?;
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.buf >>= k;
        self.n -= k;
        Ok(v)
    }

    fn align(&mut self) {
        let r = self.n % 8;
        self.buf >>= r;
        self.n -= r;
    }

    /// Bytes consumed so far, counting whole bytes still buffered as unread.
    fn consumed(&self) -> usize {
        self.pos - (self.n / 8) as usize
    }
}

/// A canonical Huffman decoding table: indexed by the next `max` bits (least
/// significant first), each entry is `symbol << 4 | length`.
struct Table {
    entries: Vec<u16>,
    max: u32,
}

impl Table {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let max = u32::from(lengths.iter().copied().max().unwrap_or(0));
        if max == 0 {
            // An empty code: any use is an error, caught by length 0.
            return Ok(Self { entries: vec![0; 2], max: 1 });
        }
        let mut count = [0u32; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        count[0] = 0;
        let mut code = 0u32;
        let mut next = [0u32; 16];
        let mut left = 1i64;
        for bits in 1..16 {
            left = (left << 1) - i64::from(count[bits]);
            if left < 0 {
                return Err(InflateError::Invalid("over-subscribed code"));
            }
            code = (code + count[bits - 1]) << 1;
            next[bits] = code;
        }
        let size = 1usize << max;
        let mut entries = vec![0u16; size];
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l32 = u32::from(l);
            let c = next[usize::from(l)];
            next[usize::from(l)] += 1;
            // Reverse the code: DEFLATE sends Huffman codes MSB first into an
            // LSB-first stream.
            let rev = (c.reverse_bits() >> (32 - l32)) as usize;
            let entry = ((sym as u16) << 4) | l as u16;
            let mut i = rev;
            while i < size {
                entries[i] = entry;
                i += 1 << l32;
            }
        }
        Ok(Self { entries, max })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, InflateError> {
        if bits.n < self.max {
            bits.refill();
        }
        let avail = bits.n.min(self.max);
        let e = self.entries[(bits.buf & ((1u64 << self.max) - 1)) as usize];
        let len = u32::from(e & 15);
        if len == 0 {
            return Err(if avail < self.max { InflateError::Truncated } else { InflateError::Invalid("unused code") });
        }
        if len > bits.n {
            return Err(InflateError::Truncated);
        }
        bits.buf >>= len;
        bits.n -= len;
        Ok(e >> 4)
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Inflate a raw DEFLATE stream, appending to `out` but never beyond
/// `limit` bytes in total. Returns the number of input bytes the stream
/// occupied. On error, `out` holds whatever was decoded before it.
pub fn inflate(data: &[u8], out: &mut Vec<u8>, limit: usize) -> Result<usize, InflateError> {
    let mut bits = Bits { data, pos: 0, buf: 0, n: 0 };
    loop {
        let last = bits.take(1)?;
        let kind = bits.take(2)?;
        match kind {
            0 => {
                bits.align();
                let len = bits.take(16)?;
                let nlen = bits.take(16)?;
                if len != !nlen & 0xFFFF {
                    return Err(InflateError::Invalid("stored block length check"));
                }
                // Whatever is still in the bit buffer is whole bytes now.
                let mut len = len as usize;
                while len > 0 && bits.n >= 8 {
                    push(out, bits.take(8)? as u8, limit)?;
                    len -= 1;
                }
                let start = bits.pos;
                let end = start.checked_add(len).ok_or(InflateError::Truncated)?;
                let src = data.get(start..end).ok_or(InflateError::Truncated)?;
                if out.len() + src.len() > limit {
                    return Err(InflateError::TooLarge);
                }
                out.extend_from_slice(src);
                bits.pos = end;
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Table::new(&l)?;
                let dist = Table::new(&[5u8; 30])?;
                block(&mut bits, out, &lit, &dist, limit)?;
            }
            2 => {
                let hlit = bits.take(5)? as usize + 257;
                let hdist = bits.take(5)? as usize + 1;
                let hclen = bits.take(4)? as usize + 4;
                let mut cl = [0u8; 19];
                for &i in CL_ORDER.iter().take(hclen) {
                    cl[i] = bits.take(3)? as u8;
                }
                let cl_table = Table::new(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lengths.len() {
                    let sym = cl_table.decode(&mut bits)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => {
                            let prev = *lengths[..i].last().ok_or(InflateError::Invalid("repeat with no length"))?;
                            (prev, 3 + bits.take(2)? as usize)
                        }
                        17 => (0, 3 + bits.take(3)? as usize),
                        18 => (0, 11 + bits.take(7)? as usize),
                        _ => return Err(InflateError::Invalid("code length symbol")),
                    };
                    if i + repeat > lengths.len() {
                        return Err(InflateError::Invalid("code lengths overrun"));
                    }
                    lengths[i..i + repeat].fill(value);
                    i += repeat;
                }
                if lengths[256] == 0 {
                    return Err(InflateError::Invalid("no end-of-block code"));
                }
                let lit = Table::new(&lengths[..hlit])?;
                let dist = Table::new(&lengths[hlit..])?;
                block(&mut bits, out, &lit, &dist, limit)?;
            }
            _ => return Err(InflateError::Invalid("block type 3")),
        }
        if last == 1 {
            bits.align();
            return Ok(bits.consumed());
        }
    }
}

fn push(out: &mut Vec<u8>, b: u8, limit: usize) -> Result<(), InflateError> {
    if out.len() >= limit {
        return Err(InflateError::TooLarge);
    }
    out.push(b);
    Ok(())
}

fn block(bits: &mut Bits, out: &mut Vec<u8>, lit: &Table, dist: &Table, limit: usize) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => push(out, sym as u8, limit)?,
            256 => return Ok(()),
            257..=285 => {
                let i = usize::from(sym - 257);
                let len = usize::from(LEN_BASE[i]) + bits.take(u32::from(LEN_EXTRA[i]))? as usize;
                let d = usize::from(dist.decode(bits)?);
                if d >= 30 {
                    return Err(InflateError::Invalid("distance code"));
                }
                let distance = usize::from(DIST_BASE[d]) + bits.take(u32::from(DIST_EXTRA[d]))? as usize;
                if distance > out.len() {
                    return Err(InflateError::Invalid("distance before the start"));
                }
                if out.len() + len > limit {
                    return Err(InflateError::TooLarge);
                }
                let start = out.len() - distance;
                if distance >= len {
                    out.extend_from_within(start..start + len);
                } else {
                    for k in 0..len {
                        let b = out[start + k];
                        out.push(b);
                    }
                }
            }
            _ => return Err(InflateError::Invalid("literal/length code")),
        }
    }
}

/// Adler-32 (RFC 1950 §8.2).
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// Decompress a zlib stream (RFC 1950) of at most `limit` bytes. When
/// `check` is false the Adler-32 trailer is neither required nor checked
/// (some writers omit or botch it).
pub fn zlib_decompress(data: &[u8], limit: usize, check: bool) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::new();
    zlib_decompress_into(data, &mut out, limit, check)?;
    Ok(out)
}

/// [`zlib_decompress`] into a caller's buffer, which keeps whatever was
/// decoded if an error ends the stream early.
pub fn zlib_decompress_into(data: &[u8], out: &mut Vec<u8>, limit: usize, check: bool) -> Result<(), InflateError> {
    if data.len() < 2 {
        return Err(InflateError::Truncated);
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(InflateError::Invalid("zlib header"));
    }
    if flg & 0x20 != 0 {
        return Err(InflateError::Invalid("zlib preset dictionary"));
    }
    let start = out.len();
    let used = inflate(&data[2..], out, limit)?;
    if check {
        let tail = data.get(2 + used..2 + used + 4).ok_or(InflateError::Truncated)?;
        if u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) != adler32(&out[start..]) {
            return Err(InflateError::Checksum);
        }
    }
    Ok(())
}
