use crate::capture::IrFrame;
use crate::detector::FaceBox;
use image::{DynamicImage, ImageBuffer, Luma};
use tract_onnx::prelude::tract_ndarray::Array3;

const ENCODER_SIZE: usize = 112;

/// Wrap a captured frame as a 16-bit greyscale image.
///
/// Shared by the detector and encoder paths so the geometry check lives in
/// one place: `ImageBuffer::from_fn` indexes `width * height` pixels, and a
/// frame shorter than that would otherwise be silently zero-padded into a
/// picture that is part real and part black.
pub fn frame_to_luma16(frame: &IrFrame) -> anyhow::Result<ImageBuffer<Luma<u16>, Vec<u16>>> {
    let width = frame.width;
    let height = frame.height;
    anyhow::ensure!(width > 0 && height > 0, "frame has zero extent");

    let expected = width as usize * height as usize;
    anyhow::ensure!(
        frame.data.len() >= expected,
        "frame holds {} samples, expected {} for {}x{}",
        frame.data.len(),
        expected,
        width,
        height
    );

    Ok(ImageBuffer::from_fn(width, height, |x, y| {
        Luma([frame.data[(y * width + x) as usize]])
    }))
}

/// Crop a frame to the detected face, grown by `margin` (a fraction of the
/// box's own size on each side) and squared up so the encoder's
/// `resize_exact` does not distort the aspect ratio.
///
/// A window that runs off an edge is shifted back inside rather than shrunk,
/// so a face near the border still gets a full-size crop.
pub fn crop_to_face(frame: &IrFrame, face_box: &FaceBox, margin: f32) -> anyhow::Result<IrFrame> {
    // Validates geometry against the buffer before any slicing below.
    frame_to_luma16(frame)?;

    let width = frame.width as f32;
    let height = frame.height as f32;

    let x1 = (face_box.x1 * width).clamp(0.0, width);
    let y1 = (face_box.y1 * height).clamp(0.0, height);
    let x2 = (face_box.x2 * width).clamp(0.0, width);
    let y2 = (face_box.y2 * height).clamp(0.0, height);

    let box_w = (x2 - x1).max(1.0);
    let box_h = (y2 - y1).max(1.0);
    let (mx, my) = (box_w * margin, box_h * margin);

    let cx = (x1 + x2) / 2.0;
    let cy = (y1 + y2) / 2.0;
    let side = (box_w + 2.0 * mx).max(box_h + 2.0 * my).min(width.min(height));

    let mut sx1 = cx - side / 2.0;
    let mut sy1 = cy - side / 2.0;
    let mut sx2 = cx + side / 2.0;
    let mut sy2 = cy + side / 2.0;
    if sx1 < 0.0 {
        sx2 -= sx1;
        sx1 = 0.0;
    }
    if sy1 < 0.0 {
        sy2 -= sy1;
        sy1 = 0.0;
    }
    if sx2 > width {
        sx1 -= sx2 - width;
        sx2 = width;
    }
    if sy2 > height {
        sy1 -= sy2 - height;
        sy2 = height;
    }

    let ix1 = (sx1.clamp(0.0, width).round() as u32).min(frame.width - 1);
    let iy1 = (sy1.clamp(0.0, height).round() as u32).min(frame.height - 1);
    let ix2 = (sx2.clamp(0.0, width).round() as u32).max(ix1 + 1).min(frame.width);
    let iy2 = (sy2.clamp(0.0, height).round() as u32).max(iy1 + 1).min(frame.height);

    let (crop_w, crop_h) = (ix2 - ix1, iy2 - iy1);
    let mut data = Vec::with_capacity((crop_w * crop_h) as usize);
    for y in iy1..iy2 {
        let row = (y * frame.width + ix1) as usize;
        data.extend_from_slice(&frame.data[row..row + crop_w as usize]);
    }

    Ok(IrFrame { data, width: crop_w, height: crop_h })
}

