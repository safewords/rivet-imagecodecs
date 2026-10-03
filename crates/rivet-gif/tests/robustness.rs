//! Malformed input never panics, and declared sizes are checked before
//! anything is allocated.

use gif::{EncodeOptions, Encoder, Limits};
use testkit::Rng;

fn exercise(data: &[u8]) {
    let limits = Limits { max_pixels: 1 << 22, max_frames: 1000, max_total_bytes: 1 << 26 };
    if let Ok(mut d) = gif::Decoder::with_limits(data, limits) {
        let mut n = 0;
        while let Ok(Some(f)) = d.next_frame() {
            assert_eq!(f.rgba.len(), usize::from(d.info().width) * usize::from(d.info().height) * 4);
            n += 1;
            if n > 1000 {
                break;
            }
        }
    }
    let _ = gif::decode_with_limits(data, limits);
    let _ = gif::read_info(data);
}

fn sample_animation() -> Vec<u8> {
    let mut rng = Rng::new(9);
    let (w, h) = (23u16, 17u16);
    let mut e = Encoder::new(w, h, EncodeOptions { loop_count: Some(0), ..Default::default() }).unwrap();
    for k in 0..4 {
        let f: Vec<u8> = (0..usize::from(w) * usize::from(h))
            .flat_map(|i| if (i + k) % 7 == 0 { [0, 0, 0, 0] } else { [rng.byte(), (i % 251) as u8, 7, 255] })
            .collect();
        e.add_frame(&f, 4).unwrap();
    }
    e.finish().unwrap()
}

#[test]
fn damaged_encoder_output_never_panics() {
    let data = sample_animation();
    for d in testkit::damaged(&data, 1, 3000) {
        exercise(&d);
    }
}

#[test]
fn damaged_corpus_files_never_panic() {
    let Some(dir) = testkit::corpus("gif-suite") else { return };
    let mut files = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "gif") {
            let data = std::fs::read(&path).unwrap();
            for d in testkit::damaged(&data, files, 200) {
                exercise(&d);
            }
            files += 1;
        }
    }
    eprintln!("damaged {files} corpus files");
}

#[test]
fn random_bytes_after_a_valid_header_never_panic() {
    let mut rng = Rng::new(5);
    for _ in 0..3000 {
        let mut d = b"GIF89a\x10\x00\x10\x00\xF1\x00\x00".to_vec();
        d.extend((0..12).map(|_| rng.byte()));
        let n = rng.below(300);
        d.extend((0..n).map(|_| match rng.below(5) {
            0 => 0x2C,
            1 => 0x21,
            2 => 0,
            _ => rng.byte(),
        }));
        exercise(&d);
    }
}

#[test]
fn declared_sizes_over_the_limit_are_refused_before_allocating() {
    // 65535 x 65535 logical screen.
    let mut d = b"GIF89a\xFF\xFF\xFF\xFF\x00\x00\x00".to_vec();
    d.push(0x3B);
    assert!(matches!(gif::decode(&d), Err(gif::Error::LimitExceeded(_))));
    // A small screen with a huge image inside it.
    let mut d = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xFF\xFF\xFF".to_vec();
    d.extend_from_slice(&[0x2C, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 2, 2, 0x4C, 0x01, 0, 0x3B]);
    assert!(matches!(gif::decode(&d), Err(gif::Error::LimitExceeded(_))));
    // Many tiny frames stop at the frame limit.
    let mut e = Encoder::new(1, 1, EncodeOptions::default()).unwrap();
    for k in 0..50u8 {
        e.add_frame(&[k, 0, 0, 255], 1).unwrap();
    }
    let bytes = e.finish().unwrap();
    let limits = Limits { max_frames: 10, ..Limits::default() };
    assert!(matches!(gif::decode_with_limits(&bytes, limits), Err(gif::Error::LimitExceeded(_))));
    let limits = Limits { max_total_bytes: 40, ..Limits::default() };
    assert!(matches!(gif::decode_with_limits(&bytes, limits), Err(gif::Error::LimitExceeded(_))));
}

#[test]
fn not_a_gif() {
    assert!(gif::decode(b"").is_err());
    assert!(gif::decode(b"GIF").is_err());
    assert!(gif::decode(b"GIF90a\x01\x00\x01\x00\x00\x00\x00;").is_err());
    assert!(gif::decode(b"\x89PNG\r\n\x1a\n").is_err());
}
