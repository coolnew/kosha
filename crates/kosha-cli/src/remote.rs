// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use kosha_remote::key_input::{self, Loaded};

type Error = Box<dyn std::error::Error>;
const USAGE: &str = "Usage: kosha remote <alias|user@host:port> --path <remote-path> [--agent-key <public-key-file>]";

pub fn run(args: impl Iterator<Item = OsString>) -> ExitCode {
    let args: Vec<_> = args.collect();
    if args.as_slice() == ["--help"] {
        kosha_core::sayln!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = run_args(&args);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kosha remote: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_args(args: &[OsString]) -> Result<(), Error> {
    let (target, path, agent_key) = match args {
        [target, flag, path] if flag == "--path" => (target, path, None),
        [target, flag, path, agent_flag, key]
            if flag == "--path" && agent_flag == "--agent-key" =>
        {
            (target, path, Some(Path::new(key)))
        }
        _ => return Err(USAGE.into()),
    };
    match (target.to_str(), path.to_str()) {
        (Some(target), Some(path)) if !path.is_empty() => scan(target, path, agent_key),
        _ => Err(USAGE.into()),
    }
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Settings {
    ssh_access: bool,
}

fn scan(target: &str, path: &str, agent_key: Option<&Path>) -> Result<(), Error> {
    let config_dir =
        kosha_core::dirs::config_dir().ok_or("Kosha settings directory is unavailable")?;
    let settings = match std::fs::read_to_string(config_dir.join("settings.json")) {
        Ok(input) => serde_json::from_str::<Settings>(&input)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
        Err(error) => return Err(error.into()),
    };
    if !settings.ssh_access {
        return Err(
            "SSH scanning is disabled. Set ssh-access to true in Kosha settings.json to enable it."
                .into(),
        );
    }
    #[cfg(not(any(unix, windows)))]
    if agent_key.is_some() {
        return Err("this build does not support SSH agent authentication".into());
    }

    #[cfg(any(unix, windows))]
    let agent_key = agent_key
        .map(|path| {
            kosha_remote::SelectedAgentKey::load(settings.ssh_access, path)
                .map_err(|error| format!("{}: {error}", path.display()))
        })
        .transpose()?;
    let prepared = kosha_remote::prepare_local_target(settings.ssh_access, target, &config_dir)?;
    let request = kosha_scan_wire::Request {
        path: path.to_owned(),
        max_depth: None,
        owners: false,
        cross_filesystems: false,
        dedup_hardlinks: true,
    };
    let record = kosha_remote::run_ssh(async {
        let (target, mut session) = prepared.connect_default(settings.ssh_access).await?;
        #[cfg(any(unix, windows))]
        if let Some(key) = agent_key {
            let session = key.authenticate(session, &target.user).await?;
            return Ok::<_, Error>(
                kosha_remote::scan_session(settings.ssh_access, &session, &request, |_| {}).await?,
            );
        }
        let mut rejected = false;
        for path in &target.identity_files {
            let Some(key) = load_identity(settings.ssh_access, path)? else {
                continue;
            };
            let (returned, accepted) =
                kosha_remote::try_authenticate_key(session, &target.user, Arc::new(key.key))
                    .await?;
            if accepted {
                return Ok::<_, Error>(
                    kosha_remote::scan_session(settings.ssh_access, &returned, &request, |_| {})
                        .await?,
                );
            }
            session = returned;
            rejected = true;
        }
        if rejected {
            return Err(kosha_remote::AuthenticationError::Rejected.into());
        }
        Err("no configured SSH private key was found".into())
    })??;
    let meta = record.meta();
    kosha_core::sayln!(
        "SSH snapshot: {} files, {} directories, {} apparent bytes",
        meta.files,
        meta.dirs,
        meta.bytes_logical
    );
    Ok(())
}

fn load_identity(enabled: bool, path: &Path) -> Result<Option<Loaded>, Error> {
    let key = match key_input::load(enabled, path, None) {
        Ok(key) => key,
        Err(key_input::Error::Read(std::io::ErrorKind::NotFound)) => return Ok(None),
        Err(key_input::Error::PassphraseRequired) => unlock(enabled, path)?,
        Err(error) => return Err(format!("{}: {error}", path.display()).into()),
    };
    if key.readable_by_others {
        eprintln!(
            "SSH key {} is readable by group or others. Use chmod 600 to restrict access.",
            path.display()
        );
    }
    Ok(Some(key))
}

fn unlock(enabled: bool, path: &Path) -> Result<Loaded, Error> {
    #[cfg(unix)]
    {
        let phrase = kosha_remote::unix_prompt::read()?.ok_or("SSH passphrase cancelled")?;
        Ok(key_input::load(enabled, path, Some(phrase.as_str()))?)
    }
    #[cfg(not(unix))]
    {
        let _ = (enabled, path);
        Err("this build does not support prompting for an SSH passphrase".into())
    }
}
