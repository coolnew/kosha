// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "--version") {
        args.next();
        if args.next().is_none() {
            println!("{}", kosha_core::version::long());
            return std::process::ExitCode::SUCCESS;
        }
        return std::process::ExitCode::FAILURE;
    }
    match kosha_core::ntfs_worker::worker_main(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
