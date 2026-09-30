use crate::error::FaceAuthError;
use crate::user::validate_username;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// v1: version, count, dim, embeddings. No model identity. Still readable so
/// existing installs keep working; loads with `model_tag: None`.
const EMBEDDING_VERSION_LEGACY: u32 = 1;
/// v2: adds a length-prefixed `model_tag` (the recognition model's file name,
/// e.g. "w600k_mbf.onnx") after the version. Different recognition models
/// produce incompatible embedding spaces with the same 512-d shape, so
/// comparing across them yields meaningless similarities rather than an error.
const EMBEDDING_VERSION: u32 = 2;
/// v3: the v2 body (count, dim, vectors) sealed to the TPM. The version and
/// `model_tag` stay in the clear so "wrong model" can be reported without a
/// working TPM; the tag is authenticated anyway, as part of the sealed name.
/// Layout: version, tag length, tag, blob length, blob.
const EMBEDDING_VERSION_SEALED: u32 = 3;
const EMBEDDING_DIM: u32 = 512;

/// A tag is a file name. Bounded for the same reason as `MAX_EMBEDDINGS`.
const MAX_MODEL_TAG_LEN: u32 = 255;

/// Upper bound on stored embeddings per user.
///
/// `count` is read straight off disk and drives an allocation, so it is
/// bounded before use: an unbounded `u32` here asks for ~96 GB and aborts
/// the process. Enrolment adds 30 at a time by default, leaving room for
/// several `--improve` passes.
const MAX_EMBEDDINGS: u32 = 256;

const MAX_PAYLOAD_BYTES: u64 = 8 + MAX_EMBEDDINGS as u64 * EMBEDDING_DIM as u64 * 4;

/// A sealed blob is the payload base64-encoded plus systemd's credential
/// header; 2x and a page of slack covers it.
const MAX_SEALED_BYTES: u64 = MAX_PAYLOAD_BYTES * 2 + 4096;

/// Largest valid file: header, a full-length tag, and a full sealed blob.
const MAX_STORE_BYTES: u64 = 16 + MAX_MODEL_TAG_LEN as u64 + MAX_SEALED_BYTES;

/// Biometric templates: root-owned, readable by the `face-auth` group that the
/// set-group-ID `face-auth` binary runs with, never writable by it. The
/// set-group-ID bit on the directories makes new entries inherit that group.
/// See `deploy.sh`.
pub(crate) const EMBEDDINGS_FILE_MODE: u32 = 0o640;
pub(crate) const EMBEDDINGS_DIR_MODE: u32 = 0o2750;
/// The one place the group may write: lockout state, which lock screens
/// running as the user have to update.
pub(crate) const LOCKOUT_DIR_MODE: u32 = 0o2770;
pub(crate) const LOCKOUT_FILE_MODE: u32 = 0o660;

pub(crate) fn lockout_dir(user_dir: &Path) -> PathBuf {
    user_dir.join("lockout")
}

/// mkdir(2) ignores the set-group-ID bit in its mode argument and the umask
/// strips group bits, so the mode is applied explicitly after creation.
pub(crate) fn ensure_dir(path: &Path, mode: u32) -> std::io::Result<()> {
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(mode)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Default)]
pub struct EmbeddingStore {
    pub embeddings: Vec<Vec<f32>>,
    /// Recognition model that produced these embeddings. `None` for a v1 file
    /// or a store not yet saved, meaning "unknown", which is compatible with
    /// anything: a mismatch is only raised once both sides are known.
    pub model_tag: Option<String>,
}

/// Build the per-user store path, rejecting anything that would escape
/// `embeddings_dir`. The username reaching here originates from PAM, but it
/// is a path component either way and is checked rather than trusted.
pub(crate) fn user_store_dir(user: &str, embeddings_dir: &Path) -> anyhow::Result<PathBuf> {
    validate_username(user)?;
    Ok(embeddings_dir.join(user))
}

/// The template file must be what enrolment wrote: a regular file owned like
/// its directory (root, in production), with no group or other write access, a
/// single link and a bounded size. A FIFO or a device would otherwise hang or
/// mislead the PAM helper, which reads this as root.
pub(crate) fn check_template_file(file: &File, dir_uid: u32) -> anyhow::Result<()> {
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.is_file()
            && meta.uid() == dir_uid
            && meta.mode() & 0o022 == 0
            && meta.nlink() == 1
            && meta.len() <= MAX_STORE_BYTES,
        FaceAuthError::InvalidEmbeddingFormat
    );
    Ok(())
}

