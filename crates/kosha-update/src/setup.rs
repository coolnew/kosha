// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupScope {
    CurrentUser,
    AllUsers,
}

/// Fetch, authenticate, and open the interactive installer for this Setup copy.
#[cfg(windows)]
pub fn install_windows_update(release: &crate::Release) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use windows_sys::Win32::UI::{
        Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
        WindowsAndMessaging::SW_SHOWNORMAL,
    };

    let scope = windows_setup_scope().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Use the update guide for this copy of Kosha",
        )
    })?;
    let parent = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("Local application data is unavailable"))?
        .join("coolnew/kosha/updates/setup");
    fs::create_dir_all(&parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let verified =
        crate::prepare_package(release, crate::Package::Setup, staging.path())?.authenticate()?;
    if windows_setup_scope() != Some(scope) {
        return Err(io::Error::other(
            "The Setup installation changed during download",
        ));
    }
    let setup = staging
        .path()
        .join(release.package_name(crate::Package::Setup));
    let file = verified.save_and_pin_setup(&setup)?;
    let wide = |string: &OsStr| string.encode_wide().chain([0]).collect::<Vec<_>>();
    let verb = wide(OsStr::new(match scope {
        SetupScope::CurrentUser => "open",
        SetupScope::AllUsers => "runas",
    }));
    let path = wide(setup.as_os_str());
    let directory = wide(staging.path().as_os_str());
    let args = wide(OsStr::new(match scope {
        SetupScope::CurrentUser => "/KOSHAUPDATE /AddToPath=0",
        SetupScope::AllUsers => "/KOSHAUPDATE /ALLUSERS /AddToPath=0",
    }));
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        lpParameters: args.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: the structure is initialised and the NUL-terminated buffers live
    // through the synchronous launch. The image remains pinned until it returns.
    if unsafe { ShellExecuteExW(&raw mut launch) } == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(file);
    // Keep the image available while the Setup process uses it.
    let _ = staging.keep();
    Ok(())
}

/// Identify the Setup registration that matches the running executable.
/// An absent or ambiguous match requires the caller to leave the choice to the user.
pub fn windows_setup_scope() -> Option<SetupScope> {
    #[cfg(windows)]
    {
        use std::path::PathBuf;
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

        if super::managed_windows_package() {
            return None;
        }
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        unique_scope(
            [
                (HKEY_CURRENT_USER, SetupScope::CurrentUser),
                (HKEY_LOCAL_MACHINE, SetupScope::AllUsers),
            ]
            .into_iter()
            .filter_map(|(key, scope)| {
                let version = registry_string(key, "DisplayVersion")?;
                let location = PathBuf::from(registry_string(key, "InstallLocation")?);
                matches_setup(&executable, &location, version.to_str()?).then_some(scope)
            }),
        )
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(any(windows, test))]
fn unique_scope(mut scopes: impl Iterator<Item = SetupScope>) -> Option<SetupScope> {
    let scope = scopes.next()?;
    scopes.next().is_none().then_some(scope)
}

#[cfg(any(windows, test))]
fn matches_setup(executable: &std::path::Path, location: &std::path::Path, version: &str) -> bool {
    if version != kosha_core::version::NUMBER
        || !location.is_absolute()
        || !location.join("Uninstall.exe").is_file()
    {
        return false;
    }
    ["kosha.exe", "kosha-gui.exe"].into_iter().any(|name| {
        location
            .join(name)
            .canonicalize()
            .is_ok_and(|path| path == executable)
    })
}

#[cfg(windows)]
fn registry_string(
    key: windows_sys::Win32::System::Registry::HKEY,
    name: &str,
) -> Option<std::ffi::OsString> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Registry::{
        RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
    };

    let subkey: Vec<u16> =
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\KoshaDesktop\0"
            .encode_utf16()
            .collect();
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut buffer = [0_u16; 4096];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: the input strings are terminated and the output size matches the buffer.
    let status = unsafe {
        RegGetValueW(
            key,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut bytes,
        )
    };
    if status != 0 || !bytes.is_multiple_of(2) {
        return None;
    }
    let (terminator, value) = buffer.get(..bytes as usize / 2)?.split_last()?;
    (*terminator == 0 && !value.is_empty() && !value.contains(&0))
        .then(|| std::ffi::OsString::from_wide(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn requires_an_unambiguous_current_setup_registration() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("installed");
        let portable = root.path().join("portable");
        for directory in [&installed, &portable] {
            fs::create_dir(directory).unwrap();
            fs::write(directory.join("kosha.exe"), b"fixture").unwrap();
        }
        let cli = installed.join("kosha.exe").canonicalize().unwrap();
        let version = kosha_core::version::NUMBER;
        assert!(!matches_setup(&cli, &installed, version));
        fs::write(installed.join("Uninstall.exe"), b"fixture").unwrap();
        assert!(matches_setup(&cli, &installed, version));
        assert!(!matches_setup(&cli, &installed, "0.0.0"));
        assert!(!matches_setup(
            &portable.join("kosha.exe").canonicalize().unwrap(),
            &installed,
            version
        ));
        assert_eq!(
            unique_scope([SetupScope::AllUsers].into_iter()),
            Some(SetupScope::AllUsers)
        );
        assert_eq!(
            unique_scope([SetupScope::AllUsers, SetupScope::CurrentUser].into_iter()),
            None
        );
    }
}
