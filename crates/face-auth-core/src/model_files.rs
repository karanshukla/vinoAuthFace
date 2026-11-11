//! The model files deploy.sh installs, and the compiled cache made from them:
//! what is on disk, which models the system config doesn't use, and removing
//! those. The settings window lists them; `vinoauthface-helper`'s
//! `delete-unused-models` and `clear-cache` verbs remove them as root.
//!
//! Nothing here follows a symlink or leaves the directory it is given: a
//! model is a regular file directly inside [`SHARE_DIR`], and a model the
//! config names stays wherever it is.

use crate::FaceAuthConfig;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Where deploy.sh installs the models, root-owned.
pub const SHARE_DIR: &str = "/usr/local/share/face-auth";
/// OpenVINO's compiled models, root-owned. Refilled on the next unlock, or
/// at once by `vinoauthface-auth --warm-cache`.
pub const CACHE_DIR: &str = "/var/cache/face-auth";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    pub path: PathBuf,
    pub bytes: u64,
}

impl ModelFile {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The `.onnx` files directly inside `dir` that are neither `config`'s
/// recognition model nor its detector, by path or by the file they resolve
/// to (a configured symlink or hard link into `dir` keeps its target). Only
/// regular files: a symlink, directory or anything else is never listed.
/// Sorted by name; empty when `dir` doesn't exist.
pub fn unused_models(dir: &Path, config: &FaceAuthConfig) -> io::Result<Vec<ModelFile>> {
    let in_use: Vec<PathBuf> = vec![config.model_path().into(), config.detector_model_path().into()];
    // The files the configured paths resolve to, followed through symlinks.
    let in_use_ids: Vec<(u64, u64)> =
        in_use.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| (m.dev(), m.ino())).collect();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut unused = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "onnx") {
            continue;
        }
        // lstat: a symlink is a symlink here, whatever it points at.
        let meta = std::fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() {
            continue;
        }
        if in_use.contains(&path) || in_use_ids.contains(&(meta.dev(), meta.ino())) {
            continue;
        }
        unused.push(ModelFile { path, bytes: meta.len() });
    }
    unused.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(unused)
}

/// Delete what [`unused_models`] lists, worked out again here rather than
/// taken from the caller. Each file is checked again just before it goes.
/// Returns what was deleted.
pub fn delete_unused_models(dir: &Path, config: &FaceAuthConfig) -> io::Result<Vec<ModelFile>> {
    refuse_symlinked_dir(dir)?;
    let mut deleted = Vec::new();
    for model in unused_models(dir, config)? {
        let meta = std::fs::symlink_metadata(&model.path)?;
        if !meta.file_type().is_file() || model.path.parent() != Some(dir) {
            continue;
        }
        std::fs::remove_file(&model.path)?;
        deleted.push(model);
    }
    Ok(deleted)
}

/// Bytes in the regular files under `dir`, without following symlinks. 0
/// when it doesn't exist.
pub fn dir_size(dir: &Path) -> io::Result<u64> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut total = 0;
    for entry in entries {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_dir() {
            total += dir_size(&entry.path())?;
        } else if meta.file_type().is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

/// Empty `dir`, keeping the directory itself (its owner, mode and SELinux
/// label are deploy.sh's). A symlink inside is removed, never followed.
/// Returns the bytes freed; 0 when it doesn't exist.
pub fn clear_dir(dir: &Path) -> io::Result<u64> {
    match refuse_symlinked_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        other => other?,
    }
    let freed = dir_size(dir)?;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if std::fs::symlink_metadata(&path)?.file_type().is_dir() {
            // Doesn't follow symlinks inside, either.
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(freed)
}

fn refuse_symlinked_dir(dir: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(dir)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{} is not a directory", dir.display())))
    }
}

