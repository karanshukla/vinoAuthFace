//! Replacing `/etc/face-auth.toml` from the settings window (fork-only).
//!
//! The window runs as the user and builds the new file; the tray's root
//! helper (`vinoauthface-helper write-config`, behind an `auth_admin` polkit
//! action) checks it here and writes it. Unlike the helper's other verbs, the
//! content comes from the caller, so the checks don't trust it:
//!
//! - it must parse and validate exactly as [`FaceAuthConfig::load_system`]
//!   reads the file, so a write can never leave PAM with a config it refuses;
//! - it must have been made from the file as it is now ([`fingerprint`]), so a
//!   second writer's change isn't lost;
//! - a model or template directory it names anew must be root's, all the way
//!   up ([`trusted_path`]): either decides whose face unlocks the account, so a
//!   path the user could write would make the password prompt a formality;
//! - it can't set a camera pin, only keep or remove one: `pin-camera.sh`
//!   derives the pin from sysfs, and a value typed in would be a guess.

use crate::config::FaceAuthConfig;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The most the helper reads. The shipped example is about 12 KiB.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

const HEADER: &str = "vinoauthface-settings base=";

/// FNV-1a 64 of a file's bytes, in hex: enough to tell that the file
/// changed between the window reading it and the helper writing it. Not a
/// security check; whoever can run the helper can pass any base.
pub fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What the window writes to the helper's stdin: a header naming the file it
/// edited (`None` when there was none), then the new file.
pub fn frame(base: Option<&str>, new: &str) -> String {
    let base = base.map_or_else(|| "none".to_string(), fingerprint);
    format!("{HEADER}{base}\n{new}")
}

/// The base fingerprint and the new file, from what [`frame`] made.
pub fn unframe(payload: &str) -> Result<(&str, &str)> {
    let (header, new) = payload.split_once('\n').context("no header line")?;
    let base = header.strip_prefix(HEADER).context("not a settings window request")?;
    Ok((base, new))
}

/// Check a new system config against the current one (`None`: no file yet).
/// `trusted` is [`trusted_path`] outside tests. Returns the text to write.
pub fn check(
    current: Option<&str>,
    payload: &str,
    trusted: impl Fn(&Path, bool) -> Result<()>,
) -> Result<String> {
    if payload.len() > MAX_CONFIG_BYTES {
        bail!("the new settings are larger than {MAX_CONFIG_BYTES} bytes");
    }
    let (base, new) = unframe(payload)?;
    let now = current.map_or_else(|| "none".to_string(), fingerprint);
    if base != now {
        bail!("/etc/face-auth.toml changed after the settings window read it; nothing was written");
    }
    let config = FaceAuthConfig::parse_system(new)?;
    // A current file that doesn't parse is being repaired: everything in the
    // new one counts as new.
    let old = current.and_then(|text| FaceAuthConfig::from_toml(text).ok());
    let old = old.as_ref();

    let paths = [
        ("model_path", &config.model_path, old.and_then(|o| o.model_path.as_ref()), false),
        (
            "detector_model_path",
            &config.detector_model_path,
            old.and_then(|o| o.detector_model_path.as_ref()),
            false,
        ),
        ("embeddings_dir", &config.embeddings_dir, old.and_then(|o| o.embeddings_dir.as_ref()), true),
    ];
    for (key, new, old, dir) in paths {
        if let Some(path) = new.as_ref().filter(|path| Some(*path) != old) {
            trusted(Path::new(path), dir).with_context(|| format!("{key} = {path:?} is refused"))?;
        }
    }

    let pin = (&config.pinned_camera_path, config.pinned_camera_index);
    let was = (old.and_then(|o| o.pinned_camera_path.as_ref()), old.and_then(|o| o.pinned_camera_index));
    let kept = pin.0.as_ref() == was.0 && pin.1 == was.1;
    if !kept && (pin.0.is_some() || pin.1.is_some()) {
        bail!("the camera pin can only be removed here; run pin-camera.sh to set one");
    }
    Ok(new.to_string())
}

/// Whether `path` (a directory when `dir`) is root's and nobody else can
/// change it or swap it: it and every directory above it, after symlinks
/// are resolved, owned by root and writable by nobody else.
pub fn trusted_path(path: &Path, dir: bool) -> Result<()> {
    trusted_path_for(path, dir, 0)
}

/// [`trusted_path`] with the owner a parameter, for tests that can't make
/// root's files.
pub fn trusted_path_for(path: &Path, dir: bool, owner: u32) -> Result<()> {
    if !path.is_absolute() {
        bail!("not an absolute path");
    }
    let real = std::fs::canonicalize(path).context("not found")?;
    let meta = std::fs::metadata(&real)?;
    if dir && !meta.is_dir() {
        bail!("{} is not a directory", real.display());
    }
    if !dir && !meta.is_file() {
        bail!("{} is not a regular file", real.display());
    }
    let mut at: Option<&Path> = Some(&real);
    while let Some(here) = at {
        let meta = std::fs::metadata(here)?;
        if meta.uid() != owner && meta.uid() != 0 {
            bail!("{} is not owned by root", here.display());
        }
        if meta.permissions().mode() & 0o022 != 0 {
            bail!("{} can be changed by users other than root", here.display());
        }
        at = here.parent();
    }
    Ok(())
}

