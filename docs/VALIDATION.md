# Validation

Figures measured 2026-10-03 on Windows x86-64, Rust 1.99, the test profile
(optimised, overflow checks on). Reproduce with:

```sh
python3 tools/fetch_corpora.py
cargo test --workspace -- --nocapture
```

The corpora are fetched from their publishers and checked file by file
against `tools/corpora.sha256` (439 files). Without them the corpus tests
print a note and skip; everything else runs anywhere.

## GIF — Robert Ancell's GIF test suite

The `test-suite/` data of pygif at commit `741b282`: 84 GIFs, each with a
`.conf` giving the expected screen size, frames (raw `.rgba` reference
pixels), delays, loop count, comment and colour profile.
`crates/rivet-gif/tests/suite.rs` checks all of them.

**83 of 83 pass**, plus one recorded divergence (below).

The suite describes what a viewer shows, so the harness applies three of
its viewer conventions to the decoder's per-image output (the decoder
itself returns one composited frame per image, which is the format's model):

- an image with no delay is shown together with the images after it, up to
  one with a delay (unless the test is `force-animation`, when each image is
  a frame);
- a file with no image at all shows one transparent frame;
- a forced animation without a loop extension loops forever.

Divergence: **`plain-text`** expects no frames from a file whose Plain Text
Extension is followed by an ordinary image. This decoder skips the
extension (plain text is not rendered — no viewer in practice does) and
decodes the image after it, so it returns one frame.

Encoder (`tests/roundtrip.rs`): stills with 1 to 256 colours and binary
transparency are bit-exact; 24 random animations (frames with changes,
transparency appearing and disappearing, unchanged frames) are bit-exact
with and without frame differencing, with delays and loop counts kept.
Differencing makes a 10-frame 120×90 animation with a moving block 5 384
bytes against 42 558 for whole frames. A 160×120 gradient with thousands of
colours quantises to 36.4 dB PSNR (34.2 dB dithered).

## BMP — Jason Summers' BMP Suite 2.8

Every file listed in the suite's `html/bmpsuite.html`, compared pixel for
pixel with the reference PNG(s) that page gives for it (where it gives
several acceptable renderings, any one counts). `crates/rivet-bmp/tests/bmpsuite.rs`.

| group | files | exact | within 1 level | exact after ICC | refused | mismatch |
|---|---|---|---|---|---|---|
| g (good) | 27 | **27** | 0 | 0 | 0 | 0 |
| q (questionable) | 43 | 37 | 1 | 1 | 4 | 0 |
| b (bad) | 20 | 14 decoded | — | — | 6 | — (no panics) |

- `q/rgb32-111110.bmp` (11-11-10-bit channels) is within one level of the
  8-bit reference: the reference was made at another precision; this
  decoder scales every channel width by rounding (`v × 255 / max`).
