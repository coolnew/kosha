// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! An append-only record of everything this tool changed.
//!
//! If a user cannot answer "what did this thing do to my machine last Tuesday",
//! the tool has failed, no matter how good the numbers were. One JSON object
//! per line, never rotated automatically, openable from the UI.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::ItemCloudRisk;
use crate::plan::{Action, Outcome};

pub struct AuditLog {
    path: PathBuf,
    file: Option<File>,
}

impl AuditLog {
    /// Open (or create) the log in the platform data directory.
    ///
    /// A log we cannot open must never stop a user reclaiming their disk, so
    /// failure degrades to a no-op recorder rather than an error. The UI shows
    /// the path either way, so a silent failure is still visible.
    pub fn open_default() -> Self {
        match default_path() {
            Some(path) => Self::at(path),
            None => AuditLog {
                path: PathBuf::from("audit.jsonl"),
                file: None,
            },
        }
    }

    /// Open a log at an explicit path, creating the directory it lives in.
    pub fn at(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        AuditLog { path, file }
    }

    /// A recorder that writes nothing, for tests and dry runs.
    pub fn disabled() -> Self {
        AuditLog {
            path: PathBuf::new(),
            file: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether writes are actually landing anywhere.
    pub fn is_active(&self) -> bool {
        self.file.is_some()
    }

    /// Typed tool operation. Bytes reclaimed are deliberately unknown until a
    /// rescan, rather than copied from a profile's total size.
    pub(crate) fn record_tool(
        &mut self,
        cache: &Path,
        executable: &Path,
        outcome: &str,
    ) -> std::io::Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("audit log unavailable"))?;
        let line = format!(
            "{{\"time\":\"{}\",\"action\":\"uv cache prune\",\"reversible\":false,\"bytes\":null,\"outcome\":{},\"path\":{},\"executable\":{}}}\n",
            timestamp(),
            json_string(outcome),
            json_string(&cache.to_string_lossy()),
            json_string(&executable.to_string_lossy())
        );
        file.write_all(line.as_bytes())
    }

    pub fn record(&mut self, path: &Path, action: Action, bytes: u64, outcome: &Outcome) {
        self.record_item(path, action, bytes, outcome, None);
    }

    /// One record, with what was known about cloud-only files in it.
    ///
    /// `cloud` is `None` for an item with none, and then the line is
    /// byte-for-byte what it always was, so older readers see no change. With
    /// one, a `cloud` object is added: how many cloud-only files, what the
    /// evidence was, and whether the user accepted the cloud warning.
    pub(crate) fn record_item(
        &mut self,
        path: &Path,
        action: Action,
        bytes: u64,
        outcome: &Outcome,
        cloud: Option<(ItemCloudRisk, bool)>,
    ) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        // **The newline belongs to the record, not to a second write.**
        // `writeln!` goes through `write_fmt`, which hands the formatter one
        // piece at a time and so issues a `write` for the record and another
        // for the newline. The file is opened for append and there is one log
        // per user, shared by every kosha on the machine -- a window and a
        // command line deleting at the same time, or two command lines -- so
        // another process's record lands in the gap. Measured with four
        // writers and 2,000 records: 869 empty lines and 478 lines holding
        // two or three records end to end, in a file whose whole contract is
        // one JSON object per line. An append of a few hundred bytes to a
        // regular file is atomic against other appenders; two of them are
        // not, so this builds the line with its newline and writes once.
        let mut line = format!(
            r#"{{"time":"{}","action":"{}","reversible":{},"bytes":{},"outcome":"{}","path":{}"#,
            timestamp(),
            action.label(),
            action.is_reversible(),
            bytes,
            match outcome {
                Outcome::Done(_) => "done",
                // Distinct from both, because the line's `bytes` is then what
                // was actually removed rather than what was asked for, and a
                // reader of this log has to be able to tell those apart.
                Outcome::Partial { .. } => "partial",
                Outcome::WouldDo(_) => "dry-run",
                Outcome::Skipped(_) => "skipped",
                Outcome::Failed(_) => "failed",
            },
            json_string(&path.to_string_lossy()),
        );
        if let Some((risk, confirmed)) = cloud {
            line.push_str(&format!(
                r#","cloud":{{"offline_files":{},"evidence":"{}","confirmed":{}}}"#,
                risk.offline_files,
                risk.evidence.audit_name(),
                confirmed,
            ));
        }
        line.push_str("}\n");
        // A failed audit write is not worth aborting a deletion that already
        // happened, but it must not be silent either.
        if file.write_all(line.as_bytes()).is_err() {
            self.file = None;
        }
    }
}

/// Minimal RFC 3339, UTC.
///
/// Both this and the date arithmetic behind it live in `kosha_core::format`:
/// the exporter needs the same conversion, and the query language needs its
/// inverse, so one copy is the only way they cannot drift.
fn timestamp() -> String {
    kosha_core::format::now_rfc3339()
}

/// Escape a string for JSON. Paths contain backslashes, quotes and, on some
/// systems, control characters, so this cannot be skipped.
fn json_string(s: &str) -> String {
    kosha_core::json::escaped(s)
}

