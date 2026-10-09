use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use sealed_diary_core::{Diary, EntryContent, KdfParams, ObjectKind, Vault};
use sealed_diary_sync::{DirRemote, Remote, Report, SyncError, SyncState, WebDav, sync};

const FAST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
};

/// One device: its own vault folder and its own memory of past syncs.
struct Device {
    dir: tempfile::TempDir,
    diary: Diary,
    state: SyncState,
}

impl Device {
    fn first() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let diary = Diary::new(Vault::create(dir.path(), "pw", FAST_KDF).unwrap().0);
        Self {
            dir,
            diary,
            state: SyncState::default(),
        }
    }

    /// A new device joining an existing diary: it has nothing but the remote
    /// address and the password.
    fn join(remote: &dyn Remote) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut header = Vec::new();
        let path = sealed_diary_sync::RemotePath::Header;
        remote
            .download(&path)
            .unwrap()
            .unwrap()
            .read_to_end(&mut header)
            .unwrap();
        Vault::install_header(dir.path(), &header).unwrap();
        let diary = Diary::new(Vault::open(dir.path(), "pw").unwrap());
        Self {
            dir,
            diary,
            state: SyncState::default(),
        }
    }

    fn sync(&mut self, remote: &dyn Remote) -> Report {
        sync(&self.diary, remote, &mut self.state).unwrap()
    }

    fn bodies(&self) -> Vec<String> {
        let listing = self.diary.list().unwrap();
        assert_eq!(listing.unreadable, 0);
        listing
            .entries
            .into_iter()
            .map(|e| e.current.content.body)
            .collect()
    }

    fn media(&self) -> usize {
        self.diary.vault().list(ObjectKind::Media).unwrap().len()
    }
}

fn content(date: &str, body: &str) -> EntryContent {
    EntryContent {
        date: date.into(),
        body: body.into(),
        ..Default::default()
    }
}

fn remote_files(root: &Path, kind: ObjectKind) -> usize {
    fs::read_dir(root.join(kind.dir())).map_or(0, |d| d.count())
}

/// The full life of a diary shared by a laptop and a phone, run against any
/// kind of remote.
fn two_devices_stay_in_step(remote: &dyn Remote) {
    let mut laptop = Device::first();
    let photo = laptop
        .diary
        .attach(&[7u8; 200_000][..], "a.jpg", "image/jpeg")
        .unwrap();
    let mut with_photo = content("2026-10-08", "con foto");
    with_photo.attachments.push(photo.clone());
    let first = laptop.diary.create(with_photo).unwrap();
    laptop
        .diary
        .create(content("2026-10-09", "segunda"))
        .unwrap();
    let up = laptop.sync(remote);
    assert_eq!((up.uploaded, up.downloaded, up.deleted), (3, 0, 0));
    assert_eq!(
        laptop.sync(remote),
        Report::default(),
        "a second sync has nothing to do"
    );

    let mut phone = Device::join(remote);
    let down = phone.sync(remote);
    assert_eq!((down.downloaded, down.uploaded, down.rejected), (3, 0, 0));
    assert_eq!(phone.bodies(), ["segunda", "con foto"]);
    assert_eq!(phone.diary.open_attachment(&photo).unwrap().len(), 200_000);

    // An edit on the phone reaches the laptop, and the old revision goes.
    let state = phone.diary.get(&first.id).unwrap().unwrap();
    let mut edited = state.current.content.clone();
    edited.body = "editada en el móvil".into();
    phone
        .diary
        .update(&first.id, &state.revisions(), edited)
        .unwrap();
    phone.sync(remote);
    laptop.sync(remote);
    assert_eq!(laptop.bodies(), ["segunda", "editada en el móvil"]);
    assert_eq!(
        laptop.diary.vault().list(ObjectKind::Entry).unwrap().len(),
        2
    );

    // Both edit the same entry offline: nobody's text is lost.
    let seen = laptop.diary.get(&first.id).unwrap().unwrap();
    for (device, body) in [(&laptop, "versión portátil"), (&phone, "versión móvil")] {
        let mut edit = seen.current.content.clone();
        edit.body = body.into();
        device
            .diary
            .update(&first.id, &seen.revisions(), edit)
            .unwrap();
    }
    laptop.sync(remote);
    phone.sync(remote);
    laptop.sync(remote);
    for device in [&laptop, &phone] {
        let state = device.diary.get(&first.id).unwrap().unwrap();
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(
            device.media(),
            1,
            "the photo is still attached in both versions"
        );
    }

    // Deleting on the laptop removes entry and photo everywhere.
    let state = laptop.diary.get(&first.id).unwrap().unwrap();
    laptop.diary.delete(&first.id, &state.revisions()).unwrap();
    assert_eq!(laptop.media(), 0);
    laptop.sync(remote);
    assert!(remote.list(ObjectKind::Media).unwrap().is_empty());
    phone.sync(remote);
    assert_eq!(phone.bodies(), ["segunda"]);
    assert_eq!(phone.media(), 0);

    // A password change travels too.
    laptop
        .diary
        .vault()
        .change_password("nueva", FAST_KDF)
        .unwrap();
    assert!(!laptop.sync(remote).password_changed);
    assert!(phone.sync(remote).password_changed);
    assert!(Vault::open(phone.dir.path(), "nueva").is_ok());
    assert!(Vault::open(phone.dir.path(), "pw").is_err());
    assert_eq!(phone.sync(remote), Report::default());
}

