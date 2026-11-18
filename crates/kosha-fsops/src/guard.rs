// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Refusals that hold regardless of mode.
//!
//! Read-write mode says the user is allowed to change *their* files. It does
//! not say they are allowed to delete `C:\Windows` by clicking a card in a
//! panel. These checks are not warnings and there is no override flag.

use std::path::{Component, Path, PathBuf};

use kosha_core::path::simplify;

use crate::Action;

#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("`{path}` is outside the scanned root `{root}`")]
    OutsideRoot { path: PathBuf, root: PathBuf },

    #[error("`{0}` is a filesystem root")]
    FilesystemRoot(PathBuf),

    #[error("`{0}` is a protected system location")]
    Protected(PathBuf),

    #[error("`{0}` is the home directory itself")]
    HomeDirectory(PathBuf),

    #[error("`{0}` contains the running program")]
    OwnInstallation(PathBuf),

    #[error("`{path}` could not be resolved: {source}")]
    Unresolvable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "`{0}` is too long for this machine's Recycle Bin ({1} characters, and the \
         limit is 259 unless long paths are enabled) — a permanent delete would \
         work, and would say so"
    )]
    TooDeepToTrash(PathBuf, usize),

    #[error(
        "`{0}` is on a network location, which the Recycle Bin does not serve — \
         a permanent delete would work, and would say so"
    )]
    NoBinOnThisVolume(PathBuf),

    #[error(
        "`{path}` has a name Windows cannot ask for — `{name}` ends in a {what}, \
         and every way of naming a file drops it, so acting on this would act on \
         something else"
    )]
    UnnameableOnWindows {
        path: PathBuf,
        name: String,
        what: &'static str,
    },

    #[error(
        "`{0}` is named after a device Windows keeps in every directory, and \
         asking for it by name reaches the device instead of the file — kosha \
         can read it and cannot act on it"
    )]
    DeviceName(PathBuf),
}

/// Locations that are refused outright.
///
/// Prefix matching, so anything beneath them is refused too. These are the
/// directories where a deletion is not a mistake the user recovers from by
/// re-downloading something.
///
/// **A drive letter is a guess, and this list was made of them.** It is the
/// right guess on nearly every machine and the wrong one on machines that
/// exist: a second Windows in a multi-boot install gets whatever letter was
/// free, a recovery environment is usually `X:`, and `%ProgramFiles%` on a
/// machine short of room is quite often on another disk deliberately. Where
/// the list is wrong it does not misfire, it protects *nothing* — silently,
/// in the one place the safety model says a mistake cannot be undone. So the
/// machine is asked, and these remain the answer for a machine that will not
/// say.
#[cfg(windows)]
const PROTECTED_FALLBACK: &[&str] = &[
    r"C:\Windows",
    r"C:\Program Files",
    r"C:\Program Files (x86)",
    r"C:\ProgramData",
];

/// The variables that say where those are on this machine.
///
/// `windir` and `SystemRoot` have named the same directory since NT and both
/// are asked for anyway: a duplicate costs one entry in a `Vec`, and a missing
/// one costs the whole guard.
#[cfg(windows)]
const FROM_ENVIRONMENT: &[&str] = &[
    "SystemRoot",
    "windir",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
];

/// Directories a Windows *volume* has, on whichever volume that is.
///
/// These were in the list above with `C:` in front of them, which is how
/// `J:\$Recycle.Bin` on a machine with six volumes came back from `check` as
/// `Ok`. Matched as the first component after the root, so every drive and
/// every share is covered, and so is everything inside one.
///
/// The recycle bin is the pointed one: `Action::Trash` *moves files into it*,
/// so a guard that allows its deletion undermines the one action this model
/// calls recoverable. `System Volume Information` holds the volume's shadow
/// copies, which is where a restore point of the user's own files lives. It is
/// unreadable to an ordinary account, so it was being refused by an access
/// denial rather than by a rule — and an elevated read-write session, which
/// the safety model names as the one irreversible combination, is exactly
/// where that accident stops happening.
#[cfg(windows)]
const PER_VOLUME: &[&str] = &["$recycle.bin", "system volume information"];

