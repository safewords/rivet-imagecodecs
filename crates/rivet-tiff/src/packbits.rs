//! PackBits (TIFF 6.0 §9): a header byte `n`; 0 ..= 127 copies the next
//! `n + 1` bytes, -127 ..= -1 repeats the next byte `1 - n` times, -128 is
//! a no-op.

/// Decode into `out`; returns the bytes written (clipped to `out`).
pub(crate) fn decode(data: &[u8], out: &mut [u8]) -> usize {
    let mut i = 0;
    let mut w = 0;
    while i < data.len() && w < out.len() {
        let n = data[i] as i8;
        i += 1;
        if n >= 0 {
            let k = usize::from(n as u8) + 1;
            let src = &data[i..(i + k).min(data.len())];
            let take = src.len().min(out.len() - w);
            out[w..w + take].copy_from_slice(&src[..take]);
            w += take;
            i += k;
        } else if n != -128 {
            let Some(&b) = data.get(i) else { break };
            i += 1;
            let k = (1 - isize::from(n)) as usize;
            let take = k.min(out.len() - w);
            out[w..w + take].fill(b);
            w += take;
        }
    }
    w
}

/// Encode one row (PackBits runs never cross rows in TIFF).
pub(crate) fn encode_row(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        // A run of at least 3 equal bytes is worth a repeat code.
        let mut run = 1;
        while i + run < row.len() && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((1 - run as isize) as u8);
            out.push(row[i]);
            i += run;
            continue;
        }
        // Literals up to the next run of 3 (or 128 bytes).
        let start = i;
        while i < row.len() && i - start < 128 {
            if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                break;
            }
            i += 1;
        }
        out.push((i - start - 1) as u8);
        out.extend_from_slice(&row[start..i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specification_example() {
        // TIFF 6.0 §9's example.
        let packed = [0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7, 0xAA];
        let unpacked = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22, 0xAA, 0xAA, 0xAA, 0xAA,
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        let mut out = [0u8; 24];
        assert_eq!(decode(&packed, &mut out), 24);
        assert_eq!(out, unpacked);
    }

    #[test]
    fn round_trips() {
        let mut x = 5u32;
        for len in [0, 1, 2, 3, 127, 128, 129, 300, 1000] {
            let row: Vec<u8> = (0..len)
                .map(|i| {
                    x = x.wrapping_mul(69069).wrapping_add(1);
                    if (i / 40) % 2 == 0 { 7 } else { (x >> 24) as u8 }
                })
                .collect();
            let mut enc = Vec::new();
            encode_row(&row, &mut enc);
            let mut out = vec![0u8; len];
            assert_eq!(decode(&enc, &mut out), len);
            assert_eq!(out, row);
        }
    }
}
