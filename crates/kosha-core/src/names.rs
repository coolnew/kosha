// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! One contiguous byte buffer holding every file name in a scan.
//!
//! Names are stored as OS-native encoded bytes rather than `String` so that
//! non-UTF-8 Unix names and Windows names containing unpaired surrogates
//! survive a round trip and can be handed straight back to the OS.

use std::ffi::OsStr;

/// Append-only arena of file names.
#[derive(Default)]
pub struct NameArena {
    buf: Vec<u8>,
}

impl NameArena {
    pub fn with_capacity(bytes: usize) -> Self {
        NameArena {
            buf: Vec::with_capacity(bytes),
        }
    }

    /// Append `name`, returning the `(offset, len)` pair to store on the node.
    ///
    /// A name longer than `u16::MAX` bytes cannot exist on any filesystem we
    /// support (`NAME_MAX` is 255 on Linux, 255 UTF-16 units on Windows). If
    /// one somehow appears we store an empty name rather than truncating —
    /// truncation could split a multi-byte sequence and break the encoding
    /// invariant that [`NameArena::get`] relies on.
    #[inline]
    pub fn push(&mut self, name: &OsStr) -> (u32, u16) {
        self.push_bytes(name.as_encoded_bytes())
    }

    /// Append one name that is already in OS-native encoding.
    ///
    /// The MFT reader encodes every name on the volume into a buffer of its own
    /// before it knows which of them end up in the tree, so it has bytes to
    /// hand over rather than an `OsStr`. The contract is [`NameArena::push`]'s:
    /// these must be a *whole* name exactly as `OsStr::as_encoded_bytes` would
    /// produce it, because [`NameArena::get`] hands them back as one.
    #[inline]
    pub fn push_bytes(&mut self, bytes: &[u8]) -> (u32, u16) {
        let off = self.buf.len() as u32;
        if bytes.len() > u16::MAX as usize {
            return (off, 0);
        }
        self.buf.extend_from_slice(bytes);
        (off, bytes.len() as u16)
    }

    /// Retrieve a previously pushed name.
    ///
    /// # Panics
    ///
    /// Panics if `off..off + len` is out of bounds, which would mean the caller
    /// passed a pair this arena never produced.
    #[inline]
    pub fn get(&self, off: u32, len: u16) -> &OsStr {
        let start = off as usize;
        let bytes = &self.buf[start..start + len as usize];
        // SAFETY: `bytes` is exactly the byte range produced by a previous
        // `push` (or `append_raw`) of whole `OsStr`s, copied verbatim from
        // `OsStr::as_encoded_bytes`.
        // The buffer is append-only and private, so no other code can have
        // written to or re-sliced this range, and `push` never stores a
        // partial name. The encoding invariant therefore still holds.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }

    /// Append a pre-built block of concatenated names, returning the offset the
    /// block landed at. Callers rebase their local offsets by this value.
    ///
    /// This is the merge step's fast path: one `memcpy` per directory instead
    /// of one push per entry.
    #[inline]
    pub fn append_raw(&mut self, block: &[u8]) -> u32 {
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(block);
        off
    }

    /// Release growth slack once the arena is complete.
    /// The whole arena, for writing it to a file.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Take an arena back from a file.
    ///
    /// No validation here: an offset is only meaningful against the nodes that
    /// point at it, so the check belongs where both halves are in hand. See
    /// `snapshot::validate`.
    #[inline]
    pub fn from_bytes(buf: Vec<u8>) -> Self {
        NameArena { buf }
    }

    pub fn shrink_to_fit(&mut self) {
        self.buf.shrink_to_fit();
    }

    /// Total bytes held, for memory accounting.
    #[inline]
    pub fn bytes(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_names() {
        let mut arena = NameArena::default();
        let a = arena.push(OsStr::new("Cargo.toml"));
        let b = arena.push(OsStr::new("node_modules"));
        let c = arena.push(OsStr::new("файл — «тест».txt"));

        assert_eq!(arena.get(a.0, a.1), OsStr::new("Cargo.toml"));
        assert_eq!(arena.get(b.0, b.1), OsStr::new("node_modules"));
        assert_eq!(arena.get(c.0, c.1), OsStr::new("файл — «тест».txt"));
    }

    #[test]
    fn empty_name_is_representable() {
        let mut arena = NameArena::default();
        let e = arena.push(OsStr::new(""));
        assert_eq!(e.1, 0);
        assert_eq!(arena.get(e.0, e.1), OsStr::new(""));
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_non_utf8_unix_names() {
        use std::os::unix::ffi::OsStrExt;

        let raw = OsStr::from_bytes(b"broken-\xff\xfe-name");
        let mut arena = NameArena::default();
        let n = arena.push(raw);
        assert_eq!(arena.get(n.0, n.1), raw);
    }

    /// The other half of this module's opening claim. Windows filenames are
    /// UTF-16 that is not required to be well-formed, and an unpaired surrogate
    /// is the case that breaks anything routing a name through `String`. They
    /// do occur — an interrupted transfer, or a filesystem written by something
    /// careless — and a name we cannot reproduce exactly is a file we cannot
    /// hand back to the OS to open or delete.
    #[cfg(windows)]
    #[test]
    fn round_trips_windows_names_with_an_unpaired_surrogate() {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        // "odd-", a lone high surrogate, "-name": legal UTF-16 code units that
        // are not legal UTF-16 text, and a legal NTFS name.
        let units: Vec<u16> = "odd-"
            .encode_utf16()
            .chain(std::iter::once(0xD800))
            .chain("-name".encode_utf16())
            .collect();
        let raw = OsString::from_wide(&units);
        assert!(raw.to_str().is_none(), "the fixture has to be ill-formed");

        let mut arena = NameArena::default();
        let n = arena.push(&raw);
        let back = arena.get(n.0, n.1);

        assert_eq!(back, raw.as_os_str());
        assert_eq!(
            back.encode_wide().collect::<Vec<u16>>(),
            units,
            "the name has to come back as the same UTF-16 the OS gave us"
        );
    }
}