#[cfg(target_os = "macos")]
const PROTECTED: &[&str] = &[
    "/System",
    "/Library",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/var",
    "/Applications",
    "/private/var/db",
];

#[cfg(all(unix, not(target_os = "macos")))]
const PROTECTED: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/etc", "/proc", "/sys", "/dev", "/var/lib",
];

/// The protected locations, as this machine spells them.
#[cfg(windows)]
fn protected() -> &'static [PathBuf] {
    static PROTECTED: std::sync::LazyLock<Vec<PathBuf>> =
        std::sync::LazyLock::new(|| protected_from(|name: &str| std::env::var_os(name)));
    &PROTECTED
}

/// Built from a lookup rather than from the environment directly, so that the
/// machine this runs on is not the only one it can be tested against.
#[cfg(windows)]
fn protected_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = PROTECTED_FALLBACK.iter().map(PathBuf::from).collect();
    for name in FROM_ENVIRONMENT {
        let Some(value) = var(name) else { continue };
        let path = PathBuf::from(value);
        // An environment can hold anything. A relative path is not a system
        // location, and an empty one would make `is_under` match every path
        // there is.
        if !path.is_absolute() {
            continue;
        }
        if !out
            .iter()
            .any(|had| is_under(had, &path) && is_under(&path, had))
        {
            out.push(path);
        }
    }
    out
}

/// Nothing to ask on Unix: these are the filesystem hierarchy rather than an
/// install location, and no variable names them.
#[cfg(not(windows))]
fn protected() -> &'static [PathBuf] {
    static PROTECTED_PATHS: std::sync::LazyLock<Vec<PathBuf>> =
        std::sync::LazyLock::new(|| PROTECTED.iter().map(PathBuf::from).collect());
    &PROTECTED_PATHS
}

/// Validate one path before anything touches it.
///
/// Resolution happens first: a path is only judged after symlinks and `..` have
/// been collapsed, so nothing escapes the root by spelling itself cleverly.
pub fn check(path: &Path, root: &Path) -> Result<PathBuf, Refusal> {
    check_for(path, root, None)
}

/// [`check`], for an action that has its own refusals.
///
/// Only one does: sending something to the Recycle Bin is not always possible,
/// and where it is not, the shell deletes the file instead of refusing. See
/// [`too_deep_to_trash`].
pub fn check_for(path: &Path, root: &Path, action: Option<Action>) -> Result<PathBuf, Refusal> {
    // Before anything resolves it, because resolving is what loses it.
    #[cfg(windows)]
    if let Some((name, what)) = unnameable(path) {
        return Err(Refusal::UnnameableOnWindows {
            path: path.to_path_buf(),
            name,
            what,
        });
    }

    let resolved = match resolve(path) {
        Ok(resolved) => resolved,
        #[cfg(windows)]
        Err(refusal) => return Err(name_the_device(path, refusal)),
        #[cfg(not(windows))]
        Err(refusal) => return Err(refusal),
    };
    let root = resolve(root).unwrap_or_else(|_| root.to_path_buf());

    if !resolved.starts_with(&root) {
        return Err(Refusal::OutsideRoot {
            path: resolved,
            root,
        });
    }

    if is_filesystem_root(&resolved) {
        return Err(Refusal::FilesystemRoot(resolved));
    }

    for location in protected() {
        if is_under(&resolved, location) {
            return Err(Refusal::Protected(resolved));
        }
    }

    #[cfg(windows)]
    if is_volume_metadata(&resolved) {
        return Err(Refusal::Protected(resolved));
    }

    if let Some(home) = home_dir()
        && resolved == home
    {
        return Err(Refusal::HomeDirectory(resolved));
    }

    if let Some(dir) = own_directory()
        && resolved.starts_with(&dir)
    {
        return Err(Refusal::OwnInstallation(resolved));
    }

    #[cfg(windows)]
    if action == Some(Action::Trash)
        && let Some(length) = too_deep_to_trash(&resolved)
    {
        return Err(Refusal::TooDeepToTrash(resolved, length));
    }
    #[cfg(windows)]
    if action == Some(Action::Trash) && no_bin_on_this_volume(&resolved) {
        return Err(Refusal::NoBinOnThisVolume(resolved));
    }
    #[cfg(not(windows))]
    let _ = action;

    Ok(resolved)
}