/// Resize and normalise a frame into the encoder's `[3, 112, 112]` input.
///
/// The arithmetic here defines what an enrolled embedding means; changing it
/// invalidates every template already on disk.
pub fn preprocess_ir_frame(frame: &IrFrame) -> anyhow::Result<Array3<f32>> {
    let img_buffer = frame_to_luma16(frame)?;

    let dynamic_img = DynamicImage::ImageLuma16(img_buffer);
    let resized = dynamic_img.resize_exact(
        ENCODER_SIZE as u32,
        ENCODER_SIZE as u32,
        image::imageops::FilterType::Lanczos3,
    );
    let gray_img = resized.to_luma16();

    let mut array = Array3::<f32>::zeros((3, ENCODER_SIZE, ENCODER_SIZE));

    for y in 0..ENCODER_SIZE {
        for x in 0..ENCODER_SIZE {
            let pixel = gray_img.get_pixel(x as u32, y as u32).0[0] as f32 / 65535.0;
            let normalized = (pixel - 0.5) / 0.5;
            for c in 0..3usize {
                array[[c, y, x]] = normalized;
            }
        }
    }

    Ok(array)
}

/// Side of the square face patch that liveness compares, in pixels.
pub const MOTION_PATCH_SIZE: usize = 48;

/// Largest rigid shift, in patch pixels each way, that `motion_profile`
/// compensates for. The patch is re-cropped around each frame's detection, so
/// a moved face or print is already mostly centred; this absorbs the
/// detector's box jitter on top.
pub const MOTION_MAX_SHIFT: i32 = 4;

/// Side of the square blocks `MotionProfile::local` is taken over, in patch
/// pixels. About the size of an eye at `MOTION_PATCH_SIZE`.
pub const MOTION_BLOCK: usize = 8;

/// Per-pixel change, in units of the patch's own standard deviation, below
/// which a difference is sensor noise rather than motion.
const MOTION_NOISE_FLOOR: f32 = 0.25;

/// The face region of one frame, reduced to what liveness compares: a fixed
/// size, zero mean and unit variance.
///
/// Cropping to the detected box keeps motion elsewhere in view (the hand
/// holding a print, someone walking past) out of the signal. Normalising
/// removes gain and offset, so an auto-exposure step or a brighter strobe
/// frame is not mistaken for motion. Take it from the frame before CLAHE:
/// equalisation remaps each frame differently and would add its own change.
#[derive(Clone, Debug)]
pub struct FacePatch {
    data: Vec<f32>,
}

/// How two consecutive face patches differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionProfile {
    /// Fraction of pixels that changed, as the patches stand.
    pub total: f32,
    /// Fraction still changed after undoing the best rigid shift. A photo
    /// moved by hand moves as one piece and leaves little here; a real face
    /// moves in parts (blinks, mouth, parallax as the head turns).
    pub residual: f32,
    /// The largest `residual` of any one `MOTION_BLOCK` square block. A blink
    /// is a big change in a small area, which `residual`, averaged over the
    /// whole face, dilutes to almost nothing.
    pub local: f32,
    /// The rigid shift, in patch pixels, that best lines `b` up with `a`.
    pub shift: (f32, f32),
}

pub fn face_patch(frame: &IrFrame, face_box: &FaceBox) -> anyhow::Result<FacePatch> {
    let face = crop_to_face(frame, face_box, 0.0)?;
    let img = DynamicImage::ImageLuma16(frame_to_luma16(&face)?);
    let small = img
        .resize_exact(
            MOTION_PATCH_SIZE as u32,
            MOTION_PATCH_SIZE as u32,
            image::imageops::FilterType::Triangle,
        )
        .to_luma16();

    let mut data: Vec<f32> = small.pixels().map(|p| p.0[0] as f32).collect();
    let len = data.len() as f32;
    let mean = data.iter().sum::<f32>() / len;
    let var = data.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / len;
    // A flat patch has no structure to compare; leave it at zero rather than
    // dividing noise up into something that looks like motion.
    let scale = if var > 1.0 { 1.0 / var.sqrt() } else { 0.0 };
    for v in &mut data {
        *v = (*v - mean) * scale;
    }
    Ok(FacePatch { data })
}

