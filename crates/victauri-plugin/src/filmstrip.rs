//! Compose captured frames into a single contact-sheet PNG ("filmstrip").
//!
//! The animation `scrub` tool seeks an animation to N evenly-spaced points and
//! captures one frame at each. Returning N separate images is expensive for an
//! agent to read; a single grid image shows the whole motion arc in one look.
//! The caller pairs the image with a `manifest` that maps each cell index to its
//! animation progress/time, so the (unlabelled) grid stays cheap to produce
//! while remaining fully interpretable.

/// A single captured frame: straight (non-premultiplied) RGBA bytes + size.
#[derive(Debug, Clone)]
pub struct Frame {
    /// RGBA pixel data, 4 bytes per pixel, `w * h * 4` long.
    pub rgba: Vec<u8>,
    /// Frame width in pixels.
    pub w: u32,
    /// Frame height in pixels.
    pub h: u32,
}

impl Frame {
    /// Construct a frame, validating that the buffer matches the dimensions.
    #[must_use]
    pub fn new(rgba: Vec<u8>, w: u32, h: u32) -> Option<Self> {
        let expected = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        if rgba.len() == expected && w > 0 && h > 0 {
            Some(Self { rgba, w, h })
        } else {
            None
        }
    }
}

/// Compose `frames` into a grid `cols` wide (rows derived), separated and
/// bordered by `gap` pixels of `bg`. Cells are sized to the largest frame;
/// smaller frames are placed top-left within their cell. Returns
/// `(rgba, width, height)` for the composed sheet, or `None` if `frames` is
/// empty or the resulting buffer would overflow `usize`.
#[must_use]
pub fn compose(
    frames: &[Frame],
    cols: usize,
    gap: u32,
    bg: [u8; 4],
) -> Option<(Vec<u8>, u32, u32)> {
    if frames.is_empty() {
        return None;
    }
    let n = frames.len();
    let cols = cols.max(1).min(n);
    let rows = n.div_ceil(cols);
    let gap = gap as usize;

    let cell_w = frames.iter().map(|f| f.w as usize).max()?;
    let cell_h = frames.iter().map(|f| f.h as usize).max()?;

    // out_w = cols*cell_w + (cols+1)*gap ; out_h analogous. All checked.
    let out_w = cols
        .checked_mul(cell_w)?
        .checked_add(cols.checked_add(1)?.checked_mul(gap)?)?;
    let out_h = rows
        .checked_mul(cell_h)?
        .checked_add(rows.checked_add(1)?.checked_mul(gap)?)?;
    let total = out_w.checked_mul(out_h)?.checked_mul(4)?;
    // Guard against absurd allocations (e.g. > ~512 MB sheet).
    if total > 512 * 1024 * 1024 {
        return None;
    }

    // Fill background.
    let mut out = vec![0u8; total];
    for px in out.as_chunks_mut::<4>().0.iter_mut() {
        px.copy_from_slice(&bg);
    }

    let out_row_bytes = out_w * 4;
    for (i, frame) in frames.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let x0 = gap + col * (cell_w + gap);
        let y0 = gap + row * (cell_h + gap);
        let fw = frame.w as usize;
        let fh = frame.h as usize;
        let frame_row_bytes = fw * 4;
        for y in 0..fh {
            let dst_start = (y0 + y) * out_row_bytes + x0 * 4;
            let src_start = y * frame_row_bytes;
            // Bounds are guaranteed by construction (fw<=cell_w, fh<=cell_h),
            // but slice with care to avoid any panic on malformed input.
            let dst_end = dst_start + frame_row_bytes;
            let src_end = src_start + frame_row_bytes;
            if dst_end <= out.len() && src_end <= frame.rgba.len() {
                out[dst_start..dst_end].copy_from_slice(&frame.rgba[src_start..src_end]);
            }
        }
    }

    Some((out, out_w as u32, out_h as u32))
}

/// Default column count for `n` frames: roughly square, capped so wide strips
/// stay readable.
#[must_use]
pub fn default_cols(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation
    )]
    let c = (n as f64).sqrt().ceil() as usize;
    c.clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, color: [u8; 4]) -> Frame {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&color);
        }
        Frame::new(rgba, w, h).unwrap()
    }

    #[test]
    fn frame_new_validates_size() {
        assert!(Frame::new(vec![0; 16], 2, 2).is_some());
        assert!(Frame::new(vec![0; 15], 2, 2).is_none());
        assert!(Frame::new(vec![], 0, 0).is_none());
    }

    #[test]
    fn empty_returns_none() {
        assert!(compose(&[], 4, 2, [0, 0, 0, 0]).is_none());
    }

    #[test]
    fn single_frame_no_gap() {
        let f = solid(3, 2, [10, 20, 30, 255]);
        let (rgba, w, h) = compose(std::slice::from_ref(&f), 4, 0, [0, 0, 0, 0]).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(rgba.len(), 3 * 2 * 4);
        // First pixel should be the frame color (no gap).
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn grid_dims_with_gap() {
        // 3 frames of 4x2, cols=2 -> rows=2, gap=1.
        let frames: Vec<Frame> = (0..3).map(|_| solid(4, 2, [1, 2, 3, 255])).collect();
        let (_, w, h) = compose(&frames, 2, 1, [0, 0, 0, 255]).unwrap();
        // out_w = 2*4 + 3*1 = 11 ; out_h = 2*2 + 3*1 = 7
        assert_eq!((w, h), (11, 7));
    }

    #[test]
    fn ragged_sizes_clamp_to_max_cell() {
        let a = solid(4, 2, [255, 0, 0, 255]);
        let b = solid(2, 4, [0, 255, 0, 255]);
        let (_, w, h) = compose(&[a, b], 2, 0, [0, 0, 0, 0]).unwrap();
        // cell = max(4,2) x max(2,4) = 4x4 ; cols=2 -> out_w=8, out_h=4
        assert_eq!((w, h), (8, 4));
    }

    #[test]
    fn background_fills_gaps() {
        let f = solid(2, 2, [255, 255, 255, 255]);
        let bg = [9, 8, 7, 255];
        let (rgba, w, _h) = compose(std::slice::from_ref(&f), 1, 1, bg).unwrap();
        // Top-left corner is gap → background color.
        assert_eq!(&rgba[0..4], &bg);
        // Frame sits at (gap,gap) = (1,1): offset = (w + 1)*4.
        let off = (w as usize + 1) * 4;
        assert_eq!(&rgba[off..off + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn default_cols_is_roughly_square() {
        assert_eq!(default_cols(0), 1);
        assert_eq!(default_cols(1), 1);
        assert_eq!(default_cols(4), 2);
        assert_eq!(default_cols(9), 3);
        assert_eq!(default_cols(20), 5);
        assert_eq!(default_cols(1000), 8); // capped
    }
}
