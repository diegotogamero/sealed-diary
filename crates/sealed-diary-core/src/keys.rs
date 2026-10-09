//! Key hierarchy.
//!
//! ```text
//! password ──Argon2id──▶ KEK ──unwraps──┐
//!                                       ├──▶ master key ──HKDF──▶ per-object keys
//! recovery key ──HKDF──▶ KEK ──unwraps──┘
//! ```
//!
//! The master key is random and never changes, so changing the password only
//! rewrites the header, never the objects.

use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

pub(crate) const KEY_LEN: usize = 32;
const HEADER_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const VAULT_ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;

// Upper bounds on parameters read from an untrusted header, so a hostile
// storage backend cannot make unlocking exhaust memory or run forever.
const MAX_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_ITERATIONS: u32 = 64;
const MAX_PARALLELISM: u32 = 16;

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| Error::Rng)?;
    Ok(bytes)
}

pub struct MasterKey(Zeroizing<[u8; KEY_LEN]>);

impl MasterKey {
    fn generate() -> Result<Self> {
        Ok(Self(Zeroizing::new(random_bytes()?)))
    }

    pub(crate) fn derive(&self, info: &[&[u8]]) -> Zeroizing<[u8; KEY_LEN]> {
        hkdf_expand(self.0.as_ref(), info)
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

/// Random 256-bit key shown to the user once at vault creation. It is the only
/// way back in if the password is forgotten.
pub struct RecoveryKey(Zeroizing<[u8; KEY_LEN]>);

impl RecoveryKey {
    /// Hex in dash-separated groups of four, e.g. `3fa9-01bc-…`.
    pub fn to_display_string(&self) -> Zeroizing<String> {
        let hex = Zeroizing::new(hex::encode(self.0.as_ref()));
        let mut out = Zeroizing::new(String::with_capacity(hex.len() + hex.len() / 4));
        for (i, c) in hex.chars().enumerate() {
            if i > 0 && i % 4 == 0 {
                out.push('-');
            }
            out.push(c);
        }
        out
    }

    /// Accepts the display form in any case, with or without dashes and spaces.
    pub fn parse(text: &str) -> Result<Self> {
        let cleaned: Zeroizing<String> = Zeroizing::new(
            text.chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .collect(),
        );
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        hex::decode_to_slice(cleaned.as_bytes(), key.as_mut())
            .map_err(|_| Error::InvalidParameter("recovery key"))?;
        Ok(Self(key))
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// 128 MiB, 3 passes: affordable on a low-end phone, expensive to brute-force.
    pub const DEFAULT: Self = Self {
        memory_kib: 128 * 1024,
        iterations: 3,
        parallelism: 1,
    };

    fn to_argon2(self) -> Result<Params> {
        if self.memory_kib > MAX_MEMORY_KIB
            || self.iterations > MAX_ITERATIONS
            || self.parallelism > MAX_PARALLELISM
        {
            return Err(Error::InvalidParameter("kdf parameters out of range"));
        }
        Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(KEY_LEN),
        )
        .map_err(|_| Error::InvalidParameter("kdf parameters"))
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KeySlot {
    #[serde(with = "hex_serde")]
    nonce: Vec<u8>,
    #[serde(with = "hex_serde")]
    ciphertext: Vec<u8>,
}

/// Contents of `vault.json`. Stored in the clear; holds no secrets, only what
/// is needed to turn a password or recovery key into the master key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultHeader {
    version: u32,
    #[serde(with = "hex_serde")]
    vault_id: Vec<u8>,
    kdf: KdfParams,
    #[serde(with = "hex_serde")]
    salt: Vec<u8>,
    password_slot: KeySlot,
    recovery_slot: KeySlot,
}

impl VaultHeader {
    pub fn create(password: &str, kdf: KdfParams) -> Result<(Self, MasterKey, RecoveryKey)> {
        let master = MasterKey::generate()?;
        let recovery = RecoveryKey(Zeroizing::new(random_bytes()?));
        let vault_id = random_bytes::<VAULT_ID_LEN>()?.to_vec();
        let salt = random_bytes::<SALT_LEN>()?.to_vec();

        let password_kek = password_kek(password, &salt, kdf)?;
        let password_slot = wrap(&password_kek, &password_aad(&vault_id, &salt, kdf), &master)?;
        let recovery_slot = wrap(
            &recovery_kek(&recovery, &vault_id),
            &recovery_aad(&vault_id),
            &master,
        )?;

        let header = Self {
            version: HEADER_VERSION,
            vault_id,
            kdf,
            salt,
            password_slot,
            recovery_slot,
        };
        Ok((header, master, recovery))
    }

    pub fn from_json(json: &[u8]) -> Result<Self> {
        let header: Self = serde_json::from_slice(json)?;
        if header.version != HEADER_VERSION {
            return Err(Error::UnsupportedVersion(header.version));
        }
        if header.vault_id.len() != VAULT_ID_LEN || header.salt.len() != SALT_LEN {
            return Err(Error::Corrupted);
        }
        Ok(header)
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn unlock(&self, password: &str) -> Result<MasterKey> {
        let kek = password_kek(password, &self.salt, self.kdf)?;
        unwrap(
            &kek,
            &password_aad(&self.vault_id, &self.salt, self.kdf),
            &self.password_slot,
        )
    }

    pub fn unlock_with_recovery(&self, recovery: &RecoveryKey) -> Result<MasterKey> {
        unwrap(
            &recovery_kek(recovery, &self.vault_id),
            &recovery_aad(&self.vault_id),
            &self.recovery_slot,
        )
    }

    /// Replaces the password slot. Requires an unlocked vault; the recovery
    /// key and all objects stay valid.
    pub fn change_password(
        &mut self,
        master: &MasterKey,
        new_password: &str,
        kdf: KdfParams,
    ) -> Result<()> {
        let salt = random_bytes::<SALT_LEN>()?.to_vec();
        let kek = password_kek(new_password, &salt, kdf)?;
        self.password_slot = wrap(&kek, &password_aad(&self.vault_id, &salt, kdf), master)?;
        self.salt = salt;
        self.kdf = kdf;
        Ok(())
    }
}

fn hkdf_expand(ikm: &[u8], info: &[&[u8]]) -> Zeroizing<[u8; KEY_LEN]> {
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    Hkdf::<Sha256>::new(None, ikm)
        .expand_multi_info(info, out.as_mut())
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

fn password_kek(password: &str, salt: &[u8], kdf: KdfParams) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    // NFKC so that "ñ" typed on Android, iOS and desktop keyboards yields the
    // same bytes regardless of how each platform composes it.
    let normalized: Zeroizing<String> = Zeroizing::new(password.nfkc().collect());
    let mut kek = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, kdf.to_argon2()?)
        .hash_password_into(normalized.as_bytes(), salt, kek.as_mut())
        .map_err(|_| Error::InvalidParameter("kdf parameters"))?;
    Ok(kek)
}

fn recovery_kek(recovery: &RecoveryKey, vault_id: &[u8]) -> Zeroizing<[u8; KEY_LEN]> {
    hkdf_expand(
        recovery.0.as_ref(),
        &[b"sealed-diary/v1/recovery", vault_id],
    )
}

// Binding the KDF parameters and salt into the slot means a header edited on
// the storage side simply fails to unlock.
fn password_aad(vault_id: &[u8], salt: &[u8], kdf: KdfParams) -> Vec<u8> {
    let mut aad = b"sealed-diary/v1/slot/password".to_vec();
    aad.extend_from_slice(vault_id);
    aad.extend_from_slice(salt);
    aad.extend_from_slice(&kdf.memory_kib.to_le_bytes());
    aad.extend_from_slice(&kdf.iterations.to_le_bytes());
    aad.extend_from_slice(&kdf.parallelism.to_le_bytes());
    aad
}

fn recovery_aad(vault_id: &[u8]) -> Vec<u8> {
    let mut aad = b"sealed-diary/v1/slot/recovery".to_vec();
    aad.extend_from_slice(vault_id);
    aad
}

fn wrap(kek: &[u8; KEY_LEN], aad: &[u8], master: &MasterKey) -> Result<KeySlot> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let ciphertext = XChaCha20Poly1305::new(kek.into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: master.0.as_ref(),
                aad,
            },
        )
        .map_err(|_| Error::InvalidParameter("key wrap"))?;
    Ok(KeySlot {
        nonce: nonce.to_vec(),
        ciphertext,
    })
}

fn unwrap(kek: &[u8; KEY_LEN], aad: &[u8], slot: &KeySlot) -> Result<MasterKey> {
    let nonce: [u8; NONCE_LEN] = slot
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| Error::Corrupted)?;
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new(kek.into())
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &slot.ciphertext,
                    aad,
                },
            )
            .map_err(|_| Error::WrongSecret)?,
    );
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    if plaintext.len() != KEY_LEN {
        return Err(Error::Corrupted);
    }
    key.copy_from_slice(&plaintext);
    Ok(MasterKey(key))
}

mod hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        hex::decode(text).map_err(serde::de::Error::custom)
    }
}
