//! Reading a GIF: the block structure of GIF89a §§17–26 and the compositing
//! a viewer does with it.

use crate::lzw::{self, LzwError};
use crate::{Error, Limits, Result};

/// What a frame's area becomes once it has been shown (GIF89a §23,
/// "Disposal Method").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Disposal {
    /// No disposal specified (0, and the undefined values 4–7): left as is.
    #[default]
    Unspecified,
    /// Do not dispose (1): left in place.
    Keep,
    /// Restore to background (2): the area is cleared. This decoder clears
    /// it to transparent, as browsers do, rather than to the background
    /// colour; see `docs/VALIDATION.md`.
    Background,
    /// Restore to previous (3): the area returns to what was there before
    /// the frame was drawn.
    Previous,
}

impl Disposal {
    fn from_bits(v: u8) -> Self {
        match v {
            1 => Disposal::Keep,
            2 => Disposal::Background,
            3 => Disposal::Previous,
            _ => Disposal::Unspecified,
        }
    }

    pub(crate) fn bits(self) -> u8 {
        match self {
            Disposal::Unspecified => 0,
            Disposal::Keep => 1,
            Disposal::Background => 2,
            Disposal::Previous => 3,
        }
    }
}

/// The header of a GIF: what is known before any image is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// `b"87a"` or `b"89a"`.
    pub version: [u8; 3],
    /// Logical screen width in pixels.
    pub width: u16,
    /// Logical screen height in pixels.
    pub height: u16,
    /// The global colour table, if there is one.
    pub global_palette: Option<Vec<[u8; 3]>>,
    /// The background colour's index in the global table.
    pub background_index: u8,
    /// The pixel aspect ratio byte: 0 for none given, else the ratio is
    /// `(value + 15) / 64`.
    pub aspect_ratio: u8,
}

impl Info {
    /// The background colour, when there is a global table and the index is
    /// inside it.
    pub fn background(&self) -> Option<[u8; 3]> {
        self.global_palette.as_ref()?.get(usize::from(self.background_index)).copied()
    }
}

/// One displayed frame: the whole logical screen, composited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// `width × height × 4` bytes of RGBA, rows top to bottom. Pixels no
    /// image has drawn are `[0, 0, 0, 0]`.
    pub rgba: Vec<u8>,
    /// How long the frame is shown, in hundredths of a second, as written.
    /// 0 means "not given"; viewers commonly substitute a minimum.
    pub delay_cs: u16,
    /// What happens to this frame's area before the next is drawn.
    pub disposal: Disposal,
    /// The image's own rectangle on the screen: left, top, width, height.
    pub rect: (u16, u16, u16, u16),
    /// Whether the image was stored interlaced.
    pub interlaced: bool,
    /// Whether the viewer was asked to wait for user input (rarely honoured).
    pub user_input: bool,
    /// Whether the image's data ran out before all of its pixels were
    /// given (the missing pixels are left as they were).
    pub incomplete: bool,
}

/// A whole GIF, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    /// The header.
    pub info: Info,
    /// Every image, composited, in order.
    pub frames: Vec<Frame>,
    /// The NETSCAPE2.0 (or ANIMEXTS1.0) loop count as stored: `None` without
    /// the extension (play once), `Some(0)` for forever, otherwise the value
    /// written. Whether `n` means *n* plays or *n* repeats after the first
    /// is not settled by any specification; see `docs/VALIDATION.md`.
    pub loop_count: Option<u16>,
    /// Comment extensions, in order, as raw bytes (the specification asks
    /// for 7-bit ASCII; files carry anything).
    pub comments: Vec<Vec<u8>>,
    /// An ICC profile from an `ICCRGBG1012` application extension.
    pub icc_profile: Option<Vec<u8>>,
}

/// Read the header only: dimensions, version, global table.
pub fn read_info(data: &[u8]) -> Result<Info> {
    Ok(Decoder::new(data)?.info)
}

/// Decode every frame with the default [`Limits`].
pub fn decode(data: &[u8]) -> Result<Animation> {
    decode_with_limits(data, Limits::default())
}

/// Decode every frame within `limits`.
pub fn decode_with_limits(data: &[u8], limits: Limits) -> Result<Animation> {
    let mut d = Decoder::with_limits(data, limits)?;
    let mut frames = Vec::new();
    let frame_bytes = u64::from(d.info.width) * u64::from(d.info.height) * 4;
    let mut total = 0u64;
    while let Some(f) = d.next_frame()? {
        total = total.saturating_add(frame_bytes);
        if total > limits.max_total_bytes {
            return Err(Error::LimitExceeded(format!(
                "the frames need more than {} bytes",
                limits.max_total_bytes
            )));
        }
        frames.push(f);
    }
    Ok(Animation {
        info: d.info.clone(),
        frames,
        loop_count: d.loop_count,
        comments: std::mem::take(&mut d.comments),
        icc_profile: d.icc_profile.take(),
    })
}

