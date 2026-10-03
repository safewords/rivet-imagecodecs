//! Bilevel fax coding: ITU-T T.4 (Group 3: Modified Huffman one-dimensional
//! coding and Modified READ two-dimensional coding) and T.6 (Group 4), as
//! TIFF 6.0 §§10–11 and the T4Options / T6Options fields use them.
//!
//! Output rows are packed one bit per pixel, most significant bit first,
//! with 1 for black.

use std::sync::OnceLock;

/// Why fax data could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FaxError {
    /// A bit pattern that is not a code here.
    BadCode,
    /// Runs that overshoot the row.
    BadRun,
    /// Uncompressed mode (a T.4/T.6 extension this decoder does not do).
    Uncompressed,
}

/// The coding scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheme {
    /// Compression 2: one-dimensional Modified Huffman, no EOLs, every row
    /// starting on a byte boundary.
    Mh,
    /// Compression 3 (T.4): EOL before each row; `two_d` from T4Options
    /// bit 0 (a tag bit after each EOL then picks 1D or 2D).
    G3 { two_d: bool },
    /// Compression 4 (T.6): two-dimensional only, no EOLs.
    G4,
}

// Run-length codes of T.4 §4.1 (Tables 2 and 3), as bit strings.
const WHITE_TERM: [&str; 64] = [
    "00110101", "000111", "0111", "1000", "1011", "1100", "1110", "1111", "10011", "10100", "00111", "01000", "001000",
    "000011", "110100", "110101", "101010", "101011", "0100111", "0001100", "0001000", "0010111", "0000011", "0000100",
    "0101000", "0101011", "0010011", "0100100", "0011000", "00000010", "00000011", "00011010", "00011011", "00010010",
    "00010011", "00010100", "00010101", "00010110", "00010111", "00101000", "00101001", "00101010", "00101011",
    "00101100", "00101101", "00000100", "00000101", "00001010", "00001011", "01010010", "01010011", "01010100",
    "01010101", "00100100", "00100101", "01011000", "01011001", "01011010", "01011011", "01001010", "01001011",
    "00110010", "00110011", "00110100",
];
const BLACK_TERM: [&str; 64] = [
    "0000110111",
    "010",
    "11",
    "10",
    "011",
    "0011",
    "0010",
    "00011",
    "000101",
    "000100",
    "0000100",
    "0000101",
    "0000111",
    "00000100",
    "00000111",
    "000011000",
    "0000010111",
    "0000011000",
    "0000001000",
    "00001100111",
    "00001101000",
    "00001101100",
    "00000110111",
    "00000101000",
    "00000010111",
    "00000011000",
    "000011001010",
    "000011001011",
    "000011001100",
    "000011001101",
    "000001101000",
    "000001101001",
    "000001101010",
    "000001101011",
    "000011010010",
    "000011010011",
    "000011010100",
    "000011010101",
    "000011010110",
    "000011010111",
    "000001101100",
    "000001101101",
    "000011011010",
    "000011011011",
    "000001010100",
    "000001010101",
    "000001010110",
    "000001010111",
    "000001100100",
    "000001100101",
    "000001010010",
    "000001010011",
    "000000100100",
    "000000110111",
    "000000111000",
    "000000100111",
    "000000101000",
    "000001011000",
    "000001011001",
    "000000101011",
    "000000101100",
    "000001011010",
    "000001100110",
    "000001100111",
];
/// Make-up codes for 64, 128, … 1728.
const WHITE_MAKEUP: [&str; 27] = [
    "11011",
    "10010",
    "010111",
    "0110111",
    "00110110",
    "00110111",
    "01100100",
    "01100101",
    "01101000",
    "01100111",
    "011001100",
    "011001101",
    "011010010",
    "011010011",
    "011010100",
    "011010101",
    "011010110",
    "011010111",
    "011011000",
    "011011001",
    "011011010",
    "011011011",
    "010011000",
    "010011001",
    "010011010",
    "011000",
    "010011011",
];
const BLACK_MAKEUP: [&str; 27] = [
    "0000001111",
    "000011001000",
    "000011001001",
    "000001011011",
    "000000110011",
    "000000110100",
    "000000110101",
    "0000001101100",
    "0000001101101",
    "0000001001010",
    "0000001001011",
    "0000001001100",
    "0000001001101",
    "0000001110010",
    "0000001110011",
    "0000001110100",
    "0000001110101",
    "0000001110110",
    "0000001110111",
    "0000001010010",
    "0000001010011",
    "0000001010100",
    "0000001010101",
    "0000001011010",
    "0000001011011",
    "0000001100100",
    "0000001100101",
];
/// Extended make-up codes for 1792, 1856, … 2560, shared by both colours.
const EXT_MAKEUP: [&str; 13] = [
    "00000001000",
    "00000001100",
    "00000001101",
    "000000010010",
    "000000010011",
    "000000010100",
    "000000010101",
    "000000010110",
    "000000010111",
    "000000011100",
    "000000011101",
    "000000011110",
    "000000011111",
];

