//! Jason Summers' BMP Suite 2.8 (fetched by tools/fetch_corpora.py).
//!
//! The suite's own `html/bmpsuite.html` lists, for every file, the
//! reference rendering(s) it alleges to be correct; that list is read here
//! and each decode is compared pixel for pixel with the reference PNGs.
//! Where the suite gives several acceptable renderings (undefined RLE
//! pixels shown transparent, black or as palette entry 0), matching any one
//! passes. Files in `b/` are invalid: they pass when they are refused or
//! decoded without a panic.

use std::fs;
use std::path::Path;

struct Row {
    bmp: String,
    references: Vec<String>,
}

fn rows(html: &str) -> Vec<Row> {
    let mut out = Vec::new();
    for tr in html.split("<tr").skip(1) {
        let mut srcs = Vec::new();
        let mut rest = tr;
        while let Some(i) = rest.find("src=\"") {
            rest = &rest[i + 5..];
            let end = rest.find('"').unwrap();
            srcs.push(rest[..end].to_string());
            rest = &rest[end..];
        }
        let Some(bmp) = srcs.iter().find(|s| s.ends_with(".bmp")) else { continue };
        let bmp = bmp.trim_start_matches("../").to_string();
        let references = srcs.iter().filter(|s| !s.ends_with(".bmp")).cloned().collect();
        out.push(Row { bmp, references });
    }
    out
}

/// The largest difference in any channel, treating fully transparent
/// pixels as equal whatever their colour.
fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    let mut worst = 0;
    for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        if p[3] == 0 && q[3] == 0 {
            continue;
        }
        for c in 0..4 {
            worst = worst.max(p[c].abs_diff(q[c]));
        }
    }
    worst
}

enum Outcome {
    Match,
    /// Within one level of an 8-bit reference, for channels stored with
    /// more than 8 bits (the reference was made at another precision).
    Near,
    /// Matches once its embedded ICC profile is applied (here: the suite's
    /// profile swaps red and green back).
    NeedsProfile,
    Refused(String),
    Mismatch(String),
}

fn check(dir: &Path, row: &Row) -> Outcome {
    let Ok(data) = fs::read(dir.join(&row.bmp)) else {
        return Outcome::Refused("file not in this release".into());
    };
    let img = match bmp::decode(&data) {
        Ok(i) => i,
        Err(e) => return Outcome::Refused(e.to_string()),
    };
    if row.bmp.starts_with("b/") {
        return Outcome::Match;
    }
    let mut notes = Vec::new();
    for r in &row.references {
        if !r.ends_with(".png") {
            notes.push(format!("{r}: not a PNG"));
            continue;
        }
        let png = testkit::read_png(&fs::read(dir.join("html").join(r)).unwrap()).unwrap();
        if (png.width, png.height) != (img.width, img.height) {
            notes.push(format!("{r}: size {}x{} vs {}x{}", png.width, png.height, img.width, img.height));
            continue;
        }
        let d = max_diff(&png.data, &img.rgba);
        if d == 0 {
            return Outcome::Match;
        }
        let wide = img.info.masks.is_some_and(|m| m.iter().any(|&v| v.count_ones() > 8));
        if d == 1 && wide {
            return Outcome::Near;
        }
        if row.bmp == "q/rgb24prof2.bmp" && img.icc_profile.is_some() {
            let mut swapped = img.rgba.clone();
            for p in swapped.as_chunks_mut::<4>().0 {
                p.swap(0, 1);
            }
            if max_diff(&png.data, &swapped) == 0 {
                return Outcome::NeedsProfile;
            }
        }
        notes.push(format!("{r}: max channel difference {d}"));
    }
    Outcome::Mismatch(notes.join("; "))
}

#[test]
fn bmp_suite() {
    let Some(dir) = testkit::corpus("bmpsuite") else { return };
    let html = fs::read_to_string(dir.join("html/bmpsuite.html")).unwrap();
    let rows = rows(&html);
    assert!(rows.len() > 80, "parsed {} rows", rows.len());
    let mut tally = std::collections::BTreeMap::<&str, [usize; 5]>::new();
    let mut mismatches = Vec::new();
    for row in &rows {
        let group = &row.bmp[..1];
        let group = match group {
            "g" => "g (good)",
            "q" => "q (questionable)",
            "b" => "b (bad)",
            _ => "x (other)",
        };
        let t = tally.entry(group).or_default();
        match check(&dir, row) {
            Outcome::Match => t[0] += 1,
            Outcome::Near => {
                t[1] += 1;
                eprintln!("  within one level {}", row.bmp);
            }
            Outcome::NeedsProfile => {
                t[2] += 1;
                eprintln!("  matches once its ICC profile is applied {}", row.bmp);
            }
            Outcome::Refused(why) => {
                t[3] += 1;
                eprintln!("  refused {}: {why}", row.bmp);
                if row.bmp.starts_with("g/") {
                    mismatches.push(format!("{}: refused: {why}", row.bmp));
                }
            }
            Outcome::Mismatch(why) => {
                t[4] += 1;
                eprintln!("  MISMATCH {}: {why}", row.bmp);
                mismatches.push(format!("{}: {why}", row.bmp));
            }
        }
    }
    for (g, [ok, near, icc, refused, bad]) in &tally {
        if g.starts_with('b') {
            eprintln!("BMP Suite {g}: {ok} decoded without complaint, {refused} refused, none panicked");
            continue;
        }
        eprintln!(
            "BMP Suite {g}: {ok} exact, {near} within one level, {icc} exact after ICC, {refused} refused, {bad} mismatch"
        );
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}