/// The name a sealed payload is bound to. systemd authenticates it with the
/// data, so a blob copied to another account or another model's tag no longer
/// unseals. Both parts are validated or bounded: usernames by
/// `validate_username`, the tag by length and by rejecting `/`.
fn seal_name(user: &str, model_tag: &str) -> anyhow::Result<String> {
    validate_username(user)?;
    anyhow::ensure!(
        !model_tag.is_empty() && !model_tag.contains('/') && !model_tag.contains('\0'),
        FaceAuthError::InvalidEmbeddingFormat
    );
    Ok(format!("vinoauthface.{user}.{model_tag}"))
}

/// The v2 body: count, dim, vectors. Rejects trailing bytes.
fn read_body(reader: &mut impl Read) -> anyhow::Result<Vec<Vec<f32>>> {
    let count = reader.read_u32::<LittleEndian>()?;
    let dim = reader.read_u32::<LittleEndian>()?;

    if dim != EMBEDDING_DIM || count > MAX_EMBEDDINGS {
        return Err(FaceAuthError::InvalidEmbeddingFormat.into());
    }

    let mut embeddings = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut embedding = vec![0.0f32; EMBEDDING_DIM as usize];
        for val in &mut embedding {
            *val = reader.read_f32::<LittleEndian>()?;
        }
        // A non-finite stored value makes every comparison NaN, which
        // fails closed but silently. Reject it as corruption instead.
        if !embedding.iter().all(|v| v.is_finite()) {
            return Err(FaceAuthError::InvalidEmbeddingFormat.into());
        }
        embeddings.push(embedding);
    }

    // Trailing bytes mean this is not the file we think it is.
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(FaceAuthError::InvalidEmbeddingFormat.into());
    }
    Ok(embeddings)
}

fn write_body(w: &mut impl Write, embeddings: &[Vec<f32>]) -> anyhow::Result<()> {
    w.write_u32::<LittleEndian>(embeddings.len() as u32)?;
    w.write_u32::<LittleEndian>(EMBEDDING_DIM)?;
    for embedding in embeddings {
        anyhow::ensure!(
            embedding.len() == EMBEDDING_DIM as usize,
            "embedding has {} dimensions, expected {}",
            embedding.len(),
            EMBEDDING_DIM
        );
        for &val in embedding {
            w.write_f32::<LittleEndian>(val)?;
        }
    }
    Ok(())
}

fn read_tag(reader: &mut impl Read) -> anyhow::Result<String> {
    let len = reader.read_u32::<LittleEndian>()?;
    if len > MAX_MODEL_TAG_LEN {
        return Err(FaceAuthError::InvalidEmbeddingFormat.into());
    }
    let mut bytes = vec![0u8; len as usize];
    reader.read_exact(&mut bytes)?;
    Ok(String::from_utf8(bytes).map_err(|_| FaceAuthError::InvalidEmbeddingFormat)?)
}

impl EmbeddingStore {
    pub fn model_tag_matches(&self, current_tag: &str) -> bool {
        self.model_tag.as_deref().is_none_or(|t| t == current_tag)
    }

