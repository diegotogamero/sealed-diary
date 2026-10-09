//! Diary entries on top of a vault.
//!
//! An entry is never edited in place. Every save writes a new immutable
//! *revision* naming the revisions it replaces, then removes those. The current
//! state of an entry is its *heads*: the revisions nothing else replaces.
//!
//! This is what makes sync over a dumb folder safe with no server logic. Two
//! devices that edit the same entry while offline each add a revision; after
//! the folders merge both are heads, and the entry shows the newer one with the
//! other kept as a conflict instead of one silently overwriting the other. A
//! replaced revision that reappears (stale device, hostile backend) is still
//! named by its successor, so it cannot come back as current.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::object::{ObjectId, ObjectKind, ObjectReader};
use crate::vault::Vault;

const REVISION_VERSION: u32 = 1;

/// Stable identity of an entry across all its revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(ObjectId);

impl EntryId {
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }

    pub fn from_hex(text: &str) -> Result<Self> {
        ObjectId::from_hex(text).map(Self)
    }
}

/// A media file stored in the vault and referenced from an entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub object: ObjectId,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryContent {
    /// The day the entry is about, `YYYY-MM-DD`, in the writer's own calendar.
    pub date: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl EntryContent {
    fn validate(&self) -> Result<()> {
        let b = self.date.as_bytes();
        let number = |range: std::ops::Range<usize>| -> Option<u32> {
            let digits = b.get(range)?;
            digits
                .iter()
                .all(u8::is_ascii_digit)
                .then(|| digits.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
        };
        let valid = b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && number(0..4).is_some()
            && number(5..7).is_some_and(|m| (1..=12).contains(&m))
            && number(8..10).is_some_and(|d| (1..=31).contains(&d));
        if valid {
            Ok(())
        } else {
            Err(Error::InvalidParameter("date must be YYYY-MM-DD"))
        }
    }
}

/// One revision of an entry, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub revision: ObjectId,
    pub saved_at_ms: u64,
    pub content: EntryContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryState {
    pub current: Entry,
    /// Other heads of the same entry, newest first. Non-empty only after
    /// concurrent edits on different devices.
    pub conflicts: Vec<Entry>,
}

impl EntryState {
    /// Everything shown to the user; pass to [`Diary::update`] to save a
    /// version that settles the conflict.
    pub fn revisions(&self) -> Vec<ObjectId> {
        std::iter::once(&self.current)
            .chain(&self.conflicts)
            .map(|e| e.revision)
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct Listing {
    /// Newest diary date first.
    pub entries: Vec<EntryState>,
    /// Revisions that failed to decrypt or parse: damaged, tampered with, or
    /// written by a newer version of the app. Reported, never fatal, so one
    /// bad file cannot lock the user out of the rest of the diary.
    pub unreadable: usize,
}

#[derive(Serialize, Deserialize)]
struct Revision {
    v: u32,
    entry: EntryId,
    parents: Vec<ObjectId>,
    saved_at_ms: u64,
    /// `None` marks the entry as deleted.
    content: Option<EntryContent>,
}

#[derive(Debug)]
pub struct Diary {
    vault: Vault,
}

impl Diary {
    pub fn new(vault: Vault) -> Self {
        Self { vault }
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn list(&self) -> Result<Listing> {
        let (revisions, unreadable) = self.load_all()?;
        let replaced: HashSet<ObjectId> = revisions
            .iter()
            .flat_map(|(_, r)| r.parents.iter().copied())
            .collect();

        let mut heads: HashMap<EntryId, Vec<Entry>> = HashMap::new();
        for (id, revision) in revisions {
            if replaced.contains(&id) {
                continue;
            }
            // A deleted head contributes nothing; the entry stays visible only
            // if some other head still has content.
            if let Some(content) = revision.content {
                heads.entry(revision.entry).or_default().push(Entry {
                    id: revision.entry,
                    revision: id,
                    saved_at_ms: revision.saved_at_ms,
                    content,
                });
            }
        }

        let mut entries: Vec<EntryState> = heads
            .into_values()
            .map(|mut heads| {
                heads.sort_by_key(|e| std::cmp::Reverse((e.saved_at_ms, e.revision)));
                let current = heads.remove(0);
                EntryState {
                    current,
                    conflicts: heads,
                }
            })
            .collect();
        entries.sort_by(|a, b| {
            let key = |s: &EntryState| (s.current.content.date.clone(), s.current.saved_at_ms);
            key(b).cmp(&key(a))
        });
        Ok(Listing {
            entries,
            unreadable,
        })
    }

    pub fn get(&self, id: &EntryId) -> Result<Option<EntryState>> {
        Ok(self
            .list()?
            .entries
            .into_iter()
            .find(|state| state.current.id == *id))
    }

    pub fn create(&self, content: EntryContent) -> Result<Entry> {
        content.validate()?;
        self.write(EntryId(ObjectId::random()?), &[], Some(content))
    }

    /// Saves a new version of an entry.
    ///
    /// `replaces` lists the revisions the user was looking at. A revision that
    /// arrived from another device in the meantime is not in that list, so it
    /// survives as a conflict rather than being overwritten unseen.
    pub fn update(
        &self,
        id: &EntryId,
        replaces: &[ObjectId],
        content: EntryContent,
    ) -> Result<Entry> {
        content.validate()?;
        let dropped = self.attachments_of(id, replaces)?;
        let entry = self.write(*id, replaces, Some(content))?;
        self.remove_unreferenced_media(dropped)?;
        Ok(entry)
    }

    /// Deletes an entry and its media. Same `replaces` contract as
    /// [`Diary::update`]: an unseen concurrent edit keeps the entry alive.
    pub fn delete(&self, id: &EntryId, replaces: &[ObjectId]) -> Result<()> {
        let dropped = self.attachments_of(id, replaces)?;
        self.write_revision(*id, replaces, None)?;
        self.remove_unreferenced_media(dropped)
    }

    /// Encrypts a media file into the vault. The result does nothing until it
    /// is added to an entry's attachments and that entry is saved.
    pub fn attach<R: Read>(&self, reader: R, name: &str, mime: &str) -> Result<Attachment> {
        let (object, size) = self.vault.put(ObjectKind::Media, reader)?;
        Ok(Attachment {
            object,
            name: name.to_owned(),
            mime: mime.to_owned(),
            size,
        })
    }

    pub fn open_attachment(&self, attachment: &Attachment) -> Result<ObjectReader<File>> {
        self.vault.reader(ObjectKind::Media, &attachment.object)
    }

    fn write(
        &self,
        id: EntryId,
        replaces: &[ObjectId],
        content: Option<EntryContent>,
    ) -> Result<Entry> {
        let (revision, saved_at_ms, content) = self.write_revision(id, replaces, content)?;
        Ok(Entry {
            id,
            revision,
            saved_at_ms,
            content: content.expect("write is only called with content"),
        })
    }

    fn write_revision(
        &self,
        id: EntryId,
        replaces: &[ObjectId],
        content: Option<EntryContent>,
    ) -> Result<(ObjectId, u64, Option<EntryContent>)> {
        let revision = Revision {
            v: REVISION_VERSION,
            entry: id,
            parents: replaces.to_vec(),
            saved_at_ms: now_ms(),
            content,
        };
        let json = Zeroizing::new(serde_json::to_vec(&revision)?);
        let (object, _) = self.vault.put(ObjectKind::Entry, json.as_slice())?;
        // New revision is durable before the old ones go: a crash in between
        // leaves both, and the parent link already marks the old as replaced.
        for parent in replaces {
            self.vault.delete(ObjectKind::Entry, parent)?;
        }
        Ok((object, revision.saved_at_ms, revision.content))
    }

    /// Media referenced by `revisions`, after checking they belong to `id`.
    /// Revisions already gone are skipped.
    fn attachments_of(&self, id: &EntryId, revisions: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let mut media = Vec::new();
        for object in revisions {
            let revision = match self.read_revision(object) {
                Ok(revision) => revision,
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if revision.entry != *id {
                return Err(Error::InvalidParameter(
                    "revision belongs to a different entry",
                ));
            }
            let attachments = revision.content.into_iter().flat_map(|c| c.attachments);
            media.extend(attachments.map(|a| a.object));
        }
        Ok(media)
    }

    /// Deletes those of `candidates` that no remaining revision references.
    fn remove_unreferenced_media(&self, candidates: Vec<ObjectId>) -> Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        let (revisions, unreadable) = self.load_all()?;
        // A revision we cannot read may still reference the file. Keeping an
        // orphan costs disk space; deleting a referenced video loses a memory.
        if unreadable > 0 {
            return Ok(());
        }
        let referenced: HashSet<ObjectId> = revisions
            .into_iter()
            .filter_map(|(_, r)| r.content)
            .flat_map(|c| c.attachments)
            .map(|a| a.object)
            .collect();
        for object in candidates {
            if !referenced.contains(&object) {
                self.vault.delete(ObjectKind::Media, &object)?;
            }
        }
        Ok(())
    }

    fn read_revision(&self, object: &ObjectId) -> Result<Revision> {
        let mut json = Zeroizing::new(Vec::new());
        self.vault.get(ObjectKind::Entry, object, &mut *json)?;
        let revision: Revision = serde_json::from_slice(&json).map_err(|_| Error::Corrupted)?;
        if revision.v != REVISION_VERSION {
            return Err(Error::UnsupportedVersion(revision.v));
        }
        Ok(revision)
    }

    fn load_all(&self) -> Result<(Vec<(ObjectId, Revision)>, usize)> {
        let mut revisions = Vec::new();
        let mut unreadable = 0;
        for object in self.vault.list(ObjectKind::Entry)? {
            match self.read_revision(&object) {
                Ok(revision) => revisions.push((object, revision)),
                // Removed by a sync between listing and reading.
                Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
                Err(Error::Io(e)) => return Err(e.into()),
                Err(_) => unreadable += 1,
            }
        }
        Ok((revisions, unreadable))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}