#[test]
fn folder_remote() {
    let remote = tempfile::tempdir().unwrap();
    two_devices_stay_in_step(&DirRemote::new(remote.path()));
}

#[test]
fn webdav_remote() {
    let url = fake_webdav_server("diego", "secreto");
    let remote = WebDav::new(&url, "diego", "secreto").unwrap();
    remote.prepare().unwrap();
    two_devices_stay_in_step(&remote);
}

#[test]
fn webdav_rejects_wrong_credentials() {
    let url = fake_webdav_server("diego", "secreto");
    let remote = WebDav::new(&url, "diego", "otra").unwrap();
    assert!(matches!(remote.prepare(), Err(SyncError::Unauthorized)));
}

#[test]
fn hostile_remote_cannot_inject_or_destroy() {
    let folder = tempfile::tempdir().unwrap();
    let remote = DirRemote::new(folder.path());
    let mut laptop = Device::first();
    let photo = laptop
        .diary
        .attach(&[1u8; 5000][..], "a.jpg", "image/jpeg")
        .unwrap();
    let mut with_photo = content("2026-10-09", "real");
    with_photo.attachments.push(photo);
    laptop.diary.create(with_photo).unwrap();
    laptop.sync(&remote);

    // The server plants files of its own and damages one of ours.
    let planted = "00112233445566778899aabbccddeeff";
    fs::write(
        folder.path().join("entries").join(planted),
        b"not a revision",
    )
    .unwrap();
    fs::write(folder.path().join("media").join(planted), b"not a photo").unwrap();
    let ours = fs::read_dir(folder.path().join("entries"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| !p.ends_with(planted))
        .unwrap();
    fs::write(&ours, b"damaged").unwrap();

    let mut phone = Device::join(&remote);
    let report = phone.sync(&remote);
    assert_eq!((report.downloaded, report.rejected), (1, 3));
    assert!(phone.bodies().is_empty(), "nothing forged is shown");

    // The laptop still has everything, and the remote's lies cost it nothing.
    let report = laptop.sync(&remote);
    assert_eq!((report.rejected, report.deleted), (2, 0));
    assert_eq!(laptop.bodies(), ["real"]);
    assert_eq!(laptop.media(), 1);

    // The server withholds the photo: the laptop puts it back, not delete it.
    for file in fs::read_dir(folder.path().join("media")).unwrap() {
        fs::remove_file(file.unwrap().path()).unwrap();
    }
    let report = laptop.sync(&remote);
    assert_eq!((report.uploaded, report.deleted), (1, 0));
    assert_eq!(laptop.media(), 1);
    assert_eq!(remote_files(folder.path(), ObjectKind::Media), 1);
}

#[test]
fn refuses_a_remote_holding_another_diary() {
    let folder = tempfile::tempdir().unwrap();
    let remote = DirRemote::new(folder.path());
    let mut mine = Device::first();
    let mut theirs = Device::first();
    theirs.diary.create(content("2026-10-09", "ajena")).unwrap();
    theirs.sync(&remote);

    mine.diary.create(content("2026-10-09", "mía")).unwrap();
    let result = sync(&mine.diary, &remote, &mut mine.state);
    assert!(matches!(result, Err(SyncError::DifferentVault)));
    assert_eq!(
        remote_files(folder.path(), ObjectKind::Entry),
        1,
        "their diary is untouched"
    );
}

// ------------------------------------------------------------------------
// A WebDAV server small enough to read: just the five methods the client uses.

fn fake_webdav_server(user: &'static str, password: &'static str) -> String {
    use base64::Engine;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/dav", listener.local_addr().unwrap());
    let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    );
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (files, expected) = (files.clone(), expected.clone());
            std::thread::spawn(move || serve(stream, &files, &expected));
        }
    });
    url
}