    fn open(user_dir: &Path) -> anyhow::Result<BufReader<File>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(user_dir.join("embeddings.bin"))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(FaceAuthError::NoEmbeddings.into());
            }
            Err(e) => return Err(e.into()),
        };
        check_template_file(&file, fs::metadata(user_dir)?.uid())?;
        Ok(BufReader::new(file))
    }

    pub fn load(user: &str, embeddings_dir: &Path) -> anyhow::Result<Self> {
        Self::load_with(user, embeddings_dir, false)
    }

    /// `require_sealed` is the downgrade guard: with sealing on, a plaintext
    /// file is refused rather than trusted, so swapping one in does not
    /// sidestep the TPM.
    ///
    /// A sealed file that will not unseal is `SealUnavailable`, not a fallback:
    /// the key is gone. It is an error before any face is scanned, so it never
    /// counts toward lockout.
    pub fn load_with(user: &str, embeddings_dir: &Path, require_sealed: bool) -> anyhow::Result<Self> {
        let user_dir = user_store_dir(user, embeddings_dir)?;
        let mut reader = Self::open(&user_dir)?;

        let version = reader.read_u32::<LittleEndian>()?;
        if require_sealed && version != EMBEDDING_VERSION_SEALED {
            return Err(FaceAuthError::SealRequired.into());
        }
        let (model_tag, embeddings) = match version {
            EMBEDDING_VERSION_LEGACY => (None, read_body(&mut reader)?),
            EMBEDDING_VERSION => {
                let tag = read_tag(&mut reader)?;
                (Some(tag), read_body(&mut reader)?)
            }
            EMBEDDING_VERSION_SEALED => {
                let tag = read_tag(&mut reader)?;
                let len = reader.read_u32::<LittleEndian>()?;
                if len as u64 > MAX_SEALED_BYTES {
                    return Err(FaceAuthError::InvalidEmbeddingFormat.into());
                }
                let mut blob = vec![0u8; len as usize];
                reader.read_exact(&mut blob)?;
                let mut trailing = [0u8; 1];
                if reader.read(&mut trailing)? != 0 {
                    return Err(FaceAuthError::InvalidEmbeddingFormat.into());
                }
                let payload = crate::seal::unseal(user, &seal_name(user, &tag)?, &blob)?;
                anyhow::ensure!(
                    payload.len() as u64 <= MAX_PAYLOAD_BYTES,
                    FaceAuthError::InvalidEmbeddingFormat
                );
                (Some(tag), read_body(&mut payload.as_slice())?)
            }
            _ => return Err(FaceAuthError::InvalidEmbeddingFormat.into()),
        };

        Ok(Self { embeddings, model_tag })
    }

    /// Has this user enrolled? Reads the header only, so a sealed store is
    /// answered without touching the TPM (the tray asks as the user, who has
    /// no business opening it).
    pub fn is_enrolled(user: &str, embeddings_dir: &Path) -> anyhow::Result<bool> {
        let user_dir = user_store_dir(user, embeddings_dir)?;
        let mut reader = match Self::open(&user_dir) {
            Ok(r) => r,
            Err(e) if matches!(e.downcast_ref(), Some(FaceAuthError::NoEmbeddings)) => return Ok(false),
            Err(e) => return Err(e),
        };
        if reader.read_u32::<LittleEndian>()? == EMBEDDING_VERSION_SEALED {
            return Ok(true);
        }
        drop(reader);
        Ok(!Self::load(user, embeddings_dir)?.embeddings.is_empty())
    }

    /// The model a user's templates were enrolled with, from the header only
    /// (a sealed store needs no TPM). `None` for a legacy v1 file, which
    /// `model_tag_matches` treats as unknown.
    pub fn stored_model_tag(user: &str, embeddings_dir: &Path) -> anyhow::Result<Option<String>> {
        let mut reader = Self::open(&user_store_dir(user, embeddings_dir)?)?;
        match reader.read_u32::<LittleEndian>()? {
            EMBEDDING_VERSION_LEGACY => Ok(None),
            EMBEDDING_VERSION | EMBEDDING_VERSION_SEALED => Ok(Some(read_tag(&mut reader)?)),
            _ => Err(FaceAuthError::InvalidEmbeddingFormat.into()),
        }
    }

    pub fn save(&self, user: &str, embeddings_dir: &Path, model_tag: &str) -> anyhow::Result<()> {
        self.save_with(user, embeddings_dir, model_tag, false)
    }

    /// With `seal`, the body is sealed to the TPM. No TPM (or a failed seal)
    /// warns and writes a plaintext v2 file instead: absence fails open so
    /// enrolment still works on hardware without one. An *existing* sealed
    /// store is the opposite case and never falls back; see `load_with`.
    pub fn save_with(
        &self,
        user: &str,
        embeddings_dir: &Path,
        model_tag: &str,
        seal: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            model_tag.len() <= MAX_MODEL_TAG_LEN as usize,
            "model tag longer than {MAX_MODEL_TAG_LEN} bytes"
        );
        if self.embeddings.len() > MAX_EMBEDDINGS as usize {
            anyhow::bail!(
                "refusing to store {} embeddings (limit {})",
                self.embeddings.len(),
                MAX_EMBEDDINGS
            );
        }

        let user_dir = user_store_dir(user, embeddings_dir)?;
        if let Some(parent) = user_dir.parent() {
            fs::DirBuilder::new().recursive(true).mode(EMBEDDINGS_DIR_MODE).create(parent)?;
        }
        ensure_dir(&user_dir, EMBEDDINGS_DIR_MODE)?;
        // Created here, by root at enrolment, because the group cannot create
        // entries in the user directory itself.
        ensure_dir(&lockout_dir(&user_dir), LOCKOUT_DIR_MODE)?;

        // Unique and created exclusively, never followed: nothing pre-planted
        // at a fixed name can redirect the write.
        let tmp_path = user_dir.join(format!("embeddings.bin.{}.tmp", std::process::id()));
        let _ = fs::remove_file(&tmp_path);
        let path = user_dir.join("embeddings.bin");

        {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(EMBEDDINGS_FILE_MODE)
                .open(&tmp_path)?;
            file.set_permissions(fs::Permissions::from_mode(EMBEDDINGS_FILE_MODE))?;
            let mut writer = BufWriter::new(file);

            let sealed = if seal { seal_payload(user, model_tag, &self.embeddings) } else { None };
            match sealed {
                Some(blob) => {
                    writer.write_u32::<LittleEndian>(EMBEDDING_VERSION_SEALED)?;
                    writer.write_u32::<LittleEndian>(model_tag.len() as u32)?;
                    writer.write_all(model_tag.as_bytes())?;
                    writer.write_u32::<LittleEndian>(blob.len() as u32)?;
                    writer.write_all(&blob)?;
                }
                None => {
                    writer.write_u32::<LittleEndian>(EMBEDDING_VERSION)?;
                    writer.write_u32::<LittleEndian>(model_tag.len() as u32)?;
                    writer.write_all(model_tag.as_bytes())?;
                    write_body(&mut writer, &self.embeddings)?;
                }
            }

            writer.flush()?;
            // Rename alone is atomic but not durable: without this a crash can
            // leave a present-but-empty template file, locking the user out.
            writer.get_ref().sync_all()?;
        }

        fs::rename(&tmp_path, &path)?;

        // Persist the rename itself.
        if let Ok(dir) = File::open(&user_dir) {
            let _ = dir.sync_all();
        }

        Ok(())
    }

    pub fn add_embedding(&mut self, embedding: Vec<f32>) {
        self.embeddings.push(embedding);
    }
}

