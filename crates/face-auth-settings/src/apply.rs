//! Apply: the new `/etc/face-auth.toml` to `vinoauthface-helper write-config`
//! through pkexec, off the UI thread, since the password prompt waits for
//! the user.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Where deploy.sh installs the tray's root helper; polkit pins this path.
pub const HELPER: &str = "/usr/local/libexec/vinoauthface-helper";

/// Runs the helper with a payload from `config_write::frame`; tests swap it.
pub type Runner = fn(&str) -> Result<String, String>;

#[derive(Default)]
pub struct State {
    pub running: bool,
    /// What the last Apply said: the helper's words either way.
    pub result: Option<Result<String, String>>,
}

pub fn start(state: &Arc<Mutex<State>>, runner: Runner, payload: String, ctx: &eframe::egui::Context) {
    state.lock().unwrap().running = true;
    let state = Arc::clone(state);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = runner(&payload);
        let mut state = state.lock().unwrap();
        state.running = false;
        state.result = Some(result);
        ctx.request_repaint();
    });
}

pub fn helper_installed() -> bool {
    std::path::Path::new(HELPER).is_file()
}

pub fn pkexec(payload: &str) -> Result<String, String> {
    let mut child = Command::new("pkexec")
        .arg(HELPER)
        .arg("write-config")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("pkexec couldn't be started: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A refused prompt closes the pipe early; the exit status says why.
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        // pkexec's own: dismissed, or the password was wrong.
        Some(126) | Some(127) => Err("Nothing was changed: the password prompt was cancelled or failed.".into()),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim().trim_start_matches("vinoauthface-helper: ");
            Err(if err.is_empty() { "The helper failed and said nothing.".into() } else { format!("Not saved: {err}") })
        }
    }
}
