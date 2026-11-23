// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::cell::Cell;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;

use flate2::bufread::ZlibDecoder;

use super::{
    BEFORE_HEADER, BEFORE_TREE, FORMAT, Meta, Node, ReadError, SizeTree, cut_short, decode_header,
    finish_body, read_body, read_header_bytes,
};

/// Limits for a single untrusted version 2 snapshot stream.
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    /// Header and compressed body together.
    pub max_wire_bytes: u64,
    /// All decompressed body bytes, including side tables.
    pub max_body_bytes: u64,
    pub max_nodes: u64,
    pub max_name_bytes: u64,
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 64 << 20,
            max_body_bytes: 128 << 20,
            max_nodes: 1_000_000,
            max_name_bytes: 64 << 20,
        }
    }
}

/// A verified SSH reply for browsing as a read-only record. Paths in this
/// record are display labels. Keep this source type when exposing the tree
/// to an application so it cannot be treated as a live local scan.
pub struct RemoteRecord {
    meta: Meta,
    tree: SizeTree,
}

impl RemoteRecord {
    pub fn read(input: impl Read, limits: ReadLimits) -> Result<Self, ReadError> {
        let (mut meta, mut tree) = read_limited(input, limits)?;
        // Peer-provided path and labels are not local provenance. Keep the
        // measured counts and flags while assigning the source locally.
        let root = PathBuf::from("[SSH snapshot]");
        tree.root_path = root.clone();
        meta.root = root;
        meta.label = "SSH snapshot".into();
        meta.tool.clear();
        meta.image.clear();
        tree.stats.image = None;
        Ok(Self { meta, tree })
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The record's tree for analysis and display, with a synthetic root.
    pub fn tree(&self) -> &SizeTree {
        &self.tree
    }

    /// Transfer the validated metadata and tree to a read-only source.
    pub fn into_parts(self) -> (Meta, SizeTree) {
        (self.meta, self.tree)
    }
}

/// Read and verify one untrusted version 2 snapshot with hard input limits.
///
/// The stream must end at the snapshot boundary. The caller supplies an
/// appropriate transport timeout and keeps returned paths inert.
pub fn read_limited<R: Read>(input: R, limits: ReadLimits) -> Result<(Meta, SizeTree), ReadError> {
    let wire_exceeded = Cell::new(false);
    let body_exceeded = Cell::new(false);
    let result = (|| {
        let mut wire = BufReader::new(Meter::new(input, limits.max_wire_bytes, &wire_exceeded));
        let (format, header) = read_header_bytes(&mut wire).map_err(cut_short(BEFORE_HEADER))?;
        if format != FORMAT {
            return Err(ReadError::Damaged(
                "the bounded reader requires snapshot version 2",
            ));
        }
        let meta = decode_header(format, &header)?;
        if meta.import.is_some() {
            return Err(ReadError::Damaged(
                "a remote scan reply must describe a scan",
            ));
        }
        check("node count", meta.entries, limits.max_nodes)?;

        let decoder = ZlibDecoder::new(&mut wire);
        let mut body = Meter::new(decoder, limits.max_body_bytes, &body_exceeded);
        let tree =
            read_body(&meta, format, &mut body, Some(limits)).map_err(cut_short(BEFORE_TREE))?;
        finish_body(&mut body)?;
        if !wire.fill_buf()?.is_empty() {
            return Err(ReadError::Damaged(
                "there are bytes after the snapshot stream",
            ));
        }
        Ok((meta, tree))
    })();
    // The compression layer can wrap errors. Record which meter hit its
    // boundary independently so the user sees the actual limit.
    if wire_exceeded.get() {
        return Err(ReadError::Limit {
            resource: "wire byte",
            max: limits.max_wire_bytes,
        });
    }
    if body_exceeded.get() {
        return Err(ReadError::Limit {
            resource: "decoded byte",
            max: limits.max_body_bytes,
        });
    }
    result
}

pub(super) fn check(resource: &'static str, actual: u64, max: u64) -> Result<(), ReadError> {
    if actual > max {
        Err(ReadError::Limit { resource, max })
    } else {
        Ok(())
    }
}

struct Meter<'a, R> {
    input: R,
    remaining: u64,
    exceeded: &'a Cell<bool>,
}

impl<'a, R> Meter<'a, R> {
    fn new(input: R, remaining: u64, exceeded: &'a Cell<bool>) -> Self {
        Self {
            input,
            remaining,
            exceeded,
        }
    }
}

impl<R: Read> Read for Meter<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            if self.input.read(&mut [0])? == 0 {
                return Ok(0);
            }
            self.exceeded.set(true);
            return Err(io::Error::other("snapshot stream limit reached"));
        }
        let capacity = self.remaining.min(output.len() as u64) as usize;
        let read = self.input.read(&mut output[..capacity])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// Extra invariants for an untrusted tree. The caller already checked that
/// all array indices and name spans fit.
pub(super) fn check_tree(nodes: &[Node], names: &[u8]) -> Result<(), ReadError> {
    super::check_child_layout(nodes).map_err(ReadError::Damaged)?;
    if nodes[0].depth != 0 {
        return Err(ReadError::Damaged("the root depth must be zero"));
    }
    let mut used = 0u64;
    for (index, node) in nodes.iter().enumerate() {
        used += u64::from(node.name_len);
        if index == 0 {
            continue;
        }
        if nodes[node.parent as usize].depth.checked_add(1) != Some(node.depth) {
            return Err(ReadError::Damaged(
                "the tree depth exceeds 255 or is inconsistent",
            ));
        }
        let start = node.name_off as usize;
        let name = &names[start..start + node.name_len as usize];
        if !super::valid_path_component(name) {
            return Err(ReadError::Damaged("invalid snapshot path component"));
        }
    }
    // Even if a malicious arena points many nodes at the same invalid name,
    // a later platform conversion cannot copy more than the name-byte budget.
    if used > names.len() as u64 {
        return Err(ReadError::Damaged("name spans exceed the name arena"));
    }
    Ok(())
}