/// A little-endian cursor that reports running out as [`Error::Truncated`].
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or(Error::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes([self.u8()?, self.u8()?]))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn palette(&mut self, entries: usize) -> Result<Vec<[u8; 3]>> {
        Ok(self.bytes(entries * 3)?.as_chunks::<3>().0.to_vec())
    }

    /// A run of data sub-blocks up to the terminator, appended to `into` when
    /// given. A sequence the data cuts off is returned as far as it went,
    /// with `false`.
    fn sub_blocks(&mut self, mut into: Option<&mut Vec<u8>>) -> bool {
        loop {
            let Ok(n) = self.u8() else { return false };
            if n == 0 {
                return true;
            }
            let n = usize::from(n);
            let avail = self.data.len() - self.pos;
            let take = n.min(avail);
            if let Some(v) = into.as_deref_mut() {
                v.extend_from_slice(&self.data[self.pos..self.pos + take]);
            }
            self.pos += take;
            if take < n {
                return false;
            }
        }
    }
}

/// The Graphic Control Extension in force for the next image.
#[derive(Debug, Clone, Copy, Default)]
struct Control {
    disposal: Disposal,
    user_input: bool,
    delay_cs: u16,
    transparent: Option<u8>,
}

/// Decodes a GIF one composited frame at a time.
///
/// ```
/// # let bytes = gif::encode(1, 1, &[1, 2, 3, 255], &Default::default()).unwrap();
/// let mut d = gif::Decoder::new(&bytes)?;
/// let (w, h) = (d.info().width, d.info().height);
/// while let Some(frame) = d.next_frame()? {
///     assert_eq!(frame.rgba.len(), usize::from(w) * usize::from(h) * 4);
/// }
/// # Ok::<(), gif::Error>(())
/// ```
pub struct Decoder<'a> {
    cur: Cursor<'a>,
    info: Info,
    limits: Limits,
    canvas: Vec<u8>,
    /// The canvas as it was before the last frame drew, kept when that frame
    /// asked to be disposed to previous.
    saved: Option<Vec<u8>>,
    /// The last frame's disposal and rectangle (clipped to the screen),
    /// applied before the next frame draws.
    pending: Option<(Disposal, (usize, usize, usize, usize))>,
    loop_count: Option<u16>,
    comments: Vec<Vec<u8>>,
    icc_profile: Option<Vec<u8>>,
    frames: usize,
    done: bool,
}

