//! Encoder round trips (all lossless) and robustness against damage and
//! decompression bombs.

use testkit::Rng;
use tiff::{Compression, EncodeOptions, Encoder, PixelFormat, SampleData, Samples};

const FORMATS: [PixelFormat; 11] = [
    PixelFormat::Gray8,
    PixelFormat::Gray16,
    PixelFormat::GrayAlpha8,
    PixelFormat::GrayAlpha16,
    PixelFormat::Rgb8,
    PixelFormat::Rgb16,
    PixelFormat::Rgba8,
    PixelFormat::Rgba16,
    PixelFormat::Gray32F,
    PixelFormat::Rgb32F,
    PixelFormat::Rgba32F,
];

/// Smooth-ish content (so predictors matter) with noise.
fn samples(rng: &mut Rng, format: PixelFormat, w: usize, h: usize) -> Samples {
    let n = w * h * format.channels();
    let base = |i: usize| ((i / format.channels()) % w + (i / format.channels() / w) * 3) as u64;
    match format {
        PixelFormat::Gray8 | PixelFormat::GrayAlpha8 | PixelFormat::Rgb8 | PixelFormat::Rgba8 => {
            Samples::U8((0..n).map(|i| (base(i) as u8).wrapping_add(rng.byte() % 4)).collect())
        }
        PixelFormat::Gray32F | PixelFormat::Rgb32F | PixelFormat::Rgba32F => {
            Samples::F32((0..n).map(|i| base(i) as f32 / 97.0 + f32::from(rng.byte()) / 1e4 - 0.5).collect())
        }
        _ => Samples::U16((0..n).map(|i| (base(i) as u16).wrapping_mul(311).wrapping_add(rng.byte() as u16)).collect()),
    }
}

fn data(s: &Samples) -> SampleData<'_> {
    match s {
        Samples::U8(v) => SampleData::U8(v),
        Samples::U16(v) => SampleData::U16(v),
        Samples::F32(v) => SampleData::F32(v),
    }
}

#[test]
fn every_format_and_compression_round_trips_exactly() {
    let mut rng = Rng::new(21);
    let mut cases = 0;
    for format in FORMATS {
        for compression in [Compression::None, Compression::PackBits, Compression::Lzw, Compression::Deflate] {
            for predictor in [false, true] {
                for (big_endian, big_tiff) in [(false, false), (true, false), (false, true), (true, true)] {
                    let (w, h) = (1 + rng.below(70), 1 + rng.below(40));
                    let s = samples(&mut rng, format, w, h);
                    let options = EncodeOptions {
                        compression,
                        predictor,
                        big_endian,
                        big_tiff,
                        strip_bytes: 1 + rng.below(3000),
                        ..Default::default()
                    };
                    let bytes = tiff::encode(w as u32, h as u32, format, data(&s), &options).unwrap();
                    let file = tiff::Tiff::new(&bytes).unwrap();
                    assert_eq!((file.is_big_endian(), file.is_big_tiff()), (big_endian, big_tiff));
                    let img = file.decode(0).unwrap();
                    assert_eq!((img.width, img.height), (w as u32, h as u32));
                    assert_eq!(img.color.channels(), format.channels());
                    assert!(
                        img.samples == s,
                        "{format:?} {compression:?} predictor {predictor} MM {big_endian} BigTIFF {big_tiff} {w}x{h}"
                    );
                    assert_eq!(img.info.compression, compression);
                    cases += 1;
                }
            }
        }
    }
    eprintln!("{cases} encoder configurations round-trip exactly");
}

