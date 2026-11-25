// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Identity and measurement for historical observations of local volumes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{DiskUsage, SizeTree, Strategy};

/// A machine-local, platform-qualified identity obtained from a mounted volume.
/// A missing identity must never be replaced with the drive letter or path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VolumeId(String);

impl VolumeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve a local filesystem root. An ordinary directory, unsupported
    /// filesystem, or unavailable volume returns no identity.
    pub fn at_root(root: &Path) -> Option<Self> {
        persistent_id(root).map(Self)
    }
}

/// Start the measurement before scanning, then confirm the same volume is
/// still mounted before accepting the result.
pub struct ScanObservation {
    root: PathBuf,
    volume: VolumeId,
}

pub struct Observation<'a> {
    volume: VolumeId,
    time: SystemTime,
    usage: Option<DiskUsage>,
    tree: &'a SizeTree,
}

impl<'a> Observation<'a> {
    pub fn volume(&self) -> &VolumeId {
        &self.volume
    }

    pub fn time(&self) -> SystemTime {
        self.time
    }

    pub fn usage(&self) -> Option<DiskUsage> {
        self.usage
    }

    pub fn partial(&self) -> bool {
        self.tree.is_partial()
    }

    pub(crate) fn tree(&self) -> &'a SizeTree {
        self.tree
    }
}

impl ScanObservation {
    pub fn start(root: &Path) -> Option<Self> {
        let volume = VolumeId::at_root(root)?;
        Some(Self {
            root: crate::path::canonical(root),
            volume,
        })
    }

    pub fn finish(self, tree: &SizeTree) -> Option<Observation<'_>> {
        if !matches!(
            tree.stats().strategy,
            Strategy::DirectoryWalk | Strategy::MasterFileTable
        ) || !crate::snapshot::store::same_root(&self.root, tree.root_path())
            || tree.is_incomplete()
            || tree.root_unreadable()
            || VolumeId::at_root(&self.root).as_ref() != Some(&self.volume)
        {
            return None;
        }
        let usage = crate::disk_usage(&self.root);
        let time = SystemTime::now();
        // Do not attach a replacement volume's capacity to the old tree.
        (VolumeId::at_root(&self.root).as_ref() == Some(&self.volume)).then_some(Observation {
            volume: self.volume,
            time,
            usage,
            tree,
        })
    }

    pub fn volume(&self) -> &VolumeId {
        &self.volume
    }
}

/// Change in measured allocation under one immediate child of the volume.
/// Root-level files and space outside the scanned tree are outside this list.
#[derive(Debug, PartialEq, Eq)]
pub struct FolderGrowth {
    pub name: std::ffi::OsString,
    pub before: u64,
    pub after: u64,
}

impl FolderGrowth {
    pub fn delta(&self) -> i128 {
        i128::from(self.after) - i128::from(self.before)
    }
}

/// Rank disjoint root folders by the magnitude of their allocation change.
/// An incomplete observation cannot establish how much a directory changed.
pub fn folder_growth(before: &SizeTree, after: &SizeTree) -> Option<Vec<FolderGrowth>> {
    use crate::node::Flags;
    use std::collections::BTreeMap;

    if [before, after].iter().any(|tree| {
        tree.is_incomplete()
            || tree.is_partial()
            || tree.root_unreadable()
            || tree.names_replaced() != 0
            || tree.stats().cross_filesystems
    }) || before.stats().strategy != after.stats().strategy
    {
        return None;
    }

    let mut sizes = BTreeMap::<&std::ffi::OsStr, [u64; 2]>::new();
    for (index, tree) in [before, after].into_iter().enumerate() {
        for id in tree.children(tree.root()) {
            let child = tree.node(id);
            if child.flags.contains(Flags::DIR) {
                let size = &mut sizes.entry(tree.name(id)).or_default()[index];
                *size = size.saturating_add(child.allocated);
            }
        }
    }
    let mut changed: Vec<_> = sizes
        .into_iter()
        .filter_map(|(name, [before, after])| {
            (before != after).then(|| FolderGrowth {
                name: name.to_owned(),
                before,
                after,
            })
        })
        .collect();
    changed.sort_unstable_by(|a, b| {
        b.delta()
            .abs()
            .cmp(&a.delta().abs())
            .then_with(|| a.name.cmp(&b.name))
    });
    Some(changed)
}

#[cfg(windows)]
fn persistent_id(root: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;

    let mut path: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                unit
            }
        })
        .collect();
    if !root.is_absolute() || path.contains(&0) {
        return None;
    }
    if path.last() != Some(&u16::from(b'\\')) {
        path.push(u16::from(b'\\'));
    }
    path.push(0);
    let mut name = [0u16; 64];
    // SAFETY: path is NUL-terminated and name is a writable UTF-16 buffer.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(path.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    };
    if ok == 0 {
        return None;
    }
    let name = String::from_utf16(&name[..name.iter().position(|unit| *unit == 0)?]).ok()?;
    let name = name.to_ascii_lowercase();
    let guid = name.strip_prefix(r"\\?\volume{")?.strip_suffix("}\\")?;
    valid_uuid(guid).then(|| format!("windows:{guid}"))
}

