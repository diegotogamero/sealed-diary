//! Tauri shell around `sealed-diary-core`.
//!
//! The webview never sees key material or file paths. It calls the commands
//! below, and loads media through the `media:` scheme, which decrypts the
//! requested byte range on the fly so a video can play and seek without a
//! plaintext copy ever touching disk.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::{Mutex, RwLock, RwLockReadGuard};

use sealed_diary_core::{
    Attachment, Diary, Entry, EntryContent, EntryId, Error, KdfParams, Listing, ObjectId,
    ObjectKind, RecoveryKey, Vault, VaultHeader,
};
use sealed_diary_sync::{Remote, RemotePath, Report, SyncError, SyncState, WebDav, sync};
use serde::{Deserialize, Serialize};
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use zeroize::Zeroizing;

const MIN_PASSWORD_CHARS: usize = 10;
const VAULT_DIR_ENV: &str = "SEALED_DIARY_VAULT";

/// Largest slice of a media file decrypted per request. The player simply
/// asks for the next range, so memory stays flat however long the video is.
const MAX_RANGE_LEN: u64 = 1 << 20;

struct AppState {
    root: PathBuf,
    /// Sync settings, sealed with the vault key so the server password is
    /// unreadable while the diary is locked.
    sync_file: PathBuf,
    /// Held for the length of a sync so two never run at once.
    syncing: Mutex<()>,
    /// `None` while locked. Dropping the diary wipes the master key.
    diary: RwLock<Option<Diary>>,
}

impl AppState {
    fn read(&self) -> RwLockReadGuard<'_, Option<Diary>> {
        self.diary.read().unwrap_or_else(|e| e.into_inner())
    }

    fn set(&self, diary: Option<Diary>) {
        *self.diary.write().unwrap_or_else(|e| e.into_inner()) = diary;
    }

    fn with<T>(&self, action: impl FnOnce(&Diary) -> Result<T, Error>) -> Result<T, String> {
        action(self.read().as_ref().ok_or("locked")?).map_err(code)
    }
}

/// Error codes the interface turns into messages.
fn code(error: Error) -> String {
    match error {
        Error::WrongSecret => "wrong-secret".into(),
        Error::Corrupted | Error::Header(_) => "corrupted".into(),
        Error::UnsupportedVersion(_) => "unsupported-version".into(),
        Error::InvalidParameter(what) => format!("invalid: {what}"),
        Error::Rng => "rng".into(),
        Error::Io(e) => format!("io: {e}"),
    }
}

fn sync_code(error: SyncError) -> String {
    match error {
        SyncError::Network(_) => "sync-network".into(),
        SyncError::Unauthorized => "sync-unauthorized".into(),
        SyncError::Status(status) => format!("sync-status: {status}"),
        SyncError::InvalidUrl(_) => "sync-url".into(),
        SyncError::DifferentVault => "different-vault".into(),
        SyncError::Vault(e) => code(e),
        SyncError::Io(e) => format!("io: {e}"),
    }
}

fn check_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err("weak-password".into());
    }
    Ok(())
}

#[derive(Serialize)]
struct Status {
    exists: bool,
    unlocked: bool,
}

#[tauri::command]
fn status(state: State<'_, AppState>) -> Status {
    Status {
        exists: Vault::exists(&state.root),
        unlocked: state.read().is_some(),
    }
}

/// Creates the vault and returns the recovery key for the user to write down.
#[tauri::command(async)]
fn create_vault(state: State<'_, AppState>, password: String) -> Result<String, String> {
    let password = Zeroizing::new(password);
    check_password(&password)?;
    let (vault, recovery) =
        Vault::create(&state.root, &password, KdfParams::DEFAULT).map_err(code)?;
    state.set(Some(Diary::new(vault)));
    Ok(recovery.to_display_string().to_string())
}

#[tauri::command(async)]
fn unlock(state: State<'_, AppState>, password: String) -> Result<(), String> {
    let password = Zeroizing::new(password);
    let vault = Vault::open(&state.root, &password).map_err(code)?;
    state.set(Some(Diary::new(vault)));
    Ok(())
}

/// Unlocks with the recovery key and sets a new password in one step.
#[tauri::command(async)]
fn recover(
    state: State<'_, AppState>,
    recovery_key: String,
    new_password: String,
) -> Result<(), String> {
    let recovery_key = Zeroizing::new(recovery_key);
    let new_password = Zeroizing::new(new_password);
    check_password(&new_password)?;
    let recovery = RecoveryKey::parse(&recovery_key).map_err(|_| "wrong-secret")?;
    let vault = Vault::open_with_recovery(&state.root, &recovery).map_err(code)?;
    vault
        .change_password(&new_password, KdfParams::DEFAULT)
        .map_err(code)?;
    state.set(Some(Diary::new(vault)));
    Ok(())
}