/// Compare two face patches: how much changed, and how much of that a single
/// rigid shift can't explain.
///
/// The shift is found to a quarter of a patch pixel: whole pixels first, then
/// refined with bilinear sampling. A photo rarely moves by whole pixels, and
/// a half-pixel misalignment leaves residue along every sharp edge (eyes,
/// glasses), right where a blink would show.
///
/// Every count is taken over the same inner window (the patch less
/// `MOTION_MAX_SHIFT` on each side) so every candidate shift is scored on the
/// same pixels.
pub fn motion_profile(a: &FacePatch, b: &FacePatch) -> MotionProfile {
    let n = MOTION_PATCH_SIZE as i32;
    let s = MOTION_MAX_SHIFT;
    let expected = MOTION_PATCH_SIZE * MOTION_PATCH_SIZE;
    if a.data.len() != expected || b.data.len() != expected {
        return MotionProfile { total: 0.0, residual: 0.0, local: 0.0, shift: (0.0, 0.0) };
    }

    // `b` at (x, y), bilinearly. Callers keep x and y within [0, n - 1].
    let sample = |x: f32, y: f32| {
        let (x0, y0) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
        let px = |xx: i32, yy: i32| b.data[(yy * n + xx) as usize];
        (px(x0, y0) * (1.0 - fx) + px(x1, y0) * fx) * (1.0 - fy) + (px(x0, y1) * (1.0 - fx) + px(x1, y1) * fx) * fy
    };
    let span = (n - 2 * s) as usize;
    let inner = (span * span) as f32;
    // Sum of absolute differences at a shift, for choosing it.
    let cost = |dx: f32, dy: f32| {
        let mut sum = 0.0f32;
        for y in s..n - s {
            for x in s..n - s {
                sum += (a.data[(y * n + x) as usize] - sample(x as f32 + dx, y as f32 + dy)).abs();
            }
        }
        sum
    };
    // Changed pixels at a shift, overall and per block.
    let blocks = span / MOTION_BLOCK;
    let changed = |dx: f32, dy: f32| {
        let mut all = 0usize;
        let mut per_block = vec![0usize; blocks * blocks];
        for y in 0..span {
            for x in 0..span {
                let (px, py) = (x as i32 + s, y as i32 + s);
                let d = (a.data[(py * n + px) as usize] - sample(px as f32 + dx, py as f32 + dy)).abs();
                if d > MOTION_NOISE_FLOOR {
                    all += 1;
                    if x / MOTION_BLOCK < blocks && y / MOTION_BLOCK < blocks {
                        per_block[(y / MOTION_BLOCK) * blocks + x / MOTION_BLOCK] += 1;
                    }
                }
            }
        }
        let worst = per_block.iter().copied().max().unwrap_or(0);
        (all as f32 / inner, worst as f32 / (MOTION_BLOCK * MOTION_BLOCK) as f32)
    };

    let mut best = (f32::INFINITY, (0.0f32, 0.0f32));
    for dy in -s..=s {
        for dx in -s..=s {
            let c = cost(dx as f32, dy as f32);
            if c < best.0 {
                best = (c, (dx as f32, dy as f32));
            }
        }
    }
    let (ix, iy) = best.1;
    let limit = s as f32;
    for qy in -3..=3 {
        for qx in -3..=3 {
            let (dx, dy) = (ix + qx as f32 * 0.25, iy + qy as f32 * 0.25);
            if dx.abs() > limit || dy.abs() > limit {
                continue;
            }
            let c = cost(dx, dy);
            if c < best.0 {
                best = (c, (dx, dy));
            }
        }
    }

    let (total, _) = changed(0.0, 0.0);
    let (residual, local) = changed(best.1 .0, best.1 .1);
    MotionProfile { total, residual, local, shift: best.1 }
}

/// Default CLAHE parameters. `CLIP_LIMIT` follows OpenCV's convention: the
/// per-bin ceiling is `clip * tile_pixels / 256`, with the clipped mass
/// redistributed. Howdy uses 2.0; 3.0 measured better on this sensor's frames
/// without visibly amplifying noise.
pub const CLAHE_CLIP_LIMIT: f32 = 3.0;
pub const CLAHE_TILES: u32 = 8;

