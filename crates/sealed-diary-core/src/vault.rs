//! A vault on a local directory.
//!
//! ```text
//! <root>/vault.json            key slots, no secrets
//! <root>/entries/ab/abcdef…    encrypted entry revisions, named by random id
//! <root>/media/ab/abcdef…      encrypted photos, songs, videos
//! ```
//!
//! This directory is the unit of sync: point Syncthing at it, or mirror it to
//! WebDAV. Nothing in it is readable without the password or recovery key.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};

use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{KdfParams, MasterKey, RecoveryKey, VaultHeader, random_bytes};
use crate::object::{
    DEFAULT_CHUNK_LOG2, ObjectId, ObjectKind, ObjectReader, decrypt_bytes, decrypt_object,
    encrypt_bytes, encrypt_object,
};

const HEADER_FILE: &str = "vault.json";

// Local secrets are not addressed by id; a fresh nonce per sealing keeps
// reusing one id safe.
const LOCAL_ID: ObjectId = ObjectId::ZERO;

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
        fs::create_dir_all(&root)?;
        let (header, key, recovery) = VaultHeader::create(password, kdf)?;
        write_atomic(&root.join(HEADER_FILE), &header.to_json()?)?;
        Ok((Self { root, key }, recovery))
    }

    pub fn exists(root: impl AsRef<Path>) -> bool {
        root.as_ref().join(HEADER_FILE).exists()
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

    /// The header file exactly as stored, for copying to a sync remote.
    pub fn header_bytes(&self) -> Result<Vec<u8>> {
        Ok(fs::read(self.root.join(HEADER_FILE))?)
    }

    /// Whether `header` is a genuine header of this same vault, written by
    /// someone who holds its master key.
    pub fn check_header(&self, header: &[u8]) -> Result<()> {
        let theirs = VaultHeader::from_json(header)?;
        if theirs.vault_id() != read_header(&self.root)?.vault_id() {
            return Err(Error::InvalidParameter(
                "header belongs to a different vault",
            ));
        }
        theirs.verify(&self.key)
    }

    /// Replaces the local header with one from another device, picking up a
    /// password change made there. Refuses anything [`Vault::check_header`]
    /// would.
    pub fn adopt_header(&self, header: &[u8]) -> Result<()> {
        self.check_header(header)?;
        write_atomic(&self.root.join(HEADER_FILE), header)
    }

    /// Puts a header fetched from a sync remote into an empty `root`, so this
    /// device can join an existing vault. It is only trusted once
    /// [`Vault::open`] succeeds with the user's password.
    pub fn install_header(root: impl AsRef<Path>, header: &[u8]) -> Result<()> {
        let root = root.as_ref();
        if Self::exists(root) {
            return Err(Error::InvalidParameter("a vault already exists here"));
        }
        VaultHeader::from_json(header)?;
        fs::create_dir_all(root)?;
        write_atomic(&root.join(HEADER_FILE), header)
    }

    /// Opens an object's ciphertext, with its length, for copying to a remote.
    pub fn open_raw(&self, kind: ObjectKind, id: &ObjectId) -> Result<(File, u64)> {
        let file = File::open(self.object_path(kind, id))?;
        let len = file.metadata()?.len();
        Ok((file, len))
    }

    /// Stores ciphertext fetched from a remote under `id`, but only after it
    /// decrypts in full: nothing unauthenticated ever enters the vault.
    pub fn import_raw<R: Read>(
        &self,
        kind: ObjectKind,
        id: &ObjectId,
        mut reader: R,
    ) -> Result<()> {
        let path = self.object_path(kind, id);
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;
        let tmp = tmp_path(&path)?;
        let imported = (|| {
            let mut file = File::create(&tmp)?;
            io::copy(&mut reader, &mut file)?;
            file.sync_all()?;
            decrypt_object(
                &self.key,
                kind,
                id,
                BufReader::new(File::open(&tmp)?),
                io::sink(),
            )?;
            fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if imported.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        imported
    }

    /// Encrypts a small secret that stays on this device, such as the sync
    /// server's credentials, so it is unreadable while the diary is locked.
    pub fn seal_local(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        encrypt_bytes(&self.key, ObjectKind::Local, &LOCAL_ID, plaintext)
    }

    pub fn open_local(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        decrypt_bytes(&self.key, ObjectKind::Local, &LOCAL_ID, sealed)
    }

    /// Encrypts `reader` into a new object. Returns its id and plaintext size.
    pub fn put<R: Read>(&self, kind: ObjectKind, reader: R) -> Result<(ObjectId, u64)> {
        let id = ObjectId::random()?;
        let path = self.object_path(kind, &id);
        fs::create_dir_all(path.parent().expect("object path has a parent"))?;

        let tmp = tmp_path(&path)?;
        let written = (|| {
            let mut file = BufWriter::new(File::create(&tmp)?);
            let size = encrypt_object(&self.key, kind, &id, DEFAULT_CHUNK_LOG2, reader, &mut file)?;
            file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            fs::rename(&tmp, &path)?;
            Ok(size)
        })();
        match written {
            Ok(size) => Ok((id, size)),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Decrypts an object into `writer`. See [`decrypt_object`] for what
    /// `writer` holds on error.
    pub fn get<W: Write>(&self, kind: ObjectKind, id: &ObjectId, writer: W) -> Result<u64> {
        let file = BufReader::new(File::open(self.object_path(kind, id))?);
        decrypt_object(&self.key, kind, id, file, writer)
    }

    pub fn reader(&self, kind: ObjectKind, id: &ObjectId) -> Result<ObjectReader<File>> {
        ObjectReader::open(&self.key, kind, id, File::open(self.object_path(kind, id))?)
    }

    /// Removes an object. Already being gone is not an error: a sync from
    /// another device may have removed it first.
    pub fn delete(&self, kind: ObjectKind, id: &ObjectId) -> Result<()> {
        match fs::remove_file(self.object_path(kind, id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Ids of all objects of `kind`. Files that are not objects, such as a
    /// sync client's temporary files, are skipped.
    pub fn list(&self, kind: ObjectKind) -> Result<Vec<ObjectId>> {
        let mut ids = Vec::new();
        let shards = match fs::read_dir(self.root.join(kind.dir())) {
            Ok(shards) => shards,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ids),
            Err(e) => return Err(e.into()),
        };
        for shard in shards {
            let shard = shard?.path();
            if !shard.is_dir() {
                continue;
            }
            for file in fs::read_dir(&shard)? {
                let name = file?.file_name();
                let id = name.to_str().and_then(|n| ObjectId::from_hex(n).ok());
                if let Some(id) = id.filter(|id| self.object_path(kind, id).is_file()) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    fn object_path(&self, kind: ObjectKind, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(kind.dir()).join(&hex[..2]).join(hex)
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
