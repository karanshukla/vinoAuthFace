//! Guided enrolment (fork-only): about ten templates from poses the camera
//! has measured, instead of thirty near-copies of whatever pose the user held.
//!
//! Matching takes the best template (`verify::max_similarity`), so templates
//! that differ (a slight turn, a tilt, glasses on and off) cover more of how
//! the user will be seen at unlock than many of one pose. Each step says what
//! to do and only keeps frames whose pose shows it was done: the pose comes
//! from SCRFD's five landmarks, so guided enrolment needs a detector that has
//! them.
//!
//! Left and right, up and down are never assumed: an IR sensor's image may be
//! mirrored, and which way a tilt moves the nose depends on the camera's
//! height. The first turn the user makes (asked "to one side") sets the side,
//! and the next step wants the other; likewise for tilts.
//!
//! The pose bounds are issue #19's starting bracket for IR, not yet
//! measured on a real sensor. Tune them on the laptop, and measure the
//! false-accept rate before and after, as the issue asks: off-angle
//! templates can raise impostor scores at the same threshold.

use crate::capture::IrFrame;
use crate::detector::{assess_frame, DetectorKind, FaceBox, FrameQuality};
use crate::{face_input, Camera, FaceAuth};
use anyhow::{bail, Result};
use std::time::{Duration, Instant};

/// Eyes closer than this in the frame are too far away to keep: below about
/// 30 px the five points, and so the alignment, get unreliable.
pub const MIN_EYES_PX: f32 = 30.0;
/// Straight on: the nose within this share of the eye distance of the eyes'
/// midpoint, and between the eyes and the mouth.
const STRAIGHT_YAW: f32 = 0.18;
const STRAIGHT_PITCH: std::ops::RangeInclusive<f32> = 0.2..=0.8;
/// A slight turn, as a change from the straight-on yaw. The upper bound keeps
/// it slight: further than unlock's own frames go adds false-accept risk and
/// no coverage.
const TURN: std::ops::RangeInclusive<f32> = 0.16..=0.40;
/// A slight tilt, as a change in where the nose sits between eyes and mouth.
const TILT: std::ops::RangeInclusive<f32> = 0.07..=0.25;
/// Two frames in a row this close on both axes, before one is kept: a pose
/// held, not one passed through on the way somewhere else.
const STEADY_YAW: f32 = 0.08;
const STEADY_PITCH: f32 = 0.06;
/// How far a tilt may also turn.
const TILT_YAW: f32 = 0.12;
/// A frame this alike to one already kept adds nothing.
const DUPLICATE: f32 = 0.97;
/// How long a step waits for its pose before moving on.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Time to take glasses off or put them on before the glasses step looks.
const GLASSES_PAUSE: Duration = Duration::from_secs(4);
const PAUSE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Straight,
    Side,
    OtherSide,
    Up,
    Down,
    Glasses,
}

impl Step {
    pub const ALL: [Step; 6] = [Step::Straight, Step::Side, Step::OtherSide, Step::Up, Step::Down, Step::Glasses];

    /// The word `vinoauthface enroll --guided` prints for it.
    pub fn id(self) -> &'static str {
        match self {
            Step::Straight => "straight",
            Step::Side => "side",
            Step::OtherSide => "other-side",
            Step::Up => "up",
            Step::Down => "down",
            Step::Glasses => "glasses",
        }
    }

    pub fn parse(id: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Step::Straight => "Look straight at the camera.",
            Step::Side => "Turn your head a little to one side.",
            Step::OtherSide => "Now a little to the other side.",
            Step::Up => "Tilt your chin up a little.",
            Step::Down => "Now tilt it down a little.",
            Step::Glasses => "Take your glasses off, or put them on. Skip if you wear none.",
        }
    }

    /// Templates kept from it: ten in all.
    pub fn wanted(self) -> usize {
        match self {
            Step::Up | Step::Down => 1,
            _ => 2,
        }
    }
}

/// Why a frame wasn't kept, for the person in front of the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    NoContent,
    NoFace,
    TooFar,
    LookAtCamera,
    TurnMore,
    TurnLess,
    OtherWay,
    TiltMore,
    TiltLess,
    /// Too alike to a frame already kept: move a little.
    Same,
    /// The glasses step's pause.
    Waiting,
    /// In the pose, but still moving.
    Hold,
}