#[test]
fn multi_page_files_keep_every_page() {
    let mut rng = Rng::new(22);
    let mut e = Encoder::new(EncodeOptions::default()).unwrap();
    let mut pages = Vec::new();
    for (k, format) in FORMATS.iter().enumerate() {
        let (w, h) = (3 + k, 5 + 2 * k);
        let s = samples(&mut rng, *format, w, h);
        e.add_page(w as u32, h as u32, *format, data(&s)).unwrap();
        pages.push((w, h, s));
    }
    let bytes = e.finish().unwrap();
    let decoded = tiff::decode_pages(&bytes).unwrap();
    assert_eq!(decoded.len(), pages.len());
    for (k, (img, (w, h, s))) in decoded.iter().zip(&pages).enumerate() {
        assert_eq!((img.width as usize, img.height as usize), (*w, *h));
        assert!(img.samples == *s, "page {k}");
        assert_eq!(img.info.page_number, Some((k as u16, pages.len() as u16)));
    }
}

#[test]
fn predictors_and_compression_shrink_smooth_images() {
    let (w, h) = (256usize, 256usize);
    let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, ((i % w + i / w) / 2) as u8]).collect();
    let size = |compression, predictor| {
        let o = EncodeOptions { compression, predictor, ..Default::default() };
        tiff::encode(w as u32, h as u32, PixelFormat::Rgb8, SampleData::U8(&rgb), &o).unwrap().len()
    };
    let raw = size(Compression::None, false);
    let lzw = size(Compression::Lzw, false);
    let lzw_p = size(Compression::Lzw, true);
    let zip_p = size(Compression::Deflate, true);
    eprintln!("gradient 256x256 RGB: none {raw}, LZW {lzw}, LZW+predictor {lzw_p}, Deflate+predictor {zip_p}");
    assert!(lzw_p < lzw && lzw_p * 10 < raw && zip_p * 10 < raw);
}

#[test]
fn bad_encoder_input_is_refused() {
    let o = EncodeOptions::default();
    assert!(tiff::encode(0, 1, PixelFormat::Gray8, SampleData::U8(&[]), &o).is_err());
    assert!(tiff::encode(2, 2, PixelFormat::Gray8, SampleData::U8(&[0; 3]), &o).is_err());
    assert!(tiff::encode(1, 1, PixelFormat::Gray16, SampleData::U8(&[0; 2]), &o).is_err());
    assert!(Encoder::new(EncodeOptions { compression: Compression::Jpeg, ..o }).is_err());
    assert!(Encoder::new(o).unwrap().finish().is_err());
}

fn exercise(data: &[u8]) {
    let limits = tiff::Limits { max_pixels: 1 << 22, max_alloc: 1 << 26, max_pages: 64 };
    if let Ok(t) = tiff::Tiff::with_limits(data, limits) {
        for i in 0..t.page_count() {
            let _ = t.info(i);
            if let Ok(img) = t.decode(i) {
                assert_eq!(img.to_rgba8().len(), img.width as usize * img.height as usize * 4);
                let _ = img.to_rgba8_upright();
            }
        }
    }
}

#[test]
fn damaged_files_never_panic() {
    let mut rng = Rng::new(23);
    for (format, compression) in [
        (PixelFormat::Rgb8, Compression::Lzw),
        (PixelFormat::Gray16, Compression::Deflate),
        (PixelFormat::Rgba8, Compression::PackBits),
        (PixelFormat::Rgb32F, Compression::Lzw),
    ] {
        let s = samples(&mut rng, format, 17, 13);
        for big_tiff in [false, true] {
            let o = EncodeOptions { compression, big_tiff, strip_bytes: 200, ..Default::default() };
            let bytes = tiff::encode(17, 13, format, data(&s), &o).unwrap();
            for d in testkit::damaged(&bytes, rng.next_u64(), 1500) {
                exercise(&d);
            }
        }
    }
    if let Some(root) = testkit::corpora() {
        let mut n = 0;
        for dir in ["libtiff-pics", "libtiff-pics/depth", "libtiff-test"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap() {
                let p = e.unwrap().path();
                if p.extension().is_some_and(|e| e == "tif" || e == "tiff") {
                    let data = std::fs::read(&p).unwrap();
                    if data.len() > 400_000 {
                        continue;
                    }
                    for d in testkit::damaged(&data, n, 60) {
                        exercise(&d);
                    }
                    n += 1;
                }
            }
        }
        eprintln!("damaged {n} corpus files");
    }
}

