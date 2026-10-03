//! Encoder round trips: exact wherever the input fits GIF (256 colours or
//! fewer per frame, binary alpha), close otherwise.

use gif::{EncodeOptions, Encoder, PaletteMode};
use testkit::Rng;

/// Transparent pixels as the decoder reports them: all zero.
fn normalise(rgba: &[u8]) -> Vec<u8> {
    let mut v = rgba.to_vec();
    for px in v.as_chunks_mut::<4>().0.iter_mut() {
        if px[3] < 128 {
            px.copy_from_slice(&[0; 4]);
        } else {
            px[3] = 255;
        }
    }
    v
}

fn palette_frame(rng: &mut Rng, w: usize, h: usize, colours: &[[u8; 4]]) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 4);
    // Blocky content so differencing has something to find.
    let bw = 1 + rng.below(6);
    let cells: Vec<[u8; 4]> = (0..(w / bw + 1) * (h / bw + 1)).map(|_| colours[rng.below(colours.len())]).collect();
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&cells[(y / bw) * (w / bw + 1) + x / bw]);
        }
    }
    v
}

#[test]
fn still_images_with_few_colours_are_exact() {
    let mut rng = Rng::new(1);
    for &(w, h, n) in &[(1, 1, 1), (2, 2, 2), (17, 5, 3), (64, 64, 16), (100, 37, 200), (40, 40, 256)] {
        let colours: Vec<[u8; 4]> = (0..n).map(|_| [rng.byte(), rng.byte(), rng.byte(), 255]).collect();
        let img = palette_frame(&mut rng, w, h, &colours);
        let bytes = gif::encode(w as u16, h as u16, &img, &EncodeOptions::default()).unwrap();
        let anim = gif::decode(&bytes).unwrap();
        assert_eq!(anim.frames.len(), 1);
        assert_eq!(anim.frames[0].rgba, img, "{w}x{h} with {n} colours");
        assert_eq!(anim.loop_count, None);
    }
}

#[test]
fn transparency_is_exact() {
    let mut rng = Rng::new(2);
    let mut colours: Vec<[u8; 4]> = (0..255).map(|_| [rng.byte(), rng.byte(), rng.byte(), 255]).collect();
    colours.push([9, 9, 9, 0]);
    let img = palette_frame(&mut rng, 50, 30, &colours);
    let bytes = gif::encode(50, 30, &img, &EncodeOptions::default()).unwrap();
    assert_eq!(gif::decode(&bytes).unwrap().frames[0].rgba, normalise(&img));
}

#[test]
fn animations_are_exact_with_and_without_differencing() {
    let mut rng = Rng::new(3);
    for differencing in [true, false] {
        for case in 0..12 {
            let (w, h) = (1 + rng.below(60), 1 + rng.below(60));
            let ncol = 1 + rng.below(40);
            let mut colours: Vec<[u8; 4]> = (0..ncol).map(|_| [rng.byte(), rng.byte(), rng.byte(), 255]).collect();
            colours.push([0, 0, 0, 0]);
            let mut frames = vec![palette_frame(&mut rng, w, h, &colours)];
            for _ in 0..6 {
                // Change a rectangle of the previous frame, sometimes to
                // transparency, sometimes from it; sometimes nothing.
                let mut f = frames.last().unwrap().clone();
                if rng.below(4) != 0 {
                    let (x0, y0) = (rng.below(w), rng.below(h));
                    let (x1, y1) = (x0 + 1 + rng.below(w - x0), y0 + 1 + rng.below(h - y0));
                    let c = colours[rng.below(colours.len())];
                    for y in y0..y1 {
                        for x in x0..x1 {
                            f[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
                        }
                    }
                }
                frames.push(f);
            }
            let options = EncodeOptions { loop_count: Some(case as u16), differencing, ..Default::default() };
            let mut e = Encoder::new(w as u16, h as u16, options).unwrap();
            for (i, f) in frames.iter().enumerate() {
                e.add_frame(f, i as u16 * 3).unwrap();
            }
            let bytes = e.finish().unwrap();
            let anim = gif::decode(&bytes).unwrap();
            assert_eq!(anim.loop_count, Some(case as u16));
            assert_eq!(anim.frames.len(), frames.len());
            for (i, (got, want)) in anim.frames.iter().zip(&frames).enumerate() {
                assert_eq!(got.rgba, normalise(want), "case {case} frame {i} differencing {differencing}");
                assert_eq!(got.delay_cs, i as u16 * 3);
            }
        }
    }
}

#[test]
fn differencing_shrinks_a_mostly_static_animation() {
    let mut rng = Rng::new(4);
    let (w, h) = (120, 90);
    let colours: Vec<[u8; 4]> = (0..64).map(|_| [rng.byte(), rng.byte(), rng.byte(), 255]).collect();
    let base = palette_frame(&mut rng, w, h, &colours);
    let frames: Vec<Vec<u8>> = (0..10)
        .map(|k| {
            let mut f = base.clone();
            for y in 40..50 {
                for x in k * 10..k * 10 + 10 {
                    f[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
                }
            }
            f
        })
        .collect();
    let size = |differencing| {
        let mut e = Encoder::new(w as u16, h as u16, EncodeOptions { differencing, ..Default::default() }).unwrap();
        for f in &frames {
            e.add_frame(f, 10).unwrap();
        }
        e.finish().unwrap().len()
    };
    let (with, without) = (size(true), size(false));
    eprintln!("differencing: {with} bytes, whole frames: {without} bytes");
    assert!(with * 3 < without, "{with} vs {without}");
}

fn gradient(w: usize, h: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 127 / (w + h)) as u8 + 64, 255]);
        }
    }
    v
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mut se = 0f64;
    let mut n = 0f64;
    for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        for c in 0..3 {
            se += (f64::from(p[c]) - f64::from(q[c])).powi(2);
            n += 1.0;
        }
    }
    10.0 * (255.0f64.powi(2) / (se / n).max(1e-9)).log10()
}

