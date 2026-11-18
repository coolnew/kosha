// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reaching the bytes of a disk image, and finding the filesystem in it.
//!
//! Four things sit between a path and a tree, and none of them is a
//! filesystem:
//!
//! ```text
//! a path        a distribution's name, or a file
//! a container   a VHDX, or nothing at all
//! a table       MBR or GPT, or nothing at all
//! a region      the partition that holds the filesystem
//! ```
//!
//! This module is all four. It lived inside [`ext4`](crate::ext4) because ext4
//! was the only filesystem read this way, which made a container and a
//! partition table look like ext4's business — and they are not. A VHDX does
//! not know what is inside it.
//!
//! The one part that touches a machine rather than a byte slice is the
//! registry, which says which WSL distributions exist and where their images
//! are. It finds nothing off Windows, and everything else here runs and is
//! tested everywhere.
//!
//! Every reason this can decline is a named [`Unavailable`], because a scan
//! that quietly fell back to the ordinary walk would leave somebody wondering
//! why it was slow. The caller puts the reason on the tree and the interface
//! reads it off there — what actually happened, rather than what might.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use crate::error::ScanError;
use crate::scan::{Progress, ScanOptions};
use crate::tree::{SizeTree, Unavailable};
use crate::vhdx::{self, BlockMap, Placement, ReadAt};

use crate::ext4::{build, reader::Filesystem};
use crate::fat;
use crate::iso;
use crate::ntfs;
use crate::udf;
use crate::xfs;

/// Scan `root` by reading the disk image it names, or the image of the
/// distribution it is in.
pub fn scan(
    root: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let started = Instant::now();
    let (image, inside) = locate(root).map_err(ScanError::Strategy)?;

    // Separate reasons, because they are separate things to do about it. The
    // file would not open; the container is broken; there is no filesystem
    // kosha reads inside it. Collapsing them into one message sent somebody
    // hunting for a corrupt image when what they actually had was an `xfs`
    // data disk that opened perfectly.
    let file = File::open(&image).map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?;
    let mut disk = Disk::open(file).map_err(ScanError::Strategy)?;

    let found = find_filesystem(&mut disk).map_err(ScanError::Strategy)?;
    let note = found.note.clone();
    let region = found.region(disk);

    let mut tree = match found.kind {
        Kind::Ext => read_ext(region, root, &inside, options, publish, cancelled)?,
        Kind::Ntfs => read_ntfs(region, root, &inside, options, publish, cancelled)?,
        Kind::Xfs => read_xfs(region, root, &inside, options, publish, cancelled)?,
        Kind::Fat(_) => read_fat(region, root, &inside, options, publish, cancelled)?,
        Kind::Iso => read_iso(region, root, &inside, options, publish, cancelled)?,
        Kind::Udf => read_udf(region, root, &inside, options, publish, cancelled)?,
    };
    tree.stats.duration = started.elapsed();
    // The note above says what was found and where it was. A reader may know
    // one more thing about it that could not be known before the filesystem
    // was opened — which tree of an ISO 9660 disc the names came out of, and
    // so why they look the way they do.
    tree.stats.image = match (note, tree.stats.image.take()) {
        (Some(found), Some(detail)) => Some(format!("{found} · {detail}")),
        (Some(found), None) => Some(found),
        (None, detail) => detail,
    };
    Ok(tree)
}

/// Build a tree from an ext2, ext3 or ext4 filesystem.
fn read_ext(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let mut fs =
        Filesystem::open(region).map_err(|_| ScanError::Strategy(Unavailable::NotAnImage))?;

    // A scan of a subdirectory starts wherever that path names. Resolving it is
    // a handful of directory reads, so scanning one directory of a distribution
    // costs what that directory costs rather than what the whole thing does.
    let start = build::resolve(&mut fs, inside)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?
        .ok_or(ScanError::Strategy(Unavailable::NotInImage))?;

    build::build(&mut fs, root, start, options, publish, cancelled)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))
}

/// Build a tree from an XFS filesystem.
///
/// The default on Red Hat, Fedora, Rocky and Alma, and what a WSL data disk
/// gets when somebody formats one without thinking about it.
fn read_xfs(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let mut fs = xfs::reader::Filesystem::open(region)
        .map_err(|_| ScanError::Strategy(Unavailable::NotAnImage))?;

    let start = xfs::build::resolve(&mut fs, inside)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?
        .ok_or(ScanError::Strategy(Unavailable::NotInImage))?;

    xfs::build::build(&mut fs, root, start, options, publish, cancelled)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))
}

