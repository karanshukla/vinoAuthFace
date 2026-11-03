//! The window as a screen reader sees it: every page rendered headless through egui_kittest,
//! which reads egui's AccessKit tree, then found and driven by role and name.

use std::sync::{Mutex, OnceLock};

use eframe::egui::accesskit::Role;
use eframe::egui::{accesskit, Theme};
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, Node};

use super::*;

const EXAMPLE: &str = include_str!("../../../../config/face-auth.toml.example");

/// A window over scratch copies of both files: the shipped example as
/// `/etc/face-auth.toml`, and `user` (if any) as the user's own. No test
/// reaches pkexec: Apply goes to `runner`.
fn window(page: Page, name: &str, user: Option<&str>, runner: apply::Runner) -> (SettingsWindow, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("vinoauthface-settings-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let system = dir.join("face-auth.toml");
    std::fs::write(&system, EXAMPLE).unwrap();
    let mine = dir.join("user").join("face-auth.toml");
    if let Some(text) = user {
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, text).unwrap();
    }
    let mut w = SettingsWindow::with(Some(page), system.clone(), mine.clone(), runner, true);
    w.spawner = fake_enrolment;
    w.check_enrolled = || Some(true);
    w.enrolled = Some(true);
    w.cameras = vec![("/dev/video2".into(), "Integrated IR Camera".into())];
    w.ir_cameras = vec!["/dev/video2".into()];
    (w, system, mine)
}

fn refuse(_: &str) -> Result<String, String> {
    Err("Not saved: no".into())
}

/// Where `accept` writes, as the helper would.
static ACCEPT_TO: OnceLock<PathBuf> = OnceLock::new();
static SENT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn accept(payload: &str) -> Result<String, String> {
    SENT.lock().unwrap().push(payload.to_string());
    let (_, new) = config_write::unframe(payload).map_err(|e| e.to_string())?;
    std::fs::write(ACCEPT_TO.get().unwrap(), new).map_err(|e| e.to_string())?;
    Ok("Saved".into())
}

fn harness(w: SettingsWindow, size: [f32; 2], theme: Theme) -> Harness<'static, SettingsWindow> {
    let mut styled = false;
    let mut h = Harness::builder().with_size(size).with_theme(theme).build_ui_state(
        move |ui, w: &mut SettingsWindow| {
            // Fonts set in a frame arrive with the next one, and the window needs its bold.
            if !styled {
                theme::apply(ui.ctx());
                theme::visible_scroll_handles(ui.ctx());
                styled = true;
                ui.ctx().request_repaint();
                return;
            }
            w.show(ui);
        },
        w,
    );
    settle(&mut h);
    h
}

const WIDE: [f32; 2] = [860.0, 660.0];
const NARROW: [f32; 2] = [380.0, 640.0];

fn settle(h: &mut Harness<'_, SettingsWindow>) {
    h.run_steps(3);
}

