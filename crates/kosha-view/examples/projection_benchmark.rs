// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Reproducible in-memory disclosure benchmark; no source-file I/O or throughput claim.
use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
use kosha_query::Query;
use kosha_view::{Fields, Selection};
use std::{ffi::OsStr, path::PathBuf, time::Instant};

fn main() {
    for count in [10_000, 100_000, 200_000] {
        let mut names = NameArena::default();
        let mut nodes = Vec::with_capacity(count + 1);
        for i in 0..=count {
            let name = if i == 0 {
                "PRIVATE_ROOT".into()
            } else {
                format!("file-{i:06}.md")
            };
            let (name_off, name_len) = names.push(OsStr::new(&name));
            nodes.push(Node {
                logical: if i == 0 { count as u64 * 4096 } else { 4096 },
                allocated: if i == 0 { count as u64 * 4096 } else { 4096 },
                parent: 0,
                first_child: if i == 0 { 1 } else { 0 },
                child_count: if i == 0 { count as u32 } else { 0 },
                subtree_files: if i == 0 { count as u32 } else { 1 },
                name_off,
                name_len,
                mtime: 0,
                flags: if i == 0 { Flags::DIR } else { Flags::default() },
                depth: if i == 0 { 0 } else { 1 },
            });
        }
        let tree = SizeTree::from_parts(
            nodes,
            names,
            Owners::default(),
            PathBuf::from("/PRIVATE_ROOT"),
            ScanStats::default(),
        );
        let mut runs = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let view = kosha_view::publish(
                &tree,
                10000,
                Selection {
                    root: ".",
                    alias: "shared",
                    excludes: &[],
                    fields: Fields::default(),
                },
            )
            .unwrap();
            let projection_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            let bytes = kosha_view::encode(&view).unwrap();
            let encode_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = Instant::now();
            let prepared = kosha_view::read(bytes.as_slice()).unwrap();
            let load_ms = start.elapsed().as_secs_f64() * 1000.;
            let query = Query::parse("file and ext = md and size > 1kb").unwrap();
            prepared.check_query(&query).unwrap();
            let start = Instant::now();
            let matches = (1..prepared.view().entries.len())
                .filter(|i| prepared.matches(&query, *i, 10000))
                .count();
            let query_ms = start.elapsed().as_secs_f64() * 1000.;
            assert_eq!(matches, count);
            runs.push(
                serde_json::json!({"projection_ms":projection_ms,"encode_ms":encode_ms,
                "load_ms":load_ms,"query_ms":query_ms,"bytes":bytes.len()}),
            );
        }
        println!(
            "{}",
            serde_json::json!({"entries":count+1,"workload":"synthetic_flat_in_memory","runs":runs})
        );
    }
}
