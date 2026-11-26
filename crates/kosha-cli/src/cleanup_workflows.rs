// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kosha_core::sayln;
use kosha_fsops::workflows::{self, Workflow};
use kosha_fsops::{AuditLog, Outcome, Session, UserConfirmed};

const HELP: &str = "\
kosha cleanup-workflow create NAME ROOT FILE [FILE ...]
kosha cleanup-workflow edit NAME FILE [FILE ...]
kosha cleanup-workflow list
kosha cleanup-workflow show NAME
kosha cleanup-workflow preview NAME
kosha cleanup-workflow run NAME

Use explicit file paths relative to ROOT. Quote names and paths with spaces.
Edit replaces the file list and retains the original location.
Run previews the current targets and requires interactive confirmation.";

pub fn run(args: &[String]) -> ExitCode {
    if args.is_empty() || matches!(args, [arg] if arg == "--help" || arg == "-h") {
        sayln!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = workflows::directory()
        .ok_or_else(|| io::Error::other("cleanup workflow data directory is unavailable"))
        .and_then(|dir| execute(&dir, args));
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(dir: &Path, args: &[String]) -> io::Result<bool> {
    let saved = workflows::list(dir)?;
    let get = |name: &str| {
        saved
            .get(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No workflow with that name."))
    };
    match args {
        [cmd] if cmd == "list" => {
            for (name, workflow) in saved {
                sayln!("{name}\t{:?}", workflow.location().root());
            }
        }
        [cmd, name, root, files @ ..] if cmd == "create" && !files.is_empty() => {
            let root = Path::new(root).canonicalize()?;
            let workflow = Workflow::new(&root, files.iter().map(PathBuf::from).collect())?;
            workflow.preview()?;
            workflows::save(dir, name, workflow, None)?;
        }
        [cmd, name, files @ ..] if cmd == "edit" && !files.is_empty() => {
            let original = get(name)?;
            let workflow = original.with_targets(files.iter().map(PathBuf::from).collect())?;
            workflow.preview()?;
            workflows::save(dir, name, workflow, Some(original))?;
        }
        [cmd, name] if cmd == "show" => {
            let workflow = get(name)?;
            sayln!("{name}: {:?}", workflow.location().root());
            for path in workflow.targets() {
                sayln!("  {path:?}");
            }
        }
        [cmd, name] if cmd == "preview" || cmd == "run" => {
            return review(dir, name, get(name)?, cmd == "run");
        }
        _ => return Err(io::Error::other(HELP)),
    }
    Ok(true)
}

fn review(dir: &Path, name: &str, workflow: &Workflow, run: bool) -> io::Result<bool> {
    if run && !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(io::Error::other(
            "Run requires an interactive terminal. Use preview for scripts.",
        ));
    }
    let plan = workflow.preview()?;
    let preview = plan.dry_run();
    let mut output = io::stdout().lock();
    writeln!(output, "Move to Trash in {:?}", workflow.location().root())?;
    for (path, outcome) in &preview.details {
        writeln!(output, "  {path:?}  {}", outcome_text(outcome))?;
    }
    if preview.skipped != 0 {
        return Err(io::Error::other(
            "The plan contains changed or cloud-only files. Review them before continuing.",
        ));
    }
    if !run {
        return Ok(true);
    }
    writeln!(
        output,
        "Type MOVE TO TRASH to approve these files, or press Enter to cancel."
    )?;
    output.flush()?;
    drop(output);
    let mut confirmation = String::new();
    io::stdin().read_line(&mut confirmation)?;
    if confirmation.trim_end_matches(['\r', '\n']) != "MOVE TO TRASH" {
        sayln!("Cancelled.");
        return Ok(false);
    }
    if workflows::list(dir)?.get(name) != Some(workflow) {
        return Err(io::Error::other(
            "The saved workflow changed. Preview it again.",
        ));
    }
    let mut audit = AuditLog::open_default();
    if !audit.is_active() {
        return Err(io::Error::other("The cleanup audit log is unavailable."));
    }
    let mut session = Session::new(workflow.location().root());
    session.enable_read_write(UserConfirmed::from_dialog_acceptance());
    let grant = session.grant().map_err(io::Error::other)?;
    let receipt = plan.execute_measured(&grant, None, &mut audit);
    for (path, outcome) in &receipt.details {
        sayln!("{path:?}  {}", outcome_text(outcome));
    }
    for volume in &receipt.volume_changes {
        match volume.delta() {
            Some(delta) => sayln!(
                "{:?}: observed free-space change {delta:+} bytes",
                volume.root
            ),
            None => sayln!("{:?}: free-space change unavailable", volume.root),
        }
    }
    Ok(receipt
        .details
        .iter()
        .all(|(_, outcome)| matches!(outcome, Outcome::Done(_))))
}

fn outcome_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done(_) => "Moved to Trash".into(),
        Outcome::WouldDo(_) => "Ready for review".into(),
        Outcome::Skipped(reason) => format!("Skipped: {reason}"),
        Outcome::Partial { why, .. } => format!("Partially completed: {why}"),
        Outcome::Failed(error) => format!("Failed: {error}"),
    }
}
