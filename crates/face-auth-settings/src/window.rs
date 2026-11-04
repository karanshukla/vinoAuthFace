//! The window: six pages of rows over the two config files.
//!
//! Changes to `/etc/face-auth.toml` are for everyone on the computer and
//! need an administrator, so they wait for one Apply (one password prompt)
//! rather than saving on change as vino-ui's DESIGN.md has it; the bar that
//! holds Apply is one control row tall, and only there while something waits.
//! The user's own file saves as it changes, as usual.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Align, Layout, RichText};
use face_auth_core::config::{self as limits, SYSTEM_CONFIG_PATH};
use face_auth_core::{capture, config_write, FaceAuthConfig};
use vino_ui::a11y;
use vino_ui::icons::Icon;
use vino_ui::theme;
use vino_ui::widgets::{self, Button, ChipKind, Row, Tone};

use crate::apply;
use crate::enrol;
use crate::files::{self, ConfigFile};
use crate::keys::{self, Key};
use crate::models::{self, Check, Checks};

const TITLE: &str = "vinoAuthFace settings";
pub const APP_ID: &str = "io.github.karanshukla.vinoauthface.Settings";
const DEFAULT_MODEL: &str = "/usr/local/share/face-auth/w600k_mbf.onnx";
const DEFAULT_DETECTOR: &str = "/usr/local/share/face-auth/det_500m.onnx";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Face,
    Matching,
    Liveness,
    Camera,
    Models,
    Security,
    You,
}

impl Page {
    pub const ALL: [Page; 7] =
        [Page::Face, Page::Matching, Page::Liveness, Page::Camera, Page::Models, Page::Security, Page::You];

    pub fn parse(name: &str) -> Option<Page> {
        let name = name.to_ascii_lowercase();
        Page::ALL.into_iter().find(|page| page.title().to_ascii_lowercase() == name || page.slug() == name)
    }

    fn slug(self) -> &'static str {
        match self {
            Page::Face => "face",
            Page::Matching => "matching",
            Page::Liveness => "liveness",
            Page::Camera => "camera",
            Page::Models => "models",
            Page::Security => "security",
            Page::You => "you",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Face => "Face",
            Page::Matching => "Matching",
            Page::Liveness => "Liveness",
            Page::Camera => "Camera",
            Page::Models => "Models",
            Page::Security => "Security",
            Page::You => "Just you",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Page::Face => Icon::Face,
            Page::Matching => Icon::Sliders,
            Page::Liveness => Icon::Motion,
            Page::Camera => Icon::Hardware,
            Page::Models => Icon::Model,
            Page::Security => Icon::Lock,
            Page::You => Icon::User,
        }
    }
}

/// How a number is shown, and written.
#[derive(Clone, Copy)]
enum Unit {
    /// A fraction, to this many decimals.
    Ratio(usize),
    /// Stored in milliseconds, shown in seconds.
    Seconds,
    Count,
    Minutes,
}

impl Unit {
    fn step(self) -> f64 {
        match self {
            Unit::Ratio(decimals) => 10f64.powi(-(decimals as i32)),
            Unit::Seconds => 0.1,
            Unit::Count | Unit::Minutes => 1.0,
        }
    }

    fn literal(self, v: f64) -> String {
        match self {
            Unit::Ratio(decimals) => {
                let scale = 10f64.powi(decimals as i32);
                keys::float(((v * scale).round() / scale) as f32)
            }
            Unit::Seconds => keys::int((v * 1000.0).round().max(0.0) as u64),
            Unit::Count | Unit::Minutes => keys::int(v.round().max(0.0) as u64),
        }
    }

    fn show(self, v: f64) -> String {
        match self {
            Unit::Ratio(decimals) => format!("{v:.decimals$}"),
            Unit::Seconds => format!("{} s", trim(v, 1)),
            Unit::Count => format!("{v:.0}"),
            Unit::Minutes => format!("{v:.0} min"),
        }
    }
}

