// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Path shapes that differ between platforms.

use std::path::PathBuf;

/// Strip Windows' verbatim `\\?\` prefix from a canonicalised path.
///
/// `Path::canonicalize` returns `\\?\C:\Users\me` on Windows. That form is
/// correct and works with every API, but it is wrong in two ways that matter:
///
/// * Users do not recognise it. A breadcrumb reading `\\?\C:\ ▸ Users` is
///   noise in front of the only part anyone reads.
/// * It breaks textual prefix comparison. `\\?\C:\Windows` does **not**
///   `starts_with("C:\Windows")`, so any deny-list or scope check written the
///   obvious way silently matches nothing.
///
/// The verbatim UNC form is shortened too: `\\?\UNC\server\share` is
/// `\\server\share`, which is the spelling people type and the one Explorer
/// shows. This was missed at first because the rule was written as "UNC paths
/// have no shorter spelling" — true of `\\server\share`, and not of the
/// verbatim form of it. Scanning a WSL distribution is where it showed: the
/// breadcrumb read `\\?\UNC\wsl.localhost\archlinux\etc`.
///
/// Device paths are still returned unchanged. `\\.\PhysicalDrive0` names
/// something no shorter spelling would.
///
/// On Unix this is the identity function — `canonicalize` already returns the
/// plain form.
pub fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};

        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return path;
        };
        let mut out = match prefix.kind() {
            Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", letter as char)),
            Prefix::VerbatimUNC(server, share) => {
                let mut unc = PathBuf::from(r"\\");
                unc.push(server);
                unc.push(share);
                unc
            }
            _ => return path,
        };
        for component in components {
            if !matches!(component, Component::RootDir) {
                out.push(component.as_os_str());
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// Canonicalise a path into its plainest correct form.
///
/// Falls back to the path as given when it cannot be resolved, because a scan
/// of something unusual should still run and report what it can.
pub fn canonical(path: &std::path::Path) -> PathBuf {
    path.canonicalize()
        .map(simplify)
        .unwrap_or_else(|_| path.to_path_buf())
}

/// A relative path in the spelling exports and comparisons use: `/`-separated,
/// whatever the platform wrote it.
///
/// That normalisation is the whole reason an export taken on Windows diffs
/// cleanly against one taken on Linux over the same tree.
///
/// **Only on Windows.** A backslash is a perfectly legal character in a Unix
/// filename, and rewriting it there does not normalise anything: it invents a
/// directory level that does not exist, so the row contradicts its own `name`
/// column and names a file nothing will ever have. `--since` then compares
/// against a path no scan can produce. It is safe on Windows for the one
/// reason that matters — the filesystem forbids the character in a name, so
/// every backslash in a Windows path really is a separator.
///
/// Lives here, shared, because the exporter and the comparer have to agree on
/// it exactly. They each had their own copy of the substitution, which is two
/// chances to fix one of them.
pub fn slashed(text: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) && text.contains('\\') {
        std::borrow::Cow::Owned(text.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn drops_the_verbatim_disk_prefix() {
        let simplified = simplify(PathBuf::from(r"\\?\C:\Users\me\Downloads"));
        assert_eq!(simplified, PathBuf::from(r"C:\Users\me\Downloads"));
        assert!(simplified.starts_with(r"C:\Users"));
    }

    #[cfg(windows)]
    #[test]
    fn leaves_a_bare_drive_usable() {
        assert_eq!(simplify(PathBuf::from(r"\\?\C:\")), PathBuf::from(r"C:\"));
    }

    /// The spelling Explorer shows and a person types, out of the one
    /// `canonicalize` returns. A WSL distribution is the everyday case:
    /// `\\wsl.localhost\archlinux` canonicalises to the verbatim form, and the
    /// breadcrumb was showing it.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_unc_path_gets_its_ordinary_spelling_back() {
        assert_eq!(
            simplify(PathBuf::from(r"\\?\UNC\server\share\file")),
            PathBuf::from(r"\\server\share\file")
        );
        assert_eq!(
            simplify(PathBuf::from(r"\\?\UNC\wsl.localhost\archlinux\etc")),
            PathBuf::from(r"\\wsl.localhost\archlinux\etc")
        );
    }

    /// A plain UNC path really is already its own shortest spelling.
    #[cfg(windows)]
    #[test]
    fn a_plain_unc_path_is_left_alone() {
        let unc = PathBuf::from(r"\\server\share\file");
        assert_eq!(simplify(unc.clone()), unc);
    }

    /// A device path names something no shorter spelling would.
    #[cfg(windows)]
    #[test]
    fn a_device_path_is_left_alone() {
        let device = PathBuf::from(r"\\.\PhysicalDrive0");
        assert_eq!(simplify(device.clone()), device);
    }

    #[cfg(windows)]
    #[test]
    fn leaves_plain_paths_alone() {
        let plain = PathBuf::from(r"C:\already\plain");
        assert_eq!(simplify(plain.clone()), plain);
    }

    #[cfg(not(windows))]
    #[test]
    fn is_the_identity_on_unix() {
        let path = PathBuf::from("/home/me/Downloads");
        assert_eq!(simplify(path.clone()), path);
    }

    #[test]
    fn a_path_already_using_slashes_is_left_exactly_alone() {
        assert_eq!(slashed("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(slashed(""), "");
        assert!(
            matches!(slashed("a/b"), std::borrow::Cow::Borrowed(_)),
            "the common case should not allocate"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_separators_become_slashes() {
        assert_eq!(slashed(r"a\b\c.txt"), "a/b/c.txt");
        assert_eq!(slashed(r"one\two"), "one/two");
    }

    /// The bug this function exists for. A Unix filename may contain a
    /// backslash, and rewriting it fabricates a directory: the export said
    /// `real/back/slash.txt` for a file whose own `name` column read
    /// `back\slash.txt`, so the row contradicted itself and named a path no
    /// scan could ever produce again.
    #[cfg(not(windows))]
    #[test]
    fn a_backslash_in_a_unix_name_is_a_character_not_a_separator() {
        assert_eq!(slashed(r"real/back\slash.txt"), r"real/back\slash.txt");
        assert_eq!(slashed(r"back\slash.txt"), r"back\slash.txt");
    }
}
