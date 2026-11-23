// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, RawFd};

use zeroize::Zeroizing;

use crate::key_input::{PASSPHRASE_BYTES, SecretText};

struct Terminal {
    fd: RawFd,
    saved: libc::termios,
}

impl Terminal {
    fn disable_echo(fd: RawFd) -> io::Result<Self> {
        let mut saved = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: termios writes into valid storage, and a successful call
        // initializes it. The caller owns the open stdin handle.
        if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let saved = unsafe { saved.assume_init() };
        let mut prompt = saved;
        // Read Ctrl-C as input so cancellation can restore the saved mode
        // before returning. The prompt handles line editing locally.
        prompt.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
        prompt.c_cc[libc::VMIN] = 1;
        prompt.c_cc[libc::VTIME] = 0;
        set_mode(fd, &prompt)?;
        Ok(Self { fd, saved })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = set_mode(self.fd, &self.saved);
    }
}

fn set_mode(fd: RawFd, mode: &libc::termios) -> io::Result<()> {
    // SAFETY: mode points to an initialized termios for the open terminal.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, mode) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Return None on Ctrl-C or Ctrl-D. Refuse input when stdin is not a terminal.
pub fn read() -> io::Result<Option<SecretText>> {
    let stdin = io::stdin().lock();
    // Bypass std's shared buffered stdin so it cannot retain a second copy.
    let mut input = File::from(stdin.as_fd().try_clone_to_owned()?);
    let terminal = Terminal::disable_echo(input.as_raw_fd())?;
    let mut output = io::stderr().lock();
    output.write_all(b"SSH passphrase: ")?;
    output.flush()?;
    let result = read_line(&mut input);
    drop(terminal);
    output.write_all(b"\n")?;
    result
}

fn read_line(input: &mut impl Read) -> io::Result<Option<SecretText>> {
    let mut bytes = Zeroizing::new(vec![0; PASSPHRASE_BYTES]);
    let mut len = 0;
    let mut next = Zeroizing::new([0]);
    loop {
        input.read_exact(next.as_mut())?;
        match next[0] {
            b'\r' | b'\n' => {
                return SecretText::from_bytes(bytes, len).map(Some).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "passphrase is not UTF-8")
                });
            }
            3 | 4 => return Ok(None),
            8 | 127 if len != 0 => {
                len -= 1;
                while len > 0 && bytes[len] & 0xc0 == 0x80 {
                    len -= 1;
                }
                bytes[len..].fill(0);
            }
            8 | 127 => {}
            byte if len < bytes.len() => {
                bytes[len] = byte;
                len += 1;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "passphrase is too long",
                ));
            }
        }
    }
}