/// Build a tree from an ISO 9660 disc image.
///
/// **The only filesystem here that a partition table can sit on top of without
/// disturbing.** ISO 9660 leaves its first sixteen blocks to whoever wrote the
/// disc, and a bootable installer puts a master boot record and a GPT there —
/// so the same bytes are honestly both a disc and a partitioned disk. Which of
/// the two a scan means is settled by [`find_filesystem`], where a filesystem
/// at the front of the disk wins: the disc is the thing, and the EFI partition
/// on it is ten megabytes of boot loader.
fn read_iso(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let mut fs = iso::reader::Filesystem::open(region)
        .map_err(|_| ScanError::Strategy(Unavailable::NotAnImage))?;

    let start = iso::build::resolve(&mut fs, inside)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?
        .ok_or(ScanError::Strategy(Unavailable::NotInImage))?;

    let naming = fs.naming();
    let mut tree = iso::build::build(&mut fs, root, start, options, publish, cancelled)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?;
    tree.stats.image = Some(naming.in_a_note().to_string());
    Ok(tree)
}

/// Build a tree from a UDF volume.
///
/// **Preferred over the ISO 9660 filesystem beside it, where a disc has both.**
/// A bridge disc carries two descriptions of the same files so that anything
/// able to read only one can — and they are not equal. ISO 9660 cannot hold a
/// file over four gigabytes in one extent, so on a Windows installer the large
/// file is either split across records or absent from that tree altogether,
/// while the UDF tree is the disc as its author meant it. Taking the older of
/// two answers because it was found first would be the same mistake as reporting
/// a bootable installer's EFI partition.
fn read_udf(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    // **Every failure here is a failure to read UDF**, because this is reached
    // only once an anchor has been found — a 512-byte descriptor that names
    // itself, says which block it is in and checks its own CRC. So the refusal
    // says so, rather than "there is no filesystem kosha reads in this file",
    // which would send somebody looking for damage in a perfectly good disc.
    let mut fs = udf::reader::Filesystem::open(region).map_err(|error| {
        let reason = match error
            .get_ref()
            .and_then(|reason| reason.downcast_ref::<udf::format::Unreadable>())
        {
            // A write-once disc's blocks go through a table built from where the
            // writing stopped, and an image of one does not carry it.
            Some(udf::format::Unreadable::Unmapped) => {
                "its blocks are behind an indirection kosha does not follow"
            }
            _ => "kosha could not read its descriptors",
        };
        ScanError::Strategy(Unavailable::Udf(reason))
    })?;

    let start = udf::build::resolve(&mut fs, inside)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?
        .ok_or(ScanError::Strategy(Unavailable::NotInImage))?;

    let note = fs.in_a_note();
    let mut tree = udf::build::build(&mut fs, root, start, options, publish, cancelled)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?;
    tree.stats.image = Some(note);
    Ok(tree)
}

/// Build a tree from a FAT or exFAT filesystem.
///
/// The one at the front of almost every partitioned disk: the specification
/// that defines an EFI system partition says it holds FAT — 12, 16 or 32 — so
/// a whole-disk image of any machine that starts with UEFI has one, and until
/// this a scan of such an image found the root filesystem and stepped straight
/// over it.
fn read_fat(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    let mut fs = fat::reader::Filesystem::open(region)
        .map_err(|_| ScanError::Strategy(Unavailable::NotAnImage))?;

    let start = fat::build::resolve(&mut fs, inside)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?
        .ok_or(ScanError::Strategy(Unavailable::NotInImage))?;

    fat::build::build(&mut fs, root, start, options, publish, cancelled)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))
}

