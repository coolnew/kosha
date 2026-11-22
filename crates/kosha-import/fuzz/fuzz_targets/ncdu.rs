// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Any bytes into both listing importers: never a panic, never past the caps.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = kosha_import::Limits {
        max_input_bytes: 16 << 20,
        max_entries: 200_000,
        ..kosha_import::Limits::default()
    };
    let result = kosha_import::import_ncdu(data, "fuzz.json", "", std::time::UNIX_EPOCH, &limits);
    // The importer turns a panic on its worker into `Internal`; to the fuzzer
    // that is the crash it is looking for.
    assert!(!matches!(result, Err(kosha_import::ImportError::Internal)));
    let source = kosha_import::RcloneSource {
        source_name: "fuzz.json",
        label: "",
        root: "",
        tool: "",
        observed: None,
    };
    let result = kosha_import::import_rclone(data, &source, std::time::UNIX_EPOCH, &limits);
    assert!(!matches!(result, Err(kosha_import::ImportError::Internal)));
});