impl<'a> Decoder<'a> {
    /// Start decoding with the default [`Limits`]. Reads the header.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Self::with_limits(data, Limits::default())
    }

    /// Start decoding within `limits`. Reads the header.
    pub fn with_limits(data: &'a [u8], limits: Limits) -> Result<Self> {
        let mut cur = Cursor { data, pos: 0 };
        let sig = cur.bytes(6).map_err(|_| Error::Invalid("not a GIF (too short)".into()))?;
        if &sig[..3] != b"GIF" {
            return Err(Error::Invalid("not a GIF (no signature)".into()));
        }
        let version = [sig[3], sig[4], sig[5]];
        if &version != b"87a" && &version != b"89a" {
            return Err(Error::Unsupported(format!("version {:?}", String::from_utf8_lossy(&version))));
        }
        let width = cur.u16()?;
        let height = cur.u16()?;
        let flags = cur.u8()?;
        let background_index = cur.u8()?;
        let aspect_ratio = cur.u8()?;
        if width == 0 || height == 0 {
            return Err(Error::Invalid(format!("the logical screen is {width}x{height}")));
        }
        check_pixels(u64::from(width) * u64::from(height), &limits)?;
        let global_palette = if flags & 0x80 != 0 { Some(cur.palette(2 << (flags & 7))?) } else { None };
        let info = Info { version, width, height, global_palette, background_index, aspect_ratio };
        Ok(Self {
            cur,
            info,
            limits,
            canvas: Vec::new(),
            saved: None,
            pending: None,
            loop_count: None,
            comments: Vec::new(),
            icc_profile: None,
            frames: 0,
            done: false,
        })
    }

    /// The header.
    pub fn info(&self) -> &Info {
        &self.info
    }

    /// The loop count, as far as the file has been read (it is normally
    /// before the first image). See [`Animation::loop_count`].
    pub fn loop_count(&self) -> Option<u16> {
        self.loop_count
    }

    /// Comment extensions read so far.
    pub fn comments(&self) -> &[Vec<u8>] {
        &self.comments
    }

    /// An ICC profile, once its extension has been read.
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.icc_profile.as_deref()
    }

    /// The next composited frame, or `None` after the last.
    ///
    /// A file that ends without its trailer, or with bytes that are not a
    /// block, ends the animation after the frames already read (as viewers
    /// do); before any frame it is an error.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        if self.done {
            return Ok(None);
        }
        let mut control = Control::default();
        loop {
            let introducer = match self.cur.u8() {
                Ok(b) => b,
                Err(e) => return self.end(e),
            };
            match introducer {
                0x3B => {
                    self.done = true;
                    return Ok(None);
                }
                0x21 => {
                    if let Err(e) = self.extension(&mut control) {
                        return self.end(e);
                    }
                }
                0x2C => {
                    let frame = self.image(control);
                    return match frame {
                        Ok(f) => {
                            self.frames += 1;
                            Ok(Some(f))
                        }
                        Err(Error::Truncated) if self.frames > 0 => {
                            self.done = true;
                            Ok(None)
                        }
                        Err(e) => {
                            self.done = true;
                            Err(e)
                        }
                    };
                }
                other => {
                    return self.end(Error::Invalid(format!("unknown block 0x{other:02X}")));
                }
            }
        }
    }

    fn end(&mut self, e: Error) -> Result<Option<Frame>> {
        self.done = true;
        if self.frames > 0 { Ok(None) } else { Err(e) }
    }

    fn extension(&mut self, control: &mut Control) -> Result<()> {
        let label = self.cur.u8()?;
        match label {
            0xF9 => {
                // The block size is 4; a writer that says otherwise is read
                // as far as it goes.
                let size = usize::from(self.cur.u8()?);
                let body = self.cur.bytes(size)?;
                if body.len() >= 4 {
                    let flags = body[0];
                    *control = Control {
                        disposal: Disposal::from_bits((flags >> 2) & 7),
                        user_input: flags & 2 != 0,
                        delay_cs: u16::from_le_bytes([body[1], body[2]]),
                        transparent: (flags & 1 != 0).then_some(body[3]),
                    };
                }
                self.cur.sub_blocks(None);
            }
            0xFF => {
                let size = usize::from(self.cur.u8()?);
                let ident = self.cur.bytes(size)?;
                let mut body = Vec::new();
                self.cur.sub_blocks(Some(&mut body));
                if ident == b"NETSCAPE2.0" || ident == b"ANIMEXTS1.0" {
                    // Sub-block 1 is the loop count; others (2: buffer size)
                    // are not used here. The body is the concatenation, so
                    // look at the first sub-block's bytes.
                    if body.len() >= 3 && body[0] == 1 {
                        self.loop_count = Some(u16::from_le_bytes([body[1], body[2]]));
                    }
                } else if ident == b"ICCRGBG1012" {
                    self.icc_profile = Some(body);
                }
            }
            0xFE => {
                let mut body = Vec::new();
                self.cur.sub_blocks(Some(&mut body));
                self.comments.push(body);
            }
            0x01 => {
                // Plain Text: a graphic rendering block this decoder does not
                // draw. It consumes the Graphic Control Extension before it.
                self.cur.sub_blocks(None);
                *control = Control::default();
            }
            _ => {
                self.cur.sub_blocks(None);
            }
        }
        Ok(())
    }

    fn image(&mut self, control: Control) -> Result<Frame> {
        if self.frames >= self.limits.max_frames {
            return Err(Error::LimitExceeded(format!("more than {} frames", self.limits.max_frames)));
        }
        let left = self.cur.u16()?;
        let top = self.cur.u16()?;
        let w = self.cur.u16()?;
        let h = self.cur.u16()?;
        let flags = self.cur.u8()?;
        let interlaced = flags & 0x40 != 0;
        // From here on, data that runs out still gives a frame: what was
        // drawn before it ran out (a viewer shows a partial download too).
        let mut cut = false;
        let mut table_cut = false;
        let local = if flags & 0x80 != 0 {
            match self.cur.palette(2 << (flags & 7)) {
                Ok(p) => Some(p),
                Err(_) => {
                    (cut, table_cut) = (true, true);
                    None
                }
            }
        } else {
            None
        };
        let min_size = if cut { None } else { self.cur.u8().ok() };
        cut |= min_size.is_none();
        let mut data = Vec::new();
        if !cut {
            cut = !self.cur.sub_blocks(Some(&mut data));
        }
        if cut {
            self.done = true;
        }

        let (sw, sh) = (usize::from(self.info.width), usize::from(self.info.height));
        if self.canvas.is_empty() {
            self.canvas = vec![0; sw * sh * 4];
        }
        // The previous frame's disposal happens before this one draws.
        if let Some((disposal, (x0, y0, x1, y1))) = self.pending.take() {
            match disposal {
                Disposal::Background => {
                    for y in y0..y1 {
                        self.canvas[(y * sw + x0) * 4..(y * sw + x1) * 4].fill(0);
                    }
                }
                Disposal::Previous => {
                    if let Some(saved) = self.saved.take() {
                        self.canvas = saved;
                    }
                }
                Disposal::Unspecified | Disposal::Keep => {}
            }
        }
        if control.disposal == Disposal::Previous {
            self.saved = Some(self.canvas.clone());
        }

        let (iw, ih) = (usize::from(w), usize::from(h));
        check_pixels(u64::from(w) * u64::from(h), &self.limits)?;
        let mut incomplete = cut;
        if let Some(min_size) = min_size
            && iw * ih > 0
            && !table_cut
        {
            let palette = local
                .as_deref()
                .or(self.info.global_palette.as_deref())
                .ok_or_else(|| Error::Invalid("an image with no colour table".into()))?;
            let mut indices = vec![0u8; iw * ih];
            let decoded = match lzw::decode(min_size, &data, &mut indices) {
                Ok(d) => d,
                Err(LzwError::CodeSize(s)) => return Err(Error::Invalid(format!("LZW minimum code size {s}"))),
                Err(LzwError::InvalidCode) => return Err(Error::Invalid("an LZW code outside the table".into())),
                Err(LzwError::IndexRange) => return Err(Error::Invalid("a pixel index above 255".into())),
            };
            let written = decoded.written;
            incomplete |= written < indices.len();
            let (x0, y0) = (usize::from(left), usize::from(top));
            for stored_row in 0..ih {
                let row_start = stored_row * iw;
                if row_start >= written {
                    break;
                }
                let y = y0 + if interlaced { interlaced_row(stored_row, ih) } else { stored_row };
                if y >= sh {
                    continue;
                }
                let row_end = (row_start + iw).min(written);
                for (i, &index) in indices[row_start..row_end].iter().enumerate() {
                    let x = x0 + i;
                    if x >= sw {
                        break;
                    }
                    if control.transparent == Some(index) {
                        continue;
                    }
                    let [r, g, b] = *palette
                        .get(usize::from(index))
                        .ok_or_else(|| Error::Invalid(format!("pixel index {index} outside a {}-colour table", palette.len())))?;
                    let at = (y * sw + x) * 4;
                    self.canvas[at..at + 4].copy_from_slice(&[r, g, b, 255]);
                }
            }
        }

        let clip = |a: u16, len: u16, max: usize| (usize::from(a).min(max), (usize::from(a) + usize::from(len)).min(max));
        let (x0, x1) = clip(left, w, sw);
        let (y0, y1) = clip(top, h, sh);
        self.pending = Some((control.disposal, (x0, y0, x1, y1)));
        Ok(Frame {
            rgba: self.canvas.clone(),
            delay_cs: control.delay_cs,
            disposal: control.disposal,
            rect: (left, top, w, h),
            interlaced,
            user_input: control.user_input,
            incomplete,
        })
    }
}

