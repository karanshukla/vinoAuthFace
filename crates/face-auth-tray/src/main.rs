//! vinoAuthFace's tray icon: status, enrol, retrain, test, uninstall.
//!
//! A per-user session app, never in the authentication path. It runs
//! unprivileged and elevates each action that touches the template store
//! through pkexec and `vinoauthface-helper`. It learns about scans only by
//! looking for the process in `/proc` (see `scanning.rs`): `vinoauthface-auth` does not
//! know the tray exists.

use face_auth_core::{capture, user, Camera, FaceAuthConfig};
use face_auth_tray::helper::{Verb, FACE_AUTH, HELPER, SAFE_PATH};
use face_auth_tray::icon::{self, State};
use face_auth_tray::{progress, scanning};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Category, MenuItem, ToolTip};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const DOCS: &str = "https://github.com/karanshukla/vinoAuthFace/tree/main/docs";
/// How often `/proc` is checked for a running scan.
const SCAN_POLL: Duration = Duration::from_millis(200);
/// How often enrolment is re-read, to catch a `sudo vinoauthface enroll` run by hand.
const STATUS_POLL: Duration = Duration::from_secs(30);
/// pkexec's exit codes for a dismissed or refused prompt.
const PKEXEC_CANCELLED: [i32; 2] = [126, 127];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Enrol,
    Retrain,
    Test,
    Uninstall,
}

enum Msg {
    Run(Action),
    /// A worker finished; re-read the status.
    Done,
    Quit,
}

#[derive(Debug, Clone, Default)]
struct Status {
    /// The IR camera vinoauthface-auth would use, if one opens.
    camera: Option<String>,
    /// `None` when vinoauthface-auth is missing or could not answer.
    enrolled: Option<bool>,
    backend: String,
}

impl Status {
    fn read() -> Status {
        let config = FaceAuthConfig::load_system().ok();
        // The system config's device is trusted as is: it is often
        // pin-camera.sh's /dev/face-auth-ir symlink, which has no sysfs node
        // of its own for the stricter IR checks to read.
        let camera = config.as_ref().and_then(|c| match &c.device {
            Some(device) => Camera::open(device).is_ok().then(|| device.clone()),
            None => capture::detect_ir_camera(),
        });
        let backend = match config.as_ref().map(|c| (c.backend(), c.npu_device())) {
            Some((b, device)) if b == "openvino" => format!("OpenVINO ({device})"),
            Some(_) => "tract (CPU)".into(),
            None => "unknown (cannot read /etc/face-auth.toml)".into(),
        };
        Status { camera, enrolled: enrolled(), backend }
    }

    fn ready(&self) -> bool {
        self.camera.is_some() && self.enrolled == Some(true)
    }
}

/// Asks the set-group-ID vinoauthface-auth, which can open the store the user cannot.
fn enrolled() -> Option<bool> {
    let status = Command::new(FACE_AUTH)
        .arg("--enrolled")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Desktop notifications over the session bus. Best effort: without a
/// notification server the tooltip still carries the same text.
struct Notifier {
    bus: Option<zbus::blocking::Connection>,
}

impl Notifier {
    fn new() -> Notifier {
        Notifier { bus: zbus::blocking::Connection::session().ok() }
    }

    /// Show, or with a non-zero `replaces` update, a notification. Returns
    /// its ID for the next update.
    fn send(&self, replaces: u32, summary: &str, body: &str) -> u32 {
        let Some(bus) = &self.bus else { return 0 };
        let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
        let reply = bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("vinoAuthFace", replaces, "vinoauthface", summary, body, Vec::<&str>::new(), hints, -1i32),
        );
        reply.ok().and_then(|m| m.body().deserialize::<u32>().ok()).unwrap_or(0)
    }
}

/// A destructive entry clicked once, waiting for the second click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Enrol,
    Uninstall,
}

struct Tray {
    tx: Sender<Msg>,
    status: Status,
    scanning: bool,
    /// What a running action is doing, for the tooltip. Actions are refused
    /// while one runs.
    busy: Option<String>,
    confirm: Option<Confirm>,
}

impl Tray {
    fn state(&self) -> State {
        if self.scanning {
            State::Scanning
        } else if self.status.ready() {
            State::Ready
        } else {
            State::Attention
        }
    }

    fn summary(&self) -> String {
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

    fn run(&mut self, action: Action) {
        self.confirm = None;
        if self.busy.is_none() {
            let _ = self.tx.send(Msg::Run(action));
        }
    }
}

fn info(label: String) -> MenuItem<Tray> {
    StandardItem { label, enabled: false, ..Default::default() }.into()
}

fn item(label: &str, icon: &str, enabled: bool, activate: impl Fn(&mut Tray) + Send + 'static) -> MenuItem<Tray> {
    StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        enabled,
        activate: Box::new(activate),
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "vinoauthface".into()
    }