/// Build a tree from an NTFS filesystem, by reading `$MFT` out of the image.
///
/// The same pass the elevated volume scan makes, over the same records, by the
/// same code — the only difference is where the bytes come from, which is the
/// whole point of [`ntfs::mft::Source`]. So a Windows disk in a virtual
/// machine's image is measured in seconds, from a Mac, by a process with no
/// privileges at all.
fn read_ntfs(
    region: Region,
    root: &Path,
    inside: &Path,
    options: &ScanOptions,
    publish: &dyn Fn(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<SizeTree, ScanError> {
    // A subdirectory inside an NTFS image cannot be resolved the way one inside
    // an ext image can. On a live volume the record number comes from asking
    // Windows about the path; there is nothing to ask here, and guessing by
    // walking the whole table to find a name would be a different feature
    // pretending to be this one.
    if !inside.as_os_str().is_empty() {
        return Err(ScanError::Strategy(Unavailable::NotInImage));
    }

    let mut mft = ntfs::mft::Mft::open(region)
        .map_err(|_| ScanError::Strategy(Unavailable::ImageUnreadable))?;

    let mut table = mft
        .read_table(
            |records| {
                publish(Progress {
                    dirs_scanned: None,
                    entries: records,
                    dirs_pending: 0,
                    depth: 0,
                })
            },
            cancelled,
        )
        .map_err(|error| match error.kind() {
            io::ErrorKind::Interrupted => ScanError::Cancelled,
            _ => ScanError::Strategy(Unavailable::ImageUnreadable),
        })?;

    ntfs::build::consolidate(&mut table);

    // Failing to read `$Secure` is not failing the scan: the tree is complete
    // and every entry in it simply has an owner that could not be read, which
    // the report already has a row for.
    let security = if options.collect_owners && !table.security_stream.fragments.is_empty() {
        mft.read_security(&table.security_stream)
            .unwrap_or_default()
    } else {
        ntfs::security::Security::default()
    };

    let mut tree = ntfs::build::build(
        &table,
        ntfs::build::Root {
            record: ntfs::build::ROOT_RECORD,
            path: root,
        },
        options,
        &security,
        publish,
        cancelled,
    );
    // `build` stamps every tree it makes as the Master File Table strategy,
    // which is where this table came from and not how it was reached. The line
    // somebody reads has to say the second: no volume was opened, nothing was
    // elevated, and "Master File Table" would send them looking for a drive.
    tree.stats.strategy = crate::tree::Strategy::DiskImage;
    Ok(tree)
}

/// What is inside this file, where it is something kosha can name but not
/// read.
///
/// **For the error message, and it opens no filesystem.** Working out what a
/// file *is* has always happened on its own, on the failure path of a scan
/// that is already returning an error; this is the same thing reaching one
/// layer further in, so that "there is no filesystem kosha reads in this file"
/// becomes "this is btrfs".
///
/// A container is opened first, so a VHDX holding btrfs is named too — which
/// is the ordinary shape of a virtual machine's disk and the case where being
/// told "no filesystem" is least believable.
pub(crate) fn recognised_in(path: &Path) -> Option<&'static str> {
    let file = File::open(path).ok()?;
    let mut disk = Disk::open(file).ok()?;
    crate::recognise::name_of(&mut disk)
}

/// Whether this file carries a partition table.
///
/// **A disk does not have to begin with a filesystem, and a whole one never
/// does.** `dd if=/dev/sda` puts a table at the front and the first filesystem a
/// megabyte in, so every check that looks at offset zero finds nothing — and
/// such a file came back `is not a directory` while `--image` read it
/// completely. A table is what says *this is a disk* whatever is in it, and it
/// is the same parser the image reader uses, not a second opinion about the
/// same bytes.
pub(crate) fn is_partitioned(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut disk) = Disk::open(file) else {
        return false;
    };
    crate::partition::read(&mut disk).is_some()
}

/// Where on the disk the filesystem is.
///
/// A disk image does not have to begin with a filesystem. An image of a whole
/// disk begins with a partition table, and the filesystem starts a megabyte in
/// — which is why `dd if=/dev/sda` produced a file kosha could see nothing
/// inside while `dd if=/dev/sda2` produced one it read completely.
struct Found {
    start: u64,
    length: u64,
    kind: Kind,
    /// What to put on the tree: the filesystem, and the partition where there
    /// was more than one to choose from.
    note: Option<String>,
}

/// A filesystem kosha can read out of an image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// ext2, ext3 or ext4 — one reader, because they are one format with
    /// features turned on.
    Ext,
    Ntfs,
    Xfs,
    /// FAT12, FAT16, FAT32 or exFAT. Which one is carried rather than
    /// flattened, because it is what the note on the tree says and "FAT32" and
    /// "exFAT" are different enough answers that collapsing them would lose
    /// the one fact somebody looking at a boot partition wants.
    Fat(fat::format::Flavour),
    /// An optical disc's filesystem, in an `.iso` or on a real one.
    Iso,
    /// What ISO 9660 was replaced by: a Windows installer, a rewritable disc, a
    /// Blu-ray, a stick Windows formatted.
    Udf,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Ext => "ext4",
            Kind::Ntfs => "NTFS",
            Kind::Xfs => "XFS",
            Kind::Fat(flavour) => flavour.name(),
            Kind::Iso => "ISO 9660",
            // Without a revision, which the note gets from the reader once the
            // volume is open — the same way the ISO 9660 note gets its naming.
            Kind::Udf => "UDF",
        }
    }
}

impl Found {
    fn region(&self, disk: Disk) -> Region {
        Region {
            disk,
            start: self.start,
            length: self.length,
        }
    }
}

