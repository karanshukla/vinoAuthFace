//! A BlazeFace-style detector with five alignment points (fork-only): the
//! short-range MediaPipe layout, retrained so its points are the ArcFace
//! five. Not shipped; this is the decoder, so such a model works wherever
//! SCRFD does once its path is set.
//!
//! The model's interface:
//!
//! - input `[1, 3, 128, 128]`: the greyscale frame copied to three channels,
//!   scaled to [-1, 1], letterboxed into the top-left corner with zeros below
//!   and to the right;
//! - outputs `regressors` `[1, 896, 16]` and `classificators` `[1, 896, 1]`
//!   (logits), one row per anchor.
//!
//! Each anchor's 16 values, divided by 128, are the box centre's offset from
//! the anchor, the box width and height, then six points as offsets from the
//! anchor: image-left eye, image-right eye, nose tip, image-left mouth corner,
//! image-right mouth corner, and one unused. The first five are in
//! `scrfd::TEMPLATE`'s order, so they align the face exactly as SCRFD's do.
//!
//! Detection runs twice. The first pass finds the face in the whole frame;
//! the second looks again at a square around it, sized so the eyes are about
//! [`EYES_IN_CROP`] pixels apart in the input, which gives the points the
//! resolution the model was trained at. If the second pass loses the face,
//! the first pass's answer stands.

use crate::capture::IrFrame;
use crate::detector::FaceBox;
use tract_onnx::prelude::tract_ndarray::Array3;

pub const INPUT_SIZE: usize = 128;
/// Anchors per cell and cells per side, per stride.
const LAYERS: [(usize, usize); 2] = [(16, 2), (8, 6)];
pub const ANCHORS: usize = 16 * 16 * 2 + 8 * 8 * 6;
pub const REGRESSORS: usize = 16;
const SCALE: f32 = 128.0;
/// Candidates overlapping the best by more than this are merged into it.
const NMS_IOU: f32 = 0.3;
/// Eye distance, in input pixels, the second pass crops for.
pub const EYES_IN_CROP: f32 = 30.0;
/// The input outside the frame (the letterbox and any crop past the edge).
const PAD: f32 = 0.0;

/// Where the input square sits in the frame, in frame pixels: its top-left
/// corner and its side. A letterbox is a square from the frame's corner as
/// wide as the frame's longer side; a crop is any square.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub x: f32,
    pub y: f32,
    pub side: f32,
}

impl Window {
    pub fn letterbox(frame: &IrFrame) -> Window {
        Window { x: 0.0, y: 0.0, side: frame.width.max(frame.height) as f32 }
    }

    /// A point in the input, from 0 to 1 on each axis, as a fraction of the
    /// frame's width and height.
    fn to_frame(self, frame: &IrFrame, (u, v): (f32, f32)) -> (f32, f32) {
        ((self.x + u * self.side) / frame.width as f32, (self.y + v * self.side) / frame.height as f32)
    }
}

/// The model input for the part of `frame` under `window`. Bilinear.
pub fn preprocess(frame: &IrFrame, window: Window) -> anyhow::Result<Array3<f32>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    anyhow::ensure!(w > 0 && h > 0 && frame.data.len() == w * h, "bad frame geometry {w}x{h}");
    anyhow::ensure!(window.side.is_finite() && window.side > 0.0, "empty detector window");
    let step = window.side / INPUT_SIZE as f32;
    let mut input = Array3::<f32>::from_elem((3, INPUT_SIZE, INPUT_SIZE), PAD);
    for y in 0..INPUT_SIZE {
        for x in 0..INPUT_SIZE {
            let fx = window.x + (x as f32 + 0.5) * step - 0.5;
            let fy = window.y + (y as f32 + 0.5) * step - 0.5;
            if fx < -0.5 || fy < -0.5 || fx > w as f32 - 0.5 || fy > h as f32 - 0.5 {
                continue;
            }
            let v = crate::scrfd::sample(frame, fx, fy) / 257.0 / 127.5 - 1.0;
            for c in 0..3 {
                input[[c, y, x]] = v;
            }
        }
    }
    Ok(input)
}

