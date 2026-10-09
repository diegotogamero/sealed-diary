//! A vault on a local directory.
//!
//! ```text
//! <root>/vault.json            key slots, no secrets
//! <root>/objects/ab/abcdef…    encrypted objects, named by random id
//! ```
//!
//! This directory is the unit of sync: point Syncthing at it, or mirror it to
//! WebDAV. Nothing in it is readable without the password or recovery key.

use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::keys::{KdfParams, MasterKey, RecoveryKey, VaultHeader, random_bytes};
use crate::object::{DEFAULT_CHUNK_LOG2, ObjectId, ObjectReader, decrypt_object, encrypt_object};

const HEADER_FILE: &str = "vault.json";
const OBJECTS_DIR: &str = "objects";

pub struct Vault {
    root: PathBuf,
    key: MasterKey,
}

impl Vault {
    /// Creates a vault in `root`. The returned recovery key must be shown to
    /// the user now; it is not stored anywhere.
    pub fn create(
        root: impl AsRef<Path>,
        password: &str,
        kdf: KdfParams,
    ) -> Result<(Self, RecoveryKey)> {
        let root = root.as_ref().to_path_buf();
        if root.join(HEADER_FILE).exists() {
            return Err(Error::InvalidParameter("a vault already exists here"));
        }
        fs::create_dir_all(root.join(OBJECTS_DIR))?;
        let (header, key, recovery) = VaultHeader::create(password, kdf)?;
        write_atomic(&root.join(HEADER_FILE), &header.to_json()?)?;
        Ok((Self { root, key }, recovery))
    }

    pub fn open(root: impl AsRef<Path>, password: &str) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let key = read_header(&root)?.unlock(password)?;
        Ok(Self { root, key })
    }

    pub fn open_with_recovery(root: impl AsRef<Path>, recovery: &RecoveryKey) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let key = read_header(&root)?.unlock_with_recovery(recovery)?;
        Ok(Self { root, key })
    }

    pub fn change_password(&self, new_password: &str, kdf: KdfParams) -> Result<()> {
        let mut header = read_header(&self.root)?;
        header.change_password(&self.key, new_password, kdf)?;
        write_atomic(&self.root.join(HEADER_FILE), &header.to_json()?)
    }

    /// Encrypts `reader` into a new object and returns its id.
    pub fn put<R: Read>(&self, reader: R) -> Result<ObjectId> {
        let id = ObjectId::random()?;
        let path = self.object_path(&id);
        let dir = path.parent().expect("object path has a parent");
        fs::create_dir_all(dir)?;

        let tmp = tmp_path(&path)?;
        let written = (|| {
            let mut file = BufWriter::new(File::create(&tmp)?);
            encrypt_object(&self.key, &id, DEFAULT_CHUNK_LOG2, reader, &mut file)?;
            file.into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
            fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(id)
    }

    /// Decrypts an object into `writer`. See [`decrypt_object`] for what
    /// `writer` holds on error.
    pub fn get<W: Write>(&self, id: &ObjectId, writer: W) -> Result<u64> {
        let file = BufReader::new(File::open(self.object_path(id))?);
        decrypt_object(&self.key, id, file, writer)
    }

    pub fn reader(&self, id: &ObjectId) -> Result<ObjectReader<File>> {
        ObjectReader::open(&self.key, id, File::open(self.object_path(id))?)
    }

    pub fn delete(&self, id: &ObjectId) -> Result<()> {
        Ok(fs::remove_file(self.object_path(id))?)
    }

    fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(OBJECTS_DIR).join(&hex[..2]).join(hex)
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault").field("root", &self.root).finish()
    }
}

fn read_header(root: &Path) -> Result<VaultHeader> {
    VaultHeader::from_json(&fs::read(root.join(HEADER_FILE))?)
}

fn tmp_path(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().expect("path has a file name");
    let suffix = hex::encode(random_bytes::<6>()?);
    Ok(path.with_file_name(format!(".{}.{suffix}.tmp", name.to_string_lossy())))
}

/// Write to a sibling temp file and rename, so a crash or a sync client
/// reading mid-write never sees a half-written file.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = tmp_path(path)?;
    let written = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(written?)
}
