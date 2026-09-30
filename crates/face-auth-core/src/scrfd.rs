//! SCRFD face detection with five landmarks, and alignment of the face to the
//! template the ArcFace-family encoders were trained on (#27).
//!
//! `w600k_mbf` / `w600k_r50` expect a 112x112 crop warped so the eyes, nose
//! tip and mouth corners land on fixed points. A plain box crop leaves roll,
//! scale and centring to vary from frame to frame, and that variation leaks
//! into the embedding. SCRFD (`det_500m.onnx`, shipped in the same InsightFace
//! pack as `w600k_mbf`) finds the five points; `align` warps to the template.
//!
//! Reimplemented from the published model's I/O and the standard similarity
//! (Umeyama) fit; nothing copied.

use crate::capture::IrFrame;
use crate::detector::FaceBox;
use tract_onnx::prelude::tract_ndarray::Array3;

/// Square input side. The frame is letterboxed into it (scaled to fit, padded
/// right and bottom), so boxes map back with one scale factor.
pub const INPUT_SIZE: usize = 320;
const STRIDES: [usize; 3] = [8, 16, 32];
const ANCHORS_PER_CELL: usize = 2;
/// Model outputs: scores for each stride, then boxes, then landmarks.
pub const OUTPUTS: usize = 9;

/// ArcFace's 112x112 template: left eye, right eye, nose tip, left and right
/// mouth corner (from the image's point of view).
const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];
pub const ALIGNED_SIZE: usize = 112;

/// How the frame was fitted into the input: input pixels per frame pixel.
pub struct Letterbox {
    scale: f32,
}

/// The detector input `[3, 320, 320]`, `(v - 127.5) / 128` on 8-bit values,
/// greyscale replicated to all three channels. Bilinear downscale.
pub fn preprocess(frame: &IrFrame) -> anyhow::Result<(Array3<f32>, Letterbox)> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    anyhow::ensure!(w > 0 && h > 0 && frame.data.len() == w * h, "bad frame geometry {w}x{h}");
    let scale = INPUT_SIZE as f32 / w.max(h) as f32;
    let (sw, sh) = (((w as f32 * scale).round() as usize).min(INPUT_SIZE), ((h as f32 * scale).round() as usize).min(INPUT_SIZE));
    // Padding is 0 in 8-bit terms, as the reference implementation pads.
    let pad = -127.5 / 128.0;
    let mut input = Array3::<f32>::from_elem((3, INPUT_SIZE, INPUT_SIZE), pad);
    for y in 0..sh {
        for x in 0..sw {
            let v = sample(frame, (x as f32 + 0.5) / scale - 0.5, (y as f32 + 0.5) / scale - 0.5) / 257.0;
            let n = (v - 127.5) / 128.0;
            for c in 0..3 {
                input[[c, y, x]] = n;
            }
        }
    }
    Ok((input, Letterbox { scale }))
}

