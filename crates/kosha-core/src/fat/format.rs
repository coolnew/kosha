// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! FAT and exFAT on-disk structures, as bytes in and numbers out.
//!
//! **Two formats under one name, and they share almost no bytes.** FAT12,
//! FAT16 and FAT32 are one format with three widths of table entry, the way
//! ext2, ext3 and ext4 are one format with features turned on. exFAT is a
//! different format that Microsoft gave a similar name: a different boot
//! sector, a different directory entry, a different way of saying how long a
//! file is, and names in UTF-16 rather than an eight-character field with a
//! second field of thirteen characters bolted alongside it.
//!
//! What they do share is the shape of the answer — a cluster heap, a table
//! that chains clusters together, and directories made of 32-byte records —
//! and that is exactly the part [`reader`](super::reader) and
//! [`build`](super::build) work in. So the two formats meet here, in the
//! parsing, and nowhere above it.
//!
//! ```text
//! 0x0000  boot sector      geometry: where the table is, where the heap is
//! fat     table            12, 16 or 32 bits per cluster; exFAT uses 32
//! heap    clusters         file bytes, and directories' 32-byte records
//! ```
//!
//! ## What is needed, and what is not
//!
//! A file's length is in its directory entry — `DIR_FileSize` in FAT,
//! `DataLength` in exFAT — and neither format has sparse files, so **no
//! file's cluster chain is ever followed**. How much of the disk a file
//! occupies is its length rounded up to a cluster, which is arithmetic rather
//! than a table walk, and it saves one read per file on a filesystem whose
//! table is the one structure a scan would otherwise hammer.
//!
//! Directories are different: their chains have to be followed, because
//! following them is how the entries are found. A directory's own allocation
//! comes out of that walk for free.
//!
//! ## What is declined, and why it is named
//!
//! A geometry whose numbers do not agree with each other is a named refusal
//! rather than a best guess. The fields here are multiplied together to
//! produce byte offsets, and a boot sector is the one structure on a disk that
//! nothing checksums — so "these numbers cannot describe a disk" has to be an
//! answer, or a flipped bit becomes a seek to nowhere and a tree of rubbish.

/// One directory record, in both formats. The one number they agree on.
pub const ENTRY_BYTES: usize = 32;

/// Sector sizes a real volume uses. Everything here divides or multiplies by
/// one, so a value outside this set is a refusal rather than a curiosity.
const SECTOR_SIZES: [u32; 4] = [512, 1024, 2048, 4096];

/// The largest cluster this reader will read as one piece.
///
/// exFAT permits 32 MiB clusters and a directory is read a cluster at a time,
/// so this is the buffer a directory walk allocates. It is a bound rather than
/// a preference: without one, two bytes of a damaged boot sector ask for an
/// allocation no machine can serve, and a failed allocation aborts the process
/// rather than returning an error somebody can catch.
pub const MAX_CLUSTER_BYTES: u64 = 32 * 1024 * 1024;

/// The cluster counts that separate the three FAT widths.
///
/// **These are the specification's numbers and they are not round.** 4085 and
/// 65525 are what Microsoft's own documentation gives, and a reader that
/// rounds them to 4096 and 65536 will read a small number of real volumes with
/// the wrong table width — which produces a chain of plausible garbage rather
/// than an error.
const FAT12_MAX_CLUSTERS: u32 = 4085;
const FAT16_MAX_CLUSTERS: u32 = 65525;

/// `DIR_Attr`, and the bits of it that mean anything here.
pub const ATTR_DIRECTORY: u8 = 0x10;
pub const ATTR_VOLUME_ID: u8 = 0x08;
/// A long-name record is all four of read-only, hidden, system and volume-id
/// at once — a combination no real file has, which is how the format smuggled
/// long names past software that predated them.
const ATTR_LONG_NAME: u8 = 0x0F;
const ATTR_LONG_MASK: u8 = 0x3F;

/// `DIR_NTRes`: the two bits VFAT uses to say a short name is really lowercase.
const CASE_BASE_LOWER: u8 = 0x08;
const CASE_EXT_LOWER: u8 = 0x10;

/// The first byte of a directory record, where it is not a name.
const FREE: u8 = 0xE5;
/// A name really starting with 0xE5 is stored as 0x05, so it cannot be
/// mistaken for a deleted record.
const REAL_E5: u8 = 0x05;

/// exFAT record type codes, with the in-use bit set.
const EXFAT_FILE: u8 = 0x85;
const EXFAT_STREAM: u8 = 0xC0;
const EXFAT_NAME: u8 = 0xC1;
/// The high bit of an exFAT record type: clear means the record was deleted.
const EXFAT_IN_USE: u8 = 0x80;