/// Would sending this to the Recycle Bin actually delete it?
///
/// **Yes, on a default Windows, for any path past `MAX_PATH`.** The shell does
/// the recycling, through `IFileOperation` and by path, and a path it cannot
/// parse is one it cannot record in the bin's own bookkeeping — so with the
/// confirmation prompts suppressed, which is what an unattended API call does, it
/// deletes the file outright and reports success. Measured on a clean Windows in
/// a sandbox: a file 315 characters deep left its directory, `trash::delete`
/// returned `Ok`, and nothing of it existed anywhere under `C:\$Recycle.Bin` or in
/// the shell's listing of it. On a machine with `LongPathsEnabled` set — this
/// developer's — the same file is recycled and restorable, which is why it took a
/// clean machine to see.
///
/// So the item is refused instead, by name, with the length in the message. The
/// alternative is an action the whole safety model calls reversible doing the one
/// thing it promises not to: `Action::Trash` is labelled recoverable in the
/// confirmation dialog, counted as `bytes_trashed` rather than freed, and written
/// to the audit log as `reversible: true`. All three would have been false.
///
/// A directory is judged by *its own* path, not by what is under it: the bin
/// records the item it was given, so a short-named folder full of deep files
/// recycles whole and is not refused.
#[cfg(windows)]
fn too_deep_to_trash(path: &Path) -> Option<usize> {
    // `MAX_PATH` is 260 including the NUL, so 259 characters is the most a path
    // can be.
    const LIMIT: usize = 259;
    let length = path.as_os_str().len();
    (length > LIMIT && !kosha_core::long_paths_enabled()).then_some(length)
}