/// The longest run code is 13 bits.
const RUN_BITS: u32 = 13;

/// A run-length decoding table indexed by the next 13 bits: (run, code
/// length); length 0 for no code.
struct RunTable {
    entries: Vec<(u16, u8)>,
}

impl RunTable {
    fn build(codes: &[(&str, u16)]) -> Self {
        let mut entries = vec![(0u16, 0u8); 1 << RUN_BITS];
        for &(code, run) in codes {
            let len = code.len() as u32;
            let v = u32::from_str_radix(code, 2).unwrap_or(0);
            let base = (v << (RUN_BITS - len)) as usize;
            for e in &mut entries[base..base + (1 << (RUN_BITS - len))] {
                *e = (run, len as u8);
            }
        }
        Self { entries }
    }
}

fn tables() -> &'static (RunTable, RunTable) {
    static T: OnceLock<(RunTable, RunTable)> = OnceLock::new();
    T.get_or_init(|| {
        let colour = |term: &[&'static str; 64], makeup: &[&'static str; 27]| {
            let mut codes: Vec<(&str, u16)> = Vec::new();
            for (i, c) in term.iter().enumerate() {
                codes.push((c, i as u16));
            }
            for (i, c) in makeup.iter().enumerate() {
                codes.push((c, 64 * (i as u16 + 1)));
            }
            for (i, c) in EXT_MAKEUP.iter().enumerate() {
                codes.push((c, 1792 + 64 * i as u16));
            }
            RunTable::build(&codes)
        };
        (colour(&WHITE_TERM, &WHITE_MAKEUP), colour(&BLACK_TERM, &BLACK_MAKEUP))
    })
}

/// Two-dimensional mode codes (T.4 §4.2.1.3.2, Table 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pass,
    Horizontal,
    Vertical(i32),
    Extension,
}

fn mode(bits: &mut Bits) -> Option<Mode> {
    // Longest mode code: 7 bits (VR3/VL3); extensions start 0000001.
    let p = bits.peek(7);
    let (m, len) = if p & 0x40 != 0 {
        (Mode::Vertical(0), 1)
    } else if p >> 4 == 0b011 {
        (Mode::Vertical(1), 3)
    } else if p >> 4 == 0b010 {
        (Mode::Vertical(-1), 3)
    } else if p >> 4 == 0b001 {
        (Mode::Horizontal, 3)
    } else if p >> 3 == 0b0001 {
        (Mode::Pass, 4)
    } else if p >> 1 == 0b000011 {
        (Mode::Vertical(2), 6)
    } else if p >> 1 == 0b000010 {
        (Mode::Vertical(-2), 6)
    } else if p == 0b0000011 {
        (Mode::Vertical(3), 7)
    } else if p == 0b0000010 {
        (Mode::Vertical(-3), 7)
    } else if p == 0b0000001 {
        (Mode::Extension, 7)
    } else {
        return None;
    };
    bits.skip(len);
    Some(m)
}

