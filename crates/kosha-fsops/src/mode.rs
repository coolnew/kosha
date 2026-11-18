// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Read-only by default, enforced by the type system.
//!
//! Every mutating function in this crate takes a [`ReadWriteGrant`], and
//! `ReadWriteGrant` has no public constructor. The only way to obtain one is
//! [`Session::grant`], which fails unless the session has been deliberately
//! switched into read-write mode.
//!
//! There is no `bool` a stray `if` can get wrong, and no code path anywhere in
//! the workspace can reach `remove` without first naming a type that says what
//! it is doing.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    ReadOnly,
    ReadWrite,
}

impl Mode {
    pub fn is_read_write(self) -> bool {
        self == Mode::ReadWrite
    }

    /// The label the UI shows in the title bar.
    ///
    /// Sentence case for both. The shouting used to be done in capitals,
    /// which is the weakest way to do it: a word in capitals is a word the eye
    /// still has to read. The window now turns the chip red and draws a red
    /// edge around itself, which is seen rather than read, and capitals on top
    /// of that were only harder to scan.
    pub fn label(self) -> &'static str {
        match self {
            Mode::ReadOnly => "Read-only",
            Mode::ReadWrite => "Read-write",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModeError {
    #[error("this session is read-only; switch to read-write mode to make changes")]
    ReadOnly,
}

/// Evidence that a human was shown what read-write mode permits and agreed.
///
/// Construction is explicit and named so that the confirmation step is visible
/// in a diff. A caller that fabricates one has bypassed the dialog on purpose,
/// which is exactly the sort of change a reviewer should notice.
#[derive(Debug)]
pub struct UserConfirmed {
    _private: (),
}

impl UserConfirmed {
    /// Call this only from the branch where the user accepted the confirmation
    /// dialog — never to satisfy the type checker.
    pub fn from_dialog_acceptance() -> Self {
        UserConfirmed { _private: () }
    }
}

/// Permission to modify the filesystem, scoped to one session and one borrow.
///
/// Neither `Clone` nor `Copy`, and it borrows the session, so it cannot outlive
/// the mode it was issued under: dropping back to read-only invalidates every
/// outstanding grant at compile time.
pub struct ReadWriteGrant<'a> {
    pub(crate) session: &'a Session,
}

impl ReadWriteGrant<'_> {
    /// The root every action must stay inside.
    pub fn root(&self) -> &Path {
        &self.session.root
    }
}

/// One run of the application against one scanned root.
#[derive(Debug)]
pub struct Session {
    root: PathBuf,
    mode: Mode,
}

impl Session {
    /// Open a session. Always read-only; there is no constructor that is not.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Session {
            root: root.into(),
            mode: Mode::ReadOnly,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switch into read-write mode.
    ///
    /// Never persisted: a restart returns to read-only, because a mode that
    /// survives a restart is a mode the user forgets they are in.
    pub fn enable_read_write(&mut self, _confirmed: UserConfirmed) {
        self.mode = Mode::ReadWrite;
    }

    pub fn return_to_read_only(&mut self) {
        self.mode = Mode::ReadOnly;
    }

    /// Obtain permission to modify the filesystem.
    pub fn grant(&self) -> Result<ReadWriteGrant<'_>, ModeError> {
        match self.mode {
            Mode::ReadWrite => Ok(ReadWriteGrant { session: self }),
            Mode::ReadOnly => Err(ModeError::ReadOnly),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_start_read_only() {
        let session = Session::new("/tmp");
        assert_eq!(session.mode(), Mode::ReadOnly);
        assert!(session.grant().is_err());
    }

    #[test]
    fn a_grant_needs_an_explicit_switch() {
        let mut session = Session::new("/tmp");
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        assert!(session.grant().is_ok());

        session.return_to_read_only();
        assert!(session.grant().is_err());
    }
}
