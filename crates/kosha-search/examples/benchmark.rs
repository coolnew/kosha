// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `cargo run --release -p kosha-search --example benchmark -- [snapshot.kosha]`
//! No path: build a deterministic two-million-entry arena in memory. A path
//! reads an existing snapshot; neither mode scans or changes the user's files.

use kosha_core::{Flags, NameArena, Node, Owners, ScanStats, SizeTree};
use kosha_search::{SearchContext, SearchIndex, SearchMode, translate_with_mode};
use std::{
    ffi::OsStr,
    hint::black_box,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn synthetic(count: usize) -> SizeTree {
    let mut names = NameArena::with_capacity(count * 24);
    let (name_off, name_len) = names.push(OsStr::new("synthetic"));
    let mut nodes = Vec::with_capacity(count + 1);
    nodes.push(Node {
        logical: 0,
        allocated: 0,
        parent: 0,
        first_child: 1,
        child_count: count as u32,
        subtree_files: count as u32,
        name_off,
        mtime: 0,
        name_len,
        flags: Flags::DIR,
        depth: 0,
    });
    let extensions = [
        "bin", "txt", "jpg", "docx", "md", "c", "xlsx", "zip", "mp4", "rs",
    ];
    for i in 0..count {
        let name = format!("entry-{i:08}.{}", extensions[i % extensions.len()]);
        let (name_off, name_len) = names.push(OsStr::new(&name));
        let allocated = ((i % 4096) + 1) as u64 * 4096;
        nodes.push(Node {
            logical: allocated,
            allocated,
            parent: 0,
            first_child: 0,
            child_count: 0,
            subtree_files: 0,
            name_off,
            mtime: 1794956400,
            name_len,
            flags: Flags::default(),
            depth: 1,
        });
        nodes[0].allocated += allocated;
        nodes[0].logical += allocated;
    }
    SizeTree::from_parts(
        nodes,
        names,
        Owners::default(),
        "C:/synthetic".into(),
        ScanStats::default(),
    )
}

fn main() {
    let tree = if let Some(path) = std::env::args_os().nth(1) {
        let file = std::fs::File::open(&path).expect("read existing snapshot");
        let (meta, tree) = kosha_core::snapshot::read(file).expect("valid snapshot");
        println!(
            "source=snapshot root={} taken={} entries={}",
            tree.root_path().display(),
            meta.taken,
            tree.len()
        );
        tree
    } else {
        println!(
            "source=synthetic two-million root children; ten evenly distributed extensions; 4KiB..16MiB on disk"
        );
        synthetic(2_000_000)
    };
    println!(
        "arena_entries={} arena_memory_bytes={} packed_search_scratch_bytes={}",
        tree.len(),
        tree.memory_bytes(),
        tree.len().div_ceil(64) * 8
    );
    let started = Instant::now();
    let index = SearchIndex::new(Arc::new(tree));
    println!(
        "index_build_ms={:.3} index_extra_memory_bytes={}",
        started.elapsed().as_secs_f64() * 1e3,
        index.memory_bytes()
    );
    let context = SearchContext::new(1795003200, [2026, 11, 18]);
    let cancel = AtomicBool::new(false);
    let inputs = [
        "files greater than 10 MB",
        "files ending with md",
        "files which are c code",
        "word documents",
        "image files",
        "files named entry-00000020.bin",
        "files in synthetic",
        "files extensions md or txt",
        "documents except pdf",
        "files starting with \"report\"",
        "empty folders",
        "files created in last 24 hours",
        "files larger than ten megabytes",
        "folders with at least one hundred files",
        "files modified in October 2026",
        "files created two days ago",
        "files larger than half a GB",
        "files modified last Tuesday",
        "files apparent size over 10 MB",
        "files apparent size over 10 MB and allocated size under 2 MB",
        "files not apparent size between 1 and 2 MB",
    ];
    for input in inputs {
        let started = Instant::now();
        for _ in 0..10000 {
            black_box(translate_with_mode(
                black_box(input),
                context,
                SearchMode::Natural,
            ));
        }
        let translation_us = started.elapsed().as_secs_f64() * 1e6 / 10000.0;
        let translation = translate_with_mode(input, context, SearchMode::Natural);
        let query = translation.query.expect("benchmark input translates");
        let mut samples = vec![];
        let mut total = 0;
        for _ in 0..7 {
            let started = Instant::now();
            let result = index.run(&query, context.now, 2000, &cancel).unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1e3);
            total = result.total_matched;
            black_box(result);
        }
        samples.sort_by(f64::total_cmp);
        let started = Instant::now();
        let legacy = query.run(index.tree(), context.now, 2000);
        let legacy_ms = started.elapsed().as_secs_f64() * 1e3;
        assert_eq!(legacy.total_matched, total);
        println!(
            "input={input:?} translate_us={translation_us:.3} search_median_ms={:.3} search_min_ms={:.3} legacy_once_ms={legacy_ms:.3} matched={total}",
            samples[3], samples[0]
        );
    }
}