    fn title(&self) -> String {
        "vinoAuthFace".into()
    }

    // Left click opens the menu, which is the status panel; without this
    // it does nothing.
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
        let s = &self.status;
        let idle = self.busy.is_none();
        let enrolled = s.enrolled == Some(true);
        // "ksni" treats "_" as an access-key marker, so labels avoid it.
        let mut menu = vec![
            info(self.summary()),
            info(match &s.camera {
                Some(device) => format!("Camera: {device}"),
                None => "Camera: no IR camera found".into(),
            }),
            info(format!(
                "Face: {}",
                match s.enrolled {
                    Some(true) => "enrolled",
                    Some(false) => "not enrolled",
                    None => "unknown (is vinoauthface installed?)",
                }
            )),
            info(format!("Backend: {}", s.backend)),
            MenuItem::Separator,
        ];

        if s.enrolled == Some(false) {
            menu.push(item("Enrol face…", "list-add-user", idle, |t| t.run(Action::Enrol)));
        } else {
            // Unknown is treated as enrolled: a one-click enrol could replace
            // templates the tray just failed to see.
            menu.push(item("Retrain face (add more frames)…", "view-refresh", idle, |t| t.run(Action::Retrain)));
            if enrolled {
                menu.push(item("Test scan", "camera-web", idle, |t| t.run(Action::Test)));
            }
            // Re-enrolling throws the existing templates away, so it takes a
            // second click.
            if self.confirm == Some(Confirm::Enrol) {
                menu.push(item("Replace your enrolled face? Click to confirm", "dialog-warning", idle, |t| {
                    t.run(Action::Enrol)
                }));
            } else {
                menu.push(item("Enrol again from scratch…", "list-add-user", idle, |t| {
                    t.confirm = Some(Confirm::Enrol)
                }));
            }
        }

        menu.push(MenuItem::Separator);
        if self.confirm == Some(Confirm::Uninstall) {
            menu.push(item("Uninstall vinoAuthFace? Click to confirm", "dialog-warning", idle, |t| {
                t.run(Action::Uninstall)
            }));
        } else {
            menu.push(item("Uninstall vinoAuthFace…", "edit-delete", idle, |t| {
                t.confirm = Some(Confirm::Uninstall)
            }));
        }
        if self.confirm.is_some() {
            menu.push(item("Cancel", "dialog-cancel", true, |t| t.confirm = None));
        }
        menu.push(MenuItem::Separator);
        menu.push(item("Open documentation", "help-contents", true, |_| {
            let _ = Command::new("xdg-open").arg(DOCS).stdin(Stdio::null()).spawn();
        }));
        menu.push(item("Quit", "application-exit", true, |t| {
            let _ = t.tx.send(Msg::Quit);
        }));
        menu
    }
}

/// Run `pkexec vinoauthface-helper <verb>`, reading vinoauthface enroll's progress off
/// its stdout. `Ok(None)` when the password prompt was dismissed.
fn run_helper(
    verb: Verb,
    mut on_line: impl FnMut(&str),
) -> std::io::Result<Option<(bool, String)>> {
    let mut child = Command::new("pkexec")
        .args([HELPER, verb.arg()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = e.read_to_string(&mut text);
            text
        })
    });
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            on_line(&line);
        }
    }
    let status = child.wait()?;
    let stderr = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    if status.code().is_some_and(|c| PKEXEC_CANCELLED.contains(&c)) {
        return Ok(None);
    }
    Ok(Some((status.success(), last_line(&stderr))))
}

fn last_line(text: &str) -> String {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_owned()
}

/// One live verify as the user, the same way the lock screen runs it. Counts
/// toward the lockout like any other failed scan, so it is not a free oracle.
fn test_scan(user: &str) -> std::io::Result<(bool, String)> {
    let out = Command::new(FACE_AUTH)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .env("PAM_USER", user)
        .env("PAM_SERVICE", "vinoauthface-tray")
        .stdin(Stdio::null())
        .output()?;
    Ok((out.status.success(), last_line(&String::from_utf8_lossy(&out.stderr))))
}

