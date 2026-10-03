//! Reducing colours to a palette: an exact palette when there are few
//! enough colours, otherwise median cut (Heckbert, "Color Image Quantization
//! for Frame Buffer Display", 1982) refined by a pass of k-means, and a
//! nearest-colour mapper.

use std::collections::HashMap;

/// Colour counts, exact (24-bit).
#[derive(Debug, Clone, Default)]
pub struct Histogram {
    counts: HashMap<u32, u32>,
}

fn pack([r, g, b]: [u8; 3]) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

fn unpack(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

impl Histogram {
    /// An empty histogram.
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one pixel.
    pub fn add(&mut self, rgb: [u8; 3]) {
        let n = self.counts.entry(pack(rgb)).or_insert(0);
        *n = n.saturating_add(1);
    }

    /// How many distinct colours have been counted.
    pub fn distinct(&self) -> usize {
        self.counts.len()
    }

    /// A palette of at most `max` colours (at least 1). When the histogram
    /// has `max` colours or fewer they are returned exactly, in ascending
    /// order; otherwise median cut chooses them.
    pub fn palette(&self, max: usize) -> Vec<[u8; 3]> {
        let max = max.max(1);
        let mut colours: Vec<(u32, u32)> = self.counts.iter().map(|(&c, &n)| (c, n)).collect();
        colours.sort_unstable();
        if colours.len() <= max {
            return colours.into_iter().map(|(c, _)| unpack(c)).collect();
        }
        let mut palette = median_cut(&mut colours, max);
        refine(&colours, &mut palette);
        palette
    }
}

/// A box of colours: a range of the shared colour list.
struct Box {
    start: usize,
    end: usize,
    /// Total squared error from the box's mean, summed over channels.
    error: f64,
    /// The channel with the widest variance.
    axis: usize,
}

fn channel(c: u32, axis: usize) -> u32 {
    (c >> (16 - 8 * axis)) & 0xFF
}

fn measure(colours: &[(u32, u32)], start: usize, end: usize) -> Box {
    let mut n = 0f64;
    let mut sum = [0f64; 3];
    let mut sq = [0f64; 3];
    for &(c, k) in &colours[start..end] {
        let k = f64::from(k);
        n += k;
        for (a, (s, q)) in sum.iter_mut().zip(sq.iter_mut()).enumerate() {
            let v = f64::from(channel(c, a));
            *s += k * v;
            *q += k * v * v;
        }
    }
    let var: [f64; 3] = std::array::from_fn(|a| sq[a] - sum[a] * sum[a] / n.max(1.0));
    let axis = (0..3).max_by(|&a, &b| var[a].total_cmp(&var[b])).unwrap_or(0);
    let error = if end - start > 1 { var.iter().sum() } else { 0.0 };
    Box { start, end, error, axis }
}

fn mean(colours: &[(u32, u32)]) -> [u8; 3] {
    let mut n = 0u64;
    let mut sum = [0u64; 3];
    for &(c, k) in colours {
        n += u64::from(k);
        for (a, s) in sum.iter_mut().enumerate() {
            *s += u64::from(k) * u64::from(channel(c, a));
        }
    }
    let n = n.max(1);
    sum.map(|s| ((s + n / 2) / n) as u8)
}

fn median_cut(colours: &mut [(u32, u32)], max: usize) -> Vec<[u8; 3]> {
    let mut boxes = vec![measure(colours, 0, colours.len())];
    while boxes.len() < max {
        let Some((i, _)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.end - b.start > 1)
            .max_by(|a, b| a.1.error.total_cmp(&b.1.error))
        else {
            break;
        };
        let b = boxes.swap_remove(i);
        let slice = &mut colours[b.start..b.end];
        slice.sort_unstable_by_key(|&(c, _)| channel(c, b.axis));
        let total: u64 = slice.iter().map(|&(_, k)| u64::from(k)).sum();
        let mut acc = 0u64;
        let mut cut = 1;
        for (j, &(_, k)) in slice.iter().enumerate() {
            acc += u64::from(k);
            if acc * 2 >= total {
                cut = j + 1;
                break;
            }
        }
        let cut = cut.clamp(1, slice.len() - 1);
        boxes.push(measure(colours, b.start, b.start + cut));
        boxes.push(measure(colours, b.start + cut, b.end));
    }
    boxes.iter().map(|b| mean(&colours[b.start..b.end])).collect()
}

/// One or two Lloyd iterations: move each palette entry to the mean of the
/// colours nearest it. Skipped when that would be too slow.
fn refine(colours: &[(u32, u32)], palette: &mut [[u8; 3]]) {
    let work = colours.len().saturating_mul(palette.len());
    let passes = if work <= 1 << 24 {
        2
    } else if work <= 1 << 27 {
        1
    } else {
        0
    };
    for _ in 0..passes {
        let mut sums = vec![[0u64; 4]; palette.len()];
        for &(c, k) in colours {
            let i = nearest(palette, unpack(c));
            let s = &mut sums[i];
            let rgb = unpack(c);
            for a in 0..3 {
                s[a] += u64::from(k) * u64::from(rgb[a]);
            }
            s[3] += u64::from(k);
        }
        for (p, s) in palette.iter_mut().zip(&sums) {
            if s[3] > 0 {
                *p = std::array::from_fn(|a| ((s[a] + s[3] / 2) / s[3]) as u8);
            }
        }
    }
}

/// The index of the palette colour nearest `rgb` (squared Euclidean
/// distance; the first of equals).
pub fn nearest(palette: &[[u8; 3]], rgb: [u8; 3]) -> usize {
    let mut best = 0;
    let mut best_d = u32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let d: u32 = (0..3).map(|a| (i32::from(p[a]) - i32::from(rgb[a])).unsigned_abs().pow(2)).sum();
        if d < best_d {
            best_d = d;
            best = i;
            if d == 0 {
                break;
            }
        }
    }
    best
}