/// An MSB-first bit reader; reads past the end as zeros and remembers that
/// it did.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    over: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, acc: 0, n: 0, over: 0 }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let b = match self.data.get(self.pos) {
                Some(&b) => b,
                None => {
                    self.over += 8;
                    0
                }
            };
            self.pos += 1;
            self.acc |= u64::from(b) << (56 - self.n);
            self.n += 8;
        }
    }

    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.acc >> (64 - k)) as u32
    }

    fn skip(&mut self, k: u32) {
        if self.n < k {
            self.fill();
        }
        self.acc <<= k;
        self.n -= k;
    }

    fn bit(&mut self) -> u32 {
        let b = self.peek(1);
        self.skip(1);
        b
    }

    /// Whether the reader has gone past the data.
    fn exhausted(&self) -> bool {
        // Bits consumed beyond the end: bytes fetched past the end minus
        // what is still buffered.
        self.over > self.n
    }

    /// Skip to the next byte boundary.
    fn align(&mut self) {
        let r = self.n % 8;
        self.skip(r);
    }

    /// Consume an EOL (eleven zeros and a one, after any number of zero
    /// fill bits) if one is next; otherwise consume nothing.
    fn eol(&mut self) -> bool {
        // Twelve zeros cannot begin any code, so they are fill.
        while self.peek(12) == 0 {
            if self.exhausted() {
                return false;
            }
            self.skip(1);
        }
        if self.peek(12) == 1 {
            self.skip(12);
            true
        } else {
            false
        }
    }

    fn run(&mut self, black: bool) -> Result<usize, FaxError> {
        let (white_t, black_t) = tables();
        let t = if black { black_t } else { white_t };
        let mut total = 0usize;
        loop {
            let (run, len) = t.entries[self.peek(RUN_BITS) as usize];
            if len == 0 {
                return Err(FaxError::BadCode);
            }
            self.skip(u32::from(len));
            total += usize::from(run);
            if run < 64 {
                return Ok(total);
            }
            if total > 1 << 20 {
                return Err(FaxError::BadRun);
            }
        }
    }
}

/// Fill pixels `from..to` of a packed row with black.
fn paint(row: &mut [u8], from: usize, to: usize) {
    for x in from..to {
        row[x / 8] |= 0x80 >> (x % 8);
    }
}

/// Decode a one-dimensional row into changing elements; returns them.
fn row_1d(bits: &mut Bits, width: usize, row: &mut [u8], changes: &mut Vec<usize>) -> Result<(), FaxError> {
    changes.clear();
    let mut x = 0usize;
    let mut black = false;
    while x < width {
        let run = bits.run(black)?;
        let end = x + run;
        if end > width {
            return Err(FaxError::BadRun);
        }
        if black {
            paint(row, x, end);
        }
        x = end;
        if x < width {
            changes.push(x);
        }
        black = !black;
        if bits.exhausted() {
            return Err(FaxError::BadCode);
        }
    }
    Ok(())
}

/// Decode a two-dimensional row against `reference` (its changing elements,
/// starting with a change to black).
fn row_2d(
    bits: &mut Bits,
    width: usize,
    row: &mut [u8],
    reference: &[usize],
    changes: &mut Vec<usize>,
) -> Result<(), FaxError> {
    changes.clear();
    let mut a0: isize = -1;
    let mut black = false;
    // Where the search for b1 starts; a0 only moves right, so it rarely
    // moves back.
    let mut k = 0usize;
    let rc = |i: usize| reference.get(i).copied().unwrap_or(width);
    while a0 < width as isize {
        // b1: the first changing element on the reference line right of a0
        // whose colour is opposite a0's. Changes alternate, even indices
        // turning to black, odd ones to white.
        while k > 0 && rc(k - 1) as isize > a0 {
            k -= 1;
        }
        while k < reference.len() && rc(k) as isize <= a0 {
            k += 1;
        }
        if (k % 2 == 1) != black {
            k += 1;
        }
        let b1 = rc(k);
        let b2 = rc(k + 1);
        let start = a0.max(0) as usize;
        match mode(bits).ok_or(FaxError::BadCode)? {
            Mode::Pass => {
                if b2 < start || b2 > width {
                    return Err(FaxError::BadRun);
                }
                if black {
                    paint(row, start, b2);
                }
                a0 = b2 as isize;
            }
            Mode::Horizontal => {
                let r1 = bits.run(black)?;
                let r2 = bits.run(!black)?;
                let a1 = start + r1;
                let a2 = a1 + r2;
                if a2 > width {
                    return Err(FaxError::BadRun);
                }
                if black {
                    paint(row, start, a1);
                } else {
                    paint(row, a1, a2);
                }
                if a1 < width {
                    changes.push(a1);
                }
                if a2 < width {
                    changes.push(a2);
                }
                a0 = a2 as isize;
            }
            Mode::Vertical(d) => {
                let a1 = b1 as isize + d as isize;
                if a1 < start as isize || a1 > width as isize {
                    return Err(FaxError::BadRun);
                }
                let a1 = a1 as usize;
                if black {
                    paint(row, start, a1);
                }
                if a1 < width {
                    changes.push(a1);
                }
                a0 = a1 as isize;
                black = !black;
            }
            Mode::Extension => return Err(FaxError::Uncompressed),
        }
        if bits.exhausted() {
            return Err(FaxError::BadCode);
        }
    }
    Ok(())
}

