//! The tray's menu, built from what the tray knows.

use super::tray::{Action, Msg, Tray};
use super::DOCS;
use face_auth_tray::helper::{Setting, SETTINGS};
use face_auth_tray::idle;
use face_auth_tray::status::{LoginMode, Status, SETTINGS_WINDOW};
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::MenuItem;
use std::process::{Command, Stdio};
use vino_ui::tray::MenuIcon;

/// The whole menu, top to bottom.
pub fn build(tray: &Tray) -> Vec<MenuItem<Tray>> {
        let s = &tray.status;
        let idle = tray.busy.is_none();
        let enrolled = s.enrolled == Some(true);
        let mut menu = Vec::new();
        if let Some(tag) = &tray.update {
            menu.push(item(&format!("Update to {tag}…"), MenuIcon::Update.name(), idle, |t| {
                t.run(Action::Upgrade)
            }));
            menu.push(MenuItem::Separator);
        }

        // Some hosts never show the tooltip, so the label carries the summary:
        // pinned by `the_status_entry_says_what_the_tooltip_says`.
        let plain = s.ready() && tray.busy.is_none() && !tray.scanning;
        menu.push(
            SubMenu {
                label: if plain { "Status".into() } else { format!("Status: {}", tray.summary()) },
                icon_name: if s.ready() { MenuIcon::Info } else { MenuIcon::Warning }.name().into(),
                submenu: s.lines().into_iter().map(info).collect(),
                ..Default::default()
            }
            .into(),
        );
        menu.push(MenuItem::Separator);

        if s.enrolled == Some(false) {
            menu.push(item("Enrol face…", MenuIcon::Add.name(), idle, |t| t.run(Action::Enrol)));
        } else {
            // Unknown is treated as enrolled: a one-click enrol could replace
            // templates the tray just failed to see.
            menu.push(item("Retrain face (add more frames)…", MenuIcon::Refresh.name(), idle, |t| t.run(Action::Retrain)));
            if enrolled {
                menu.push(item("Test scan", MenuIcon::Camera.name(), idle, |t| t.run(Action::Test)));
            }
        }

        if let Some(mode) = s.login {
            menu.push(
                SubMenu {
                    label: format!("Login screen: {}", mode.label()),
                    icon_name: MenuIcon::Users.name().into(),
                    submenu: vec![RadioGroup {
                        selected: LoginMode::ALL.iter().position(|m| *m == mode).unwrap_or(0),
                        select: Box::new(move |t: &mut Tray, i| {
                            if LoginMode::ALL[i] != mode {
                                t.run(Action::Login(LoginMode::ALL[i]))
                            }
                        }),
                        options: LoginMode::ALL
                            .map(|m| RadioItem { label: m.label().into(), enabled: idle, ..Default::default() })
                            .into(),
                    }
                    .into()],
                    ..Default::default()
                }
                .into(),
            );
        }

        if s.settings_window {
            // The window does its own pkexec, so it opens while an action runs.
            menu.push(item("Settings…", MenuIcon::Settings.name(), true, |_| {
                if let Err(e) = Command::new(SETTINGS_WINDOW).stdin(Stdio::null()).spawn() {
                    eprintln!("vinoauthface-tray: cannot start {SETTINGS_WINDOW}: {e}");
                }
            }));
        } else {
            menu.push(
                SubMenu {
                    label: "Settings".into(),
                    icon_name: MenuIcon::Settings.name().into(),
                    submenu: settings_menu(s, idle, tray.idle_minutes),
                    ..Default::default()
                }
                .into(),
            );
        }

        menu.push(MenuItem::Separator);
        menu.push(item("Open documentation", MenuIcon::Help.name(), true, |_| {
            let _ = Command::new("xdg-open").arg(DOCS).stdin(Stdio::null()).spawn();
        }));
        menu.push(item("Quit", MenuIcon::Quit.name(), true, |t| {
            let _ = t.tx.send(Msg::Quit);
        }));
        escape_underscores(&mut menu);
        menu
}

fn info(label: String) -> MenuItem<Tray> {
    StandardItem { label, enabled: false, ..Default::default() }.into()
}

fn item(label: &str, icon: &str, enabled: bool, activate: impl Fn(&mut Tray) + Send + 'static) -> MenuItem<Tray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        enabled,
        activate: Box::new(move |t: &mut Tray| {
            let _ = t.tx.send(Msg::Touch);
            activate(t)
        }),
        ..Default::default()
    }
    .into()
}

