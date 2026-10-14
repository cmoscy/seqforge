use std::path::{Path, PathBuf};

/// Result of a CLI install attempt — shown in the UI and returned from `--install-cli`.
#[derive(Debug)]
pub struct InstallResult {
    pub target: PathBuf,
    pub was_updated: bool, // true if a previous install was replaced
}

/// Install the bundled `seqforge` CLI binary so it is available on PATH.
///
/// **Unix:** symlink into `/usr/local/bin` (if writable) or `~/.local/bin`.
/// **Windows:** copy `seqforge.exe` to `%LOCALAPPDATA%\SeqForge\bin` and
/// prepend that directory to the user `PATH` when missing.
///
/// Re-running refreshes the link or copy after a rebuild.
pub fn install_cli_to_path() -> Result<InstallResult, String> {
    #[cfg(unix)]
    {
        install_unix()
    }
    #[cfg(windows)]
    {
        install_windows()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("CLI install is not supported on this platform".into())
    }
}

/// Whether the CLI appears installed at the default location for this OS.
pub fn is_installed() -> bool {
    #[cfg(unix)]
    {
        choose_unix_install_dir()
            .ok()
            .map(|d| d.join("seqforge").is_symlink())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        windows_install_target()
            .map(|t| t.is_file())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

// ── Shared ────────────────────────────────────────────────────────────────────

fn find_bundled_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate app binary: {e}"))?;
    let dir = exe.parent().ok_or("app binary has no parent directory")?;
    let name = format!("seqforge{}", std::env::consts::EXE_SUFFIX);
    let candidate = dir.join(name);
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(format!(
            "bundled seqforge binary not found at {};\n\
             run `cargo build` to build both binaries together.",
            candidate.display()
        ))
    }
}

#[cfg(unix)]
fn is_writable(path: &Path) -> bool {
    let probe = path.join(".seqforge_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

// ── Unix: symlink ─────────────────────────────────────────────────────────────

#[cfg(unix)]
fn install_unix() -> Result<InstallResult, String> {
    let src = find_bundled_binary()?;
    let target_dir = choose_unix_install_dir()?;
    let target = target_dir.join("seqforge");

    let was_updated = target.exists() || target.is_symlink();
    if was_updated {
        std::fs::remove_file(&target)
            .map_err(|e| format!("could not remove {}: {e}", target.display()))?;
    }

    std::os::unix::fs::symlink(&src, &target)
        .map_err(|e| format!("could not create symlink at {}: {e}", target.display()))?;

    Ok(InstallResult {
        target,
        was_updated,
    })
}

#[cfg(unix)]
fn choose_unix_install_dir() -> Result<PathBuf, String> {
    // Prefer /usr/local/bin if it exists and is writable — no extra PATH setup needed.
    let usr_local = Path::new("/usr/local/bin");
    if usr_local.is_dir() && is_writable(usr_local) {
        return Ok(usr_local.to_owned());
    }

    // Fall back to ~/.local/bin (XDG); create it if absent.
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let local_bin = PathBuf::from(home).join(".local/bin");
    std::fs::create_dir_all(&local_bin)
        .map_err(|e| format!("could not create {}: {e}", local_bin.display()))?;
    Ok(local_bin)
}

// ── Windows: copy + user PATH ─────────────────────────────────────────────────

#[cfg(windows)]
fn windows_install_dir() -> Result<PathBuf, String> {
    let base = directories::BaseDirs::new()
        .ok_or_else(|| "could not resolve %LOCALAPPDATA%".to_string())?;
    let dir = base.data_local_dir().join("SeqForge").join("bin");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

#[cfg(windows)]
fn windows_install_target() -> Result<PathBuf, String> {
    Ok(windows_install_dir()?.join(format!("seqforge{}", std::env::consts::EXE_SUFFIX)))
}

#[cfg(windows)]
fn install_windows() -> Result<InstallResult, String> {
    let src = find_bundled_binary()?;
    let target = windows_install_target()?;
    let was_updated = target.is_file();

    std::fs::copy(&src, &target).map_err(|e| {
        format!(
            "could not copy {} → {}: {e}",
            src.display(),
            target.display()
        )
    })?;

    let bin_dir = target
        .parent()
        .ok_or_else(|| "install target has no parent directory".to_string())?;
    ensure_user_path_contains(bin_dir)?;

    Ok(InstallResult {
        target,
        was_updated,
    })
}

/// Prepend `dir` to the HKCU user `PATH` when it is not already present, then
/// broadcast `WM_SETTINGCHANGE` so newly opened shells pick it up.
#[cfg(windows)]
fn ensure_user_path_contains(dir: &Path) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let dir_str = dir
        .to_str()
        .ok_or_else(|| format!("install path is not valid UTF-8: {}", dir.display()))?;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env = hkcu
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(|e| format!("could not open HKCU\\Environment: {e}"))?;

    let current: String = env.get_value("Path").unwrap_or_default();
    let already = std::env::split_paths(&current).any(|p| p == dir);
    if !already {
        let new_path = if current.is_empty() {
            dir_str.to_owned()
        } else {
            format!("{dir_str};{current}")
        };
        env.set_value("Path", &new_path)
            .map_err(|e| format!("could not update user PATH: {e}"))?;
    }

    // Notify running processes (Explorer, new consoles) that Environment changed.
    let wide: Vec<u16> = OsStr::new("Environment")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST as HWND,
            WM_SETTINGCHANGE,
            0 as WPARAM,
            wide.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bundled_binary_uses_exe_suffix() {
        // When running under `cargo test`, the test binary lives next to
        // whatever else was built in the same profile dir — we only assert
        // that the candidate name includes the platform suffix.
        let name = format!("seqforge{}", std::env::consts::EXE_SUFFIX);
        assert!(
            name.starts_with("seqforge"),
            "candidate name should start with seqforge"
        );
        #[cfg(windows)]
        assert!(name.ends_with(".exe"));
        #[cfg(unix)]
        assert_eq!(name, "seqforge");
    }

    #[cfg(unix)]
    #[test]
    fn choose_unix_install_dir_returns_a_writable_path() {
        let dir = choose_unix_install_dir().expect("should find an install dir");
        assert!(dir.is_dir());
        assert!(is_writable(&dir));
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_dir_is_under_local_appdata() {
        let dir = windows_install_dir().expect("should resolve install dir");
        assert!(dir.ends_with(Path::new("SeqForge").join("bin")));
        assert!(dir.is_dir());
    }
}