/// Bilinear sample in frame pixel coordinates; outside the frame is 0.
fn sample(frame: &IrFrame, x: f32, y: f32) -> f32 {
    let (w, h) = (frame.width as i64, frame.height as i64);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |xi: i64, yi: i64| -> f32 {
        if xi < 0 || yi < 0 || xi >= w || yi >= h {
            0.0
        } else {
            frame.data[(yi * w + xi) as usize] as f32
        }
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// The highest-scoring face, as a box normalised to the frame with its five
/// landmarks, or `None` below `threshold`. `outputs` are the model's nine, in
/// order: scores, boxes, landmarks, each for strides 8, 16 and 32.
pub fn best_face(outputs: &[Vec<f32>], lb: &Letterbox, frame: &IrFrame, threshold: f32) -> anyhow::Result<Option<FaceBox>> {
    anyhow::ensure!(outputs.len() == OUTPUTS, "SCRFD returned {} outputs, expected {OUTPUTS}", outputs.len());
    let mut best: Option<(f32, usize, usize)> = None; // (score, stride index, anchor)
    for (s, &stride) in STRIDES.iter().enumerate() {
        let cells = (INPUT_SIZE / stride) * (INPUT_SIZE / stride);
        let anchors = cells * ANCHORS_PER_CELL;
        let (scores, boxes, kps) = (&outputs[s], &outputs[3 + s], &outputs[6 + s]);
        anyhow::ensure!(
            scores.len() == anchors && boxes.len() == anchors * 4 && kps.len() == anchors * 10,
            "SCRFD stride {stride}: {} scores / {} boxes / {} landmarks, expected {anchors} anchors",
            scores.len(),
            boxes.len(),
            kps.len()
        );
        for (i, &score) in scores.iter().enumerate() {
            if score.is_finite() && best.is_none_or(|(b, _, _)| score > b) {
                best = Some((score, s, i));
            }
        }
    }
    let Some((score, s, i)) = best else { return Ok(None) };
    tracing::debug!(max_face = score, threshold, "detector: best face score");
    if score < threshold {
        return Ok(None);
    }

    let stride = STRIDES[s] as f32;
    let cell = i / ANCHORS_PER_CELL;
    let per_row = INPUT_SIZE / STRIDES[s];
    // Anchor centres sit on the grid points themselves, not cell centres.
    let (cx, cy) = ((cell % per_row) as f32 * stride, (cell / per_row) as f32 * stride);
    let d = &outputs[3 + s][i * 4..i * 4 + 4];
    let k = &outputs[6 + s][i * 10..i * 10 + 10];
    let (fw, fh) = (frame.width as f32, frame.height as f32);
    // Input pixels → normalised frame coordinates.
    let nx = |x: f32| x / lb.scale / fw;
    let ny = |y: f32| y / lb.scale / fh;
    let mut landmarks = [(0.0f32, 0.0f32); 5];
    for (p, point) in landmarks.iter_mut().enumerate() {
        *point = (nx(cx + k[p * 2] * stride), ny(cy + k[p * 2 + 1] * stride));
    }
    let face_box = FaceBox {
        x1: nx(cx - d[0] * stride),
        y1: ny(cy - d[1] * stride),
        x2: nx(cx + d[2] * stride),
        y2: ny(cy + d[3] * stride),
        landmarks: Some(landmarks),
    };
    anyhow::ensure!(
        [face_box.x1, face_box.y1, face_box.x2, face_box.y2].iter().all(|v| v.is_finite())
            && landmarks.iter().all(|(x, y)| x.is_finite() && y.is_finite()),
        "SCRFD produced a non-finite face"
    );
    tracing::debug!(?face_box, "detector: face box");
    Ok(Some(face_box))
}

/// Least-squares similarity transform (rotation, uniform scale, translation;
/// no reflection) taking `src` onto `dst`: `[a, b, tx, ty]` for
/// `x' = a x - b y + tx`, `y' = b x + a y + ty`. `None` when the points are
/// coincident, so no transform is defined.
fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<[f32; 4]> {
    let mean = |p: &[[f32; 2]; 5]| {
        let (sx, sy) = p.iter().fold((0.0, 0.0), |(x, y), q| (x + q[0], y + q[1]));
        (sx / 5.0, sy / 5.0)
    };
    let (msx, msy) = mean(src);
    let (mdx, mdy) = mean(dst);
    let (mut norm, mut dot, mut cross) = (0.0f32, 0.0f32, 0.0f32);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy, dx, dy) = (s[0] - msx, s[1] - msy, d[0] - mdx, d[1] - mdy);
        norm += sx * sx + sy * sy;
        dot += sx * dx + sy * dy;
        cross += sx * dy - sy * dx;
    }
    if !norm.is_finite() || norm <= 1e-6 {
        return None;
    }
    let (a, b) = (dot / norm, cross / norm);
    let t = [a, b, mdx - (a * msx - b * msy), mdy - (b * msx + a * msy)];
    t.iter().all(|v| v.is_finite()).then_some(t)
}

