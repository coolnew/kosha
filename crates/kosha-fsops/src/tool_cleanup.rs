// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Typed owning-tool cleanup. No shell, plugins, custom arguments or downloads.
//! uv is a user-installed trusted native executable, not sandboxed by Kosha.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::{AuditLog, Fingerprint, ReadWriteGrant, guard};

const CACHE_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

#[derive(Clone, Debug)]
pub struct UvPrunePlan {
    root: PathBuf,
    cache: PathBuf,
    executable: PathBuf,
    cache_identity: Fingerprint,
    executable_hash: blake3::Hash,
}

impl UvPrunePlan {
    /// Inspect only: does not run uv, install it or touch the cache.
    pub fn prepare(cache: &Path, root: &Path) -> Result<Self, String> {
        if kosha_core::elevated() {
            return Err(
                "Run Kosha without administrator/root privileges for owning-tool cleanup".into(),
            );
        }
        let cache = guard::check(cache, root).map_err(|error| error.to_string())?;
        check_cache(&cache)?;
        let executable = find_uv()?;
        Self::with_executable(cache, kosha_core::path::canonical(root), executable)
    }

    fn with_executable(cache: PathBuf, root: PathBuf, executable: PathBuf) -> Result<Self, String> {
        if executable.starts_with(&cache) {
            return Err("The cleanup executable cannot live inside the cache it prunes".into());
        }
        let cache_identity = Fingerprint::of(&cache).map_err(|error| error.to_string())?;
        let executable_hash = native_hash(&executable)?;
        Ok(Self {
            root,
            cache,
            executable,
            cache_identity,
            executable_hash,
        })
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn executable_digest(&self) -> String {
        self.executable_hash.to_hex().to_string()
    }

    fn revalidate(&self, grant: &ReadWriteGrant<'_>) -> Result<(), String> {
        if kosha_core::elevated() {
            return Err("Owning-tool cleanup is unavailable while elevated".into());
        }
        self.revalidate_target(grant)
    }

    fn revalidate_target(&self, grant: &ReadWriteGrant<'_>) -> Result<(), String> {
        if kosha_core::path::canonical(grant.root()) != self.root {
            return Err("The scan root changed; prepare another tool cleanup preview".into());
        }
        let current = guard::check(&self.cache, grant.root()).map_err(|error| error.to_string())?;
        if current != self.cache
            || Fingerprint::of(&current).map_err(|error| error.to_string())? != self.cache_identity
        {
            return Err("The cache changed since preview; prepare another preview".into());
        }
        check_cache(&self.cache)?;
        if native_hash(&self.executable)? != self.executable_hash {
            return Err("The uv executable changed since preview; prepare another preview".into());
        }
        Ok(())
    }

    /// Must follow a real user confirmation of this exact plan. The owning
    /// tool performs pruning and locking; these checks do not sandbox its code.
    /// Cancel/timeout stops its process but cannot roll back completed pruning.
    pub fn execute(
        &self,
        grant: &ReadWriteGrant<'_>,
        audit: &mut AuditLog,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        self.revalidate(grant)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Tool cleanup cancelled before launch".into());
        }
        audit
            .record_tool(&self.cache, &self.executable, "started")
            .map_err(|error| format!("Tool cleanup requires a writable audit log: {error}"))?;
        let result = self.run(cancel);
        let outcome = match &result {
            Ok(()) => "done",
            Err(_) if cancel.load(Ordering::Relaxed) => "cancelled",
            Err(_) => "failed",
        };
        audit.record_tool(&self.cache, &self.executable, outcome).map_err(|error| format!("Tool finished but audit recording failed: {error}. Rescan to inspect the result."))?;
        result
    }

    fn command(&self, work: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args([
                "--no-config",
                "--offline",
                "--no-progress",
                "cache",
                "prune",
                "--cache-dir",
            ])
            .arg(&self.cache)
            .current_dir(work)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Runtime OS paths only. No API keys, proxy settings, UV_* overrides,
        // Python startup hooks, dynamic-loader overrides or project config.
        for name in ["SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // uv refuses a working directory inside its cache. Give it an empty
        // operation-owned home/project so it cannot discover the user's
        // default profile/config/state through a runtime fallback.
        for name in [
            "HOME",
            "USERPROFILE",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "LOCALAPPDATA",
            "APPDATA",
        ] {
            command.env(name, work);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        #[cfg(test)]
        command.stderr(Stdio::inherit());
        command
    }

    fn run(&self, cancel: &AtomicBool) -> Result<(), String> {
        let work = tempfile::Builder::new()
            .prefix("kosha-uv-prune-")
            .tempdir()
            .map_err(|error| format!("Cannot create isolated tool workspace: {error}"))?;
        let child = self
            .command(work.path())
            .spawn()
            .map_err(|error| format!("Cannot launch uv: {error}"))?;
        wait_for_child(child, cancel, Duration::from_secs(300))
    }
}

fn wait_for_child(
    mut child: std::process::Child,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Tool cleanup stopped or timed out; pruning may be partial. Rescan before continuing.".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "uv cache prune exited with {status}; pruning may be partial. Rescan before continuing."
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Could not monitor uv: {error}; rescan to inspect the result"
                ));
            }
        }
    }
}