/// ksni hands labels to dbusmenu as they are, where "_" marks an access key
/// and is hidden; "__" shows one. The fixed labels avoid it, but the camera
/// path (a /dev/v4l/by-id name is full of them), a release tag or a value set
/// by hand in /etc/face-auth.toml can carry one.
fn escape_underscores(items: &mut [MenuItem<Tray>]) {
    let escape = |label: &mut String| *label = label.replace('_', "__");
    for item in items {
        match item {
            MenuItem::Standard(i) => escape(&mut i.label),
            MenuItem::Checkmark(i) => escape(&mut i.label),
            MenuItem::SubMenu(m) => {
                escape(&mut m.label);
                escape_underscores(&mut m.submenu);
            }
            MenuItem::RadioGroup(g) => g.options.iter_mut().for_each(|o| escape(&mut o.label)),
            MenuItem::Separator => {}
        }
    }
}

/// The Settings submenu, one level deep: an on/off setting is a checkmark,
/// any other a heading over its radio choices. The system settings come
/// first, when `setting-mode.sh` is there to read them; the tray's own idle
/// delay always shows.
fn settings_menu(status: &Status, idle: bool, idle_minutes: u64) -> Vec<MenuItem<Tray>> {
    let mut menu = Vec::new();
    for setting in SETTINGS.iter().filter(|_| !status.settings.is_empty()) {
        if let Some(item) = setting_toggle(setting, status, idle) {
            menu.push(item);
            continue;
        }
        if !menu.is_empty() {
            menu.push(MenuItem::Separator);
        }
        menu.extend(setting_radio(setting, status, idle));
    }
    if !menu.is_empty() {
        menu.push(MenuItem::Separator);
    }
    menu.extend(idle_radio(idle_minutes));
    menu
}

/// How long the icon waits before tucking itself away. Saved as the user, so
/// it needs no password and works while an action runs.
fn idle_radio(minutes: u64) -> [MenuItem<Tray>; 2] {
    let selected = idle::CHOICES.iter().position(|(m, _)| *m == minutes);
    let heading = match selected {
        Some(_) => "Hide icon when idle".to_string(),
        None => format!("Hide icon when idle: {minutes} min"),
    };
    [
        info(heading),
        RadioGroup {
            selected: selected.unwrap_or(usize::MAX),
            select: Box::new(move |t: &mut Tray, i| {
                let _ = t.tx.send(Msg::IdleMinutes(idle::CHOICES[i].0));
            }),
            options: idle::CHOICES.iter().map(|(_, label)| RadioItem { label: (*label).into(), ..Default::default() }).collect(),
        }
        .into(),
    ]
}

/// The index of a setting's current choice, and the word to show for it. A
/// value set by hand that the menu doesn't offer shows as "custom" with
/// nothing selected.
fn current_choice(setting: &Setting, status: &Status) -> (Option<usize>, &'static str) {
    let current = status.settings.get(setting.key).map(String::as_str);
    let selected = setting.choices.iter().position(|c| Some(c.id) == current);
    let shown = match (selected, current) {
        (Some(i), _) => setting.choices[i].label,
        (None, Some("custom")) => "custom",
        _ => "unknown",
    };
    (selected, shown)
}

/// A setting whose choices are exactly "on" and "off", as one checkmark.
fn setting_toggle(setting: &'static Setting, status: &Status, idle: bool) -> Option<MenuItem<Tray>> {
    if setting.choices.len() != 2 {
        return None;
    }
    let on = setting.choices.iter().find(|c| c.id == "on")?;
    let off = setting.choices.iter().find(|c| c.id == "off")?;
    let current = status.settings.get(setting.key).map(String::as_str);
    let label = match current {
        Some("on" | "off") => setting.title.to_string(),
        Some(other) => format!("{} ({other})", setting.title),
        None => format!("{} (unknown)", setting.title),
    };
    let checked = current == Some("on");
    Some(
        CheckmarkItem {
            label,
            enabled: idle,
            checked,
            activate: Box::new(move |t: &mut Tray| t.run(Action::Set(setting, if checked { off } else { on }))),
            ..Default::default()
        }
        .into(),
    )
}

