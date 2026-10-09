use std::fs;
use std::io::Cursor;

use diario_core::{
    Error, KdfParams, MasterKey, ObjectId, ObjectReader, RecoveryKey, Vault, VaultHeader,
    decrypt_bytes, decrypt_object, encrypt_bytes, encrypt_object,
};

// Cheap parameters: these tests exercise the format, not Argon2's cost.
const FAST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    iterations: 1,
    parallelism: 1,
};
const CHUNK_LOG2: u8 = 12;
const CHUNK: usize = 1 << CHUNK_LOG2;
const SEALED: usize = CHUNK + 16;
const HEADER_LEN: usize = 25;

fn key() -> MasterKey {
    VaultHeader::create("pw", FAST_KDF).unwrap().1
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn seal(key: &MasterKey, id: &ObjectId, plaintext: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encrypt_object(key, id, CHUNK_LOG2, plaintext, &mut out).unwrap();
    out
}

fn open(key: &MasterKey, id: &ObjectId, ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    decrypt_object(key, id, ciphertext, &mut out)?;
    Ok(out)
}

#[test]
fn round_trips_at_chunk_boundaries() {
    let key = key();
    for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 5] {
        let id = ObjectId::random().unwrap();
        let plaintext = pattern(len);
        let ciphertext = seal(&key, &id, &plaintext);
        assert_eq!(open(&key, &id, &ciphertext).unwrap(), plaintext, "len {len}");
    }
}

#[test]
fn same_plaintext_encrypts_differently() {
    let key = key();
    let id = ObjectId::random().unwrap();
    assert_ne!(seal(&key, &id, b"hola"), seal(&key, &id, b"hola"));
}

#[test]
fn rejects_any_flipped_byte() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let ciphertext = seal(&key, &id, &pattern(CHUNK + 10));
    for position in [0, 4, 5, 10, HEADER_LEN, HEADER_LEN + SEALED, ciphertext.len() - 1] {
        let mut tampered = ciphertext.clone();
        tampered[position] ^= 1;
        assert!(open(&key, &id, &tampered).is_err(), "position {position}");
    }
}

#[test]
fn rejects_truncation_at_chunk_boundary() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let ciphertext = seal(&key, &id, &pattern(2 * CHUNK + 10));
    let truncated = &ciphertext[..HEADER_LEN + 2 * SEALED];
    assert!(matches!(open(&key, &id, truncated), Err(Error::Corrupted)));
    assert!(ObjectReader::open(&key, &id, Cursor::new(truncated)).is_err());
}

#[test]
fn rejects_reordered_chunks() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let mut ciphertext = seal(&key, &id, &pattern(3 * CHUNK));
    let (first, rest) = ciphertext[HEADER_LEN..].split_at_mut(SEALED);
    first.swap_with_slice(&mut rest[..SEALED]);
    assert!(matches!(open(&key, &id, &ciphertext), Err(Error::Corrupted)));
}

#[test]
fn rejects_object_served_under_another_id() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let other = ObjectId::random().unwrap();
    let ciphertext = seal(&key, &id, b"entrada");
    assert!(matches!(open(&key, &other, &ciphertext), Err(Error::Corrupted)));
}

#[test]
fn rejects_wrong_master_key() {
    let id = ObjectId::random().unwrap();
    let ciphertext = encrypt_bytes(&key(), &id, b"entrada").unwrap();
    assert!(decrypt_bytes(&key(), &id, &ciphertext).is_err());
}

#[test]
fn random_access_matches_plaintext() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let plaintext = pattern(3 * CHUNK + 100);
    let mut reader =
        ObjectReader::open(&key, &id, Cursor::new(seal(&key, &id, &plaintext))).unwrap();
    assert_eq!(reader.len(), plaintext.len() as u64);

    for offset in [0, 1, CHUNK - 1, CHUNK, 2 * CHUNK + 7, 3 * CHUNK + 99] {
        let mut buf = [0u8; 64];
        let n = reader.read_at(offset as u64, &mut buf).unwrap();
        assert!(n > 0, "offset {offset}");
        assert_eq!(&buf[..n], &plaintext[offset..offset + n], "offset {offset}");
    }
    let mut buf = [0u8; 8];
    assert_eq!(reader.read_at(plaintext.len() as u64, &mut buf).unwrap(), 0);
}

