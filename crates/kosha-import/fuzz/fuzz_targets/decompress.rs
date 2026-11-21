// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Any bytes through the decompression layer with the production window cap.

#![no_main]

use kosha_import::decompress::{Caps, MAX_WINDOW, decode_all};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let caps = Caps {
        max_input_bytes: 64 << 20,
        max_output_bytes: 64 << 20,
        max_window: MAX_WINDOW,
    };
    if let Ok(out) = decode_all(data, caps) {
        assert!(out.len() as u64 <= caps.max_output_bytes);
    }
});