- `q/rgb24prof2.bmp` has red and green swapped and an ICC profile that
  swaps them back; the decoder returns the stored pixels and the profile
  (colour management is the caller's), and the pixels match the reference
  with the swap applied.
- Refused: `q/rgb24jpeg.bmp` and `q/rgb24png.bmp` (embedded JPEG / PNG),
  `q/pal1huffmsb.bmp` (OS/2 Huffman 1D), `q/rgba64.bmp` (64-bit pixels).
- `b/`: refused are badbitcount, badheadersize, badwidth, reallybig (over
  the pixel limit), rletopdown, shortfile; the other 14 decode to
  something without complaint (badrle*, badpalettesize, pal8badindex …).
- `x/ba-bm.bmp` is listed by the page but not shipped in the 2.8 release.

RLE files whose delta codes leave pixels undefined decode those pixels as
transparent, which matches the suite's first reference rendering.

Encoder: 24-bit, 32-bit with alpha and 8-bit palette, bottom-up and
top-down, at eight sizes covering every row padding: bit-exact. 1, 4 and
8-bit palette images from indices: exact. The RLE8 example of Microsoft's
`BITMAPINFOHEADER` documentation decodes as documented.

## TIFF — libtiff's images

`pics-3.8.0` (61 TIFFs) and `test/images` at v4.6.0 (28 TIFFs), plus the
fax TIFF in the BMP Suite's data. `crates/rivet-tiff/tests/corpus.rs`.

**Every file decodes (every page) or is refused for a known reason**:

| corpus | files | decode | refused by design |
|---|---|---|---|
| libtiff pics-3.8.0 | 61 | 54 (55 pages) | 7: JPEG, old JPEG ×2, SGILog ×3, ThunderScan |
| libtiff test/images | 28 | 22 (29 pages) | 6: old JPEG ×3, JPEG, SGILog, WebP |
| BMP Suite data | 1 | 1 | 0 |

What the decoded pixels are checked against:

- **PNM references** (libtiff ships them beside four test images): 4 of 4
  identical — 8-bit grey, 8-bit RGB, 16-bit RGB, 1-bit white-is-zero.
- **The same picture in different storage**, 15 of 15 pairs identical:
  PackBits strips vs uncompressed (SGI) tiles; LZW strips vs LZW tiles;
  new-style vs old-style LZW; big- vs little-endian float64 with
  Predictor 2; chunky vs planar RGB at 2, 4, 8, 10, 12, 14, 16, 24 and 32
  bits; chunky vs planar CMYK at 8 and 16 bits.
- **Bit-depth series** (GraphicsMagick's `depth/` images): at 8 bits and
  above every depth is within one level (8-bit output) of the 16-bit image.
  Below 8 bits the series was made by truncation (the error reaches a whole
  step, never more) while this decoder scales by rounding, so the bound is
  one step: worst 85 at 2 bits, 17 at 4, 5 at 6. Palette depths are each
  their own quantisation: mean error 14.6 / 8.3 / 2.4 levels at 2 / 4 / 8
  bits.
- **Fax**: `fax2d.tif` (G3 2D, 1728×1082), `g3test.tif` (G3, 1728×1103),
  `testfax4.tiff` (G4, 2453×3369) and `testfax3_bug_513.tiff` decode with
  every row's runs summing exactly to the width (any bad code fails the
  decode). The BMP Suite's `pal1huff.tif` (G3, Orientation 4) matches the
  suite's `pal1.png` exactly once turned upright.
- Viewed by eye (as PNGs written with rivet-png): the YCbCr images
  (`ycbcr-cat.tif` subsampled LZW, `dscf0013.tif`), associated-alpha
  `strike.tif`, CMYK, float64 `caspian.tif`, fax pages.

Encoder (`tests/roundtrip.rs`): 11 pixel formats (grey, grey + alpha, RGB,
RGBA at 8 and 16 bits; grey, RGB, RGBA float32) × none / PackBits / LZW /
Deflate × predictor on/off × II/MM × classic/BigTIFF, random sizes and
strip heights: **352 of 352 configurations bit-exact**. Multi-page files
keep every page and their PageNumber. A smooth 256×256 RGB image: 196 886
bytes raw, 4 826 with LZW + predictor, 2 080 with Deflate + predictor.

## Robustness (all crates)

- Thousands of damaged files per crate — every truncation of the first 80
  bytes and 60 more across each file, and random byte changes (half of
  them inside the first 64 bytes) — from encoder output and from 84 GIF,
  90 BMP and 86 TIFF corpus files: no panic, and every image that decodes
  has a buffer of exactly the size it reports.
- Random block sequences after a valid GIF header: no panic.
- Declared sizes over the limits are refused before allocation: a
  65 535² GIF screen, a 1×1 screen holding a 65 535² image, frame and byte
  limits on long animations; a 100 000² BMP, `i32::MAX` dimensions; a
  100 000² TIFF, 2³⁰-pixel tiles, a 4-billion-entry directory.
- Decompression is bounded by what the image needs: a 4×4 TIFF whose
  Deflate strip expands to 10 MB is cut off at the limit. Directory chains
  that loop stop where they loop.

## Ambiguities in the specifications, and what was chosen

GIF:
- **Loop count**: the NETSCAPE2.0 value is reported as stored (`None`
  without the extension, `Some(0)` forever). Whether `n` means *n* plays or
  *n* repeats after the first is not specified anywhere; callers decide.
- **Restore to background** clears to transparent, not to the background
  colour (what browsers do; the suite agrees). The background index is
  reported.
- **Disposal 4–7** are undefined; treated as 0 (none).
- **Pixel index beyond the colour table** fails the decode (the suite's
  `invalid-colors` expects that); a **transparent index beyond the table**
  is simply never matched.
- **Minimum code size** outside 2–8: 1 is read as written (2-bit codes);
  9–11 are accepted, with literals above 255 an error (the suite's
  `max-codes` uses 11).
- **A full table without a Clear** (deferred clear) keeps decoding with
  12-bit codes; a code past the table is an error.
- **Truncated files** yield the frames before the cut and a partial frame
  for an image cut short (its `incomplete` flag set); before any image it
  is an error.
- **Plain Text Extensions** are skipped, not rendered; they consume the
  Graphic Control Extension before them.

BMP:
- **Undefined pixels** left by RLE delta and early end-of-line codes are
  transparent (one of three renderings the BMP Suite accepts; `has_alpha`
  is set).
- **Top-down RLE** is refused, as the documentation forbids it.
- **Palette indices beyond the palette** read as opaque black.
- **Channel scaling**: a mask of *n* bits is scaled by `round(v × 255 /
  (2ⁿ − 1))` — not by bit shifting — matching the suite's references.
- **32-bit `BI_RGB`**: the fourth byte is unused, never alpha, whatever its
  contents (`rgb32fakealpha.bmp`); alpha only comes from a mask.
- **Header size 40** is read as Windows; an OS/2 2.x header of 40 bytes is
  indistinguishable and differs only for compressions 3 and 4.
- **Short palettes**: when `bfOffBits` leaves room for fewer entries than
  the depth implies, the palette ends there.
- **Planes ≠ 1** and implausible resolutions are accepted.
- Non-square pixels (`biXPelsPerMeter` ≠ `biYPelsPerMeter`) are reported,
  not resampled.

TIFF:
- **LZW code width** changes one code early for new-style data (511, 1023,
  2047) and on time for old-style (LSB-first) data, detected from the first
  byte.
- **Bits per sample not a multiple of 8** are packed most significant bit
  first whatever the byte order; 16, 24, 32 and 64-bit samples follow the
  byte order (confirmed by the 10/12/14 and 24-bit depth series).
- **Predictor 2 on floating-point data** differences the raw words
  (libtiff's float64 test files do this).
- **FillOrder 2** reverses the bits of every byte of compressed data before
  decompression, whatever the compression.
- **ExtraSamples 0** (unspecified) are dropped; 1 (associated alpha) is
  reported as `premultiplied` and divided out by `to_rgba8`; 2 is straight
  alpha.
- **Signed integer samples** are offset by half the range into unsigned
  (so −2ⁿ⁻¹ is black); TIFF does not say how to display them.
- **Floats** are passed through unscaled; `to_rgba8` maps 0.0–1.0.
  WhiteIsZero floats are reported as `1 − v`.
- **Tiles located by StripOffsets** (pre-6.0 SGI writers, `cramps-tile.tif`)
  are accepted when there is one offset per tile.
- **Deflate**: the Adler-32 trailer is not required, and a strip may
  decompress to more than the image needs (the last strip holding whole
  RowsPerStrip rows); the excess is dropped.
- **YCbCr subsampling** replicates chroma over its block;
  YCbCrPositioning (centred or co-sited) is ignored.
- **CMYK** is converted naively for `to_rgba8` (`R = (1 − C)(1 − K)`); the
  samples themselves are returned untouched.
- **Orientation** is reported, not applied by `decode`;
  `to_rgba8_upright` / `orient_rgba8` apply it.
- **Missing PhotometricInterpretation**: WhiteIsZero for fax compressions,
  RGB for three or more samples, else BlackIsZero.
- **Directory chains** that loop, or point outside the file after the
  first page, end there rather than failing the file.
