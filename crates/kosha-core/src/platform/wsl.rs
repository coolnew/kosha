// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Filesystem boundaries Windows cannot see, inside a WSL distribution.
//!
//! Windows publishes every WSL distribution's Linux filesystem at
//! `\\wsl.localhost\<name>` through the 9P redirector, and kosha scans it. What
//! comes back through that redirector is one flat filesystem: the mounts a
//! Linux kernel knows about are not visible, and there is no `st_dev` to
//! compare, so `cross_filesystems: false` — the promise that a scan of a root
//! does not wander off it — is quietly not kept.
//!
//! It is not a small difference. Scanning `\\wsl.localhost\alpine-musl` whole
//! took 1m 49s and 660,914 entries, and reported **128 TiB on disk**, because
//! `/proc` is a pseudo filesystem whose files have enormous nominal lengths.
//! `/mnt` is worse than wrong: it holds the DrvFs mounts of every Windows
//! drive, so a scan of a Linux distribution walks back out onto C:\ and J:\
//! over 9P and measures the whole machine again, slowly, in the wrong units.
//!
//! A Linux kernel would have refused all of that on `st_dev` alone. This is
//! how the same refusal is made from the Windows side: the mount points are
//! not discoverable from here, but they are *known* — every WSL2 distribution
//! has the same ones in the same places, because WSL puts them there.
//!
//! With them declined, the same scan reports 17.7 GiB across 301,054 entries,
//! against the 299,729 that `find / -xdev` counts from inside the distribution
//! — the same tree, measured two ways, agreeing.
//!
//! What is left is the redirector's own price, and it is steep: that scan takes
//! 95 s from Windows against 0.9 s natively, a hundredfold. It is not kosha's
//! to give back — `cmd dir /s /b` over the same path takes 287 s for the same
//! work, so the parallel walk is already 2.6x faster than the platform's own
//! walker. Getting closer to the native figure means reading the ext4 image
//! directly, which is a different feature with different privileges.
//!
//! Entries named here are still listed, with their own size, flagged as the
//! boundary they are. That is the existing behaviour for an NTFS volume mount
//! point, and quick look already explains it: "On another filesystem, so the
//! scan stopped here."
//!
//! Compiled on every platform so the rules stay under test on whichever
//! machine they are being edited on — the same reason `places::mounts` is. Only
//! Windows ever consults them; see `platform::opaque_boundary`.

/// Directories a WSL2 distribution always has something else mounted at.
///
/// Each is a separate filesystem on the Linux side, and each would be declined
/// by `st_dev` in a native scan:
///
/// * `proc`, `sys` — kernel pseudo filesystems. Their apparent sizes are
///   fiction, and `/proc` alone is where the 128 TiB came from.
/// * `dev` — devtmpfs. Character devices with nominal lengths.
/// * `run` — tmpfs. Memory, not disk.
/// * `mnt` — where WSL mounts the Windows drives. Descending is a loop back
///   onto the volumes kosha already lists, reached the slowest way available.
///
/// Not `tmp`: it is tmpfs on some distributions and an ordinary directory on
/// others, and a wrongly skipped `/tmp` hides real disk usage. Getting that one
/// wrong costs more than leaving it in.
const ALWAYS_MOUNTED: &[&str] = &["proc", "sys", "dev", "run", "mnt"];

/// The hosts the redirector answers on. `wsl$` is the older spelling and still
/// works; both reach the same filesystem.
const HOSTS: &[&str] = &["wsl.localhost", "wsl$"];

/// Is `name`, directly inside `dir`, a mount this scan should stop at?
///
/// True only at a distribution's root. `\\wsl.localhost\archlinux\proc` is the
/// kernel's; `\\wsl.localhost\archlinux\home\me\proc` is somebody's directory
/// that happens to share a name, and skipping it would silently lose whatever
/// is in it.
pub fn is_boundary(dir: &str, name: &str) -> bool {
    is_distribution_root(dir)
        && ALWAYS_MOUNTED
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
}

/// Is this path the root of a WSL distribution, and nothing deeper?
///
/// Accepts the verbatim spelling as well, because `Path::canonicalize` returns
/// `\\?\UNC\wsl.localhost\archlinux` and not every path reaching the scanner
/// has been through `path::simplify`.
fn is_distribution_root(dir: &str) -> bool {
    distribution_root_name(dir).is_some()
}