/// `GeneralSecondaryFlags` bit 1: the clusters are contiguous and the table
/// says nothing about them.
const EXFAT_NO_FAT_CHAIN: u8 = 0x02;

/// exFAT `FileAttributes`, which are the FAT attributes widened to sixteen
/// bits.
const EXFAT_ATTR_DIRECTORY: u16 = 0x0010;

/// UTF-16 units in one long-name record (FAT) and one name record (exFAT).
pub const LONG_NAME_CHARS: usize = 13;
pub const EXFAT_NAME_CHARS: usize = 15;
/// The most long-name records one name may be spread over, which the ordinal
/// field's five bits would allow to be 31 and the format limits to 20.
pub const MAX_LONG_PARTS: usize = 20;

/// Why a FAT or exFAT filesystem could not be read.
///
/// Named rather than collapsed into one error, for the same reason the ext and
/// XFS readers name theirs: "kosha cannot read this" is not something anybody
/// can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unreadable {
    #[error("this is not a FAT or exFAT filesystem")]
    NotFat,
    #[error("the filesystem's geometry does not make sense")]
    Impossible,
}

/// Which of the four this is.
///
/// Kept even though the reader only branches on it in two places, because it
/// is what the status line says and because "FAT32" and "exFAT" are different
/// enough answers that collapsing them would lose the one fact somebody
/// looking at a boot partition actually wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    Fat12,
    Fat16,
    Fat32,
    ExFat,
}

impl Flavour {
    pub fn name(self) -> &'static str {
        match self {
            Flavour::Fat12 => "FAT12",
            Flavour::Fat16 => "FAT16",
            Flavour::Fat32 => "FAT32",
            Flavour::ExFat => "exFAT",
        }
    }

    /// Whether directory records are FAT's short-and-long pair or exFAT's
    /// record sets. The one question the reader asks this type.
    pub fn is_exfat(self) -> bool {
        self == Flavour::ExFat
    }
}

/// Where a filesystem's root directory is.
///
/// **FAT12 and FAT16 are the only filesystems kosha reads whose root is not
/// addressed like everything else.** It is a fixed run of sectors between the
/// table and the heap, with no cluster number and no chain — which is why the
/// number of entries in it is a field in the boot sector and why a FAT16 root
/// directory can fill up while the disk is half empty. FAT32 fixed this by
/// giving the root a cluster like any other directory, and exFAT kept that.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Root {
    /// A byte offset and a length, addressed directly.
    Fixed { start: u64, bytes: u64 },
    /// A cluster number, chained through the table like any directory.
    Cluster(u32),
}

/// Where a directory's bytes are, in the one form both formats reduce to.
///
/// Three things can hold directory records, and only one of them is what
/// anybody expects. An ordinary directory is a chain through the table. A
/// FAT12 or FAT16 root is a fixed run of sectors with no cluster number at
/// all. And an exFAT file whose clusters were never fragmented says so, and
/// its run is read without touching the table once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// A byte offset and a length, addressed directly.
    Fixed { start: u64, bytes: u64 },
    Chain {
        first: u32,
        /// `Some(length)` for an exFAT run the table says nothing about: the
        /// clusters follow one another from `first`, and the length says how
        /// many there are.
        contiguous: Option<u64>,
    },
}

/// Everything the reader needs to turn a cluster number into a byte offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    pub flavour: Flavour,
    pub sector_size: u32,
    pub cluster_sectors: u32,
    /// Data clusters, which are numbered from 2. Cluster `n` is valid when
    /// `n - 2 < cluster_count`.
    pub cluster_count: u32,
    /// The *active* table, which is not always the first one. See
    /// [`boot`].
    pub fat_start: u64,
    pub fat_bytes: u64,
    /// Where cluster 2 begins.
    pub heap_start: u64,
    pub root: Root,
    /// What the boot sector says the volume's length is, which is not the same
    /// as how long the file is.
    pub volume_bytes: u64,
}

impl Geometry {
    pub fn cluster_bytes(&self) -> u64 {
        u64::from(self.sector_size) * u64::from(self.cluster_sectors)
    }

    /// Whether this cluster number names a cluster that exists.
    ///
    /// 0 and 1 never do: they are the two table entries the format reserves
    /// for the media descriptor and the dirty flags, which is why a heap
    /// begins at 2 and why every offset here has a `- 2` in it.
    pub fn holds(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster - 2 < self.cluster_count
    }

    /// Where a cluster's bytes begin.
    pub fn cluster_offset(&self, cluster: u32) -> Option<u64> {
        if !self.holds(cluster) {
            return None;
        }
        u64::from(cluster - 2)
            .checked_mul(self.cluster_bytes())?
            .checked_add(self.heap_start)
    }