/// [`nearest`] with a cache of colours already looked up.
#[derive(Debug, Clone)]
pub struct Mapper {
    palette: Vec<[u8; 3]>,
    cache: HashMap<u32, u8>,
}

impl Mapper {
    /// A mapper onto `palette` (at most 256 colours).
    pub fn new(palette: Vec<[u8; 3]>) -> Self {
        debug_assert!(palette.len() <= 256);
        Self { palette, cache: HashMap::new() }
    }

    /// The palette.
    pub fn palette(&self) -> &[[u8; 3]] {
        &self.palette
    }

    /// The index of the nearest palette colour.
    pub fn index(&mut self, rgb: [u8; 3]) -> u8 {
        if self.cache.len() >= 1 << 20 {
            self.cache.clear();
        }
        let palette = &self.palette;
        *self.cache.entry(pack(rgb)).or_insert_with(|| nearest(palette, rgb) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn few_colours_are_exact() {
        let mut h = Histogram::new();
        for c in [[1, 2, 3], [4, 5, 6], [1, 2, 3]] {
            h.add(c);
        }
        assert_eq!(h.palette(256), vec![[1, 2, 3], [4, 5, 6]]);
    }

    #[test]
    fn many_colours_reduce_to_the_limit_and_stay_close() {
        let mut h = Histogram::new();
        let mut px = Vec::new();
        for r in 0..64u32 {
            for g in 0..64u32 {
                let c = [(r * 4) as u8, (g * 4) as u8, ((r + g) * 2) as u8];
                h.add(c);
                px.push(c);
            }
        }
        let pal = h.palette(64);
        assert_eq!(pal.len(), 64);
        let mut err = 0f64;
        for c in &px {
            let p = pal[nearest(&pal, *c)];
            err += (0..3).map(|a| (f64::from(p[a]) - f64::from(c[a])).powi(2)).sum::<f64>();
        }
        let rms = (err / px.len() as f64 / 3.0).sqrt();
        assert!(rms < 16.0, "rms {rms}");
    }
}