/// A classic little-endian TIFF with the given (tag, type, count, value)
/// entries in one directory at offset 8, followed by `tail`.
fn handmade(entries: &[(u16, u16, u32, u32)], tail: &[u8]) -> Vec<u8> {
    let mut v = b"II*\0".to_vec();
    v.extend_from_slice(&8u32.to_le_bytes());
    v.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for &(t, ty, c, val) in entries {
        v.extend_from_slice(&t.to_le_bytes());
        v.extend_from_slice(&ty.to_le_bytes());
        v.extend_from_slice(&c.to_le_bytes());
        v.extend_from_slice(&val.to_le_bytes());
    }
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(tail);
    v
}

#[test]
fn bombs_are_refused_before_allocating() {
    // A 100000 x 100000 grey image.
    let d = handmade(&[(256, 4, 1, 100_000), (257, 4, 1, 100_000), (273, 4, 1, 8), (279, 4, 1, 1)], &[]);
    assert!(matches!(tiff::decode(&d), Err(tiff::Error::LimitExceeded(_))));
    // Small image, enormous tiles.
    let d = handmade(
        &[(256, 4, 1, 16), (257, 4, 1, 16), (322, 4, 1, 1 << 30), (323, 4, 1, 1 << 30), (324, 4, 1, 8), (325, 4, 1, 1)],
        &[],
    );
    assert!(matches!(tiff::decode(&d), Err(tiff::Error::LimitExceeded(_))));
    // A Deflate strip that inflates far beyond the image: a 4x4 image whose
    // strip is 10 MB of zeros, compressed.
    let zeros = vec![0u8; 10 << 20];
    let z = rpng_zlib(&zeros);
    let off = 8 + 2 + 6 * 12 + 4;
    let d = handmade(
        &[
            (256, 4, 1, 4),
            (257, 4, 1, 4),
            (259, 3, 1, 8),
            (262, 3, 1, 1),
            (273, 4, 1, off),
            (279, 4, 1, z.len() as u32),
        ],
        &z,
    );
    // The decoder stops at what the image needs, or refuses; it never
    // holds the 10 MB.
    match tiff::decode(&d) {
        Ok(img) => assert_eq!(img.samples, Samples::U8(vec![0; 16])),
        Err(e) => assert!(matches!(e, tiff::Error::Invalid(_)), "{e}"),
    }
    // A directory that claims four billion entries.
    let mut d = b"II*\0\x08\0\0\0\xFF\xFF".to_vec();
    d.extend_from_slice(&[0; 20]);
    assert!(tiff::decode(&d).is_err());
    // Directory chains that loop end where they loop.
    let mut d = handmade(&[(256, 4, 1, 1), (257, 4, 1, 1), (273, 4, 1, 8), (279, 4, 1, 1)], &[]);
    let next_at = 8 + 2 + 4 * 12;
    d[next_at..next_at + 4].copy_from_slice(&8u32.to_le_bytes());
    assert_eq!(tiff::Tiff::new(&d).unwrap().page_count(), 1);
}

fn rpng_zlib(data: &[u8]) -> Vec<u8> {
    // Through the TIFF encoder's own Deflate path: a one-strip Gray8 page.
    let o = EncodeOptions {
        compression: Compression::Deflate,
        predictor: false,
        strip_bytes: usize::MAX,
        ..Default::default()
    };
    let bytes = tiff::encode(data.len() as u32, 1, PixelFormat::Gray8, SampleData::U8(data), &o).unwrap();
    let t = tiff::read_info(&bytes).unwrap();
    assert_eq!(t.compression, Compression::Deflate);
    // The strip is the only data before the directory: bytes 8 .. the
    // directory offset.
    let dir = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    bytes[8..dir].to_vec()
}