#[cfg(target_os = "macos")]
fn persistent_id(root: &Path) -> Option<String> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    let root = root.canonicalize().ok()?;
    let path = CString::new(root.as_os_str().as_bytes()).ok()?;
    // SAFETY: statfs takes a terminated path and fills the initialized output.
    let mut mount: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &raw mut mount) } != 0 {
        return None;
    }
    // SAFETY: statfs returns a fixed, NUL-terminated mount name.
    let mount_path = unsafe { CStr::from_ptr(mount.f_mntonname.as_ptr()) };
    if mount_path != path.as_c_str() || mount.f_flags & libc::MNT_LOCAL as u32 == 0 {
        return None;
    }

    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    // getattrlist packs a 32-bit length followed by the 16 UUID bytes.
    let mut result = [0u32; 5];
    // SAFETY: the path and initialized attrlist are valid, and result is a
    // writable, aligned buffer whose byte length is passed to the syscall.
    if unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut attributes).cast(),
            result.as_mut_ptr().cast(),
            size_of_val(&result),
            0,
        )
    } != 0
        || result[0] as usize != size_of_val(&result)
        || result[1..].iter().all(|word| *word == 0)
    {
        return None;
    }
    let uuid: String = result[1..]
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(format!("macos:{uuid}"))
}

#[cfg(target_os = "linux")]
fn persistent_id(root: &Path) -> Option<String> {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let root = root.canonicalize().ok()?;
    let device = fs::metadata(&root).ok()?.dev();
    let mounts = fs::read("/proc/self/mountinfo").ok()?;
    let major_minor = format!("{}:{}", libc::major(device), libc::minor(device));
    if !whole_block_mount(&mounts, root.as_os_str().as_bytes(), major_minor.as_bytes()) {
        return None;
    }
    let mut ids = fs::read_dir("/dev/disk/by-uuid").ok()?.filter_map(|entry| {
        let entry = entry.ok()?;
        let target = fs::metadata(entry.path()).ok()?;
        let name = entry.file_name().into_string().ok()?;
        (target.file_type().is_block_device() && target.rdev() == device && valid_uuid(&name))
            .then_some(name)
    });
    let uuid = ids.next()?;
    ids.next()
        .is_none()
        .then(|| format!("linux:{}", uuid.to_ascii_lowercase()))
}

#[cfg(target_os = "linux")]
fn whole_block_mount(mounts: &[u8], root: &[u8], device: &[u8]) -> bool {
    // mountinfo uses octal escapes for space, tab, newline and backslash.
    let mut escaped = Vec::with_capacity(root.len());
    for byte in root {
        escaped.extend_from_slice(match byte {
            b' ' => b"\\040",
            b'\t' => b"\\011",
            b'\n' => b"\\012",
            b'\\' => b"\\134",
            other => std::slice::from_ref(other),
        });
    }
    let mut matching = mounts.split(|byte| *byte == b'\n').filter_map(|line| {
        let mut fields = line.split(|byte| *byte == b' ');
        let mounted_device = fields.nth(2)?;
        let mounted_root = fields.next()?;
        let mount_point = fields.next()?;
        (mount_point == escaped).then_some(mounted_device == device && mounted_root == b"/")
    });
    matching.next() == Some(true) && matching.next().is_none()
}

