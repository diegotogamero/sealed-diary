use std::fs;
use std::path::{Path, PathBuf};

use sealed_diary_core::{Diary, EntryContent, Error, KdfParams, ObjectKind, Vault};

const FAST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
};

fn new_diary(dir: &Path) -> Diary {
    Diary::new(Vault::create(dir, "pw", FAST_KDF).unwrap().0)
}

fn open_diary(dir: &Path) -> Diary {
    Diary::new(Vault::open(dir, "pw").unwrap())
}

fn content(date: &str, body: &str) -> EntryContent {
    EntryContent {
        date: date.into(),
        title: format!("Título {date}"),
        body: body.into(),
        ..Default::default()
    }
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(children) = fs::read_dir(&path) else {
            continue;
        };
        for child in children {
            let child = child.unwrap().path();
            if child.is_dir() {
                pending.push(child);
            } else {
                files.push(child);
            }
        }
    }
    files
}

/// Stands in for a sync client: copies every file `to` does not have yet.
/// Like a real one on first contact, it never deletes.
fn sync(from: &Path, to: &Path) {
    for file in files_in(from) {
        let target = to.join(file.strip_prefix(from).unwrap());
        if !target.exists() {
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(&file, &target).unwrap();
        }
    }
}

#[test]
fn entries_survive_reopen_and_sort_by_date() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    diary.create(content("2026-10-08", "ayer")).unwrap();
    diary.create(content("2026-10-09", "hoy")).unwrap();
    diary.create(content("2026-01-01", "enero")).unwrap();
    drop(diary);

    let listing = open_diary(dir.path()).list().unwrap();
    let bodies: Vec<_> = listing
        .entries
        .iter()
        .map(|e| e.current.content.body.as_str())
        .collect();
    assert_eq!(bodies, ["hoy", "ayer", "enero"]);
    assert_eq!(listing.unreadable, 0);
}

#[test]
fn update_replaces_previous_revision() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    let first = diary.create(content("2026-10-09", "borrador")).unwrap();
    let second = diary
        .update(&first.id, &[first.revision], content("2026-10-09", "final"))
        .unwrap();

    let state = diary.get(&first.id).unwrap().unwrap();
    assert_eq!(state.current, second);
    assert!(state.conflicts.is_empty());
    assert_eq!(
        diary.vault().list(ObjectKind::Entry).unwrap(),
        [second.revision]
    );
}

#[test]
fn rejects_malformed_dates() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    for date in [
        "",
        "2026-13-01",
        "2026-10-32",
        "09/10/2026",
        "2026-1-01",
        "2026-10-0x",
    ] {
        assert!(
            matches!(
                diary.create(content(date, "x")),
                Err(Error::InvalidParameter(_))
            ),
            "{date}"
        );
    }
}

#[test]
fn delete_removes_entry_and_its_media() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    let photo = diary
        .attach(&b"jpeg bytes"[..], "foto.jpg", "image/jpeg")
        .unwrap();
    let mut with_photo = content("2026-10-09", "con foto");
    with_photo.attachments.push(photo.clone());
    let entry = diary.create(with_photo).unwrap();
    assert_eq!(diary.open_attachment(&photo).unwrap().len(), 10);

    diary.delete(&entry.id, &[entry.revision]).unwrap();
    assert!(diary.get(&entry.id).unwrap().is_none());
    assert!(diary.list().unwrap().entries.is_empty());
    assert!(diary.vault().list(ObjectKind::Media).unwrap().is_empty());
}

#[test]
fn removing_an_attachment_deletes_only_that_media() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    let photo = diary.attach(&b"foto"[..], "a.jpg", "image/jpeg").unwrap();
    let song = diary
        .attach(&b"cancion"[..], "b.mp3", "audio/mpeg")
        .unwrap();
    let mut both = content("2026-10-09", "dos adjuntos");
    both.attachments = vec![photo.clone(), song.clone()];
    let entry = diary.create(both.clone()).unwrap();

    both.attachments = vec![song.clone()];
    diary.update(&entry.id, &[entry.revision], both).unwrap();
    assert_eq!(
        diary.vault().list(ObjectKind::Media).unwrap(),
        [song.object]
    );
}

#[test]
fn update_refuses_revisions_of_another_entry() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    let a = diary.create(content("2026-10-08", "a")).unwrap();
    let b = diary.create(content("2026-10-09", "b")).unwrap();
    let mixed_up = diary.update(&a.id, &[b.revision], content("2026-10-08", "a2"));
    assert!(matches!(mixed_up, Err(Error::InvalidParameter(_))));
    assert_eq!(diary.list().unwrap().entries.len(), 2);
}

