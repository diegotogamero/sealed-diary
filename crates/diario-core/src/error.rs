pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Also returned when the key slot was tampered with: the two cases are
    /// indistinguishable by design.
    #[error("wrong password or recovery key")]
    WrongSecret,
    #[error("data is corrupted or has been tampered with")]
    Corrupted,
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid parameter: {0}")]
    InvalidParameter(&'static str),
    #[error("system random number generator failed")]
    Rng,
    #[error("malformed vault header: {0}")]
    Header(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
