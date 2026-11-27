//! The pages, one method each.

use super::*;

impl SettingsWindow {
    /// Guided enrolment. The steps, and why they help a model, are in
    /// `face_auth_core::guided`.
    pub(super) fn face(&mut self, ui: &mut egui::Ui) {
        let run = {
            let run = self.enrol.lock().unwrap();
            (run.running, run.step, run.kept, run.hint)
        };
        let (running, step, kept, hint) = run;
        widgets::group_label(ui, "Guided enrolment");
        widgets::group_help(
            ui,
            "Walks you through six poses so face unlock works from everyday angles.",
        );
        if running {
            let mut skip = false;
            let mut stop = false;
            widgets::group(ui, |ui| {
                let (title, progress) = match step {
                    Some((n, total, step)) => (
                        step.prompt(),
                        format!("Step {n} of {total} · {} of {} captured", kept.0, kept.1),
                    ),
                    None => ("Waiting for authentication…", String::new()),
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
            Row::new("Enrolment status").show(ui, |ui, _| {
                a11y::live(&widgets::chip(ui, kind, text));
            });
            let enrolled = self.enrolled == Some(true);
            if self.confirm_replace {
                let p = theme::palette(ui.ctx());
                Row::new("Enrol again")
                    .help("Replaces your saved face data.")
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
                Row::new("Enrol again").show(ui, |ui, _| {
                    if ui.add(Button::secondary("Enrol again…")).clicked() {
                        self.confirm_replace = true;
                    }
                });
            } else {
                Row::new("Enrol")
                    .help("Takes about two minutes.")
                    .show(ui, |ui, _| {
                        if ui.add(Button::primary("Start")).clicked() {
                            start = Some(false);
                        }
                    });
            }
            if enrolled {
                Row::new("Add poses")
                    .help("Add more angles without replacing existing scans.")
                    .show(ui, |ui, _| {
                        if ui
                            .add_enabled(!self.confirm_replace, Button::tonal("Add"))
                            .clicked()
                        {
                            start = Some(true);
                        }
                    });
            }
        });
        ui.add_space(8.0);
        widgets::small(ui, "No camera preview during enrolment.");
        if let Some(append) = start {
            self.notice = None;
            enrol::start(&self.enrol, self.spawner, append, ui.ctx());
        }
    }

    pub(super) fn matching(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        columns(ui, |ui, column| match column {
            Column::Left => self.matching_rows(ui, &c),
            Column::Right => {
                widgets::group_label(ui, "Scan");
                widgets::group(ui, |ui| self.timing(ui, false, &c));
            }
        });
    }

    pub(super) fn matching_rows(&mut self, ui: &mut egui::Ui, c: &FaceAuthConfig) {
        widgets::group_label(ui, "Matching");
        widgets::group(ui, |ui| {
            self.number(
                ui,
                false,
                keys::THRESHOLD,
                Num {
                    title: "Match threshold",
                    help: "Higher values are stricter.".into(),
                    tone: Tone::Neutral,
                    value: c.threshold().into(),
                    default: 0.6,
                    range: range_f64(limits::THRESHOLD_RANGE),
                    unit: Unit::Ratio(2),
                    log: false,
                    zero: None,
                },
            );
            self.number(
                ui,
                false,
                keys::DETECTOR_THRESHOLD,
                Num {
                    title: "Face detection",
                    help: "Minimum confidence to detect a face.".into(),
                    tone: Tone::Neutral,
                    value: c.detector_threshold().into(),
                    default: 0.5,
                    range: range_f64(limits::DETECTOR_THRESHOLD_RANGE),
                    unit: Unit::Ratio(2),
                    log: false,
                    zero: None,
                },
            );
            self.number(
                ui,
                false,
                keys::MIN_FACE_SIZE_RATIO,
                Num {
                    title: "Smallest face",
                    help: "Ignore faces smaller than this fraction of the frame.".into(),
                    tone: Tone::Neutral,
                    value: c.min_face_size_ratio().into(),
                    default: 0.0,
                    range: range_f64(limits::MIN_FACE_SIZE_RANGE),
                    unit: Unit::Ratio(2),
                    log: false,
                    zero: Some("Off"),
                },
            );
        });
    }

    /// The scan timing rows, for either file: they're the same keys and
    /// bounds, and the user's are honoured as they are.
    pub(super) fn timing(&mut self, ui: &mut egui::Ui, user: bool, c: &FaceAuthConfig) {
        let sys = self.system.values();
        let def =
            |user_default: u64, built_in: u64| secs(if user { user_default } else { built_in });
        let unset: Vec<Key> = [
            keys::SCAN_DURATION_MS,
            keys::LIVENESS_GRACE_MS,
            keys::CAPTURE_TIMEOUT_MS,
            keys::SCAN_INTERVAL_MS,
        ]
        .into_iter()
        .filter(|key| user && !self.user.is_set(*key))
        .collect();
        let inherits = |key: Key, shown: String| {
            if unset.contains(&key) {
                format!(" System default: {shown}.")
            } else {
                String::new()
            }
        };
        let help = format!(
            "How long to scan before falling back to your password.{}",
            inherits(
                keys::SCAN_DURATION_MS,
                Unit::Seconds.show(secs(sys.scan_duration_ms()))
            )
        );
        self.number(
            ui,
            user,
            keys::SCAN_DURATION_MS,
            Num {
                title: "Scan time",
                help,
                tone: Tone::Neutral,
                value: secs(c.scan_duration_ms()),
                default: def(sys.scan_duration_ms(), 5000),
                range: range_secs(limits::SCAN_DURATION_RANGE),
                unit: Unit::Seconds,
                log: false,
                zero: None,
            },
        );
        let liveness_on =
            c.liveness_motion_threshold() > 0.0 || c.liveness_residual_motion_threshold() > 0.0;
        if liveness_on {
            let help = format!(
                "Extra wait after a match for movement.{}",
                inherits(
                    keys::LIVENESS_GRACE_MS,
                    Unit::Seconds.show(secs(sys.liveness_grace_ms()))
                )
            );
            self.number(
                ui,
                user,
                keys::LIVENESS_GRACE_MS,
                Num {
                    title: "Liveness grace period",
                    help,
                    tone: Tone::Neutral,
                    value: secs(c.liveness_grace_ms()),
                    default: def(sys.liveness_grace_ms(), 4000),
                    range: range_secs(limits::LIVENESS_GRACE_RANGE),
                    unit: Unit::Seconds,
                    log: false,
                    zero: None,
                },
            );
        }
        let help = format!(
            "Wait limit for a single camera frame.{}",
            inherits(
                keys::CAPTURE_TIMEOUT_MS,
                Unit::Seconds.show(secs(sys.capture_timeout_ms() as u64))
            )
        );
        self.number(
            ui,
            user,
            keys::CAPTURE_TIMEOUT_MS,
            Num {
                title: "Camera timeout",
                help,
                tone: Tone::Neutral,
                value: secs(c.capture_timeout_ms() as u64),
                default: def(sys.capture_timeout_ms() as u64, 5000),
                range: range_secs(limits::CAPTURE_TIMEOUT_RANGE),
                unit: Unit::Seconds,
                log: false,
                zero: None,
            },
        );
        let help = format!(
            "Delay between frames to reduce CPU use.{}",
            inherits(
                keys::SCAN_INTERVAL_MS,
                Unit::Seconds.show_with(secs(sys.scan_interval_ms()), Some("Off"))
            )
        );
        self.number(
            ui,
            user,
            keys::SCAN_INTERVAL_MS,
            Num {
                title: "Pause between frames",
                help,
                tone: Tone::Neutral,
                value: secs(c.scan_interval_ms()),
                default: def(sys.scan_interval_ms(), 0),
                range: range_secs(limits::SCAN_INTERVAL_RANGE),
                unit: Unit::Seconds,
                log: false,
                zero: Some("Off"),
            },
        );
    }

    pub(super) fn liveness(&mut self, ui: &mut egui::Ui) {
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
            "off" => (
                "A still photo or video replay could unlock this computer.",
                Tone::Warn,
            ),
            "standard" => ("Rejects static photos.", Tone::Neutral),
            "strict" => (
                "Also rejects moving photos; may fail if you hold completely still.",
                Tone::Neutral,
            ),
            _ => ("", Tone::Neutral),
        };
        widgets::group_label(ui, "Motion liveness");
        widgets::group_help(ui, "Requires slight movement to reject static photos.");
        widgets::group(ui, |ui| {
            let mut picked = preset;
            let mut changed = false;
            Row::new("Preset").help(help).tone(tone).show(ui, |ui, _| {
                changed = widgets::button_group(
                    ui,
                    "Preset",
                    &mut picked,
                    &[
                        ("off", "Off"),
                        ("standard", "Standard"),
                        ("strict", "Strict"),
                    ],
                );
            });
            if changed {
                // The preset only fills values that aren't set by hand.
                self.set(false, keys::LIVENESS_MOTION, None);
                self.set(false, keys::LIVENESS_RESIDUAL, None);
                self.set(
                    false,
                    keys::LIVENESS_PRESET,
                    (picked != "standard").then(|| keys::string(picked)),
                );
            }
            if preset != "off" {
                self.number(
                    ui,
                    false,
                    keys::LIVENESS_MOTION,
                    Num {
                        title: "Face motion",
                        help: "Minimum movement between frames.".into(),
                        tone: Tone::Neutral,
                        value: motion.into(),
                        default: 0.01,
                        range: range_f64(limits::LIVENESS_MOTION_RANGE),
                        unit: Unit::Ratio(3),
                        log: true,
                        zero: Some("Off"),
                    },
                );
                self.number(
                    ui,
                    false,
                    keys::LIVENESS_RESIDUAL,
                    Num {
                        title: "Non-rigid motion",
                        help: "Natural movement like blinking or turning.".into(),
                        tone: Tone::Neutral,
                        value: residual.into(),
                        default: 0.0,
                        range: range_f64(limits::LIVENESS_MOTION_RANGE),
                        unit: Unit::Ratio(2),
                        log: false,
                        zero: Some("Off"),
                    },
                );
                self.number(
                    ui,
                    false,
                    keys::LIVENESS_WINDOW_MS,
                    Num {
                        title: "Liveness window",
                        help: "Window for comparing frames.".into(),
                        tone: Tone::Neutral,
                        value: secs(c.liveness_window_ms()),
                        default: 1.0,
                        range: range_secs(limits::LIVENESS_WINDOW_RANGE),
                        unit: Unit::Seconds,
                        log: false,
                        zero: Some("Consecutive"),
                    },
                );
            }
        });
    }

    pub(super) fn camera_page(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        widgets::group_label(ui, "Camera");
        widgets::group(ui, |ui| {
            self.camera(ui, false);
            self.switch(
                ui,
                false,
                keys::BIND_CAMERA,
                "Require enrolled camera",
                "Only allow cameras matching the USB ID used during enrolment.",
                c.bind_camera(),
                true,
            );
            let pin = (
                self.system.raw(keys::PINNED_CAMERA_PATH),
                self.system.raw(keys::PINNED_CAMERA_INDEX),
            );
            match pin {
                (Some(path), index) => {
                    let help = format!(
                        "{} (node {}). Other cameras are rejected.",
                        path.as_str().unwrap_or("?"),
                        index.and_then(|i| i.as_integer()).unwrap_or(0)
                    );
                    let mut remove = false;
                    Row::new("Pinned to a USB port")
                        .help(&help)
                        .show(ui, |ui, _| {
                            remove = a11y::for_what(
                                ui.add(Button::secondary("Remove pin")),
                                "the camera pin",
                            )
                            .clicked();
                        });
                    if remove {
                        self.set(false, keys::PINNED_CAMERA_PATH, None);
                        self.set(false, keys::PINNED_CAMERA_INDEX, None);
                    }
                }
                _ => {
                    Row::new("Pinned to a USB port")
                        .help("Not pinned. Run sudo vinoauthface-pin-camera to lock to a USB port.")
                        .show(ui, |_, _| {});
                }
            }
        });
    }

    pub(super) fn models(&mut self, ui: &mut egui::Ui) {
        columns(ui, |ui, column| match column {
            Column::Left => self.models_in_use(ui),
            Column::Right => self.clean_up(ui),
        });
    }

    pub(super) fn models_in_use(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        let was = self
            .system
            .read
            .as_deref()
            .and_then(|t| FaceAuthConfig::from_toml(t).ok());
        widgets::group_label(ui, "Recognition");
        widgets::group_help(
            ui,
            "Read-only here. Edit /etc/face-auth.toml to switch models (requires re-enrolment).",
        );
        widgets::group(ui, |ui| {
            self.path(
                ui,
                keys::MODEL_PATH,
                "Recognition model",
                "Compares faces against enrolled templates.",
                DEFAULT_MODEL,
                false,
                Some(models::Kind::Encoder),
                false,
            );
            self.path(
                ui,
                keys::DETECTOR_MODEL_PATH,
                "Detector model",
                "Finds faces in each frame.",
                DEFAULT_DETECTOR,
                false,
                Some(models::Kind::Detector),
                false,
            );
            let tag = c.model_tag();
            let old = was
                .as_ref()
                .map(FaceAuthConfig::model_tag)
                .unwrap_or_else(|| tag.clone());
            if tag == old {
                Row::new("Templates")
                    .help(&format!("Tagged with {tag}."))
                    .show(ui, |_, _| {});
            } else {
                Row::new("Templates")
                    .help(&format!(
                        "Changing from {old} to {tag}. All users must re-enrol after applying."
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
                "OpenVINO requires an NPU-enabled build.",
                Tone::Neutral,
                &backend,
                "tract",
                &[("tract", "Tract (CPU)"), ("openvino", "OpenVINO")],
            );
            if backend == "openvino" {
                self.choice(
                    ui,
                    keys::NPU_DEVICE,
                    "OpenVINO device",
                    "",
                    Tone::Neutral,
                    &c.npu_device(),
                    "NPU",
                    &[("NPU", "NPU"), ("GPU", "GPU"), ("CPU", "CPU")],
                );
            }
        });
    }

    /// Spare models and the compiled cache, each removed by a helper verb
    /// that works out for itself what goes. The models in use are
    /// uninstall's to remove.
    pub(super) fn clean_up(&mut self, ui: &mut egui::Ui) {
        if self.disk.is_none() {
            let config = match (&self.system.unreadable, &self.system.read) {
                (Some(_), _) => None,
                (None, Some(text)) => FaceAuthConfig::from_toml(text).ok(),
                (None, None) => FaceAuthConfig::from_toml("").ok(),
            };
            self.disk = Some(Disk::read(
                &self.disk_dirs.0,
                &self.disk_dirs.1,
                config.as_ref(),
            ));
        }
        let Some(disk) = self.disk.clone() else {
            return;
        };
        let running = self.clean_up.lock().unwrap().running;
        let can = self.helper && !running;
        let mut run = None;
        widgets::group_label(ui, "Disk space");
        widgets::group(ui, |ui| {
            match &disk.unused {
                Err(e) => {
                    Row::new("Unused models")
                        .help(&format!("Unable to check unused models: {e}."))
                        .show(ui, |_, _| {});
                }
                Ok(unused) if unused.is_empty() => {
                    self.confirm_delete_models = false;
                    Row::new("Unused models").help("None.").show(ui, |_, _| {});
                }
                Ok(unused) if self.confirm_delete_models => {
                    let p = theme::palette(ui.ctx());
                    let help = format!("Deletes {}.", listed(unused));
                    Row::new("Unused models")
                        .help(&help)
                        .tone(Tone::Error)
                        .tint(p.error_tint)
                        .show(ui, |ui, _| {
                            if ui.add(Button::ghost("Cancel")).clicked() {
                                self.confirm_delete_models = false;
                            }
                            if ui.add_enabled(can, Button::destructive("Delete")).clicked() {
                                self.confirm_delete_models = false;
                                run = Some(apply::DELETE_UNUSED_MODELS);
                            }
                        });
                }
                Ok(unused) => {
                    let help = format!("{}.", listed(unused));
                    Row::new("Unused models").help(&help).show(ui, |ui, _| {
                        if a11y::for_what(
                            ui.add_enabled(can, Button::secondary("Delete…")),
                            "the unused models",
                        )
                        .clicked()
                        {
                            self.confirm_delete_models = true;
                        }
                    });
                }
            }
            let help = match &disk.cache {
                Err(e) => format!("Unable to read {}: {e}.", self.disk_dirs.1.display()),
                Ok(0) => "Empty.".into(),
                Ok(bytes) => format!(
                    "{}. Rebuilds automatically on the next unlock.",
                    human_size(*bytes)
                ),
            };
            Row::new("Compiled cache").help(&help).show(ui, |ui, _| {
                let full = matches!(disk.cache, Ok(bytes) if bytes > 0);
                if a11y::for_what(
                    ui.add_enabled(can && full, Button::secondary("Clear")),
                    "the compiled cache",
                )
                .clicked()
                {
                    run = Some(apply::CLEAR_CACHE);
                }
            });
        });
        if !self.helper {
            ui.add_space(8.0);
            widgets::small(ui, "Cleanup requires the system helper.");
        }
        if let Some(verb) = run {
            self.notice = None;
            apply::start(
                &self.clean_up,
                self.clean_up_runner,
                verb.to_string(),
                ui.ctx(),
            );
        }
    }

    pub(super) fn security(&mut self, ui: &mut egui::Ui) {
        let c = self.system.values();
        columns(ui, |ui, column| match column {
            Column::Left => self.when_a_scan_runs(ui, &c),
            Column::Right => self.after_a_scan(ui, &c),
        });
    }

    pub(super) fn when_a_scan_runs(&mut self, ui: &mut egui::Ui, c: &FaceAuthConfig) {
        widgets::group_label(ui, "When a scan runs");
        widgets::group(ui, |ui| {
            self.switch(
                ui,
                false,
                keys::ABORT_IF_SSH,
                "Skip over SSH",
                "Use password for sudo over SSH.",
                c.abort_if_ssh(),
                true,
            );
            self.switch(
                ui,
                false,
                keys::ABORT_IF_LID_CLOSED,
                "Skip with the lid closed",
                "Disable this if you use an external IR camera while docked.",
                c.abort_if_lid_closed(),
                false,
            );
            self.switch(
                ui,
                false,
                keys::SEAT_CHECK,
                "Require active local seat",
                "Only allow the active desktop user.",
                c.seat_check(),
                false,
            );
            self.switch(
                ui,
                false,
                keys::REQUIRE_CONFIRMATION,
                "Confirm sudo with Enter",
                "Press Enter after a match before granting access.",
                c.require_confirmation_elevation.unwrap_or(false),
                false,
            );
            self.number(
                ui,
                false,
                keys::START_DELAY_MS,
                Num {
                    title: "Scan start delay",
                    help: "Wait before scanning after locking the screen.".into(),
                    tone: Tone::Neutral,
                    value: secs(c.start_delay_ms()),
                    default: 2.0,
                    range: range_secs(limits::START_DELAY_RANGE),
                    unit: Unit::Seconds,
                    log: false,
                    zero: Some("Off"),
                },
            );
            if c.start_delay_ms() > 0 {
                self.choice(
                    ui,
                    keys::START_DELAY_SCOPE,
                    "Delay applies to",
                    "Lock screen only, or all prompts (sudo, polkit, login).",
                    Tone::Neutral,
                    &c.start_delay_scope(),
                    "screen_lock",
                    &[("screen_lock", "Lock screen"), ("all", "Everything")],
                );
            }
        });
    }

    pub(super) fn after_a_scan(&mut self, ui: &mut egui::Ui, c: &FaceAuthConfig) {
        let lockout = c.lockout_policy();
        widgets::group_label(ui, "After failed scans");
        widgets::group_help(
            ui,
            "Pauses face unlock after repeated failures. Password login still works.",
        );
        widgets::group(ui, |ui| {
            self.number(
                ui,
                false,
                keys::LOCKOUT_THRESHOLD,
                Num {
                    title: "Failures allowed",
                    help: "Consecutive failures before cooldown.".into(),
                    tone: Tone::Neutral,
                    value: lockout.threshold.into(),
                    default: 5.0,
                    range: 1.0..=50.0,
                    unit: Unit::Count,
                    log: false,
                    zero: None,
                },
            );
            self.number(
                ui,
                false,
                keys::LOCKOUT_BASE_DELAY_MS,
                Num {
                    title: "Initial cooldown",
                    help: "First cooldown; doubles after each further failure.".into(),
                    tone: Tone::Neutral,
                    value: secs(lockout.base_delay_ms),
                    default: 2.0,
                    range: 0.0..=60.0,
                    unit: Unit::Seconds,
                    log: false,
                    zero: None,
                },
            );
            self.number(
                ui,
                false,
                keys::LOCKOUT_MAX_DELAY_MS,
                Num {
                    title: "Maximum cooldown",
                    help: String::new(),
                    tone: Tone::Neutral,
                    value: secs(lockout.max_delay_ms),
                    default: 300.0,
                    range: 0.0..=3600.0,
                    unit: Unit::Seconds,
                    log: true,
                    zero: None,
                },
            );
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Templates");
        widgets::group(ui, |ui| {
            self.switch(
                ui,
                false,
                keys::SEAL_EMBEDDINGS,
                "TPM template sealing",
                "Tie templates to this machine's TPM (applies to new enrolments).",
                c.seal_embeddings(),
                false,
            );
            self.path(
                ui,
                keys::EMBEDDINGS_DIR,
                "Template directory",
                "Must be owned by root.",
                limits::DEFAULT_EMBEDDINGS_DIR,
                true,
                None,
                true,
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
                "Checks daily for new releases. Never runs during unlock.",
                c.update_check(),
                true,
            );
        });
    }

    /// The user's own file: stricter values only, saved as they change.
    pub(super) fn you(&mut self, ui: &mut egui::Ui) {
        let sys = self.system.values();
        let mine = self.user.values();
        if let Err(e) = &self.user.config {
            if self.user.read.is_some() {
                ui.add_space(10.0);
                widgets::banner(
                    ui,
                    Tone::Error,
                    &format!("Invalid personal config — ignoring it: {e}"),
                    |_| {},
                );
            }
        }
        columns(ui, |ui, column| match column {
            Column::Left => self.stricter_for_you(ui, &sys, &mine),
            Column::Right => self.timing_and_camera_for_you(ui, &sys, &mine),
        });
    }

    pub(super) fn stricter_for_you(
        &mut self,
        ui: &mut egui::Ui,
        sys: &FaceAuthConfig,
        mine: &FaceAuthConfig,
    ) {
        widgets::group_label(ui, "Stricter thresholds");
        widgets::group_help(
            ui,
            "Saved immediately. Can only be stricter than the system setting.",
        );
        widgets::group(ui, |ui| {
            let floors = [
                (
                    keys::THRESHOLD,
                    "Match threshold",
                    sys.threshold(),
                    mine.threshold,
                    limits::THRESHOLD_RANGE,
                    2,
                    None,
                ),
                (
                    keys::DETECTOR_THRESHOLD,
                    "Face detection",
                    sys.detector_threshold(),
                    mine.detector_threshold,
                    limits::DETECTOR_THRESHOLD_RANGE,
                    2,
                    None,
                ),
                (
                    keys::MIN_FACE_SIZE_RATIO,
                    "Smallest face",
                    sys.min_face_size_ratio(),
                    mine.min_face_size_ratio,
                    limits::MIN_FACE_SIZE_RANGE,
                    2,
                    Some("Off"),
                ),
                (
                    keys::LIVENESS_MOTION,
                    "Face motion",
                    sys.liveness_motion_threshold(),
                    mine.liveness_motion_threshold,
                    limits::LIVENESS_MOTION_RANGE,
                    3,
                    Some("Off"),
                ),
                (
                    keys::LIVENESS_RESIDUAL,
                    "Non-rigid motion",
                    sys.liveness_residual_motion_threshold(),
                    mine.liveness_residual_motion_threshold,
                    limits::LIVENESS_MOTION_RANGE,
                    2,
                    Some("Off"),
                ),
            ];
            for (key, title, floor, mine, range, decimals, zero) in floors {
                let shown = Unit::Ratio(decimals).show_with(floor.into(), zero);
                let (help, tone) = match mine {
                    Some(v) if v < floor => (
                        format!("Ignored: looser than the system setting ({shown})."),
                        Tone::Warn,
                    ),
                    Some(_) => (format!("System setting: {shown}."), Tone::Neutral),
                    None => (format!("Using system setting ({shown})."), Tone::Neutral),
                };
                self.number(
                    ui,
                    true,
                    key,
                    Num {
                        title,
                        help,
                        tone,
                        value: mine.unwrap_or(floor).into(),
                        default: floor.into(),
                        range: f64::from(floor)..=f64::from(*range.end()),
                        unit: Unit::Ratio(decimals),
                        log: key == keys::LIVENESS_MOTION,
                        zero,
                    },
                );
            }
        });
    }

    pub(super) fn timing_and_camera_for_you(
        &mut self,
        ui: &mut egui::Ui,
        sys: &FaceAuthConfig,
        mine: &FaceAuthConfig,
    ) {
        widgets::group_label(ui, "Scan timing");
        widgets::group(ui, |ui| {
            let effective = sys.clone().with_user_overlay(mine);
            self.timing(ui, true, &effective);
        });
        ui.add_space(20.0);
        widgets::group_label(ui, "Camera and tray");
        widgets::group(ui, |ui| {
            self.camera(ui, true);
            self.number(
                ui,
                true,
                keys::TRAY_IDLE_MINUTES,
                Num {
                    title: "Hide tray icon after",
                    help: String::new(),
                    tone: Tone::Neutral,
                    value: mine.tray_idle_minutes() as f64,
                    default: 30.0,
                    range: 0.0..=1440.0,
                    unit: Unit::Minutes,
                    log: false,
                    zero: Some("Never"),
                },
            );
        });
    }
}