/// Contrast-limited adaptive histogram equalisation.
///
/// Replaces the global equalisation this used to do. Global equalisation maps
/// one CDF over the whole frame, so a dark IR frame — where nearly all samples
/// sit in a narrow band — gets that band stretched across the full range,
/// turning sensor noise into hard posterised contours. Measured on this
/// camera, the face detector scored ~0.11 on globally-equalised frames (below
/// its 0.5 threshold, indistinguishable from an empty room) and 0.60-0.99 on
/// the same frames under CLAHE.
///
/// CLAHE instead equalises per tile with a ceiling on how much any one
/// intensity may be amplified, then bilinearly interpolates between
/// neighbouring tiles' mappings so no tile seams appear.
///
/// Samples are u16 carrying 8-bit data widened by 257 (see `capture_frame`),
/// so 256 bins are exact here.
pub fn clahe_equalize(frame: &mut IrFrame, clip_limit: f32, tiles: u32) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if frame.data.is_empty() || w == 0 || h == 0 || frame.data.len() < w * h {
        return;
    }
    let tiles = tiles.max(1) as usize;
    let tile_w = w.div_ceil(tiles);
    let tile_h = h.div_ceil(tiles);

    // One 256-entry lookup table per tile.
    let mut luts = vec![[0u8; 256]; tiles * tiles];
    for ty in 0..tiles {
        for tx in 0..tiles {
            let x0 = tx * tile_w;
            let y0 = ty * tile_h;
            let x1 = (x0 + tile_w).min(w);
            let y1 = (y0 + tile_h).min(h);
            if x0 >= x1 || y0 >= y1 {
                continue;
            }

            let mut hist = [0u32; 256];
            for y in y0..y1 {
                for x in x0..x1 {
                    hist[(frame.data[y * w + x] >> 8) as usize] += 1;
                }
            }

            // Clip, then hand the excess back evenly. This is what bounds the
            // contrast gain and stops noise being amplified without limit.
            let count = ((x1 - x0) * (y1 - y0)) as f32;
            let limit = ((clip_limit * count) / 256.0).max(1.0) as u32;
            let mut excess: u32 = 0;
            for b in hist.iter_mut() {
                if *b > limit {
                    excess += *b - limit;
                    *b = limit;
                }
            }
            let share = excess / 256;
            let mut remainder = excess % 256;
            for b in hist.iter_mut() {
                *b += share;
                if remainder > 0 {
                    *b += 1;
                    remainder -= 1;
                }
            }

            let lut = &mut luts[ty * tiles + tx];
            let total = count.max(1.0);
            let mut cumulative = 0u32;
            for (i, &b) in hist.iter().enumerate() {
                cumulative += b;
                lut[i] = ((cumulative as f32 / total) * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
    }

    // Bilinear blend between the four nearest tile centres.
    for y in 0..h {
        let gy = ((y as f32 - tile_h as f32 * 0.5) / tile_h as f32).max(0.0);
        let ty0 = (gy as usize).min(tiles - 1);
        let ty1 = (ty0 + 1).min(tiles - 1);
        let fy = gy - ty0 as f32;

        for x in 0..w {
            let gx = ((x as f32 - tile_w as f32 * 0.5) / tile_w as f32).max(0.0);
            let tx0 = (gx as usize).min(tiles - 1);
            let tx1 = (tx0 + 1).min(tiles - 1);
            let fx = gx - tx0 as f32;

            let v = (frame.data[y * w + x] >> 8) as usize;
            let tl = luts[ty0 * tiles + tx0][v] as f32;
            let tr = luts[ty0 * tiles + tx1][v] as f32;
            let bl = luts[ty1 * tiles + tx0][v] as f32;
            let br = luts[ty1 * tiles + tx1][v] as f32;

            let top = tl + (tr - tl) * fx;
            let bottom = bl + (br - bl) * fx;
            let out = (top + (bottom - top) * fy).clamp(0.0, 255.0) as u16;
            frame.data[y * w + x] = out * 257;
        }
    }
}

/// Equalise a frame for detection and encoding, using the project defaults.
pub fn histogram_equalize(frame: &mut IrFrame) {
    clahe_equalize(frame, CLAHE_CLIP_LIMIT, CLAHE_TILES);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(data: Vec<u16>, width: u32, height: u32) -> IrFrame {
        IrFrame { data, width, height }
    }

    #[test]
    fn rejects_frame_shorter_than_its_geometry() {
        // Previously padded with zeros, producing a half-black image that the
        // encoder would happily turn into an embedding.
        let f = frame(vec![0; 10], 32, 32);
        assert!(frame_to_luma16(&f).is_err());
        assert!(preprocess_ir_frame(&f).is_err());
    }

    #[test]
    fn rejects_zero_extent_frame() {
        assert!(frame_to_luma16(&frame(vec![], 0, 0)).is_err());
    }

    #[test]
    fn accepts_exactly_sized_frame() {
        let f = frame(vec![1234; 32 * 32], 32, 32);
        let img = frame_to_luma16(&f).unwrap();
        assert_eq!(img.dimensions(), (32, 32));
    }

    #[test]
    fn accepts_frame_with_trailing_padding() {
        // Some drivers report bytesused beyond the visible image.
        let f = frame(vec![7; 32 * 32 + 64], 32, 32);
        assert!(frame_to_luma16(&f).is_ok());
    }

    #[test]
    fn preprocess_produces_encoder_shaped_input() {
        let f = frame((0..64 * 64).map(|i| (i % 65536) as u16).collect(), 64, 64);
        let arr = preprocess_ir_frame(&f).unwrap();
        assert_eq!(arr.shape(), &[3, ENCODER_SIZE, ENCODER_SIZE]);
        assert!(arr.iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)));
    }

    #[test]
    fn clahe_expands_a_low_contrast_frame() {
        // A dark, narrow-range frame — what an unlit-ish IR capture looks like.
        let data: Vec<u16> = (0..64 * 64).map(|i| ((i % 20) as u16 + 20) * 257).collect();
        let mut f = frame(data, 64, 64);
        let before = spread(&f);
        histogram_equalize(&mut f);
        assert!(spread(&f) > before, "CLAHE should widen the tonal range");
        assert!(f.data.iter().all(|&v| v % 257 == 0), "output stays 8-bit widened");
    }

    #[test]
    fn clahe_leaves_a_flat_frame_flat() {
        // Uniform input has no contrast to recover; it must not explode into
        // noise, which is precisely what the clip limit is for.
        let mut f = frame(vec![128 * 257; 64 * 64], 64, 64);
        histogram_equalize(&mut f);
        let first = f.data[0];
        assert!(f.data.iter().all(|&v| v == first));
    }

    #[test]
    fn clahe_handles_degenerate_frames() {
        let mut empty = frame(vec![], 0, 0);
        histogram_equalize(&mut empty);
        assert!(empty.data.is_empty());

        // Short buffer must be left alone rather than indexed out of bounds.
        let mut short = frame(vec![100; 10], 32, 32);
        histogram_equalize(&mut short);
        assert_eq!(short.data.len(), 10);
    }

    #[test]
    fn crop_is_square_and_inside_the_frame() {
        let f = frame((0..640 * 400).map(|i| (i % 65536) as u16).collect(), 640, 400);
        let b = FaceBox { x1: 0.4, y1: 0.3, x2: 0.6, y2: 0.7 };
        let c = crop_to_face(&f, &b, 0.3).unwrap();
        assert_eq!(c.width, c.height);
        assert_eq!(c.data.len(), (c.width * c.height) as usize);
        assert!(c.width <= 400);
    }

    #[test]
    fn crop_near_an_edge_shifts_instead_of_shrinking() {
        let f = frame(vec![1; 640 * 400], 640, 400);
        let centred = crop_to_face(&f, &FaceBox { x1: 0.4, y1: 0.4, x2: 0.5, y2: 0.6 }, 0.3).unwrap();
        let edge = crop_to_face(&f, &FaceBox { x1: -0.05, y1: 0.4, x2: 0.05, y2: 0.6 }, 0.3).unwrap();
        assert_eq!(edge.width, centred.width);
    }

    #[test]
    fn crop_rejects_a_short_frame() {
        let f = frame(vec![0; 10], 32, 32);
        let b = FaceBox { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 };
        assert!(crop_to_face(&f, &b, 0.3).is_err());
    }

    /// A 200x200 frame of smooth, non-repeating texture, offset by `(ox, oy)`
    /// pixels, with an optional bright square standing in for a local change
    /// such as a blink.
    fn scene(ox: i32, oy: i32, gain: f32, offset: f32, blob: Option<(u32, u32)>) -> IrFrame {
        let (w, h) = (200u32, 200u32);
        let data = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as i32 + ox, (i / w) as i32 + oy);
                let (fx, fy) = (x as f32, y as f32);
                let mut v = 20000.0 + 8000.0 * (fx * 0.11).sin() * (fy * 0.07).cos() + 6000.0 * ((fx + fy) * 0.05).sin();
                if let Some((bx, by)) = blob {
                    let (px, py) = ((i % w), (i / w));
                    if (bx..bx + 30).contains(&px) && (by..by + 20).contains(&py) {
                        v = 60000.0;
                    }
                }
                (v * gain + offset).clamp(0.0, 65535.0) as u16
            })
            .collect();
        frame(data, w, h)
    }

    const MIDDLE: FaceBox = FaceBox { x1: 0.25, y1: 0.25, x2: 0.75, y2: 0.75 };

    fn profile(a: &IrFrame, b: &IrFrame, box_b: &FaceBox) -> MotionProfile {
        motion_profile(&face_patch(a, &MIDDLE).unwrap(), &face_patch(b, box_b).unwrap())
    }

    #[test]
    fn identical_faces_have_zero_motion() {
        let a = scene(0, 0, 1.0, 0.0, None);
        let p = profile(&a, &a.clone(), &MIDDLE);
        assert_eq!(p, MotionProfile { total: 0.0, residual: 0.0, local: 0.0, shift: (0.0, 0.0) });
    }

    #[test]
    fn brightness_change_is_not_motion() {
        // An auto-exposure step: same picture, different gain and black level.
        let a = scene(0, 0, 1.0, 0.0, None);
        let b = scene(0, 0, 1.3, 2000.0, None);
        let p = profile(&a, &b, &MIDDLE);
        assert!(p.total < 0.01, "{p:?}");
    }

    #[test]
    fn motion_outside_the_face_is_ignored() {
        let a = scene(0, 0, 1.0, 0.0, Some((5, 5)));
        let b = scene(0, 0, 1.0, 0.0, Some((160, 170)));
        let p = profile(&a, &b, &MIDDLE);
        assert_eq!(p.total, 0.0, "{p:?}");
    }

    #[test]
    fn rigid_translation_leaves_little_residual() {
        // The scene moves 6px and the detection doesn't follow: the shift
        // search has to find it.
        let a = scene(0, 0, 1.0, 0.0, None);
        let b = scene(6, -4, 1.0, 0.0, None);
        let p = profile(&a, &b, &MIDDLE);
        assert!(p.total > 0.1, "{p:?}");
        assert!(p.residual < 0.02, "{p:?}");
        assert_eq!(p.shift, (-3.0, 2.0));
    }

    #[test]
    fn subpixel_translation_leaves_little_local_residual() {
        // 1 frame px is about half a patch px: whole-pixel alignment alone
        // would leave residue along every edge.
        let a = scene(0, 0, 1.0, 0.0, None);
        let b = scene(1, 1, 1.0, 0.0, None);
        let p = profile(&a, &b, &MIDDLE);
        assert!(p.local < 0.1, "{p:?}");
        assert!(p.residual < 0.01, "{p:?}");
    }

    #[test]
    fn blink_sized_change_is_local_not_spread() {
        // An eyelid closing: a 16px square (8x8 in the patch, most of one
        // block, a small share of the face) loses its texture.
        let a = scene(0, 0, 1.0, 0.0, None);
        let mut blink = a.clone();
        for y in 90..106u32 {
            for x in 90..106u32 {
                blink.data[(y * 200 + x) as usize] = 20000;
            }
        }
        let p = profile(&a, &blink, &MIDDLE);
        assert!(p.local > 0.3, "{p:?}");
        assert!(p.residual < 0.1, "{p:?}");
    }

    #[test]
    fn local_change_inside_the_face_is_residual() {
        let a = scene(0, 0, 1.0, 0.0, None);
        let b = scene(0, 0, 1.0, 0.0, Some((85, 90)));
        let p = profile(&a, &b, &MIDDLE);
        assert!(p.residual > 0.05, "{p:?}");
    }

    #[test]
    fn flat_patches_have_zero_motion() {
        let a = frame(vec![30000; 100 * 100], 100, 100);
        let b = frame(vec![40000; 100 * 100], 100, 100);
        assert_eq!(profile(&a, &b, &MIDDLE).total, 0.0);
    }

    fn spread(f: &IrFrame) -> u16 {
        let max = f.data.iter().copied().max().unwrap_or(0);
        let min = f.data.iter().copied().min().unwrap_or(0);
        max - min
    }
}