/// The anchor centres, as fractions of the input, in the model's row order.
fn anchors() -> &'static [(f32, f32)] {
    use std::sync::OnceLock;
    static ANCHOR_LIST: OnceLock<Vec<(f32, f32)>> = OnceLock::new();
    ANCHOR_LIST.get_or_init(|| {
        let mut list = Vec::with_capacity(ANCHORS);
        for (cells, per_cell) in LAYERS {
            for y in 0..cells {
                for x in 0..cells {
                    let centre = ((x as f32 + 0.5) / cells as f32, (y as f32 + 0.5) / cells as f32);
                    list.extend(std::iter::repeat_n(centre, per_cell));
                }
            }
        }
        list
    })
}

/// One decoded anchor, in input fractions.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    score: f32,
    /// x1, y1, x2, y2.
    bbox: [f32; 4],
    points: [(f32, f32); 5],
}

impl Candidate {
    fn area(&self) -> f32 {
        (self.bbox[2] - self.bbox[0]).max(0.0) * (self.bbox[3] - self.bbox[1]).max(0.0)
    }

    fn iou(&self, other: &Candidate) -> f32 {
        let ix = (self.bbox[2].min(other.bbox[2]) - self.bbox[0].max(other.bbox[0])).max(0.0);
        let iy = (self.bbox[3].min(other.bbox[3]) - self.bbox[1].max(other.bbox[1])).max(0.0);
        let inter = ix * iy;
        let union = self.area() + other.area() - inter;
        if union > 0.0 {
            inter / union
        } else {
            0.0
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-100.0, 100.0)).exp())
}

fn decode(row: &[f32], (ax, ay): (f32, f32), score: f32) -> Candidate {
    let (cx, cy) = (ax + row[0] / SCALE, ay + row[1] / SCALE);
    let (w, h) = (row[2] / SCALE, row[3] / SCALE);
    let point = |k: usize| (ax + row[4 + 2 * k] / SCALE, ay + row[5 + 2 * k] / SCALE);
    Candidate {
        score,
        bbox: [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0],
        points: [point(0), point(1), point(2), point(3), point(4)],
    }
}

/// Weighted non-maximum suppression: each face is the score-weighted mean of
/// the candidates overlapping its best one.
fn merge(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut faces = Vec::new();
    while let Some(top) = candidates.first().copied() {
        let (group, rest): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| c.iou(&top) > NMS_IOU);
        candidates = rest;
        let total: f32 = group.iter().map(|c| c.score).sum();
        let mut face = Candidate { score: top.score, bbox: [0.0; 4], points: [(0.0, 0.0); 5] };
        for c in &group {
            let wgt = c.score / total;
            for i in 0..4 {
                face.bbox[i] += c.bbox[i] * wgt;
            }
            for k in 0..5 {
                face.points[k].0 += c.points[k].0 * wgt;
                face.points[k].1 += c.points[k].1 * wgt;
            }
        }
        faces.push(face);
    }
    faces
}

/// The largest face scoring at least `threshold` in one run's outputs
/// (`regressors`, then `classificators`), in input fractions. Largest, as in
/// `scrfd::best_face`: the user is the face nearest the camera.
fn best(outputs: &[Vec<f32>], threshold: f32) -> anyhow::Result<Option<Candidate>> {
    anyhow::ensure!(outputs.len() == 2, "BlazeFace returned {} outputs, expected 2", outputs.len());
    let (regressors, logits) = (&outputs[0], &outputs[1]);
    anyhow::ensure!(
        regressors.len() == ANCHORS * REGRESSORS && logits.len() == ANCHORS,
        "BlazeFace returned {} regressor / {} score values, expected {ANCHORS} anchors",
        regressors.len(),
        logits.len()
    );
    let mut max_face = f32::NEG_INFINITY;
    let mut candidates = Vec::new();
    for (i, (&logit, &anchor)) in logits.iter().zip(anchors()).enumerate() {
        if !logit.is_finite() {
            continue;
        }
        let score = sigmoid(logit);
        max_face = max_face.max(score);
        if score >= threshold {
            let row = &regressors[i * REGRESSORS..(i + 1) * REGRESSORS];
            if row.iter().all(|v| v.is_finite()) {
                candidates.push(decode(row, anchor, score));
            }
        }
    }
    tracing::debug!(max_face, threshold, "detector: best face score");
    Ok(merge(candidates).into_iter().max_by(|a, b| a.area().total_cmp(&b.area())))
}

