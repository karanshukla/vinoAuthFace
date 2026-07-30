use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use crate::error::FaceAuthError;

// v1 (legacy): version, count, dim, embeddings — no model identity, produced back when
// w600k_mbf.onnx was the only recognition model this project supported. Still readable so
// existing installs aren't broken by this change, but carries no `model_tag` (treated as
// "unknown model", which `model_tag_matches` always allows — there was nothing to compare
// against before this).
const EMBEDDING_VERSION_LEGACY: u32 = 1;
// v2: adds a length-prefixed model_tag string (config's `model_path` basename, e.g.
// "w600k_mbf.onnx" or "w600k_r50.onnx") right after the header, before the embeddings.
// Different recognition models produce numerically incompatible embedding spaces despite
// sharing the same 512-d shape — cosine similarity between them is meaningless, not just
// less accurate — so this exists to turn "switched model_path, forgot to re-enroll" into a
// clear error instead of silent, unpredictable match/no-match behavior.
const EMBEDDING_VERSION: u32 = 2;
const EMBEDDING_DIM: u32 = 512;

#[derive(Debug, Clone)]
pub struct EmbeddingStore {
    pub embeddings: Vec<Vec<f32>>,
    /// Which recognition model produced these embeddings (config's `model_path` basename).
    /// `None` for legacy v1 files or a fresh `Default::default()` store, meaning "unknown" —
    /// callers should treat unknown as compatible (nothing to contradict), not as a mismatch.
    pub model_tag: Option<String>,
}

impl EmbeddingStore {
    /// Whether these embeddings are safe to compare against `current_tag`. Unknown-origin
    /// stores (legacy v1, or a store that hasn't been saved yet) always pass — a mismatch can
    /// only be raised once both sides are actually known.
    pub fn model_tag_matches(&self, current_tag: &str) -> bool {
        self.model_tag.as_deref().map(|t| t == current_tag).unwrap_or(true)
    }

    pub fn load(user: &str, embeddings_dir: &Path) -> anyhow::Result<Self> {
        let path = embeddings_dir.join(user).join("embeddings.bin");
        if !path.exists() {
            return Err(FaceAuthError::NoEmbeddings.into());
        }

        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);

        let version = reader.read_u32::<LittleEndian>()?;
        let model_tag = match version {
            EMBEDDING_VERSION_LEGACY => None,
            EMBEDDING_VERSION => {
                let tag_len = reader.read_u32::<LittleEndian>()? as usize;
                let mut tag_bytes = vec![0u8; tag_len];
                reader.read_exact(&mut tag_bytes)?;
                Some(String::from_utf8(tag_bytes).map_err(|_| FaceAuthError::InvalidEmbeddingFormat)?)
            }
            _ => return Err(FaceAuthError::InvalidEmbeddingFormat.into()),
        };

        let count = reader.read_u32::<LittleEndian>()?;
        let dim = reader.read_u32::<LittleEndian>()?;

        if dim != EMBEDDING_DIM {
            return Err(FaceAuthError::InvalidEmbeddingFormat.into());
        }

        let mut embeddings = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut embedding = vec![0.0f32; EMBEDDING_DIM as usize];
            for val in &mut embedding {
                *val = reader.read_f32::<LittleEndian>()?;
            }
            embeddings.push(embedding);
        }

        Ok(Self { embeddings, model_tag })
    }

    pub fn save(&self, user: &str, embeddings_dir: &Path, model_tag: &str) -> anyhow::Result<()> {
        let user_dir = embeddings_dir.join(user);
        fs::create_dir_all(&user_dir)?;
        // Biometric templates: lock the per-user directory and file down explicitly rather
        // than trusting umask, so another local user can't read them off disk regardless of
        // what mode bits `embeddings_dir`'s parent was created with.
        fs::set_permissions(&user_dir, fs::Permissions::from_mode(0o700))?;

        let tmp_path = user_dir.join("embeddings.bin.tmp");
        let path = user_dir.join("embeddings.bin");

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        let mut writer = BufWriter::new(file);

        writer.write_u32::<LittleEndian>(EMBEDDING_VERSION)?;
        let tag_bytes = model_tag.as_bytes();
        writer.write_u32::<LittleEndian>(tag_bytes.len() as u32)?;
        writer.write_all(tag_bytes)?;
        writer.write_u32::<LittleEndian>(self.embeddings.len() as u32)?;
        writer.write_u32::<LittleEndian>(EMBEDDING_DIM)?;

        for embedding in &self.embeddings {
            for &val in embedding {
                writer.write_f32::<LittleEndian>(val)?;
            }
        }

        writer.flush()?;
        drop(writer);

        fs::rename(&tmp_path, &path)?;

        Ok(())
    }

    pub fn add_embedding(&mut self, embedding: Vec<f32>) {
        self.embeddings.push(embedding);
    }
}

impl Default for EmbeddingStore {
    fn default() -> Self {
        Self { embeddings: Vec::new(), model_tag: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    #[test]
    fn save_then_load_round_trips_embeddings_and_tag() {
        let dir = std::env::temp_dir().join(format!("face-auth-storage-test-{}", std::process::id()));
        let store = EmbeddingStore { embeddings: vec![vec![0.5f32; EMBEDDING_DIM as usize]], model_tag: None };
        store.save("alice", &dir, "w600k_r50.onnx").unwrap();

        let loaded = EmbeddingStore::load("alice", &dir).unwrap();
        assert_eq!(loaded.embeddings, store.embeddings);
        assert_eq!(loaded.model_tag.as_deref(), Some("w600k_r50.onnx"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn model_tag_matches_is_permissive_for_unknown_origin() {
        let unknown = EmbeddingStore { embeddings: vec![], model_tag: None };
        assert!(unknown.model_tag_matches("w600k_mbf.onnx"));
        assert!(unknown.model_tag_matches("w600k_r50.onnx"));
    }

    #[test]
    fn model_tag_matches_rejects_a_real_mismatch() {
        let mbf = EmbeddingStore { embeddings: vec![], model_tag: Some("w600k_mbf.onnx".to_string()) };
        assert!(mbf.model_tag_matches("w600k_mbf.onnx"));
        assert!(!mbf.model_tag_matches("w600k_r50.onnx"));
    }

    #[test]
    fn legacy_v1_file_without_a_tag_still_loads() {
        let dir = std::env::temp_dir().join(format!("face-auth-storage-test-legacy-{}", std::process::id()));
        let user_dir = dir.join("bob");
        fs::create_dir_all(&user_dir).unwrap();
        let path = user_dir.join("embeddings.bin");
        let mut writer = BufWriter::new(File::create(&path).unwrap());
        writer.write_u32::<LittleEndian>(EMBEDDING_VERSION_LEGACY).unwrap();
        writer.write_u32::<LittleEndian>(1).unwrap();
        writer.write_u32::<LittleEndian>(EMBEDDING_DIM).unwrap();
        for _ in 0..EMBEDDING_DIM {
            writer.write_f32::<LittleEndian>(0.25).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);

        let loaded = EmbeddingStore::load("bob", &dir).unwrap();
        assert_eq!(loaded.embeddings.len(), 1);
        assert_eq!(loaded.model_tag, None);
        assert!(loaded.model_tag_matches("w600k_r50.onnx"));

        std::fs::remove_dir_all(&dir).ok();
    }
}