    /// How much of the disk a file of this length occupies: its length rounded
    /// up to a whole cluster.
    ///
    /// **Arithmetic rather than a walk of the table, and that is a decision
    /// about what is true rather than about what is fast.** Neither format has
    /// sparse files, so a chain is always exactly as long as the file needs.
    /// A chain that is *longer* than that is a cross-link or a lost cluster —
    /// damage, which `chkdsk` and `fsck.fat` exist to find and which kosha
    /// would be guessing at rather than measuring.
    pub fn allocation(&self, length: u64) -> u64 {
        let cluster = self.cluster_bytes();
        if cluster == 0 {
            return 0;
        }
        length.div_ceil(cluster).saturating_mul(cluster)
    }
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

/// Read the boot sector, whichever of the two formats it is.
///
/// **The two are told apart by exFAT's name, not by the shape of the BPB.**
/// An exFAT boot sector deliberately zeroes the bytes where FAT keeps its
/// sector size and its cluster size, so that software which does not know
/// about exFAT refuses it rather than reading it wrong — and this reader
/// checks the name first for the same reason from the other direction.
pub fn boot(sector: &[u8]) -> Result<Geometry, Unreadable> {
    if sector.len() < 512 {
        return Err(Unreadable::NotFat);
    }
    // Both formats end their boot sector with it, and it is the one byte pair
    // that says somebody meant this to be bootable rather than that it happens
    // to start with plausible numbers.
    if le16(sector, 0x1FE) != 0xAA55 {
        return Err(Unreadable::NotFat);
    }
    if &sector[0x03..0x0B] == b"EXFAT   " {
        return exfat(sector);
    }
    fat(sector)
}

/// FAT12, FAT16 or FAT32, which is decided at the end rather than the start.
///
/// **Nothing in the boot sector says which.** The string at 0x36 says
/// "FAT16   " on plenty of FAT12 volumes and is explicitly documented as not
/// being the answer. The only answer is the number of clusters, which comes
/// out of five other fields multiplied and subtracted — so the width of the
/// table is the *last* thing known about a FAT volume, not the first.
fn fat(sector: &[u8]) -> Result<Geometry, Unreadable> {
    // `BS_jmpBoot`, which the specification requires and which keeps this from
    // firing on a file that happens to end its first sector with 0x55 0xAA.
    if sector[0] != 0xEB && sector[0] != 0xE9 {
        return Err(Unreadable::NotFat);
    }
    // `BPB_Media`, which is 0xF8 for a fixed disk and one of a handful of
    // values for the floppy formats. Nothing else is legal.
    if sector[0x15] < 0xF0 {
        return Err(Unreadable::NotFat);
    }

    let sector_size = u32::from(le16(sector, 0x0B));
    if !SECTOR_SIZES.contains(&sector_size) {
        return Err(Unreadable::NotFat);
    }
    let cluster_sectors = u32::from(sector[0x0D]);
    if cluster_sectors == 0 || !cluster_sectors.is_power_of_two() {
        return Err(Unreadable::NotFat);
    }
    let cluster_bytes = u64::from(sector_size) * u64::from(cluster_sectors);
    if cluster_bytes > MAX_CLUSTER_BYTES {
        return Err(Unreadable::Impossible);
    }

    let reserved = u64::from(le16(sector, 0x0E));
    if reserved == 0 {
        return Err(Unreadable::NotFat);
    }
    let fats = u64::from(sector[0x10]);
    if fats == 0 || fats > 4 {
        return Err(Unreadable::NotFat);
    }

    let root_entries = u64::from(le16(sector, 0x11));
    let small_sectors = u64::from(le16(sector, 0x13));
    let large_sectors = u64::from(le32(sector, 0x20));
    let total_sectors = if small_sectors != 0 {
        small_sectors
    } else {
        large_sectors
    };
    // NTFS shares this layout as far as here and then leaves both of these
    // zero, keeping its own sector count eight bytes further on. So a zero
    // here is not merely an impossible FAT volume — it is the ordinary way an
    // NTFS boot sector announces that it is not one.
    if total_sectors == 0 {
        return Err(Unreadable::NotFat);
    }

    let small_fat = u64::from(le16(sector, 0x16));
    let large_fat = u64::from(le32(sector, 0x24));
    let fat_sectors = if small_fat != 0 { small_fat } else { large_fat };
    if fat_sectors == 0 {
        return Err(Unreadable::NotFat);
    }

    // The root directory of a FAT12 or FAT16 volume sits between the tables
    // and the heap, and its length is part of where the heap starts. On FAT32
    // the entry count is zero and this is zero with it.
    let root_sectors = root_entries
        .checked_mul(ENTRY_BYTES as u64)
        .ok_or(Unreadable::Impossible)?
        .div_ceil(u64::from(sector_size));

    let first_data = reserved
        .checked_add(
            fats.checked_mul(fat_sectors)
                .ok_or(Unreadable::Impossible)?,
        )
        .and_then(|used| used.checked_add(root_sectors))
        .ok_or(Unreadable::Impossible)?;
    if first_data >= total_sectors {
        return Err(Unreadable::Impossible);
    }
    let cluster_count = (total_sectors - first_data) / u64::from(cluster_sectors);
    if cluster_count == 0 {
        return Err(Unreadable::Impossible);
    }
    let cluster_count = u32::try_from(cluster_count).map_err(|_| Unreadable::Impossible)?;

    let flavour = if cluster_count < FAT12_MAX_CLUSTERS {
        Flavour::Fat12
    } else if cluster_count < FAT16_MAX_CLUSTERS {
        Flavour::Fat16
    } else {
        Flavour::Fat32
    };

    // **Which table is the live one is a field, not an assumption.** FAT32
    // mirrors its table by default, but `BPB_ExtFlags` bit 7 turns mirroring
    // off and names one table as the only one being maintained. Reading the
    // first table on such a volume reads a copy that stopped being updated at
    // some point in the past, and every chain in it is stale.
    let active = if flavour == Flavour::Fat32 {
        let flags = le16(sector, 0x28);
        if flags & 0x0080 != 0 {
            u64::from(flags & 0x000F)
        } else {
            0
        }
    } else {
        0
    };
    if active >= fats {
        return Err(Unreadable::Impossible);
    }

    let root = if flavour == Flavour::Fat32 {
        let cluster = le32(sector, 0x2C);
        // Nothing else in the volume can be reached if this is wrong, so it is
        // checked here rather than failing as an empty root later.
        if cluster < 2 || cluster - 2 >= cluster_count {
            return Err(Unreadable::Impossible);
        }
        Root::Cluster(cluster)
    } else {
        if root_entries == 0 {
            return Err(Unreadable::Impossible);
        }
        Root::Fixed {
            start: (reserved + fats * fat_sectors) * u64::from(sector_size),
            bytes: root_sectors * u64::from(sector_size),
        }
    };

    Ok(Geometry {
        flavour,
        sector_size,
        cluster_sectors,
        cluster_count,
        fat_start: (reserved + active * fat_sectors) * u64::from(sector_size),
        fat_bytes: fat_sectors * u64::from(sector_size),
        heap_start: first_data * u64::from(sector_size),
        root,
        volume_bytes: total_sectors * u64::from(sector_size),
    })
}

/// exFAT, which says where everything is rather than leaving it to be derived.
///
/// Every offset below is a sector count read straight out of the boot sector,
/// where a FAT volume has them implied by the order things are laid out in.
/// That makes this shorter and it also makes it less forgiving: there is
/// nothing to cross-check the fields against except the volume's own length,
/// so that check is the whole of the validation.
fn exfat(sector: &[u8]) -> Result<Geometry, Unreadable> {
    // Bytes 0x0B to 0x40 must be zero. This is the field exFAT uses to make
    // sure software that reads a BPB refuses the volume rather than reading a
    // sector size of nothing, and checking it here is what keeps a FAT volume
    // whose label happens to read EXFAT from being taken for one.
    if sector[0x0B..0x40].iter().any(|byte| *byte != 0) {
        return Err(Unreadable::NotFat);
    }

    let sector_shift = u32::from(sector[0x6C]);
    let cluster_shift = u32::from(sector[0x6D]);
    if !(9..=12).contains(&sector_shift) || cluster_shift > 25 - sector_shift {
        return Err(Unreadable::Impossible);
    }
    let sector_size = 1u32 << sector_shift;
    let cluster_sectors = 1u32 << cluster_shift;

    let fats = u64::from(sector[0x6E]);
    if fats == 0 || fats > 2 {
        return Err(Unreadable::Impossible);
    }

    let volume_sectors = le64(sector, 0x48);
    let fat_offset = u64::from(le32(sector, 0x50));
    let fat_length = u64::from(le32(sector, 0x54));
    let heap_offset = u64::from(le32(sector, 0x58));
    let cluster_count = le32(sector, 0x5C);
    let root = le32(sector, 0x60);

    if fat_length == 0 || cluster_count == 0 {
        return Err(Unreadable::Impossible);
    }
    // Everything the boot sector names has to fit inside the volume it names,
    // and the heap has to come after the tables.
    let tables_end = fat_offset
        .checked_add(fats.checked_mul(fat_length).ok_or(Unreadable::Impossible)?)
        .ok_or(Unreadable::Impossible)?;
    if fat_offset == 0 || tables_end > heap_offset || heap_offset >= volume_sectors {
        return Err(Unreadable::Impossible);
    }
    let heap_sectors = u64::from(cluster_count)
        .checked_mul(u64::from(cluster_sectors))
        .ok_or(Unreadable::Impossible)?;
    if heap_offset
        .checked_add(heap_sectors)
        .ok_or(Unreadable::Impossible)?
        > volume_sectors
    {
        return Err(Unreadable::Impossible);
    }
    if root < 2 || root - 2 >= cluster_count {
        return Err(Unreadable::Impossible);
    }
    // **The one field here that is sixty-four bits wide, and so the one that
    // can describe a volume no arithmetic can express.** Every other number in
    // an exFAT boot sector is a `u32` and multiplying it by a sector size
    // cannot overflow; this one can, and a flipped bit in it was the first
    // thing the sweep found.
    let volume_bytes = volume_sectors
        .checked_mul(u64::from(sector_size))
        .ok_or(Unreadable::Impossible)?;

    // `VolumeFlags` bit 1 names the active table on a volume with two, which
    // only TexFAT ever made. The same reasoning as FAT32's mirroring bit: a
    // table that stopped being maintained still parses.
    let active = if fats == 2 && le16(sector, 0x6A) & 0x0002 != 0 {
        1
    } else {
        0
    };

    Ok(Geometry {
        flavour: Flavour::ExFat,
        sector_size,
        cluster_sectors,
        cluster_count,
        fat_start: (fat_offset + active * fat_length) * u64::from(sector_size),
        fat_bytes: fat_length * u64::from(sector_size),
        heap_start: heap_offset * u64::from(sector_size),
        root: Root::Cluster(root),
        volume_bytes,
    })
}

// ----------------------------------------------------------------- the table

/// Where a cluster's table entry is, and how many bytes of it to read.
///
/// **FAT12 is the reason this returns a width rather than a fixed size.** Its
/// entries are twelve bits, so they are stored a byte and a half apart and
/// every other one straddles a byte boundary — and, on a table whose length is
/// not a multiple of three, the last one can straddle a *sector* boundary.
/// That is why the caller reads bytes through a window rather than reading a
/// structure.
pub fn table_entry_at(flavour: Flavour, cluster: u32) -> Option<(u64, usize)> {
    let cluster = u64::from(cluster);
    match flavour {
        Flavour::Fat12 => Some((cluster + cluster / 2, 2)),
        Flavour::Fat16 => Some((cluster.checked_mul(2)?, 2)),
        Flavour::Fat32 | Flavour::ExFat => Some((cluster.checked_mul(4)?, 4)),
    }
}

/// Turn the bytes at [`table_entry_at`] into the cluster they name.
pub fn table_entry(flavour: Flavour, cluster: u32, bytes: &[u8]) -> Option<u32> {
    match flavour {
        Flavour::Fat12 => {
            let packed = u32::from(le16(bytes.get(..2)?, 0));
            // The low half of a byte and then the high half, or the other way
            // round, depending on whether the cluster number is odd.
            Some(if cluster & 1 == 1 {
                packed >> 4
            } else {
                packed & 0x0FFF
            })
        }
        Flavour::Fat16 => Some(u32::from(le16(bytes.get(..2)?, 0))),
        Flavour::Fat32 | Flavour::ExFat => Some(le32(bytes.get(..4)?, 0) & 0x0FFF_FFFF),
    }
}

/// Whether a table entry says the chain stops here.
///
/// The end-of-chain marker is every bit set, and the value one below it marks
/// a bad cluster — both of which end a chain, because there is nothing after
/// either. The width differs, so the comparison does.
pub fn chain_ends(flavour: Flavour, entry: u32) -> bool {
    match flavour {
        Flavour::Fat12 => entry >= 0x0FF7,
        Flavour::Fat16 => entry >= 0xFFF7,
        Flavour::Fat32 | Flavour::ExFat => entry >= 0x0FFF_FFF7,
    }
}

// --------------------------------------------------------- FAT's directories

/// One 32-byte record of a FAT directory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot<'a> {
    /// The first record never written. Nothing after it has ever been used, so
    /// a directory walk stops here rather than reading to the end of the
    /// cluster.
    End,
    /// Deleted, or a volume label. Skipped, and the walk goes on.
    Free,
    Long(Long),
    Short(Short<'a>),
}

/// A piece of a long name. They are stored in reverse, last piece first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Long {
    /// The low five bits are which piece this is, counting from one. The 0x40
    /// bit marks the piece that is stored first and read last.
    pub ordinal: u8,
    pub last: bool,
    /// A checksum of the short name this belongs to, which is what ties a run
    /// of pieces to the record that follows them.
    pub checksum: u8,
    pub chars: [u16; LONG_NAME_CHARS],
}

