//! Encrypted objects: diary entries, photos, songs, videos.
//!
//! ```text
//! "DIAR" | version (1) | chunk_log2 (1) | nonce prefix (19) | chunk 0 | chunk 1 | …
//! ```
//!
//! The plaintext is split into fixed-size chunks, each sealed on its own with
//! XChaCha20-Poly1305. The nonce of a chunk is `prefix | index (u32 BE) | last`,
//! so chunks cannot be reordered, dropped or duplicated, and cutting the file
//! short is detected because the surviving tail was not sealed as `last`.
//! Independent chunks are what let a video seek without decrypting from the
//! start, and keep memory use flat for files of any size.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::keys::{MasterKey, random_bytes};

const MAGIC: [u8; 4] = *b"DIAR";
const FORMAT_VERSION: u8 = 1;
const PREFIX_LEN: usize = 19;
const HEADER_LEN: usize = MAGIC.len() + 2 + PREFIX_LEN;
const TAG_LEN: usize = 16;
const ID_LEN: usize = 16;

/// 64 KiB: small enough to seek and stream on a phone, large enough that the
/// 16-byte tag per chunk is negligible overhead.
pub const DEFAULT_CHUNK_LOG2: u8 = 16;
const MIN_CHUNK_LOG2: u8 = 12;
const MAX_CHUNK_LOG2: u8 = 22;

/// Random name of an object in storage. Deliberately unrelated to the content,
/// so the backend cannot tell whether two vaults hold the same file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId([u8; ID_LEN]);