fn serve(stream: TcpStream, files: &Mutex<HashMap<String, Vec<u8>>>, expected: &str) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut stream = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap().to_owned(),
            parts.next().unwrap().to_owned(),
        );
        let (mut length, mut authorized) = (0usize, false);
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let Some((name, value)) = header.trim().split_once(':') else {
                break;
            };
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap(),
                "authorization" => authorized = value.trim() == expected,
                "transfer-encoding" => panic!("client must send a Content-Length"),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();

        let mut files = files.lock().unwrap();
        let (status, reply): (&str, Vec<u8>) = match method.as_str() {
            _ if !authorized => ("401 Unauthorized", Vec::new()),
            "MKCOL" => ("201 Created", Vec::new()),
            "PUT" => {
                files.insert(path, body);
                ("201 Created", Vec::new())
            }
            "GET" => match files.get(&path) {
                Some(data) => ("200 OK", data.clone()),
                None => ("404 Not Found", Vec::new()),
            },
            "DELETE" => match files.remove(&path) {
                Some(_) => ("204 No Content", Vec::new()),
                None => ("404 Not Found", Vec::new()),
            },
            "PROPFIND" => {
                let hrefs: String = std::iter::once(&path)
                    .chain(files.keys().filter(|file| file.starts_with(&path)))
                    .map(|href| format!("<D:response><D:href>{href}</D:href></D:response>"))
                    .collect();
                let xml = format!("<D:multistatus xmlns:D=\"DAV:\">{hrefs}</D:multistatus>");
                ("207 Multi-Status", xml.into_bytes())
            }
            _ => ("405 Method Not Allowed", Vec::new()),
        };
        drop(files);
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n",
            reply.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&reply).unwrap();
    }
}

/// Against a real server, e.g. the one in `server/docker-compose.yml`:
/// `SEALED_DIARY_TEST_WEBDAV=http://127.0.0.1:8080 SEALED_DIARY_TEST_USER=…
/// SEALED_DIARY_TEST_PASSWORD=… cargo test -p sealed-diary-sync -- --ignored`.
/// The folder it points at must be empty.
#[test]
#[ignore = "needs a running WebDAV server"]
fn real_webdav_server() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let remote = WebDav::new(
        &var("SEALED_DIARY_TEST_WEBDAV"),
        &var("SEALED_DIARY_TEST_USER"),
        &var("SEALED_DIARY_TEST_PASSWORD"),
    )
    .unwrap();
    remote.prepare().unwrap();
    two_devices_stay_in_step(&remote);
}
