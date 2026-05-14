use std::path::{Path, PathBuf};

/// Result of a CLI install attempt — shown in the UI and returned from `--install-cli`.
#[derive(Debug)]
pub struct InstallResult {
    pub target: PathBuf,
    pub was_updated: bool, // true if a previous symlink/file was replaced
}

/// Install the bundled `seqforge` CLI binary into the system PATH by symlinking
/// it from a well-known directory.
///
/// Target priority:
///   1. `/usr/local/bin`  — if writable (typical on macOS + Homebrew)
///   2. `~/.local/bin`    — created if absent (XDG standard, always user-owned)
///
/// Replaces any pre-existing file or symlink at the target path.
pub fn install_cli_to_path() -> Result<InstallResult, String> {
    let src = find_bundled_binary()?;
    let target_dir = choose_install_dir()?;
    let target = target_dir.join("seqforge");

    let was_updated = target.exists() || target.is_symlink();
    if was_updated {
        std::fs::remove_file(&target)
            .map_err(|e| format!("could not remove {}: {e}", target.display()))?;
    }

    std::os::unix::fs::symlink(&src, &target)
        .map_err(|e| format!("could not create symlink at {}: {e}", target.display()))?;

    Ok(InstallResult { target, was_updated })
}

/// Check whether a symlink already exists at the default install location.
pub fn is_installed() -> bool {
    choose_install_dir()
        .ok()
        .map(|d| d.join("seqforge").is_symlink())
        .unwrap_or(false)
}

// ── Internals ─────────────────────────────────────────────────────────────────

fn find_bundled_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate app binary: {e}"))?;
    let dir = exe.parent().ok_or("app binary has no parent directory")?;
    let candidate = dir.join("seqforge");
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

fn choose_install_dir() -> Result<PathBuf, String> {
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

fn is_writable(path: &Path) -> bool {
    // Probe by attempting to create a temp file.
    let probe = path.join(".seqforge_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_install_dir_returns_a_writable_path() {
        let dir = choose_install_dir().expect("should find an install dir");
        assert!(dir.is_dir());
        assert!(is_writable(&dir));
    }
}