/// `None` when sealing is unavailable or fails, after saying so loudly.
fn seal_payload(user: &str, model_tag: &str, embeddings: &[Vec<f32>]) -> Option<Vec<u8>> {
    if !crate::seal::available() {
        tracing::warn!(
            "seal_embeddings is on but no TPM (/dev/tpmrm0) or systemd-creds was found: \
             templates are stored UNSEALED"
        );
        return None;
    }
    let sealed = (|| {
        let mut payload = Vec::new();
        write_body(&mut payload, embeddings)?;
        crate::seal::seal(user, &seal_name(user, model_tag)?, &payload)
    })();
    match sealed {
        Ok(blob) if blob.len() as u64 <= MAX_SEALED_BYTES => Some(blob),
        Ok(_) => {
            tracing::warn!("sealed blob larger than expected: templates are stored UNSEALED");
            None
        }
        Err(e) => {
            tracing::warn!("could not seal templates to the TPM ({e:#}): templates are stored UNSEALED");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "face-auth-test-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const TAG: &str = "w600k_mbf.onnx";

    /// Writes a test file with the mode a real template has, whatever the
    /// umask: a group-writable file would be refused before it's parsed, and
    /// a rejection test would pass for the wrong reason.
    fn plant(path: impl AsRef<Path>, data: impl AsRef<[u8]>) {
        fs::write(&path, data).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }

    fn sample(seed: f32) -> Vec<f32> {
        (0..EMBEDDING_DIM).map(|i| seed + i as f32 * 1e-3).collect()
    }

    #[test]
    fn round_trips() {
        let dir = tmpdir("roundtrip");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));
        store.add_embedding(sample(0.2));
        store.save("alice", &dir, TAG).unwrap();

        let loaded = EmbeddingStore::load("alice", &dir).unwrap();
        assert_eq!(loaded.embeddings.len(), 2);
        assert_eq!(loaded.embeddings[1], sample(0.2));
        assert_eq!(loaded.model_tag.as_deref(), Some(TAG));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_planted_template_files() {
        let dir = tmpdir("planted");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));
        store.save("alice", &dir, TAG).unwrap();
        let path = dir.join("alice/embeddings.bin");

        let link = dir.join("alice/second-link");
        fs::hard_link(&path, &link).unwrap();
        assert!(EmbeddingStore::load("alice", &dir).is_err(), "hard-linked file accepted");
        fs::remove_file(&link).unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(EmbeddingStore::load("alice", &dir).is_err(), "group-writable file accepted");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(EmbeddingStore::load("alice", &dir).is_ok());

        let target = dir.join("alice/target");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(EmbeddingStore::load("alice", &dir).is_err(), "symlink followed");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stores_templates_unreadable_by_others() {
        let dir = tmpdir("perms");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));
        store.save("alice", &dir, TAG).unwrap();

        let mode = |p: &str| fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("alice/embeddings.bin"), 0o640, "group reads, never writes");
        assert_eq!(mode("alice"), 0o2750, "group cannot add or replace templates");
        assert_eq!(mode("alice/lockout"), 0o2770, "lockout is the only group-writable place");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_path_traversal_in_username() {
        let dir = tmpdir("traversal");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));

        assert!(store.save("../escaped", &dir, TAG).is_err());
        assert!(store.save("../../etc/shadow", &dir, TAG).is_err());
        assert!(EmbeddingStore::load("../escaped", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_absurd_embedding_count() {
        let dir = tmpdir("count");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION_LEGACY).unwrap();
        buf.write_u32::<LittleEndian>(u32::MAX).unwrap();
        buf.write_u32::<LittleEndian>(EMBEDDING_DIM).unwrap();
        plant(dir.join("alice/embeddings.bin"), &buf);

        // Must fail on the bound, not by attempting a 96 GB allocation.
        assert!(EmbeddingStore::load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_non_finite_values() {
        let dir = tmpdir("nan");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION_LEGACY).unwrap();
        buf.write_u32::<LittleEndian>(1).unwrap();
        buf.write_u32::<LittleEndian>(EMBEDDING_DIM).unwrap();
        for _ in 0..EMBEDDING_DIM {
            buf.write_f32::<LittleEndian>(f32::NAN).unwrap();
        }
        plant(dir.join("alice/embeddings.bin"), &buf);

        assert!(EmbeddingStore::load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_trailing_bytes() {
        let dir = tmpdir("trailing");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));
        store.save("alice", &dir, TAG).unwrap();

        let path = dir.join("alice/embeddings.bin");
        let mut data = fs::read(&path).unwrap();
        data.extend_from_slice(b"extra");
        plant(&path, data);

        assert!(EmbeddingStore::load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_store_reports_no_embeddings() {
        let dir = tmpdir("missing");
        let err = EmbeddingStore::load("alice", &dir).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<FaceAuthError>(),
            Some(FaceAuthError::NoEmbeddings)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn legacy_v1_file_loads_with_unknown_tag() {
        let dir = tmpdir("legacy");
        fs::create_dir_all(dir.join("bob")).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION_LEGACY).unwrap();
        buf.write_u32::<LittleEndian>(1).unwrap();
        buf.write_u32::<LittleEndian>(EMBEDDING_DIM).unwrap();
        for v in sample(0.25) {
            buf.write_f32::<LittleEndian>(v).unwrap();
        }
        plant(dir.join("bob/embeddings.bin"), &buf);

        let loaded = EmbeddingStore::load("bob", &dir).unwrap();
        assert_eq!(loaded.embeddings.len(), 1);
        assert_eq!(loaded.model_tag, None);
        assert!(loaded.model_tag_matches("w600k_r50.onnx"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stored_model_tag_reads_the_header_only() {
        let dir = tmpdir("storedtag");
        let store = EmbeddingStore { embeddings: vec![sample(0.1)], model_tag: None };
        store.save("alice", &dir, TAG).unwrap();
        assert_eq!(EmbeddingStore::stored_model_tag("alice", &dir).unwrap().as_deref(), Some(TAG));

        // A sealed store answers without unsealing.
        plant_sealed(&dir, "bob", b"not a credential");
        assert_eq!(EmbeddingStore::stored_model_tag("bob", &dir).unwrap().as_deref(), Some(TAG));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_tag_mismatch_is_only_raised_when_known() {
        let unknown = EmbeddingStore::default();
        assert!(unknown.model_tag_matches("w600k_r50.onnx"));

        let mbf = EmbeddingStore { model_tag: Some(TAG.to_string()), ..Default::default() };
        assert!(mbf.model_tag_matches(TAG));
        assert!(!mbf.model_tag_matches("w600k_r50.onnx"));
    }

    #[test]
    fn rejects_oversized_model_tag() {
        let dir = tmpdir("taglen");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION).unwrap();
        buf.write_u32::<LittleEndian>(u32::MAX).unwrap();
        plant(dir.join("alice/embeddings.bin"), &buf);

        assert!(EmbeddingStore::load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Writes a v3 file whose blob is junk: what a cleared TPM or a swapped
    /// board looks like, and needs no TPM to build.
    fn plant_sealed(dir: &Path, user: &str, blob: &[u8]) {
        fs::create_dir_all(dir.join(user)).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION_SEALED).unwrap();
        buf.write_u32::<LittleEndian>(TAG.len() as u32).unwrap();
        buf.extend_from_slice(TAG.as_bytes());
        buf.write_u32::<LittleEndian>(blob.len() as u32).unwrap();
        buf.extend_from_slice(blob);
        let path = dir.join(user).join("embeddings.bin");
        plant(&path, &buf);
    }

    #[test]
    fn unsealable_store_is_a_specific_error_not_a_fallback() {
        let dir = tmpdir("unseal");
        plant_sealed(&dir, "alice", b"not a credential");

        let err = EmbeddingStore::load("alice", &dir).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<FaceAuthError>(),
            Some(FaceAuthError::SealUnavailable)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enrolled_check_needs_no_tpm_for_a_sealed_store() {
        let dir = tmpdir("enrolled");
        assert!(!EmbeddingStore::is_enrolled("alice", &dir).unwrap());
        plant_sealed(&dir, "alice", b"opaque");
        assert!(EmbeddingStore::is_enrolled("alice", &dir).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sealing_required_refuses_a_plaintext_store() {
        let dir = tmpdir("downgrade");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.1));
        store.save("alice", &dir, TAG).unwrap();

        assert!(EmbeddingStore::load_with("alice", &dir, false).is_ok());
        let err = EmbeddingStore::load_with("alice", &dir, true).unwrap_err();
        assert!(matches!(err.downcast_ref::<FaceAuthError>(), Some(FaceAuthError::SealRequired)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn asking_for_a_seal_never_loses_the_templates() {
        // With a usable TPM this writes v3, without one a warned-about v2.
        // Either way what was enrolled comes back.
        let dir = tmpdir("sealfallback");
        let mut store = EmbeddingStore::default();
        store.add_embedding(sample(0.3));
        store.save_with("alice", &dir, TAG, true).unwrap();

        let loaded = EmbeddingStore::load("alice", &dir).unwrap();
        assert_eq!(loaded.embeddings, vec![sample(0.3)]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_oversized_sealed_blob() {
        let dir = tmpdir("bigblob");
        fs::create_dir_all(dir.join("alice")).unwrap();
        let mut buf = Vec::new();
        buf.write_u32::<LittleEndian>(EMBEDDING_VERSION_SEALED).unwrap();
        buf.write_u32::<LittleEndian>(0).unwrap();
        buf.write_u32::<LittleEndian>(u32::MAX).unwrap();
        let path = dir.join("alice/embeddings.bin");
        plant(&path, &buf);

        assert!(EmbeddingStore::load("alice", &dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn seal_name_binds_user_and_tag_and_rejects_path_tricks() {
        assert_eq!(seal_name("alice", TAG).unwrap(), format!("vinoauthface.alice.{TAG}"));
        assert_ne!(seal_name("alice", TAG).unwrap(), seal_name("bob", TAG).unwrap());
        assert!(seal_name("alice", "../x").is_err());
        assert!(seal_name("../alice", TAG).is_err());
        assert!(seal_name("alice", "").is_err());
    }
}
