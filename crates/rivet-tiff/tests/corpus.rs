//! libtiff's sample images (pics-3.8.0) and test images (v4.6.0), and the
//! TIFF in the BMP Suite's data (fetched by tools/fetch_corpora.py).
//!
//! The corpora give few reference renderings, so they are checked four
//! ways: the PNM files libtiff ships beside some of its test images; files
//! that hold the same picture in different storage (strips and tiles,
//! chunky and planar, new- and old-style LZW, big- and little-endian); the
//! same picture at different bit depths; and the BMP Suite's own reference
//! PNG for its fax-coded TIFF. Every file is decoded; the only refusals
//! allowed are the compressions this crate does not implement.

use std::fs;
use std::path::{Path, PathBuf};

fn load(path: &Path) -> tiff::Image {
    let data = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    tiff::decode(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "tif" || e == "tiff") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Compressions and photometrics this crate refuses by design.
fn expected_refusal(e: &tiff::Error) -> bool {
    match e {
        tiff::Error::Unsupported(m) => {
            ["Jpeg", "OldJpeg", "34676", "34677", "32809", "50001"].iter().any(|k| m.contains(k))
        }
        _ => false,
    }
}

#[test]
fn every_file_decodes_or_is_refused_for_a_known_reason() {
    let Some(root) = testkit::corpora() else { return };
    for corpus in ["libtiff-pics", "libtiff-test", "bmpsuite/data"] {
        let dir = root.join(corpus);
        if !dir.is_dir() {
            continue;
        }
        let (mut ok, mut pages, mut refused) = (0, 0, Vec::new());
        let mut failed = Vec::new();
        for p in files(&dir) {
            let data = fs::read(&p).unwrap();
            let name = p.strip_prefix(&dir).unwrap().display().to_string();
            match tiff::Tiff::new(&data) {
                Ok(t) => {
                    let mut file_ok = true;
                    for i in 0..t.page_count() {
                        match t.decode(i) {
                            Ok(img) => {
                                assert_eq!(
                                    img.to_rgba8().len(),
                                    img.width as usize * img.height as usize * 4,
                                    "{name} page {i}"
                                );
                                pages += 1;
                            }
                            Err(e) if expected_refusal(&e) => {
                                refused.push(format!("{name}: {e}"));
                                file_ok = false;
                                break;
                            }
                            Err(e) => {
                                failed.push(format!("{name} page {i}: {e}"));
                                file_ok = false;
                                break;
                            }
                        }
                    }
                    ok += usize::from(file_ok);
                }
                Err(e) => failed.push(format!("{name}: {e}")),
            }
        }
        eprintln!(
            "{corpus}: {ok} of {} files decode ({pages} pages), {} refused by design, {} failed",
            ok + refused.len() + failed.len(),
            refused.len(),
            failed.len()
        );
        for r in &refused {
            eprintln!("  refused {r}");
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }
}

fn u8s(img: &tiff::Image) -> &[u8] {
    match &img.samples {
        tiff::Samples::U8(v) => v,
        other => panic!("expected 8-bit samples, got {other:?}"),
    }
}

#[test]
fn libtiff_test_images_match_their_pnm_references() {
    let Some(dir) = testkit::corpus("libtiff-test") else { return };
    for (tif, pnm) in [
        ("minisblack-1c-8b.tiff", "minisblack-1c-8b.pgm"),
        ("rgb-3c-8b.tiff", "rgb-3c-8b.ppm"),
        ("rgb-3c-16b.tiff", "rgb-3c-16b.ppm"),
        ("miniswhite-1c-1b.tiff", "miniswhite-1c-1b.pbm"),
    ] {
        let img = load(&dir.join(tif));
        let reference = testkit::read_pnm(&fs::read(dir.join(pnm)).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (reference.width, reference.height), "{tif}");
        assert_eq!(img.color.channels() as u32, reference.channels, "{tif}");
        let got: Vec<u16> = match &img.samples {
            tiff::Samples::U8(v) if reference.maxval == 1 => {
                // PBM: 1 is black; the decoder gives black as 0.
                v.iter().map(|&s| u16::from(s == 0)).collect()
            }
            tiff::Samples::U8(v) => v.iter().map(|&s| u16::from(s)).collect(),
            tiff::Samples::U16(v) => v.clone(),
            other => panic!("{tif}: {other:?}"),
        };
        assert_eq!(got, reference.samples, "{tif} differs from {pnm}");
        eprintln!("{tif} = {pnm}");
    }
}

#[test]
fn the_same_picture_stored_differently_decodes_the_same() {
    let Some(root) = testkit::corpora() else { return };
    let pics = root.join("libtiff-pics");
    let test = root.join("libtiff-test");
    let mut pairs: Vec<(PathBuf, PathBuf)> = vec![
        // PackBits strips against uncompressed (SGI-written) tiles.
        (pics.join("cramps.tif"), pics.join("cramps-tile.tif")),
        // LZW strips against LZW tiles, and against old-style LZW.
        (pics.join("quad-lzw.tif"), pics.join("quad-tile.tif")),
        (pics.join("quad-lzw.tif"), test.join("quad-lzw-compat.tiff")),
        // Floats with the horizontal predictor, in both byte orders.
        (test.join("test_float64_predictor2_be_lzw.tif"), test.join("test_float64_predictor2_le_lzw.tif")),
    ];
    for depth in ["02", "04", "08", "10", "12", "14", "16", "24", "32"] {
        let d = pics.join("depth");
        pairs
            .push((d.join(format!("flower-rgb-contig-{depth}.tif")), d.join(format!("flower-rgb-planar-{depth}.tif"))));
    }
    for depth in ["08", "16"] {
        let d = pics.join("depth");
        pairs.push((
            d.join(format!("flower-separated-contig-{depth}.tif")),
            d.join(format!("flower-separated-planar-{depth}.tif")),
        ));
    }
    let mut n = 0;
    for (a, b) in &pairs {
        if !a.exists() || !b.exists() {
            continue;
        }
        let (x, y) = (load(a), load(b));
        assert_eq!((x.width, x.height, x.color), (y.width, y.height, y.color), "{} vs {}", a.display(), b.display());
        assert!(x.samples == y.samples, "{} and {} differ", a.display(), b.display());
        n += 1;
    }
    eprintln!("{n} of {} storage pairs decode identically", pairs.len());
    assert_eq!(n, pairs.len());
}

/// The depth series was made by truncating 16-bit samples (each depth's
/// error from the 16-bit image reaches a whole step, never more), while
/// this decoder scales by rounding; so a `depth`-bit image may be up to one
/// step of `depth` bits from the 16-bit one. Palettes are each depth's own
/// quantisation, so only their mean error is bounded.
#[test]
fn bit_depths_agree() {
    let Some(dir) = testkit::corpus("libtiff-pics") else { return };
    let dir = dir.join("depth");
    for kind in ["minisblack", "rgb-contig", "palette"] {
        let reference = load(&dir.join(format!("flower-{kind}-16.tif"))).to_rgba8();
        for depth in [2u32, 4, 6, 8, 10, 12, 14, 24, 32] {
            let p = dir.join(format!("flower-{kind}-{depth:02}.tif"));
            if !p.exists() {
                continue;
            }
            let got = load(&p).to_rgba8();
            let diffs: Vec<u32> = got.iter().zip(&reference).map(|(a, b)| u32::from(a.abs_diff(*b))).collect();
            let worst = *diffs.iter().max().unwrap();
            let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
            eprintln!("flower-{kind}-{depth:02}: worst {worst}, mean {mean:.2} levels from 16-bit");
            if kind == "palette" {
                let bound = match depth {
                    2 => 20.0,
                    4 => 10.0,
                    _ => 4.0,
                };
                assert!(mean < bound, "flower-{kind}-{depth:02}: mean {mean}");
            } else {
                // One step, plus one for the two roundings to 8 bits.
                let step = if depth >= 8 { 1 } else { 255u32.div_ceil((1 << depth) - 1) + 1 };
                assert!(worst <= step, "flower-{kind}-{depth:02}: {worst} levels from the 16-bit image");
            }
        }
    }
}

#[test]
fn bmp_suite_fax_tiff_matches_its_reference() {
    let Some(dir) = testkit::corpus("bmpsuite") else { return };
    let img = load(&dir.join("data/pal1huff.tif"));
    // Stored bottom row first (Orientation 4), like the BMP it came from.
    assert_eq!(img.info.orientation, 4);
    let (w, h, rgba) = img.to_rgba8_upright();
    let reference = testkit::read_png(&fs::read(dir.join("html/pal1.png")).unwrap()).unwrap();
    assert_eq!((w, h), (reference.width, reference.height));
    assert_eq!(rgba, reference.data);
}

#[test]
fn fax_pages_decode_every_row() {
    // Group 3 1D and 2D, Group 4: a code error anywhere fails the decode, so
    // a clean decode of a full page exercises every table.
    let Some(root) = testkit::corpora() else { return };
    for (p, w, h) in [
        ("libtiff-pics/fax2d.tif", 1728, 1082),
        ("libtiff-pics/g3test.tif", 1728, 1103),
        ("libtiff-test/testfax4.tiff", 2453, 3369),
        ("libtiff-test/testfax3_bug_513.tiff", 32, 2),
    ] {
        let img = load(&root.join(p));
        assert_eq!((img.width, img.height), (w, h), "{p}");
        let black = u8s(&img).iter().filter(|&&v| v == 0).count();
        // Some ink, not all ink.
        assert!(black > 0 && black <= (w * h / 2) as usize, "{p}: {black} black pixels");
    }
}
