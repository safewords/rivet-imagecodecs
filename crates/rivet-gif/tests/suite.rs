//! Robert Ancell's GIF test suite (fetched by tools/fetch_corpora.py): every
//! test's expected frames, delays, loop count, comment and colour profile.
//!
//! The suite counts "frames" as a viewer shows them: an image whose delay is
//! zero (or that has no Graphic Control Extension) is shown together with
//! the images after it, up to one with a delay, unless the test is marked
//! `force-animation`, when every image is a frame. This decoder hands back
//! every image composited; the grouping is applied here.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

fn parse_conf(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = line[1..line.len() - 1].to_string();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            sections.entry(current.clone()).or_default().insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    sections
}

fn check(dir: &Path, name: &str) -> Result<(), String> {
    let conf = parse_conf(&fs::read_to_string(dir.join(format!("{name}.conf"))).map_err(|e| e.to_string())?);
    let cfg = &conf["config"];
    let data = fs::read(dir.join(&cfg["input"])).map_err(|e| e.to_string())?;
    let frames_spec: Vec<&str> = cfg["frames"].split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let decoded = gif::decode(&data);
    if frames_spec.is_empty() {
        return match decoded {
            Err(_) => Ok(()),
            Ok(a) if a.frames.is_empty() => Ok(()),
            Ok(a) => Err(format!("expected no frames, decoded {}", a.frames.len())),
        };
    }
    let mut anim = decoded.map_err(|e| format!("decode failed: {e}"))?;
    // A file with no image at all is shown by the suite as one empty
    // (transparent) frame.
    if anim.frames.is_empty() {
        anim.frames.push(gif::Frame {
            rgba: vec![0; usize::from(anim.info.width) * usize::from(anim.info.height) * 4],
            delay_cs: 0,
            disposal: gif::Disposal::Unspecified,
            rect: (0, 0, 0, 0),
            interlaced: false,
            user_input: false,
            incomplete: false,
        });
    }
    let w: u16 = cfg["width"].parse().unwrap();
    let h: u16 = cfg["height"].parse().unwrap();
    if (anim.info.width, anim.info.height) != (w, h) {
        return Err(format!("size {}x{}", anim.info.width, anim.info.height));
    }
    let force = cfg.get("force-animation").is_some_and(|v| v == "yes");
    let n = anim.frames.len();
    let shown: Vec<&gif::Frame> = anim
        .frames
        .iter()
        .enumerate()
        .filter(|(i, f)| force || f.delay_cs > 0 || *i == n - 1)
        .map(|(_, f)| f)
        .collect();
    if shown.len() != frames_spec.len() {
        return Err(format!("{} frames shown, expected {}", shown.len(), frames_spec.len()));
    }
    for (k, (spec, frame)) in frames_spec.iter().zip(&shown).enumerate() {
        let s = &conf[*spec];
        let want = fs::read(dir.join(&s["pixels"])).map_err(|e| e.to_string())?;
        if want != frame.rgba {
            let first = want.chunks(4).zip(frame.rgba.chunks(4)).position(|(a, b)| a != b);
            return Err(format!("frame {k} pixels differ (first at pixel {first:?})"));
        }
        if let Some(d) = s.get("delay")
            && d.parse::<u16>().unwrap() != frame.delay_cs
        {
            return Err(format!("frame {k} delay {} expected {d}", frame.delay_cs));
        }
    }
    let loops = match anim.loop_count {
        // A forced animation (several images, no loop extension) is shown
        // looping by the suite's convention.
        None if force => "infinite".to_string(),
        None => "0".to_string(),
        Some(0) => "infinite".to_string(),
        Some(n) => n.to_string(),
    };
    if loops != cfg["loop-count"] {
        return Err(format!("loop count {loops} expected {}", cfg["loop-count"]));
    }
    if let Some(c) = cfg.get("comment") {
        // The .conf holds the comment's bytes between quotes, with NUL
        // written as a backslash escape.
        let want = c.trim_matches('\'').replace(r"\x00", "\0").into_bytes();
        let got = anim.comments.first().cloned().unwrap_or_default();
        if got != want {
            return Err(format!("comment {got:?} expected {want:?}"));
        }
    }
    if let Some(p) = cfg.get("color-profile") {
        let want = fs::read(dir.join(p)).map_err(|e| e.to_string())?;
        if anim.icc_profile.as_deref() != Some(&want[..]) {
            return Err("colour profile differs".into());
        }
    }
    Ok(())
}

/// Tests whose expectation this decoder deliberately does not meet.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[(
    "plain-text",
    "the suite expects no frames from a file with a Plain Text Extension followed by an image;      the extension is skipped (it is not rendered) and the image after it is decoded",
)];

#[test]
fn gif_test_suite() {
    let Some(dir) = testkit::corpus("gif-suite") else { return };
    let tests = fs::read_to_string(dir.join("TESTS")).unwrap();
    let mut pass = 0;
    let mut failures = Vec::new();
    for name in tests.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if KNOWN_DIVERGENCES.iter().any(|(n, _)| *n == name) {
            continue;
        }
        match check(&dir, name) {
            Ok(()) => pass += 1,
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    eprintln!(
        "GIF test suite: {pass} of {} pass ({} known divergences not run)",
        pass + failures.len(),
        KNOWN_DIVERGENCES.len()
    );
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    assert!(failures.is_empty(), "{} failures", failures.len());
}
