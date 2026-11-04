//! The two config files the window edits: `/etc/face-auth.toml`, whose
//! changes wait for Apply, and the user's own, saved as it changes.

use crate::keys::{self, Key};
use face_auth_core::FaceAuthConfig;
use std::path::PathBuf;

pub struct ConfigFile {
    pub path: PathBuf,
    /// The file as last read; `None` when there is none.
    pub read: Option<String>,
    /// The file with this window's changes.
    pub text: String,
    /// Why the file couldn't be read, when it exists but can't be.
    pub unreadable: Option<String>,
    /// `text`, parsed: the config, or why it doesn't parse.
    pub config: Result<FaceAuthConfig, String>,
    pub table: toml::Table,
}

impl ConfigFile {
    pub fn load(path: PathBuf) -> Self {
        let (read, unreadable) = match std::fs::read_to_string(&path) {
            Ok(text) => (Some(text), None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(e) => (None, Some(e.to_string())),
        };
        let mut file = ConfigFile {
            path,
            text: read.clone().unwrap_or_default(),
            read,
            unreadable,
            config: Err(String::new()),
            table: toml::Table::new(),
        };
        file.parse();
        file
    }

    fn parse(&mut self) {
        self.config = FaceAuthConfig::from_toml(&self.text).map_err(|e| format!("{e:#}"));
        self.table = toml::from_str(&self.text).unwrap_or_default();
    }

    /// The config to show: this file's, or the defaults when it doesn't parse.
    pub fn values(&self) -> FaceAuthConfig {
        self.config.clone().unwrap_or_else(|_| FaceAuthConfig::from_toml("").expect("defaults parse"))
    }

    pub fn is_set(&self, key: Key) -> bool {
        key.value(&self.table).is_some()
    }

    pub fn raw(&self, key: Key) -> Option<&toml::Value> {
        key.value(&self.table)
    }

    /// Set `key` to a literal from `keys::float` and the rest, or back to its
    /// default with `None`.
    pub fn set(&mut self, key: Key, value: Option<String>) {
        self.text = keys::edit(&self.text, key, value.as_deref());
        self.parse();
    }

    /// The keys whose value differs from the file as read.
    pub fn changes(&self) -> Vec<Key> {
        let read: toml::Table = self.read.as_deref().and_then(|t| toml::from_str(t).ok()).unwrap_or_default();
        keys::ALL.into_iter().filter(|key| key.value(&read) != key.value(&self.table)).collect()
    }

    pub fn discard(&mut self) {
        self.text = self.read.clone().unwrap_or_default();
        self.parse();
    }

    /// Read again, keeping nothing from before.
    pub fn reload(&mut self) {
        *self = ConfigFile::load(std::mem::take(&mut self.path));
    }

    /// Write `text` to the file, for the user's own: their file, their
    /// directory, no root needed.
    pub fn save(&mut self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        face_auth_core::config_write::write_atomic(&self.path, &self.text).map_err(|e| format!("{e:#}"))?;
        self.read = Some(self.text.clone());
        Ok(())
    }
}

/// `~/.config/face-auth.toml`, the file the PAM path reads as the user's.
pub fn user_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config")).join("face-auth.toml")
}