/// Replace `path` with `text` all at once, keeping the old file's owner and
/// mode (0644 when there was none), as `setting-mode.sh` does.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("no parent directory")?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let old = std::fs::metadata(path).ok();
    let mode = old.as_ref().map_or(0o644, |m| m.permissions().mode() & 0o7777);
    let tmp: PathBuf = dir.join(format!(".{name}.settings-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        if let Some(old) = &old {
            std::os::unix::fs::fchown(&file, Some(old.uid()), Some(old.gid()))?;
        }
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anything(_: &Path, _: bool) -> Result<()> {
        Ok(())
    }

    fn nothing(path: &Path, _: bool) -> Result<()> {
        bail!("{} untrusted", path.display())
    }

    #[test]
    fn a_frame_round_trips() {
        let payload = frame(Some("threshold = 0.6\n"), "threshold = 0.7\n");
        let (base, new) = unframe(&payload).unwrap();
        assert_eq!(base, fingerprint("threshold = 0.6\n"));
        assert_eq!(new, "threshold = 0.7\n");
        assert_eq!(unframe(&frame(None, "")).unwrap(), ("none", ""));
        assert!(unframe("threshold = 0.7\n").is_err());
    }

    #[test]
    fn a_valid_change_passes() {
        let now = "threshold = 0.6\n";
        assert_eq!(check(Some(now), &frame(Some(now), "threshold = 0.7\n"), nothing).unwrap(), "threshold = 0.7\n");
        assert!(check(None, &frame(None, "scan_duration_ms = 3000\n"), nothing).is_ok());
    }

    #[test]
    fn a_stale_base_is_refused() {
        let err = check(Some("threshold = 0.65\n"), &frame(Some("threshold = 0.6\n"), "threshold = 0.7\n"), anything)
            .unwrap_err();
        assert!(err.to_string().contains("changed after"), "{err}");
        assert!(check(Some("threshold = 0.6\n"), &frame(None, "threshold = 0.7\n"), anything).is_err());
    }

    #[test]
    fn anything_load_system_would_refuse_is_refused() {
        for bad in ["threshold = 0.1\n", "backend = \"magic\"\n", "liveness.preset = \"lax\"\n", "threshold = \n"] {
            assert!(check(None, &frame(None, bad), anything).is_err(), "{bad:?} passed");
        }
    }

    #[test]
    fn a_new_model_or_template_dir_must_be_trusted() {
        for key in ["model_path", "detector_model_path", "embeddings_dir"] {
            let new = format!("{key} = \"/home/me/{key}\"\n");
            let err = check(None, &frame(None, &new), nothing).unwrap_err();
            assert!(format!("{err:#}").contains("untrusted"), "{key}: {err:#}");
            assert!(check(None, &frame(None, &new), anything).is_ok());
        }
    }

    #[test]
    fn an_unchanged_path_is_not_checked_again() {
        // Already in the file: whatever made it so was root.
        let now = "model_path = \"/home/me/model.onnx\"\nthreshold = 0.6\n";
        let new = "model_path = \"/home/me/model.onnx\"\nthreshold = 0.7\n";
        assert!(check(Some(now), &frame(Some(now), new), nothing).is_ok());
    }

    #[test]
    fn a_camera_pin_can_be_kept_or_removed_but_not_set() {
        let pinned = "pinned_camera_path = \"/sys/x\"\npinned_camera_index = 0\n";
        assert!(check(Some(pinned), &frame(Some(pinned), pinned), anything).is_ok());
        assert!(check(Some(pinned), &frame(Some(pinned), ""), anything).is_ok());
        let moved = "pinned_camera_path = \"/sys/y\"\npinned_camera_index = 0\n";
        assert!(check(Some(pinned), &frame(Some(pinned), moved), anything).is_err());
        assert!(check(None, &frame(None, pinned), anything).is_err());
    }

    #[test]
    fn oversized_input_is_refused() {
        let big = format!("# {}\n", "x".repeat(MAX_CONFIG_BYTES));
        assert!(check(None, &frame(None, &big), anything).is_err());
    }

    #[test]
    fn root_files_are_trusted() {
        assert!(trusted_path(Path::new("/usr/bin/env"), false).is_ok());
        assert!(trusted_path(Path::new("/usr"), true).is_ok());
        assert!(trusted_path(Path::new("/usr"), false).is_err(), "a directory is not a model");
        assert!(trusted_path(Path::new("/usr/bin/env"), true).is_err());
        assert!(trusted_path(Path::new("usr/bin/env"), false).is_err());
        assert!(trusted_path(Path::new("/no/such/model.onnx"), false).is_err());
    }

    #[test]
    fn anything_others_can_write_is_not() {
        // /tmp is world-writable, so nothing under it is trusted, whoever owns it.
        let dir = std::env::temp_dir().join(format!("face-auth-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("model.onnx");
        std::fs::write(&file, b"x").unwrap();
        let me = unsafe { libc::geteuid() };
        let err = trusted_path_for(&file, false, me).unwrap_err();
        assert!(err.to_string().contains("other than root"), "{err}");
        if me != 0 {
            let err = trusted_path(&file, false).unwrap_err();
            assert!(err.to_string().contains("not owned by root"), "{err}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writing_keeps_the_mode_and_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("face-auth-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("face-auth.toml");
        write_atomic(&path, "threshold = 0.6\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&path, "threshold = 0.7\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "threshold = 0.7\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