/// Decode `rows` rows of `width` pixels. Returns the packed rows (1 =
/// black) and how many rows decoded cleanly; on damage the rest are white.
pub(crate) fn decode(data: &[u8], scheme: Scheme, width: usize, rows: usize) -> (Vec<u8>, usize, Option<FaxError>) {
    let stride = width.div_ceil(8);
    let mut out = vec![0u8; stride * rows];
    let mut bits = Bits::new(data);
    let mut reference: Vec<usize> = Vec::new();
    let mut changes: Vec<usize> = Vec::new();
    for r in 0..rows {
        let row = &mut out[r * stride..(r + 1) * stride];
        let result = match scheme {
            Scheme::Mh => {
                if r > 0 {
                    bits.align();
                }
                row_1d(&mut bits, width, row, &mut changes)
            }
            Scheme::G3 { two_d } => {
                bits.eol();
                let one_d = !two_d || bits.bit() == 1;
                if one_d {
                    row_1d(&mut bits, width, row, &mut changes)
                } else {
                    row_2d(&mut bits, width, row, &reference, &mut changes)
                }
            }
            Scheme::G4 => row_2d(&mut bits, width, row, &reference, &mut changes),
        };
        if let Err(e) = result {
            // Keep what this row managed; the rest stays white.
            return (out, r, Some(e));
        }
        std::mem::swap(&mut reference, &mut changes);
    }
    (out, rows, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kraft(codes: impl Iterator<Item = &'static str>) -> f64 {
        codes.map(|c| 0.5f64.powi(c.len() as i32)).sum()
    }

    fn prefix_free(codes: &[&str]) -> bool {
        for (i, a) in codes.iter().enumerate() {
            for (j, b) in codes.iter().enumerate() {
                if i != j && b.starts_with(a) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn the_run_codes_are_prefix_free_and_nearly_complete() {
        for (term, makeup) in [(&WHITE_TERM, &WHITE_MAKEUP), (&BLACK_TERM, &BLACK_MAKEUP)] {
            let all: Vec<&str> = term.iter().chain(makeup.iter()).chain(EXT_MAKEUP.iter()).copied().collect();
            assert_eq!(all.len(), 104);
            assert!(prefix_free(&all));
            // Everything but the space the EOL (twelve bits, 0…01) and
            // other all-zero prefixes occupy.
            let k = kraft(all.iter().copied());
            assert!(k < 1.0 && k > 1.0 - 2.0f64.powi(-7), "kraft {k}");
        }
    }

    /// Encode runs with the tables, then decode them.
    fn encode_runs(runs: &[usize]) -> Vec<u8> {
        let mut s = String::new();
        for (i, &r) in runs.iter().enumerate() {
            let black = i % 2 == 1;
            let (term, makeup) = if black { (&BLACK_TERM, &BLACK_MAKEUP) } else { (&WHITE_TERM, &WHITE_MAKEUP) };
            let mut left = r;
            while left >= 2560 + 64 {
                s.push_str(EXT_MAKEUP[12]);
                left -= 2560;
            }
            if left >= 1792 {
                let m = (left - 1792) / 64;
                s.push_str(EXT_MAKEUP[m]);
                left -= 1792 + 64 * m;
            } else if left >= 64 {
                s.push_str(makeup[left / 64 - 1]);
                left %= 64;
            }
            s.push_str(term[left]);
        }
        while !s.len().is_multiple_of(8) {
            s.push('0');
        }
        s.as_bytes().chunks(8).map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap()).collect()
    }

    #[test]
    fn one_dimensional_rows_decode() {
        let runs = [0, 3, 2000, 100, 1, 1, 5000, 2];
        let width: usize = runs.iter().sum();
        let (out, rows, err) = decode(&encode_runs(&runs), Scheme::Mh, width, 1);
        assert_eq!((rows, err), (1, None));
        let mut x = 0;
        for (i, &r) in runs.iter().enumerate() {
            for p in x..x + r {
                assert_eq!(out[p / 8] >> (7 - p % 8) & 1, (i % 2) as u8, "pixel {p}");
            }
            x += r;
        }
    }
}
