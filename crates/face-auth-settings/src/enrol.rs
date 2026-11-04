//! Guided enrolment from the window: `vinoauthface-helper enrol-guided` (or
//! `retrain-guided`) through pkexec, its stdout read line by line into what
//! the Face page shows, and its stdin kept open to skip a step or cancel.
//! No camera preview: the frames never leave the root process.

use face_auth_core::guided::{Hint, Step};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::apply::HELPER;

const FACE_AUTH: &str = "/usr/local/bin/vinoauthface-auth";

#[derive(Default)]
pub struct Run {
    pub running: bool,
    /// The step under way: its number from 1, how many there are, which.
    pub step: Option<(usize, usize, Step)>,
    /// Kept and wanted in the current step.
    pub kept: (usize, usize),
    pub hint: Option<Hint>,
    pub skipped: Vec<Step>,
    /// What the CLI said it saved, read before it exits.
    saved: Option<String>,
    /// How it ended, once the process has exited: what it saved, or why not.
    pub done: Option<Result<String, String>>,
    stdin: Option<ChildStdin>,
}

impl Run {
    /// One line of `vinoauthface enroll --guided`'s output.
    pub fn read(&mut self, line: &str) {
        let line = line.trim();
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let id = rest.split([':', ' ']).next().unwrap_or("");
        match word {
            "step" => {
                let (count, id) = rest.split_once(' ').unwrap_or((rest, ""));
                let id = id.split(':').next().unwrap_or("");
                if let (Some((n, total)), Some(step)) = (count.split_once('/'), Step::parse(id)) {
                    if let (Ok(n), Ok(total)) = (n.parse(), total.parse()) {
                        self.step = Some((n, total, step));
                        self.kept = (0, step.wanted());
                        self.hint = None;
                    }
                }
            }
            "hint" => self.hint = Hint::parse(id),
            "kept" => {
                if let Some((k, w)) = id.split_once('/') {
                    if let (Ok(k), Ok(w)) = (k.parse(), w.parse()) {
                        self.kept = (k, w);
                        self.hint = None;
                    }
                }
            }
            "skipped" => {
                if let Some(step) = Step::parse(id) {
                    self.skipped.push(step);
                }
            }
            "Saved" | "Added" => self.saved = Some(line.to_string()),
            _ => {}
        }
    }
}

pub type Shared = Arc<Mutex<Run>>;

/// Starts the enrolment process; tests swap it for one that needs no root.
pub type Spawner = fn(append: bool) -> std::io::Result<Child>;

pub fn pkexec(append: bool) -> std::io::Result<Child> {
    Command::new("pkexec")
        .arg(HELPER)
        .arg(if append { "retrain-guided" } else { "enrol-guided" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

pub fn start(shared: &Shared, spawner: Spawner, append: bool, ctx: &eframe::egui::Context) {
    let mut child = match spawner(append) {
        Ok(child) => child,
        Err(e) => {
            let mut run = shared.lock().unwrap();
            *run = Run { done: Some(Err(format!("pkexec couldn't be started: {e}"))), ..Run::default() };
            return;
        }
    };
    {
        let mut run = shared.lock().unwrap();
        *run = Run { running: true, stdin: child.stdin.take(), ..Run::default() };
    }
    let shared = Arc::clone(shared);
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let mut stderr = child.stderr.take();
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                shared.lock().unwrap().read(&line);
                ctx.request_repaint();
            }
        }
        let mut err = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut err);
        }
        let status = child.wait();
        let mut run = shared.lock().unwrap();
        run.running = false;
        run.stdin = None;
        let code = status.ok().and_then(|s| s.code());
        run.done = Some(match (code, run.saved.take()) {
            (Some(0), Some(saved)) => Ok(saved),
            (Some(0), None) => Ok("Saved.".into()),
            (Some(126) | Some(127), _) => Err("Nothing was enrolled: the password prompt was cancelled or failed.".into()),
            _ => {
                let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                let last = last.trim_start_matches("Error: ");
                Err(if last.contains("cancelled") || last.is_empty() {
                    "Stopped. Nothing was saved.".into()
                } else {
                    format!("Not enrolled: {last}")
                })
            }
        });
        ctx.request_repaint();
    });
}

/// Skip the current step, or stop the whole run without saving.
pub fn send(shared: &Shared, cancel: bool) {
    if let Some(stdin) = shared.lock().unwrap().stdin.as_mut() {
        let _ = stdin.write_all(if cancel { b"cancel\n" } else { b"\n" });
        let _ = stdin.flush();
    }
}

/// Whether the user has templates: `vinoauthface-auth --enrolled`, the
/// set-group-ID binary that can open the store, as the tray asks it.
/// `None` when it can't say.
pub fn enrolled() -> Option<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_cli_prints() {
        let mut run = Run::default();
        run.read("step 2/6 side: Turn your head a little to one side. (Enter skips)");
        assert_eq!(run.step, Some((2, 6, Step::Side)));
        assert_eq!(run.kept, (0, 2));
        run.read("  hint turn-more: Turn a little further.");
        assert_eq!(run.hint, Some(Hint::TurnMore));
        run.read("  kept 1/2");
        assert_eq!((run.kept, run.hint), ((1, 2), None));
        run.read("  skipped glasses");
        assert_eq!(run.skipped, [Step::Glasses]);
        run.read("Saved 9 embeddings for 'dev'");
        assert_eq!(run.saved.as_deref(), Some("Saved 9 embeddings for 'dev'"));
        assert_eq!(run.done, None, "not done until the process exits");
        // Whatever else it says is left alone.
        run.read("Enrolling 'dev'");
        run.read("step x/y nonsense");
        assert_eq!(run.step, Some((2, 6, Step::Side)));
    }
}
