//! The cameras each user enrolled on, by USB vendor:product ID, so a scan from
//! any other camera is refused.
//!
//! Stops the commodity swap: unplug the IR camera, plug in another to feed the
//! recognition path. It is not attestation (a programmable USB device can
//! claim any ID; `pin-camera.sh` pins the port for that), but it is on by
//! default and needs no setup.
//!
//! `<user>/cameras` holds one `vvvv:pppp` per line, written at enrolment beside
//! the templates, with the same mode. A camera with no USB identity (MIPI,
//! v4l2loopback) records nothing. No file, or an empty one, means unknown and
//! allows any camera, as `model_tag_matches` does for untagged templates, so
//! existing installs keep working until their next enrolment.

use crate::storage::{check_template_file, ensure_dir, user_store_dir, EMBEDDINGS_DIR_MODE, EMBEDDINGS_FILE_MODE};
use anyhow::{bail, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const FILE_NAME: &str = "cameras";
/// Bounds what a load reads and allocates. A user enrolled on more cameras
/// than this re-enrols fresh.
const MAX_CAMERAS: usize = 16;
/// `vvvv:pppp` and a newline.
const CAMERA_LINE_BYTES: usize = 10;
const MAX_FILE_BYTES: u64 = (MAX_CAMERAS * CAMERA_LINE_BYTES) as u64;

/// `vvvv:pppp` of the USB device behind a V4L2 node, or `None` when it has no
/// USB identity (MIPI, v4l2loopback) or reports a malformed one.
pub fn camera_id(device: &str) -> Option<String> {
    let (vendor, product) = crate::capture::usb_ids(device).ok()?;
    let id = format!("{vendor}:{product}").to_ascii_lowercase();
    valid_id(&id).then_some(id)
}

fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9
        && b[4] == b':'
        && b.iter().enumerate().all(|(i, c)| i == 4 || c.is_ascii_hexdigit())
}

/// The cameras `user` enrolled on. Empty when unknown.
pub fn load(user: &str, embeddings_dir: &Path) -> Result<Vec<String>> {
    let user_dir = user_store_dir(user, embeddings_dir)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(user_dir.join(FILE_NAME))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    check_template_file(&file, fs::metadata(&user_dir)?.uid())?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text)?;
    parse(&text)
}

fn parse(text: &str) -> Result<Vec<String>> {
    if text.len() as u64 > MAX_FILE_BYTES {
        bail!("camera list longer than {MAX_FILE_BYTES} bytes");
    }
    let ids: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    if let Some(bad) = ids.iter().find(|id| !valid_id(id)) {
        bail!("malformed camera id {bad:?} in the camera list");
    }
    Ok(ids)
}

/// Record the camera an enrolment used: `replace` for a fresh enrolment,
/// otherwise added to the list. A camera with no ID leaves a fresh enrolment
/// unbound rather than bound to nothing.
pub fn record(user: &str, embeddings_dir: &Path, id: Option<&str>, replace: bool) -> Result<()> {
    let mut ids = if replace { Vec::new() } else { load(user, embeddings_dir)? };
    if let Some(id) = id {
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_string());
        }
    }
    if ids.len() > MAX_CAMERAS {
        bail!("enrolled on more than {MAX_CAMERAS} cameras; enrol fresh instead of improving");
    }
    save(user, embeddings_dir, &ids)
}

fn save(user: &str, embeddings_dir: &Path, ids: &[String]) -> Result<()> {
    let user_dir = user_store_dir(user, embeddings_dir)?;
    let path = user_dir.join(FILE_NAME);
    if ids.is_empty() {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    ensure_dir(&user_dir, EMBEDDINGS_DIR_MODE)?;
    // Same discipline as the templates: unique, exclusive, never followed,
    // durable before the rename.
    let tmp = user_dir.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(EMBEDDINGS_FILE_MODE)
            .open(&tmp)?;
        file.set_permissions(fs::Permissions::from_mode(EMBEDDINGS_FILE_MODE))?;
        for id in ids {
            writeln!(file, "{id}")?;
        }
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    if let Ok(dir) = File::open(&user_dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// May a scan from the camera `live` (its ID, if it has one) use templates
/// enrolled on `enrolled`?
pub fn check(enrolled: &[String], live: Option<&str>, device: &str) -> Result<()> {
    if enrolled.is_empty() {
        return Ok(());
    }
    match live {
        Some(id) if enrolled.iter().any(|known| known == id) => Ok(()),
        Some(id) => bail!(
            "{device} ({id}) is not a camera this account enrolled on ({}); refusing its frames. \
             If you replaced the camera on purpose, re-enrol",
            enrolled.join(", ")
        ),
        None => bail!(
            "{device} has no USB identity, but this account enrolled on {}; refusing its frames. \
             If you replaced the camera on purpose, re-enrol",
            enrolled.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "face-auth-cameras-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unknown_allows_any_camera() {
        assert!(check(&[], Some("046d:085e"), "/dev/video2").is_ok());
        assert!(check(&[], None, "/dev/video10").is_ok());
    }

    #[test]
    fn another_camera_is_refused() {
        let enrolled = vec!["2b7e:55c0".to_string()];
        assert!(check(&enrolled, Some("2b7e:55c0"), "/dev/video2").is_ok());
        let err = check(&enrolled, Some("046d:085e"), "/dev/video2").unwrap_err().to_string();
        assert!(err.contains("046d:085e") && err.contains("2b7e:55c0"), "{err}");
        assert!(check(&enrolled, None, "/dev/video10").is_err(), "a loopback must not stand in");
    }

    #[test]
    fn records_replace_and_append() {
        let dir = tmpdir("record");
        record("alice", &dir, Some("2b7e:55c0"), true).unwrap();
        record("alice", &dir, Some("046d:085e"), false).unwrap();
        record("alice", &dir, Some("046d:085e"), false).unwrap();
        assert_eq!(load("alice", &dir).unwrap(), ["2b7e:55c0", "046d:085e"]);

        record("alice", &dir, Some("046d:085e"), true).unwrap();
        assert_eq!(load("alice", &dir).unwrap(), ["046d:085e"]);

        // A fresh enrolment on a camera with no ID leaves the account unbound.
        record("alice", &dir, None, true).unwrap();
        assert!(load("alice", &dir).unwrap().is_empty());
        assert!(!dir.join("alice").join(FILE_NAME).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn malformed_or_oversized_lists_are_refused() {
        assert!(parse("2b7e:55c0\n../etc\n").is_err());
        assert!(parse("2b7e:55c0x\n").is_err());
        assert!(parse(&"2b7e:55c0\n".repeat(MAX_CAMERAS + 1)).is_err());
        assert_eq!(parse("2b7e:55c0\n\n").unwrap(), ["2b7e:55c0"]);
    }

    #[test]
    fn group_writable_list_is_refused() {
        let dir = tmpdir("mode");
        record("alice", &dir, Some("2b7e:55c0"), true).unwrap();
        let path = dir.join("alice").join(FILE_NAME);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