/// Is this somewhere the Recycle Bin does not reach?
///
/// **A network location, which is every path that resolves to a UNC one.** The
/// bin is per-volume and local: a share has nowhere to put the item and no
/// bookkeeping to record it in, so the shell does not recycle from one at all.
///
/// Measured, on `\\wsl.localhost\archlinux\tmp` — a real share this tool is
/// expected to be pointed at when scanning a WSL distribution.
/// `trash::delete` came back with the shell's own
/// `0x80070002`, *"The system cannot find the file specified"*, about a file
/// that was sitting right there. Nothing was lost, which is the important part;
/// what was wrong was the account of it. A mapped drive letter is the same
/// case and needs no separate test: `W:` resolved to `\\wsl.localhost\...`
/// before this check ever saw it, which is `resolve` doing its job.
///
/// Said before the plan runs rather than after it fails, which is the pattern
/// [`too_deep_to_trash`] set: the dialog then offers a permanent delete instead
/// of reporting a failure nobody can act on.
///
/// Deliberately not extended to removable drives. Windows does not recycle from
/// most of them either, but "most" is the problem: some USB disks present as
/// fixed and do have a bin, and a guess that refuses a working trash costs
/// somebody the reversible option on the disks they care about most. There is
/// no removable drive on this machine to measure, so it is not claimed.
#[cfg(windows)]
fn no_bin_on_this_volume(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    // `\\?\UNC\server\share\…` is the verbatim spelling of the same thing;
    // `\\?\C:\…` is a local path and must not be caught by it.
    text.starts_with(r"\\?\UNC\") || (text.starts_with(r"\\") && !text.starts_with(r"\\?\"))
}

/// Canonicalise without following the final symlink.
///
/// A symlink must be removable as a link, so resolving it away would both point
/// the check at the wrong file and make the link itself un-deletable. Only the
/// parent is canonicalised.
/// Say *why* a path would not resolve, when the reason is a device.
///
/// **A file called `NUL` is refused with "Incorrect function".** Windows keeps
/// a handful of device names in every directory at once, so asking for
/// `...\cache\NUL` reaches the null device, and the null device answers a
/// request for its metadata with `ERROR_INVALID_FUNCTION`. That error is true
/// and says nothing: what a person sees is kosha listing a file, being asked
/// to remove it, and coming back with `os error 1`.
///
/// Measured, and narrower than it sounds. `CON`, `PRN`, `COM1` and `aux.txt`
/// all resolve to the real file on Windows 11, fingerprint it, delete it, and
/// leave the file beside it alone — so refusing every device name would take
/// away four things that work in order to explain one that does not. This
/// renames a failure that has already happened and never causes one.
///
/// `NotFound` is left alone, because a path that is simply not there is not a
/// device story and already says so plainly.
///
/// The file is real and another operating system made it — WSL writes into
/// trees Windows then shares, and `\\wsl.localhost\...` is a location this
/// tool expects to be pointed at.
#[cfg(windows)]
fn name_the_device(path: &Path, refusal: Refusal) -> Refusal {
    let Refusal::Unresolvable { source, .. } = &refusal else {
        return refusal;
    };
    if source.kind() == std::io::ErrorKind::NotFound {
        return refusal;
    }
    match path.file_name() {
        Some(name) if is_dos_device(name) => Refusal::DeviceName(path.to_path_buf()),
        _ => refusal,
    }
}

/// The names Windows reserves in every directory, extension and case aside.
#[cfg(windows)]
fn is_dos_device(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    // `COM1`..`COM9` and `LPT1`..`LPT9`. `COM0` is not one of them.
    matches!(stem.get(..3), Some("COM") | Some("LPT"))
        && matches!(stem.as_bytes().get(3), Some(b'1'..=b'9'))
        && stem.len() == 4
}

/// A component Win32 will not spell back to you.
///
/// **A file called `victim.bin ` is a file that can be deleted by asking for
/// `victim.bin`.** Win32 strips a trailing space or dot from every path it is
/// given, so `CreateFileW`, `symlink_metadata`, `canonicalize` and the shell
/// all reach the neighbour instead — and they do it silently, returning a
/// perfectly good answer about the wrong file. Nothing downstream can catch
/// it, because everything downstream agrees: measured, a plan aimed at a
/// 64 KiB `victim.bin ` resolved to a 1 KiB `victim.bin`, fingerprinted *that*
/// file, and so passed the check that exists to notice a file changing under
/// the plan. The dry run said it would trash `victim.bin` and quoted 64 KiB
/// for it, which is one sentence about two different files.
///
/// Such a name can only be made through a verbatim `\\?\` path or by another
/// operating system, and kosha's own scanner reads it correctly, because
/// enumerating a directory returns names rather than asking for them. Reading
/// is the whole of what kosha can honestly do with it: a permanent delete
/// through a verbatim path would work, the Recycle Bin would not, and the
/// difference between them is the one thing the safety model refuses to guess
/// at. So this is a refusal, in the shape of the two beside it.
///
/// Every component, not just the last: a directory called `logs ` redirects
/// everything under it just as thoroughly.
#[cfg(windows)]
fn unnameable(path: &Path) -> Option<(String, &'static str)> {
    path.components().find_map(|part| {
        let std::path::Component::Normal(name) = part else {
            return None;
        };
        let name = name.to_string_lossy();
        let what = match name.chars().next_back()? {
            ' ' => "space",
            '.' => "dot",
            _ => return None,
        };
        Some((name.into_owned(), what))
    })
}

fn resolve(path: &Path) -> Result<PathBuf, Refusal> {
    let unresolvable = |source| Refusal::Unresolvable {
        path: path.to_path_buf(),
        source,
    };

    let meta = std::fs::symlink_metadata(path).map_err(unresolvable)?;
    if !meta.file_type().is_symlink() {
        return path.canonicalize().map(simplify).map_err(unresolvable);
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = simplify(parent.canonicalize().map_err(unresolvable)?);
            Ok(parent.join(name))
        }
        _ => Err(unresolvable(std::io::Error::other(
            "a symlink with no parent",
        ))),
    }
}