/// A real directory record: a name, a size, and where the bytes start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Short<'a> {
    /// Eight characters of name and three of extension, space-padded, with no
    /// separator between them.
    pub raw: &'a [u8],
    pub attributes: u8,
    /// `DIR_NTRes`, which VFAT used to record that a name a DOS machine would
    /// write in capitals is really lowercase.
    pub case: u8,
    pub cluster: u32,
    pub size: u32,
    pub mtime: u32,
}

impl Short<'_> {
    pub fn is_directory(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

/// Read one record, without deciding what to do about it.
pub fn slot(bytes: &[u8]) -> Slot<'_> {
    if bytes.len() < ENTRY_BYTES {
        return Slot::End;
    }
    match bytes[0] {
        0x00 => return Slot::End,
        FREE => return Slot::Free,
        _ => {}
    }
    let attributes = bytes[0x0B];
    if attributes & ATTR_LONG_MASK == ATTR_LONG_NAME {
        let mut chars = [0u16; LONG_NAME_CHARS];
        // Five here, six there, two at the end: the fields a long-name record
        // is chopped into, so that the bytes a short record uses for a cluster
        // number stay where software that predated long names expects them.
        for (index, at) in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
            .into_iter()
            .enumerate()
        {
            chars[index] = le16(bytes, at);
        }
        return Slot::Long(Long {
            ordinal: bytes[0] & 0x1F,
            last: bytes[0] & 0x40 != 0,
            checksum: bytes[0x0D],
            chars,
        });
    }
    // A volume label is a record with a name and no file behind it. It is not
    // deleted, so it is not `Free` by the letter of the format — but it names
    // nothing, and reporting it as a zero-byte file in the root directory
    // would be inventing an entry.
    if attributes & ATTR_VOLUME_ID != 0 {
        return Slot::Free;
    }
    Slot::Short(Short {
        raw: &bytes[0..11],
        attributes,
        case: bytes[0x0C],
        cluster: (u32::from(le16(bytes, 0x14)) << 16) | u32::from(le16(bytes, 0x1A)),
        size: le32(bytes, 0x1C),
        mtime: timestamp(le16(bytes, 0x18), le16(bytes, 0x16)),
    })
}