#[tauri::command]
fn lock(state: State<'_, AppState>) {
    state.set(None);
}

#[tauri::command(async)]
fn list_entries(state: State<'_, AppState>) -> Result<Listing, String> {
    state.with(Diary::list)
}

#[derive(Deserialize)]
struct SaveRequest {
    /// Absent for a new entry.
    id: Option<EntryId>,
    replaces: Vec<ObjectId>,
    content: EntryContent,
}

#[tauri::command(async)]
fn save_entry(state: State<'_, AppState>, request: SaveRequest) -> Result<Entry, String> {
    state.with(|diary| match request.id {
        Some(id) => diary.update(&id, &request.replaces, request.content),
        None => diary.create(request.content),
    })
}

#[tauri::command(async)]
fn delete_entry(
    state: State<'_, AppState>,
    id: EntryId,
    replaces: Vec<ObjectId>,
) -> Result<(), String> {
    state.with(|diary| diary.delete(&id, &replaces))
}

/// Lets the user pick files and encrypts them into the vault. The picker runs
/// here, not in the webview, so page content can never name a path to read.
#[tauri::command(async)]
fn attach_files(app: AppHandle, state: State<'_, AppState>) -> Result<Vec<Attachment>, String> {
    if state.read().is_none() {
        return Err("locked".into());
    }
    let picked = app
        .dialog()
        .file()
        .blocking_pick_files()
        .unwrap_or_default();
    let mut attachments = Vec::new();
    for file in picked {
        let path = file.into_path().map_err(|e| format!("io: {e}"))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source = BufReader::new(File::open(&path).map_err(|e| format!("io: {e}"))?);
        attachments.push(state.with(|diary| diary.attach(source, &name, mime_for(&name)))?);
    }
    Ok(attachments)
}

fn mime_for(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, e)| e);
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SyncSettings {
    url: String,
    username: String,
    password: String,
    state: SyncState,
}

impl SyncSettings {
    fn remote(&self) -> Result<WebDav, String> {
        WebDav::new(&self.url, &self.username, &self.password).map_err(sync_code)
    }

    fn load(state: &AppState, diary: &Diary) -> Result<Option<Self>, String> {
        let sealed = match std::fs::read(&state.sync_file) {
            Ok(sealed) => sealed,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("io: {e}")),
        };
        let json = diary.vault().open_local(&sealed).map_err(code)?;
        serde_json::from_slice(&json)
            .map(Some)
            .map_err(|_| "corrupted".to_owned())
    }

    fn store(&self, state: &AppState, diary: &Diary) -> Result<(), String> {
        let json = Zeroizing::new(serde_json::to_vec(self).map_err(|e| format!("io: {e}"))?);
        let sealed = diary.vault().seal_local(&json).map_err(code)?;
        if let Some(dir) = state.sync_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("io: {e}"))?;
        }
        std::fs::write(&state.sync_file, sealed).map_err(|e| format!("io: {e}"))
    }

    /// Syncs and remembers the outcome, so the next run knows what changed.
    fn run(&mut self, state: &AppState, diary: &Diary) -> Result<Report, String> {
        let remote = self.remote()?;
        let _one_at_a_time = state.syncing.lock().unwrap_or_else(|e| e.into_inner());
        let report = sync(diary, &remote, &mut self.state).map_err(sync_code)?;
        self.store(state, diary)?;
        Ok(report)
    }
}

#[derive(Serialize)]
struct SyncInfo {
    url: String,
    username: String,
}

/// Where this diary syncs to, if anywhere. Never returns the password.
#[tauri::command(async)]
fn sync_info(state: State<'_, AppState>) -> Result<Option<SyncInfo>, String> {
    let guard = state.read();
    let diary = guard.as_ref().ok_or("locked")?;
    Ok(SyncSettings::load(&state, diary)?.map(|s| SyncInfo {
        url: s.url,
        username: s.username,
    }))
}

/// Sets the server up and runs a first sync; nothing is saved unless it works.
#[tauri::command(async)]
fn configure_sync(
    state: State<'_, AppState>,
    url: String,
    username: String,
    password: String,
) -> Result<Report, String> {
    let guard = state.read();
    let diary = guard.as_ref().ok_or("locked")?;
    let mut settings = SyncSettings {
        url: url.trim().to_owned(),
        username,
        password,
        state: SyncState::default(),
    };
    settings.remote()?.prepare().map_err(sync_code)?;
    settings.run(&state, diary)
}

#[tauri::command(async)]
fn disable_sync(state: State<'_, AppState>) -> Result<(), String> {
    match std::fs::remove_file(&state.sync_file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("io: {e}")),
        _ => Ok(()),
    }
}