/// Component-wise prefix test, case-insensitive on Windows.
///
/// Component-wise so `C:\Program Files` does not match `C:\Program Files Extra`,
/// and case-insensitive on Windows because `c:\windows` and `C:\Windows` name
/// the same directory and only one of them would otherwise be refused.
fn is_under(path: &Path, ancestor: &Path) -> bool {
    #[cfg(windows)]
    {
        let lowered = path.to_string_lossy().to_ascii_lowercase();
        let ancestor = ancestor.to_string_lossy().to_ascii_lowercase();
        Path::new(&lowered).starts_with(Path::new(&ancestor))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(ancestor)
    }
}

/// Is this a volume's own bookkeeping directory, or something inside one?
///
/// The first component after the root, which is the only place either of them
/// lives: `E:\$Recycle.Bin`, `\\server\share\System Volume Information`. A
/// directory somebody named `$Recycle.Bin.old` is not one, which is what
/// comparing whole components buys.
#[cfg(windows)]
fn is_volume_metadata(path: &Path) -> bool {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Prefix(_))) {
        return false;
    }
    if !matches!(components.next(), Some(Component::RootDir)) {
        return false;
    }
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    let first = first.to_string_lossy().to_ascii_lowercase();
    PER_VOLUME.contains(&first.as_str())
}

fn is_filesystem_root(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        // Unix: `/` alone.
        Some(Component::RootDir) => components.next().is_none(),
        // Windows: `C:\` is a prefix followed by a root and nothing else.
        Some(Component::Prefix(_)) => {
            matches!(components.next(), Some(Component::RootDir)) && components.next().is_none()
        }
        _ => false,
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
        .map(simplify)
}

fn own_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
        .and_then(|p| p.canonicalize().ok())
        .map(simplify)
}

#[cfg(test)]
mod tests {
    /// Which spellings of a path are somewhere the Recycle Bin cannot reach.
    ///
    /// The verbatim pair is the whole reason this is a named function rather
    /// than one `starts_with`: `\\?\UNC\server\share` and `\\?\C:\dir`
    /// both begin with two backslashes and only the first of them is a share.
    /// Refusing the second would refuse every long local path there is.
    #[cfg(windows)]
    #[test]
    fn a_share_is_told_from_a_long_local_path() {
        use std::path::Path;

        for network in [
            r"\\server\share\file.bin",
            r"\\wsl.localhost\archlinux\tmp\file.bin",
            r"\\?\UNC\server\share\file.bin",
        ] {
            assert!(
                super::no_bin_on_this_volume(Path::new(network)),
                "`{network}` is a network location"
            );
        }
        for local in [
            r"C:\Users\someone\file.bin",
            r"\\?\C:\Users\someone\file.bin",
            r"D:\file.bin",
        ] {
            assert!(
                !super::no_bin_on_this_volume(Path::new(local)),
                "`{local}` is on this machine"
            );
        }
    }

    use super::*;