#[test]
fn many_colours_are_quantised_closely() {
    let (w, h) = (160, 120);
    let img = gradient(w, h);
    for dither in [false, true] {
        let bytes = gif::encode(w as u16, h as u16, &img, &EncodeOptions { dither, ..Default::default() }).unwrap();
        let out = &gif::decode(&bytes).unwrap().frames[0].rgba;
        let p = psnr(&img, out);
        eprintln!("gradient, dither {dither}: PSNR {p:.2} dB, {} bytes", bytes.len());
        assert!(p > 30.0, "PSNR {p}");
    }
    // A small palette still lands near.
    let bytes =
        gif::encode(w as u16, h as u16, &img, &EncodeOptions { max_colors: 16, dither: true, ..Default::default() })
            .unwrap();
    let p = psnr(&img, &gif::decode(&bytes).unwrap().frames[0].rgba);
    assert!(p > 22.0, "16 colours: PSNR {p}");
}

#[test]
fn a_fixed_palette_is_used_for_every_frame() {
    let mut hist = gif::quantize::Histogram::new();
    let (w, h) = (64, 48);
    let frames: Vec<Vec<u8>> = (0..3)
        .map(|k| {
            let mut g = gradient(w, h);
            for px in g.as_chunks_mut::<4>().0.iter_mut() {
                px[0] = px[0].wrapping_add(k * 40);
            }
            g
        })
        .collect();
    for f in &frames {
        for px in f.as_chunks::<4>().0.iter() {
            hist.add([px[0], px[1], px[2]]);
        }
    }
    let palette = hist.palette(255);
    let mut e = Encoder::new(
        w as u16,
        h as u16,
        EncodeOptions { palette: PaletteMode::Fixed(palette.clone()), ..Default::default() },
    )
    .unwrap();
    for f in &frames {
        e.add_frame(f, 5).unwrap();
    }
    let bytes = e.finish().unwrap();
    let anim = gif::decode(&bytes).unwrap();
    assert_eq!(anim.info.global_palette.as_ref().unwrap()[..palette.len()], palette[..]);
    for (got, want) in anim.frames.iter().zip(&frames) {
        assert!(psnr(want, &got.rgba) > 26.0);
        for px in got.rgba.as_chunks::<4>().0.iter() {
            assert!(palette.contains(&[px[0], px[1], px[2]]));
        }
    }
}

#[test]
fn bad_encoder_input_is_refused() {
    assert!(gif::encode(0, 1, &[], &EncodeOptions::default()).is_err());
    assert!(gif::encode(2, 2, &[0; 15], &EncodeOptions::default()).is_err());
    assert!(Encoder::new(1, 1, EncodeOptions { max_colors: 1, ..Default::default() }).is_err());
    assert!(
        Encoder::new(1, 1, EncodeOptions { palette: PaletteMode::Fixed(vec![[0; 3]; 256]), ..Default::default() })
            .is_err()
    );
    assert!(Encoder::new(1, 1, EncodeOptions::default()).unwrap().finish().is_err());
}