/// `candidate`, found through `window`, as a face in frame fractions.
fn to_face(frame: &IrFrame, window: Window, candidate: &Candidate) -> FaceBox {
    let (x1, y1) = window.to_frame(frame, (candidate.bbox[0], candidate.bbox[1]));
    let (x2, y2) = window.to_frame(frame, (candidate.bbox[2], candidate.bbox[3]));
    FaceBox { x1, y1, x2, y2, landmarks: Some(candidate.points.map(|p| window.to_frame(frame, p))) }
}

/// The square around a face for the second pass, in frame pixels: centred
/// on its points, the eyes [`EYES_IN_CROP`] input pixels apart.
pub fn crop_for(frame: &IrFrame, face: &FaceBox) -> Option<Window> {
    let points = face.landmarks?;
    let px = |(x, y): (f32, f32)| (x * frame.width as f32, y * frame.height as f32);
    let points = points.map(px);
    let eyes = (points[1].0 - points[0].0).hypot(points[1].1 - points[0].1);
    if !eyes.is_finite() || eyes < 1.0 {
        return None;
    }
    let (cx, cy) = (points.iter().map(|p| p.0).sum::<f32>() / 5.0, points.iter().map(|p| p.1).sum::<f32>() / 5.0);
    let side = eyes * INPUT_SIZE as f32 / EYES_IN_CROP;
    Some(Window { x: cx - side / 2.0, y: cy - side / 2.0, side })
}