/// Extract the distribution name without accessing the redirector.
pub fn distribution_root_name(dir: &str) -> Option<&str> {
    let rest = dir
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| dir.strip_prefix(r"\\"))
        // A forward-slash spelling reaches the redirector too, and a user who
        // pasted one should get the same scan as one who did not.
        .or_else(|| dir.strip_prefix("//?/UNC/"))
        .or_else(|| dir.strip_prefix("//"));
    let rest = rest?;

    let mut parts = rest
        .split(['\\', '/'])
        // A trailing separator is a spelling, not a level.
        .filter(|part| !part.is_empty());

    let host = parts.next()?;
    if !HOSTS.iter().any(|known| host.eq_ignore_ascii_case(known)) {
        return None;
    }
    let name = parts.next()?;
    (name != "." && name != ".." && parts.next().is_none()).then_some(name)
}

// ------------------------------------------------- which distributions exist

// Moved here from the window, which had it first and is no longer the only
// caller: a scan that reads a distribution's disk image directly has to find
// that image, and finding it is the same registry read the sidebar was already
// doing. One copy, in the crate that owns platform facts.

/// Every installed distribution, sorted by name.
///
/// **Deliberately does not touch a distribution's own filesystem.** Everything else in
/// `places.rs` checks `is_dir` so the rail never offers something that fails
/// the moment it is clicked. Doing that here would be worse than useless: the
/// first access to `\\wsl.localhost\<name>` *starts* that distribution, so
/// probing them at launch would boot every stopped WSL virtual machine on the
/// box, silently, in order to draw a sidebar. The registry says which are
/// installed; starting one is the user's decision, made by clicking.
///
/// The disk image is a different matter and *is* checked: it is an ordinary
/// file on an ordinary Windows drive, and looking at it wakes nothing.
pub fn distributions() -> Vec<Distribution> {
    let mut found = platform::installed();
    // The registry enumerates by GUID, which is arbitrary and would reshuffle
    // the rail whenever a distribution was installed or removed.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.dedup_by(|a, b| a.name == b.name);
    found
}

/// An installed distribution: what it is called, and where Windows keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Distribution {
    pub name: String,
    pub version: Option<u32>,
    /// The virtual disk holding its filesystem, when the registry says where
    /// to look and the file is really there.
    ///
    /// Always `ext4.vhdx` under the registry's `BasePath`, never any other
    /// image beside it. A distribution can have further disks attached — this
    /// machine has a `clickhouse-data.vhdx` next to one — but those are
    /// separate volumes the user mounted deliberately, they do not appear
    /// under the path kosha scans, and counting them towards the
    /// distribution's size would be a claim with nothing behind it.
    ///
    /// Checked on disk, unlike the distribution's own filesystem: this is an
    /// ordinary file on an ordinary Windows drive, and looking at it wakes
    /// nothing.
    pub image: Option<std::path::PathBuf>,
}

/// The file name WSL gives a distribution's root disk.
///
/// Declared unconditionally, like `places::mounts`, so the fact lives in one
/// place and the test that checks it runs on whichever machine this is being
/// edited on. Only the Windows path consults it, so unused everywhere else is
/// the intended state rather than an oversight.
#[cfg_attr(not(windows), allow(dead_code))]
pub const IMAGE: &str = "ext4.vhdx";

/// The path Explorer uses, and the one kosha scans.
///
/// `PathBuf::from` of the whole string rather than pushing components: a
/// distribution name is a registry value, and one containing a separator would
/// otherwise silently name a different place.
pub fn path_of(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(r"\\wsl.localhost\{name}"))
}

/// Scan the Linux root for the Windows WSL client and write one snapshot to stdout.
#[cfg(target_os = "linux")]
pub fn write_native_snapshot(
    options: crate::ScanOptions,
    mut output: impl std::io::Write,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    if options.cross_filesystems || options.use_image || options.use_mft {
        return Err(std::io::Error::other(
            "native WSL only supports a single-filesystem directory walk",
        )
        .into());
    }
    let mut tree = crate::Scanner::new(options)
        .with_cancel(cancel)
        .without_wsl_mnt()
        .scan(std::path::Path::new("/"))?;
    if tree.stats().cancelled {
        return Err(crate::ScanError::Cancelled.into());
    }
    if tree.root_unreadable() {
        return Err(std::io::Error::other("the Linux root could not be read").into());
    }
    tree.stats.strategy = crate::Strategy::WslNative;
    let meta = crate::snapshot::Meta::of(&tree, std::time::SystemTime::now(), "Native WSL scan");
    crate::snapshot::write(&tree, &meta, &mut output)?;
    output.flush()?;
    Ok(())
}