/// `None` when sync is not set up.
#[tauri::command(async)]
fn sync_now(state: State<'_, AppState>) -> Result<Option<Report>, String> {
    let guard = state.read();
    let diary = guard.as_ref().ok_or("locked")?;
    match SyncSettings::load(&state, diary)? {
        Some(mut settings) => settings.run(&state, diary).map(Some),
        None => Ok(None),
    }
}

/// Sets this device up from a diary that already lives on the user's server.
#[tauri::command(async)]
fn join_vault(
    state: State<'_, AppState>,
    url: String,
    username: String,
    password: String,
    diary_password: String,
) -> Result<Report, String> {
    let diary_password = Zeroizing::new(diary_password);
    let mut settings = SyncSettings {
        url: url.trim().to_owned(),
        username,
        password,
        state: SyncState::default(),
    };
    let remote = settings.remote()?;
    let mut header = Vec::new();
    remote
        .download(&RemotePath::Header)
        .map_err(sync_code)?
        .ok_or("no-remote-vault")?
        .take(1 << 20)
        .read_to_end(&mut header)
        .map_err(|e| format!("io: {e}"))?;
    // Checked against the password before anything is written, so a wrong
    // password or a forged header leaves this device untouched.
    VaultHeader::from_json(&header)
        .and_then(|h| h.unlock(&diary_password))
        .map_err(code)?;
    Vault::install_header(&state.root, &header).map_err(code)?;
    let diary = Diary::new(Vault::open(&state.root, &diary_password).map_err(code)?);
    // Settings are kept even if this first sync is cut short: the diary is
    // set up, and the next sync carries on from wherever this one stopped.
    let report = settings
        .store(&state, &diary)
        .and_then(|()| settings.run(&state, &diary));
    state.set(Some(diary));
    report
}

/// `bytes=a-b`, `bytes=a-` or `bytes=-n` as an inclusive range within `len`.
fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let last = len.checked_sub(1)?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        return (suffix > 0).then(|| (len.saturating_sub(suffix), last));
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        last
    } else {
        end.parse::<u64>().ok()?.min(last)
    };
    (start <= end).then_some((start, end))
}

