//! Vault format and cryptography for the encrypted diary.
//!
//! Everything that leaves this crate towards disk or network is ciphertext.
//! The storage backend (local folder, Syncthing, WebDAV) is treated as hostile:
//! it may read, modify, reorder, truncate or swap files, and must learn nothing
//! beyond object count, sizes and access times.

mod error;
mod keys;
mod object;
mod vault;

pub use error::{Error, Result};
pub use keys::{KdfParams, MasterKey, RecoveryKey, VaultHeader};
pub use object::{
    DEFAULT_CHUNK_LOG2, ObjectId, ObjectReader, decrypt_bytes, decrypt_object, encrypt_bytes,
    encrypt_object,
};
pub use vault::Vault;
