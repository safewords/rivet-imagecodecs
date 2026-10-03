//! Encoder round trips (all lossless), hand-built files from the
//! documentation's layouts, and robustness against damage.

use bmp::{EncodeOptions, Format};
use testkit::Rng;

fn random_rgba(rng: &mut Rng, w: usize, h: usize, colours: usize) -> Vec<u8> {
    let pal: Vec<[u8; 4]> = (0..colours).map(|_| [rng.byte(), rng.byte(), rng.byte(), rng.byte()]).collect();
    (0..w * h).flat_map(|_| pal[rng.below(colours)]).collect()
}

#[test]
fn every_format_round_trips_exactly() {
    let mut rng = Rng::new(11);
    for &(w, h) in &[(1, 1), (2, 3), (3, 2), (5, 7), (31, 9), (64, 1), (1, 64), (127, 33)] {
        let img = random_rgba(&mut rng, w, h, 200);
        for top_down in [false, true] {
            let options = EncodeOptions { top_down, ..Default::default() };
            for format in [Format::Rgb24, Format::Rgba32, Format::Indexed8] {
                let bytes = bmp::encode_with_options(w as u32, h as u32, &img, format, options).unwrap();
                let back = bmp::decode(&bytes).unwrap();
                assert_eq!((back.width, back.height), (w as u32, h as u32));
                assert_eq!(back.info.top_down, top_down);
                let want: Vec<u8> = if format == Format::Rgba32 {
                    img.clone()
                } else {
                    img.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
                };
                assert_eq!(back.rgba, want, "{w}x{h} {format:?} top_down {top_down}");
                assert_eq!(back.has_alpha, format == Format::Rgba32);
            }
        }
    }
}

#[test]
fn palette_depths_round_trip() {
    let mut rng = Rng::new(12);
    for bits in [1u16, 4, 8] {
        let n = 1usize << bits;
        let palette: Vec<[u8; 3]> = (0..n).map(|_| [rng.byte(), rng.byte(), rng.byte()]).collect();
        for &(w, h) in &[(1, 1), (7, 3), (9, 9), (33, 2)] {
            let idx: Vec<u8> = (0..w * h).map(|_| rng.below(n) as u8).collect();
            let bytes =
                bmp::encode_indexed(w as u32, h as u32, &palette, &idx, bits, EncodeOptions::default()).unwrap();
            let back = bmp::decode(&bytes).unwrap();
            assert_eq!(back.info.bits_per_pixel, bits);
            for (k, p) in back.rgba.as_chunks::<4>().0.iter().enumerate() {
                let c = palette[usize::from(idx[k])];
                assert_eq!(p, &[c[0], c[1], c[2], 255]);
            }
        }
    }
}

#[test]
fn too_many_colours_for_a_palette_are_refused() {
    let img: Vec<u8> = (0..300u32).flat_map(|i| [i as u8, (i >> 8) as u8, 0, 255]).collect();
    assert!(matches!(bmp::encode(300, 1, &img, Format::Indexed8), Err(bmp::Error::BadInput(_))));
    assert!(bmp::encode(0, 1, &[], Format::Rgb24).is_err());
    assert!(bmp::encode(2, 2, &[0; 3], Format::Rgb24).is_err());
    assert!(bmp::encode_indexed(1, 1, &[[0; 3]; 2], &[2], 1, EncodeOptions::default()).is_err());
}

