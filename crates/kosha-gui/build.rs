// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Compile the Windows resource section.
//!
//! Without this the executable has the default icon everywhere Windows shows
//! one — Explorer, the taskbar, Alt-Tab, the Start menu — and its properties
//! dialog has no version tab. Neither is cosmetic to somebody deciding whether
//! to trust a binary they downloaded.
//!
//! Everything here is Windows-only and build-time-only; nothing is linked on
//! any other platform, and the crate builds identically without it.

fn main() {
    // The icon is checked in rather than generated here: `xtask-icon.py` draws
    // it, deterministically, and a build that shells out to Python to make a
    // picture is a build that fails on a machine without Python.
    println!("cargo:rerun-if-changed=../../assets/kosha.ico");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/kosha.ico");
        // Shown in the properties dialog. `FileDescription` is also what the
        // Task Manager lists a process under, which is why it is a sentence
        // rather than the crate name.
        res.set("FileDescription", "kosha — disk usage explorer");
        res.set("ProductName", "kosha");
        res.set("OriginalFilename", "kosha-gui.exe");
        res.set(
            "LegalCopyright",
            "Copyright (c) 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors. MIT OR Apache-2.0.",
        );

        // A failure here is not worth stopping a build over: the executable
        // works, it simply looks anonymous. Say so and carry on, rather than
        // making an icon a prerequisite for compiling a disk scanner.
        if let Err(error) = res.compile() {
            println!("cargo:warning=no Windows resources compiled: {error}");
        }
    }
}