/// A heading with the current choice, then the choices as radio items.
fn setting_radio(setting: &'static Setting, status: &Status, idle: bool) -> [MenuItem<Tray>; 2] {
    let (selected, shown) = current_choice(setting, status);
    let heading = if selected.is_some() { setting.title.to_string() } else { format!("{}: {shown}", setting.title) };
    [
        info(heading),
        RadioGroup {
            selected: selected.unwrap_or(usize::MAX),
            select: Box::new(move |t: &mut Tray, i| {
                if Some(i) != selected {
                    t.run(Action::Set(setting, &setting.choices[i]))
                }
            }),
            options: setting
                .choices
                .iter()
                .map(|c| RadioItem { label: c.label.into(), enabled: idle, ..Default::default() })
                .collect(),
        }
        .into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tray::fixtures::*;
    use ksni::Tray as _;

    #[test]
    fn setting_labels_avoid_the_access_key_marker() {
        // ksni treats "_" as an access-key marker.
        for setting in SETTINGS {
            assert!(!setting.title.contains('_'));
            assert!(setting.choices.iter().all(|c| !c.label.contains('_')));
        }
        assert!(idle::CHOICES.iter().all(|(_, label)| !label.contains('_')));
    }

    /// Every label a screen reader could land on, submenus included.
    fn all_labels(items: &[MenuItem<Tray>], out: &mut Vec<String>) {
        for item in items {
            match item {
                MenuItem::Standard(i) => out.push(i.label.clone()),
                MenuItem::Checkmark(i) => out.push(i.label.clone()),
                MenuItem::SubMenu(m) => {
                    out.push(m.label.clone());
                    all_labels(&m.submenu, out);
                }
                MenuItem::RadioGroup(g) => out.extend(g.options.iter().map(|o| o.label.clone())),
                MenuItem::Separator => {}
            }
        }
    }

    #[test]
    fn the_menu_ends_with_quit_and_leaves_the_other_apps_to_the_settings_window() {
        for settings_window in [true, false] {
            let mut s = status(Some("/dev/video2"), Some(true));
            s.settings_window = settings_window;
            let menu = tray(s).menu();
            let [.., MenuItem::Standard(quit)] = menu.as_slice() else {
                panic!("the menu should end with Quit");
            };
            assert_eq!(quit.label, "Quit");
            // The settings window's About page links to vinopilot.ca and the
            // other vinoPilot apps; the tray doesn't, at any level.
            let mut labels = Vec::new();
            all_labels(&menu, &mut labels);
            assert!(
                !labels.iter().any(|l| l.contains("vinoPilot") || l.contains("vinopilot.ca") || l == "vinoWhisper"),
                "{labels:?}"
            );
        }
    }

    #[test]
    fn the_settings_submenu_ends_with_the_idle_delay() {
        let menu = tray(status(Some("/dev/video2"), Some(true))).menu();
        let settings = menu
            .iter()
            .find_map(|i| match i {
                MenuItem::SubMenu(m) if m.label == "Settings" => Some(m),
                _ => None,
            })
            .expect("without the settings window, Settings is a submenu");
        assert!(
            matches!(settings.submenu.last(), Some(MenuItem::RadioGroup(_))),
            "the Settings submenu should end with the idle delay's choices"
        );
    }

    /// What a screen reader would read for one item: its label with
    /// dbusmenu's access-key markers taken out.
    fn spoken(label: &str) -> String {
        label.replace("__", "\u{0}").replace('_', "").replace('\u{0}', "_")
    }

    /// One item: labelled, shown, and with any underscore kept.
    fn check(label: &str, visible: bool, out: &mut Vec<String>) {
        assert!(!spoken(label).trim().is_empty(), "an item has no label: {label:?}");
        assert!(visible, "{label:?} is hidden instead of disabled");
        // A lone "_" would be eaten as an access key.
        assert!(!label.replace("__", "").contains('_'), "{label:?} loses an underscore");
        out.push(spoken(label));
    }

    /// Every label in a menu, depth first, checking each item on the way.
    fn walk(items: &[MenuItem<Tray>], out: &mut Vec<String>) {
        for item in items {
            match item {
                MenuItem::Standard(i) => check(&i.label, i.visible, out),
                MenuItem::Checkmark(i) => check(&i.label, i.visible, out),
                MenuItem::SubMenu(m) => {
                    check(&m.label, m.visible, out);
                    assert!(!m.submenu.is_empty(), "{:?} opens onto nothing", m.label);
                    walk(&m.submenu, out);
                }
                MenuItem::RadioGroup(g) => {
                    assert!(g.selected < g.options.len() || g.selected == usize::MAX, "radio selection out of range");
                    g.options.iter().for_each(|o| check(&o.label, o.visible, out));
                }
                MenuItem::Separator => {}
            }
        }
    }

    fn labels(t: &Tray) -> Vec<String> {
        let mut out = Vec::new();
        walk(&t.menu(), &mut out);
        out
    }

    #[test]
    fn settings_opens_the_window_when_it_is_installed() {
        let mut s = status(Some("/dev/video2"), Some(true));
        s.settings_window = true;
        let labels = labels(&tray(s.clone()));
        assert!(labels.contains(&"Settings…".to_string()), "{labels:?}");
        assert!(!labels.iter().any(|l| l.starts_with("Hide icon when idle")), "{labels:?}");
        s.settings_window = false;
        let labels = super::tests::labels(&tray(s));
        assert!(labels.contains(&"Settings".to_string()) && labels.iter().any(|l| l.starts_with("Hide icon when idle")), "{labels:?}");
    }

    #[test]
    fn every_menu_item_has_a_label() {
        for (name, t) in situations() {
            let labels = labels(&t);
            assert!(labels.contains(&"Quit".to_string()), "{name}: {labels:?}");
        }
    }

    /// The menu says the same, for hosts that show no tooltip.
    #[test]
    fn the_status_entry_says_what_the_tooltip_says() {
        for (name, t) in situations().into_iter().take(6) {
            let status = labels(&t).into_iter().find(|l| l.starts_with("Status")).unwrap();
            if name == "ready" {
                assert_eq!(status, "Status");
            } else {
                assert_eq!(status, format!("Status: {}", t.summary()), "{name}");
            }
        }
    }

    #[test]
    fn a_running_action_greys_the_actions_and_says_why() {
        let (_, t) = situations().into_iter().find(|(n, _)| *n == "busy").unwrap();
        for item in t.menu() {
            match item {
                MenuItem::Standard(i) if i.label.ends_with('…') || i.label == "Test scan" => {
                    assert!(!i.enabled, "{} is live while busy", i.label)
                }
                MenuItem::SubMenu(m) if m.label.starts_with("Status") => {
                    assert!(m.label.contains("Captured 3/30"), "{}", m.label)
                }
                _ => {}
            }
        }
    }

    #[test]
    fn literal_underscores_are_kept() {
        let mut s = status(Some("/dev/v4l/by-id/usb-SunplusIT_Inc_Integrated_IR_Camera-video-index0"), Some(true));
        s.settings = [("confirm".to_string(), "my_value".to_string())].into();
        let mut t = tray(s);
        t.update = Some("v9_rc".into());
        let labels = labels(&t);
        for wanted in [
            "Camera: /dev/v4l/by-id/usb-SunplusIT_Inc_Integrated_IR_Camera-video-index0",
            "Update to v9_rc…",
            "Confirm sudo with Enter (my_value)",
        ] {
            assert!(labels.iter().any(|l| l == wanted), "{wanted:?} not in {labels:?}");
        }
    }

    #[test]
    fn on_off_settings_are_checkmarks_and_the_rest_radio_groups() {
        let mut s = status(Some("/dev/video2"), Some(true));
        for setting in SETTINGS {
            for (i, choice) in setting.choices.iter().enumerate() {
                s.settings = [(setting.key.to_string(), choice.id.to_string())].into();
                match setting_toggle(setting, &s, true) {
                    Some(MenuItem::Checkmark(c)) => {
                        assert_eq!(c.checked, choice.id == "on", "{} {}", setting.key, choice.id);
                        assert_eq!(c.label, setting.title);
                    }
                    Some(_) => panic!("{} is not a checkmark", setting.key),
                    None => {
                        let [heading, group] = setting_radio(setting, &s, true);
                        let MenuItem::Standard(heading) = heading else { panic!("{} has no heading", setting.key) };
                        assert_eq!(heading.label, setting.title);
                        let MenuItem::RadioGroup(group) = group else { panic!("{} is not a radio group", setting.key) };
                        assert_eq!(group.selected, i);
                        assert_eq!(group.options.len(), setting.choices.len());
                    }
                }
            }
        }
        // A value the menu doesn't offer selects nothing and says so.
        let security = SETTINGS.iter().find(|x| x.key == "security").unwrap();
        s.settings = [("security".to_string(), "custom".to_string())].into();
        let [MenuItem::Standard(heading), MenuItem::RadioGroup(group)] = setting_radio(security, &s, true) else {
            panic!()
        };
        assert_eq!(heading.label, "Security: custom");
        assert_eq!(group.selected, usize::MAX);
        // The idle delay is a radio group on the current choice.
        let [_, MenuItem::RadioGroup(group)] = idle_radio(60) else { panic!() };
        assert_eq!(group.selected, idle::CHOICES.iter().position(|(m, _)| *m == 60).unwrap());
    }
}