#[test]
fn random_access_on_empty_object() {
    let key = key();
    let id = ObjectId::random().unwrap();
    let reader = ObjectReader::open(&key, &id, Cursor::new(seal(&key, &id, b""))).unwrap();
    assert!(reader.is_empty());
}

#[test]
fn header_unlocks_with_password_and_recovery_key() {
    let (header, master, recovery) = VaultHeader::create("contraseña", FAST_KDF).unwrap();
    let header = VaultHeader::from_json(&header.to_json().unwrap()).unwrap();
    let id = ObjectId::random().unwrap();
    let ciphertext = encrypt_bytes(&master, &id, b"secreto").unwrap();

    let by_password = header.unlock("contraseña").unwrap();
    assert_eq!(&**decrypt_bytes(&by_password, &id, &ciphertext).unwrap(), b"secreto");

    let typed_back = RecoveryKey::parse(&recovery.to_display_string().to_uppercase()).unwrap();
    let by_recovery = header.unlock_with_recovery(&typed_back).unwrap();
    assert_eq!(&**decrypt_bytes(&by_recovery, &id, &ciphertext).unwrap(), b"secreto");

    assert!(matches!(header.unlock("contrasena"), Err(Error::WrongSecret)));
}

#[test]
fn password_is_unicode_normalized() {
    let (header, ..) = VaultHeader::create("ma\u{f1}ana", FAST_KDF).unwrap();
    assert!(header.unlock("man\u{303}ana").is_ok());
}

#[test]
fn edited_kdf_parameters_do_not_unlock() {
    let (header, ..) = VaultHeader::create("pw", FAST_KDF).unwrap();
    let json = String::from_utf8(header.to_json().unwrap()).unwrap();
    let edited = json.replace("\"iterations\": 1", "\"iterations\": 2");
    assert_ne!(json, edited);
    let header = VaultHeader::from_json(edited.as_bytes()).unwrap();
    assert!(matches!(header.unlock("pw"), Err(Error::WrongSecret)));
}

#[test]
fn oversized_kdf_parameters_are_refused() {
    let huge = KdfParams {
        memory_kib: u32::MAX,
        ..FAST_KDF
    };
    assert!(matches!(
        VaultHeader::create("pw", huge),
        Err(Error::InvalidParameter(_))
    ));
}

#[test]
fn vault_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let plaintext = pattern(200_000);

    let (vault, recovery) = Vault::create(dir.path(), "uno", FAST_KDF).unwrap();
    let id = vault.put(plaintext.as_slice()).unwrap();
    assert!(Vault::create(dir.path(), "uno", FAST_KDF).is_err());

    vault.change_password("dos", FAST_KDF).unwrap();
    drop(vault);
    assert!(matches!(Vault::open(dir.path(), "uno"), Err(Error::WrongSecret)));

    for vault in [
        Vault::open(dir.path(), "dos").unwrap(),
        Vault::open_with_recovery(dir.path(), &recovery).unwrap(),
    ] {
        let mut out = Vec::new();
        assert_eq!(vault.get(&id, &mut out).unwrap(), plaintext.len() as u64);
        assert_eq!(out, plaintext);
        assert_eq!(vault.reader(&id).unwrap().len(), plaintext.len() as u64);
    }

    // Nothing recognisable reaches the disk, and no temp files are left behind.
    let mut files = Vec::new();
    let mut pending = vec![dir.path().to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert_eq!(files.len(), 2);
    for file in &files {
        let stored = fs::read(file).unwrap();
        assert!(!stored.windows(64).any(|w| w == &plaintext[..64]));
    }

    let vault = Vault::open(dir.path(), "dos").unwrap();
    vault.delete(&id).unwrap();
    assert!(vault.get(&id, Vec::new()).is_err());
}