/// Serves `media://localhost/<object id>/<mime type>`.
fn serve_media(
    state: &AppState,
    request: &Request<Vec<u8>>,
) -> Result<Response<Vec<u8>>, StatusCode> {
    let path = request.uri().path().trim_start_matches('/');
    let (id, mime) = path.split_once('/').unwrap_or((path, ""));
    let id = ObjectId::from_hex(id).map_err(|_| StatusCode::BAD_REQUEST)?;
    // Only ever answer as inert media, whatever the URL asks for.
    let is_media = ["image/", "audio/", "video/"]
        .iter()
        .any(|prefix| mime.starts_with(prefix))
        && mime
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-.".contains(&b));
    let mime = if is_media {
        mime
    } else {
        "application/octet-stream"
    };

    let guard = state.read();
    let diary = guard.as_ref().ok_or(StatusCode::FORBIDDEN)?;
    let mut reader = diary
        .vault()
        .reader(ObjectKind::Media, &id)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let len = reader.len();

    let range = match request.headers().get(header::RANGE) {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|v| parse_range(v, len))
                .ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?,
        ),
        None => None,
    };
    let (start, body_len) = match range {
        Some((start, end)) => (start, (end - start + 1).min(MAX_RANGE_LEN)),
        None => (0, len),
    };

    let mut body = vec![0u8; body_len as usize];
    let mut filled = 0;
    while filled < body.len() {
        let n = reader
            .read_at(start + filled as u64, &mut body[filled..])
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if n == 0 {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        filled += n;
    }

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        );
    if range.is_some() {
        let end = start + body_len - 1;
        response = response
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    response
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Point this at a Syncthing or WebDAV-mounted folder to sync.
            let data_dir = app.path().app_data_dir()?;
            let root = match std::env::var_os(VAULT_DIR_ENV) {
                Some(dir) => PathBuf::from(dir),
                None => data_dir.join("vault"),
            };
            app.manage(AppState {
                root,
                sync_file: data_dir.join("sync.bin"),
                syncing: Mutex::new(()),
                diary: RwLock::new(None),
            });
            Ok(())
        })
        .register_uri_scheme_protocol("media", |context, request| {
            let state = context.app_handle().state::<AppState>();
            serve_media(&state, &request).unwrap_or_else(|status| {
                let mut response = Response::new(Vec::new());
                *response.status_mut() = status;
                response
            })
        })
        .invoke_handler(tauri::generate_handler![
            status,
            create_vault,
            unlock,
            recover,
            lock,
            list_entries,
            save_entry,
            delete_entry,
            attach_files,
            sync_info,
            configure_sync,
            disable_sync,
            sync_now,
            join_vault,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Sealed Diary");
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST_KDF: KdfParams = KdfParams {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    };

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-500", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-500", 100), Some((0, 99)));
        for bad in [
            "bytes=100-",
            "bytes=5-2",
            "bytes=-0",
            "bytes=0-1,5-6",
            "items=0-1",
            "bytes=a-",
        ] {
            assert_eq!(parse_range(bad, 100), None, "{bad}");
        }
        assert_eq!(parse_range("bytes=0-", 0), None);
    }

    fn get(
        state: &AppState,
        path: &str,
        range: Option<&str>,
    ) -> Result<Response<Vec<u8>>, StatusCode> {
        let mut request = Request::builder().uri(format!("media://localhost/{path}"));
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        serve_media(state, &request.body(Vec::new()).unwrap())
    }

    #[test]
    fn media_is_served_in_ranges_only_while_unlocked() {
        let dir = std::env::temp_dir().join(format!("sealed-diary-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (vault, _) = Vault::create(&dir, "una contraseña", FAST_KDF).unwrap();
        let diary = Diary::new(vault);
        let video: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let attachment = diary
            .attach(video.as_slice(), "v.mp4", "video/mp4")
            .unwrap();
        let id = attachment.object.to_hex();
        let state = AppState {
            root: dir.clone(),
            sync_file: dir.join("sync.bin"),
            syncing: Mutex::new(()),
            diary: RwLock::new(Some(diary)),
        };

        let whole = get(&state, &format!("{id}/video/mp4"), None).unwrap();
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(whole.headers()[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(whole.body(), &video);

        let part = get(
            &state,
            &format!("{id}/video/mp4"),
            Some("bytes=70000-70009"),
        )
        .unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            part.headers()[header::CONTENT_RANGE],
            "bytes 70000-70009/3000000"
        );
        assert_eq!(part.body(), &video[70000..70010]);

        // An open-ended request is answered with a bounded slice.
        let open = get(&state, &format!("{id}/video/mp4"), Some("bytes=2000000-")).unwrap();
        assert_eq!(
            open.headers()[header::CONTENT_RANGE],
            "bytes 2000000-2999999/3000000"
        );
        let open = get(&state, &format!("{id}/video/mp4"), Some("bytes=0-")).unwrap();
        assert_eq!(open.body().len() as u64, MAX_RANGE_LEN);
        assert_eq!(open.body(), &video[..MAX_RANGE_LEN as usize]);

        let html = get(&state, &format!("{id}/text/html"), None).unwrap();
        assert_eq!(
            html.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );

        assert_eq!(
            get(&state, &format!("{id}/video/mp4"), Some("bytes=3000000-")).unwrap_err(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
        let missing = ObjectId::random().unwrap().to_hex();
        assert_eq!(
            get(&state, &missing, None).unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&state, "../vault.json", None).unwrap_err(),
            StatusCode::BAD_REQUEST
        );

        state.set(None);
        assert_eq!(get(&state, &id, None).unwrap_err(), StatusCode::FORBIDDEN);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Run with the same variables as the sync crate's `real_webdav_server`.
    #[test]
    #[ignore = "needs a running WebDAV server"]
    fn sync_settings_survive_sealed_on_disk() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let dir = std::env::temp_dir().join(format!("sealed-diary-sync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (vault, _) = Vault::create(dir.join("vault"), "una contraseña", FAST_KDF).unwrap();
        let diary = Diary::new(vault);
        let state = AppState {
            root: dir.join("vault"),
            sync_file: dir.join("sync.bin"),
            syncing: Mutex::new(()),
            diary: RwLock::new(None),
        };
        assert!(SyncSettings::load(&state, &diary).unwrap().is_none());

        let content = EntryContent {
            date: "2026-10-09".into(),
            body: "hola".into(),
            ..Default::default()
        };
        diary.create(content).unwrap();
        let mut settings = SyncSettings {
            url: var("SEALED_DIARY_TEST_WEBDAV"),
            username: var("SEALED_DIARY_TEST_USER"),
            password: var("SEALED_DIARY_TEST_PASSWORD"),
            state: SyncState::default(),
        };
        settings.remote().unwrap().prepare().unwrap();
        assert_eq!(settings.run(&state, &diary).unwrap().uploaded, 1);

        let on_disk = std::fs::read(&state.sync_file).unwrap();
        let secret = settings.password.as_bytes();
        assert!(!on_disk.windows(secret.len()).any(|w| w == secret));
        let mut reloaded = SyncSettings::load(&state, &diary).unwrap().unwrap();
        assert_eq!(reloaded.password, settings.password);
        assert_eq!(reloaded.run(&state, &diary).unwrap(), Report::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn short_passwords_are_refused() {
        assert!(check_password("123456789").is_err());
        assert!(check_password("contraseña").is_ok());
    }
}