    /// The list is only right if it is this machine's list.
    ///
    /// Driven through a substituted environment rather than the real one,
    /// because `set_var` is unsafe in this edition and a crate that denies
    /// `unsafe` outside one audited ioctl is not going to make an exception
    /// for a test. The fallback stays in the list: on a machine whose Windows
    /// is on `D:` there is no reason to start permitting a directory that is
    /// literally spelled `C:\Windows`.
    #[cfg(windows)]
    #[test]
    fn the_protected_locations_come_from_the_machine_and_not_from_a_drive_letter() {
        let elsewhere = |name: &str| -> Option<std::ffi::OsString> {
            match name {
                "SystemRoot" | "windir" => Some(r"D:\Windows".into()),
                "ProgramFiles" => Some(r"D:\Program Files".into()),
                "ProgramData" => Some(r"D:\ProgramData".into()),
                _ => None,
            }
        };
        let list = protected_from(elsewhere);

        for spelling in [r"D:\Windows", r"D:\Program Files", r"D:\ProgramData"] {
            assert!(
                list.iter().any(|had| had == Path::new(spelling)),
                "{spelling} is where Windows is, and is not protected: {list:?}"
            );
        }
        assert!(list.iter().any(|had| had == Path::new(r"C:\Windows")));

        // And the path a rule is asked about is the one that gets refused.
        assert!(
            list.iter()
                .any(|had| is_under(Path::new(r"d:\windows\system32\kernel32.dll"), had))
        );
    }

    /// An environment that says something useless must not widen the guard
    /// into everything or narrow it into nothing.
    #[cfg(windows)]
    #[test]
    fn an_environment_value_that_is_not_a_path_is_ignored() {
        let nonsense = |name: &str| -> Option<std::ffi::OsString> {
            match name {
                "SystemRoot" => Some("".into()),
                "windir" => Some("Windows".into()),
                "ProgramFiles" => Some(r"\Program Files".into()),
                _ => None,
            }
        };
        let list = protected_from(nonsense);

        assert_eq!(
            list.len(),
            PROTECTED_FALLBACK.len(),
            "a relative or empty value became a protected location: {list:?}"
        );
        // The empty string as an ancestor matches every path there is, which
        // is the failure this is really about: a guard that refuses the user's
        // own files is as broken as one that refuses nothing.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("junk.bin");
        std::fs::write(&file, b"x").unwrap();
        assert!(check(&file, dir.path()).is_ok());
    }

    /// Every volume has these two, and they were protected on `C:` alone.
    #[cfg(windows)]
    #[test]
    fn a_volume_s_own_directories_are_its_own_on_every_volume() {
        for spelling in [
            r"E:\$Recycle.Bin",
            r"e:\$recycle.bin\S-1-5-21-1\$RXYZ.zip",
            r"J:\System Volume Information",
            r"f:\system volume information\{3808876b-c176-4e48-b7ae-04046e6cc752}",
            r"C:\$Recycle.Bin",
            "\\\\server\\share\\$Recycle.Bin",
        ] {
            assert!(
                is_volume_metadata(Path::new(spelling)),
                "{spelling} is a volume's own directory"
            );
        }

        for spelling in [
            r"E:\$Recycle.Bin.old",
            r"E:\Users\me\$Recycle.Bin",
            r"E:\System Volume",
            r"E:\",
            r"Recycle.Bin",
        ] {
            assert!(
                !is_volume_metadata(Path::new(spelling)),
                "{spelling} is somebody's own directory"
            );
        }
    }

    /// And the same rule against the volumes this machine really has.
    ///
    /// The unit test above is the rule; this is the bug. `check` returned
    /// `Ok("J:\$RECYCLE.BIN")` on the machine this was written on, which has
    /// six volumes and a deny-list that named one of them.
    #[cfg(windows)]
    #[test]
    fn the_recycle_bin_of_a_second_volume_is_refused() {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let mut found = 0usize;
        // From C rather than A: probing a floppy letter is slow and there is
        // no recycle bin on one.
        for letter in 'C'..='Z' {
            let drive = format!("{letter}:");
            if drive.eq_ignore_ascii_case(&system) {
                continue;
            }
            let bin = PathBuf::from(format!(r"{drive}\$Recycle.Bin"));
            if !bin.exists() {
                continue;
            }
            found += 1;
            let root = PathBuf::from(format!(r"{drive}\"));
            match check(&bin, &root) {
                Err(Refusal::Protected(_)) => {}
                other => panic!("{} came back as {other:?}", bin.display()),
            }
        }
        // A machine with one volume has nothing to say here, and that is not
        // a failure — the rule is asserted by the test above on every one.
        if found == 0 {
            eprintln!("no second volume with a recycle bin on this machine");
        }
    }

    #[test]
    fn accepts_a_file_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("junk.bin");
        std::fs::write(&file, b"x").unwrap();

        assert!(check(&file, dir.path()).is_ok());
    }

    #[test]
    fn refuses_escape_from_the_root() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("root");
        let sibling = outer.path().join("sibling");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let victim = sibling.join("important.txt");
        std::fs::write(&victim, b"x").unwrap();

        // Spelled as a traversal out of the root, which is the interesting case.
        let sneaky = root.join("..").join("sibling").join("important.txt");
        match check(&sneaky, &root) {
            Err(Refusal::OutsideRoot { .. }) => {}
            other => panic!("expected OutsideRoot, got {other:?}"),
        }
    }