/// Look at the front of the disk, then at its partitions.
///
/// **A filesystem at the front wins, and is looked for first.** That is the
/// ordinary case — `mkfs` on a file, a WSL distribution's image, a `dd` of one
/// partition rather than of the disk holding it — and it is also the cheap
/// case, one read of a kilobyte.
///
/// **Choosing between partitions is a judgement, so it is recorded.** A disk
/// with a boot filesystem and a root filesystem has two kosha can read, and
/// picking the larger is right almost every time and wrong occasionally. The
/// tree carries which one was taken, the status line says it, and a snapshot
/// keeps it — because a number nobody can trace back to a decision is a number
/// nobody can check.
fn find_filesystem(disk: &mut Disk) -> Result<Found, Unavailable> {
    let size = disk.size();
    if let Some(kind) = filesystem_at(disk, 0) {
        return Ok(Found {
            start: 0,
            length: size,
            kind,
            note: Some(kind.name().to_string()),
        });
    }

    let Some(table) = crate::partition::read(disk) else {
        // No filesystem at the front and no table saying where one is. Before
        // giving up, ask what *is* there: naming costs a magic number and
        // reading costs a reader, so kosha can name btrfs, LVM, LUKS and a
        // dozen others it cannot read — and "this is btrfs" is a fact somebody
        // can act on where "no filesystem" sends them looking for corruption
        // that is not there.
        // The front of the disk only: there is no table, which is what this
        // branch is. Asking `name_of` would read for one again.
        if let Some(name) = crate::recognise::name_at(disk, 0) {
            return Err(Unavailable::NamedFilesystem(name));
        }
        // Which of the two remaining refusals fits depends on whether there was
        // a container: a VHDX that opened is a real image of a filesystem kosha
        // neither reads nor recognises, while a plain file with neither was
        // probably never an image at all.
        return Err(if disk.is_contained() {
            Unavailable::UnknownFilesystem
        } else {
            Unavailable::NotAnImage
        });
    };

    let total = table.partitions.len();
    let mut readable: Vec<(&crate::partition::Partition, Kind)> = table
        .partitions
        .iter()
        .filter_map(|partition| Some((partition, filesystem_at(disk, partition.start)?)))
        .collect();
    // Largest first, and the number breaks a tie so that two partitions of
    // exactly equal size cannot make two runs over the same disk disagree.
    readable.sort_by(|(a, _), (b, _)| b.length.cmp(&a.length).then(a.number.cmp(&b.number)));

    // A table and no readable partition is a disk, definitely — so this is
    // never "not an image", whatever the container was. It is an image full of
    // filesystems kosha does not read, and the useful thing to say is *which*:
    // a Fedora disk is a small FAT boot partition and a large btrfs one, and
    // the large one is the answer somebody wants named.
    let (chosen, kind) = match readable.first() {
        Some(found) => *found,
        None => {
            if let Some(name) = crate::recognise::name_in(disk, &table) {
                return Err(Unavailable::NamedFilesystem(name));
            }
            return Err(Unavailable::UnknownFilesystem);
        }
    };
    // **A partition's number is a name, not a position in this list.** MBR
    // numbers logical partitions from five whatever the primaries did, and GPT
    // keeps a partition in the slot it was written to — so a disk with one
    // logical partition on it is partition 5 of a list of one, and a GPT whose
    // second slot is empty has a partition 3 in a list of two. Said the obvious
    // way, both come out as "partition 5 of 1" and "partition 3 of 2", which
    // read as arithmetic somebody got wrong.
    //
    // The count is still worth saying: it is what tells you other partitions
    // were there and are not in this total. So it is said as a count where it
    // cannot be read as an ordinal.
    let note = if chosen.number as usize <= total {
        format!(
            "{} · partition {} of {total} ({})",
            kind.name(),
            chosen.number,
            table.scheme.name()
        )
    } else {
        format!(
            "{} · partition {} ({}, {total} on the disk)",
            kind.name(),
            chosen.number,
            table.scheme.name()
        )
    };
    Ok(Found {
        kind,
        start: chosen.start,
        // A table can describe more disk than the file holds, which is what a
        // truncated or still-copying image looks like. Clamping here means the
        // reader sees a short filesystem and says so, rather than reads failing
        // one at a time somewhere deep in a directory walk.
        length: chosen.length.min(size.saturating_sub(chosen.start)),
        note: Some(note),
    })
}