/// Frames until Apply, which answers from another thread, has been heard.
fn until_applied(h: &mut Harness<'_, SettingsWindow>) {
    for _ in 0..200 {
        settle(h);
        let state = h.state().apply.lock().unwrap();
        if !state.running && state.result.is_none() {
            drop(state);
            settle(h);
            return;
        }
        drop(state);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("Apply never answered");
}

/// Roles a user operates, and anything that takes focus; each needs a name of its own.
const CONTROLS: [Role; 9] = [
    Role::ScrollBar,
    Role::Button,
    Role::Switch,
    Role::RadioButton,
    Role::TextInput,
    Role::Slider,
    Role::SpinButton,
    Role::ComboBox,
    Role::Tab,
];

fn unnamed_controls(h: &Harness<'_, SettingsWindow>) -> Vec<String> {
    h.query_all_by(|node| {
        let focusable = node.data().supports_action(accesskit::Action::Focus);
        (focusable || CONTROLS.contains(&node.role())) && !node.is_hidden()
    })
    .filter(|node: &Node<'_>| node.accesskit_node().label().is_none_or(|label| label.trim().is_empty()))
    .map(|node| format!("{node:?} at {:?}", node.rect()))
    .collect()
}

#[test]
fn every_control_on_every_page_has_a_name() {
    for (size, theme) in [(WIDE, Theme::Light), (NARROW, Theme::Dark)] {
        for page in Page::ALL {
            let (w, _, _) = window(page, "names", Some("threshold = 0.7\n"), refuse);
            let h = harness(w, size, theme);
            let unnamed = unnamed_controls(&h);
            assert!(unnamed.is_empty(), "{} at {size:?}: unnamed controls {unnamed:#?}", page.title());
        }
    }
}

#[test]
fn the_pages_are_tabs_and_clicking_one_shows_it() {
    let (w, _, _) = window(Page::Matching, "tabs", None, refuse);
    let mut h = harness(w, WIDE, Theme::Dark);
    assert_eq!(h.root().accesskit_node().label().as_deref(), Some(TITLE));
    h.get_by_role_and_label(Role::TabList, "Pages");
    for page in Page::ALL {
        h.get_by_role_and_label(Role::Tab, page.title()).click();
        settle(&mut h);
        assert!(h.state().page == page, "{} didn't open", page.title());
        let titles = h.query_all_by_role_and_label(Role::Heading, page.title());
        assert_eq!(titles.filter(|t| t.accesskit_node().data().level() == Some(1)).count(), 1);
    }
}

#[test]
fn zoom_keeps_every_page_usable() {
    for page in Page::ALL {
        let (w, _, _) = window(page, "zoom", None, refuse);
        let mut h = harness(w, WIDE, Theme::Light);
        h.ctx.set_zoom_factor(2.0);
        settle(&mut h);
        // Twice the size is past the sidebar's breakpoint: the tabs take over.
        h.get_by_role_and_label(Role::Tab, page.title());
        assert!(unnamed_controls(&h).is_empty(), "{}", page.title());
    }
}

/// Showing a value is not changing it: the example's `threshold = 0.6`
/// widened to f64 once read as a change.
#[test]
fn opening_any_page_changes_nothing() {
    for size in [WIDE, NARROW] {
        for page in Page::ALL {
            let (w, system, mine) = window(page, "untouched", None, refuse);
            let h = harness(w, size, Theme::Light);
            assert_eq!(h.state().system.changes(), vec![], "{}", page.title());
            assert!(h.query_by_role_and_label(Role::Button, "Apply…").is_none());
            assert_eq!(std::fs::read_to_string(&system).unwrap(), EXAMPLE);
            assert!(!mine.exists(), "{} wrote the user's file", page.title());
        }
    }
}

#[test]
fn a_change_for_everyone_waits_for_apply() {
    let (w, system, _) = window(Page::Security, "apply", None, accept);
    ACCEPT_TO.set(system.clone()).unwrap();
    let mut h = harness(w, WIDE, Theme::Light);
    h.get_by_role_and_label(Role::Switch, "Skip over SSH").click();
    settle(&mut h);
    assert_eq!(h.state().system.changes(), vec![keys::ABORT_IF_SSH]);
    h.get_by_label("1 change for everyone on this computer");
    h.get_by_label("1 change not applied");
    assert_eq!(std::fs::read_to_string(&system).unwrap(), EXAMPLE, "written before Apply");

    h.get_by_role_and_label(Role::Button, "Apply…").click();
    until_applied(&mut h);
    let sent = SENT.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let (base, new) = config_write::unframe(&sent[0]).unwrap();
    assert_eq!(base, config_write::fingerprint(EXAMPLE));
    assert!(new.contains("\nguards.abort_if_ssh = false\n"), "{new}");
    // What the helper would accept, and nothing else moved.
    assert!(config_write::check(Some(EXAMPLE), &sent[0], |_, _| anyhow::bail!("no paths changed")).is_ok());
    assert_eq!(new.lines().count(), EXAMPLE.lines().count());

    // Read back from the file the helper wrote.
    assert_eq!(h.state().system.changes(), vec![]);
    assert!(!h.state().system.values().abort_if_ssh());
    h.get_by_label("Saved. The next unlock uses the new settings.");
    assert!(h.query_by_role_and_label(Role::Button, "Apply…").is_none());
}

#[test]
fn a_refused_apply_keeps_the_changes() {
    let (w, system, _) = window(Page::Matching, "refused", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    h.state_mut().set(false, keys::THRESHOLD, Some("0.7".into()));
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Apply…").click();
    until_applied(&mut h);
    h.get_by_label("Not saved: no");
    assert_eq!(h.state().system.changes(), vec![keys::THRESHOLD]);
    assert_eq!(std::fs::read_to_string(&system).unwrap(), EXAMPLE);
}

#[test]
fn discard_puts_everything_back() {
    let (w, _, _) = window(Page::Liveness, "discard", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    h.get_by_role_and_label(Role::RadioButton, "Strict").click();
    settle(&mut h);
    assert_eq!(h.state().system.changes(), vec![keys::LIVENESS_PRESET]);
    assert_eq!(h.state().system.values().liveness_residual_motion_threshold(), 0.3);
    h.get_by_role_and_label(Role::Button, "Discard").click();
    settle(&mut h);
    assert_eq!(h.state().system.changes(), vec![]);
    assert_eq!(h.state().system.text, EXAMPLE);
}

#[test]
fn without_the_helper_there_is_no_apply() {
    let (mut w, _, _) = window(Page::Matching, "nohelper", None, refuse);
    w.helper = false;
    let mut h = harness(w, WIDE, Theme::Light);
    h.state_mut().set(false, keys::THRESHOLD, Some("0.7".into()));
    settle(&mut h);
    let apply = h.get_by_role_and_label(Role::Button, "Apply…");
    assert!(apply.accesskit_node().is_disabled());
}

#[test]
fn your_values_save_to_your_file_at_once() {
    let (w, system, mine) = window(Page::You, "yours", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    h.get_by_label("This computer's 0.60 applies.");
    h.state_mut().set(true, keys::THRESHOLD, Some("0.7".into()));
    settle(&mut h);
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "threshold = 0.7\n");
    assert_eq!(std::fs::read_to_string(&system).unwrap(), EXAMPLE);
    assert!(h.query_by_role_and_label(Role::Button, "Apply…").is_none(), "nothing for everyone changed");
    h.get_by_label("This computer: 0.60.");
}

#[test]
fn a_looser_value_of_yours_says_it_is_ignored() {
    let (w, _, _) = window(Page::You, "looser", Some("threshold = 0.5\ndevice = \"/dev/video0\"\n"), refuse);
    let h = harness(w, WIDE, Theme::Light);
    h.get_by_label("Ignored: looser than this computer's 0.60.");
    h.get_by_label("Ignored: only a real IR sensor on this computer can be picked here.");
}

#[test]
fn a_new_model_says_everyone_enrols_again() {
    let (w, _, _) = window(Page::Models, "model", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    h.get_by_label("Made with w600k_mbf.onnx+det_500m.onnx. Faces enrolled with other models are refused.");
    h.state_mut().set(false, keys::MODEL_PATH, Some(keys::string("/usr/local/share/face-auth/mine.onnx")));
    settle(&mut h);
    h.get_by_label(
        "w600k_mbf.onnx+det_500m.onnx becomes mine.onnx+det_500m.onnx: after Apply, everyone enrols again (the \
         tray's Enrol again from scratch).",
    );
}

#[test]
fn a_model_path_users_can_write_is_flagged_before_apply() {
    let (w, _, _) = window(Page::Models, "untrusted", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    let dir = std::env::temp_dir().join(format!("vinoauthface-settings-model-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("mine.onnx");
    std::fs::write(&model, b"x").unwrap();
    h.state_mut().set(false, keys::MODEL_PATH, Some(keys::string(&model.display().to_string())));
    settle(&mut h);
    let flagged = h.query_all_by_label_contains("Apply will refuse this:");
    assert_eq!(flagged.count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stands in for `pkexec vinoauthface-helper enrol-guided`: one step, then
/// waits for a line on stdin. Enter saves; "cancel" stops as the CLI does.
fn fake_enrolment(append: bool) -> std::io::Result<std::process::Child> {
    let saved = if append { "Added 1 embeddings for 'dev' (11 total)" } else { "Saved 1 embeddings for 'dev'" };
    let script = format!(
        "echo 'step 1/6 straight: Look straight at the camera. (Enter skips)'; echo '  hint too-far: Move closer.'; \
         read l; if [ \"$l\" = cancel ]; then echo 'Error: enrolment cancelled; nothing was saved' >&2; exit 1; fi; \
         echo '  kept 1/2'; echo \"{saved}\""
    );
    std::process::Command::new("sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Frames until the fake enrolment shows `label`.
fn until_shown(h: &mut Harness<'_, SettingsWindow>, label: &str) {
    // By the clock, not a count of frames: under a full parallel test run the
    // shell that stands in for the CLI can take seconds to start.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        settle(h);
        if h.query_by_label(label).is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("{label:?} never showed");
}

#[test]
fn guided_enrolment_shows_each_step_and_its_hint() {
    let (mut w, _, _) = window(Page::Face, "guided", None, refuse);
    w.enrolled = Some(false);
    let mut h = harness(w, WIDE, Theme::Light);
    h.get_by_label("Not enrolled");
    h.get_by_role_and_label(Role::Button, "Start").click();
    until_shown(&mut h, "Look straight at the camera.");
    until_shown(&mut h, "Move closer.");
    h.get_by_label("Step 1 of 6, 0 of 2 kept.");
    assert!(unnamed_controls(&h).is_empty());
    h.get_by_role_and_label(Role::Button, "Skip this step").click();
    until_shown(&mut h, "Saved 1 embeddings for 'dev'");
    h.get_by_label("Enrolled");
}

#[test]
fn stopping_guided_enrolment_saves_nothing() {
    let (w, _, _) = window(Page::Face, "stop", None, refuse);
    let mut h = harness(w, WIDE, Theme::Light);
    // Enrolled already: adding poses is one click, replacing them two.
    h.get_by_role_and_label(Role::Button, "Enrol again…").click();
    settle(&mut h);
    h.get_by_label("Your current templates are replaced by the new ones.");
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    settle(&mut h);
    h.get_by_role_and_label(Role::Button, "Add").click();
    until_shown(&mut h, "Look straight at the camera.");
    h.get_by_role_and_label(Role::Button, "Stop").click();
    until_shown(&mut h, "Stopped. Nothing was saved.");
}
