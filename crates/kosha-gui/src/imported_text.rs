// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Wording that depends on whether the open record was imported.
//!
//! A listing imported from another tool is not a snapshot Kosha took, and there
//! may be no live folder on this machine to scan. Each string is one pure
//! function so it can be tested without a window. Native snapshots and live
//! scans keep their exact wording here too.

/// The refusal for every file action and for opening files on an imported
/// listing. It is the same in Read-only and Read-write mode.
pub fn imported_refusal() -> &'static str {
    "This is a listing imported from another tool. Nothing in it can be acted on."
}

/// The refusal for moving an entry to Trash from a past or disk-image view.
pub fn trash_refusal(imported: bool) -> &'static str {
    if imported {
        imported_refusal()
    } else {
        "Scan the live folder before moving files to Trash."
    }
}

/// The refusal for opening a file from a past or disk-image view.
pub fn open_files_refusal(imported: bool) -> &'static str {
    if imported {
        imported_refusal()
    } else {
        "This view records metadata, not file contents. Scan the live folder to open its files."
    }
}

/// The sentence shown when a record is opened. `age` is already formatted
/// ("11 minutes ago").
pub fn opened_notice(imported: bool, age: &str) -> String {
    if imported {
        format!(
            "Showing a listing exported {age} by another tool. Nothing here is the disk as it is now."
        )
    } else {
        format!("Showing a snapshot taken {age}. Nothing here is the disk as it is now.")
    }
}

/// The label of the disabled or enabled Move to Trash menu item.
pub fn trash_menu_label(writable: bool, live: bool, imported: bool) -> &'static str {
    if writable {
        "Move to Trash…"
    } else if live {
        "Move to Trash (Read-only)"
    } else if imported {
        "Move to Trash (not available for an imported listing)"
    } else {
        "Move to Trash (live scan required)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_refusals_are_identical_and_say_imported() {
        assert_eq!(trash_refusal(true), imported_refusal());
        assert_eq!(open_files_refusal(true), imported_refusal());
        assert!(!imported_refusal().contains("live folder"));
        assert!(!imported_refusal().contains("snapshot"));
    }

    #[test]
    fn native_refusals_keep_their_wording() {
        assert_eq!(
            trash_refusal(false),
            "Scan the live folder before moving files to Trash."
        );
        assert_eq!(
            open_files_refusal(false),
            "This view records metadata, not file contents. Scan the live folder to open its files."
        );
    }

    #[test]
    fn opened_notice_for_both_kinds() {
        assert_eq!(
            opened_notice(true, "11 minutes ago"),
            "Showing a listing exported 11 minutes ago by another tool. Nothing here is the disk as it is now."
        );
        assert_eq!(
            opened_notice(false, "11 minutes ago"),
            "Showing a snapshot taken 11 minutes ago. Nothing here is the disk as it is now."
        );
        assert!(opened_notice(true, "1 minute ago").contains("exported 1 minute ago by"));
        assert!(opened_notice(false, "just now").contains("taken just now."));
    }

    #[test]
    fn menu_label_per_case() {
        assert_eq!(trash_menu_label(true, true, false), "Move to Trash…");
        assert_eq!(
            trash_menu_label(false, true, false),
            "Move to Trash (Read-only)"
        );
        assert_eq!(
            trash_menu_label(false, false, false),
            "Move to Trash (live scan required)"
        );
        assert_eq!(
            trash_menu_label(false, false, true),
            "Move to Trash (not available for an imported listing)"
        );
    }
}