fn perform(action: Action, handle: &Handle<Tray>, notifier: &Notifier, user: &str) -> bool {
    let set_busy = |text: Option<String>| {
        handle.update(|t| t.busy = text);
    };
    match action {
        Action::Test => {
            set_busy(Some("Testing: look at the camera".into()));
            let id = notifier.send(0, "Test scan", "Look at the camera");
            match test_scan(user) {
                Ok((true, _)) => notifier.send(id, "Face recognised", "Face unlock works for you."),
                Ok((false, err)) if !err.is_empty() => notifier.send(id, "Test scan failed", &err),
                Ok((false, _)) => notifier.send(
                    id,
                    "Face not recognised",
                    "No match within the scan window. Try Retrain in different light, or check the camera.",
                ),
                Err(e) => notifier.send(id, "Cannot run vinoauthface", &e.to_string()),
            };
        }
        Action::Enrol | Action::Retrain => {
            let (verb, title) = if action == Action::Enrol {
                (Verb::Enrol, "Enrolling your face")
            } else {
                (Verb::Retrain, "Retraining your face")
            };
            set_busy(Some(format!("{title}: waiting for authentication")));
            let mut id = 0;
            let mut finished = None;
            let result = run_helper(verb, |line| {
                let Some(event) = progress::parse(line) else { return };
                let text = progress::describe(&event);
                if let progress::Event::Finished(summary) = &event {
                    finished = Some(summary.clone());
                }
                set_busy(Some(format!("{title}: {text}")));
                id = notifier.send(id, title, &text);
            });
            match result {
                Ok(None) => {}
                Ok(Some((true, _))) => {
                    let body = finished.unwrap_or_else(|| "Done.".into());
                    notifier.send(id, "Face enrolled", &format!("{body}. Use Test scan to check it."));
                }
                Ok(Some((false, err))) => {
                    notifier.send(id, &format!("{title} failed"), &err);
                }
                Err(e) => {
                    notifier.send(id, "Cannot run pkexec", &e.to_string());
                }
            }
        }
        Action::Uninstall => {
            set_busy(Some("Uninstalling: waiting for authentication".into()));
            match run_helper(Verb::Uninstall, |_| {}) {
                Ok(None) => {}
                Ok(Some((true, _))) => {
                    notifier.send(
                        0,
                        "vinoAuthFace uninstalled",
                        "Your enrolled face was kept in /var/lib/face-auth. Remove it with sudo rm -rf /var/lib/face-auth.",
                    );
                    return true;
                }
                Ok(Some((false, err))) => {
                    notifier.send(0, "Uninstall failed", &err);
                }
                Err(e) => {
                    notifier.send(0, "Cannot run pkexec", &e.to_string());
                }
            }
        }
    }
    set_busy(None);
    false
}

fn event_loop(handle: Handle<Tray>, rx: Receiver<Msg>, tx: Sender<Msg>, user: String) {
    let proc_root = Path::new("/proc");
    let mut last_status = Instant::now();
    let mut scanning = false;
    loop {
        match rx.recv_timeout(SCAN_POLL) {
            Ok(Msg::Quit) => return,
            Ok(Msg::Run(action)) => {
                let (handle, tx, user) = (handle.clone(), tx.clone(), user.clone());
                handle.clone().update(|t| t.busy = Some("Starting…".into()));
                std::thread::spawn(move || {
                    let uninstalled = perform(action, &handle, &Notifier::new(), &user);
                    let _ = tx.send(if uninstalled { Msg::Quit } else { Msg::Done });
                });
            }
            Ok(Msg::Done) => {
                let status = Status::read();
                handle.update(|t| t.status = status);
                last_status = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }

        let now = scanning::face_auth_running(proc_root);
        if now != scanning {
            scanning = now;
            handle.update(|t| t.scanning = now);
        }
        // `vinoauthface-auth --enrolled` is itself a scan process, so it only
        // runs from here, between two /proc checks, never alongside one.
        if !scanning && last_status.elapsed() >= STATUS_POLL {
            let enrolled = enrolled();
            handle.update(|t| t.status.enrolled = enrolled);
            last_status = Instant::now();
        }
    }
}

/// Leaves the launching terminal: a tray started from a shell would otherwise
/// die with it (SIGHUP on close). Autostart and launchers have no tty and skip
/// this. Must run before any thread exists, since it forks.
fn detach_from_terminal() {
    // SAFETY: single-threaded here; the child only calls async-signal-safe libc.
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) != 1 && libc::isatty(libc::STDERR_FILENO) != 1 {
            return;
        }
        match libc::fork() {
            -1 => return,
            0 => {}
            _ => libc::_exit(0),
        }
        libc::setsid();
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                libc::dup2(null, fd);
            }
            if null > libc::STDERR_FILENO {
                libc::close(null);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    detach_from_terminal();
    let me = user::current()?;
    let (tx, rx) = mpsc::channel();
    let tray = Tray {
        tx: tx.clone(),
        status: Status::read(),
        scanning: false,
        busy: None,
        confirm: None,
    };
    // Assumed, not checked: at login this can start before Plasma's tray does.
    let handle = tray.assume_sni_available(true).spawn()?;
    event_loop(handle.clone(), rx, tx, me.name);
    handle.shutdown().wait();
    Ok(())
}