#[cfg(any(windows, target_os = "linux"))]
fn valid_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
        && bytes
            .iter()
            .any(|byte| byte.is_ascii_hexdigit() && *byte != b'0')
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn persistent_id(_: &Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_directory_is_not_a_volume_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ScanObservation::start(dir.path()).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_system_volume_has_a_mount_manager_guid() {
        let drive = std::env::var("SystemDrive").expect("Windows system drive");
        let root = format!("{drive}\\");
        let id = VolumeId::at_root(Path::new(&root)).expect("Windows system volume GUID");
        assert!(id.as_str().starts_with("windows:"));
        assert_eq!(VolumeId::at_root(Path::new(&format!("{drive}/"))), Some(id));
    }

    #[test]
    fn allocation_growth_ranks_disjoint_top_level_folders() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir(root.join("growing")).unwrap();
        std::fs::create_dir(root.join("shrinking")).unwrap();
        std::fs::write(root.join("shrinking/file"), vec![1; 16 * 1024]).unwrap();
        let scanner = crate::Scanner::new(crate::ScanOptions::default());
        let mut before = scanner.scan(root).unwrap();
        std::fs::remove_file(root.join("shrinking/file")).unwrap();
        std::fs::write(root.join("growing/file"), vec![2; 64 * 1024]).unwrap();
        let after = scanner.scan(root).unwrap();
        let changes = folder_growth(&before, &after).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].name, "growing");
        assert!(changes[0].delta() > 0 && changes[1].delta() < 0);
        before.mark_cancelled();
        assert!(folder_growth(&before, &after).is_none());
    }

    #[test]
    fn committed_history_is_ordered_bounded_and_separate_by_volume() {
        use crate::snapshot::store::{list_history_in, save_history_in};
        use std::time::{Duration, UNIX_EPOCH};

        let source = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let tree = crate::Scanner::new(crate::ScanOptions::default())
            .scan(source.path())
            .unwrap();
        let volume = VolumeId("test:1".to_owned());
        let series = history.path().join("test-1");
        std::fs::create_dir(&series).unwrap();
        std::fs::write(series.join(".pending"), b"interrupted writer").unwrap();
        let usage = DiskUsage {
            total: 100,
            free: 30,
            available: 25,
        };
        for index in (0..10).rev() {
            let observation = Observation {
                volume: volume.clone(),
                time: UNIX_EPOCH + Duration::from_secs(index),
                usage: (index != 9).then_some(usage),
                tree: &tree,
            };
            save_history_in(history.path(), &observation).unwrap();
        }
        let records = list_history_in(history.path(), &volume).unwrap();
        assert_eq!(records.len(), 8);
        assert_eq!(records[0].record.meta.taken, 2);
        assert_eq!(records[0].usage, Some(usage));
        assert_eq!(records[7].record.meta.taken, 9);
        assert_eq!(records[7].usage, None);
        assert!(!series.join(".pending").exists());
        assert!(
            list_history_in(history.path(), &VolumeId("test:2".into()))
                .unwrap()
                .is_empty()
        );
        let (_, trees) = crate::snapshot::store::open(&records[0].record.path).unwrap();
        assert_eq!(trees.root_path(), tree.root_path());
    }

    #[test]
    fn clearing_rotates_tracking_and_preserves_other_volume_records() {
        use crate::snapshot::store::*;

        let source = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let tree = crate::Scanner::new(crate::ScanOptions::default())
            .scan(source.path())
            .unwrap();
        let observation = |id: &str| Observation {
            volume: VolumeId(id.to_owned()),
            time: SystemTime::now(),
            usage: None,
            tree: &tree,
        };
        let first = observation("test:1");
        let second = observation("test:2");
        let dir = history.path();
        set_history_tracking_in(dir, first.volume(), true).unwrap();
        let old = history_tracking_in(dir, first.volume()).unwrap().unwrap();
        save_tracked_history_in(dir, &first, &old).unwrap();
        save_history_in(dir, &second).unwrap();
        let unrelated = dir.join("test-1").join("keep.txt");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        clear_history_in(dir, first.volume()).unwrap();
        let current = history_tracking_in(dir, first.volume()).unwrap().unwrap();
        assert_ne!(old, current);
        save_tracked_history_in(dir, &first, &old).unwrap();
        assert!(list_history_in(dir, first.volume()).unwrap().is_empty());
        assert_eq!(list_history_in(dir, second.volume()).unwrap().len(), 1);
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
        set_history_tracking_in(dir, first.volume(), false).unwrap();
        save_tracked_history_in(dir, &first, &current).unwrap();
        assert!(list_history_in(dir, first.volume()).unwrap().is_empty());
        assert!(history_tracking_in(dir, first.volume()).unwrap().is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_volume_has_a_persistent_uuid() {
        let root = Path::new("/");
        let id = VolumeId::at_root(root).expect("local macOS root UUID");
        assert_eq!(id.as_str().len(), "macos:".len() + 32);
        assert_eq!(VolumeId::at_root(root), Some(id));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_scope_rejects_bind_subroots_and_stacked_mounts() {
        let mounts = b"10 2 8:1 / /media/My\\040Drive rw - ext4 /dev/sda1 rw\n";
        assert!(whole_block_mount(mounts, b"/media/My Drive", b"8:1"));
        assert!(!whole_block_mount(
            mounts,
            b"/media/My Drive/folder",
            b"8:1"
        ));
        assert!(!whole_block_mount(mounts, b"/media/My Drive", b"8:2"));
        assert!(!whole_block_mount(
            b"10 2 8:1 /folder /bind rw - ext4 /dev/sda1 rw\n",
            b"/bind",
            b"8:1"
        ));
        assert!(!whole_block_mount(
            &[mounts.as_slice(), mounts].concat(),
            b"/media/My Drive",
            b"8:1"
        ));
    }
}
