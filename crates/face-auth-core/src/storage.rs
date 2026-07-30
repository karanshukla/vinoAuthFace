use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use crate::error::FaceAuthError;

const EMBEDDING_VERSION: u32 = 1;
const EMBEDDING_DIM: u32 = 512;

#[derive(Debug, Clone)]
pub struct EmbeddingStore {
    pub embeddings: Vec<Vec<f32>>,
}

impl EmbeddingStore {
    pub fn load(user: &str, embeddings_dir: &Path) -> anyhow::Result<Self> {
        let path = embeddings_dir.join(user).join("embeddings.bin");
        if !path.exists() {
            return Err(FaceAuthError::NoEmbeddings.into());
        }
        
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);
        
        let version = reader.read_u32::<LittleEndian>()?;
        if version != EMBEDDING_VERSION {
            return Err(FaceAuthError::InvalidEmbeddingFormat.into());
        }
        
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
        
        Ok(Self { embeddings })
    }
    
    pub fn save(&self, user: &str, embeddings_dir: &Path) -> anyhow::Result<()> {
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
        Self { embeddings: Vec::new() }
    }
}