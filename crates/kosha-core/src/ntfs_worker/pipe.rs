// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_WRITE_DATA, PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SYNCHRONIZE, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
#[cfg(feature = "fast-ntfs-worker")]
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, WaitForSingleObject};

const PREFIX: &str = r"\\.\pipe\KoshaFastNtfs-";
const CLIENT_ACCESS: u32 = FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// One local endpoint, holding the first and only server instance.
pub struct LocalServer {
    name: String,
    pipe: File,
}

impl LocalServer {
    pub fn create() -> io::Result<Self> {
        let mut random = [0u8; 16];
        // SAFETY: the system RNG accepts a null algorithm and fills this array.
        let status = unsafe {
            BCryptGenRandom(
                null_mut(),
                random.as_mut_ptr(),
                16,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "pipe random name failed: 0x{status:08x}"
            )));
        }
        let name = format!("{PREFIX}{:032x}", u128::from_be_bytes(random));
        let wide: Vec<_> = name.encode_utf16().chain([0]).collect();
        // SAFETY: this pseudo-handle refers to the current process.
        let process = unsafe { GetCurrentProcess() };
        let Some(crate::OwnerId::Sid(sid)) = crate::platform::process_owner(process) else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot identify pipe owner",
            ));
        };
        // Use the token's canonical SID and individual client rights. Generic
        // write would also grant the right to create a pipe instance.
        let acl: Vec<_> = format!("D:P(A;;0x{CLIENT_ACCESS:x};;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = null_mut();
        // SAFETY: acl is a terminated UTF-16 string and descriptor is writable.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                acl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: name and descriptor remain valid until the call returns.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                &attributes,
            )
        };
        let failure = (handle == INVALID_HANDLE_VALUE).then(io::Error::last_os_error);
        // SAFETY: the successful conversion returned a LocalAlloc descriptor.
        unsafe { LocalFree(descriptor) };
        if let Some(error) = failure {
            return Err(error);
        }
        // SAFETY: the successful pipe creation returned a new owned handle.
        let pipe = unsafe { File::from_raw_handle(handle) };
        Ok(Self { name, pipe })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait for one local connection. No request or response may be sent
    /// until the caller verifies the connected peer.
    pub fn accept(
        self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<ConnectedPipe> {
        let raw = self.pipe.as_raw_handle();
        // SAFETY: the operation keeps the live handle and OVERLAPPED valid
        // until the pending connection has completed or cancelled.
        let connected = unsafe {
            operation(raw, deadline, cancelled, |overlapped, _| {
                ConnectNamedPipe(raw, overlapped)
            })
        };
        match connected {
            Ok(_) => Ok(ConnectedPipe(self.pipe)),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) => {
                Ok(ConnectedPipe(self.pipe))
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => {
                Err(io::Error::new(io::ErrorKind::Interrupted, error))
            }
            Err(error) => Err(error),
        }
    }
}

/// Open only the local Kosha rendezvous name. Identification-only QoS denies
/// the server the ability to impersonate the elevated client.
pub fn open_local_client(name: &str) -> io::Result<ConnectedPipe> {
    if !name
        .strip_prefix(PREFIX)
        .is_some_and(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid fast NTFS pipe name",
        ));
    }
    OpenOptions::new()
        .access_mode(CLIENT_ACCESS)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(name)
        .map(ConnectedPipe)
}

#[derive(Debug)]
pub struct ConnectedPipe(File);

impl AsHandle for ConnectedPipe {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl ConnectedPipe {
    #[cfg(feature = "fast-ntfs-worker")]
    pub(super) fn is_connected(&self) -> bool {
        // SAFETY: the pipe is live and all optional output pointers are null.
        unsafe {
            PeekNamedPipe(
                self.0.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
            ) != 0
        }
    }

    /// Create a bounded I/O view after verifying the peer. Keep the same
    /// absolute deadline for all reads and writes in a request.
    pub fn transfer<'a>(
        &'a self,
        deadline: Instant,
        cancelled: &'a dyn Fn() -> bool,
    ) -> Transfer<'a> {
        Transfer {
            pipe: self,
            deadline,
            cancelled,
        }
    }
}

pub struct Transfer<'a> {
    pipe: &'a ConnectedPipe,
    deadline: Instant,
    cancelled: &'a dyn Fn() -> bool,
}

impl Read for Transfer<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let raw = self.pipe.0.as_raw_handle();
        let length = buffer.len().min(u32::MAX as usize) as u32;
        // SAFETY: buffer and raw remain live until operation drains all pending I/O.
        let result = unsafe {
            operation(raw, self.deadline, self.cancelled, |overlapped, count| {
                ReadFile(raw, buffer.as_mut_ptr(), length, count, overlapped)
            })
        };
        match result {
            Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            result => result.map(|count| count as usize),
        }
    }
}

impl Write for Transfer<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let raw = self.pipe.0.as_raw_handle();
        let length = buffer.len().min(u32::MAX as usize) as u32;
        // SAFETY: buffer and raw remain live until operation drains all pending I/O.
        unsafe {
            operation(raw, self.deadline, self.cancelled, |overlapped, count| {
                WriteFile(raw, buffer.as_ptr(), length, count, overlapped)
            })
        }
        .map(|count| count as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // There is no user-space buffer. FlushFileBuffers would wait for the
        // receiver to read, outside the operation deadline.
        check_deadline(self.deadline, self.cancelled)
    }
}

fn check_deadline(deadline: Instant, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "fast NTFS pipe cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "fast NTFS pipe timed out",
        ));
    }
    Ok(())
}

/// The caller supplies an overlapped-capable handle and keeps any buffers
/// captured by start valid through the return of this function.
unsafe fn operation(
    pipe: HANDLE,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
    start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32,
) -> io::Result<u32> {
    check_deadline(deadline, cancelled)?;
    // SAFETY: no attributes or name, manual reset, initially nonsignaled.
    let event = unsafe { CreateEventW(null_mut(), 1, 0, null_mut()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned an owned handle.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let mut overlapped = OVERLAPPED {
        hEvent: event.as_raw_handle(),
        ..Default::default()
    };
    let mut count = 0;
    if start(&mut overlapped, &mut count) != 0 {
        return Ok(count);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        return Err(error);
    }

    // Keep the operation and any borrowed buffers alive even if the caller's
    // cancellation callback panics. Once completed, this guard does nothing.
    struct Pending<'a> {
        pipe: HANDLE,
        overlapped: &'a mut OVERLAPPED,
        done: bool,
    }
    impl Drop for Pending<'_> {
        fn drop(&mut self) {
            if !self.done {
                let mut ignored = 0;
                // SAFETY: the operation storage and buffers are still live.
                // Cancel may race completion; always drain the operation.
                unsafe {
                    CancelIoEx(self.pipe, self.overlapped);
                    GetOverlappedResult(self.pipe, self.overlapped, &mut ignored, 1);
                }
            }
        }
    }
    let mut pending = Pending {
        pipe,
        overlapped: &mut overlapped,
        done: false,
    };
    loop {
        check_deadline(deadline, cancelled)?;
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(20))
            .as_millis() as u32;
        // SAFETY: this event belongs only to the pending operation above.
        match unsafe { WaitForSingleObject(event.as_raw_handle(), wait.max(1)) } {
            WAIT_TIMEOUT => {}
            WAIT_OBJECT_0 => {
                // SAFETY: the event signals that the pending I/O completed.
                let ok = unsafe { GetOverlappedResult(pipe, pending.overlapped, &mut count, 0) };
                pending.done = true;
                return if ok == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(count)
                };
            }
            _ => return Err(io::Error::last_os_error()),
        }
    }
}
