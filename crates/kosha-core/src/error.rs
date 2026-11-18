// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::io;
use std::path::PathBuf;

/// Failures that stop a scan before it can produce a tree.
///
/// Note what is *not* here: unreadable directories and entries. Those are
/// routine — a scan of `/` as an ordinary user hits hundreds — so they are
/// recorded as flags on the affected nodes and counted in
/// [`ScanStats`](crate::ScanStats) rather than aborting the walk. A cancelled
/// scan is likewise not an error: it returns a valid tree with
/// `stats.cancelled` set.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("cannot read scan root `{path}`")]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("scan root `{0}` is not a directory")]
    NotADirectory(PathBuf),

    /// The scan root is a disk image and reading images was not asked for.
    ///
    /// A separate answer from [`NotADirectory`](ScanError::NotADirectory)
    /// because it is a different situation with a different thing to do about
    /// it. An image *is* a file, so the ordinary walk refuses it with a
    /// sentence that is true of every disk image ever made and tells nobody
    /// anything. Whoever pointed kosha at `ext4.vhdx` — or at the `disk.img`
    /// they made with `mkfs` — meant to look inside it.
    #[error("scan root `{0}` is a disk image, and kosha was not asked to read images")]
    ImageNotRead(PathBuf),

    /// The scan root is a disk image, reading it was asked for, and it could
    /// not be done — carrying the reason the strategy worked out.
    ///
    /// Without this the reason is destroyed exactly where it is most needed:
    /// the strategy knows the file is not a VHDX, or holds a filesystem it does
    /// not read, and the walk then throws that away and reports "not a
    /// directory".
    // Not a `#[source]`: the reason *is* the sentence, and chaining it as a
    // cause made the command line print the same words twice.
    #[error("scan root `{path}`: {reason}")]
    Image {
        path: PathBuf,
        reason: crate::tree::Unavailable,
    },

    /// The requested strategy could not run. Never reaches a caller of
    /// [`Scanner::scan`](crate::Scanner::scan): the MFT is a *request*, and
    /// this is what the walk falls back from, recorded on the tree it produces
    /// as [`ScanStats::declined`](crate::ScanStats::declined).
    #[error("{0}")]
    Strategy(#[source] crate::tree::Unavailable),

    /// The caller cancelled before there was a tree to hand back.
    ///
    /// The walk has no use for this — it stops at a level boundary and returns
    /// the valid prefix it has built — but the MFT strategy has nothing to hand
    /// back until the whole table is read, so a cancel during that read is an
    /// error rather than a partial tree.
    #[error("the scan was cancelled")]
    Cancelled,

    /// Node ids are `u32`. No real volume comes close, and wrapping silently
    /// would corrupt every parent pointer above the overflow.
    #[error("scan exceeded the supported maximum of {max} entries (reached {reached})")]
    TooManyEntries { reached: usize, max: u64 },
}

impl ScanError {
    /// The reason on its own, for somewhere that shows the path itself.
    ///
    /// **Every sentence [`Display`](std::fmt::Display) produces has the path in
    /// it**, because the command line prints one line and the path is half of
    /// what that line says. A window has more room and less of it: it draws the
    /// path on a line of its own and the reason underneath, so `Display` there
    /// is the same path twice.
    ///
    /// That is not a tidiness point. A real path is long — the one this was
    /// found with ran to a hundred and twenty characters — so the window filled
    /// four lines, three of them path, and put the one fact anybody needed at
    /// the end of the fourth. The failure screen already learnt this lesson
    /// once, about the size of its heading.
    ///
    /// Written so it reads as a sentence about the path above it, which is why
    /// several of these begin "it".
    pub fn reason(&self) -> String {
        match self {
            ScanError::Root { source, .. } => format!("it could not be read: {source}"),
            ScanError::NotADirectory(_) => "it is not a directory".into(),
            ScanError::ImageNotRead(_) => {
                "it is a disk image, and kosha was not asked to read images".into()
            }
            // These two already say only the reason: the path in `Image` is a
            // separate field and `Strategy` never had one.
            ScanError::Image { reason, .. } => reason.to_string(),
            ScanError::Strategy(reason) => reason.to_string(),
            ScanError::Cancelled => "the scan was cancelled".into(),
            ScanError::TooManyEntries { reached, max } => {
                format!(
                    "it holds more than the {max} entries kosha can address (reached {reached})"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The invariant, checked against every variant rather than trusted: a
    /// reason that carries the path is one the window will print twice.
    #[test]
    fn no_reason_repeats_the_path() {
        let path = Path::new("/a/very/distinctive/path/to/an/image.vhdx");
        let errors = [
            ScanError::Root {
                path: path.to_path_buf(),
                source: io::Error::from(io::ErrorKind::PermissionDenied),
            },
            ScanError::NotADirectory(path.to_path_buf()),
            ScanError::ImageNotRead(path.to_path_buf()),
            ScanError::Image {
                path: path.to_path_buf(),
                reason: crate::tree::Unavailable::NamedFilesystem("btrfs"),
            },
            ScanError::Strategy(crate::tree::Unavailable::NotElevated),
            ScanError::Cancelled,
            ScanError::TooManyEntries {
                reached: 5_000_000_000,
                max: 4_294_967_295,
            },
        ];
        for error in errors {
            let reason = error.reason();
            assert!(
                !reason.contains("image.vhdx"),
                "{error:?} put the path in its reason: {reason}"
            );
            assert!(!reason.is_empty(), "{error:?} has no reason");
            // And `Display` still does, because the command line needs it.
            if !matches!(
                error,
                ScanError::Strategy(_) | ScanError::Cancelled | ScanError::TooManyEntries { .. }
            ) {
                assert!(
                    error.to_string().contains("image.vhdx"),
                    "{error:?} lost the path from its one-line form"
                );
            }
        }
    }

    /// The one that matters most, spelt out: the name of the filesystem is the
    /// whole of what the window has to show.
    #[test]
    fn a_named_filesystem_reads_as_its_own_sentence() {
        let error = ScanError::Image {
            path: PathBuf::from("/x.img"),
            reason: crate::tree::Unavailable::NamedFilesystem("btrfs"),
        };
        assert_eq!(
            error.reason(),
            "the disk image holds btrfs, which kosha does not read"
        );
    }
}