    #[test]
    fn refuses_a_filesystem_root() {
        #[cfg(windows)]
        let (path, root) = (Path::new(r"C:\"), Path::new(r"C:\"));
        #[cfg(not(windows))]
        let (path, root) = (Path::new("/"), Path::new("/"));

        assert!(matches!(check(path, root), Err(Refusal::FilesystemRoot(_))));
    }

    #[test]
    fn refuses_protected_system_locations() {
        #[cfg(windows)]
        let (path, root) = (Path::new(r"C:\Windows\System32"), Path::new(r"C:\"));
        #[cfg(target_os = "macos")]
        let (path, root) = (Path::new("/System/Library"), Path::new("/"));
        #[cfg(all(unix, not(target_os = "macos")))]
        let (path, root) = (Path::new("/usr/bin"), Path::new("/"));

        // Skip where the path genuinely does not exist on this machine; the
        // rule is still asserted everywhere it can be.
        if path.exists() {
            assert!(matches!(check(path, root), Err(Refusal::Protected(_))));
        }
    }

    #[cfg(windows)]
    #[test]
    fn canonicalised_paths_lose_the_verbatim_prefix() {
        // Without this, every protected-location check below is dead code.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.bin");
        std::fs::write(&file, b"x").unwrap();

        let resolved = check(&file, dir.path()).unwrap();
        assert!(
            !resolved.to_string_lossy().starts_with(r"\?\"),
            "leaked a verbatim prefix: {}",
            resolved.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn protected_matching_ignores_case() {
        assert!(is_under(
            Path::new(r"c:\windows\system32"),
            Path::new(r"C:\Windows")
        ));
        assert!(is_under(
            Path::new(r"C:\WINDOWS\System32"),
            Path::new(r"C:\Windows")
        ));
    }

    #[test]
    fn prefix_matching_is_component_wise() {
        // A directory that merely shares a textual prefix is not protected.
        // This held a literal tab where the raw string was meant to have
        // a separator, so the multi-component case it describes was never
        // actually exercised.
        #[cfg(windows)]
        {
            assert!(!is_under(
                Path::new(r"C:\Program Files Extra\thing"),
                Path::new(r"C:\Program Files")
            ));
            // And the matcher is not simply always false.
            assert!(is_under(
                Path::new(r"C:\Program Files\thing"),
                Path::new(r"C:\Program Files")
            ));
        }
        #[cfg(not(windows))]
        {
            assert!(!is_under(Path::new("/usrlocal/thing"), Path::new("/usr")));
            assert!(is_under(Path::new("/usr/local/thing"), Path::new("/usr")));
        }
    }

    #[test]
    fn identifies_filesystem_roots() {
        #[cfg(windows)]
        {
            assert!(is_filesystem_root(Path::new(r"C:\")));
            assert!(!is_filesystem_root(Path::new(r"C:\Users")));
        }
        #[cfg(not(windows))]
        {
            assert!(is_filesystem_root(Path::new("/")));
            assert!(!is_filesystem_root(Path::new("/home")));
        }
    }
}