/// The checksum that ties long-name records to the short record they belong to.
///
/// Eight lines of the specification, and the whole of what makes a long name
/// trustworthy: software that did not know about long names would delete or
/// rename the short record without touching them, and the checksum is how a
/// reader notices that the pieces in front of it belong to a name that is no
/// longer there.
pub fn checksum(raw: &[u8]) -> u8 {
    let mut sum = 0u8;
    for byte in raw.iter().take(11) {
        sum = sum.rotate_right(1).wrapping_add(*byte);
    }
    sum
}

/// Write a short name out as text: trailing spaces dropped, a dot put back in
/// between, and the lowercase bits honoured.
pub fn push_short_name(raw: &[u8], case: u8, out: &mut Vec<u8>) {
    if raw.len() < 11 {
        return;
    }
    let base = trimmed(&raw[0..8]);
    let extension = trimmed(&raw[8..11]);
    for (index, byte) in base.iter().enumerate() {
        // Only the first character is ever escaped, and only when the name
        // really begins with the byte that marks a deleted record.
        let byte = if index == 0 && *byte == REAL_E5 {
            FREE
        } else {
            *byte
        };
        push_oem(byte, case & CASE_BASE_LOWER != 0, out);
    }
    if !extension.is_empty() {
        out.push(b'.');
        for byte in extension {
            push_oem(*byte, case & CASE_EXT_LOWER != 0, out);
        }
    }
}