/// Which filesystem, if any, begins at this offset.
///
/// Each answer comes from the same parser the reader itself will use — an ext
/// superblock from `format::superblock`, an NTFS boot sector from
/// `boot::parse` — so this cannot drift from what will actually open. A second
/// opinion written here would eventually disagree with the first, and the
/// symptom would be a filesystem recognised and then refused.
///
/// **This is the list that grows** when another filesystem is read from an
/// image. Everything above it — a container, a table, a region — is already
/// indifferent to which one it is.
fn filesystem_at(disk: &mut Disk, start: u64) -> Option<Kind> {
    // **UDF before ISO 9660, and ISO 9660 before everything else.** A disc image
    // can honestly be three things at once: a UDF volume, an ISO 9660 disc, and
    // a partitioned disk with a FAT boot partition on it. All three are true and
    // they are in decreasing order of how much of the disc they describe — the
    // UDF tree is the whole of it, the ISO 9660 tree is what an older reader can
    // see of it, and the EFI partition is ten megabytes of boot loader.
    //
    // Recognising UDF costs one read of 512 bytes, at a place fixed by the
    // standard, of a descriptor that says where it is and checks its own CRC.
    if udf::reader::block_size_of(disk, start, disk.size().saturating_sub(start)).is_some() {
        return Some(Kind::Udf);
    }

    // **ISO 9660 next, and the order is the whole point.** A bootable disc
    // image carries a partition table in the sixteen blocks ISO 9660 leaves
    // free at its front, so the same file is honestly a disc *and* a
    // partitioned disk with a FAT boot partition on it. Both answers are true
    // and only one of them is useful: the disc is five gigabytes and the
    // partition is ten megabytes of boot loader.
    let mut block = [0u8; crate::iso::format::DESCRIPTOR_BYTES];
    if let Some(at) = start.checked_add(crate::iso::format::DESCRIPTORS_AT)
        && disk.read_exact_at(at, &mut block).is_ok()
        && iso::format::volume(&block).is_ok()
    {
        return Some(Kind::Iso);
    }

    let mut sector = [0u8; 4096];
    if disk.read_exact_at(start, &mut sector).is_ok() {
        if ntfs::boot::parse(&sector).is_some() {
            return Some(Kind::Ntfs);
        }
        if xfs::format::superblock(&sector).is_ok() {
            return Some(Kind::Xfs);
        }
        // After NTFS, and that order is load-bearing: an NTFS boot sector
        // shares this layout as far as the sector size and is told apart by
        // fields FAT leaves non-zero. `fat::format::boot` refuses one on its
        // own — see the total-sector check there — but the cheap, certain
        // answer is to ask the reader that has a name to match against first.
        if let Ok(geometry) = fat::format::boot(&sector) {
            return Some(Kind::Fat(geometry.flavour));
        }
    }

    let mut bytes = [0u8; 1024];
    let at = start.checked_add(crate::ext4::format::SUPERBLOCK_OFFSET)?;
    if disk.read_exact_at(at, &mut bytes).is_ok() && crate::ext4::format::superblock(&bytes).is_ok()
    {
        return Some(Kind::Ext);
    }
    None
}

/// One partition of a disk, or the whole of it.
///
/// Every offset the filesystem reader produces is relative to its own start,
/// and a partition begins somewhere else. Wrapping rather than teaching the
/// reader about partitions keeps that arithmetic in one place, and there is
/// always a region even for an unpartitioned image — one starting at zero —
/// so there is one type here rather than two.
struct Region {
    disk: Disk,
    start: u64,
    length: u64,
}

/// The same window, for the NTFS reader.
///
/// Two traits rather than one because they want different things: `ReadAt`
/// knows about sparse blocks and about where a read will really land, which is
/// how an ext4 scan turns fifteen hundred seeks into a sweep; `Source` is one
/// method, because the other thing that has to satisfy it is a raw device and
/// none of that means anything there.
impl ntfs::mft::Source for Region {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        ReadAt::read_exact_at(self, offset, buffer)
    }

    /// The partition's length, which is what an NTFS volume inside it is
    /// allowed to be — the one number about an image that does not come out of
    /// the image's own headers.
    fn len(&self) -> Option<u64> {
        Some(self.length)
    }
}

impl ReadAt for Region {
    fn size(&self) -> u64 {
        self.length
    }

    fn placement(&self, offset: u64) -> Placement {
        self.disk.placement(self.start.saturating_add(offset))
    }

    /// Already an absolute position in the file: a placement came from the
    /// disk, so it goes back to the disk untranslated.
    fn read_placed(&mut self, at: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.disk.read_placed(at, buffer)
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        // Refusing past the end of the partition rather than reading into
        // whatever follows it. Two filesystems on one disk are two filesystems,
        // and a reader that wandered from one into the next would produce a
        // tree that is wrong rather than an error that is right.
        let end = offset.checked_add(buffer.len() as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "read past the partition")
        })?;
        if end > self.length {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the end of the partition",
            ));
        }
        let at = self.start.checked_add(offset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "read past the partition")
        })?;
        self.disk.read_exact_at(at, buffer)
    }
}