impl ObjectId {
    pub fn random() -> Result<Self> {
        Ok(Self(random_bytes()?))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(text: &str) -> Result<Self> {
        let mut id = [0u8; ID_LEN];
        hex::decode_to_slice(text, &mut id).map_err(|_| Error::InvalidParameter("object id"))?;
        Ok(Self(id))
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

struct ChunkCipher {
    cipher: XChaCha20Poly1305,
    prefix: [u8; PREFIX_LEN],
    // Object id plus header: a chunk only opens under the id it was stored
    // with, so the backend cannot serve one object in place of another.
    aad: [u8; ID_LEN + HEADER_LEN],
}

impl ChunkCipher {
    fn new(key: &MasterKey, id: &ObjectId, header: &[u8; HEADER_LEN]) -> Self {
        let object_key = key.derive(&[b"diario/v1/object", &id.0]);
        let mut prefix = [0u8; PREFIX_LEN];
        prefix.copy_from_slice(&header[HEADER_LEN - PREFIX_LEN..]);
        let mut aad = [0u8; ID_LEN + HEADER_LEN];
        aad[..ID_LEN].copy_from_slice(&id.0);
        aad[ID_LEN..].copy_from_slice(header);
        Self {
            cipher: XChaCha20Poly1305::new((&*object_key).into()),
            prefix,
            aad,
        }
    }

    fn nonce(&self, index: u32, last: bool) -> XNonce {
        let mut nonce = [0u8; PREFIX_LEN + 5];
        nonce[..PREFIX_LEN].copy_from_slice(&self.prefix);
        nonce[PREFIX_LEN..PREFIX_LEN + 4].copy_from_slice(&index.to_be_bytes());
        nonce[PREFIX_LEN + 4] = last as u8;
        XNonce::from(nonce)
    }

    fn seal(&self, index: u32, last: bool, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.cipher
            .encrypt(
                &self.nonce(index, last),
                Payload {
                    msg: plaintext,
                    aad: &self.aad,
                },
            )
            .map_err(|_| Error::InvalidParameter("chunk too large"))
    }

    fn open(&self, index: u32, last: bool, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        self.cipher
            .decrypt(
                &self.nonce(index, last),
                Payload {
                    msg: ciphertext,
                    aad: &self.aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::Corrupted)
    }
}

fn build_header(chunk_log2: u8, prefix: &[u8; PREFIX_LEN]) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = FORMAT_VERSION;
    header[5] = chunk_log2;
    header[6..].copy_from_slice(prefix);
    header
}

/// Returns the chunk size in bytes.
fn parse_header(header: &[u8; HEADER_LEN]) -> Result<usize> {
    if header[..4] != MAGIC {
        return Err(Error::Corrupted);
    }
    if header[4] != FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(header[4].into()));
    }
    if !(MIN_CHUNK_LOG2..=MAX_CHUNK_LOG2).contains(&header[5]) {
        return Err(Error::Corrupted);
    }
    Ok(1 << header[5])
}

fn read_header<R: Read>(reader: &mut R) -> Result<[u8; HEADER_LEN]> {
    let mut header = [0u8; HEADER_LEN];
    if read_full(reader, &mut header)? != HEADER_LEN {
        return Err(Error::Corrupted);
    }
    Ok(header)
}

/// Like `read_exact`, but a short read at end of input is not an error.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Encrypts `reader` into `writer`, returning the number of plaintext bytes.
pub fn encrypt_object<R: Read, W: Write>(
    key: &MasterKey,
    id: &ObjectId,
    chunk_log2: u8,
    mut reader: R,
    mut writer: W,
) -> Result<u64> {
    if !(MIN_CHUNK_LOG2..=MAX_CHUNK_LOG2).contains(&chunk_log2) {
        return Err(Error::InvalidParameter("chunk size"));
    }
    let chunk = 1usize << chunk_log2;
    let header = build_header(chunk_log2, &random_bytes()?);
    let cipher = ChunkCipher::new(key, id, &header);
    writer.write_all(&header)?;

    // One chunk of read-ahead: a chunk is the last one only if nothing follows.
    let mut current = Zeroizing::new(vec![0u8; chunk]);
    let mut next = Zeroizing::new(vec![0u8; chunk]);
    let mut current_len = read_full(&mut reader, &mut current)?;
    let mut index = 0u32;
    let mut total = 0u64;
    loop {
        let next_len = if current_len == chunk {
            read_full(&mut reader, &mut next)?
        } else {
            0
        };
        let last = next_len == 0;
        writer.write_all(&cipher.seal(index, last, &current[..current_len])?)?;
        total += current_len as u64;
        if last {
            break;
        }
        index = index
            .checked_add(1)
            .ok_or(Error::InvalidParameter("object too large"))?;
        std::mem::swap(&mut current, &mut next);
        current_len = next_len;
    }
    writer.flush()?;
    Ok(total)
}

/// Decrypts `reader` into `writer`, returning the number of plaintext bytes.
///
/// Chunks are written as they are verified, so on error `writer` may already
/// hold a genuine prefix of the plaintext. Callers must discard it.
pub fn decrypt_object<R: Read, W: Write>(
    key: &MasterKey,
    id: &ObjectId,
    mut reader: R,
    mut writer: W,
) -> Result<u64> {
    let header = read_header(&mut reader)?;
    let sealed = parse_header(&header)? + TAG_LEN;
    let cipher = ChunkCipher::new(key, id, &header);

    let mut current = vec![0u8; sealed];
    let mut next = vec![0u8; sealed];
    let mut current_len = read_full(&mut reader, &mut current)?;
    let mut index = 0u32;
    let mut total = 0u64;
    loop {
        let next_len = if current_len == sealed {
            read_full(&mut reader, &mut next)?
        } else {
            0
        };
        let last = next_len == 0;
        let plaintext = cipher.open(index, last, &current[..current_len])?;
        writer.write_all(&plaintext)?;
        total += plaintext.len() as u64;
        if last {
            break;
        }
        index = index.checked_add(1).ok_or(Error::Corrupted)?;
        std::mem::swap(&mut current, &mut next);
        current_len = next_len;
    }
    writer.flush()?;
    Ok(total)
}

pub fn encrypt_bytes(key: &MasterKey, id: &ObjectId, plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(plaintext.len() + HEADER_LEN + TAG_LEN);
    encrypt_object(key, id, DEFAULT_CHUNK_LOG2, plaintext, &mut out)?;
    Ok(out)
}

pub fn decrypt_bytes(
    key: &MasterKey,
    id: &ObjectId,
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    decrypt_object(key, id, ciphertext, &mut *out)?;
    Ok(out)
}

/// Random access into an encrypted object, for media playback.
pub struct ObjectReader<R> {
    inner: R,
    cipher: ChunkCipher,
    chunk: usize,
    chunks: u32,
    last_sealed_len: usize,
    len: u64,
    cached: Option<(u32, Zeroizing<Vec<u8>>)>,
}

impl<R: Read + Seek> ObjectReader<R> {
    pub fn open(key: &MasterKey, id: &ObjectId, mut inner: R) -> Result<Self> {
        let file_len = inner.seek(SeekFrom::End(0))?;
        inner.seek(SeekFrom::Start(0))?;
        let header = read_header(&mut inner)?;
        let chunk = parse_header(&header)?;

        let sealed = (chunk + TAG_LEN) as u64;
        let body = file_len - HEADER_LEN as u64;
        if body < TAG_LEN as u64 {
            return Err(Error::Corrupted);
        }
        let chunks = body.div_ceil(sealed);
        let last_sealed_len = body - (chunks - 1) * sealed;
        if last_sealed_len < TAG_LEN as u64 {
            return Err(Error::Corrupted);
        }

        let mut reader = Self {
            inner,
            cipher: ChunkCipher::new(key, id, &header),
            chunk,
            chunks: u32::try_from(chunks).map_err(|_| Error::Corrupted)?,
            last_sealed_len: last_sealed_len as usize,
            len: (chunks - 1) * chunk as u64 + last_sealed_len - TAG_LEN as u64,
            cached: None,
        };
        // The length above comes from the file size, which the backend
        // controls. Opening the final chunk proves the file was not cut short.
        reader.load(reader.chunks - 1)?;
        Ok(reader)
    }

    /// Plaintext length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads up to `buf.len()` bytes at `offset`, stopping at a chunk boundary.
    /// Returns 0 at or past the end.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let chunk = self.chunk as u64;
        let data = self.load((offset / chunk) as u32)?;
        let start = (offset % chunk) as usize;
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn load(&mut self, index: u32) -> Result<&[u8]> {
        if self.cached.as_ref().is_none_or(|(i, _)| *i != index) {
            let sealed = self.chunk + TAG_LEN;
            let last = index == self.chunks - 1;
            let mut ciphertext = vec![0u8; if last { self.last_sealed_len } else { sealed }];
            self.inner.seek(SeekFrom::Start(
                HEADER_LEN as u64 + u64::from(index) * sealed as u64,
            ))?;
            if read_full(&mut self.inner, &mut ciphertext)? != ciphertext.len() {
                return Err(Error::Corrupted);
            }
            self.cached = Some((index, self.cipher.open(index, last, &ciphertext)?));
        }
        Ok(&self.cached.as_ref().expect("chunk was just cached").1)
    }
}
