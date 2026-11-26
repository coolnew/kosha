// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! `kosha-fsops` — the only crate in this workspace that can modify the
//! filesystem, and it will not do so by accident.
//!
//! `kosha-core`, `kosha-analyze` and `kosha-cli` do not depend on this crate.
//! That is deliberate: "could this code path delete a file?" is answerable by
//! reading `Cargo.toml`, and the dependency graph is part of the safety
//! argument.
//!
//! Every mutation needs a [`ReadWriteGrant`], which has no public constructor:
//!
//! ```
//! use kosha_fsops::{Session, UserConfirmed};
//!
//! let mut session = Session::new(std::env::temp_dir());
//! assert!(session.grant().is_err(), "read-only by default");
//!
//! session.enable_read_write(UserConfirmed::from_dialog_acceptance());
//! let _grant = session.grant().expect("now permitted");
//! ```
//!
//! On top of that sit four more layers: the platform trash rather than
//! `unlink`, a reviewable [`ActionPlan`] instead of direct action, guards that
//! refuse system locations regardless of mode, and an append-only audit log.
//! A plan that would delete files stored only in the cloud also needs a
//! [`CloudConfirmed`], which only that plan's own warning can produce.

// `deny`, not `forbid`, for exactly one call: the `FSCTL_SET_COMPRESSION`
// ioctl in `compress`, which has no safe wrapper anywhere. Forbidding it here
// would mean the mutation lived in `kosha-core` instead -- which is where it
// used to live, and which made the sentence at the top of this file untrue.
// Every other `unsafe` in this crate is still a compile error.
#![deny(unsafe_code)]

pub mod audit;
pub mod cloud;
mod compress;
pub mod guard;
mod mode;
mod plan;
pub mod tool_cleanup;
pub mod workflows;

pub use audit::AuditLog;
pub use cloud::{CloudConfirmed, CloudEvidence, CloudRisk, CloudWarning, ItemCloudRisk};
pub use guard::Refusal;
pub use mode::{Mode, ModeError, ReadWriteGrant, Session, UserConfirmed};
pub use plan::{
    Action, ActionPlan, ExecutionReport, Fingerprint, Outcome, PlanItem, SkipReason, VolumeChange,
    WorkflowPreviewError,
};