/// Where the log belongs, without opening or creating it.
///
/// Public because a front end has to be able to *show* it — the safety model
/// says the log is "openable from the UI" and the README lists that as one of
/// the layers — and asking `open_default` for the path would create the file
/// as a side effect of looking. A log that exists because somebody went
/// looking for it says the wrong thing: nothing has been changed.
pub fn where_it_lives() -> Option<PathBuf> {
    default_path()
}

/// Where the log belongs. See [`kosha_core::dirs`], which is also where the
/// settings file's location comes from — one definition of what this
/// application calls itself, rather than two that can drift apart.
fn default_path() -> Option<PathBuf> {
    Some(kosha_core::dirs::data_dir()?.join(FILE))
}

const FILE: &str = "audit.jsonl";

#[cfg(test)]
mod tests {
    use super::*;

    /// Asking where the log is does not create one.
    ///
    /// The window shows the path when somebody asks for the log, and a log
    /// that exists because they *looked* says the wrong thing: an empty file
    /// where "nothing has been changed" is the truth. `open_default` creates
    /// it, which is right for writing and wrong for showing.
    #[test]
    fn asking_where_the_log_lives_does_not_make_one() {
        let Some(path) = where_it_lives() else {
            return; // A platform that will not say where data belongs.
        };
        let before = path.exists();
        let again = where_it_lives();
        assert_eq!(again.as_deref(), Some(path.as_path()));
        assert_eq!(path.exists(), before, "asking created the file");
        assert!(
            path.ends_with("audit.jsonl"),
            "the log is not where the docs say: {}",
            path.display()
        );
    }

    /// One record per line, with more than one writer on the log.
    ///
    /// There is one log per user and every kosha on the machine appends to
    /// it — the window and the command line can be removing things at the
    /// same moment, and so can two command lines. The record used to go out
    /// as `writeln!`, which is a `write` for the object and a second one for
    /// the newline, and another writer's append landed in between: four
    /// writers and 2,000 records gave 869 empty lines and 478 lines carrying
    /// two or three objects end to end. Nothing was lost, but the file
    /// stopped being the thing its name promises, and a reader that takes a
    /// line at a time — which is the only way to read JSON Lines — fails on
    /// half of it.
    ///
    /// Four handles on one path rather than four processes, because that is
    /// what a test can start; they are separate `File`s opened for append,
    /// so the kernel sees what it would see from four processes.
    #[test]
    fn every_record_is_its_own_line_with_several_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let mut hands = Vec::new();
        for who in 0..4u32 {
            let path = path.clone();
            hands.push(std::thread::spawn(move || {
                let mut log = AuditLog::at(path);
                for n in 0..500u32 {
                    log.record(
                        &PathBuf::from(format!("/writer{who}/file{n}.bin")),
                        Action::Trash,
                        4096,
                        &Outcome::Done(Action::Trash),
                    );
                }
            }));
        }
        for hand in hands {
            hand.join().unwrap();
        }

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2000, "2,000 records did not make 2,000 lines");
        for line in &lines {
            assert!(
                line.starts_with(r#"{"time":""#) && line.ends_with('}'),
                "not one record on its own line: {line}"
            );
            assert!(!line.contains("}{"), "two records on one line: {line}");
        }
    }

    #[test]
    fn escapes_backslashes_and_quotes() {
        // A Windows path that also contains a quote character.
        let input = r#"C:\Users\a"b"#;
        assert_eq!(json_string(input), r#""C:\\Users\\a\"b""#);
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(json_string("a\nb\tc"), r#""a\nb\tc""#);
        assert_eq!(json_string("\u{1}"), r#""\u0001""#);
    }

    #[test]
    fn stamps_a_well_formed_instant() {
        // The date arithmetic itself is `kosha_core::format`'s, and tested
        // there against the leap day and the 1e9 epoch second. What is this
        // log's business is that every line carries a parseable one.
        let stamp = timestamp();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-');
        assert_eq!(stamp.as_bytes()[10], b'T');
    }

    #[test]
    fn writes_one_line_per_action() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("audit.jsonl");
        let mut log = AuditLog::at(path.clone());
        assert!(log.is_active(), "should create the parent directory");

        log.record(
            Path::new("/tmp/thing"),
            Action::Trash,
            4096,
            &Outcome::Done(Action::Trash),
        );
        drop(log);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains(r#""bytes":4096"#));
        assert!(text.contains(r#""outcome":"done""#));
        assert!(text.contains(r#""reversible":true"#));
        assert!(!text.contains("cloud"), "an ordinary item gained a field");
        assert!(text.ends_with("\"path\":\"/tmp/thing\"}\n"), "{text}");
    }

    /// The cloud object is added for an item with cloud-only files, inside the
    /// same one-line record, and says whether the warning was accepted.
    #[test]
    fn a_cloud_item_records_its_count_evidence_and_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut log = AuditLog::at(path.clone());
        let risk = ItemCloudRisk {
            offline_files: 3,
            evidence: crate::cloud::CloudEvidence::OfflinePlaceholder,
            is_cloud_only_file: false,
        };
        log.record_item(
            Path::new("/tmp/folder"),
            Action::Trash,
            8192,
            &Outcome::Done(Action::Trash),
            Some((risk, true)),
        );
        drop(log);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(
            text.ends_with(
                "\"path\":\"/tmp/folder\",\"cloud\":{\"offline_files\":3,\
                 \"evidence\":\"offline\",\"confirmed\":true}}\n"
            ),
            "{text}"
        );
    }
}
