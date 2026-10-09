//! Sync of a vault with storage the user runs themselves.
//!
//! The remote is a dumb file store and is not trusted. It only ever holds
//! ciphertext, everything fetched from it is authenticated before it enters
//! the vault, and it is never asked to decide anything: which revision wins,
//! what may be deleted and whether a header is genuine are all worked out on
//! the device, with the key.

mod engine;
mod remote;
mod webdav;

pub use engine::{Report, SyncState, sync};
pub use remote::{DirRemote, Remote, RemotePath};
pub use webdav::WebDav;

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("could not reach the server: {0}")]
    Network(String),
    #[error("the server refused the user name or password")]
    Unauthorized,
    #[error("the server answered with status {0}")]
    Status(u16),
    #[error("invalid sync address: {0}")]
    InvalidUrl(&'static str),
    #[error("the remote holds a different diary")]
    DifferentVault,
    #[error(transparent)]
    Vault(#[from] sealed_diary_core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