fn trimmed(field: &[u8]) -> &[u8] {
    let end = field
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |at| at + 1);
    &field[..end]
}

// -------------------------------------------------------- exFAT's directories

/// One 32-byte record of an exFAT directory.
///
/// **A file is three records at least, and up to nineteen.** One says it is a
/// file and how many records follow, one says where its bytes are and how many
/// there are, and the rest are its name fifteen characters at a time. So
/// nothing here can be understood on its own, which is why the reader holds a
/// state machine and this holds only the reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Record {
    /// Nothing has ever been written here or after it.
    End,
    /// Written once and deleted since.
    Unused,
    File {
        secondaries: u8,
        attributes: u16,
        mtime: u32,
        /// Fifteen-minute steps east of UTC, when the top bit says the field
        /// was written at all.
        offset: u8,
        /// What the whole set should add up to. See [`set_checksum`].
        checksum: u16,
    },
    Stream {
        flags: u8,
        name_units: u8,
        cluster: u32,
        length: u64,
    },
    Name {
        chars: [u16; EXFAT_NAME_CHARS],
    },
    /// The allocation bitmap, the upcase table, the volume label, or something
    /// invented after this reader. None of them is a file.
    Other,
}

impl Record {
    /// Whether an attributes field says directory.
    pub fn is_directory(attributes: u16) -> bool {
        attributes & EXFAT_ATTR_DIRECTORY != 0
    }

