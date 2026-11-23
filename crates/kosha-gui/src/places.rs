// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! The locations worth offering in the sidebar.
//!
//! Resolved from the environment rather than a crate: this is the whole of what
//! we need, and the platform variables are stable.
//!
//! Everything here touches the filesystem — `is_dir` on each candidate, and
//! `statvfs`/`GetDiskFreeSpaceEx` on each volume — so none of it belongs in a
//! render path. The window resolves the list once and keeps it; see
//! `KoshaApp::refresh_places`.

use std::path::PathBuf;

pub struct Place {
    pub label: String,
    pub glyph: &'static str,
    pub path: PathBuf,
}

pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The user's own directories, in the order they would look for them.
///
/// Existence is checked here so the sidebar never offers a location that fails
/// the moment it is clicked.
pub fn personal() -> Vec<Place> {
    let mut places = Vec::new();

    if let Some(home) = home() {
        places.push(Place {
            label: "Home".into(),
            glyph: "⌂",
            path: home.clone(),
        });
        for (label, glyph, rel) in [
            ("Downloads", "↓", "Downloads"),
            ("Documents", "▤", "Documents"),
            ("Desktop", "▭", "Desktop"),
        ] {
            let path = home.join(rel);
            if path.is_dir() {
                places.push(Place {
                    label: label.into(),
                    glyph,
                    path,
                });
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        places.push(Place {
            label: "This folder".into(),
            // Not `▸`, which is the breadcrumb separator in the title bar. The
            // same glyph meaning "the next part of a path" up there and "a
            // place you can scan" down here is one mark doing two jobs, and in
            // a column of pictograms it also reads as a row that expands.
            // `◎` is "the one you are standing in", which is what this row is.
            glyph: "◎",
            path: cwd,
        });
    }
    places
}

/// A mounted volume and how full it is.
pub struct Volume {
    pub place: Place,
    /// `None` when the volume cannot be asked — a card reader with no card, a
    /// disconnected network drive. The row still lists it; it simply has no
    /// bar, which is honest about not knowing rather than drawing an empty one.
    pub usage: Option<kosha_core::DiskUsage>,
}

/// Mounted volumes, listed separately so a row of drive letters does not push
/// the directories anyone actually scans off the bottom of the rail.
///
/// Uses cached mount metadata on macOS and probes each volume elsewhere.
pub fn volumes() -> Vec<Volume> {
    #[cfg(target_os = "macos")]
    let volumes = {
        let mut mounts = kosha_core::macos_mounts();
        mounts.sort_by(|a, b| a.0.cmp(&b.0));
        mounts.dedup_by(|a, b| a.0 == b.0);
        let root = PathBuf::from("/");
        let usage = kosha_core::disk_usage(&root);
        match mounts.first_mut() {
            Some((path, cached_usage)) if *path == root => *cached_usage = usage,
            _ => mounts.insert(0, (root, usage)),
        }
        mounts
    };
    #[cfg(not(target_os = "macos"))]
    let volumes = volume_roots().into_iter().map(|root| {
        let usage = kosha_core::disk_usage(&root);
        (root, usage)
    });
    volumes
        .into_iter()
        .map(|(root, usage)| Volume {
            usage,
            place: Place {
                label: root.display().to_string(),
                glyph: "▣",
                path: root,
            },
        })
        .collect()
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn macos_volumes_include_root_and_the_attached_test_volume() {
    let volumes = volumes();
    assert_eq!(volumes[0].place.path, std::path::Path::new("/"));
    if let Some(expected) = std::env::var_os("KOSHA_TEST_MOUNT") {
        let expected = PathBuf::from(expected);
        let mounted = volumes
            .iter()
            .find(|volume| volume.place.path == expected)
            .expect("the explicitly selected test mount must appear in the sidebar list");
        assert!(
            mounted.usage.is_some(),
            "the test mount should report capacity"
        );
    }
}

/// Mounted volumes.
///
/// Probing drive letters on Windows is cheap. Linux has `/proc/mounts`, which
/// is where this used to answer `/` and nothing else — so a machine with
/// `/home` on its own disk, an external drive under `/media` or a NAS under
/// `/mnt` showed a single bar, while the same machine's Windows side showed
/// six. Everywhere else the root is still the only portable answer.
#[cfg(not(target_os = "macos"))]
fn volume_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        ('C'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .filter(|path| path.is_dir())
            .collect()
    }
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string("/proc/mounts") {
            Ok(text) => mounts::worth_showing(&text),
            // Unreadable inside some sandboxes. The root is always mounted.
            Err(_) => vec![PathBuf::from("/")],
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        vec![PathBuf::from("/")]
    }
}

/// Reading `/proc/mounts`, which lists forty-odd things and means about six.
///
/// Compiled everywhere rather than behind `cfg(target_os = "linux")`, so the
/// rules stay under test on whichever machine they are being edited on — the
/// file it parses is checked into `tests/data`, and the whole point of this
/// module is that its rules are decided by reading, not by running. Only Linux
/// has a live file to feed it, hence the allow: unused on every other target is
/// the intended state, not an oversight.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod mounts {
    use std::path::PathBuf;

    /// Filesystems that never hold a user's data.
    ///
    /// `overlay` and `tmpfs` are the awkward pair. Both are usually noise — a
    /// container's layers, `/run`, `/dev/shm` — but a container's *root* is an
    /// overlay and genuinely is the disk it runs on, so they are admitted at
    /// `/` and nowhere else. A tmpfs `/tmp` stays out even though it does fill
    /// up: it is memory, and a bar for it beside the disks would be comparing
    /// two different things.
    const PSEUDO: &[&str] = &[
        "autofs",
        "binfmt_misc",
        "bpf",
        "cgroup",
        "cgroup2",
        "configfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "efivarfs",
        "fuse.gvfsd-fuse",
        "fuse.portal",
        "fusectl",
        "hugetlbfs",
        "mqueue",
        "nsfs",
        "overlay",
        "proc",
        "pstore",
        "ramfs",
        "rootfs",
        "securityfs",
        "selinuxfs",
        "squashfs",
        "sysfs",
        "tmpfs",
        "tracefs",
    ];

    /// Mount points that are the operating system's business, not a user's.
    const NOISE: &[&str] = &["/proc/", "/sys/", "/dev/", "/run/", "/snap/"];

    /// The mount points worth putting in the sidebar, in path order.
    ///
    /// Four rules, in this order:
    ///
    /// - **Pseudo filesystems go by type.** That is most of the file.
    /// - **Read-only mounts go.** The rail answers "how full is this, and what
    ///   can I clear off it", and nothing can be cleared off a read-only
    ///   mount. It also removes snap images, mounted ISOs and, incidentally,
    ///   WSL's own system mounts, without naming any of them.
    /// - **One entry per device.** A btrfs root with `/`, `/home` and `/var`
    ///   as subvolumes is one disk with one free-space figure, and three
    ///   identical bars would say otherwise. First mount wins.
    /// - **Sorted by mount point**, so the rail does not reshuffle between
    ///   runs over an order that is the kernel's business rather than ours.
    pub fn worth_showing(mounts: &str) -> Vec<PathBuf> {
        let mut devices: Vec<String> = Vec::new();
        let mut roots: Vec<PathBuf> = Vec::new();

        for line in mounts.lines() {
            let mut fields = line.split(' ');
            let (Some(device), Some(point), Some(kind), Some(options)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let point = unescape(point);

            if PSEUDO.contains(&kind) && point != "/" {
                continue;
            }
            // `ro` as a whole option rather than a prefix: `rootcontext=` and
            // `rootflags=` both begin with those two letters.
            if options.split(',').any(|option| option == "ro") {
                continue;
            }
            if NOISE.iter().any(|prefix| point.starts_with(prefix)) {
                continue;
            }

            let device = unescape(device);
            if devices.contains(&device) {
                continue;
            }
            devices.push(device);
            roots.push(PathBuf::from(point));
        }

        roots.sort();
        roots
    }

    /// `/proc/mounts` escapes space, tab, newline and backslash as octal.
    ///
    /// Without this a drive mounted at `/media/My Backup` arrives as
    /// `/media/My\040Backup`, which is not a path that exists. WSL writes its
    /// Windows drives as `C:\134`, the same escape for a backslash.
    fn unescape(field: &str) -> String {
        if !field.contains('\\') {
            return field.to_string();
        }
        let mut out = String::with_capacity(field.len());
        let mut rest = field.chars();
        while let Some(c) = rest.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let octal: String = rest.clone().take(3).collect();
            match u8::from_str_radix(&octal, 8) {
                Ok(byte) if octal.len() == 3 => {
                    out.push(byte as char);
                    for _ in 0..3 {
                        rest.next();
                    }
                }
                // A lone backslash is legal in a filename and is not an escape.
                _ => out.push('\\'),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A real `/proc/mounts`, captured from the Arch distribution these
        /// tests run against under WSL. Forty-three lines, seven of which mean
        /// anything; the rest is exactly the noise the rules exist for.
        const WSL: &str = include_str!("../tests/data/wsl-arch-proc-mounts.txt");

        fn shown(mounts: &str) -> Vec<String> {
            worth_showing(mounts)
                .into_iter()
                .map(|path| path.display().to_string())
                .collect()
        }

        #[test]
        fn a_real_mount_table_comes_down_to_the_disks() {
            assert_eq!(
                shown(WSL),
                vec![
                    "/", "/mnt/c", "/mnt/d", "/mnt/e", "/mnt/f", "/mnt/g", "/mnt/j"
                ],
                "the distribution's own root and the six Windows drives — the \
                 same six letters this machine's Windows side lists"
            );
        }

        #[test]
        fn an_ordinary_desktop_keeps_its_separate_home_and_its_external_drive() {
            let table = "\
/dev/nvme0n1p2 / ext4 rw,relatime 0 0
/dev/nvme0n1p1 /boot/efi vfat rw,relatime 0 0
/dev/nvme0n1p3 /home ext4 rw,relatime 0 0
/dev/sda1 /media/someone/Backup exfat rw,nosuid,nodev,relatime 0 0
nas:/export /mnt/nas nfs4 rw,relatime 0 0
";
            assert_eq!(
                shown(table),
                vec![
                    "/",
                    "/boot/efi",
                    "/home",
                    "/media/someone/Backup",
                    "/mnt/nas"
                ]
            );
        }

        #[test]
        fn one_bar_per_disk_not_one_per_subvolume() {
            let btrfs = "\
/dev/nvme0n1p2 / btrfs rw,relatime,subvol=/@ 0 0
/dev/nvme0n1p2 /home btrfs rw,relatime,subvol=/@home 0 0
/dev/nvme0n1p2 /var/log btrfs rw,relatime,subvol=/@log 0 0
";
            assert_eq!(
                shown(btrfs),
                vec!["/"],
                "three subvolumes of one disk share one free-space figure"
            );
        }

        #[test]
        fn a_read_only_mount_has_nothing_to_free() {
            let table = "\
/dev/sdb1 / ext4 rw,relatime 0 0
/dev/sr0 /media/cdrom iso9660 ro,relatime 0 0
/dev/loop3 /var/lib/snapd/snap/core squashfs ro,nodev 0 0
";
            assert_eq!(shown(table), vec!["/"]);
        }

        /// `rootcontext=` and `rootflags=` begin with the two letters that mean
        /// read-only, and matching on a prefix would hide the disk itself.
        #[test]
        fn read_only_is_matched_as_a_whole_option() {
            let table = "/dev/sdb1 / ext4 rw,rootcontext=system_u:object_r:root_t 0 0\n";
            assert_eq!(shown(table), vec!["/"]);
        }

        #[test]
        fn a_container_root_is_an_overlay_and_is_still_the_disk() {
            let table = "\
overlay / overlay rw,lowerdir=/l,upperdir=/u,workdir=/w 0 0
tmpfs /tmp tmpfs rw,nosuid,nodev 0 0
";
            assert_eq!(
                shown(table),
                vec!["/"],
                "the overlay at the root counts; the tmpfs is memory"
            );
        }

        #[test]
        fn escaped_paths_come_back_as_paths_that_exist() {
            assert_eq!(unescape(r"/media/me/My\040Backup"), "/media/me/My Backup");
            assert_eq!(unescape(r"C:\134"), r"C:\");
            assert_eq!(unescape("/plain/path"), "/plain/path");
            // Not an escape, and not a reason to lose the rest of the name.
            assert_eq!(unescape(r"/odd/back\slash"), r"/odd/back\slash");
        }

        #[test]
        fn a_truncated_or_empty_table_yields_nothing_rather_than_panicking() {
            assert!(shown("").is_empty());
            assert!(shown("garbage\n/dev/sda1 /mnt\n").is_empty());
        }
    }
}

/// The WSL distributions, as places.
///
/// A separate list from [`personal`] because they are a separate kind of thing,
/// and because the rail gives them their own heading — a row called `archlinux`
/// among Home, Downloads and Documents would read as a folder somebody had
/// pinned. See [`crate::wsl`] for where the names come from, and why nothing
/// here touches the disk.
pub fn distributions() -> Vec<Distribution> {
    kosha_core::wsl::distributions()
        .into_iter()
        .map(|found| Distribution {
            // What the image costs on disk, not its nominal length: WSL can
            // create a sparse virtual disk, and for one of those the length is
            // a number nobody is paying for. Falls back to the length where
            // the exact figure cannot be had, because an approximate size is
            // still far better than no size.
            image: found.image.as_deref().and_then(|path| {
                kosha_core::allocated_size(path)
                    .ok()
                    .or_else(|| path.metadata().ok().map(|meta| meta.len()))
            }),
            place: Place {
                glyph: "◱",
                path: kosha_core::wsl::path_of(&found.name),
                label: found.name,
            },
        })
        .collect()
}

/// A WSL distribution in the rail: where to scan it, and what it costs
/// Windows.
pub struct Distribution {
    pub place: Place,
    /// What its virtual disk occupies on the Windows drive holding it.
    ///
    /// The number worth showing, and the one nothing else shows. A WSL disk
    /// grows and does not shrink back on its own, so it is routinely far
    /// larger than the files inside it — this machine has one occupying
    /// 26.4 GiB for 17.7 GiB of contents. Windows reports the big number and
    /// says nothing about the small one; kosha can show both, and the gap
    /// between them is the thing somebody would act on.
    ///
    /// `None` when the registry did not say where the image is, or it is not
    /// where it said.
    pub image: Option<u64>,
}

/// The pinned folders, with whether each still exists.
///
/// Existence is checked here rather than at render time — it is a filesystem
/// call, and a pinned folder can go away: an unmounted drive, a project
/// directory that moved. The rail keeps showing it, marked, rather than
/// dropping it silently; somebody who pinned it will want to know it is gone
/// rather than wonder where the row went.
pub fn scopes(saved: &[crate::settings::SavedScope]) -> Vec<(Place, bool)> {
    saved
        .iter()
        .map(|scope| {
            let present = scope.path.is_dir();
            (
                Place {
                    label: scope.name.clone(),
                    glyph: "⚑",
                    path: scope.path.clone(),
                },
                present,
            )
        })
        .collect()
}
