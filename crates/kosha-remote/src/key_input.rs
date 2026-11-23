// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use russh::keys::{self, PrivateKey};
use zeroize::Zeroizing;

const KEY_BYTES: usize = 64 << 10;
pub(crate) const PASSPHRASE_BYTES: usize = 4096;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("SSH scanning is disabled")]
    Disabled,
    #[error("could not read SSH key input: {0:?}")]
    Read(io::ErrorKind),
    #[error("SSH key path is not a regular file")]
    NotRegular,
    #[error("SSH key is writable by group or others")]
    WritableByOthers,
    #[error("SSH key input exceeds the size limit")]
    TooLarge,
    #[error("SSH key input is not UTF-8")]
    NotUtf8,
    #[error("SSH private key requires a passphrase")]
    PassphraseRequired,
    #[error("could not decode the SSH private key")]
    Decode,
}

pub struct SecretText {
    bytes: Zeroizing<Vec<u8>>,
    len: usize,
}

impl SecretText {
    pub(crate) fn from_bytes(bytes: Zeroizing<Vec<u8>>, len: usize) -> Result<Self, Error> {
        std::str::from_utf8(&bytes[..len]).map_err(|_| Error::NotUtf8)?;
        Ok(Self { bytes, len })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("validated text")
    }
}

fn read_text(mut input: impl Read, limit: usize) -> Result<SecretText, Error> {
    // Allocate once so growing the buffer cannot leave former allocations
    // containing copies of the key or passphrase.
    let mut bytes = Zeroizing::new(vec![0; limit + 1]);
    let mut len = 0;
    while len <= limit {
        match input.read(&mut bytes[len..]) {
            Ok(0) => break,
            Ok(count) => len += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(Error::Read(error.kind())),
        }
    }
    if len > limit {
        return Err(Error::TooLarge);
    }
    SecretText::from_bytes(bytes, len)
}

pub fn read_passphrase(input: impl Read) -> Result<SecretText, Error> {
    read_text(input, PASSPHRASE_BYTES)
}

pub struct Loaded {
    pub key: PrivateKey,
    pub readable_by_others: bool,
}

/// Load the public key used to select one identity from a local SSH agent.
#[cfg(any(unix, windows))]
pub fn load_public_key(enabled: bool, path: &Path) -> Result<keys::PublicKey, Error> {
    if !enabled {
        return Err(Error::Disabled);
    }
    let (file, _) = open_candidate(path)?;
    let input = read_text(file, KEY_BYTES)?;
    keys::PublicKey::from_openssh(input.as_str().trim()).map_err(|_| Error::Decode)
}

pub fn load(enabled: bool, path: &Path, passphrase: Option<&str>) -> Result<Loaded, Error> {
    if !enabled {
        return Err(Error::Disabled);
    }
    let read_error = |error: io::Error| Error::Read(error.kind());
    if !path.metadata().map_err(read_error)?.is_file() {
        return Err(Error::NotRegular);
    }
    let (file, _metadata) = open_candidate(path)?;
    #[cfg(unix)]
    let readable_by_others = {
        use std::os::unix::fs::PermissionsExt;
        permission_warning(_metadata.permissions().mode())?
    };
    #[cfg(not(unix))]
    let readable_by_others = false;
    let input = read_text(file, KEY_BYTES)?;
    let key = keys::decode_secret_key(input.as_str(), passphrase).map_err(|error| match error {
        keys::Error::KeyIsEncrypted => Error::PassphraseRequired,
        _ => Error::Decode,
    })?;
    Ok(Loaded {
        key,
        readable_by_others,
    })
}

fn open_candidate(path: &Path) -> Result<(File, Metadata), Error> {
    let read_error = |error: io::Error| Error::Read(error.kind());
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A regular path could be replaced with a FIFO after preflight.
        // Refuse based on the opened handle before reading any input.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err(Error::NotRegular);
    }
    Ok((file, metadata))
}

#[cfg(unix)]
fn permission_warning(mode: u32) -> Result<bool, Error> {
    if mode & 0o022 != 0 {
        return Err(Error::WritableByOthers);
    }
    Ok(mode & 0o044 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn disabled_key_loading_returns_before_path_access() {
        assert_eq!(
            load(false, Path::new("\0"), None).err(),
            Some(Error::Disabled)
        );
    }

    #[cfg(unix)]
    #[test]
    fn opened_fifo_is_refused_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("key-fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path in the temporary directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(open_candidate(&path).err(), Some(Error::NotRegular));
    }

    #[test]
    fn rejects_one_byte_over_limit_without_reading_the_remainder() {
        let exactly = read_text(&b"exact"[..], 5).unwrap();
        assert_eq!(exactly.as_str(), "exact");
        let mut reader = Cursor::new(b"excess data");
        assert_eq!(read_text(&mut reader, 5).err(), Some(Error::TooLarge));
        assert_eq!(reader.position(), 6);
    }

    #[test]
    fn reports_invalid_encoding_without_echoing_data() {
        assert_eq!(read_text(&b"\xff"[..], 5).err(), Some(Error::NotUtf8));
    }

    #[cfg(unix)]
    #[test]
    fn unix_permissions_refuse_write_and_warn_on_read() {
        assert_eq!(permission_warning(0o600), Ok(false));
        assert_eq!(permission_warning(0o640), Ok(true));
        assert_eq!(permission_warning(0o604), Ok(true));
        assert_eq!(permission_warning(0o620), Err(Error::WritableByOthers));
        assert_eq!(permission_warning(0o602), Err(Error::WritableByOthers));
    }
}
