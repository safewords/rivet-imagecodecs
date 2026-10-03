# rivet-imagecodecs

[![CI](https://github.com/rivet-transcoder/rivet-imagecodecs/actions/workflows/ci.yml/badge.svg)](https://github.com/rivet-transcoder/rivet-imagecodecs/actions/workflows/ci.yml)

**GIF, BMP and TIFF decoders and encoders** in Rust, three crates in one
workspace: no C, no system libraries, no build script, no `unsafe`. Written
from the formats' specifications (GIF89a, Microsoft's BMP/DIB documentation,
TIFF 6.0 with its technical notes, ITU-T T.4/T.6), not translated from any
other implementation. Written for the
**[rivet](https://github.com/rivet-transcoder/rivet)** transcoder, where they
replace the `image` crate's GIF, BMP and TIFF support.

| crate | imported as | decodes | encodes |
|---|---|---|---|
| `rivet-gif` | `gif` | GIF87a/89a, composited animation | GIF89a, quantised, animated |
| `rivet-bmp` | `bmp` | BMP/DIB, every common variant | 24-bit, 32-bit + alpha, 1/4/8-bit palette |
| `rivet-tiff` | `tiff` | TIFF 6.0 baseline and common extensions, BigTIFF | grey/RGB(A), 8/16-bit and float, LZW/Deflate/PackBits |

`rivet-gif` and `rivet-bmp` depend on nothing but `std`. `rivet-tiff` takes
DEFLATE from rivet's PNG crate (`rivet-png`, written the same clean-room way).

```toml
[dependencies]
gif = { package = "rivet-gif", git = "https://github.com/rivet-transcoder/rivet-imagecodecs", branch = "develop" }
bmp = { package = "rivet-bmp", git = "https://github.com/rivet-transcoder/rivet-imagecodecs", branch = "develop" }
tiff = { package = "rivet-tiff", git = "https://github.com/rivet-transcoder/rivet-imagecodecs", branch = "develop" }
```

## GIF

**Decoding**: LZW (1 to 11-bit minimum code sizes, deferred clear, KwKwK),
global and local colour tables, interlaced images, transparency, the four
disposal methods (restore-to-background clears to transparent, as browsers
do), images clipped to the screen, NETSCAPE2.0 / ANIMEXTS1.0 loop counts,
comments, `ICCRGBG1012` ICC profiles. Every image is composited and returned
as a full-screen RGBA frame with its delay — one at a time from
`gif::Decoder`, or all at once from `gif::decode`. A file cut short shows
what was drawn before the cut, as a viewer does.

**Encoding** (`gif::Encoder`, `gif::encode`): exact palettes whenever a frame
has 256 colours or fewer (lossless round trips), otherwise median cut refined
by k-means, optional Floyd–Steinberg dithering, per-frame or fixed global
palettes; frames are cropped to what changed and unchanged pixels inside the
crop become transparent, with each frame's disposal chosen from what the next
needs; loop count.

```rust
let anim = gif::decode(&bytes)?;
for frame in &anim.frames {
    show(&frame.rgba, frame.delay_cs);
}
let mut e = gif::Encoder::new(w, h, gif::EncodeOptions { loop_count: Some(0), dither: true, ..Default::default() })?;
e.add_frame(&rgba, 10)?;
let out = e.finish()?;
```

## BMP

**Decoding**: 1, 2, 4 and 8-bit palettes; 16 and 32-bit with default layouts
or any `BI_BITFIELDS` / `BI_ALPHABITFIELDS` masks (channels of any width,
scaled to 8 bits with rounding; alpha when a mask gives it); 24-bit; RLE8,
RLE4 and OS/2 RLE24 (skipped pixels transparent); bottom-up and top-down;
OS/2 1.x (12-byte) and 2.x (16 to 64-byte) headers; Windows 40, 52, 56, 108
and 124-byte headers; embedded ICC profiles from V5 headers. Embedded JPEG
or PNG, OS/2 Huffman 1D and 64-bit pixels are refused.

**Encoding**: 24-bit (`BITMAPINFOHEADER`), 32-bit with alpha
(`BITMAPV4HEADER` with masks), 8-bit palette from RGBA when it fits, and
1/4/8-bit palettes from indices; bottom-up or top-down.

```rust
let img = bmp::decode(&bytes)?;            // img.rgba, img.width, img.height
let out = bmp::encode(w, h, &rgba, bmp::Format::Rgba32)?;
```

## TIFF

**Decoding**: classic and BigTIFF, little and big-endian; strips and tiles
(including pre-6.0 tiles located by StripOffsets); no compression, PackBits,
LZW (new style, and the old LSB-first style), Deflate (8 and 32946), CCITT
modified Huffman, Group 3 1D/2D and Group 4; the horizontal predictor (8 to
64-bit words) and the floating-point predictor; unsigned and signed integer
samples of 1 to 32 bits, 16/32/64-bit floats; bilevel, grey, RGB, palette,
CMYK and YCbCr (subsampled 1, 2 or 4 each way); associated or unassociated
alpha; chunky or planar; every page. Samples come back at their own
precision (`u8`, `u16` or `f32`); `Image::to_rgba8` and `to_rgba8_upright`
(which applies Orientation) give display pixels. JPEG compression (6 and 7),
LogLuv, CIE L\*a\*b\* and other rarities are refused with
`Error::Unsupported`.

**Encoding**: grey, grey + alpha, RGB, RGBA at 8 and 16 bits and 32-bit
float; none, PackBits, LZW or Deflate, with the horizontal or floating-point
predictor; any number of pages; classic or BigTIFF; either byte order.

```rust
let file = tiff::Tiff::new(&bytes)?;
for page in 0..file.page_count() {
    let image = file.decode(page)?;
    let (w, h, rgba) = image.to_rgba8_upright();
}
let out = tiff::encode(w, h, tiff::PixelFormat::Rgb16, tiff::SampleData::U16(&samples), &Default::default())?;
```

## Robustness

Malformed input never panics: every crate is fed thousands of truncated and
byte-corrupted files (encoder output and the corpora) in its tests, which run
with overflow checks. Declared sizes are checked against `Limits` (100
megapixels by default) before anything is allocated, and decompression never
produces more than the declared image needs, so a small file cannot become a
large allocation.

## How it is checked

Against the public corpora, fetched by `tools/fetch_corpora.py` (SHA-256
manifest; the files are data under their own licences and are not in the
repository), and by exact round trips. Full figures in
[docs/VALIDATION.md](docs/VALIDATION.md):

- **GIF test suite** (Robert Ancell): 83 of 83 tests match their reference
  frames, delays, loop counts, comments and profiles (one more, `plain-text`,
  is a recorded divergence).
- **BMP Suite 2.8** (Jason Summers): all 27 "good" files exactly match the
  reference PNGs; of 43 "questionable", 37 exact, 1 within one level (11-bit
  channels against an 8-bit reference), 1 exact once its ICC profile is
  applied, 4 refused (embedded JPEG/PNG, Huffman 1D, 64-bit); the 20 "bad"
  files are refused or decoded, never a panic.
- **libtiff images**: 76 of 89 files decode (the 13 others use JPEG,
  LogLuv, ThunderScan or WebP compression); 4 of 4 PNM references match
  exactly; 15 of 15 pairs of the same picture in different storage decode
  identically; bit-depth series agree; full fax pages (G3 1D/2D, G4) decode
  with every row valid; the BMP Suite's fax TIFF matches its reference.
- **Round trips**: GIF exact for ≤ 256-colour frames (stills and animations,
  with and without differencing); BMP exact in every format; TIFF exact in
  352 encoder configurations.

```sh
python3 tools/fetch_corpora.py          # into ./corpora
cargo test --workspace -- --nocapture   # prints per-corpus pass counts
```

## Provenance and licensing

Clean-room: see [docs/PROVENANCE.md](docs/PROVENANCE.md) for every source and
the rules followed. Licensed under the Open Encoding Attribution License 1.0
([LICENSE.md](LICENSE.md)) — source-available, not open source; see
[NOTICE](NOTICE).