/// Run the selected distribution's installed CLI without invoking a shell.
/// The returned tree is a measurement for browsing and must not authorize file operations.
#[cfg(windows)]
pub fn scan_native(
    path: &std::path::Path,
    options: crate::ScanOptions,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<crate::SizeTree, crate::ScanError> {
    use std::io;
    let failed = |source| crate::ScanError::Root {
        path: path.to_path_buf(),
        source,
    };
    let name = path
        .to_str()
        .and_then(distribution_root_name)
        .ok_or_else(|| failed(io::Error::other("select a whole WSL2 distribution")))?;
    if options.cross_filesystems {
        return Err(failed(io::Error::other(
            "native WSL scans stay on the Linux root filesystem; turn off crossing filesystems",
        )));
    }
    let distro = distributions()
        .into_iter()
        .find(|distro| distro.name.eq_ignore_ascii_case(name) && distro.version == Some(2))
        .ok_or_else(|| failed(io::Error::other("select a registered WSL2 distribution")))?;
    let root = path_of(&distro.name);
    let mut tree = native_walk(&distro.name, options, cancel).map_err(|error| {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            crate::ScanError::Cancelled
        } else {
            failed(error)
        }
    })?;
    tree.root_path = root;
    Ok(tree)
}

#[cfg(windows)]
fn native_walk(
    distro: &str,
    options: crate::ScanOptions,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::io::Result<crate::SizeTree> {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::{atomic::Ordering, mpsc};
    use std::time::{Duration, Instant};

    let windows = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
    let mut command = Command::new(PathBuf::from(windows).join("System32/wsl.exe"));
    command.args([
        "--distribution",
        distro,
        "--cd",
        "/",
        "--exec",
        "kosha",
        "wsl-native-snapshot-v1",
    ]);
    if let Some(depth) = options.max_depth {
        command.arg("--depth").arg(depth.to_string());
    }
    if let Some(dedup) = options.dedup_hardlinks {
        command.arg(if dedup {
            "--hardlinks"
        } else {
            "--no-hardlinks"
        });
    }
    if options.collect_owners {
        command.arg("--owners");
    }
    if options.exact_allocated {
        command.arg("--exact");
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::other("native WSL scan cancelled"));
    }
    let mut child = command
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // Closing the control pipe also asks the Linux worker to stop. Killing
    // the Windows launcher alone need not stop its Linux process.
    let control = child.stdin.take().expect("stdin was piped");
    let output = child.stdout.take().expect("stdout was piped");
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("wsl-snapshot".into())
        .spawn(move || {
            let limits = crate::snapshot::ReadLimits {
                max_wire_bytes: 256 << 20,
                max_body_bytes: 768 << 20,
                max_nodes: 8_000_000,
                max_name_bytes: 256 << 20,
            };
            let _ = send.send(crate::snapshot::read_limited(output, limits));
        });
    let deadline = Instant::now() + Duration::from_secs(900);
    let result = (|| {
        reader?;
        let checkpoint = || {
            if cancel.load(Ordering::Relaxed) {
                Err(io::Error::other("native WSL scan cancelled"))
            } else if Instant::now() >= deadline {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native WSL scan timed out",
                ))
            } else {
                Ok(())
            }
        };
        let poll = Duration::from_millis(50);
        let decoded = loop {
            checkpoint()?;
            match receive.recv_timeout(poll) {
                Ok(decoded) => break decoded,
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("native WSL snapshot reader stopped"));
                }
            }
        };
        let (meta, tree) = decoded.map_err(|error| io::Error::other(format!(
            "could not read the native WSL scan: {error}. Install a matching Linux Kosha CLI on the distribution's PATH (https://coolnew.github.io/kosha/manual/scanning/#native-wsl2-scans)"
        )))?;
        let status = loop {
            checkpoint()?;
            if let Some(status) = child.try_wait()? {
                break status;
            }
            std::thread::sleep(poll);
        };
        if !status.success()
            || meta.root.as_os_str() != "/"
            || tree.stats().strategy != crate::Strategy::WslNative
            || tree.stats().cross_filesystems
            || tree.root_unreadable()
        {
            return Err(io::Error::other(
                "the Linux Kosha CLI did not return a valid root scan",
            ));
        }
        Ok(tree)
    })();
    drop(control);
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
        RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };

    /// Where WSL keeps one subkey per distribution, each named by a GUID.
    const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    /// `State` meaning installed and usable, rather than mid-install or
    /// mid-uninstall. Offering a half-installed distribution would give the
    /// user a row that cannot work and no way to tell why.
    const INSTALLED: u32 = 1;

    pub fn installed() -> Vec<super::Distribution> {
        let path = wide(LXSS);
        let mut lxss: HKEY = std::ptr::null_mut();
        // SAFETY: `path` is NUL-terminated and outlives the call; `lxss` is
        // only read after the call reports success.
        let opened =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &raw mut lxss) };
        // No key at all is the ordinary state of a machine without WSL.
        if opened != 0 {
            return Vec::new();
        }

        let mut found = Vec::new();
        // Subkey names are GUIDs, so 128 characters is already generous, and a
        // stack buffer means the loop allocates nothing per distribution.
        let mut buffer = [0u16; 128];
        for index in 0.. {
            let mut len = buffer.len() as u32;
            // SAFETY: `buffer` and `len` are a matched pair in characters,
            // which is what this API counts in; it writes at most `len`.
            let status = unsafe {
                RegEnumKeyExW(
                    lxss,
                    index,
                    buffer.as_mut_ptr(),
                    &raw mut len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            // ERROR_NO_MORE_ITEMS, or anything else. Either way there is no
            // next subkey to read, and stopping is the right answer to both.
            if status != 0 {
                break;
            }

            let subkey: Vec<u16> = buffer[..len as usize]
                .iter()
                .copied()
                .chain(std::iter::once(0))
                .collect();
            if dword(lxss, &subkey, "State") != Some(INSTALLED) {
                continue;
            }
            let Some(name) = string(lxss, &subkey, "DistributionName") else {
                continue;
            };
            // `BasePath` is where WSL put the virtual disk. Absent on an entry
            // written by an older WSL, and the directory can be gone while the
            // registry entry survives, so both are checked rather than assumed.
            let image = string(lxss, &subkey, "BasePath")
                .map(|base| std::path::PathBuf::from(base).join(super::IMAGE))
                .filter(|path| path.is_file());
            let version = dword(lxss, &subkey, "Version");
            found.push(super::Distribution {
                name,
                version,
                image,
            });
        }

        // SAFETY: `lxss` came from a successful `RegOpenKeyExW` and is not
        // used again.
        unsafe { RegCloseKey(lxss) };
        found
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One REG_DWORD from `key\subkey`.
    fn dword(key: HKEY, subkey: &[u16], name: &str) -> Option<u32> {
        let name = wide(name);
        let mut value = 0u32;
        let mut len = size_of::<u32>() as u32;
        // SAFETY: both name strings are NUL-terminated and outlive the call;
        // `value` and `len` are a matched pair in bytes.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &raw mut len,
            )
        };
        (status == 0).then_some(value)
    }

    /// One REG_SZ from `key\subkey`.
    fn string(key: HKEY, subkey: &[u16], name: &str) -> Option<String> {
        let name = wide(name);
        // A distribution name is a short identifier — `wsl --install` caps it
        // well below this — so one stack buffer is enough and the two-call
        // sizing dance buys nothing. A name that somehow did not fit comes
        // back as ERROR_MORE_DATA and is skipped, which is the right outcome:
        // a row labelled with a truncated name would scan the wrong path.
        let mut buffer = [0u16; 256];
        let mut len = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: both name strings are NUL-terminated and outlive the call;
        // `buffer` and `len` are a matched pair in bytes, which is what this
        // API wants even for a wide string.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &raw mut len,
            )
        };
        if status != 0 {
            return None;
        }
        // `len` is bytes written and includes the terminator this API
        // guarantees it writes.
        let chars = (len as usize / 2).saturating_sub(1).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..chars]);
        (!text.is_empty()).then_some(text)
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn installed() -> Vec<super::Distribution> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spelling Explorer uses. Getting this wrong is not a compile error
    /// and not a panic — it is a row in the sidebar that scans nothing.
    #[test]
    fn the_path_is_the_one_windows_publishes() {
        assert_eq!(
            path_of("archlinux"),
            std::path::PathBuf::from(r"\\wsl.localhost\archlinux")
        );
    }

    /// A name is one component, whatever is in it. A distribution called
    /// `a\b` must not turn into a path two levels deep under a distribution
    /// called `a` — that is a different place, and it might exist.
    #[test]
    fn a_name_is_never_split_into_components() {
        let path = path_of(r"odd\name");
        assert!(
            path.to_string_lossy().ends_with(r"wsl.localhost\odd\name"),
            "{}",
            path.display()
        );
        // Whatever the platform makes of it, it is built from the one string
        // rather than assembled, so it cannot escape the redirector root.
        assert!(path.to_string_lossy().starts_with(r"\\wsl.localhost\"));
    }

    /// Reading the list must be cheap enough to do at startup and on every
    /// refresh, and must never start a distribution. The second half cannot be
    /// asserted from here; the first can, and a regression to spawning
    /// `wsl.exe` would show up immediately as a jump in this number.
    #[test]
    fn listing_is_cheap_enough_to_do_at_startup() {
        let start = std::time::Instant::now();
        let found = distributions();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "listing {} distributions took {elapsed:?}",
            found.len()
        );
    }

    /// Sorted and without repeats, so the rail does not reshuffle between runs
    /// over an order that is the registry's business rather than ours.
    #[test]
    fn the_list_is_ordered_and_unique() {
        let found = distributions();
        let mut expected = found.clone();
        expected.sort_by(|a, b| a.name.cmp(&b.name));
        expected.dedup_by(|a, b| a.name == b.name);
        assert_eq!(found, expected);
    }

    /// The image is the root disk and nothing else beside it. A distribution
    /// with a second disk attached — this machine has one — must not have that
    /// disk counted towards its size: it is a separate volume the user mounted
    /// deliberately, and it does not appear under the path kosha scans.
    #[test]
    fn the_image_is_always_the_root_disk() {
        for distribution in distributions() {
            let Some(image) = &distribution.image else {
                continue;
            };
            assert_eq!(
                image.file_name().and_then(|name| name.to_str()),
                Some(IMAGE),
                "{} points at something other than its root disk",
                distribution.name
            );
            assert!(
                image.is_file(),
                "{} names a file that is not there",
                distribution.name
            );
        }
    }

    /// On a machine without WSL — every Linux and macOS build, and plenty of
    /// Windows ones — the honest answer is an empty list rather than a guess.
    #[cfg(not(windows))]
    #[test]
    fn there_is_nothing_to_redirect_to_off_windows() {
        assert!(distributions().is_empty());
    }

    #[test]
    fn the_kernel_mounts_at_a_distribution_root_are_boundaries() {
        for name in ["proc", "sys", "dev", "run", "mnt"] {
            assert!(
                is_boundary(r"\\wsl.localhost\archlinux", name),
                "{name} was walked into"
            );
        }
    }

    /// The bug this exists to prevent, named. A scan that descends here walks
    /// back onto every Windows drive through the slowest path available.
    #[test]
    fn mnt_is_a_boundary_because_it_is_the_windows_drives() {
        assert!(is_boundary(r"\\wsl.localhost\alpine-musl", "mnt"));
    }

    /// Only at the root. Somebody's `~/projects/proc` is theirs.
    #[test]
    fn the_same_name_deeper_down_is_an_ordinary_directory() {
        assert!(!is_boundary(r"\\wsl.localhost\archlinux\home", "proc"));
        assert!(!is_boundary(r"\\wsl.localhost\archlinux\home\me", "dev"));
        assert!(!is_boundary(r"\\wsl.localhost\archlinux\usr", "run"));
    }

    #[test]
    fn ordinary_directories_at_the_root_are_left_alone() {
        for name in ["usr", "home", "var", "etc", "opt", "srv", "root", "tmp"] {
            assert!(
                !is_boundary(r"\\wsl.localhost\archlinux", name),
                "{name} was skipped"
            );
        }
    }

    /// `/tmp` is deliberately not in the list: tmpfs on some distributions and
    /// a real directory on others, and skipping real disk usage is the more
    /// expensive mistake.
    #[test]
    fn tmp_is_left_in_on_purpose() {
        assert!(!is_boundary(r"\\wsl.localhost\archlinux", "tmp"));
    }

    #[test]
    fn both_redirector_spellings_are_recognised() {
        assert!(is_boundary(r"\\wsl$\archlinux", "proc"));
        assert!(is_boundary(r"\\WSL.LOCALHOST\archlinux", "proc"));
        assert!(is_boundary(r"\\?\UNC\wsl.localhost\archlinux", "proc"));
        assert!(is_boundary("//wsl.localhost/archlinux", "proc"));
    }

    #[test]
    fn a_trailing_separator_is_a_spelling_not_a_level() {
        assert!(is_boundary(r"\\wsl.localhost\archlinux\", "proc"));
    }

    /// The redirector root itself is not a distribution, and nothing under any
    /// other server is one either.
    #[test]
    fn nothing_outside_a_distribution_root_matches() {
        assert!(!is_boundary(r"\\wsl.localhost", "archlinux"));
        assert!(!is_boundary(r"\\fileserver\share", "proc"));
        assert!(!is_boundary(r"C:\Users\me", "proc"));
        assert!(!is_boundary("/home/me", "proc"));
        assert!(!is_boundary("", "proc"));
    }

    /// A host that merely starts with the right letters is a different server.
    #[test]
    fn a_lookalike_host_is_not_the_redirector() {
        assert!(!is_boundary(
            r"\\wsl.localhost.evil.example\archlinux",
            "proc"
        ));
        assert!(!is_boundary(r"\\notwsl$\archlinux", "proc"));
    }
}