/// Both passes. `run` runs the model on one input and returns its outputs.
pub fn detect(
    frame: &IrFrame,
    threshold: f32,
    mut run: impl FnMut(Array3<f32>) -> anyhow::Result<Vec<Vec<f32>>>,
) -> anyhow::Result<Option<FaceBox>> {
    let whole = Window::letterbox(frame);
    let Some(first) = best(&run(preprocess(frame, whole)?)?, threshold)? else {
        return Ok(None);
    };
    let first = to_face(frame, whole, &first);
    let Some(crop) = crop_for(frame, &first) else {
        return Ok(Some(first));
    };
    // In the crop the face is the one nearest its centre, which `best`'s
    // largest-face rule gives too: the crop is sized to this face.
    match best(&run(preprocess(frame, crop)?)?, threshold)? {
        Some(second) => Ok(Some(to_face(frame, crop, &second))),
        None => {
            tracing::debug!("detector: second pass lost the face; keeping the first");
            Ok(Some(first))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> IrFrame {
        IrFrame { data: vec![128 * 257; (width * height) as usize], width, height }
    }

    #[test]
    fn the_anchor_grid_matches_the_model() {
        let list = anchors();
        assert_eq!(list.len(), ANCHORS);
        assert_eq!(ANCHORS, 896);
        assert_eq!(list[0], (0.5 / 16.0, 0.5 / 16.0));
        assert_eq!(list[1], list[0], "two anchors per cell at stride 8");
        assert_eq!(list[16 * 16 * 2], (0.5 / 8.0, 0.5 / 8.0), "stride 16 starts after stride 8");
        assert_eq!(list[ANCHORS - 1], (7.5 / 8.0, 7.5 / 8.0));
    }

    /// Outputs with one face on `anchor`: a box `size` wide around its centre
    /// and points spread across it.
    fn outputs_with(faces: &[(usize, f32, f32)]) -> Vec<Vec<f32>> {
        let mut regressors = vec![0.0; ANCHORS * REGRESSORS];
        let mut logits = vec![-20.0; ANCHORS];
        for &(anchor, size, logit) in faces {
            let row = &mut regressors[anchor * REGRESSORS..(anchor + 1) * REGRESSORS];
            row[2] = size * SCALE;
            row[3] = size * SCALE;
            let offsets = [(-0.2, -0.1), (0.2, -0.1), (0.0, 0.05), (-0.15, 0.2), (0.15, 0.2)];
            for (k, (dx, dy)) in offsets.iter().enumerate() {
                row[4 + 2 * k] = dx * size * SCALE;
                row[5 + 2 * k] = dy * size * SCALE;
            }
            logits[anchor] = logit;
        }
        vec![regressors, logits]
    }

    #[test]
    fn decodes_a_face_back_to_the_frame() {
        // A 640x480 frame letterboxed: the input covers 640x640 from the corner.
        let f = frame(640, 480);
        let anchor = 16 * 16 * 2 + (3 * 8 + 4) * 6; // stride 16, cell (4, 3)
        let best = best(&outputs_with(&[(anchor, 0.25, 4.0)]), 0.5).unwrap().unwrap();
        let face = to_face(&f, Window::letterbox(&f), &best);
        let (ax, ay) = anchors()[anchor];
        assert!((face.x1 - (ax - 0.125) * 640.0 / 640.0).abs() < 1e-5, "{face:?}");
        assert!((face.y1 - (ay - 0.125) * 640.0 / 480.0).abs() < 1e-5, "{face:?}");
        let points = face.landmarks.unwrap();
        assert!(points[0].0 < points[1].0, "image-left eye first");
        assert!(points[3].1 > points[2].1, "mouth below the nose");
    }

    #[test]
    fn nothing_over_the_threshold_is_no_face() {
        assert!(best(&outputs_with(&[(100, 0.25, -1.0)]), 0.5).unwrap().is_none());
        assert!(best(&outputs_with(&[]), 0.5).unwrap().is_none());
    }

    #[test]
    fn overlapping_anchors_merge_and_the_largest_face_wins() {
        // Two anchors of one cell (a near-duplicate pair), and a smaller, surer face elsewhere.
        let big = 16 * 16 * 2 + (3 * 8 + 4) * 6;
        let small = 5;
        let faces = merge(
            [(big, 0.3, 2.0), (big + 1, 0.3, 1.0), (small, 0.1, 8.0)]
                .iter()
                .filter_map(|&(a, s, l)| {
                    let out = outputs_with(&[(a, s, l)]);
                    best(&out, 0.5).unwrap()
                })
                .collect(),
        );
        assert_eq!(faces.len(), 2, "the pair merged into one");
        let chosen = best(&outputs_with(&[(big, 0.3, 2.0), (big + 1, 0.3, 1.0), (small, 0.1, 8.0)]), 0.5)
            .unwrap()
            .unwrap();
        assert!((chosen.area() - 0.09).abs() < 1e-4, "the larger face, not the surer one");
    }

    #[test]
    fn rejects_outputs_of_the_wrong_shape() {
        assert!(best(&[vec![0.0; 10], vec![0.0; 10]], 0.5).is_err());
        assert!(best(&[vec![0.0; ANCHORS * REGRESSORS]], 0.5).is_err());
    }

    #[test]
    fn the_second_pass_crops_for_thirty_pixel_eyes() {
        let f = frame(640, 480);
        let n = |x: f32, y: f32| (x / 640.0, y / 480.0);
        let face = FaceBox {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            landmarks: Some([n(300.0, 200.0), n(360.0, 200.0), n(330.0, 230.0), n(310.0, 260.0), n(350.0, 260.0)]),
        };
        let crop = crop_for(&f, &face).unwrap();
        assert!((crop.side - 60.0 * 128.0 / 30.0).abs() < 1e-3);
        assert!((crop.x + crop.side / 2.0 - 330.0).abs() < 1e-3);
        assert!((crop.y + crop.side / 2.0 - 230.0).abs() < 1e-3);
        assert!(crop_for(&f, &FaceBox { landmarks: None, ..face }).is_none());
    }

    #[test]
    fn the_input_is_the_frame_scaled_to_minus_one_one_and_padded() {
        let f = frame(64, 32);
        let input = preprocess(&f, Window::letterbox(&f)).unwrap();
        // 64x32 letterboxed into 128: the frame fills the top half.
        let grey = 128.0 / 127.5 - 1.0;
        assert!((input[[0, 10, 10]] - grey).abs() < 1e-4);
        assert!((input[[2, 60, 120]] - grey).abs() < 1e-4);
        assert_eq!(input[[0, 100, 10]], PAD);
    }

    #[test]
    fn detect_runs_twice_and_keeps_the_first_pass_if_the_second_loses_it() {
        let f = frame(640, 480);
        let anchor = 16 * 16 * 2 + (3 * 8 + 4) * 6;
        let mut runs = 0;
        let face = detect(&f, 0.5, |_| {
            runs += 1;
            Ok(if runs == 1 { outputs_with(&[(anchor, 0.25, 4.0)]) } else { outputs_with(&[]) })
        })
        .unwrap()
        .unwrap();
        assert_eq!(runs, 2);
        assert!(face.landmarks.is_some());
        let mut runs = 0;
        assert!(detect(&f, 0.5, |_| {
            runs += 1;
            Ok(outputs_with(&[]))
        })
        .unwrap()
        .is_none());
        assert_eq!(runs, 1, "no face, no second pass");
    }
}
