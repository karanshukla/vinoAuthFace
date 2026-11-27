//! The kinds of row a page is built from: a number, a switch, a choice, a path, the camera.

use super::*;

/// Where a number's slider starts, so that drawing it changes nothing: on the
/// step, since an f32 widened to f64 (0.6 is 0.60000002) would snap and read
/// as a change, and inside the range, since the slider would clamp a user's
/// value below this computer's floor and write it back unasked (pinned by
/// `tests::a_widened_f32_starts_on_its_own_step`,
/// `tests::a_value_below_the_range_starts_at_its_floor`).
fn shown_value(value: f64, step: f64, range: &RangeInclusive<f64>) -> f64 {
    ((value / step).round() * step).clamp(*range.start(), *range.end())
}

impl SettingsWindow {
    pub(super) fn number(&mut self, ui: &mut egui::Ui, user: bool, key: Key, n: Num<'_>) {
        let set = self.file(user).is_set(key);
        let step = n.unit.step();
        let before = shown_value(n.value, step, &n.range);
        let mut v = before;
        let mut out = None;
        Row::new(n.title)
            .help(&n.help)
            .tone(n.tone)
            .show(ui, |ui, named| {
                // Only while it says something the default doesn't.
                let differs = (before - n.default).abs() >= step / 2.0;
                if set
                    && differs
                    && a11y::for_what(ui.add(Button::ghost("Default").small()), n.title).clicked()
                {
                    out = Some(None);
                }
                let response = slider(ui, named, &mut v, &n);
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
    pub(super) fn switch(
        &mut self,
        ui: &mut egui::Ui,
        user: bool,
        key: Key,
        title: &str,
        help: &str,
        value: bool,
        default: bool,
    ) {
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
    pub(super) fn choice(
        &mut self,
        ui: &mut egui::Ui,
        key: Key,
        title: &str,
        help: &str,
        tone: Tone,
        current: &str,
        default: &str,
        options: &[(&'static str, &str)],
    ) {
        let mut picked: &'static str = options
            .iter()
            .find(|(v, _)| *v == current)
            .map_or("", |(v, _)| v);
        let mut changed = false;
        Row::new(title).help(help).tone(tone).show(ui, |ui, _| {
            changed = widgets::button_group(ui, title, &mut picked, options);
        });
        if changed {
            self.set(
                false,
                key,
                (picked != default).then(|| keys::string(picked)),
            );
        }
    }

    /// A path typed in a field, set when the field loses focus. Empty means
    /// the default. `check` adds a button that loads the model. A path that
    /// isn't `editable` is shown, not typed in: it stays as the file has it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn path(
        &mut self,
        ui: &mut egui::Ui,
        key: Key,
        title: &str,
        help: &str,
        default: &str,
        dir: bool,
        check: Option<models::Kind>,
        editable: bool,
    ) {
        let current = self
            .system
            .raw(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let effective = if current.is_empty() {
            default.to_string()
        } else {
            current.clone()
        };
        let was = self
            .system
            .read
            .as_deref()
            .and_then(|t| toml::from_str::<toml::Table>(t).ok());
        let was = was
            .as_ref()
            .and_then(|t| key.value(t))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let mut help = help.to_string();
        let mut tone = Tone::Neutral;
        if !current.is_empty() && Some(&current) != was.as_ref() {
            if let Err(e) = config_write::trusted_path(Path::new(&current), dir) {
                help = format!("Apply will reject this path: {e}.");
                tone = Tone::Warn;
            }
        } else if !Path::new(&effective).exists() {
            help = format!("{effective} does not exist.");
            tone = Tone::Warn;
        }
        let result = check.and_then(|kind| {
            self.checks
                .lock()
                .unwrap()
                .get(&(kind, effective.clone()))
                .cloned()
        });
        match &result {
            Some(Check::Done(Ok(text))) => help = format!("{help} {text}"),
            Some(Check::Done(Err(e))) => {
                help = format!("Failed to load: {e}");
                tone = Tone::Error;
            }
            _ => {}
        }

        let draft_key = (false, key);
        let mut draft = self
            .drafts
            .remove(&draft_key)
            .unwrap_or_else(|| current.clone());
        let mut commit = false;
        let mut editing = false;
        let mut run_check = false;
        Row::new(title)
            .help(&help)
            .tone(tone)
            .show(ui, |ui, named| {
                if check.is_some() {
                    let running = result == Some(Check::Running);
                    let label = if running { "Checking…" } else { "Check" };
                    let button = ui.add_enabled(!running, Button::tonal(label).small());
                    run_check = a11y::for_what(button, title).clicked();
                }
                let width = ui.available_width().min(320.0);
                let field =
                    named(ui.add(widgets::field(&mut draft, default, width).interactive(editable)));
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
    pub(super) fn camera(&mut self, ui: &mut egui::Ui, user: bool) {
        let file = if user { &self.user } else { &self.system };
        let current = file
            .raw(keys::DEVICE)
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let cam = |path: &str| self.cameras.iter().find(|c| c.path == path);
        let label = |path: &str| match cam(path) {
            Some(c) if c.ir => format!("{} ({path})", c.name),
            Some(c) if c.virtual_ => format!("{} ({path}), virtual", c.name),
            Some(c) => format!("{} ({path}), not IR", c.name),
            None => format!("{path} (not connected)"),
        };
        let choices: Vec<String> = if user {
            self.ir_cameras.clone()
        } else {
            self.cameras.iter().map(|c| c.path.clone()).collect()
        };
        let mut choices: Vec<Option<String>> = std::iter::once(None)
            .chain(choices.into_iter().map(Some))
            .collect();
        if current
            .as_ref()
            .is_some_and(|c| !choices.contains(&Some(c.clone())))
        {
            choices.push(current.clone());
        }
        // Auto-detect says what it would pick, so it isn't a mystery choice.
        let auto = match (user, self.ir_cameras.first()) {
            (true, _) => "System default camera".to_string(),
            (false, Some(best)) => format!("Auto-detect: {}", label(best)),
            (false, None) => "Auto-detect: no IR sensor found".to_string(),
        };
        let shown = current.as_deref().map_or_else(|| auto.clone(), label);
        let (help, tone) = match (&current, user) {
            (Some(path), true) if !self.ir_cameras.contains(path) => (
                "Ignored: not a valid IR sensor on this system.".to_string(),
                Tone::Warn,
            ),
            (Some(path), false) if cam(path).is_none() => {
                (format!("{path} isn't connected."), Tone::Warn)
            }
            (Some(path), false) if cam(path).is_some_and(|c| c.virtual_) => {
                ("Virtual camera — for testing only.".to_string(), Tone::Warn)
            }
            (Some(path), false) if cam(path).is_some_and(|c| !c.ir) => (
                "Not an IR camera. Face unlock needs infrared.".to_string(),
                Tone::Warn,
            ),
            _ => (String::new(), Tone::Neutral),
        };
        let mut picked = current.clone();
        Row::new("IR camera")
            .help(&help)
            .tone(tone)
            .show(ui, |ui, named| {
                let width = ui.available_width().min(320.0);
                let combo = widgets::combo(
                    ui,
                    if user { "user-camera" } else { "camera" },
                    shown,
                    width,
                )
                .show_ui(ui, |ui| {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_widened_f32_starts_on_its_own_step() {
        let widened = f64::from(0.6f32);
        assert_ne!(widened, 0.6);
        assert!((shown_value(widened, 0.01, &(0.3..=1.0)) - 0.6).abs() < 1e-12);
    }

    #[test]
    fn a_value_below_the_range_starts_at_its_floor() {
        assert_eq!(shown_value(0.4, 0.01, &(0.5..=1.0)), 0.5);
    }
}