    /// Whether a stream's flags say its clusters are contiguous and the table
    /// has nothing to say about them.
    ///
    /// **The common case on a card straight out of a camera**, and the reason
    /// a large exFAT volume can be walked with almost no reads of the table at
    /// all: a file written in one go gets this, and so does every directory
    /// small enough never to have been extended.
    pub fn is_contiguous(flags: u8) -> bool {
        flags & EXFAT_NO_FAT_CHAIN != 0
    }
}

/// Fold one record of a set into the checksum the first record states.
///
/// **exFAT is the only format here whose directory entries are checked at
/// all**, and it is worth using. A file is three records at least and up to
/// nineteen, and a filesystem in the middle of creating one has written some
/// of them — an image copied off a running machine, which is the ordinary way
/// somebody gets one, catches that constantly. Without this, a half-written
/// set is read as a file with whatever length and cluster the records that did
/// land happen to contain: a wrong answer wearing a right one's clothes. With
/// it, the set is refused and the directory says it is partial.
///
/// The two bytes holding the checksum are skipped, which is why the first
/// record is not the same case as the rest.
pub fn set_checksum(mut checksum: u16, bytes: &[u8], first: bool) -> u16 {
    for (index, byte) in bytes.iter().enumerate().take(ENTRY_BYTES) {
        if first && (index == 2 || index == 3) {
            continue;
        }
        checksum = (checksum >> 1)
            .wrapping_add(checksum << 15)
            .wrapping_add(u16::from(*byte));
    }
    checksum
}

pub fn record(bytes: &[u8]) -> Record {
    if bytes.len() < ENTRY_BYTES {
        return Record::End;
    }
    let kind = bytes[0];
    if kind == 0 {
        return Record::End;
    }
    if kind & EXFAT_IN_USE == 0 {
        return Record::Unused;
    }
    match kind {
        EXFAT_FILE => Record::File {
            secondaries: bytes[0x01],
            attributes: le16(bytes, 0x04),
            mtime: exfat_timestamp(le32(bytes, 0x0C)),
            offset: bytes[0x17],
            checksum: le16(bytes, 0x02),
        },
        EXFAT_STREAM => Record::Stream {
            flags: bytes[0x01],
            name_units: bytes[0x03],
            cluster: le32(bytes, 0x14),
            length: le64(bytes, 0x18),
        },
        EXFAT_NAME => {
            let mut chars = [0u16; EXFAT_NAME_CHARS];
            for (index, char) in chars.iter_mut().enumerate() {
                *char = le16(bytes, 0x02 + index * 2);
            }
            Record::Name { chars }
        }
        _ => Record::Other,
    }
}

// --------------------------------------------------------------------- text

/// The upper half of code page 437.
///
/// **A short name is not ASCII and there is nothing on the disk that says what
/// it is.** The byte 0x81 in an eight-character name means u-umlaut on a DOS
/// machine, something else in Greece, and something else again in Poland — the
/// format never recorded which, because in 1981 the answer was whatever the
/// machine in front of you was set to.
///
/// 437 is the answer that is right most often: it is what DOS and every
/// version of Windows use as the default OEM page, it is what `mkfs.fat`
/// assumes, and it is what a volume formatted by a camera or a games console
/// will have used. It is also very nearly never reached — Windows has written
/// a long name alongside the short one for any name that is not plain
/// uppercase ASCII since 1995, and the long name is preferred here — so this
/// is the fallback for a disk older than that, or one whose long names a
/// pre-1995 tool removed.
const CP437: [u16; 128] = [
    0x00C7, 0x00FC, 0x00E9, 0x00E2, 0x00E4, 0x00E0, 0x00E5, 0x00E7, // 0x80
    0x00EA, 0x00EB, 0x00E8, 0x00EF, 0x00EE, 0x00EC, 0x00C4, 0x00C5, // 0x88
    0x00C9, 0x00E6, 0x00C6, 0x00F4, 0x00F6, 0x00F2, 0x00FB, 0x00F9, // 0x90
    0x00FF, 0x00D6, 0x00DC, 0x00A2, 0x00A3, 0x00A5, 0x20A7, 0x0192, // 0x98
    0x00E1, 0x00ED, 0x00F3, 0x00FA, 0x00F1, 0x00D1, 0x00AA, 0x00BA, // 0xA0
    0x00BF, 0x2310, 0x00AC, 0x00BD, 0x00BC, 0x00A1, 0x00AB, 0x00BB, // 0xA8
    0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561, 0x2562, 0x2556, // 0xB0
    0x2555, 0x2563, 0x2551, 0x2557, 0x255D, 0x255C, 0x255B, 0x2510, // 0xB8
    0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x255E, 0x255F, // 0xC0
    0x255A, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256C, 0x2567, // 0xC8
    0x2568, 0x2564, 0x2565, 0x2559, 0x2558, 0x2552, 0x2553, 0x256B, // 0xD0
    0x256A, 0x2518, 0x250C, 0x2588, 0x2584, 0x258C, 0x2590, 0x2580, // 0xD8
    0x03B1, 0x00DF, 0x0393, 0x03C0, 0x03A3, 0x03C3, 0x00B5, 0x03C4, // 0xE0
    0x03A6, 0x0398, 0x03A9, 0x03B4, 0x221E, 0x03C6, 0x03B5, 0x2229, // 0xE8
    0x2261, 0x00B1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00F7, 0x2248, // 0xF0
    0x00B0, 0x2219, 0x00B7, 0x221A, 0x207F, 0x00B2, 0x25A0, 0x00A0, // 0xF8
];