#[test]
fn concurrent_edits_on_two_devices_become_a_conflict_then_resolve() {
    let (laptop, phone) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let on_laptop = new_diary(laptop.path());
    let original = on_laptop.create(content("2026-10-09", "original")).unwrap();
    sync(laptop.path(), phone.path());
    let on_phone = open_diary(phone.path());

    // Both edit the same entry with no connection between them.
    let photo = on_phone
        .attach(&b"foto"[..], "a.jpg", "image/jpeg")
        .unwrap();
    let mut phone_edit = content("2026-10-09", "desde el móvil");
    phone_edit.attachments.push(photo.clone());
    on_phone
        .update(&original.id, &[original.revision], phone_edit)
        .unwrap();
    on_laptop
        .update(
            &original.id,
            &[original.revision],
            content("2026-10-09", "desde el portátil"),
        )
        .unwrap();

    sync(laptop.path(), phone.path());
    sync(phone.path(), laptop.path());

    // Neither edit is lost, on either device.
    for diary in [&on_laptop, &on_phone] {
        let listing = diary.list().unwrap();
        assert_eq!(listing.entries.len(), 1);
        let state = &listing.entries[0];
        assert_eq!(state.conflicts.len(), 1);
        let mut bodies = [
            state.current.content.body.as_str(),
            state.conflicts[0].content.body.as_str(),
        ];
        bodies.sort();
        assert_eq!(bodies, ["desde el móvil", "desde el portátil"]);
    }

    // The user keeps the laptop text; the phone's photo is dropped with it.
    let state = on_laptop.get(&original.id).unwrap().unwrap();
    on_laptop
        .update(
            &original.id,
            &state.revisions(),
            content("2026-10-09", "resuelto"),
        )
        .unwrap();
    let state = on_laptop.get(&original.id).unwrap().unwrap();
    assert_eq!(state.current.content.body, "resuelto");
    assert!(state.conflicts.is_empty());
    assert!(
        on_laptop
            .vault()
            .list(ObjectKind::Media)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn replaced_revision_cannot_be_rolled_back() {
    let (live, stale) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let diary = new_diary(live.path());
    let old = diary.create(content("2026-10-09", "antiguo")).unwrap();
    sync(live.path(), stale.path());
    diary
        .update(&old.id, &[old.revision], content("2026-10-09", "nuevo"))
        .unwrap();

    // A stale device, or a hostile backend, puts the old revision back.
    sync(stale.path(), live.path());
    let state = diary.get(&old.id).unwrap().unwrap();
    assert_eq!(state.current.content.body, "nuevo");
    assert!(state.conflicts.is_empty());
}

#[test]
fn unseen_edit_survives_a_delete() {
    let (laptop, phone) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let on_laptop = new_diary(laptop.path());
    let original = on_laptop.create(content("2026-10-09", "original")).unwrap();
    sync(laptop.path(), phone.path());
    let on_phone = open_diary(phone.path());

    on_laptop
        .delete(&original.id, &[original.revision])
        .unwrap();
    on_phone
        .update(
            &original.id,
            &[original.revision],
            content("2026-10-09", "escrito después"),
        )
        .unwrap();
    sync(phone.path(), laptop.path());

    let state = on_laptop.get(&original.id).unwrap().unwrap();
    assert_eq!(state.current.content.body, "escrito después");
}

#[test]
fn damaged_revision_is_reported_without_hiding_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let diary = new_diary(dir.path());
    let photo = diary.attach(&b"foto"[..], "a.jpg", "image/jpeg").unwrap();
    let mut with_photo = content("2026-10-08", "con foto");
    with_photo.attachments.push(photo.clone());
    let kept = diary.create(with_photo).unwrap();
    let damaged = diary.create(content("2026-10-09", "dañada")).unwrap();

    let path = files_in(&dir.path().join("entries"))
        .into_iter()
        .find(|p| p.file_name().unwrap().to_str() == Some(&damaged.revision.to_hex()))
        .unwrap();
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&path, bytes).unwrap();
    // Sync clients leave their own files around; they are not entries.
    fs::write(path.with_file_name(".syncthing.partial.tmp"), b"junk").unwrap();

    let listing = diary.list().unwrap();
    assert_eq!(listing.unreadable, 1);
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].current, kept);

    // With a revision unreadable, media is kept rather than risk deleting a
    // file that revision still points to.
    diary.delete(&kept.id, &[kept.revision]).unwrap();
    assert_eq!(
        diary.vault().list(ObjectKind::Media).unwrap(),
        [photo.object]
    );
}