fn check_cache(cache: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(cache).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("A real cache directory is required".into());
    }
    let marker = cache.join("CACHEDIR.TAG");
    let metadata = fs::symlink_metadata(&marker)
        .map_err(|_| "Missing cache-directory signature; use uv's own controls".to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Cache signature must be a regular file".into());
    }
    let mut prefix = [0; 43];
    fs::File::open(marker)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .map_err(|_| "Invalid cache-directory signature".to_string())?;
    if prefix != CACHE_TAG {
        return Err("Invalid cache-directory signature".into());
    }
    Ok(())
}

fn native_hash(path: &Path) -> Result<blake3::Hash, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o6000 != 0 {
            return Err("Privileged setuid/setgid cleanup executables are refused".into());
        }
    }
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 256 * 1024 * 1024
    {
        return Err("uv must be a regular native executable, at most 256 MiB".into());
    }
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)
        .map_err(|error| error.to_string())?;
    if magic != *b"\x7fELF"
        && &magic[..2] != b"MZ"
        && ![
            0xfeedface_u32,
            0xfeedfacf,
            0xcefaedfe,
            0xcffaedfe,
            0xcafebabe,
            0xbebafeca,
        ]
        .contains(&u32::from_be_bytes(magic))
    {
        return Err(
            "uv scripts and command wrappers are not supported; install the native binary".into(),
        );
    }
    let mut hash = blake3::Hasher::new();
    hash.update(&magic);
    let mut buffer = [0; 65536];
    let mut read = 4_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > 256 * 1024 * 1024 {
            return Err("uv executable exceeds size limit".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize())
}

