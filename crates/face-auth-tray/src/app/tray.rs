//! The tray item: what it knows, what it shows, and the messages its menu sends.

use super::menu;
use face_auth_tray::helper::{Choice, Setting};
use face_auth_tray::icon::{self, State};
use face_auth_tray::status::{LoginMode, Status};
use ksni::{Category, MenuItem, ToolTip};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Enrol,
    Retrain,
    Test,
    Upgrade,
    Login(LoginMode),
    Set(&'static Setting, &'static Choice),
}

/// What the tray does once an action has finished.
pub enum After {
    Stay,
    /// Re-exec the tray, which an upgrade just replaced.
    Restart,
}

pub enum Msg {
    Run(Action),
    /// A worker finished; re-read the status.
    Done,
    Restart,
    /// An update check finished; `Some` is a newer release tag.
    Update(Option<String>),
    /// A menu entry was clicked: the icon stays in view.
    Touch,
    /// A new idle delay from the Settings menu, in minutes.
    IdleMinutes(u64),
    Quit,
}

pub struct Tray {
    pub tx: Sender<Msg>,
    pub status: Status,
    pub scanning: bool,
    /// What a running action is doing, for the tooltip. Actions are refused
    /// while one runs.
    pub busy: Option<String>,
    /// A newer release than the one running, from the daily update check.
    pub update: Option<String>,
    /// Idle long enough to tuck the icon away (see `idle.rs`).
    pub hidden: bool,
    /// The idle delay in force, for the Settings menu.
    pub idle_minutes: u64,
}

impl Tray {
    pub fn state(&self) -> State {
        if self.scanning {
            State::Scanning
        } else if self.status.ready() {
            State::Ready
        } else {
            State::Attention
        }
    }

    pub fn summary(&self) -> String {
        if let Some(busy) = &self.busy {
            return busy.clone();
        }
        if self.scanning {
            return "Scanning: look at the camera".into();
        }
        match (&self.status.camera, self.status.enrolled) {
            (None, _) => "No IR camera found".into(),
            (Some(_), Some(true)) => "Face unlock is ready".into(),
            (Some(_), Some(false)) => "No face enrolled yet".into(),
            (Some(_), None) => "Cannot tell whether a face is enrolled".into(),
        }
    }

    pub fn run(&mut self, action: Action) {
        if self.busy.is_none() {
            let _ = self.tx.send(Msg::Run(action));
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "vinoauthface".into()
    }

    fn title(&self) -> String {
        "vinoAuthFace".into()
    }

    // Left click opens the menu, the status panel; otherwise it does nothing.
    const MENU_ON_ACTIVATE: bool = true;

    // ApplicationStatus, like vinoWhisper's tray: Plasma files Hardware
    // items in a different place in the panel.
    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        self.state().icon_name().into()
    }

    fn attention_icon_name(&self) -> String {
        self.icon_name()
    }

    fn status(&self) -> ksni::Status {
        if self.scanning || self.busy.is_some() {
            ksni::Status::NeedsAttention
        } else if self.hidden {
            // Plasma moves Passive items behind the panel's arrow.
            ksni::Status::Passive
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        static ICONS: OnceLock<[Vec<ksni::Icon>; 3]> = OnceLock::new();
        let [ready, scanning, attention] = ICONS.get_or_init(|| {
            State::ALL.map(icon::tray_icons)
        });
        match self.state() {
            State::Ready => ready.clone(),
            State::Scanning => scanning.clone(),
            State::Attention => attention.clone(),
        }
    }

    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon_pixmap()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "vinoAuthFace".into(),
            description: self.summary(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        menu::build(self)
    }
}

#[cfg(test)]
pub mod fixtures {
    use super::*;
    use face_auth_tray::helper::SETTINGS;
    use std::collections::HashMap;
    use std::sync::mpsc;

    pub fn status(camera: Option<&str>, enrolled: Option<bool>) -> Status {
        Status {
            camera: camera.map(Into::into),
            enrolled,
            backend: "tract (CPU)".into(),
            model: "w600k_mbf (detector det_500m)".into(),
            login: None,
            settings: HashMap::new(),
            settings_window: false,
        }
    }

    pub fn tray(status: Status) -> Tray {
        Tray {
            tx: mpsc::channel().0,
            status,
            scanning: false,
            busy: None,
            update: None,
            hidden: false,
            idle_minutes: 30,
        }
    }

    /// Every situation the tray can be in that changes its text or menu.
    pub fn situations() -> Vec<(&'static str, Tray)> {
        let ready = || status(Some("/dev/video2"), Some(true));
        let mut all = vec![
            ("ready", tray(ready())),
            ("no camera", tray(status(None, Some(true)))),
            ("not enrolled", tray(status(Some("/dev/video2"), Some(false)))),
            ("enrolment unknown", tray(status(Some("/dev/video2"), None))),
        ];
        let mut t = tray(ready());
        t.scanning = true;
        all.push(("scanning", t));
        let mut t = tray(ready());
        t.busy = Some("Enrolling your face: Captured 3/30".into());
        all.push(("busy", t));
        let mut s = ready();
        s.settings_window = true;
        all.push(("settings window", tray(s)));
        // Every setting on each of its choices, plus a hand-set value.
        let mut s = ready();
        s.login = Some(LoginMode::Both);
        s.settings = SETTINGS.iter().map(|x| (x.key.to_string(), x.choices[0].id.to_string())).collect();
        let mut t = tray(s.clone());
        t.update = Some("v99".into());
        all.push(("settings, update", t));
        s.settings = SETTINGS.iter().map(|x| (x.key.to_string(), x.choices[x.choices.len() - 1].id.to_string())).collect();
        s.backend = "OpenVINO (NPU)".into();
        let mut t = tray(s.clone());
        t.update = Some("v99".into());
        t.idle_minutes = 7;
        all.push(("other choices, OpenVINO update", t));
        s.settings = SETTINGS.iter().map(|x| (x.key.to_string(), "custom".to_string())).collect();
        all.push(("hand-set values", tray(s)));
        all
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use ksni::Tray as _;

    #[test]
    fn the_item_names_itself() {
        let t = tray(status(Some("/dev/video2"), Some(true)));
        assert_eq!(t.id(), "vinoauthface");
        assert_eq!(t.title(), "vinoAuthFace");
        assert_eq!(t.category(), Category::ApplicationStatus);
        assert_eq!(t.tool_tip().title, "vinoAuthFace");
    }

    /// The icon has three looks; the text tells every situation apart, so
    /// nothing depends on seeing its colour.
    #[test]
    fn each_state_has_its_own_tooltip_text() {
        let mut seen: Vec<(String, String)> = Vec::new();
        for (name, t) in situations().into_iter().take(6) {
            let tip = t.tool_tip();
            assert!(!tip.description.trim().is_empty(), "{name} has no tooltip text");
            assert_eq!(tip.description, t.summary());
            if let Some((other, _)) = seen.iter().find(|(_, d)| *d == tip.description) {
                panic!("{name} and {other} both read {:?}", tip.description);
            }
            seen.push((name.to_string(), tip.description));
        }
        let names = State::ALL.map(State::icon_name);
        assert!(names[0] != names[1] && names[1] != names[2] && names[0] != names[2]);
    }
}