impl Hint {
    pub const ALL: [Hint; 12] = [
        Hint::NoContent,
        Hint::NoFace,
        Hint::TooFar,
        Hint::LookAtCamera,
        Hint::TurnMore,
        Hint::TurnLess,
        Hint::OtherWay,
        Hint::TiltMore,
        Hint::TiltLess,
        Hint::Same,
        Hint::Waiting,
        Hint::Hold,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Hint::NoContent => "dark",
            Hint::NoFace => "no-face",
            Hint::TooFar => "too-far",
            Hint::LookAtCamera => "look-at-camera",
            Hint::TurnMore => "turn-more",
            Hint::TurnLess => "turn-less",
            Hint::OtherWay => "other-way",
            Hint::TiltMore => "tilt-more",
            Hint::TiltLess => "tilt-less",
            Hint::Same => "same",
            Hint::Waiting => "waiting",
            Hint::Hold => "hold",
        }
    }

    pub fn parse(id: &str) -> Option<Hint> {
        Hint::ALL.into_iter().find(|h| h.id() == id)
    }

    pub fn text(self) -> &'static str {
        match self {
            Hint::NoContent => "Nothing in view. Is the IR emitter on?",
            Hint::NoFace => "No face in view.",
            Hint::TooFar => "Move closer.",
            Hint::LookAtCamera => "Look at the camera.",
            Hint::TurnMore => "Turn a little further.",
            Hint::TurnLess => "Not so far.",
            Hint::OtherWay => "The other way.",
            Hint::TiltMore => "Tilt a little further.",
            Hint::TiltLess => "Not so far.",
            Hint::Same => "Move a little.",
            Hint::Waiting => "Get ready…",
            Hint::Hold => "Hold still.",
        }
    }
}

#[derive(Debug, Clone)]
pub enum GuidedProgress {
    /// Step `index` (from 1) of `total` begins.
    Step { index: usize, total: usize, step: Step },
    Hint(Hint),
    Kept { kept: usize, wanted: usize },
    /// The step ended with nothing kept: skipped, or it timed out.
    Skipped(Step),
}

/// What the person enrolling asked for, between frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Skip,
    Cancel,
}

/// The error a cancelled enrolment ends with. Nothing was saved.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("enrolment cancelled; nothing was saved")
    }
}

impl std::error::Error for Cancelled {}

/// Head pose from the five landmarks, in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// The nose's offset from the eyes' midpoint along the eye line, over the
    /// eye distance: 0 straight on, signed by which way the head turns.
    pub yaw: f32,
    /// Where the nose sits from the eyes (0) to the mouth (1).
    pub pitch: f32,
    /// Eye distance in pixels.
    pub eyes_px: f32,
}

pub fn pose(face: &FaceBox, width: u32, height: u32) -> Option<Pose> {
    let points = face.landmarks?;
    let px = |(x, y): (f32, f32)| (x * width as f32, y * height as f32);
    let [left, right, nose, mouth_l, mouth_r] = points.map(px);
    let eyes = ((left.0 + right.0) / 2.0, (left.1 + right.1) / 2.0);
    let mouth = ((mouth_l.0 + mouth_r.0) / 2.0, (mouth_l.1 + mouth_r.1) / 2.0);
    let along = (right.0 - left.0, right.1 - left.1);
    let eyes_px = along.0.hypot(along.1);
    let down = (mouth.0 - eyes.0, mouth.1 - eyes.1);
    let down_len = down.0.hypot(down.1);
    if eyes_px < 1.0 || down_len < 1.0 {
        return None;
    }
    let nose = (nose.0 - eyes.0, nose.1 - eyes.1);
    // Projections onto the eye line and the eyes-to-mouth line, so a rolled
    // head reads the same as an upright one.
    let yaw = (nose.0 * along.0 + nose.1 * along.1) / (eyes_px * eyes_px);
    let pitch = (nose.0 * down.0 + nose.1 * down.1) / (down_len * down_len);
    Some(Pose { yaw, pitch, eyes_px })
}

/// The user's own straight-on pose, and which way their first turn and tilt went.
#[derive(Debug, Default, Clone, Copy)]
pub struct Baseline {
    yaw: f32,
    pitch: f32,
    straight: usize,
    side: f32,
    tilt: f32,
}