/// `13.6 MB`, in the decimal units file managers use.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = "";
    for u in UNITS {
        value /= 1000.0;
        unit = u;
        if value < 1000.0 {
            break;
        }
    }
    if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("vinoauthface-model-files-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn file(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, vec![0u8; bytes]).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn config(model: &Path, detector: &Path) -> FaceAuthConfig {
        FaceAuthConfig::from_toml(&format!(
            "model_path = \"{}\"\ndetector_model_path = \"{}\"\n",
            model.display(),
            detector.display()
        ))
        .unwrap()
    }

    fn names(list: &[ModelFile]) -> Vec<String> {
        list.iter().map(ModelFile::name).collect()
    }

    #[test]
    fn lists_only_the_models_the_config_does_not_use() {
        let s = Scratch::new("list");
        let share = s.0.join("share");
        let model = s.file("share/w600k_r50.onnx", 30);
        let detector = s.file("share/det_500m.onnx", 20);
        s.file("share/w600k_mbf.onnx", 10);
        s.file("share/version-slim-320.onnx", 5);
        s.file("share/login-mode.sh", 7);
        s.file("share/selinux/face_auth.pp", 7);
        std::fs::create_dir_all(share.join("dir.onnx")).unwrap();
        let unused = unused_models(&share, &config(&model, &detector)).unwrap();
        assert_eq!(names(&unused), ["version-slim-320.onnx", "w600k_mbf.onnx"]);
        assert_eq!(unused[1].bytes, 10);
    }

    #[test]
    fn a_missing_directory_has_nothing_unused() {
        let s = Scratch::new("missing");
        let c = config(&s.0.join("m.onnx"), &s.0.join("d.onnx"));
        assert!(unused_models(&s.0.join("nope"), &c).unwrap().is_empty());
        assert!(delete_unused_models(&s.0.join("nope"), &c).is_err(), "nothing to delete in, and says so");
        assert_eq!(dir_size(&s.0.join("nope")).unwrap(), 0);
        assert_eq!(clear_dir(&s.0.join("nope")).unwrap(), 0);
    }

    #[test]
    fn a_configured_path_elsewhere_keeps_what_it_links_to() {
        let s = Scratch::new("links");
        let share = s.0.join("share");
        let target = s.file("share/mine.onnx", 4);
        let hard = s.file("share/hard.onnx", 4);
        s.file("share/det_500m.onnx", 4);
        s.file("share/spare.onnx", 4);
        // The config names paths outside the directory that resolve into it.
        let elsewhere = s.0.join("opt");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&target, elsewhere.join("model.onnx")).unwrap();
        std::fs::hard_link(&hard, elsewhere.join("detector.onnx")).unwrap();
        let c = config(&elsewhere.join("model.onnx"), &elsewhere.join("detector.onnx"));
        // det_500m.onnx is not configured, so it is unused.
        assert_eq!(names(&unused_models(&share, &c).unwrap()), ["det_500m.onnx", "spare.onnx"]);
    }

    #[test]
    fn deletes_unused_regular_files_and_never_follows_a_symlink() {
        let s = Scratch::new("delete");
        let share = s.0.join("share");
        let model = s.file("share/w600k_r50.onnx", 30);
        let detector = s.file("share/det_500m.onnx", 20);
        s.file("share/w600k_mbf.onnx", 10);
        s.file("share/setup.sh", 3);
        // A symlink in the directory to a file outside it, and one to a model in use.
        let outside = s.file("templates/alice.onnx", 9);
        symlink(&outside, share.join("link.onnx")).unwrap();
        symlink(&model, share.join("alias.onnx")).unwrap();
        let c = config(&model, &detector);
        let deleted = delete_unused_models(&share, &c).unwrap();
        assert_eq!(names(&deleted), ["w600k_mbf.onnx"]);
        assert!(model.exists() && detector.exists() && outside.exists());
        assert!(share.join("setup.sh").exists());
        assert!(std::fs::symlink_metadata(share.join("link.onnx")).is_ok(), "symlinks are left alone");
        assert!(unused_models(&share, &c).unwrap().is_empty());
        assert!(delete_unused_models(&share, &c).unwrap().is_empty(), "again is a no-op");
    }

    #[test]
    fn refuses_a_symlinked_directory() {
        let s = Scratch::new("dirlink");
        let real = s.0.join("real");
        s.file("real/spare.onnx", 4);
        symlink(&real, s.0.join("share")).unwrap();
        let c = config(&s.0.join("m.onnx"), &s.0.join("d.onnx"));
        assert!(delete_unused_models(&s.0.join("share"), &c).is_err());
        assert!(clear_dir(&s.0.join("share")).is_err());
        assert!(real.join("spare.onnx").exists());
    }

    #[test]
    fn clearing_the_cache_keeps_the_directory_and_what_links_point_at() {
        let s = Scratch::new("cache");
        let cache = s.0.join("cache");
        s.file("cache/a.blob", 100);
        s.file("cache/sub/b.blob", 50);
        let outside = s.file("keep/c.blob", 70);
        symlink(&outside, cache.join("link.blob")).unwrap();
        symlink(s.0.join("keep"), cache.join("linkdir")).unwrap();
        assert_eq!(dir_size(&cache).unwrap(), 150, "symlinks aren't counted");
        assert_eq!(clear_dir(&cache).unwrap(), 150);
        assert!(cache.is_dir());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
        assert!(outside.exists());
        assert_eq!(clear_dir(&cache).unwrap(), 0);
    }

    #[test]
    fn sizes_read_as_a_file_manager_shows_them() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(999), "999 bytes");
        assert_eq!(human_size(2_524_817), "2.5 MB");
        assert_eq!(human_size(13_616_099), "14 MB");
        assert_eq!(human_size(166_000_000), "166 MB");
        assert_eq!(human_size(1_500_000_000), "1.5 GB");
    }
}