/// Warp the face so its landmarks sit on the ArcFace template, into the
/// encoder's `[3, 112, 112]` input normalised to `[-1, 1]` (the same scale
/// `preprocess_ir_frame` uses).
///
/// The arithmetic here defines what an aligned embedding means; changing it
/// invalidates every template enrolled with it.
pub fn align(frame: &IrFrame, face_box: &FaceBox) -> anyhow::Result<Array3<f32>> {
    let lm = face_box.landmarks.ok_or_else(|| anyhow::anyhow!("face has no landmarks to align"))?;
    let (fw, fh) = (frame.width as f32, frame.height as f32);
    let src = lm.map(|(x, y)| [x * fw, y * fh]);
    let [a, b, tx, ty] =
        similarity(&src, &TEMPLATE).ok_or_else(|| anyhow::anyhow!("face landmarks are degenerate"))?;
    // Inverse of [[a, -b], [b, a]] is [[a, b], [-b, a]] / (a² + b²).
    let det = a * a + b * b;
    let mut out = Array3::<f32>::zeros((3, ALIGNED_SIZE, ALIGNED_SIZE));
    for v in 0..ALIGNED_SIZE {
        for u in 0..ALIGNED_SIZE {
            let (du, dv) = (u as f32 - tx, v as f32 - ty);
            let x = (a * du + b * dv) / det;
            let y = (-b * du + a * dv) / det;
            let n = (sample(frame, x, y) / 65535.0 - 0.5) / 0.5;
            for c in 0..3 {
                out[[c, v, u]] = n;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, f: impl Fn(u32, u32) -> u16) -> IrFrame {
        let data = (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect();
        IrFrame { data, width, height }
    }

    #[test]
    fn similarity_recovers_a_known_transform() {
        // Scale 0.5, rotate 30°, translate (10, -4).
        let (s, r) = (0.5f32, 30f32.to_radians());
        let (a, b) = (s * r.cos(), s * r.sin());
        let src = TEMPLATE.map(|[x, y]| [x * 3.0 + 7.0, y * 3.0 - 2.0]);
        let dst = src.map(|[x, y]| [a * x - b * y + 10.0, b * x + a * y - 4.0]);
        let t = similarity(&src, &dst).unwrap();
        for (got, want) in t.iter().zip([a, b, 10.0, -4.0]) {
            assert!((got - want).abs() < 1e-3, "{t:?}");
        }
    }

    #[test]
    fn coincident_landmarks_are_refused() {
        assert!(similarity(&[[5.0, 5.0]; 5], &TEMPLATE).is_none());
        let f = frame(64, 64, |_, _| 1000);
        let face = FaceBox { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0, landmarks: Some([(0.5, 0.5); 5]) };
        assert!(align(&f, &face).is_err());
        let boxed_only = FaceBox { landmarks: None, ..face };
        assert!(align(&f, &boxed_only).is_err());
    }

    #[test]
    fn landmarks_already_on_the_template_copy_pixels_through() {
        // A 112x112 frame whose landmarks are exactly the template: identity.
        let f = frame(112, 112, |x, y| ((x * 500 + y * 7) % 65535) as u16);
        let lm = TEMPLATE.map(|[x, y]| (x / 112.0, y / 112.0));
        let face = FaceBox { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0, landmarks: Some(lm) };
        let out = align(&f, &face).unwrap();
        for (u, v) in [(0usize, 0usize), (50, 60), (111, 111)] {
            let want = (f.data[v * 112 + u] as f32 / 65535.0 - 0.5) / 0.5;
            assert!((out[[0, v, u]] - want).abs() < 1e-3, "({u},{v})");
        }
    }

    #[test]
    fn letterbox_scales_the_long_side_and_pads() {
        let f = frame(640, 320, |_, _| 255 * 257);
        let (input, lb) = preprocess(&f).unwrap();
        assert!((lb.scale - 0.5).abs() < 1e-6);
        assert!((input[[0, 10, 10]] - (127.5 / 128.0)).abs() < 1e-3, "inside the frame");
        assert!((input[[0, 200, 10]] + (127.5 / 128.0)).abs() < 1e-3, "padding below");
    }

    #[test]
    fn decode_maps_the_best_anchor_back_to_the_frame() {
        let f = frame(640, 640, |_, _| 0);
        let lb = Letterbox { scale: 0.5 };
        // Scores 0 everywhere; every box and landmark offset 1 stride.
        let mut outputs: Vec<Vec<f32>> = Vec::new();
        for per in [1, 4, 10] {
            for &stride in &STRIDES {
                let anchors = (INPUT_SIZE / stride).pow(2) * ANCHORS_PER_CELL;
                outputs.push(vec![if per == 1 { 0.0 } else { 1.0 }; anchors * per]);
            }
        }
        // Stride 16, cell (x 3, y 2), second anchor: centre (48, 32) in input pixels.
        let i = (2 * (INPUT_SIZE / 16) + 3) * 2 + 1;
        outputs[1][i] = 0.9;
        let face = best_face(&outputs, &lb, &f, 0.5).unwrap().unwrap();
        // Box distances are 1 stride each way: input (32..64, 16..48), frame x2.
        assert!((face.x1 - 64.0 / 640.0).abs() < 1e-5 && (face.x2 - 128.0 / 640.0).abs() < 1e-5);
        assert!((face.y1 - 32.0 / 640.0).abs() < 1e-5 && (face.y2 - 96.0 / 640.0).abs() < 1e-5);
        let (lx, ly) = face.landmarks.unwrap()[0];
        assert!((lx - 128.0 / 640.0).abs() < 1e-5 && (ly - 96.0 / 640.0).abs() < 1e-5);

        outputs[1][i] = 0.4;
        assert!(best_face(&outputs, &lb, &f, 0.5).unwrap().is_none());
        assert!(best_face(&outputs[..8], &lb, &f, 0.5).is_err());
    }
}