/// Work out which image a path means, and where inside it to start.
///
/// Two kinds of path reach here. A redirector path names a distribution, and the
/// registry says where its image is. **A path to an image names it outright** —
/// worth supporting for its own sake: a distribution that has been exported, a
/// backup, a disk somebody detached, or the image of a distribution the registry
/// has forgotten are all things a person may want to measure, and none of them
/// can be reached any other way.
///
/// **The redirector spelling is decided first, and as a string.** Asking the
/// filesystem anything about `\\wsl.localhost\...` goes through the redirector, and
/// the first touch of one *starts the distribution* — booting a virtual machine
/// in order to work out whether we can avoid talking to it. So the name is
/// picked out of the text before any call that could reach a disk.
///
/// After that, any file is a candidate. Not any file with a particular
/// extension: a name is not evidence of a format, an image exported by another
/// tool is called whatever that tool called it, and `mkfs` on a file leaves
/// something with no extension at all. What the bytes are is settled by
/// reading them.
fn locate(root: &Path) -> Result<(PathBuf, PathBuf), Unavailable> {
    let Some(name) = distribution_in(root) else {
        return if root.is_file() {
            Ok((root.to_path_buf(), PathBuf::new()))
        } else {
            Err(Unavailable::NotAnImagePath)
        };
    };

    let found = crate::platform::wsl::distributions()
        .into_iter()
        .find(|distribution| distribution.name.eq_ignore_ascii_case(&name))
        .ok_or(Unavailable::NotWsl)?;
    let image = found.image.ok_or(Unavailable::NoImage)?;

    // Everything after `\\wsl.localhost\<name>`: the prefix is one component
    // and the distribution name is the next.
    let mut components = root.components();
    components.next();
    let mut inside = PathBuf::new();
    for component in components {
        match component {
            Component::RootDir | Component::CurDir => continue,
            // The image is walked downwards from its root, so there is nothing
            // for a `..` to walk back up from. Declining hands the scan to the
            // ordinary walk, which resolves it the way the filesystem does.
            Component::ParentDir => return Err(Unavailable::NotInImage),
            _ => inside.push(component),
        }
    }

    Ok((image, inside))
}

/// The distribution a redirector path names, if it is one.
///
/// Both spellings Windows publishes. `wsl$` is the older one and still works,
/// so a path a user pasted from somewhere old has to be recognised.
fn distribution_in(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let rest = ["\\\\wsl.localhost\\", "\\\\wsl$\\"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))?;
    let name = rest.split('\\').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// A disk image, with or without a container around it.
///
/// The seam the filesystem reader sits on: offsets going in are addresses in
/// the disk, reads coming out are from wherever in the file those bytes really
/// are. A VHDX makes that a map; a plain image makes it the identity, and
/// [`ReadAt::placement`] defaults to the identity precisely because a file is
/// one of the things it has to serve.
///
/// **A raw image is not a lesser case.** `mkfs` on a file, `dd` of a partition,
/// an image from a virtualiser that does not write VHDX, a backup somebody
/// keeps — none of those have a container, and every one of them is a
/// filesystem somebody may want measured without mounting anything and without
/// being root.
struct Disk {
    file: File,
    /// `None` for a plain image, where a disk byte *is* a file byte.
    map: Option<BlockMap>,
    /// The file's own length, for the plain case. A map knows its own.
    length: u64,
}

impl Disk {
    /// Open the image, container or not.
    ///
    /// The signature is checked here rather than left to `vhdx::open` — which
    /// checks it too — for two reasons. It decides whether there is a container
    /// at all; and where there is one, it separates "this VHDX could not be
    /// read" from "this was never a VHDX", which send somebody to completely
    /// different places — the first is a differencing disk, a header that does
    /// not checksum, or a truncated image.
    fn open(file: File) -> Result<Disk, Unavailable> {
        let mut whole = Whole(file);
        let mut start = [0u8; 8];
        if whole.read_exact_at(0, &mut start).is_err() {
            // Not even eight bytes long. Whatever it is, it is not a disk.
            return Err(Unavailable::NotAnImage);
        }
        if vhdx::has_signature(&start) {
            let map = vhdx::open(&mut whole).map_err(|_| Unavailable::ImageUnreadable)?;
            let length = whole.size();
            return Ok(Disk {
                file: whole.0,
                map: Some(map),
                length,
            });
        }
        // No container, so the bytes are the disk. Whether there is a
        // filesystem in them is the next layer's question rather than this
        // one's, which is the right division: this module knows about
        // containers and `reader` knows about filesystems.
        let length = whole.size();
        Ok(Disk {
            file: whole.0,
            map: None,
            length,
        })
    }

