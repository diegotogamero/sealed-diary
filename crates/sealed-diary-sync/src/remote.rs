use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;

use sealed_diary_core::{ObjectId, ObjectKind};

use crate::Result;

/// A file on the remote. The layout is flat, one directory per kind, so a
/// whole kind can be listed in a single request.
#[derive(Clone, Copy, Debug)]
pub enum RemotePath {
    Header,
    Object(ObjectKind, ObjectId),
}

impl RemotePath {
    pub fn relative(&self) -> String {
        match self {
            Self::Header => "vault.json".to_owned(),
            Self::Object(kind, id) => format!("{}/{}", kind.dir(), id.to_hex()),
        }
    }
}

pub trait Remote {
    fn list(&self, kind: ObjectKind) -> Result<Vec<ObjectId>>;
    /// `None` if the file does not exist.
    fn download(&self, path: &RemotePath) -> Result<Option<Box<dyn Read>>>;
    fn upload(&self, path: &RemotePath, body: &mut dyn Read, len: u64) -> Result<()>;
    /// Deleting a file that is already gone succeeds.
    fn delete(&self, path: &RemotePath) -> Result<()>;
}

/// A folder as the remote: a mounted NAS share, a USB drive, a backup disk.
pub struct DirRemote {
    root: PathBuf,
}

impl DirRemote {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Remote for DirRemote {
    fn list(&self, kind: ObjectKind) -> Result<Vec<ObjectId>> {
        let entries = match fs::read_dir(self.root.join(kind.dir())) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            ids.extend(name.to_str().and_then(|n| ObjectId::from_hex(n).ok()));
        }
        Ok(ids)
    }

    fn download(&self, path: &RemotePath) -> Result<Option<Box<dyn Read>>> {
        match File::open(self.root.join(path.relative())) {
            Ok(file) => Ok(Some(Box::new(file))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn upload(&self, path: &RemotePath, body: &mut dyn Read, _len: u64) -> Result<()> {
        let target = self.root.join(path.relative());
        fs::create_dir_all(target.parent().expect("remote path has a parent"))?;
        // Written under another name first so a reader never sees half a file.
        let partial = target.with_extension("partial");
        let mut file = File::create(&partial)?;
        io::copy(body, &mut file)?;
        file.sync_all()?;
        Ok(fs::rename(partial, target)?)
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        match fs::remove_file(self.root.join(path.relative())) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}
