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
use face_auth_core::model_files::{self, human_size, ModelFile};
use face_auth_core::{capture, config_write, update, FaceAuthConfig};
use vino_ui::a11y;
use vino_ui::icons::Icon;
use vino_ui::theme;
use vino_ui::widgets::{self, Button, ChipKind, Column, Row, Tone};

use crate::apply;
use crate::enrol;
use crate::files::{self, ConfigFile};
use crate::keys::{self, Key};
use crate::models::{self, Check, Checks, Disk};

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
    About,
}

impl Page {
    pub const ALL: [Page; 8] = [
        Page::Face,
        Page::Matching,
        Page::Liveness,
        Page::Camera,
        Page::Models,
        Page::Security,
        Page::You,
        Page::About,
    ];

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
            Page::About => "about",
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
            Page::About => vino_ui::about::TITLE,
        }
    }

    fn max_width(self) -> f32 {
        match self {
            Page::Face | Page::Liveness | Page::Camera | Page::About => widgets::CONTENT_MAX,
            Page::Matching | Page::Models | Page::Security | Page::You => widgets::WIDE_MAX,
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
            Page::About => Icon::Info,
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

/// `widgets::columns`, with the 20 between this window's groups kept when
/// the right column is stacked under the left.
fn columns(ui: &mut egui::Ui, mut add: impl FnMut(&mut egui::Ui, Column)) {
    let stacked = ui.available_width() < widgets::TWO_COLUMNS_FROM;
    widgets::columns(ui, |ui, column| {
        if stacked && column == Column::Right {
            ui.add_space(20.0);
        }
        add(ui, column);
    });
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
    /// The Models page's clean-up: what is on disk, read when needed and
    /// dropped to be read again (`None`).
    disk: Option<Disk>,
    /// The model directory and the compiled cache; tests point them elsewhere.
    pub(crate) disk_dirs: (PathBuf, PathBuf),
    /// A clean-up verb running through pkexec, and what it said.
    pub(crate) clean_up: Arc<Mutex<apply::State>>,
    pub(crate) clean_up_runner: apply::Runner,
    /// "Delete unused models" waiting for its confirmation.
    confirm_delete_models: bool,
    focused: bool,
    kde_header: Option<theme::KdeHeader>,
    /// The window's buttons and edges are drawn here, not by the desktop
    /// (vino-ui's `window`).
    app_title_bar: bool,
    text_scale: f32,
    /// About's Updates row.
    updater: vino_ui::update::Updater,
}

/// About's Updates row: "Update to vN" asks polkit to run the tray helper's `upgrade` verb,
/// as the tray's own entry does (an install is root's). Without the helper it links to the
/// release instead. A local build is "dev", and is told the newest release but never offered it.
fn updater(helper: bool) -> vino_ui::update::Updater {
    let updater = vino_ui::update::Updater::new(vino_ui::App::AuthFace, update::CURRENT);
    if helper {
        updater.with_command(["pkexec", apply::HELPER, "upgrade"])
    } else {
        updater
    }
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
            disk: None,
            disk_dirs: (PathBuf::from(model_files::SHARE_DIR), PathBuf::from(model_files::CACHE_DIR)),
            clean_up: Arc::default(),
            clean_up_runner: apply::pkexec_verb,
            confirm_delete_models: false,
            focused: false,
            kde_header: None,
            app_title_bar: false,
            text_scale: 1.0,
            updater: updater(helper),
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

    fn take_apply_result(&mut self) {
        let result = self.apply.lock().unwrap().result.take();
        match result {
            Some(Ok(_)) => {
                let tag_changed = {
                    let was = self.system.read.as_deref().and_then(|t| FaceAuthConfig::from_toml(t).ok());
                    was.map(|w| w.model_tag()) != self.system.config.as_ref().ok().map(FaceAuthConfig::model_tag)
                };
                self.system.reload();
                self.disk = None;
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
        if focused && !self.focused {
            self.disk = None;
        }
        self.focused = focused;
        self.take_apply_result();
        let cleaned = self.clean_up.lock().unwrap().result.take();
        if let Some(result) = cleaned {
            self.notice = Some(match result {
                Ok(said) => (Tone::Info, said),
                Err(e) => (Tone::Error, e),
            });
            self.disk = None;
        }
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
        let window = ctx.content_rect().width();
        let wide = window >= widgets::SIDEBAR_FROM;

        if wide {
            egui::Panel::left("pages")
                .resizable(false)
                .show_separator_line(false)
                .exact_size(widgets::sidebar_width(window))
                .frame(widgets::sidebar_frame(p))
                .show(root, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // vino-ui's, which the window bar's row is centred on.
                    let brand = ui
                        .scope(|ui| {
                            ui.style_mut().interaction.selectable_labels = false;
                            widgets::sidebar_brand(ui, Some(vino_ui::logo::authface), "vinoAuthFace", "Settings");
                        })
                        .response
                        .rect;
                    if self.app_title_bar {
                        // After the brand, so its name drags the window.
                        let clip = ui.clip_rect();
                        vino_ui::window::drag(
                            ui,
                            egui::Rect::from_x_y_ranges(clip.x_range(), clip.top()..=brand.bottom()),
                            "brand",
                        );
                    }
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
                    if self.app_title_bar {
                        vino_ui::window::drag(ui, ui.clip_rect(), "header");
                    }
                    ui.horizontal(|ui| {
                        vino_ui::logo::authface(ui, 22.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new("vinoAuthFace").size(15.0).family(theme::bold()).color(header.text),
                            )
                            .selectable(false),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // The window's buttons at the row's end, the status beside them.
                            if self.app_title_bar {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(vino_ui::window::controls_width(), vino_ui::window::BUTTON),
                                    Layout::right_to_left(Align::Center),
                                    vino_ui::window::controls,
                                );
                                ui.add_space(4.0);
                            }
                            self.status_chip(ui);
                        });
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

        // With the app's title bar, a wide window's page title moves up out of
        // the scroll area into a bar of its own, with the window buttons at its
        // end: that bar is what the window is dragged by. The status chip stays
        // at the foot of the sidebar, so nothing sits beside the buttons.
        let title_bar = wide && self.app_title_bar;
        if title_bar {
            let title = self.page.title();
            vino_ui::window::bar(
                root,
                widgets::page_inset(window),
                |ui| {
                    widgets::title(ui, title);
                },
                |_| {},
            );
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(p.bg).inner_margin(egui::Margin {
                left: if wide { widgets::page_inset(window) as i8 } else { 16 },
                right: 0,
                top: match (wide, title_bar) {
                    (_, true) => 2,
                    (true, false) => 20,
                    (false, false) => 12,
                },
                bottom: 0,
            }))
            .show(root, |ui| {
                widgets::page_scroll(ui, self.page.title(), self.page.max_width(), |ui| {
                    if !title_bar {
                        widgets::title(ui, self.page.title());
                    }
                    if !matches!(self.page, Page::You | Page::Face | Page::About) {
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
                        // The release tag ("v3"), or "dev" for a local build. update_check = false
                        // drops the Updates row with the tray's and doctor's checks.
                        Page::About if self.system.values().update_check() => {
                            vino_ui::about::page_with_updates(ui, &mut self.updater)
                        }
                        Page::About => vino_ui::about::page(ui, vino_ui::App::AuthFace, update::CURRENT),
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

/// `a.onnx and b.onnx, 180 MB`: the files, then their total.
fn listed(models: &[ModelFile]) -> String {
    let names: Vec<_> = models.iter().map(ModelFile::name).collect();
    let names = match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("{names}, {}", human_size(models.iter().map(|m| m.bytes).sum()))
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
    let app_title_bar = vino_ui::window::app_title_bar();
    let options = eframe::NativeOptions {
        viewport: vino_ui::window::viewport(
            egui::ViewportBuilder::default()
                .with_title(TITLE)
                .with_app_id(APP_ID)
                .with_inner_size([860.0, 660.0])
                .with_min_inner_size([340.0, 320.0]),
            app_title_bar,
        ),
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
            let mut window = SettingsWindow::new(&cc.egui_ctx, page);
            window.app_title_bar = app_title_bar;
            Ok(Box::new(App(window)))
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

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        vino_ui::input::faster_touchpad(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.show(ui);
        if self.0.app_title_bar {
            vino_ui::window::edges(ui);
        }
    }
}

mod pages;
mod rows;
#[cfg(test)]
mod ui_tests;