fn trim(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

fn secs(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

fn range_secs(r: RangeInclusive<u64>) -> RangeInclusive<f64> {
    secs(*r.start())..=secs(*r.end())
}

fn range_f64(r: RangeInclusive<f32>) -> RangeInclusive<f64> {
    f64::from(*r.start())..=f64::from(*r.end())
}

/// One number setting.
struct Num<'a> {
    title: &'a str,
    help: String,
    tone: Tone,
    value: f64,
    /// What not setting it means: the built-in default, or for the user's
    /// file, this computer's value.
    default: f64,
    range: RangeInclusive<f64>,
    unit: Unit,
    log: bool,
}

pub struct SettingsWindow {
    pub(crate) page: Page,
    pub(crate) system: ConfigFile,
    pub(crate) user: ConfigFile,
    user_dirty: bool,
    pub(crate) apply: Arc<Mutex<apply::State>>,
    runner: apply::Runner,
    helper: bool,
    checks: Checks,
    /// Every video node: IR sensors first, in auto-detect's order, then
    /// other cameras, then virtual ones.
    cameras: Vec<Cam>,
    /// The IR sensors alone, best first: what auto-detect would pick, and all
    /// a user's own file may name.
    ir_cameras: Vec<String>,
    notice: Option<(Tone, String)>,
    /// Path fields being typed in, by file and key.
    drafts: HashMap<(bool, Key), String>,
    pub(crate) enrol: enrol::Shared,
    pub(crate) spawner: enrol::Spawner,
    /// Whether you have templates; `None` when that can't be read.
    pub(crate) enrolled: Option<bool>,
    pub(crate) check_enrolled: fn() -> Option<bool>,
    /// "Enrol again" waiting for its confirmation.
    confirm_replace: bool,
    focused: bool,
    kde_header: Option<theme::KdeHeader>,
    text_scale: f32,
}

impl SettingsWindow {
    fn new(ctx: &egui::Context, page: Option<Page>) -> Self {
        let mut window = SettingsWindow::with(
            page,
            PathBuf::from(SYSTEM_CONFIG_PATH),
            files::user_path(),
            apply::pkexec,
            apply::helper_installed(),
        );
        window.ir_cameras = capture::enumerate_ir_cameras().into_iter().map(|(path, _)| path).collect();
        window.cameras = cameras(&window.ir_cameras);
        window.kde_header = theme::kde_header();
        window.enrolled = enrol::enrolled();
        window.follow_text_scale(ctx);
        window
    }

    pub(crate) fn with(
        page: Option<Page>,
        system: PathBuf,
        user: PathBuf,
        runner: apply::Runner,
        helper: bool,
    ) -> Self {
        SettingsWindow {
            page: page.unwrap_or(Page::Face),
            system: ConfigFile::load(system),
            user: ConfigFile::load(user),
            user_dirty: false,
            apply: Arc::default(),
            runner,
            helper,
            checks: Checks::default(),
            cameras: Vec::new(),
            ir_cameras: Vec::new(),
            notice: None,
            drafts: HashMap::new(),
            enrol: enrol::Shared::default(),
            spawner: enrol::pkexec,
            enrolled: None,
            check_enrolled: enrol::enrolled,
            confirm_replace: false,
            focused: false,
            kde_header: None,
            text_scale: 1.0,
        }
    }

    fn follow_text_scale(&mut self, ctx: &egui::Context) {
        let scale = theme::text_scale();
        if scale != self.text_scale {
            self.text_scale = scale;
            ctx.set_zoom_factor(scale);
        }
    }

    fn file(&mut self, user: bool) -> &mut ConfigFile {
        if user {
            &mut self.user
        } else {
            &mut self.system
        }
    }

    /// Set a key in one of the files; the user's is saved once input settles.
    fn set(&mut self, user: bool, key: Key, value: Option<String>) {
        self.file(user).set(key, value);
        if user {
            self.user_dirty = true;
        }
    }

    // Rows

    fn number(&mut self, ui: &mut egui::Ui, user: bool, key: Key, n: Num<'_>) {
        let set = self.file(user).is_set(key);
        // Rounded to the step first: an f32 widened to f64 (0.6 is
        // 0.60000002) would otherwise snap to the step and read as a change.
        let step = n.unit.step();
        // And inside the range: a value of the user's below this computer's
        // floor would be clamped by the slider, and written back unasked.
        let before = ((n.value / step).round() * step).clamp(*n.range.start(), *n.range.end());
        let mut v = before;
        let mut out = None;
        Row::new(n.title).help(&n.help).tone(n.tone).show(ui, |ui, named| {
            // Only while it says something the default doesn't.
            let differs = (before - n.default).abs() >= step / 2.0;
            if set && differs && a11y::for_what(ui.add(Button::ghost("Default").small()), n.title).clicked() {
                out = Some(None);
            }
            let response = slider(ui, named, &mut v, n.range.clone(), n.unit, n.log);
            if response.changed() && (v - before).abs() >= step / 2.0 {
                let same = (v - n.default).abs() < step / 2.0;
                out = Some((!same).then(|| n.unit.literal(v)));
            }
        });
        if let Some(value) = out {
            self.set(user, key, value);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn switch(&mut self, ui: &mut egui::Ui, user: bool, key: Key, title: &str, help: &str, value: bool, default: bool) {
        let mut on = value;
        let mut changed = false;
        Row::new(title).help(help).show(ui, |ui, _| {
            changed = widgets::switch(ui, &mut on, title).changed();
        });
        if changed {
            self.set(user, key, (on != default).then(|| keys::boolean(on)));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn choice(
        &mut self,
        ui: &mut egui::Ui,
        key: Key,
        title: &str,
        help: &str,
        tone: Tone,
        current: &str,
        default: &str,
        options: &[(&'static str, &str)],
        enabled: bool,
    ) {
        let mut picked: &'static str = options.iter().find(|(v, _)| *v == current).map_or("", |(v, _)| v);
        let mut changed = false;
        Row::new(title).help(help).tone(tone).show(ui, |ui, _| {
            ui.add_enabled_ui(enabled, |ui| {
                changed = widgets::segmented(ui, title, &mut picked, options);
            });
        });
        if changed {
            self.set(false, key, (picked != default).then(|| keys::string(picked)));
        }
    }

    /// A path typed in a field, set when the field loses focus. Empty means
    /// the default. `check` adds a button that loads the model.
    #[allow(clippy::too_many_arguments)]
    fn path(
        &mut self,
        ui: &mut egui::Ui,
        key: Key,
        title: &str,
        help: &str,
        default: &str,
        dir: bool,
        check: Option<models::Kind>,
    ) {
        let current = self.system.raw(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let effective = if current.is_empty() { default.to_string() } else { current.clone() };
        let was = self.system.read.as_deref().and_then(|t| toml::from_str::<toml::Table>(t).ok());
        let was = was.as_ref().and_then(|t| key.value(t)).and_then(|v| v.as_str()).map(str::to_string);

        let mut help = help.to_string();
        let mut tone = Tone::Neutral;
        if !current.is_empty() && Some(&current) != was.as_ref() {
            if let Err(e) = config_write::trusted_path(Path::new(&current), dir) {
                help = format!("Apply will refuse this: {e}.");
                tone = Tone::Warn;
            }
        } else if !Path::new(&effective).exists() {
            help = format!("{effective} isn't there.");
            tone = Tone::Warn;
        }
        let result = check.and_then(|kind| self.checks.lock().unwrap().get(&(kind, effective.clone())).cloned());
        match &result {
            Some(Check::Done(Ok(text))) => help = format!("{help} {text}"),
            Some(Check::Done(Err(e))) => {
                help = format!("Doesn't load: {e}");
                tone = Tone::Error;
            }
            _ => {}
        }

        let draft_key = (false, key);
        let mut draft = self.drafts.remove(&draft_key).unwrap_or_else(|| current.clone());
        let mut commit = false;
        let mut editing = false;
        let mut run_check = false;
        Row::new(title).help(&help).tone(tone).show(ui, |ui, named| {
            if check.is_some() {
                let running = result == Some(Check::Running);
                let label = if running { "Checking…" } else { "Check" };
                let button = ui.add_enabled(!running, Button::secondary(label).small());
                run_check = a11y::for_what(button, title).clicked();
            }
            let width = ui.available_width().min(320.0);
            let field = named(ui.add(widgets::field(&mut draft, default, width)));
            commit = field.lost_focus();
            editing = field.has_focus();
            if !draft.is_empty() {
                field.on_hover_text(draft.clone());
            }
        });
        if run_check {
            if let Some(kind) = check {
                models::start(&self.checks, kind, effective.clone(), ui.ctx());
            }
        }
        if commit {
            let typed = draft.trim().to_string();
            if typed != current {
                let value = (!typed.is_empty() && typed != default).then(|| keys::string(&typed));
                self.set(false, key, value);
            }
        } else if editing {
            self.drafts.insert(draft_key, draft);
        }
    }

    /// The camera, from the devices there are. `ir_only` for the user's
    /// file, which may only name a real IR sensor.
    fn camera(&mut self, ui: &mut egui::Ui, user: bool) {
        let file = if user { &self.user } else { &self.system };
        let current = file.raw(keys::DEVICE).and_then(|v| v.as_str()).map(str::to_string);
        let cam = |path: &str| self.cameras.iter().find(|c| c.path == path);
        let label = |path: &str| match cam(path) {
            Some(c) if c.ir => format!("{} ({path})", c.name),
            Some(c) if c.virtual_ => format!("{} ({path}), virtual", c.name),
            Some(c) => format!("{} ({path}), not IR", c.name),
            None => format!("{path} (not here)"),
        };
        let choices: Vec<String> = if user {
            self.ir_cameras.clone()
        } else {
            self.cameras.iter().map(|c| c.path.clone()).collect()
        };
        let mut choices: Vec<Option<String>> = std::iter::once(None).chain(choices.into_iter().map(Some)).collect();
        if current.as_ref().is_some_and(|c| !choices.contains(&Some(c.clone()))) {
            choices.push(current.clone());
        }
        // Auto-detect says what it would pick, so it isn't a mystery choice.
        let auto = match (user, self.ir_cameras.first()) {
            (true, _) => "This computer's camera".to_string(),
            (false, Some(best)) => format!("Auto-detect: {}", label(best)),
            (false, None) => "Auto-detect: no IR sensor found".to_string(),
        };
        let shown = current.as_deref().map_or_else(|| auto.clone(), label);
        let (help, tone) = match (&current, user) {
            (Some(path), true) if !self.ir_cameras.contains(path) => {
                ("Ignored: not an IR sensor on this computer.".to_string(), Tone::Warn)
            }
            (Some(path), false) if cam(path).is_none() => {
                (format!("{path} isn't here, so face unlock finds no camera."), Tone::Warn)
            }
            (Some(path), false) if cam(path).is_some_and(|c| c.virtual_) => (
                "A virtual camera: any program can feed it frames, a photo included. For testing only.".to_string(),
                Tone::Warn,
            ),
            (Some(path), false) if cam(path).is_some_and(|c| !c.ir) => {
                ("Not an IR sensor: face unlock needs greyscale IR frames.".to_string(), Tone::Warn)
            }
            (None, false) => (
                "The first real IR sensor. Never a virtual camera.".to_string(),
                Tone::Neutral,
            ),
            (None, true) => ("This computer's camera.".to_string(), Tone::Neutral),
            _ => ("Set by hand. For testing, or two IR sensors.".to_string(), Tone::Neutral),
        };
        let mut picked = current.clone();
        Row::new("IR camera").help(&help).tone(tone).show(ui, |ui, named| {
            let width = ui.available_width().min(320.0);
            let combo = widgets::combo(ui, if user { "user-camera" } else { "camera" }, shown, width).show_ui(ui, |ui| {
                for choice in &choices {
                    let text = choice.as_deref().map_or_else(|| auto.clone(), label);
                    ui.selectable_value(&mut picked, choice.clone(), text);
                }
            });
            named(combo.response);
        });
        if picked != current {
            self.set(user, keys::DEVICE, picked.as_deref().map(keys::string));
        }
    }

    // Pages

    /// Guided enrolment. The steps, and why they help a model, are in
    /// `face_auth_core::guided`.
    fn face(&mut self, ui: &mut egui::Ui) {
        let run = {
            let run = self.enrol.lock().unwrap();
            (run.running, run.step, run.kept, run.hint)
        };
        let (running, step, kept, hint) = run;
        widgets::group_label(ui, "Guided enrolment");
        widgets::group_help(
            ui,
            "About ten templates from measured poses: straight, slight turns and tilts, glasses off and on.",
        );
        if running {
            let mut skip = false;
            let mut stop = false;
            widgets::group(ui, |ui| {
                let (title, progress) = match step {
                    Some((n, total, step)) => (
                        step.prompt(),
                        format!("Step {n} of {total}, {} of {} kept.", kept.0, kept.1),
                    ),
                    None => ("Waiting for the password prompt…", String::new()),
                };
                Row::new(title).help(&progress).show(ui, |ui, _| {
                    if step.is_some() {
                        stop = ui.add(Button::secondary("Stop")).clicked();
                        skip = a11y::for_what(ui.add(Button::ghost("Skip")), "this step").clicked();
                    }
                });
                widgets::group_section(ui, |ui| {
                    let text = hint.map_or("", |h| h.text());
                    a11y::live(&widgets::chip(ui, ChipKind::Neutral, text));
                });
            });
            if skip {
                enrol::send(&self.enrol, false);
            }
            if stop {
                enrol::send(&self.enrol, true);
            }
            return;
        }
        let mut start = None;
        widgets::group(ui, |ui| {
            let (kind, text) = match self.enrolled {
                Some(true) => (ChipKind::Ok, "Enrolled"),
                Some(false) => (ChipKind::Warn, "Not enrolled"),
                None => (ChipKind::Neutral, "Unknown"),
            };
            Row::new("Your face").show(ui, |ui, _| {
                a11y::live(&widgets::chip(ui, kind, text));
            });
            let enrolled = self.enrolled == Some(true);
            if self.confirm_replace {
                let p = theme::palette(ui.ctx());
                Row::new("Enrol again")
                    .help("Your current templates will be replaced.")
                    .tone(Tone::Error)
                    .tint(p.error_tint)
                    .show(ui, |ui, _| {
                        if ui.add(Button::ghost("Cancel")).clicked() {
                            self.confirm_replace = false;
                        }
                        if ui.add(Button::destructive("Replace")).clicked() {
                            self.confirm_replace = false;
                            start = Some(false);
                        }
                    });
            } else if enrolled {
                Row::new("Enrol again").help("Replaces your templates.").show(ui, |ui, _| {
                    if ui.add(Button::secondary("Enrol again…")).clicked() {
                        self.confirm_replace = true;
                    }
                });
            } else {
                Row::new("Enrol").help("About two minutes.").show(ui, |ui, _| {
                    if ui.add(Button::primary("Start")).clicked() {
                        start = Some(false);
                    }
                });
            }
            Row::new("Add poses")
                .help("The same steps, kept alongside your templates.")
                .show(ui, |ui, _| {
                    if ui.add_enabled(enrolled && !self.confirm_replace, Button::secondary("Add")).clicked() {
                        start = Some(true);
                    }
                });
        });
        ui.add_space(8.0);
        widgets::small(
            ui,
            "Needs an administrator's password. No camera preview: frames stay in the root process.",
        );
        if let Some(append) = start {
            self.notice = None;
            enrol::start(&self.enrol, self.spawner, append, ui.ctx());
        }
    }

    fn matching(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        widgets::group_label(ui, "Matching");
        widgets::group(ui, |ui| {
            self.number(ui, false, keys::THRESHOLD, Num {
                title: "Match threshold",
                help: "How alike a face must be to yours. Higher is stricter."
                    .into(),
                tone: Tone::Neutral,
                value: c.threshold().into(),
                default: 0.6,
                range: range_f64(limits::THRESHOLD_RANGE),
                unit: Unit::Ratio(2),
                log: false,
            });
            self.number(ui, false, keys::DETECTOR_THRESHOLD, Num {
                title: "Face detection",
                help: "How sure the detector must be that it sees a face.".into(),
                tone: Tone::Neutral,
                value: c.detector_threshold().into(),
                default: 0.5,
                range: range_f64(limits::DETECTOR_THRESHOLD_RANGE),
                unit: Unit::Ratio(2),
                log: false,
            });
            self.number(ui, false, keys::MIN_FACE_SIZE_RATIO, Num {
                title: "Smallest face",
                help: "Share of the frame. Smaller faces are ignored; 0 is off.".into(),
                tone: Tone::Neutral,
                value: c.min_face_size_ratio().into(),
                default: 0.0,
                range: range_f64(limits::MIN_FACE_SIZE_RANGE),
                unit: Unit::Ratio(2),
                log: false,
            });
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Scan");
        widgets::group(ui, |ui| self.timing(ui, false, &c));
    }

    /// The scan timing rows, for either file: they're the same keys and
    /// bounds, and the user's are honoured as they are.
    fn timing(&mut self, ui: &mut egui::Ui, user: bool, c: &FaceAuthConfig) {
        let sys = self.system.values();
        let def = |user_default: u64, built_in: u64| secs(if user { user_default } else { built_in });
        let unset: Vec<Key> = [keys::SCAN_DURATION_MS, keys::LIVENESS_GRACE_MS, keys::CAPTURE_TIMEOUT_MS, keys::SCAN_INTERVAL_MS]
            .into_iter()
            .filter(|key| user && !self.user.is_set(*key))
            .collect();
        let inherits = |key: Key, shown: String| {
            if unset.contains(&key) {
                format!(" This computer: {shown}.")
            } else {
                String::new()
            }
        };
        let help = format!(
            "How long to look before the password.{}",
            inherits(keys::SCAN_DURATION_MS, Unit::Seconds.show(secs(sys.scan_duration_ms())))
        );
        self.number(ui, user, keys::SCAN_DURATION_MS, Num {
            title: "Scan time",
            help,
            tone: Tone::Neutral,
            value: secs(c.scan_duration_ms()),
            default: def(sys.scan_duration_ms(), 5000),
            range: range_secs(limits::SCAN_DURATION_RANGE),
            unit: Unit::Seconds,
            log: false,
        });
        let help = format!(
            "Extra wait, once, for a match that hasn't moved yet.{}",
            inherits(keys::LIVENESS_GRACE_MS, Unit::Seconds.show(secs(sys.liveness_grace_ms())))
        );
        self.number(ui, user, keys::LIVENESS_GRACE_MS, Num {
            title: "Extra time to move",
            help,
            tone: Tone::Neutral,
            value: secs(c.liveness_grace_ms()),
            default: def(sys.liveness_grace_ms(), 4000),
            range: range_secs(limits::LIVENESS_GRACE_RANGE),
            unit: Unit::Seconds,
            log: false,
        });
        let help = format!(
            "How long to wait for a frame.{}",
            inherits(keys::CAPTURE_TIMEOUT_MS, Unit::Seconds.show(secs(sys.capture_timeout_ms() as u64)))
        );
        self.number(ui, user, keys::CAPTURE_TIMEOUT_MS, Num {
            title: "Camera timeout",
            help,
            tone: Tone::Neutral,
            value: secs(c.capture_timeout_ms() as u64),
            default: def(sys.capture_timeout_ms() as u64, 5000),
            range: range_secs(limits::CAPTURE_TIMEOUT_RANGE),
            unit: Unit::Seconds,
            log: false,
        });
        let help = format!(
            "Saves CPU, costs speed. 0 suits nearly everyone.{}",
            inherits(keys::SCAN_INTERVAL_MS, Unit::Seconds.show(secs(sys.scan_interval_ms())))
        );
        self.number(ui, user, keys::SCAN_INTERVAL_MS, Num {
            title: "Pause between frames",
            help,
            tone: Tone::Neutral,
            value: secs(c.scan_interval_ms()),
            default: def(sys.scan_interval_ms(), 0),
            range: range_secs(limits::SCAN_INTERVAL_RANGE),
            unit: Unit::Seconds,
            log: false,
        });
    }

    fn liveness(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        let motion = c.liveness_motion_threshold();
        let residual = c.liveness_residual_motion_threshold();
        let preset = match (motion, residual) {
            (m, r) if m == 0.0 && r == 0.0 => "off",
            (m, r) if m == 0.01 && r == 0.0 => "standard",
            (m, r) if m == 0.01 && r == 0.3 => "strict",
            _ => "",
        };
        let (help, tone) = match preset {
            "off" => ("Off: a photo or video of you can unlock this computer.", Tone::Warn),
            "" => ("Custom: set by hand below.", Tone::Neutral),
            _ => ("Strict also stops a photo moved by hand, but may fail if you sit very still.", Tone::Neutral),
        };
        widgets::group_label(ui, "Motion check");
        widgets::group_help(
            ui,
            "A match counts only once your face has moved a little, which a photo can't.",
        );
        widgets::group(ui, |ui| {
            let mut picked = preset;
            let mut changed = false;
            Row::new("Preset").help(help).tone(tone).show(ui, |ui, _| {
                changed = widgets::segmented(
                    ui,
                    "Preset",
                    &mut picked,
                    &[("off", "Off"), ("standard", "Standard"), ("strict", "Strict")],
                );
            });
            if changed {
                // The preset only fills values that aren't set by hand.
                self.set(false, keys::LIVENESS_MOTION, None);
                self.set(false, keys::LIVENESS_RESIDUAL, None);
                self.set(false, keys::LIVENESS_PRESET, (picked != "standard").then(|| keys::string(picked)));
            }
            self.number(ui, false, keys::LIVENESS_MOTION, Num {
                title: "Face motion",
                help: "Share of the face that must change. Stops a photo held still; 0 is off."
                    .into(),
                tone: Tone::Neutral,
                value: motion.into(),
                default: 0.01,
                range: range_f64(limits::LIVENESS_MOTION_RANGE),
                unit: Unit::Ratio(3),
                log: true,
            });
            self.number(ui, false, keys::LIVENESS_RESIDUAL, Num {
                title: "Movement in parts",
                help: "Motion a moved photo can't make, like a blink. 0 is off; try 0.3.".into(),
                tone: Tone::Neutral,
                value: residual.into(),
                default: 0.0,
                range: range_f64(limits::LIVENESS_MOTION_RANGE),
                unit: Unit::Ratio(2),
                log: false,
            });
            self.number(ui, false, keys::LIVENESS_WINDOW_MS, Num {
                title: "Look back",
                help: "How far back motion is measured. 0 compares frame to frame.".into(),
                tone: Tone::Neutral,
                value: secs(c.liveness_window_ms()),
                default: 1.0,
                range: range_secs(limits::LIVENESS_WINDOW_RANGE),
                unit: Unit::Seconds,
                log: false,
            });
        });
    }

    fn camera_page(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        widgets::group_label(ui, "Camera");
        widgets::group(ui, |ui| {
            self.camera(ui, false);
            self.switch(
                ui,
                false,
                keys::BIND_CAMERA,
                "Only cameras you enrolled on",
                "Refuses a different camera, by USB ID.",
                c.bind_camera(),
                true,
            );
            let pin = (self.system.raw(keys::PINNED_CAMERA_PATH), self.system.raw(keys::PINNED_CAMERA_INDEX));
            match pin {
                (Some(path), index) => {
                    let help = format!(
                        "{}, node {}. Set by pin-camera.sh; frames from anywhere else are refused.",
                        path.as_str().unwrap_or("?"),
                        index.and_then(|i| i.as_integer()).unwrap_or(0)
                    );
                    let mut remove = false;
                    Row::new("Pinned to a USB port").help(&help).show(ui, |ui, _| {
                        remove = a11y::for_what(ui.add(Button::secondary("Remove pin")), "the camera pin").clicked();
                    });
                    if remove {
                        self.set(false, keys::PINNED_CAMERA_PATH, None);
                        self.set(false, keys::PINNED_CAMERA_INDEX, None);
                    }
                }
                _ => {
                    Row::new("Pinned to a USB port")
                        .help("No. sudo ./pin-camera.sh pins it.")
                        .show(ui, |_, _| {});
                }
            }
        });
    }

    fn models(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        let was = self.system.read.as_deref().and_then(|t| FaceAuthConfig::from_toml(t).ok());
        widgets::group_label(ui, "Recognition");
        widgets::group_help(
            ui,
            "Only files root owns. To add one: sudo install -m 644 model.onnx /usr/local/share/face-auth/",
        );
        widgets::group(ui, |ui| {
            self.path(
                ui,
                keys::MODEL_PATH,
                "Recognition model",
                "Turns a face into values to compare.",
                DEFAULT_MODEL,
                false,
                Some(models::Kind::Encoder),
            );
            self.path(
                ui,
                keys::DETECTOR_MODEL_PATH,
                "Detector model",
                "Finds the face in the frame.",
                DEFAULT_DETECTOR,
                false,
                Some(models::Kind::Detector),
            );
            let tag = c.model_tag();
            let old = was.as_ref().map(FaceAuthConfig::model_tag).unwrap_or_else(|| tag.clone());
            if tag == old {
                Row::new("Templates")
                    .help(&format!("Made with {tag}. Other models' templates are refused."))
                    .show(ui, |_, _| {});
            } else {
                Row::new("Templates")
                    .help(&format!(
                        "{old} becomes {tag}: everyone enrols again after Apply."
                    ))
                    .tone(Tone::Warn)
                    .show(ui, |_, _| {});
            }
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Inference");
        widgets::group(ui, |ui| {
            let backend = c.backend();
            self.choice(
                ui,
                keys::BACKEND,
                "Backend",
                "OpenVINO needs an npu build; deploy.sh makes one when it finds OpenVINO.",
                Tone::Neutral,
                &backend,
                "tract",
                &[("tract", "Tract (CPU)"), ("openvino", "OpenVINO")],
                true,
            );
            self.choice(
                ui,
                keys::NPU_DEVICE,
                "OpenVINO device",
                "Where OpenVINO runs the models.",
                Tone::Neutral,
                &c.npu_device(),
                "NPU",
                &[("NPU", "NPU"), ("GPU", "GPU"), ("CPU", "CPU")],
                backend == "openvino",
            );
        });
        ui.add_space(8.0);
        widgets::small(ui, "Check always uses tract on the CPU.");
    }

    fn security(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        widgets::group_label(ui, "When a scan runs");
        widgets::group(ui, |ui| {
            self.switch(
                ui,
                false,
                keys::ABORT_IF_SSH,
                "Skip over SSH",
                "Straight to the password for sudo over SSH.",
                c.abort_if_ssh(),
                true,
            );
            self.switch(
                ui,
                false,
                keys::ABORT_IF_LID_CLOSED,
                "Skip with the lid closed",
                "Leave off with an external camera.",
                c.abort_if_lid_closed(),
                false,
            );
            self.switch(
                ui,
                false,
                keys::SEAT_CHECK,
                "Only the person at the screen",
                "Only the account using the screen. Also refuses root as the target.",
                c.seat_check(),
                false,
            );
            self.switch(
                ui,
                false,
                keys::REQUIRE_CONFIRMATION,
                "Confirm sudo with Enter",
                "After a match in a terminal, press Enter to let sudo through.",
                c.require_confirmation_elevation.unwrap_or(false),
                false,
            );
            self.number(ui, false, keys::START_DELAY_MS, Num {
                title: "Wait before scanning",
                help: "So locking the screen doesn't let you straight back in. 0 is off."
                    .into(),
                tone: Tone::Neutral,
                value: secs(c.start_delay_ms()),
                default: 2.0,
                range: range_secs(limits::START_DELAY_RANGE),
                unit: Unit::Seconds,
                log: false,
            });
            self.choice(
                ui,
                keys::START_DELAY_SCOPE,
                "Wait for",
                "Everything also delays sudo, polkit and login.",
                Tone::Neutral,
                &c.start_delay_scope(),
                "screen_lock",
                &[("screen_lock", "Lock screen"), ("all", "Everything")],
                true,
            );
        });
        ui.add_space(20.0);
        let lockout = c.lockout_policy();
        widgets::group_label(ui, "After failed scans");
        widgets::group_help(ui, "Face unlock pauses; the password always works.");
        widgets::group(ui, |ui| {
            self.number(ui, false, keys::LOCKOUT_THRESHOLD, Num {
                title: "Failures allowed",
                help: "Non-matching scans before the first pause.".into(),
                tone: Tone::Neutral,
                value: lockout.threshold.into(),
                default: 5.0,
                range: 1.0..=50.0,
                unit: Unit::Count,
                log: false,
            });
            self.number(ui, false, keys::LOCKOUT_BASE_DELAY_MS, Num {
                title: "First pause",
                help: "Doubles after each failure.".into(),
                tone: Tone::Neutral,
                value: secs(lockout.base_delay_ms),
                default: 2.0,
                range: 0.0..=60.0,
                unit: Unit::Seconds,
                log: false,
            });
            self.number(ui, false, keys::LOCKOUT_MAX_DELAY_MS, Num {
                title: "Longest pause",
                help: "The doubling stops here.".into(),
                tone: Tone::Neutral,
                value: secs(lockout.max_delay_ms),
                default: 300.0,
                range: 0.0..=3600.0,
                unit: Unit::Seconds,
                log: true,
            });
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Templates");
        widgets::group(ui, |ui| {
            self.switch(
                ui,
                false,
                keys::SEAL_EMBEDDINGS,
                "Seal to the TPM",
                "Protects a stolen disk, not a running system. From the next enrol.",
                c.seal_embeddings(),
                false,
            );
            self.path(
                ui,
                keys::EMBEDDINGS_DIR,
                "Template folder",
                "Must be root's: whoever can write here picks whose face unlocks.",
                limits::DEFAULT_EMBEDDINGS_DIR,
                true,
                None,
            );
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Network");
        widgets::group(ui, |ui| {
            self.switch(
                ui,
                false,
                keys::UPDATE_CHECK,
                "Check for updates",
                "Once a day, from doctor and the tray. Never part of unlocking.",
                c.update_check(),
                true,
            );
        });
    }

    /// The user's own file: stricter values only, saved as they change.
    fn you(&mut self, ui: &mut egui::Ui) {
        let sys = self.system.values();
        let mine = self.user.values();
        if let Err(e) = &self.user.config {
            if self.user.read.is_some() {
                ui.add_space(10.0);
                widgets::banner(ui, Tone::Error, &format!("Your file doesn't parse, so it's ignored: {e}"), |_| {});
            }
        }
        widgets::group_label(ui, "Stricter for you");
        widgets::group_help(
            ui,
            "Saved to ~/.config/face-auth.toml as you go. Looser values than this computer's are ignored.",
        );
        widgets::group(ui, |ui| {
            let floors = [
                (keys::THRESHOLD, "Match threshold", sys.threshold(), mine.threshold, limits::THRESHOLD_RANGE, 2),
                (
                    keys::DETECTOR_THRESHOLD,
                    "Face detection",
                    sys.detector_threshold(),
                    mine.detector_threshold,
                    limits::DETECTOR_THRESHOLD_RANGE,
                    2,
                ),
                (
                    keys::MIN_FACE_SIZE_RATIO,
                    "Smallest face",
                    sys.min_face_size_ratio(),
                    mine.min_face_size_ratio,
                    limits::MIN_FACE_SIZE_RANGE,
                    2,
                ),
                (
                    keys::LIVENESS_MOTION,
                    "Face motion",
                    sys.liveness_motion_threshold(),
                    mine.liveness_motion_threshold,
                    limits::LIVENESS_MOTION_RANGE,
                    3,
                ),
                (
                    keys::LIVENESS_RESIDUAL,
                    "Movement in parts",
                    sys.liveness_residual_motion_threshold(),
                    mine.liveness_residual_motion_threshold,
                    limits::LIVENESS_MOTION_RANGE,
                    2,
                ),
            ];
            for (key, title, floor, mine, range, decimals) in floors {
                let shown = Unit::Ratio(decimals).show(floor.into());
                let (help, tone) = match mine {
                    Some(v) if v < floor => (format!("Ignored: looser than this computer's {shown}."), Tone::Warn),
                    Some(_) => (format!("This computer: {shown}."), Tone::Neutral),
                    None => (format!("This computer's {shown} applies."), Tone::Neutral),
                };
                self.number(ui, true, key, Num {
                    title,
                    help,
                    tone,
                    value: mine.unwrap_or(floor).into(),
                    default: floor.into(),
                    range: f64::from(floor)..=f64::from(*range.end()),
                    unit: Unit::Ratio(decimals),
                    log: key == keys::LIVENESS_MOTION,
                });
            }
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Timing for you");
        widgets::group(ui, |ui| {
            let effective = sys.clone().with_user_overlay(&mine);
            self.timing(ui, true, &effective);
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Camera and tray");
        widgets::group(ui, |ui| {
            self.camera(ui, true);
            self.number(ui, true, keys::TRAY_IDLE_MINUTES, Num {
                title: "Hide the tray icon after",
                help: "Minutes idle before it hides. 0 keeps it in view."
                    .into(),
                tone: Tone::Neutral,
                value: mine.tray_idle_minutes() as f64,
                default: 30.0,
                range: 0.0..=1440.0,
                unit: Unit::Minutes,
                log: false,
            });
        });
    }

    // The frame

    fn take_apply_result(&mut self) {
        let result = self.apply.lock().unwrap().result.take();
        match result {
            Some(Ok(_)) => {
                let tag_changed = {
                    let was = self.system.read.as_deref().and_then(|t| FaceAuthConfig::from_toml(t).ok());
                    was.map(|w| w.model_tag()) != self.system.config.as_ref().ok().map(FaceAuthConfig::model_tag)
                };
                self.system.reload();
                self.notice = Some(if tag_changed {
                    (Tone::Warn, "Saved. The models changed: enrol again before face unlock works.".into())
                } else {
                    (Tone::Info, "Saved.".into())
                });
            }
            Some(Err(e)) => self.notice = Some((Tone::Error, e)),
            None => {}
        }
    }

    fn apply_now(&mut self, ctx: &egui::Context) {
        let payload = config_write::frame(self.system.read.as_deref(), &self.system.text);
        apply::start(&self.apply, self.runner, payload, ctx);
    }

    fn status_chip(&self, ui: &mut egui::Ui) {
        let running = self.apply.lock().unwrap().running;
        let changes = self.system.changes().len();
        let shown = if running {
            widgets::chip(ui, ChipKind::Neutral, "Applying…")
        } else if self.system.unreadable.is_some() {
            widgets::chip(ui, ChipKind::Err, "Can't read the settings")
        } else if self.system.config.is_err() {
            widgets::chip(ui, ChipKind::Err, "Settings file broken")
        } else if changes > 0 {
            widgets::chip(ui, ChipKind::Warn, &plural(changes, "change not applied", "changes not applied"))
        } else {
            widgets::chip(ui, ChipKind::Ok, "Saved")
        };
        a11y::live(&shown);
    }

    fn apply_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.apply.lock().unwrap().running;
        let changes = self.system.changes().len();
        let valid = FaceAuthConfig::parse_system(&self.system.text);
        let (text, can) = if running {
            ("Waiting for the password prompt…".to_string(), false)
        } else if !self.helper {
            ("Can't apply: vinoauthface-helper isn't installed.".into(), false)
        } else if let Err(e) = &valid {
            (format!("Can't apply: {e:#}"), false)
        } else {
            (plural(changes, "change for everyone on this computer", "changes for everyone on this computer"), true)
        };
        let mut apply = false;
        let mut discard = false;
        ui.horizontal(|ui| {
            ui.set_min_height(widgets::CONTROL_HEIGHT);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                apply = ui.add_enabled(can, Button::primary("Apply…")).clicked();
                discard = ui.add_enabled(!running, Button::ghost("Discard")).clicked();
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(theme::palette(ui.ctx()).muted)).truncate());
                });
            });
        });
        if apply {
            self.notice = None;
            self.apply_now(ui.ctx());
        }
        if discard {
            self.system.discard();
            self.drafts.retain(|(user, _), _| *user);
        }
    }

    pub(crate) fn show(&mut self, root: &mut egui::Ui) {
        let ctx = &root.ctx().clone();
        let focused = ctx.input(|input| input.focused);
        if focused && !self.focused {
            theme::follow_desktop(ctx);
            self.kde_header = theme::kde_header();
            self.follow_text_scale(ctx);
            // Changed by hand, by the tray or by deploy.sh meanwhile.
            if self.system.changes().is_empty() && !self.apply.lock().unwrap().running {
                self.system.reload();
            }
            if !self.user_dirty {
                self.user.reload();
            }
        }
        self.focused = focused;
        self.take_apply_result();
        let enrolled = self.enrol.lock().unwrap().done.take();
        if let Some(result) = enrolled {
            self.notice = Some(match result {
                Ok(saved) => (Tone::Info, saved),
                Err(e) => (Tone::Error, e),
            });
            self.enrolled = (self.check_enrolled)();
        }

        ctx.accesskit_node_builder(egui::accesskit_root_id(), |node| node.set_label(TITLE));

        let p = theme::palette(ctx);
        let header = theme::header(ctx, self.kde_header.as_ref(), focused);
        let wide = ctx.content_rect().width() >= widgets::SIDEBAR_FROM;

        if wide {
            egui::Panel::left("pages")
                .resizable(false)
                .show_separator_line(false)
                .exact_size(widgets::SIDEBAR_WIDTH)
                .frame(widgets::sidebar_frame(p))
                .show(root, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new("vinoAuthFace").size(15.0).family(theme::bold()));
                            widgets::small(ui, "Settings");
                        });
                    });
                    ui.add_space(14.0);
                    a11y::tab_list(ui, "Pages");
                    for page in Page::ALL {
                        // Your own settings stand apart from the computer's, under a hairline.
                        if page == Page::You {
                            ui.add_space(8.0);
                            let rect = ui.available_rect_before_wrap();
                            let y = rect.top() + 0.5;
                            ui.painter().hline((rect.left() + 8.0)..=(rect.right() - 8.0), y, egui::Stroke::new(1.0, p.rule));
                            ui.add_space(10.0);
                        }
                        if widgets::nav_item(ui, page.icon(), page.title(), page.title(), self.page == page, None)
                            .clicked()
                        {
                            self.page = page;
                        }
                    }
                    ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            self.status_chip(ui);
                        });
                    });
                    widgets::panel_rule(ui, widgets::Edge::Left);
                });
        } else {
            egui::Panel::top("header")
                .show_separator_line(false)
                .frame(widgets::header_frame(header, p).inner_margin(egui::Margin { left: 16, right: 16, top: 8, bottom: 0 }))
                .show(root, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("vinoAuthFace").size(15.0).family(theme::bold()).color(header.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.status_chip(ui));
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(18.0, 0.0);
                        a11y::tab_list(ui, "Pages");
                        for page in Page::ALL {
                            if widgets::tab(ui, page.title(), self.page == page, true, header).clicked() {
                                self.page = page;
                            }
                        }
                    });
                });
        }

        let waiting = !self.system.changes().is_empty() || self.apply.lock().unwrap().running;
        if waiting {
            let bar = egui::Panel::bottom("apply")
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(p.surface).inner_margin(egui::Margin::symmetric(16, 8)))
                .show(root, |ui| self.apply_bar(ui));
            let rect = bar.response.rect;
            root.painter().hline(rect.x_range(), rect.top() + 0.5, egui::Stroke::new(1.0, p.rule));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(p.bg).inner_margin(egui::Margin {
                left: if wide { widgets::PAGE_INSET as i8 } else { 16 },
                right: 0,
                top: if wide { 20 } else { 12 },
                bottom: 0,
            }))
            .show(root, |ui| {
                widgets::page_scroll(ui, self.page.title(), widgets::CONTENT_MAX, |ui| {
                    widgets::title(ui, self.page.title());
                    if !matches!(self.page, Page::You | Page::Face) {
                        widgets::small(ui, "For everyone on this computer. Changes wait for Apply.");
                    }
                    if let Some((tone, text)) = self.notice.clone() {
                        ui.add_space(12.0);
                        if widgets::notice(ui, tone, &text) {
                            self.notice = None;
                        }
                    }
                    if !matches!(self.page, Page::You | Page::Face) {
                        if let Some(e) = &self.system.unreadable {
                            ui.add_space(12.0);
                            widgets::banner(ui, Tone::Error, &format!("{SYSTEM_CONFIG_PATH} can't be read: {e}"), |_| {});
                        } else if let (Err(e), Some(_)) = (&self.system.config, &self.system.read) {
                            ui.add_space(12.0);
                            widgets::banner(
                                ui,
                                Tone::Error,
                                &format!("{SYSTEM_CONFIG_PATH} doesn't parse, so face unlock is off: {e}"),
                                |_| {},
                            );
                        }
                    }
                    ui.add_space(16.0);
                    match self.page {
                        Page::Face => self.face(ui),
                        Page::Matching => self.matching(ui),
                        Page::Liveness => self.liveness(ui),
                        Page::Camera => self.camera_page(ui),
                        Page::Models => self.models(ui),
                        Page::Security => self.security(ui),
                        Page::You => self.you(ui),
                    }
                });
            });

        // Dragging or typing changes a value every frame; write once it settles.
        if self.user_dirty && ctx.dragged_id().is_none() && !ctx.egui_wants_keyboard_input() {
            self.user_dirty = false;
            if let Err(e) = self.user.save() {
                self.notice = Some((Tone::Error, format!("Couldn't save your settings: {e}")));
            }
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A video node, as the camera list shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cam {
    pub path: String,
    pub name: String,
    /// One auto-detect would consider: real hardware, IR, opens for capture.
    pub ir: bool,
    /// A v4l2loopback node: anything can write frames into it.
    pub virtual_: bool,
}

/// Every video node, IR sensors first in `ir`'s order (auto-detect's, best
/// first), then the other real cameras, then virtual ones, each by number.
fn cameras(ir: &[String]) -> Vec<Cam> {
    let mut list: Vec<Cam> = capture::list_video_devices()
        .into_iter()
        .map(|path| {
            let node = path.trim_start_matches("/dev/");
            let name = std::fs::read_to_string(format!("/sys/class/video4linux/{node}/name")).unwrap_or_default();
            Cam {
                ir: ir.contains(&path),
                virtual_: !capture::is_physical(&path),
                name: name.trim().to_string(),
                path,
            }
        })
        .collect();
    sort_cameras(&mut list, ir);
    list
}

fn sort_cameras(list: &mut [Cam], ir: &[String]) {
    // Stable: list_video_devices already gives numeric order.
    list.sort_by_key(|c| (ir.iter().position(|p| *p == c.path).unwrap_or(usize::MAX), c.virtual_));
}

/// A slider and its number box, both named by the row. Right to left: the
/// box, then the bar.
fn slider(
    ui: &mut egui::Ui,
    named: &dyn Fn(egui::Response) -> egui::Response,
    value: &mut f64,
    range: RangeInclusive<f64>,
    unit: Unit,
    log: bool,
) -> egui::Response {
    let (decimals, suffix) = match unit {
        Unit::Ratio(d) => (d, ""),
        Unit::Seconds => (1, " s"),
        Unit::Count => (0, ""),
        Unit::Minutes => (0, " min"),
    };
    let number = egui::DragValue::new(value)
        .range(range.clone())
        .speed(unit.step())
        .fixed_decimals(decimals)
        .suffix(suffix);
    let number = named(ui.add(number));
    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).clamp(48.0, 200.0);
    let bar = egui::Slider::new(value, range).show_value(false).logarithmic(log);
    let bar = if log { bar } else { bar.step_by(unit.step()) };
    let bar = named(ui.add(bar));
    bar | number
}

pub fn run(page: Option<Page>) -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_app_id(APP_ID)
            .with_inner_size([860.0, 660.0])
            .with_min_inner_size([340.0, 320.0]),
        // As in vinoSearch's settings: with vsync, KWin can stall a window it
        // considers hidden.
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_ID,
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App(SettingsWindow::new(&cc.egui_ctx, page))))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vinoauthface-settings: {err}");
            ExitCode::FAILURE
        }
    }
}

struct App(SettingsWindow);

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.0.user_dirty {
            let _ = self.0.user.save();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.show(ui);
    }
}

#[cfg(test)]
mod ui_tests;
