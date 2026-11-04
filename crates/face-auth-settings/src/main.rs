//! vinoauthface-settings: every setting in `/etc/face-auth.toml` and
//! `~/.config/face-auth.toml`, in one window.

mod apply;
mod enrol;
mod files;
mod keys;
mod models;
mod window;

use std::process::ExitCode;

const USAGE: &str = "\
vinoauthface-settings: the vinoAuthFace settings window

Usage: vinoauthface-settings [--page face|matching|liveness|camera|models|security|you]
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => window::run(None),
        ["--page", page] => match window::Page::parse(page) {
            Some(page) => window::run(Some(page)),
            None => {
                eprint!("{USAGE}");
                ExitCode::from(2)
            }
        },
        ["-h" | "--help"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
