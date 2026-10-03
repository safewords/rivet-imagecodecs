# Provenance

Where every part of these crates came from. The short version: the code is
this repository's own, written from the formats' public specifications; the
code tables (GIF/TIFF LZW parameters, the T.4 run-length and mode codes) are
the specifications' data, reproduced from the author's knowledge of them and
verified mechanically and against public test corpora; no implementation of
any of these formats was read.

## Clean-room rules

- **No other implementation's source was opened, read or searched for**:
  not image-rs (`gif`, `tiff`, `image`'s BMP), giflib, libtiff, libgd, stb,
  zlib, ImageMagick/GraphicsMagick, Pillow, FFmpeg or any other. None was
  run as an oracle either; there is no reference decoder in the tests.
- The corpora are used **as data only**: images in, documented expectations
  and reference renderings to compare against. Generator programs and
  decoders that ship alongside some of them (the BMP Suite's `bmpsuite.c`,
  pygif's Python decoder) are excluded by the fetch script and were not read.
- DEFLATE for TIFF comes from `rivet-png`, rivet's PNG crate, written under
  the same rules from RFC 1950/1951. (An earlier DEFLATE of this workspace's
  own, in the history, was dropped in its favour.)

## GIF (`rivet-gif`)

**GIF89a** (CompuServe, 31 July 1990; the text hosted by the W3C):
- §17–18: header and Logical Screen Descriptor (packed fields, global
  colour table size `2^(n+1)`, background index, aspect ratio).
- §19–21: colour tables, Image Descriptor (local table, interlace flag),
  table-based image data.
- §22: data sub-blocks and the block terminator.
- §23: Graphic Control Extension — disposal methods 0–3, user input,
  transparency flag and index, delay in hundredths of a second.
- §24–26: Comment, Plain Text and Application Extensions; the trailer.
- Appendix E: the interlace pass order (rows 0, 8, 16 …; 4, 12 …; 2, 6 …;
  1, 3 …).
- Appendix F: variable-length-code LZW — Clear `2^min`, End of Information
  `2^min + 1`, codes least significant bit first, widening when the next
  code reaches `2^width`, at most 12 bits, the KwKwK case.
- **NETSCAPE2.0 / ANIMEXTS1.0** application extension (sub-block 1: loop
  count, 0 = forever) and **ICCRGBG1012** (an ICC profile in sub-blocks):
  as publicly described; not part of GIF89a.

The encoder's colour reduction is Heckbert's median cut ("Color Image
Quantization for Frame Buffer Display", SIGGRAPH 1982) with Lloyd/k-means
refinement, and Floyd–Steinberg error diffusion ("An Adaptive Algorithm for
Spatial Greyscale", 1976): textbook algorithms, written here from those
descriptions.

## BMP (`rivet-bmp`)

**Microsoft's documentation** (Windows GDI, "Bitmap Storage" and the
structure references):
- `BITMAPFILEHEADER` (`BM`, size, `bfOffBits`).
- `BITMAPCOREHEADER` (12 bytes, 16-bit dimensions, `RGBTRIPLE` palette).
- `BITMAPINFOHEADER` (40 bytes): `biHeight` sign for top-down, `biBitCount`,
  `biCompression` (`BI_RGB`, `BI_RLE8`, `BI_RLE4`, `BI_BITFIELDS`, `BI_JPEG`,
  `BI_PNG`, `BI_ALPHABITFIELDS`), `biClrUsed`, default 5-5-5 and 8-8-8
  layouts, rows padded to 32 bits.
- "Bitmap Compression": RLE8 and RLE4 encoded and absolute modes, escape
  codes 0 (end of line), 1 (end of bitmap), 2 (delta), absolute runs padded
  to a word; its worked RLE8 example is a unit test.
- `BITMAPV4HEADER` (108 bytes: masks, `bV4CSType` with `LCS_sRGB`) and
  `BITMAPV5HEADER` (124 bytes: `PROFILE_EMBEDDED`, `bV5ProfileData` offset
  from the header's start, `bV5ProfileSize`).
- The 52 and 56-byte headers (masks inside the header) are undocumented by
  Microsoft; they are read as the V4 header's first 52/56 bytes, as the BMP
  Suite describes them.

**IBM OS/2 2.x** `BITMAPINFOHEADER2`: up to 64 bytes, any length from 16
(missing fields zero), unsigned dimensions, compression 3 = Huffman 1D and
4 = RLE24 (24-bit runs, BGR triples).

## TIFF (`rivet-tiff`)

**TIFF Revision 6.0** (Adobe / Aldus, 3 June 1992):
- §2: header (`II`/`MM`, 42), IFDs, field types 1–12, values inline or by
  offset, the directory chain.
- §§3–8 (baseline): ImageWidth/Length, BitsPerSample, Compression,
  PhotometricInterpretation 0–3, StripOffsets, RowsPerStrip,
  StripByteCounts, SamplesPerPixel, PlanarConfiguration, ColorMap (16-bit
  RGB, all reds then greens then blues), ExtraSamples (associated /
  unassociated alpha), FillOrder, Orientation, resolution fields.
- §9: PackBits (its worked example is a unit test).
- §§10–11: CCITT modified Huffman (compression 2, byte-aligned rows) and the
  T4Options / T6Options fields for compressions 3 and 4.
- §13: LZW — MSB-first codes, Clear 256, EOI 257, the table, and the
  writer's early width change (reproduced here as: a decoder widens once its
  next free code reaches 511, 1023, 2047; the encoder one step ahead).
- §14: Predictor 2, horizontal differencing.
- §15: tiles (TileWidth/Length multiples of 16, TileOffsets/ByteCounts,
  planes after one another).
- §16: CMYK (Separated, InkSet).
- §§20–21: SampleFormat (unsigned, signed, IEEE float), YCbCr —
  YCbCrCoefficients (default 299/587/114), YCbCrSubSampling and the data-unit
  layout of subsampled chunky data, ReferenceBlackWhite (default 0, 255,
  128, 255, 128, 255) and the conversion equations.
- Old-style LZW: the bit order of TIFF 5.0-era writers (LSB first, no early
  change), recognised from the first code — a Clear written LSB first begins
  `00 01`, MSB first `80`. Derived from the code layout, not from any
  implementation.

**Adobe TIFF Technical Notes**: the Deflate compression (code 8; 32946 as
the earlier private code) carrying a zlib stream per strip or tile; Adobe
Photoshop Technical Note 3 for the floating-point predictor (byte planes most
significant first, then bytewise differencing) and 16/24/32-bit floats.

**BigTIFF** (the public BigTIFF description): version 43, offset size 8,
64-bit counts and offsets, 20-byte entries, types LONG8/SLONG8/IFD8.

**ITU-T T.4** (Group 3) §4.1–4.2: the white and black terminating and
make-up codes (Tables 2 and 3), the shared extended make-up codes 1792–2560,
EOL, the one-bit tag after EOL selecting 1D or 2D coding, fill bits; the
two-dimensional Modified READ coding — changing elements a0, a1, a2, b1, b2
and the pass, horizontal and vertical (V0, VR1–3, VL1–3) modes (Table 4).
**ITU-T T.6** (Group 4): two-dimensional coding only, the imaginary white
reference line, no EOLs.

The T.4 code tables were typed from the author's knowledge of them. They are
verified by a unit test (each colour's 104 codes are prefix-free and fill
the code space except the all-zero prefixes reserved for EOL) and by full
fax pages from the libtiff corpora (G3 1D, G3 2D, G4, up to 2453 × 3369)
decoding with every row summing exactly to the page width, plus the BMP
Suite's fax-coded TIFF matching its reference PNG pixel for pixel.

## Test data

See docs/VALIDATION.md for what each corpus is and what it is compared with.
None of it is in the repository: `tools/fetch_corpora.py` downloads it from
its publishers and checks every file against `tools/corpora.sha256`.

| corpus | publisher | licence |
|---|---|---|
| BMP Suite 2.8 | Jason Summers, entropymine.com | GPL-3.0 (the suite) |
| libtiff pics-3.8.0 | libtiff (download.osgeo.org) | various; `depth/` public domain |
| libtiff test/images v4.6.0 | libtiff (gitlab.com/libtiff) | libtiff licence |
| GIF test suite | Robert Ancell (pygif, pinned commit) | CC-BY-SA-4.0 |