fn check_pixels(pixels: u64, limits: &Limits) -> Result<()> {
    if pixels > limits.max_pixels {
        return Err(Error::LimitExceeded(format!("{pixels} pixels, over the limit of {}", limits.max_pixels)));
    }
    Ok(())
}

/// The screen row of the `n`th stored row of an interlaced image `h` rows
/// tall (GIF89a Appendix E: every 8th row from 0, every 8th from 4, every
/// 4th from 2, every 2nd from 1).
fn interlaced_row(n: usize, h: usize) -> usize {
    let pass1 = h.div_ceil(8);
    let pass2 = (h + 3) / 8;
    let pass3 = (h + 1) / 4;
    if n < pass1 {
        n * 8
    } else if n < pass1 + pass2 {
        (n - pass1) * 8 + 4
    } else if n < pass1 + pass2 + pass3 {
        (n - pass1 - pass2) * 4 + 2
    } else {
        (n - pass1 - pass2 - pass3) * 2 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::interlaced_row;

    #[test]
    fn interlace_visits_every_row_once() {
        for h in 1..40 {
            let mut seen = vec![false; h];
            for n in 0..h {
                let r = interlaced_row(n, h);
                assert!(r < h && !seen[r], "h={h} n={n} r={r}");
                seen[r] = true;
            }
        }
    }

    #[test]
    fn interlace_order_of_the_specification() {
        // Appendix E's example: rows 0, 8, 16 … first.
        let order: Vec<usize> = (0..10).map(|n| interlaced_row(n, 10)).collect();
        assert_eq!(order, [0, 8, 4, 2, 6, 1, 3, 5, 7, 9]);
    }
}
