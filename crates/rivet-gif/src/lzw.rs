//! GIF's variable-length-code LZW (GIF89a Appendix F): codes packed least
//! significant bit first, a Clear code at `2^min`, End of Information at
//! `2^min + 1`, the code width growing from `min + 1` bits when the next code
//! to be assigned no longer fits, to at most 12 bits.

/// The largest code the 12-bit table can hold, plus one.
const TABLE: usize = 4096;

/// Why a code stream could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LzwError {
    /// The minimum code size is outside 1 ..= 11.
    CodeSize(u8),
    /// A code that is neither in the table nor the one about to be added.
    InvalidCode,
    /// A literal above 255 (possible only with a minimum code size above 8),
    /// which no colour table can hold.
    IndexRange,
}

/// How a decode ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// How many indices were written to the output (never more than its
    /// length; anything past it is discarded).
    pub(crate) written: usize,
    /// Whether End of Information was seen (rather than the data running out).
    pub(crate) ended: bool,
}

/// Decode `data` (the image's sub-blocks, concatenated) into `out`.
///
/// Decoding stops at End of Information, at the end of the data, or when
/// `out` is full. A stream that runs out early is not an error: the caller
/// sees how much was written.
pub(crate) fn decode(min_size: u8, data: &[u8], out: &mut [u8]) -> Result<Decoded, LzwError> {
    if !(1..=11).contains(&min_size) {
        return Err(LzwError::CodeSize(min_size));
    }
    // The specification asks for at least 2; a stream that says 1 is read
    // as written (2-bit codes: two literals, Clear and EOI).
    let literal_bits = min_size;
    let clear = 1u16 << literal_bits;
    let eoi = clear + 1;
    let first_free = clear + 2;

    let mut prefix = [0u16; TABLE];
    let mut suffix = [0u8; TABLE];
    let mut first = [0u8; TABLE];
    let mut length = [0u16; TABLE];
    for c in 0..clear.min(256) {
        suffix[c as usize] = c as u8;
        first[c as usize] = c as u8;
        length[c as usize] = 1;
    }

    let mut width = u32::from(literal_bits) + 1;
    let mut next = first_free;
    let mut prev: Option<u16> = None;

    let mut bits: u32 = 0;
    let mut nbits: u32 = 0;
    let mut at = 0usize;
    let mut written = 0usize;

    loop {
        while nbits < width {
            let Some(&b) = data.get(at) else {
                return Ok(Decoded { written, ended: false });
            };
            at += 1;
            bits |= u32::from(b) << nbits;
            nbits += 8;
        }
        let code = (bits & ((1 << width) - 1)) as u16;
        bits >>= width;
        nbits -= width;

        if code == clear {
            width = u32::from(literal_bits) + 1;
            next = first_free;
            prev = None;
            continue;
        }
        if code == eoi {
            return Ok(Decoded { written, ended: true });
        }
        if code < clear && code > 255 {
            return Err(LzwError::IndexRange);
        }
        let Some(p) = prev else {
            if code >= clear {
                return Err(LzwError::InvalidCode);
            }
            if written < out.len() {
                out[written] = code as u8;
                written += 1;
            }
            prev = Some(code);
            continue;
        };

        let (string, tail) = if code < next {
            (code, None)
        } else if code == next && usize::from(next) < TABLE {
            // KwKwK: the string for `p` followed by its own first byte.
            (p, Some(first[p as usize]))
        } else {
            return Err(LzwError::InvalidCode);
        };

        // Write the string backwards from its last byte, clipped to `out`.
        let len = usize::from(length[string as usize]);
        let total = len + usize::from(tail.is_some());
        let mut c = string;
        for i in (0..len).rev() {
            let pos = written + i;
            if pos < out.len() {
                out[pos] = suffix[c as usize];
            }
            c = prefix[c as usize];
        }
        if let Some(t) = tail
            && written + len < out.len()
        {
            out[written + len] = t;
        }
        written = (written + total).min(out.len());

        if usize::from(next) < TABLE {
            let n = next as usize;
            prefix[n] = p;
            suffix[n] = tail.unwrap_or(first[code as usize]);
            first[n] = first[p as usize];
            length[n] = length[p as usize] + 1;
            next += 1;
            if u32::from(next) == 1 << width && width < 12 {
                width += 1;
            }
        }
        prev = Some(code);
        if written == out.len() && !out.is_empty() {
            // Everything the image needs is here; what follows (more pixels,
            // or EOI) is not needed.
            return Ok(Decoded { written, ended: false });
        }
    }
}

