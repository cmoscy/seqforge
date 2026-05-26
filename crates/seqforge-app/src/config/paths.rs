//! Platform config directory and well-known file paths.
//!
//! All of these are *desired* paths — none are created at startup. The
//! settings/keybindings/theme files are seeded onto disk only when the
//! user invokes "Open Settings" / "Open Keybindings" / "Open Theme",
//! matching VSCode's behaviour. Until then the runtime uses embedded
//! defaults entirely.

use std::path::PathBuf;

use directories::ProjectDirs;

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("dev", "seqforge", "SeqForge")
}

/// Root config directory for SeqForge on this platform.
/// Falls back to `./.seqforge-config` if the platform lookup fails (rare,
/// but keeps the app usable on minimal/CI environments).
pub fn config_dir() -> PathBuf {
    project_dirs()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".seqforge-config"))
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

pub fn keybindings_path() -> PathBuf {
    config_dir().join("keybindings.toml")
}

pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

pub fn theme_path(name: &str) -> PathBuf {
    themes_dir().join(format!("{name}.toml"))
}
