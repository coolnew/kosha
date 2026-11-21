// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! One contiguous byte buffer holding every file name in a scan.
//!
//! Names are stored as OS-native encoded bytes rather than `String` so that
//! non-UTF-8 Unix names and Windows names containing unpaired surrogates
//! survive a round trip and can be handed straight back to the OS.
//!
//! ## Where the encoding is checked
//!
//! Handing bytes back as an `OsStr` uses `OsStr::from_encoded_bytes_unchecked`,
//! whose contract is that the bytes are something `OsStr::as_encoded_bytes`
//! could have produced on this platform. On Unix that is any byte string. On
//! Windows it is well-formed WTF-8, and a byte string that is not — a Linux
//! name that is not UTF-8, a corrupt snapshot, a span that starts in the middle
//! of a character — is undefined behaviour, not merely a wrong answer.
//!
//! So the check sits at the boundary, once, and never on the read path:
//!
//! * [`NameArena::push`] takes an `OsStr`, which is valid by construction.
//! * [`NameArena::push_bytes`] checks, and stores a lossy UTF-8 rendering of
//!   bytes this platform cannot represent.
//! * [`NameArena::from_bytes`] and [`NameArena::append_raw`] accept anything,
//!   because a buffer is not a name until a node points into it. Every span a
//!   tree hands to the unchecked accessor is checked where the nodes and the
//!   buffer meet: in [`SizeTree::from_parts`](crate::SizeTree::from_parts),
//!   which a snapshot reader, an image builder and a view reader all go
//!   through. The directory walk is the one exception, and it only ever
//!   stores `as_encoded_bytes` of names the OS handed it.
//! * [`NameArena::get`], the one public reader, checks the span it is given.
//!   It costs nothing on Unix, where every span is valid.

use std::borrow::Cow;
use std::ffi::OsStr;

use crate::node::Node;

/// Whether `bytes` is well-formed WTF-8: the encoding Rust uses for an
/// `OsStr` on Windows.
///
/// WTF-8 is UTF-8 that may also encode a surrogate code point
/// (U+D800..=U+DFFF) as three bytes, so that UTF-16 with an unpaired surrogate
/// has a spelling. *Well-formed* adds one rule: a high surrogate immediately
/// followed by a low surrogate must be written as the four-byte supplementary
/// character they pair into, never as two three-byte halves. Everything else —
/// overlong forms, code points above U+10FFFF, truncated or stray sequences,
/// the bytes 0xC0, 0xC1 and 0xF5..=0xFF — is refused exactly as UTF-8 refuses
/// it.
///
/// Compiled and tested on every platform, deliberately: it decides what a
/// Windows build accepts from a snapshot written on Linux, and a rule that only
/// ran where it applied would only be tested there too.
pub fn is_well_formed_wtf8(bytes: &[u8]) -> bool {
    let mut at = 0;
    // A high surrogate that ended exactly where the next character starts.
    let mut after_high_surrogate = false;
    while let Some(&lead) = bytes.get(at) {
        if lead < 0x80 {
            at += 1;
            after_high_surrogate = false;
            continue;
        }
        // The allowed range of the *second* byte depends on the first; every
        // byte after that is a plain continuation byte.
        let (width, second) = match lead {
            0xC2..=0xDF => (2, 0x80..=0xBF),
            0xE0 => (3, 0xA0..=0xBF),
            0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80..=0xBF),
            // UTF-8 stops at 0x9F here, because 0xA0..=0xBF are surrogates.
            // WTF-8 allows them: that is the whole of the difference.
            0xED => (3, 0x80..=0xBF),
            0xF0 => (4, 0x90..=0xBF),
            0xF1..=0xF3 => (4, 0x80..=0xBF),
            0xF4 => (4, 0x80..=0x8F),
            _ => return false,
        };
        let Some(sequence) = bytes.get(at..at + width) else {
            return false;
        };
        if !second.contains(&sequence[1]) {
            return false;
        }
        if !sequence[2..].iter().all(|b| (0x80..=0xBF).contains(b)) {
            return false;
        }
        let high = lead == 0xED && (0xA0..=0xAF).contains(&sequence[1]);
        let low = lead == 0xED && (0xB0..=0xBF).contains(&sequence[1]);
        if low && after_high_surrogate {
            return false;
        }
        after_high_surrogate = high;
        at += width;
    }
    true
}

