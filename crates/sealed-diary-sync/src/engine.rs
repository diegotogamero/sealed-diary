use std::collections::{BTreeSet, HashSet};
use std::io::Read;

use sealed_diary_core::{Diary, Error, ObjectId, ObjectKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::remote::{Remote, RemotePath};
use crate::{Result, SyncError};

const MAX_HEADER_LEN: u64 = 1 << 20;

/// What this device remembers between syncs with one remote. Holds object ids
/// and a hash only, nothing secret.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// Hash of the vault header as of the last sync.
    header_hash: Option<String>,
    /// Media that was on both sides after the last sync. Seeing one of these
    /// on a single side now means the other side deleted it; seeing an
    /// unknown id on a single side means it is new there.
    known_media: BTreeSet<ObjectId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub downloaded: usize,
    pub uploaded: usize,
    pub deleted: usize,
    /// Remote files that failed authentication and were left out of the vault.
    pub rejected: usize,
    /// The password was changed on another device; this one now uses it too.
    pub password_changed: bool,
}

/// Brings the vault and the remote to the same contents.
///
/// Safe to interrupt and to run from several devices at once: every step
/// leaves both sides valid, and whatever was not finished is picked up by the
/// next run.
pub fn sync(diary: &Diary, remote: &dyn Remote, state: &mut SyncState) -> Result<Report> {
    let mut report = Report::default();
    sync_header(diary, remote, state, &mut report)?;
    sync_entries(diary, remote, &mut report)?;
    sync_media(diary, remote, state, &mut report)?;
    Ok(report)
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sync_header(
    diary: &Diary,
    remote: &dyn Remote,
    state: &mut SyncState,
    report: &mut Report,
) -> Result<()> {
    let vault = diary.vault();
    let mut ours = vault.header_bytes()?;
    let theirs = match remote.download(&RemotePath::Header)? {
        Some(reader) => {
            let mut bytes = Vec::new();
            reader.take(MAX_HEADER_LEN).read_to_end(&mut bytes)?;
            Some(bytes)
        }
        None => None,
    };

    if theirs.as_ref() != Some(&ours) {
        let authentic = match theirs.as_deref().map(|header| vault.check_header(header)) {
            // Never write over somebody else's diary, or over one from a
            // newer version of the app that this one cannot judge.
            Some(Err(Error::InvalidParameter(_))) => return Err(SyncError::DifferentVault),
            Some(Err(e @ Error::UnsupportedVersion(_))) => return Err(e.into()),
            Some(result) => result.is_ok(),
            None => false,
        };
        // Both headers are genuine and differ, so the password was changed on
        // one side. If ours is untouched since the last sync, it was theirs.
        let ours_unchanged = state.header_hash.as_deref() == Some(hash(&ours).as_str());
        if let Some(theirs) = theirs.filter(|_| authentic && ours_unchanged) {
            vault.adopt_header(&theirs)?;
            ours = theirs;
            report.password_changed = true;
        } else {
            remote.upload(&RemotePath::Header, &mut ours.as_slice(), ours.len() as u64)?;
        }
    }
    state.header_hash = Some(hash(&ours));
    Ok(())
}

/// Downloads one object into the vault. Returns whether it was accepted.
fn fetch(
    diary: &Diary,
    remote: &dyn Remote,
    kind: ObjectKind,
    id: ObjectId,
    report: &mut Report,
) -> Result<bool> {
    // Gone since it was listed: another device deleted it meanwhile.
    let Some(reader) = remote.download(&RemotePath::Object(kind, id))? else {
        return Ok(false);
    };
    match diary.vault().import_raw(kind, &id, reader) {
        Ok(()) => {
            report.downloaded += 1;
            Ok(true)
        }
        // Tampered with, damaged, or still being uploaded by another device.
        // Left on the remote: if it was an upload in progress, the next sync
        // will find it whole.
        Err(Error::Corrupted | Error::UnsupportedVersion(_)) => {
            report.rejected += 1;
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

fn send(
    diary: &Diary,
    remote: &dyn Remote,
    kind: ObjectKind,
    id: ObjectId,
    report: &mut Report,
) -> Result<()> {
    let (mut file, len) = match diary.vault().open_raw(kind, &id) {
        Ok(opened) => opened,
        // Replaced or removed by the user while this sync was running.
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    remote.upload(&RemotePath::Object(kind, id), &mut file, len)?;
    report.uploaded += 1;
    Ok(())
}

fn ids(list: Vec<ObjectId>) -> HashSet<ObjectId> {
    list.into_iter().collect()
}

fn sync_entries(diary: &Diary, remote: &dyn Remote, report: &mut Report) -> Result<()> {
    let kind = ObjectKind::Entry;
    let vault = diary.vault();
    let on_remote = ids(remote.list(kind)?);
    let on_device = ids(vault.list(kind)?);

    for id in on_remote.difference(&on_device) {
        fetch(diary, remote, kind, *id, report)?;
    }

    // With every revision from both sides in hand, the revisions themselves
    // say what is obsolete; no deletion log is needed.
    let audit = diary.audit()?;
    for id in audit.readable.difference(&audit.replaced) {
        if !on_remote.contains(id) {
            send(diary, remote, kind, *id, report)?;
        }
    }
    // Only now, with their successors stored on both sides, do replaced
    // revisions go.
    for id in &audit.replaced {
        if on_remote.contains(id) {
            remote.delete(&RemotePath::Object(kind, *id))?;
        }
        if audit.readable.contains(id) {
            vault.delete(kind, id)?;
        }
        report.deleted += usize::from(on_remote.contains(id) || on_device.contains(id));
    }
    Ok(())
}

fn sync_media(
    diary: &Diary,
    remote: &dyn Remote,
    state: &mut SyncState,
    report: &mut Report,
) -> Result<()> {
    let kind = ObjectKind::Media;
    let vault = diary.vault();
    let on_remote = ids(remote.list(kind)?);
    let on_device = ids(vault.list(kind)?);
    let audit = diary.audit()?;

    // Media is deleted only when a device removed it on purpose (it was on
    // both sides before and is now on one) and nothing readable points to it.
    // In any doubt it is copied instead: an orphan costs space, a deleted
    // video costs a memory.
    let removable = |id: &ObjectId| {
        state.known_media.contains(id)
            && audit.unreadable == 0
            && !audit.referenced_media.contains(id)
    };

    let mut on_both: BTreeSet<ObjectId> = on_remote.intersection(&on_device).copied().collect();
    for id in on_device.difference(&on_remote) {
        if removable(id) {
            vault.delete(kind, id)?;
            report.deleted += 1;
        } else {
            send(diary, remote, kind, *id, report)?;
            on_both.insert(*id);
        }
    }
    for id in on_remote.difference(&on_device) {
        if removable(id) {
            remote.delete(&RemotePath::Object(kind, *id))?;
            report.deleted += 1;
        } else if fetch(diary, remote, kind, *id, report)? {
            on_both.insert(*id);
        }
    }
    state.known_media = on_both;
    Ok(())
}