impl Baseline {
    fn record(&mut self, step: Step, pose: Pose) {
        match step {
            Step::Straight => {
                let n = self.straight as f32;
                self.yaw = (self.yaw * n + pose.yaw) / (n + 1.0);
                self.pitch = (self.pitch * n + pose.pitch) / (n + 1.0);
                self.straight += 1;
            }
            Step::Side if self.side == 0.0 => self.side = (pose.yaw - self.yaw).signum(),
            Step::Up if self.tilt == 0.0 => self.tilt = (pose.pitch - self.pitch).signum(),
            _ => {}
        }
    }
}

/// Whether `pose` is what `step` asks for, or the hint that would get it there.
pub fn fit(step: Step, pose: Pose, base: &Baseline) -> Result<(), Hint> {
    if pose.eyes_px < MIN_EYES_PX {
        return Err(Hint::TooFar);
    }
    let turned = pose.yaw - base.yaw;
    let tilted = pose.pitch - base.pitch;
    let within = |v: f32, range: &std::ops::RangeInclusive<f32>, more: Hint, less: Hint| {
        if v.abs() < *range.start() {
            Err(more)
        } else if v.abs() > *range.end() {
            Err(less)
        } else {
            Ok(())
        }
    };
    match step {
        Step::Straight | Step::Glasses => {
            if pose.yaw.abs() > STRAIGHT_YAW || !STRAIGHT_PITCH.contains(&pose.pitch) {
                return Err(Hint::LookAtCamera);
            }
            Ok(())
        }
        Step::Side | Step::OtherSide => {
            let want = if step == Step::Side { base.side } else { -base.side };
            if want != 0.0 && turned.signum() != want && turned.abs() >= *TURN.start() {
                return Err(Hint::OtherWay);
            }
            within(turned, &TURN, Hint::TurnMore, Hint::TurnLess)
        }
        Step::Up | Step::Down => {
            if turned.abs() > TILT_YAW {
                return Err(Hint::LookAtCamera);
            }
            let want = if step == Step::Up { base.tilt } else { -base.tilt };
            if want != 0.0 && tilted.signum() != want && tilted.abs() >= *TILT.start() {
                return Err(Hint::OtherWay);
            }
            within(tilted, &TILT, Hint::TiltMore, Hint::TiltLess)
        }
    }
}

/// Report a hint once, not on every frame it still applies to.
fn say(last: &mut Option<Hint>, hint: Hint, progress: &mut dyn FnMut(GuidedProgress)) {
    if *last != Some(hint) {
        *last = Some(hint);
        progress(GuidedProgress::Hint(hint));
    }
}

/// Whether two poses are the same pose held.
pub fn steady(a: Pose, b: Pose) -> bool {
    (a.yaw - b.yaw).abs() < STEADY_YAW && (a.pitch - b.pitch).abs() < STEADY_PITCH
}

fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl FaceAuth {
    /// Run every step on `cam`. `control` is asked between frames; Skip ends
    /// the current step, Cancel the whole run with [`Cancelled`]. Fails if
    /// the straight-on step kept nothing, since every other step is measured
    /// from it.
    pub(crate) fn capture_guided(
        &mut self,
        cam: &mut Camera,
        progress: &mut dyn FnMut(GuidedProgress),
        control: &mut dyn FnMut() -> Option<Control>,
    ) -> Result<Vec<Vec<f32>>> {
        if self.detector.kind() != DetectorKind::Scrfd {
            bail!(
                "guided enrolment needs a detector with landmarks (SCRFD, det_500m.onnx); \
                 this one gives a box only"
            );
        }
        let mut kept: Vec<Vec<f32>> = Vec::new();
        let mut base = Baseline::default();
        for (i, step) in Step::ALL.into_iter().enumerate() {
            progress(GuidedProgress::Step { index: i + 1, total: Step::ALL.len(), step });
            let mut hint = None;
            let start = Instant::now();
            let mut skipped = false;
            if step == Step::Glasses {
                say(&mut hint, Hint::Waiting, progress);
                while start.elapsed() < GLASSES_PAUSE && !skipped {
                    match control() {
                        Some(Control::Cancel) => return Err(Cancelled.into()),
                        Some(Control::Skip) => skipped = true,
                        None => std::thread::sleep(PAUSE),
                    }
                }
            }
            let mut this_step = 0;
            let mut held: Option<Pose> = None;
            while this_step < step.wanted() && !skipped {
                match control() {
                    Some(Control::Cancel) => return Err(Cancelled.into()),
                    Some(Control::Skip) => break,
                    None => {}
                }
                if start.elapsed() > STEP_TIMEOUT {
                    break;
                }
                let frame = cam.capture_illuminated_frame(self.config.capture_timeout_ms())?;
                match self.guided_frame(frame, step, &base, &kept, &mut held)? {
                    Ok((embedding, pose)) => {
                        base.record(step, pose);
                        kept.push(embedding);
                        this_step += 1;
                        hint = None;
                        progress(GuidedProgress::Kept { kept: this_step, wanted: step.wanted() });
                    }
                    Err(h) => {
                        say(&mut hint, h, progress);
                        std::thread::sleep(PAUSE);
                    }
                }
            }
            if this_step == 0 {
                progress(GuidedProgress::Skipped(step));
                if step == Step::Straight {
                    bail!("no straight-on frame in {} s, so nothing else can be measured", STEP_TIMEOUT.as_secs());
                }
            }
        }
        Ok(kept)
    }

    /// One frame through the enrolment checks, then the step's pose and the
    /// duplicate check. The embedding and pose if it's kept, else the hint.
    fn guided_frame(
        &mut self,
        mut frame: IrFrame,
        step: Step,
        base: &Baseline,
        kept: &[Vec<f32>],
        held: &mut Option<Pose>,
    ) -> Result<std::result::Result<(Vec<f32>, Pose), Hint>> {
        if assess_frame(&frame) != FrameQuality::Ok {
            return Ok(Err(Hint::NoContent));
        }
        crate::preprocess::histogram_equalize(&mut frame);
        let Some(face) = self.detector.detect(&frame)? else {
            return Ok(Err(Hint::NoFace));
        };
        if face.is_smaller_than(self.config.min_face_size_ratio()) {
            return Ok(Err(Hint::TooFar));
        }
        let Some(pose) = pose(&face, frame.width, frame.height) else {
            return Ok(Err(Hint::NoFace));
        };
        if let Err(hint) = fit(step, pose, base) {
            *held = None;
            return Ok(Err(hint));
        }
        // The first frame in the pose only starts the count.
        match held.replace(pose) {
            Some(before) if steady(before, pose) => *held = None,
            _ => return Ok(Err(Hint::Hold)),
        }
        let Ok(input) = face_input(&frame, &face) else {
            return Ok(Err(Hint::NoFace));
        };
        let embedding = self.encoder.encode(input.view())?;
        if kept.iter().any(|k| similarity(k, &embedding) > DUPLICATE) {
            return Ok(Err(Hint::Same));
        }
        Ok(Ok((embedding, pose)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A face in a 360x360 frame: eyes 80 px apart at y=150, mouth at y=230,
    /// the nose moved `dx` along the eye line and `dy` towards the mouth.
    fn face(dx: f32, dy: f32) -> FaceBox {
        let n = |x: f32, y: f32| (x / 360.0, y / 360.0);
        FaceBox {
            x1: 0.2,
            y1: 0.2,
            x2: 0.8,
            y2: 0.8,
            landmarks: Some([
                n(140.0, 150.0),
                n(220.0, 150.0),
                n(180.0 + dx, 190.0 + dy),
                n(150.0, 230.0),
                n(210.0, 230.0),
            ]),
        }
    }

    fn at(dx: f32, dy: f32) -> Pose {
        pose(&face(dx, dy), 360, 360).unwrap()
    }

    #[test]
    fn pose_reads_the_landmarks() {
        let p = at(0.0, 0.0);
        assert!(p.yaw.abs() < 1e-6 && (p.pitch - 0.5).abs() < 1e-6 && (p.eyes_px - 80.0).abs() < 1e-3, "{p:?}");
        assert!((at(16.0, 0.0).yaw - 0.2).abs() < 1e-5);
        assert!((at(0.0, 8.0).pitch - 0.6).abs() < 1e-5);
        assert_eq!(pose(&FaceBox { landmarks: None, ..face(0.0, 0.0) }, 360, 360), None);
    }

    #[test]
    fn a_rolled_head_reads_as_upright() {
        // The same face turned 30 degrees in the image plane.
        let (s, c) = (30f32.to_radians().sin(), 30f32.to_radians().cos());
        let rot = |(x, y): (f32, f32)| {
            let (x, y) = (x * 360.0 - 180.0, y * 360.0 - 190.0);
            ((x * c - y * s + 180.0) / 360.0, (x * s + y * c + 190.0) / 360.0)
        };
        let mut rolled = face(16.0, 8.0);
        rolled.landmarks = rolled.landmarks.map(|p| p.map(rot));
        let p = pose(&rolled, 360, 360).unwrap();
        assert!((p.yaw - 0.2).abs() < 1e-4 && (p.pitch - 0.6).abs() < 1e-4, "{p:?}");
    }

    fn straight() -> Baseline {
        let mut base = Baseline::default();
        base.record(Step::Straight, at(0.0, 0.0));
        base
    }

    #[test]
    fn straight_on_means_the_nose_between_the_eyes() {
        let base = Baseline::default();
        assert_eq!(fit(Step::Straight, at(2.0, 3.0), &base), Ok(()));
        assert_eq!(fit(Step::Straight, at(16.0, 0.0), &base), Err(Hint::LookAtCamera));
        assert_eq!(fit(Step::Glasses, at(-16.0, 0.0), &base), Err(Hint::LookAtCamera));
        // A nose above the eyes or below the mouth isn't a face looking at the camera.
        assert_eq!(fit(Step::Straight, at(0.0, -32.0), &base), Err(Hint::LookAtCamera));
        assert_eq!(fit(Step::Straight, at(0.0, 32.0), &base), Err(Hint::LookAtCamera));
    }

    #[test]
    fn a_face_too_far_away_is_not_kept() {
        let small = Pose { eyes_px: MIN_EYES_PX - 1.0, ..at(0.0, 0.0) };
        assert_eq!(fit(Step::Straight, small, &Baseline::default()), Err(Hint::TooFar));
    }

    #[test]
    fn the_first_turn_sets_the_side_and_the_next_wants_the_other() {
        let mut base = straight();
        assert_eq!(fit(Step::Side, at(4.0, 0.0), &base), Err(Hint::TurnMore));
        assert_eq!(fit(Step::Side, at(40.0, 0.0), &base), Err(Hint::TurnLess));
        // Either way will do for the first.
        assert_eq!(fit(Step::Side, at(-16.0, 0.0), &base), Ok(()));
        assert_eq!(fit(Step::Side, at(16.0, 0.0), &base), Ok(()));
        base.record(Step::Side, at(-16.0, 0.0));
        assert_eq!(fit(Step::OtherSide, at(-16.0, 0.0), &base), Err(Hint::OtherWay));
        assert_eq!(fit(Step::OtherSide, at(16.0, 0.0), &base), Ok(()));
        // Later frames of the first side stay on it.
        assert_eq!(fit(Step::Side, at(16.0, 0.0), &base), Err(Hint::OtherWay));
    }

    #[test]
    fn tilts_are_measured_from_the_users_own_straight_pose() {
        // Someone whose camera sits below their eye line.
        let mut base = Baseline::default();
        base.record(Step::Straight, at(0.0, 6.0));
        assert_eq!(fit(Step::Up, at(0.0, 6.0), &base), Err(Hint::TiltMore));
        assert_eq!(fit(Step::Up, at(0.0, -2.0), &base), Ok(()));
        assert_eq!(fit(Step::Up, at(0.0, -30.0), &base), Err(Hint::TiltLess));
        assert_eq!(fit(Step::Up, at(16.0, -2.0), &base), Err(Hint::LookAtCamera));
        base.record(Step::Up, at(0.0, -2.0));
        assert_eq!(fit(Step::Down, at(0.0, -2.0), &base), Err(Hint::OtherWay));
        assert_eq!(fit(Step::Down, at(0.0, 14.0), &base), Ok(()));
    }

    #[test]
    fn a_pose_counts_once_it_is_held() {
        assert!(steady(at(16.0, 0.0), at(20.0, 2.0)));
        assert!(!steady(at(16.0, 0.0), at(24.0, 0.0)));
        assert!(!steady(at(0.0, 0.0), at(0.0, 5.0)));
    }

    #[test]
    fn ids_round_trip_and_the_steps_keep_ten() {
        for step in Step::ALL {
            assert_eq!(Step::parse(step.id()), Some(step));
        }
        for hint in Hint::ALL {
            assert_eq!(Hint::parse(hint.id()), Some(hint));
        }
        assert_eq!(Step::ALL.iter().map(|s| s.wanted()).sum::<usize>(), 10);
    }
}