/// An open-addressed map from (prefix code, next byte) to code.
struct Dictionary {
    keys: Vec<u32>,
    codes: Vec<u16>,
}

const HASH_SIZE: usize = 1 << 13;
const EMPTY: u32 = u32::MAX;

impl Dictionary {
    fn new() -> Self {
        Self { keys: vec![EMPTY; HASH_SIZE], codes: vec![0; HASH_SIZE] }
    }

    fn clear(&mut self) {
        self.keys.fill(EMPTY);
    }

    fn slot(key: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> 19) as usize & (HASH_SIZE - 1)
    }

    fn get(&self, key: u32) -> Option<u16> {
        let mut i = Self::slot(key);
        loop {
            let k = self.keys[i];
            if k == key {
                return Some(self.codes[i]);
            }
            if k == EMPTY {
                return None;
            }
            i = (i + 1) & (HASH_SIZE - 1);
        }
    }

    fn insert(&mut self, key: u32, code: u16) {
        let mut i = Self::slot(key);
        while self.keys[i] != EMPTY {
            i = (i + 1) & (HASH_SIZE - 1);
        }
        self.keys[i] = key;
        self.codes[i] = code;
    }
}

struct BitWriter {
    out: Vec<u8>,
    bits: u32,
    nbits: u32,
}

impl BitWriter {
    fn put(&mut self, code: u16, width: u32) {
        self.bits |= u32::from(code) << self.nbits;
        self.nbits += width;
        while self.nbits >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.nbits -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push(self.bits as u8);
        }
        self.out
    }
}

/// Encode `indices` (each below `2^min_size`) as a GIF LZW code stream,
/// starting with Clear and ending with End of Information. `min_size` is
/// 2 ..= 8. The table is cleared whenever it fills.
pub(crate) fn encode(min_size: u8, indices: &[u8]) -> Vec<u8> {
    debug_assert!((2..=8).contains(&min_size));
    let clear = 1u16 << min_size;
    let eoi = clear + 1;
    let reset_width = u32::from(min_size) + 1;
    let mut w = BitWriter { out: Vec::with_capacity(indices.len() / 2 + 16), bits: 0, nbits: 0 };
    let mut dict = Dictionary::new();
    let mut width = reset_width;
    let mut next = eoi + 1;

    w.put(clear, width);
    let Some((&head, rest)) = indices.split_first() else {
        w.put(eoi, width);
        return w.finish();
    };
    let mut cur = u16::from(head);
    for &b in rest {
        let key = (u32::from(cur) << 8) | u32::from(b);
        if let Some(code) = dict.get(key) {
            cur = code;
            continue;
        }
        w.put(cur, width);
        if usize::from(next) < TABLE {
            dict.insert(key, next);
            next += 1;
            if u32::from(next) > (1 << width) && width < 12 {
                width += 1;
            }
        } else {
            w.put(clear, width);
            dict.clear();
            width = reset_width;
            next = eoi + 1;
        }
        cur = u16::from(b);
    }
    w.put(cur, width);
    w.put(eoi, width);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(min: u8, data: &[u8]) {
        let code = encode(min, data);
        let mut out = vec![0u8; data.len()];
        let d = decode(min, &code, &mut out).unwrap();
        assert_eq!(d.written, data.len());
        assert_eq!(out, data);
    }

    #[test]
    fn round_trips() {
        round_trip(2, &[]);
        round_trip(2, &[1]);
        round_trip(2, &[0, 1, 0, 1, 0, 1, 0, 1, 2, 3, 3, 3, 3, 3, 3]);
        let mut x = 12345u32;
        for min in 2..=8u8 {
            let data: Vec<u8> = (0..200_000)
                .map(|i| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    // Runs and noise, so the table fills and clears.
                    if i % 1000 < 500 { (i / 37 % (1 << min)) as u8 } else { ((x >> 16) % (1 << min)) as u8 }
                })
                .collect();
            round_trip(min, &data);
        }
    }

    #[test]
    fn the_spec_example_kwkwk() {
        // A run of one value exercises the code-not-yet-in-table case.
        round_trip(2, &[3; 1000]);
    }

    #[test]
    fn invalid_codes_are_refused() {
        // Clear (4), then code 7 when only 6 could be next: 3-bit codes
        // 100 111 packed LSB first.
        let bytes = [0b0011_1100u8, 0];
        let mut out = [0u8; 4];
        // First real code must be a literal; 7 is not.
        assert_eq!(decode(2, &bytes, &mut out), Err(LzwError::InvalidCode));
    }
}