fn find_uv() -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let directories: Vec<_> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .filter(|path| path.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    #[cfg(unix)]
    let directories = {
        let mut directories = directories;
        directories.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
        if let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            directories.push(home.join(".local/bin"));
        }
        directories
    };
    for directory in directories {
        let candidate = directory.join(name);
        if let Ok(resolved) = fs::canonicalize(candidate)
            && resolved.is_file()
        {
            let resolved = kosha_core::path::simplify(resolved);
            native_hash(&resolved)?;
            return Ok(resolved);
        }
    }
    Err("Native uv was not found in absolute PATH directories; install it yourself and reopen Kosha".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, UserConfirmed};

    #[test]
    #[ignore = "internal child-process fixture; called only by monitor test"]
    fn wait_fixture() {
        if std::env::var_os("KOSHA_UV_WAIT_FIXTURE").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn running_process_is_stopped_and_reaped_on_cancel_or_timeout() {
        for timeout in [false, true] {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tool_cleanup::tests::wait_fixture", "--ignored"])
                .env("KOSHA_UV_WAIT_FIXTURE", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let cancel = std::sync::Arc::new(AtomicBool::new(false));
            let signal = std::sync::Arc::clone(&cancel);
            let setter = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                if !timeout {
                    signal.store(true, Ordering::Relaxed);
                }
            });
            let start = Instant::now();
            let error = wait_for_child(
                child,
                &cancel,
                if timeout {
                    Duration::from_millis(150)
                } else {
                    Duration::from_secs(20)
                },
            )
            .unwrap_err();
            setter.join().unwrap();
            assert!(error.contains("stopped or timed out"));
            assert!(start.elapsed() < Duration::from_secs(10));
        }
    }

    #[test]
    fn cache_signature_and_native_binary_are_required() {
        let temp = tempfile::tempdir().unwrap();
        assert!(check_cache(temp.path()).is_err());
        fs::write(temp.path().join("CACHEDIR.TAG"), CACHE_TAG).unwrap();
        check_cache(temp.path()).unwrap();
        let script = temp.path().join("uv");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        assert!(native_hash(&script).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn privileged_tool_binaries_are_refused_without_execution() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("uv");
        fs::write(&executable, b"\x7fELFnever-executed").unwrap();
        for mode in [0o4755, 0o2755] {
            fs::set_permissions(&executable, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                native_hash(&executable)
                    .unwrap_err()
                    .contains("setuid/setgid")
            );
        }
    }

    #[test]
    fn plan_pins_arguments_cache_executable_and_grant_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(temp.path());
        let cache = root.join("cache with spaces; not shell");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("CACHEDIR.TAG"), CACHE_TAG).unwrap();
        let executable = root.join("uv");
        fs::write(&executable, b"\x7fELFfake-test-binary").unwrap();
        let plan =
            UvPrunePlan::with_executable(cache.clone(), root.clone(), executable.clone()).unwrap();
        let command = plan.command(&root.join("isolated-runtime"));
        assert_eq!(command.get_program(), executable.as_os_str());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.len(), 7);
        assert_eq!(args[6], cache.as_os_str());
        assert!(args.contains(&std::ffi::OsStr::new("--offline")));
        let mut session = Session::new(&root);
        assert!(session.grant().is_err());
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        plan.revalidate_target(&session.grant().unwrap()).unwrap();
        fs::write(&executable, b"\x7fELFreplaced-test-binary").unwrap();
        assert!(
            plan.revalidate_target(&session.grant().unwrap())
                .unwrap_err()
                .contains("executable changed")
        );
        let mut other = Session::new(root.join("elsewhere"));
        other.enable_read_write(UserConfirmed::from_dialog_acceptance());
        assert!(plan.revalidate_target(&other.grant().unwrap()).is_err());
    }

    #[test]
    fn cancelled_or_unaudited_plan_never_starts_a_process() {
        let temp = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(temp.path());
        let cache = root.join("uv");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("CACHEDIR.TAG"), CACHE_TAG).unwrap();
        let exe = root.join("uv-bin");
        fs::write(&exe, b"\x7fELFnever-executed").unwrap();
        let plan = UvPrunePlan::with_executable(cache, root.clone(), exe).unwrap();
        let mut session = Session::new(root);
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        let error = plan
            .execute(
                &session.grant().unwrap(),
                &mut AuditLog::disabled(),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        if kosha_core::elevated() {
            assert!(error.contains("unavailable while elevated"));
            return;
        }
        assert!(error.contains("writable audit"));
        let error = plan
            .execute(
                &session.grant().unwrap(),
                &mut AuditLog::disabled(),
                &AtomicBool::new(true),
            )
            .unwrap_err();
        assert!(error.contains("cancelled before launch"));
    }

    #[test]
    fn changed_cache_or_symlink_signature_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(temp.path());
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("CACHEDIR.TAG"), CACHE_TAG).unwrap();
        let executable = root.join("uv");
        fs::write(&executable, b"\x7fELFfake-test-binary").unwrap();
        let plan = UvPrunePlan::with_executable(cache.clone(), root.clone(), executable).unwrap();
        let mut session = Session::new(root);
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        fs::write(cache.join("CACHEDIR.TAG"), b"invalid signature").unwrap();
        assert!(plan.revalidate(&session.grant().unwrap()).is_err());
        #[cfg(unix)]
        {
            fs::remove_file(cache.join("CACHEDIR.TAG")).unwrap();
            std::os::unix::fs::symlink("missing", cache.join("CACHEDIR.TAG")).unwrap();
            assert!(check_cache(&cache).is_err());
        }
    }

    /// Explicit operator invocation only, against fresh owned fixture data.
    #[test]
    #[ignore = "requires an installed native uv; never downloads a tool"]
    fn installed_uv_prunes_only_a_disposable_cache() {
        let temp = tempfile::tempdir().unwrap();
        let root = kosha_core::path::canonical(temp.path());
        let cache = root.join("uv-cache");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("CACHEDIR.TAG"), CACHE_TAG).unwrap();
        let orphan = cache.join("archive-v0/orphan/synthetic.txt");
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, b"disposable synthetic cache").unwrap();
        let neighbor = root.join("valuable.txt");
        fs::write(&neighbor, b"keep outside cache").unwrap();
        let plan = UvPrunePlan::prepare(&cache, &root).unwrap();
        let mut session = Session::new(&root);
        session.enable_read_write(UserConfirmed::from_dialog_acceptance());
        let audit_path = root.join("audit.jsonl");
        let mut audit = AuditLog::at(audit_path.clone());
        plan.execute(
            &session.grant().unwrap(),
            &mut audit,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!orphan.exists(), "uv did not prune the synthetic orphan");
        assert_eq!(fs::read(neighbor).unwrap(), b"keep outside cache");
        let log = fs::read_to_string(audit_path).unwrap();
        assert_eq!(log.lines().count(), 2);
        assert!(log.contains("\"outcome\":\"started\""));
        assert!(log.contains("\"outcome\":\"done\""));
        assert!(log.contains("\"bytes\":null"));
    }
}