/// One byte of a short name, as UTF-8.
fn push_oem(byte: u8, lower: bool, out: &mut Vec<u8>) {
    if byte < 0x80 {
        let byte = if lower {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        out.push(byte);
        return;
    }
    // The lowercase bits only ever meant ASCII: they were a way of recording
    // that a name DOS would have stored in capitals was typed in lower case,
    // and they say nothing about a character that has no case in 437.
    let point = CP437[usize::from(byte) - 0x80];
    let mut buffer = [0u8; 4];
    if let Some(character) = char::from_u32(u32::from(point)) {
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    }
}

/// UTF-16 units as UTF-8, stopping at the first NUL.
///
/// **Unpaired surrogates are the case worth naming.** A name is written by
/// whatever wrote the disk, and Windows will happily store a lone surrogate
/// half in a filename; there is no character for it, so it becomes the
/// replacement character rather than failing the whole entry. A name that
/// cannot be rendered is still a name that takes up space.
pub fn push_utf16(units: &[u16], out: &mut Vec<u8>) {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let mut buffer = [0u8; 4];
    for character in char::decode_utf16(units[..end].iter().copied()) {
        let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    }
}

// --------------------------------------------------------------------- time

/// A FAT date and time, as seconds since 1970.
///
/// **Local time with no zone recorded**, which is the format's own limitation
/// and not something a reader can correct: the disk says "half past two" and
/// nothing on it says where. Taking it as UTC is what every other reader does
/// and is what keeps two runs of kosha over the same disk agreeing with each
/// other, which is what the number is for here.
pub fn timestamp(date: u16, time: u16) -> u32 {
    let year = i64::from(date >> 9) + 1980;
    let month = u32::from((date >> 5) & 0x0F);
    let day = u32::from(date & 0x1F);
    let hour = u32::from(time >> 11);
    let minute = u32::from((time >> 5) & 0x3F);
    // The seconds field counts in twos, which is why every FAT timestamp is
    // even and why the format needed a separate tenths-of-a-second byte for
    // creation times.
    let second = u32::from(time & 0x1F) * 2;

    // Zero where the date is not one. A FAT record that was never written
    // holds zeros, which decode to the zeroth day of the zeroth month of 1980
    // — so this is the ordinary case rather than the damaged one, and a file
    // with no date is better than a file dated the 30th of November 1979.
    crate::calendar::unix_seconds(year, month, day, hour, minute, second).unwrap_or(0)
}

/// An exFAT timestamp, which is a FAT date and time packed into one word.
pub fn exfat_timestamp(stamp: u32) -> u32 {
    timestamp((stamp >> 16) as u16, stamp as u16)
}

/// Apply the zone exFAT records alongside a timestamp.
///
/// The one thing exFAT fixed about FAT's clocks: a byte saying how far the
/// machine that wrote the file was from UTC, in fifteen-minute steps, as a
/// seven-bit two's-complement number. The top bit says the field was filled in
/// at all — a volume written by something that did not bother leaves it zero,
/// and zero without that bit means "not recorded" rather than "UTC".
pub fn with_zone(seconds: u32, offset: u8) -> u32 {
    if offset & 0x80 == 0 {
        return seconds;
    }
    // Seven bits, sign-extended: 0x40 and above is west of UTC.
    let steps = i32::from(offset & 0x7F);
    let steps = if steps >= 0x40 { steps - 0x80 } else { steps };
    let shift = steps * 15 * 60;
    u32::try_from(i64::from(seconds) - i64::from(shift)).unwrap_or(0)
}