/// A BITMAPINFOHEADER file around `pixels`.
fn file(width: i32, height: i32, bits: u16, compression: u32, extra: &[u8], pixels: &[u8]) -> Vec<u8> {
    let off = 14 + 40 + extra.len();
    let mut v = b"BM".to_vec();
    v.extend_from_slice(&((off + pixels.len()) as u32).to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&(off as u32).to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&width.to_le_bytes());
    v.extend_from_slice(&height.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(&compression.to_le_bytes());
    v.extend_from_slice(&[0; 20]);
    v.extend_from_slice(extra);
    v.extend_from_slice(pixels);
    v
}

#[test]
fn rle8_example_from_the_documentation() {
    // The compressed-bitmap example of the BITMAPINFOHEADER documentation:
    // 03 04  05 06  00 03 45 56 67 00  02 78  00 02 05 01  02 78  00 00
    // 09 1E  00 01, on a palette where index i is grey level i.
    let rle = [
        0x03, 0x04, 0x05, 0x06, 0x00, 0x03, 0x45, 0x56, 0x67, 0x00, 0x02, 0x78, 0x00, 0x02, 0x05, 0x01, 0x02, 0x78,
        0x00, 0x00, 0x09, 0x1E, 0x00, 0x01,
    ];
    let palette: Vec<u8> = (0..=255u8).flat_map(|i| [i, i, i, 0]).collect();
    let (w, h) = (20, 3);
    let data = file(w, h, 8, 1, &palette, &rle);
    let img = bmp::decode(&data).unwrap();
    let px = |x: usize, row: usize| {
        let y = h as usize - 1 - row;
        let p = &img.rgba[(y * w as usize + x) * 4..][..4];
        if p[3] == 0 { None } else { Some(p[0]) }
    };
    // Row 0 (bottom): 04 04 04 06 06 06 06 06 45 56 67 78 78, then a delta
    // of 5 right and 1 up.
    let row0: Vec<Option<u8>> = (0..13).map(|x| px(x, 0)).collect();
    let want0: Vec<Option<u8>> =
        [4, 4, 4, 6, 6, 6, 6, 6, 0x45, 0x56, 0x67, 0x78, 0x78].iter().map(|&v| Some(v)).collect();
    assert_eq!(row0, want0);
    assert_eq!(px(13, 0), None);
    // After the delta: x = 18 on row 1, two 78s, end of line.
    assert_eq!((px(17, 1), px(18, 1), px(19, 1)), (None, Some(0x78), Some(0x78)));
    // Row 2: nine 1E.
    assert!((0..9).all(|x| px(x, 2) == Some(0x1E)));
    assert!(img.has_alpha);
}

#[test]
fn bitfields_scale_to_full_range() {
    // 16-bit 5-6-5 white and a mid grey.
    let masks: Vec<u8> = [0xF800u32, 0x07E0, 0x001F].iter().flat_map(|m| m.to_le_bytes()).collect();
    let pixels = [0xFF, 0xFF, 0x10, 0x84]; // 0xFFFF, 0x8410
    let img = bmp::decode(&file(2, 1, 16, 3, &masks, &pixels)).unwrap();
    assert_eq!(&img.rgba[..4], &[255, 255, 255, 255]);
    // 0x8410: r = 16/31, g = 32/63, b = 16/31.
    assert_eq!(&img.rgba[4..], &[132, 130, 132, 255]);
}

fn exercise(data: &[u8]) {
    let limits = bmp::Limits { max_pixels: 1 << 22 };
    if let Ok(img) = bmp::decode_with_limits(data, limits) {
        assert_eq!(img.rgba.len(), img.width as usize * img.height as usize * 4);
    }
    let _ = bmp::read_info(data);
}

#[test]
fn damaged_files_never_panic() {
    let mut rng = Rng::new(13);
    let img = random_rgba(&mut rng, 13, 11, 16);
    for format in [Format::Rgb24, Format::Rgba32, Format::Indexed8] {
        for d in testkit::damaged(&bmp::encode(13, 11, &img, format).unwrap(), 2, 2000) {
            exercise(&d);
        }
    }
    if let Some(dir) = testkit::corpus("bmpsuite") {
        let mut n = 0;
        for sub in ["g", "q", "b"] {
            for entry in std::fs::read_dir(dir.join(sub)).unwrap() {
                let data = std::fs::read(entry.unwrap().path()).unwrap();
                for d in testkit::damaged(&data, n, 150) {
                    exercise(&d);
                }
                n += 1;
            }
        }
        eprintln!("damaged {n} BMP Suite files");
    }
}

#[test]
fn declared_sizes_over_the_limit_are_refused_before_allocating() {
    let data = file(100_000, 100_000, 24, 0, &[], &[]);
    assert!(matches!(bmp::decode(&data), Err(bmp::Error::LimitExceeded(_))));
    let data = file(i32::MAX, -i32::MAX, 32, 0, &[], &[]);
    assert!(matches!(bmp::decode(&data), Err(bmp::Error::LimitExceeded(_))));
    // Within the limit but with no pixel data: truncated, not a panic.
    let data = file(1000, 1000, 24, 0, &[], &[1, 2, 3]);
    assert!(matches!(bmp::decode(&data), Err(bmp::Error::Truncated)));
}