    /// Whether a container was opened, rather than the file being the disk.
    ///
    /// Used only to choose between two refusals, and it is the difference
    /// between a real image of a filesystem kosha does not read and a file that
    /// was probably never an image at all.
    fn is_contained(&self) -> bool {
        self.map.is_some()
    }
}

impl ReadAt for Disk {
    fn size(&self) -> u64 {
        match &self.map {
            Some(map) => map.virtual_size(),
            None => self.length,
        }
    }

    fn placement(&self, offset: u64) -> Placement {
        match &self.map {
            Some(map) => map.locate(offset).0,
            // Every byte of a plain image is exactly where it says it is, so
            // the sort that orders reads by where they land is not pointless
            // here — it is already in the order the disk is in, and costs one
            // comparison per block to discover that.
            None => Placement::At(offset),
        }
    }

    /// Straight to the file, past the map. Only reached with a place the map
    /// itself produced, and a whole filesystem block always fits inside the
    /// virtual-disk block it was resolved through — a megabyte against four
    /// kilobytes — so a run built out of these never straddles a boundary.
    fn read_placed(&mut self, at: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(at))?;
        self.file.read_exact(buffer)
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let Some(map) = &self.map else {
            return self.read_placed(offset, buffer);
        };
        let mut done = 0usize;
        while done < buffer.len() {
            let (placement, left) = map.locate(offset + done as u64);
            let take = (left as usize).min(buffer.len() - done);
            match placement {
                Placement::At(at) => {
                    self.file.seek(SeekFrom::Start(at))?;
                    self.file.read_exact(&mut buffer[done..done + take])?;
                }
                // Never written, so it reads as zeros — which is most of a
                // sparse disk and is not an error anywhere.
                Placement::Unwritten => buffer[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(())
    }
}

/// The image file itself, before the block map exists.
///
/// The structures that *build* the map live at fixed offsets in the file rather
/// than in the virtual disk, so reading them cannot go through the map that has
/// not been made yet.
struct Whole(File);

impl ReadAt for Whole {
    fn size(&self) -> u64 {
        self.0.metadata().map(|meta| meta.len()).unwrap_or(0)
    }

    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.read_exact(buffer)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::ScanOptions;
    use crate::fat::format::Flavour;

    /// A 512-byte boot record with the entries given, as `(type, first, count)`.
    fn boot_record(entries: &[(u8, u32, u32)]) -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        for (slot, (kind, first, count)) in entries.iter().enumerate() {
            let at = 0x1BE + slot * 16;
            sector[at + 4] = *kind;
            sector[at + 8..at + 12].copy_from_slice(&first.to_le_bytes());
            sector[at + 12..at + 16].copy_from_slice(&count.to_le_bytes());
        }
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    /// A disk whose only partition is a *logical* one, inside an extended.
    ///
    /// The shape a second-hand disk from the MBR era has, and the one that made
    /// the sentence read as arithmetic: MBR numbers logical partitions from
    /// five whatever the primaries did, so this is partition 5 of a list of one.
    fn one_logical_partition(volume: &[u8]) -> Vec<u8> {
        const EBR: usize = 2048;
        const GAP: u32 = 2048;
        let sectors = (volume.len() / 512) as u32;
        let mut disk = boot_record(&[(0x05, EBR as u32, GAP + sectors)]);
        disk.resize(EBR * 512, 0);
        disk.extend_from_slice(&boot_record(&[(0x83, GAP, sectors)]));
        disk.resize((EBR + GAP as usize) * 512, 0);
        disk.extend_from_slice(volume);
        disk
    }

    /// An ordinary disk: one primary partition, a megabyte in.
    fn one_primary_partition(volume: &[u8]) -> Vec<u8> {
        let sectors = (volume.len() / 512) as u32;
        let mut disk = boot_record(&[(0x83, 2048, sectors)]);
        disk.resize(2048 * 512, 0);
        disk.extend_from_slice(volume);
        disk
    }

    /// A GPT disk with the partition in its *second* slot, so the first is
    /// empty.
    ///
    /// The other shape the numbering produces, and the one `partition.rs` names
    /// in its own comment: GPT numbers by slot, so a disk whose second slot is
    /// empty has a partition 1 and a partition 3. Here only slot two is filled,
    /// which makes it partition 2 of a list of one.
    fn one_partition_in_the_second_slot(volume: &[u8]) -> Vec<u8> {
        const SECTOR: usize = 512;
        const COUNT: usize = 128;
        const ENTRY: usize = 128;
        const ARRAY_SECTORS: usize = COUNT * ENTRY / SECTOR;
        const FIRST_USABLE: u64 = 2 + ARRAY_SECTORS as u64;
        const START: u64 = 2048;

        let sectors = (volume.len() / SECTOR) as u64;
        let last = START + sectors - 1;
        let total = last + 1 + ARRAY_SECTORS as u64 + 1;
        let backup_header = total - 1;
        let backup_array = backup_header - ARRAY_SECTORS as u64;

        let mut entries = vec![0u8; COUNT * ENTRY];
        // The second slot, not the first. Any non-zero type GUID is a used
        // slot as far as the reader is concerned.
        let at = ENTRY;
        entries[at..at + 16].copy_from_slice(&[0x0F; 16]);
        entries[at + 16..at + 32].copy_from_slice(&[0x11; 16]);
        entries[at + 32..at + 40].copy_from_slice(&START.to_le_bytes());
        entries[at + 40..at + 48].copy_from_slice(&last.to_le_bytes());
        let mut crc = crate::partition::Crc32::new();
        crc.update(&entries);
        let array_crc = crc.finish();

        let header = |current: u64, other: u64, array_lba: u64| {
            let mut head = vec![0u8; 92];
            head[0..8].copy_from_slice(b"EFI PART");
            head[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            head[12..16].copy_from_slice(&92u32.to_le_bytes());
            head[24..32].copy_from_slice(&current.to_le_bytes());
            head[32..40].copy_from_slice(&other.to_le_bytes());
            head[40..48].copy_from_slice(&FIRST_USABLE.to_le_bytes());
            head[48..56].copy_from_slice(&(backup_array - 1).to_le_bytes());
            head[72..80].copy_from_slice(&array_lba.to_le_bytes());
            head[80..84].copy_from_slice(&(COUNT as u32).to_le_bytes());
            head[84..88].copy_from_slice(&(ENTRY as u32).to_le_bytes());
            head[88..92].copy_from_slice(&array_crc.to_le_bytes());
            // Over the header with its own checksum field left at zero.
            let mut crc = crate::partition::Crc32::new();
            crc.update(&head);
            let own = crc.finish();
            head[16..20].copy_from_slice(&own.to_le_bytes());
            head
        };

        let mut disk = boot_record(&[(0xEE, 1, (total - 1).min(u64::from(u32::MAX)) as u32)]);
        disk.extend_from_slice(&header(1, backup_header, 2));
        disk.resize(2 * SECTOR, 0);
        disk.extend_from_slice(&entries);
        disk.resize(START as usize * SECTOR, 0);
        disk.extend_from_slice(volume);
        disk.extend_from_slice(&entries);
        disk.extend_from_slice(&header(backup_header, 1, backup_array));
        disk.resize(total as usize * SECTOR, 0);
        disk
    }

    fn note_of(disk: &[u8], name: &str) -> String {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join(name);
        std::fs::write(&path, disk).expect("the image is written");
        let tree = scan_image(&path).expect("a partitioned disk is an image");
        tree.stats
            .image
            .clone()
            .expect("a disk read through a table says so")
    }

    fn scan_image(path: &Path) -> Result<crate::SizeTree, crate::ScanError> {
        super::scan(
            path,
            &ScanOptions {
                use_image: true,
                ..ScanOptions::default()
            },
            &|_| {},
            &|| false,
        )
    }

    /// **A partition's number is a name, not a position in a list**, and saying
    /// it as one produced "partition 5 of 1" for a disk with a single logical
    /// partition on it. Found by wrapping one filesystem in every container a
    /// disk can have and reading the sentence that came back.
    #[test]
    fn a_partition_numbered_past_the_end_of_the_list_is_not_said_as_an_ordinal() {
        let volume = crate::fat::tests::fixture(Flavour::Fat16);

        assert_eq!(
            note_of(&one_primary_partition(&volume), "ordinary.img"),
            "FAT16 · partition 1 of 1 (MBR)",
            "the ordinary case is the familiar sentence and is not changed"
        );
        assert_eq!(
            note_of(&one_logical_partition(&volume), "logical.img"),
            "FAT16 · partition 5 (MBR, 1 on the disk)",
            "a logical partition is numbered from five, so 'of 1' would be wrong"
        );
        // The other scheme, and the shape `partition.rs` names in its own
        // comment. Measured before it was written down: a GPT with only its
        // second slot filled read as `partition 2 of 1 (GPT)`.
        assert_eq!(
            note_of(&one_partition_in_the_second_slot(&volume), "gapped.img"),
            "FAT16 · partition 2 (GPT, 1 on the disk)",
            "GPT keeps a partition in its slot, so a gap makes the number a name"
        );
    }
}