/// Whether `bytes` is a name this platform's `OsStr` can hold.
///
/// Any byte string on Unix; well-formed WTF-8 on Windows; UTF-8, which every
/// platform accepts, anywhere else.
#[cfg(unix)]
#[inline]
pub fn is_valid_encoded_name(_bytes: &[u8]) -> bool {
    true
}

/// Whether `bytes` is a name this platform's `OsStr` can hold.
///
/// Any byte string on Unix; well-formed WTF-8 on Windows; UTF-8, which every
/// platform accepts, anywhere else.
#[cfg(windows)]
#[inline]
pub fn is_valid_encoded_name(bytes: &[u8]) -> bool {
    is_well_formed_wtf8(bytes)
}

/// Whether `bytes` is a name this platform's `OsStr` can hold.
///
/// Any byte string on Unix; well-formed WTF-8 on Windows; UTF-8, which every
/// platform accepts, anywhere else.
#[cfg(not(any(unix, windows)))]
#[inline]
pub fn is_valid_encoded_name(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

/// True where every byte string is a valid name, so a check can be skipped
/// outright rather than run and found to pass.
pub(crate) const EVERY_BYTE_STRING_IS_A_NAME: bool = cfg!(unix);

/// What to store in place of a name this platform cannot represent.
///
/// UTF-8, which is valid on every platform, with U+FFFD where the bytes were.
/// The same choice the ext4 reader and the exports make.
fn lossy(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

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
        self.push_valid(name.as_encoded_bytes())
    }

    /// Append one name from bytes, checking that this platform can hold it.
    ///
    /// The MFT and image readers decode names into buffers of their own before
    /// they know which of them end up in the tree, so they have bytes to hand
    /// over rather than an `OsStr`. Bytes that are a valid name here (see
    /// [`is_valid_encoded_name`]) are stored exactly. Bytes that are not — a
    /// Linux name that is not UTF-8, met on Windows — are stored as UTF-8 with
    /// U+FFFD in place of what could not be represented, because a name that
    /// cannot be shown exactly can still be shown, and an unchecked one would
    /// be undefined behaviour the moment it was read.
    #[inline]
    pub fn push_bytes(&mut self, bytes: &[u8]) -> (u32, u16) {
        if is_valid_encoded_name(bytes) {
            self.push_valid(bytes)
        } else {
            self.push_valid(lossy(bytes).as_bytes())
        }
    }

    #[inline]
    fn push_valid(&mut self, bytes: &[u8]) -> (u32, u16) {
        let off = self.buf.len() as u32;
        if bytes.len() > u16::MAX as usize {
            return (off, 0);
        }
        self.buf.extend_from_slice(bytes);
        (off, bytes.len() as u16)
    }

    /// Retrieve a name by its `(offset, len)` pair.
    ///
    /// # Panics
    ///
    /// Panics if `off..off + len` is out of bounds, or if it is not a whole
    /// name this platform can hold — either would mean the caller passed a
    /// pair this arena never produced. The second check is free on Unix,
    /// where every span is a valid name; a tree reads its own names through
    /// an unchecked path whose spans were checked once when it was built.
    #[inline]
    pub fn get(&self, off: u32, len: u16) -> &OsStr {
        let bytes = self.span(off, len);
        assert!(
            is_valid_encoded_name(bytes),
            "a name span this platform cannot represent"
        );
        // SAFETY: just checked that the bytes are a valid encoded name here.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }

    /// Retrieve a name without checking its encoding.
    ///
    /// # Safety
    ///
    /// `off..off + len` must be a span whose bytes satisfy
    /// [`is_valid_encoded_name`]: one returned by [`push`](Self::push) or
    /// [`push_bytes`](Self::push_bytes), one the scanner stored from
    /// `as_encoded_bytes`, or one [`replace_invalid_names`] has checked. An
    /// out-of-bounds span still panics rather than reading past the arena.
    #[inline]
    pub(crate) unsafe fn get_unchecked(&self, off: u32, len: u16) -> &OsStr {
        let bytes = self.span(off, len);
        debug_assert!(is_valid_encoded_name(bytes));
        // SAFETY: the caller guarantees the bytes are a valid encoded name on
        // this platform, which is `from_encoded_bytes_unchecked`'s contract.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }

    #[inline]
    fn span(&self, off: u32, len: u16) -> &[u8] {
        let start = off as usize;
        &self.buf[start..start + len as usize]
    }

    /// Append a pre-built block of concatenated names, returning the offset the
    /// block landed at. Callers rebase their local offsets by this value.
    ///
    /// This is the merge step's fast path: one `memcpy` per directory instead
    /// of one push per entry. The block is not checked: bytes become a name
    /// only when a node points at them, and that is where the check is.
    #[inline]
    pub fn append_raw(&mut self, block: &[u8]) -> u32 {
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(block);
        off
    }

    /// The whole arena, for writing it to a file.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Take an arena back from a file.
    ///
    /// No validation here: an offset is only meaningful against the nodes that
    /// point at it, so both checks — bounds in `snapshot::validate`, encoding
    /// in [`SizeTree::from_parts`](crate::SizeTree::from_parts) — belong where
    /// both halves are in hand.
    #[inline]
    pub fn from_bytes(buf: Vec<u8>) -> Self {
        NameArena { buf }
    }

    /// Release growth slack once the arena is complete.
    pub fn shrink_to_fit(&mut self) {
        self.buf.shrink_to_fit();
    }

    /// Total bytes held, for memory accounting.
    #[inline]
    pub fn bytes(&self) -> usize {
        self.buf.len()
    }
}

/// Make every node's name span one that `accept` and this platform accept,
/// returning the indices of the nodes whose names had to be replaced.
///
/// A span that is out of bounds, or whose bytes are not a valid name, gets a
/// new span holding the lossy UTF-8 rendering of what was there (U+FFFD for
/// anything out of bounds), appended to the arena. Every other span is left
/// exactly as it was, byte for byte. The returned indices are ascending.
///
/// `accept` is for tests, which pass [`is_well_formed_wtf8`] to see what a
/// Windows build would do on any platform; it can only make the check
/// stricter, never looser, because the platform's own rule always applies.
pub(crate) fn replace_invalid_names(
    nodes: &mut [Node],
    names: &mut NameArena,
    accept: impl Fn(&[u8]) -> bool,
) -> Vec<u32> {
    // Spans are judged against the arena as it arrived. A replacement appended
    // below must not make a later out-of-bounds span land, by accident, in the
    // middle of a replacement.
    let original = names.buf.len();
    let mut replaced = Vec::new();
    for (index, node) in nodes.iter_mut().enumerate() {
        let start = node.name_off as usize;
        let end = start + node.name_len as usize;
        let current = if end <= original {
            Some(&names.buf[start..end])
        } else {
            None
        };
        if current.is_some_and(|bytes| accept(bytes) && is_valid_encoded_name(bytes)) {
            continue;
        }
        let text = match current {
            Some(bytes) => lossy(bytes).into_owned(),
            None => "\u{FFFD}".to_string(),
        };
        let text = if text.len() > u16::MAX as usize {
            "\u{FFFD}"
        } else {
            text.as_str()
        };
        // A name whose offset would not fit in a `u32` becomes the empty name,
        // which is a valid name everywhere and needs no bytes.
        let (off, len) = if names.buf.len() + text.len() > u32::MAX as usize {
            (0, 0)
        } else {
            names.push_valid(text.as_bytes())
        };
        node.name_off = off;
        node.name_len = len;
        replaced.push(index as u32);
    }
    replaced
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

    // ---------------------------------------------------------- the validator

    /// A surrogate code point written the WTF-8 way: three bytes, as UTF-8
    /// would write it if UTF-8 allowed it.
    fn surrogate(unit: u16) -> [u8; 3] {
        assert!((0xD800..=0xDFFF).contains(&unit));
        [
            0xE0 | (unit >> 12) as u8,
            0x80 | ((unit >> 6) & 0x3F) as u8,
            0x80 | (unit & 0x3F) as u8,
        ]
    }

    /// UTF-16, possibly ill-formed, encoded as WTF-8: paired surrogates join
    /// into one supplementary character, unpaired ones are written alone. What
    /// `OsString::from_wide(..).as_encoded_bytes()` produces on Windows, built
    /// here so the agreement can be checked on every platform.
    fn wtf8_of(units: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for item in char::decode_utf16(units.iter().copied()) {
            match item {
                Ok(c) => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                Err(e) => out.extend_from_slice(&surrogate(e.unpaired_surrogate())),
            }
        }
        out
    }

    #[test]
    fn accepts_empty_ascii_nul_and_utf8() {
        assert!(is_well_formed_wtf8(b""));
        assert!(is_well_formed_wtf8(b"Cargo.toml"));
        assert!(is_well_formed_wtf8(b"a\0b"), "NUL is a valid code point");
        assert!(is_well_formed_wtf8("файл — «тест».txt".as_bytes()));
        assert!(is_well_formed_wtf8("emoji-\u{1F600}-\u{10FFFF}".as_bytes()));
    }

    #[test]
    fn accepts_unpaired_surrogates() {
        let high = surrogate(0xD800);
        let low = surrogate(0xDFFF);
        assert!(is_well_formed_wtf8(&high));
        assert!(is_well_formed_wtf8(&low));
        assert!(is_well_formed_wtf8(
            &[b"odd-".as_slice(), &high, b"-name"].concat()
        ));
        // A low before a high is two unpaired surrogates, and legal.
        assert!(is_well_formed_wtf8(&[low, high].concat()));
        // Something in between keeps them unpaired.
        assert!(is_well_formed_wtf8(&[high.as_slice(), b"x", &low].concat()));
        // Two highs in a row are both unpaired.
        assert!(is_well_formed_wtf8(&[high, high].concat()));
    }

    #[test]
    fn refuses_a_surrogate_pair_written_as_two_halves() {
        // CESU-8's spelling of U+10000. Well-formed WTF-8 writes the pair as
        // the one four-byte character, so this is not something Windows'
        // `as_encoded_bytes` can produce.
        let split = [surrogate(0xD800), surrogate(0xDC00)].concat();
        assert!(!is_well_formed_wtf8(&split));
        assert!(!is_well_formed_wtf8(
            &[b"a".as_slice(), &split, b"b"].concat()
        ));
        assert!(is_well_formed_wtf8("\u{10000}".as_bytes()));
    }

    #[test]
    fn refuses_overlong_encodings() {
        for overlong in [
            &[0xC0, 0x80][..],               // NUL in two bytes
            &[0xC1, 0xBF],                   // U+007F in two bytes
            &[0xE0, 0x80, 0x80],             // NUL in three
            &[0xE0, 0x9F, 0xBF],             // U+07FF in three
            &[0xF0, 0x80, 0x80, 0x80],       // NUL in four
            &[0xF0, 0x8F, 0xBF, 0xBF],       // U+FFFF in four
            &[0xF8, 0x88, 0x80, 0x80, 0x80], // five-byte forms do not exist
        ] {
            assert!(!is_well_formed_wtf8(overlong), "{overlong:02x?}");
        }
    }

    #[test]
    fn refuses_truncated_and_stray_sequences() {
        for broken in [
            &[0xC3][..],
            &[0xE2, 0x82],
            &[0xF0, 0x9F, 0x98],
            &[0xED, 0xA0], // a surrogate cut short
            &[0x80],       // a continuation with no lead
            &[b'a', 0xBF, b'b'],
            &[0xE2, 0x28, 0xA1], // a lead followed by ASCII
            &[0xC3, 0xC3, 0xA9],
        ] {
            assert!(!is_well_formed_wtf8(broken), "{broken:02x?}");
        }
    }

    #[test]
    fn refuses_bytes_that_never_appear() {
        for byte in [0xC0u8, 0xC1, 0xF5, 0xF8, 0xFC, 0xFE, 0xFF] {
            assert!(!is_well_formed_wtf8(&[byte]), "{byte:02x}");
            assert!(!is_well_formed_wtf8(&[b'x', byte, b'y']), "{byte:02x}");
        }
        assert!(
            !is_well_formed_wtf8(b"caf\xe9.txt"),
            "Latin-1, as Linux may store it"
        );
        // Above U+10FFFF.
        assert!(!is_well_formed_wtf8(&[0xF4, 0x90, 0x80, 0x80]));
    }

    /// Exhaustive over every one-, two- and three-byte string: the validator
    /// agrees with UTF-8 except on exactly the three-byte surrogates.
    #[test]
    fn agrees_with_utf8_except_for_surrogates_on_every_short_string() {
        for a in 0..=255u8 {
            assert_eq!(is_well_formed_wtf8(&[a]), std::str::from_utf8(&[a]).is_ok());
            for b in 0..=255u8 {
                let two = [a, b];
                assert_eq!(
                    is_well_formed_wtf8(&two),
                    std::str::from_utf8(&two).is_ok(),
                    "{two:02x?}"
                );
                for c in 0..=255u8 {
                    let three = [a, b, c];
                    let is_surrogate =
                        a == 0xED && (0xA0..=0xBF).contains(&b) && (0x80..=0xBF).contains(&c);
                    assert_eq!(
                        is_well_formed_wtf8(&three),
                        std::str::from_utf8(&three).is_ok() || is_surrogate,
                        "{three:02x?}"
                    );
                }
            }
        }
    }

    /// Every UTF-16 code unit, and every pair involving a surrogate, encodes to
    /// something the validator accepts — so no name Windows can hand us is
    /// refused.
    #[test]
    fn accepts_everything_ill_formed_utf16_encodes_to() {
        for unit in 0..=u16::MAX {
            assert!(is_well_formed_wtf8(&wtf8_of(&[unit])), "{unit:04x}");
        }
        let interesting = [
            0x0041, 0x00E9, 0xD7FF, 0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0xE000, 0xFFFF,
        ];
        for &a in &interesting {
            for &b in &interesting {
                for &c in &interesting {
                    let units = [a, b, c];
                    assert!(is_well_formed_wtf8(&wtf8_of(&units)), "{units:04x?}");
                }
            }
        }
    }

    /// The test-side encoder above is the reference for the test before this
    /// one, so on Windows it is held against the real thing.
    #[cfg(windows)]
    #[test]
    fn the_reference_encoder_matches_windows() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let interesting = [0x0041u16, 0x00E9, 0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0xFFFF];
        for &a in &interesting {
            for &b in &interesting {
                let units = [a, b];
                assert_eq!(
                    OsString::from_wide(&units).as_encoded_bytes(),
                    wtf8_of(&units).as_slice(),
                    "{units:04x?}"
                );
            }
        }
    }

    #[test]
    fn the_platform_rule_is_what_this_platform_accepts() {
        let latin1 = b"caf\xe9.txt";
        let split_pair = [surrogate(0xD800), surrogate(0xDC00)].concat();
        let lone = surrogate(0xD800);
        assert!(is_valid_encoded_name(b"plain.txt"));
        if cfg!(unix) {
            assert!(is_valid_encoded_name(latin1));
            assert!(is_valid_encoded_name(&split_pair));
            assert!(is_valid_encoded_name(&lone));
        } else if cfg!(windows) {
            assert!(!is_valid_encoded_name(latin1));
            assert!(!is_valid_encoded_name(&split_pair));
            assert!(is_valid_encoded_name(&lone));
        } else {
            assert!(!is_valid_encoded_name(latin1));
            assert!(!is_valid_encoded_name(&lone));
        }
    }

    // ------------------------------------------------- the safe constructors

    /// `push_bytes` is safe, so whatever it is given, what `get` reads back is
    /// a valid name here: the same bytes when they were one, a lossy rendering
    /// when they were not.
    #[test]
    fn push_bytes_never_stores_a_name_this_platform_cannot_hold() {
        let inputs: [&[u8]; 6] = [
            b"plain.txt",
            b"caf\xe9.txt",
            b"\xff\xfe",
            &[0xED, 0xA0, 0x80],
            &[0xED, 0xA0, 0x80, 0xED, 0xB0, 0x80],
            &[0xE2, 0x82],
        ];
        let mut arena = NameArena::default();
        for input in inputs {
            let (off, len) = arena.push_bytes(input);
            let back = arena.get(off, len).as_encoded_bytes();
            assert!(is_valid_encoded_name(back));
            if is_valid_encoded_name(input) {
                assert_eq!(back, input, "a valid name is stored exactly");
            } else {
                assert_eq!(back, String::from_utf8_lossy(input).as_bytes());
            }
        }
    }

    /// A span that cuts a character in half is not a name. On Unix it is a
    /// perfectly good byte string; anywhere else `get` refuses it rather than
    /// handing back an `OsStr` that breaks its own type's invariant.
    #[test]
    fn get_refuses_a_span_that_splits_a_character_where_that_matters() {
        let mut arena = NameArena::default();
        let (off, _) = arena.push(OsStr::new("é"));
        let half = std::panic::catch_unwind(|| arena.get(off, 1).as_encoded_bytes().to_vec());
        if EVERY_BYTE_STRING_IS_A_NAME {
            assert_eq!(half.unwrap(), vec![0xC3]);
        } else {
            assert!(half.is_err(), "half a character must not become an OsStr");
        }
    }

    fn node(name_off: u32, name_len: u16) -> Node {
        Node {
            logical: 0,
            allocated: 0,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: 0,
            name_off,
            mtime: 0,
            name_len,
            flags: crate::node::Flags::default(),
            depth: 0,
        }
    }

    /// The repair a Windows build applies, run here with the Windows rule so
    /// it is exercised on every platform.
    #[test]
    fn replacing_invalid_names_touches_only_the_invalid_ones() {
        let high = surrogate(0xD800);
        let low = surrogate(0xDC00);
        let mut raw = Vec::new();
        let mut span = |bytes: &[u8]| {
            let off = raw.len() as u32;
            raw.extend_from_slice(bytes);
            (off, bytes.len() as u16)
        };
        let plain = span(b"a.txt");
        let latin1 = span(b"caf\xe9.txt");
        let lone_high = span(&high);
        let lone_low = span(&low);
        let arena_len = raw.len() as u32;

        let mut nodes = vec![
            node(0, 0),                     // 0: the root's empty name
            node(plain.0, plain.1),         // 1: valid everywhere
            node(latin1.0, latin1.1),       // 2: not WTF-8
            node(lone_high.0, lone_high.1), // 3: a lone surrogate is fine
            node(lone_low.0, lone_low.1),   // 4: so is this one
            // 5: the two together read as one span are a split pair.
            node(lone_high.0, lone_high.1 + lone_low.1),
            node(arena_len, 4),   // 6: outside the arena
            node(plain.0 + 1, 2), // 7: a sub-span of ASCII is fine
        ];
        let original = nodes.clone();
        let mut arena = NameArena::from_bytes(raw.clone());

        let replaced = replace_invalid_names(&mut nodes, &mut arena, is_well_formed_wtf8);

        assert_eq!(replaced, vec![2, 5, 6]);
        for index in [0, 1, 3, 4, 7] {
            assert_eq!(nodes[index].name_off, original[index].name_off);
            assert_eq!(nodes[index].name_len, original[index].name_len);
        }
        assert_eq!(
            &arena.as_bytes()[..raw.len()],
            raw.as_slice(),
            "existing bytes stay"
        );
        let text = |n: &Node| arena.span(n.name_off, n.name_len).to_vec();
        assert_eq!(text(&nodes[2]), "caf\u{FFFD}.txt".as_bytes());
        assert_eq!(text(&nodes[6]), "\u{FFFD}".as_bytes());
        for n in &nodes {
            assert!(is_well_formed_wtf8(&text(n)));
        }

        // Run again: nothing left to replace.
        assert!(replace_invalid_names(&mut nodes, &mut arena, is_well_formed_wtf8).is_empty());
    }
}
