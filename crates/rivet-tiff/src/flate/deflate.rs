//! Deflate: an RFC 1951 compressor (hash-chain LZ77 with lazy matching,
//! dynamic Huffman blocks with length-limited codes from package-merge,
//! falling back to fixed or stored blocks when smaller), and the zlib
//! wrapper of RFC 1950.

use super::inflate::adler32;

const WINDOW: usize = 1 << 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// Symbols gathered before a block is closed.
const BLOCK_SYMBOLS: usize = 1 << 15;

struct BitOut {
    out: Vec<u8>,
    buf: u64,
    n: u32,
}

impl BitOut {
    fn put(&mut self, v: u32, k: u32) {
        debug_assert!(k <= 32);
        self.buf |= u64::from(v) << self.n;
        self.n += k;
        while self.n >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, sent most significant bit first.
    fn code(&mut self, code: u16, len: u8) {
        let rev = (u32::from(code).reverse_bits()) >> (32 - u32::from(len));
        self.put(rev, u32::from(len));
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

/// An LZ77 symbol: a literal, or a (length, distance) match.
#[derive(Clone, Copy)]
enum Sym {
    Lit(u8),
    Match(u16, u16),
}

fn len_code(len: usize) -> (usize, u32, u32) {
    // (symbol index 0..29, extra bits, extra value)
    const BASE: [u16; 29] =
        [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    if len == 258 {
        return (28, 0, 0);
    }
    let i = BASE.iter().rposition(|&b| usize::from(b) <= len).unwrap_or(0);
    (i, u32::from(EXTRA[i]), (len - usize::from(BASE[i])) as u32)
}

fn dist_code(d: usize) -> (usize, u32, u32) {
    const BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097,
        6145, 8193, 12289, 16385, 24577,
    ];
    const EXTRA: [u8; 30] =
        [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
    let i = BASE.iter().rposition(|&b| usize::from(b) <= d).unwrap_or(0);
    (i, u32::from(EXTRA[i]), (d - usize::from(BASE[i])) as u32)
}

/// Code lengths for `freqs`, none longer than `limit` (package-merge).
/// Symbols with frequency 0 get length 0; a lone used symbol gets 1.
pub(crate) fn code_lengths(freqs: &[u32], limit: u8) -> Vec<u8> {
    let mut lengths = vec![0u8; freqs.len()];
    let mut leaves: Vec<(u64, usize)> =
        freqs.iter().enumerate().filter(|(_, f)| **f > 0).map(|(i, &f)| (u64::from(f), i)).collect();
    match leaves.len() {
        0 => return lengths,
        1 => {
            lengths[leaves[0].1] = 1;
            return lengths;
        }
        _ => {}
    }
    leaves.sort();
    let n = leaves.len();
    debug_assert!(n <= 1 << limit);
    // Each item: weight and the leaves it contains (as indices into
    // `leaves`).
    let base: Vec<(u64, Vec<u16>)> = leaves.iter().enumerate().map(|(i, &(w, _))| (w, vec![i as u16])).collect();
    let mut level = base.clone();
    for _ in 1..limit {
        let mut packages = Vec::with_capacity(level.len() / 2);
        for pair in level.as_chunks::<2>().0.iter() {
            let mut items = pair[0].1.clone();
            items.extend_from_slice(&pair[1].1);
            packages.push((pair[0].0 + pair[1].0, items));
        }
        let mut merged = Vec::with_capacity(base.len() + packages.len());
        let (mut a, mut b) = (base.iter().peekable(), packages.into_iter().peekable());
        loop {
            match (a.peek(), b.peek()) {
                (Some(x), Some(y)) => {
                    if x.0 <= y.0 {
                        merged.push(a.next().cloned().unwrap_or_default());
                    } else {
                        merged.push(b.next().unwrap_or_default());
                    }
                }
                (Some(_), None) => merged.push(a.next().cloned().unwrap_or_default()),
                (None, Some(_)) => merged.push(b.next().unwrap_or_default()),
                (None, None) => break,
            }
        }
        level = merged;
    }
    for item in level.iter().take(2 * n - 2) {
        for &leaf in &item.1 {
            lengths[leaves[usize::from(leaf)].1] += 1;
        }
    }
    lengths
}

/// Canonical codes for `lengths` (RFC 1951 §3.2.2).
fn canonical(lengths: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in lengths {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[usize::from(l)];
                next[usize::from(l)] += 1;
                c
            }
        })
        .collect()
}

struct Compressor {
    head: Vec<u32>,
    prev: Vec<u32>,
    max_chain: usize,
    nice: usize,
    lazy: bool,
}

const NIL: u32 = u32::MAX;

impl Compressor {
    fn hash(data: &[u8], i: usize) -> usize {
        let v = u32::from(data[i]) << 16 | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    fn insert(&mut self, data: &[u8], i: usize) {
        if i + MIN_MATCH <= data.len() {
            let h = Self::hash(data, i);
            self.prev[i % WINDOW] = self.head[h];
            self.head[h] = i as u32;
        }
    }

    fn longest(&self, data: &[u8], i: usize, at_least: usize) -> (usize, usize) {
        if i + MIN_MATCH > data.len() {
            return (0, 0);
        }
        let max = (data.len() - i).min(MAX_MATCH);
        let mut best = (at_least.max(MIN_MATCH - 1), 0);
        let mut cand = self.head[Self::hash(data, i)];
        let mut chain = self.max_chain;
        while cand != NIL && chain > 0 {
            let c = cand as usize;
            if c >= i || i - c > WINDOW - 1 {
                break;
            }
            if data[c + best.0.min(max - 1)] == data[i + best.0.min(max - 1)] {
                let mut l = 0;
                while l < max && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l > best.0 {
                    best = (l, i - c);
                    if l >= self.nice || l == max {
                        break;
                    }
                }
            }
            let p = self.prev[c % WINDOW];
            if p != NIL && p as usize >= c {
                break;
            }
            cand = p;
            chain -= 1;
        }
        if best.1 == 0 { (0, 0) } else { best }
    }
}

/// Compress `data` as raw DEFLATE. `level` 0 stores; 1 ..= 9 trade speed
/// for size.
pub(crate) fn deflate(data: &[u8], level: u8) -> Vec<u8> {
    let mut w = BitOut { out: Vec::with_capacity(data.len() / 2 + 64), buf: 0, n: 0 };
    if level == 0 || data.is_empty() {
        write_stored(&mut w, data, true);
        return w.out;
    }
    let (max_chain, nice, lazy) = match level {
        1 => (4, 16, false),
        2 => (8, 32, false),
        3 => (16, 64, true),
        4..=5 => (32, 128, true),
        6 => (64, 192, true),
        7 => (128, 258, true),
        _ => (1024, 258, true),
    };
    let mut c = Compressor { head: vec![NIL; 1 << HASH_BITS], prev: vec![NIL; WINDOW], max_chain, nice, lazy };
    let mut syms: Vec<Sym> = Vec::with_capacity(BLOCK_SYMBOLS + 2);
    let mut block_start = 0;
    let mut i = 0;
    while i < data.len() {
        let (mut len, mut dist) = c.longest(data, i, 0);
        if c.lazy && len >= MIN_MATCH && len < c.nice && i + 1 < data.len() {
            c.insert(data, i);
            let (l2, d2) = c.longest(data, i + 1, len);
            if l2 > len {
                syms.push(Sym::Lit(data[i]));
                i += 1;
                len = l2;
                dist = d2;
            } else {
                // Undo nothing: position i is already in the chain.
                syms.push(Sym::Match(len as u16, dist as u16));
                for k in 1..len {
                    c.insert(data, i + k);
                }
                i += len;
                if syms.len() >= BLOCK_SYMBOLS {
                    write_block(&mut w, &syms, &data[block_start..i], false);
                    syms.clear();
                    block_start = i;
                }
                continue;
            }
        }
        if len >= MIN_MATCH {
            syms.push(Sym::Match(len as u16, dist as u16));
            for k in 0..len {
                c.insert(data, i + k);
            }
            i += len;
        } else {
            syms.push(Sym::Lit(data[i]));
            c.insert(data, i);
            i += 1;
        }
        if syms.len() >= BLOCK_SYMBOLS {
            write_block(&mut w, &syms, &data[block_start..i], false);
            syms.clear();
            block_start = i;
        }
    }
    write_block(&mut w, &syms, &data[block_start..], true);
    w.align();
    w.out
}

fn write_stored(w: &mut BitOut, data: &[u8], last: bool) {
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        w.put(u32::from(last), 1);
        w.put(0, 2);
        w.align();
        w.put(0, 16);
        w.put(0xFFFF, 16);
        return;
    }
    while let Some(chunk) = chunks.next() {
        let fin = last && chunks.peek().is_none();
        w.put(u32::from(fin), 1);
        w.put(0, 2);
        w.align();
        w.put(chunk.len() as u32, 16);
        w.put(!(chunk.len() as u32) & 0xFFFF, 16);
        w.out.extend_from_slice(chunk);
    }
}

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let mut l = vec![0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    (l, vec![5u8; 30])
}

/// The cost in bits of `syms` under the given code lengths.
fn cost(syms: &[Sym], lit: &[u8], dist: &[u8]) -> u64 {
    let mut bits = u64::from(lit[256]);
    for s in syms {
        bits += match *s {
            Sym::Lit(b) => u64::from(lit[usize::from(b)]),
            Sym::Match(l, d) => {
                let (li, le, _) = len_code(usize::from(l));
                let (di, de, _) = dist_code(usize::from(d));
                u64::from(lit[257 + li]) + u64::from(le) + u64::from(dist[di]) + u64::from(de)
            }
        };
    }
    bits
}

/// Run-length code a sequence of code lengths with symbols 16, 17 and 18:
/// (symbol, extra bits, extra value).
fn rle_lengths(lengths: &[u8]) -> Vec<(u8, u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let v = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == v {
            run += 1;
        }
        let mut left = run;
        if v == 0 {
            while left >= 11 {
                let k = left.min(138);
                out.push((18, 7, (k - 11) as u32));
                left -= k;
            }
            if left >= 3 {
                out.push((17, 3, (left - 3) as u32));
                left = 0;
            }
        } else {
            out.push((v, 0, 0));
            left -= 1;
            while left >= 3 {
                let k = left.min(6);
                out.push((16, 2, (k - 3) as u32));
                left -= k;
            }
        }
        for _ in 0..left {
            out.push((v, 0, 0));
        }
        i += run;
    }
    out
}

const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn write_block(w: &mut BitOut, syms: &[Sym], raw: &[u8], last: bool) {
    let mut lf = vec![0u32; 286];
    let mut df = vec![0u32; 30];
    for s in syms {
        match *s {
            Sym::Lit(b) => lf[usize::from(b)] += 1,
            Sym::Match(l, d) => {
                lf[257 + len_code(usize::from(l)).0] += 1;
                df[dist_code(usize::from(d)).0] += 1;
            }
        }
    }
    lf[256] = 1;
    if lf.iter().filter(|&&f| f > 0).count() == 1 {
        // A one-code literal tree is incomplete; give it a second symbol so
        // every inflater accepts it.
        lf[0] = 1;
    }
    let lit = code_lengths(&lf, 15);
    let mut dist = code_lengths(&df, 15);
    if dist.iter().all(|&l| l == 0) {
        // At least one distance code must be described.
        dist[0] = 1;
    }
    let hlit = lit.iter().rposition(|&l| l != 0).map_or(257, |p| (p + 1).max(257));
    let hdist = dist.iter().rposition(|&l| l != 0).map_or(1, |p| p + 1);
    let mut all = lit[..hlit].to_vec();
    all.extend_from_slice(&dist[..hdist]);
    let rle = rle_lengths(&all);
    let mut cf = vec![0u32; 19];
    for &(s, _, _) in &rle {
        cf[usize::from(s)] += 1;
    }
    let cl = code_lengths(&cf, 7);
    let hclen = CL_ORDER.iter().rposition(|&i| cl[i] != 0).map_or(4, |p| (p + 1).max(4));
    let header_bits: u64 =
        14 + 3 * hclen as u64 + rle.iter().map(|&(s, e, _)| u64::from(cl[usize::from(s)]) + u64::from(e)).sum::<u64>();
    let dynamic = header_bits + cost(syms, &lit, &dist);
    let (fl, fd) = fixed_lengths();
    let fixed = cost(syms, &fl, &fd);
    let stored = (raw.len() as u64 + 5 * (raw.len() as u64 / 65535 + 1)) * 8 + 7;

    if stored < dynamic.min(fixed) {
        write_stored(w, raw, last);
        return;
    }
    w.put(u32::from(last), 1);
    let (lit, dist) = if fixed <= dynamic {
        w.put(1, 2);
        (fl, fd)
    } else {
        w.put(2, 2);
        w.put((hlit - 257) as u32, 5);
        w.put((hdist - 1) as u32, 5);
        w.put((hclen - 4) as u32, 4);
        for &i in CL_ORDER.iter().take(hclen) {
            w.put(u32::from(cl[i]), 3);
        }
        let cc = canonical(&cl);
        for &(s, e, v) in &rle {
            w.code(cc[usize::from(s)], cl[usize::from(s)]);
            w.put(v, e);
        }
        (lit, dist)
    };
    let lc = canonical(&lit);
    let dc = canonical(&dist);
    for s in syms {
        match *s {
            Sym::Lit(b) => w.code(lc[usize::from(b)], lit[usize::from(b)]),
            Sym::Match(l, d) => {
                let (li, le, lv) = len_code(usize::from(l));
                w.code(lc[257 + li], lit[257 + li]);
                w.put(lv, le);
                let (di, de, dv) = dist_code(usize::from(d));
                w.code(dc[di], dist[di]);
                w.put(dv, de);
            }
        }
    }
    w.code(lc[256], lit[256]);
}

/// Compress as a zlib stream (RFC 1950): header, DEFLATE data, Adler-32.
pub(crate) fn zlib_compress(data: &[u8], level: u8) -> Vec<u8> {
    let flevel: u8 = match level {
        0..=1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let cmf = 0x78u8;
    let mut flg = flevel << 6;
    flg += 31 - ((u16::from(cmf) << 8 | u16::from(flg)) % 31) as u8;
    let mut out = vec![cmf, flg];
    out.extend_from_slice(&deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::super::inflate::{inflate, zlib_decompress};
    use super::*;

    fn sample(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            match x >> 30 {
                0 => v.extend_from_slice(b"the quick brown fox jumps over the lazy dog "),
                1 => v.push((x >> 8) as u8),
                2 => {
                    let k = ((x >> 10) % 300) as usize;
                    let b = (x >> 20) as u8;
                    v.extend(std::iter::repeat_n(b, k));
                }
                _ => {
                    if v.len() > 100 {
                        let s = ((x >> 4) as usize) % (v.len() - 50);
                        let k = ((x >> 16) % 200) as usize;
                        for j in 0..k {
                            let b = v[s + j % 50];
                            v.push(b);
                        }
                    }
                }
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn round_trips_at_every_level() {
        for (n, seed) in [(0, 1), (1, 2), (100, 3), (70_000, 4), (300_000, 5)] {
            let data = sample(n, seed);
            for level in 0..=9 {
                let z = zlib_compress(&data, level);
                let back = zlib_decompress(&z, usize::MAX, true).unwrap();
                assert_eq!(back, data, "n={n} level={level}");
            }
        }
    }

    #[test]
    fn incompressible_data_is_stored() {
        let mut x = 7u32;
        let data: Vec<u8> = (0..100_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let raw = deflate(&data, 6);
        assert!(raw.len() < data.len() + 64);
        let mut out = Vec::new();
        inflate(&raw, &mut out, usize::MAX).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn lengths_respect_the_limit_and_kraft() {
        // Fibonacci frequencies force deep trees.
        let mut f = vec![1u32, 1];
        while f.len() < 40 {
            let n = f[f.len() - 1] + f[f.len() - 2];
            f.push(n);
        }
        for limit in [7u8, 15] {
            let l = code_lengths(&f[..f.len().min(if limit == 7 { 19 } else { 40 })], limit);
            assert!(l.iter().all(|&x| x <= limit && x > 0));
            let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(i32::from(x))).sum();
            assert!(kraft <= 1.0 + 1e-12, "kraft {kraft}");
        }
    }

    #[test]
    fn rfc1951_fixed_block_example_decodes() {
        // "abc" in a fixed block, hand-assembled from RFC 1951 §3.2.6:
        // BFINAL=1, BTYPE=01, literals 'a' 'b' 'c' (8-bit codes 0x30+0x61…),
        // end of block (7 zero bits).
        let mut w = BitOut { out: Vec::new(), buf: 0, n: 0 };
        w.put(1, 1);
        w.put(1, 2);
        for b in b"abc" {
            w.code(0x30 + u16::from(*b), 8);
        }
        w.code(0, 7);
        w.align();
        let mut out = Vec::new();
        inflate(&w.out, &mut out, 100).unwrap();
        assert_eq!(out, b"abc");
    }
}
