//! TIFF's LZW (TIFF 6.0 §13): 9 to 12-bit codes packed most significant bit
//! first, Clear = 256, EndOfInformation = 257, and the code width growing
//! one code *early* — the writer switches width as the table reaches 511,
//! 1023 and 2047 entries rather than 512, 1024 and 2048.
//!
//! Files from before TIFF 5.0's revision of the scheme ("old-style" LZW)
//! pack codes least significant bit first and switch width without the
//! early change, as GIF does; they are recognised by their first byte (a
//! Clear code written LSB first starts `0x00 0x01`, MSB first `0x80`).

const CLEAR: u16 = 256;
const EOI: u16 = 257;
const FIRST: u16 = 258;
const TABLE: usize = 4096;

/// Why a strip could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LzwError {
    InvalidCode,
}

/// Decode into `out` (whose length is the expected size). Returns how many
/// bytes were written; data that ends early is not an error here.
pub(crate) fn decode(data: &[u8], out: &mut [u8]) -> Result<usize, LzwError> {
    let old_style = data.len() >= 2 && data[0] == 0 && data[1] & 1 == 1;
    let mut prefix = vec![0u16; TABLE];
    let mut suffix = vec![0u8; TABLE];
    let mut first = vec![0u8; TABLE];
    let mut length = vec![0u16; TABLE];
    for c in 0..256 {
        suffix[c] = c as u8;
        first[c] = c as u8;
        length[c] = 1;
    }
    let mut width = 9u32;
    let mut next = FIRST;
    let mut prev: Option<u16> = None;
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    let mut at = 0;
    let mut written = 0;
    let early = u16::from(!old_style);

    loop {
        while nbits < width {
            let Some(&b) = data.get(at) else { return Ok(written) };
            at += 1;
            if old_style {
                bits |= u64::from(b) << nbits;
            } else {
                bits = (bits << 8) | u64::from(b);
            }
            nbits += 8;
        }
        let code = if old_style {
            let c = (bits & ((1 << width) - 1)) as u16;
            bits >>= width;
            c
        } else {
            ((bits >> (nbits - width)) & ((1 << width) - 1)) as u16
        };
        nbits -= width;
        if !old_style {
            bits &= (1u64 << nbits) - 1;
        }

        if code == CLEAR {
            width = 9;
            next = FIRST;
            prev = None;
            continue;
        }
        if code == EOI {
            return Ok(written);
        }
        let Some(p) = prev else {
            if code > 255 {
                return Err(LzwError::InvalidCode);
            }
            if written < out.len() {
                out[written] = code as u8;
                written += 1;
            }
            prev = Some(code);
            if written == out.len() {
                return Ok(written);
            }
            continue;
        };
        let (string, tail) = if code < next {
            (code, None)
        } else if code == next && usize::from(next) < TABLE {
            (p, Some(first[usize::from(p)]))
        } else {
            return Err(LzwError::InvalidCode);
        };
        let len = usize::from(length[usize::from(string)]);
        let mut c = string;
        for i in (0..len).rev() {
            if written + i < out.len() {
                out[written + i] = suffix[usize::from(c)];
            }
            c = prefix[usize::from(c)];
        }
        if let Some(t) = tail
            && written + len < out.len()
        {
            out[written + len] = t;
        }
        written = (written + len + usize::from(tail.is_some())).min(out.len());
        if usize::from(next) < TABLE {
            let n = usize::from(next);
            prefix[n] = p;
            suffix[n] = tail.unwrap_or(first[usize::from(code)]);
            first[n] = first[usize::from(p)];
            length[n] = length[usize::from(p)].saturating_add(1);
            next += 1;
        }
        if u32::from(next + early) >= 1 << width && width < 12 {
            width += 1;
        }
        prev = Some(code);
        if written == out.len() {
            return Ok(written);
        }
    }
}

/// Encode as (new-style) TIFF LZW: Clear first, End of Information last,
/// Clear again whenever the table reaches 4094 entries.
pub(crate) fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 16);
    let mut acc: u64 = 0;
    let mut n = 0u32;
    let mut put = |code: u16, width: u32, out: &mut Vec<u8>| {
        acc = (acc << width) | u64::from(code);
        n += width;
        while n >= 8 {
            out.push((acc >> (n - 8)) as u8);
            n -= 8;
        }
        acc &= (1u64 << n) - 1;
    };
    // Children of each code: an open-addressed (prefix, byte) -> code map.
    const SIZE: usize = 1 << 13;
    let mut keys = vec![u32::MAX; SIZE];
    let mut vals = vec![0u16; SIZE];
    let slot = |k: u32| (k.wrapping_mul(0x9E37_79B1) >> 19) as usize & (SIZE - 1);

    let mut width = 9u32;
    let mut next = FIRST;
    put(CLEAR, width, &mut out);
    let Some((&head, rest)) = data.split_first() else {
        put(EOI, width, &mut out);
        if n > 0 {
            out.push((acc << (8 - n)) as u8);
        }
        return out;
    };
    let mut cur = u16::from(head);
    for &b in rest {
        let key = (u32::from(cur) << 8) | u32::from(b);
        let mut i = slot(key);
        let mut found = None;
        while keys[i] != u32::MAX {
            if keys[i] == key {
                found = Some(vals[i]);
                break;
            }
            i = (i + 1) & (SIZE - 1);
        }
        if let Some(c) = found {
            cur = c;
            continue;
        }
        put(cur, width, &mut out);
        keys[i] = key;
        vals[i] = next;
        next += 1;
        // The decoder, one entry behind, widens after adding entry
        // 2^w - 2; this side has then just added 2^w - 1.
        if u32::from(next) >= 1 << width && width < 12 {
            width += 1;
        }
        if next >= 4094 {
            put(CLEAR, width, &mut out);
            keys.fill(u32::MAX);
            width = 9;
            next = FIRST;
        }
        cur = u16::from(b);
    }
    put(cur, width, &mut out);
    // The decoder adds one more entry on reading `cur`; if that widens the
    // code, EOI goes out at the new width.
    if next < 4094 && u32::from(next + 1) >= 1 << width && width < 12 {
        width += 1;
    }
    put(EOI, width, &mut out);
    if n > 0 {
        out.push((acc << (8 - n)) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(data: &[u8]) {
        let code = encode(data);
        let mut out = vec![0u8; data.len()];
        assert_eq!(decode(&code, &mut out), Ok(data.len()));
        assert_eq!(out, data);
    }

    #[test]
    fn round_trips() {
        round_trip(&[]);
        round_trip(&[7]);
        round_trip(&[1, 1, 1, 1, 1, 1, 1, 1, 1]);
        let mut x = 99u32;
        for len in [100, 600, 5000, 70_000, 300_000] {
            for alphabet in [2u32, 16, 256] {
                let data: Vec<u8> = (0..len)
                    .map(|i| {
                        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        if i % 512 < 200 { (i / 13 % alphabet) as u8 } else { ((x >> 16) % alphabet) as u8 }
                    })
                    .collect();
                round_trip(&data);
            }
        }
    }

    #[test]
    fn stops_when_the_output_is_full() {
        let code = encode(&[5; 1000]);
        let mut out = vec![0u8; 10];
        assert_eq!(decode(&code, &mut out), Ok(10));
        assert_eq!(out, [5; 10]);
    }
}
