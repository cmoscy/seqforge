//! User configuration: settings, theme, and key bindings.
//!
//! Files live under the platform config directory (see [`paths`]):
//!
//! - `settings.toml` — behaviour + sizing (font sizes, layout fractions,
//!   minimap geometry, terminal shell).
//! - `themes/<name>.toml` — colour palettes. The active theme is named
//!   in `settings.toml`'s `theme = "..."` key.
//! - `keybindings.toml` — chord → action overrides. Anything not listed
//!   keeps its built-in binding from [`crate::keymap::KEYMAP`].
//!
//! None of these files are created at startup. The runtime is fully
//! usable with no config files present; "Open Settings" seeds the file
//! from an embedded template the first time it's opened (mirroring
//! VSCode). Deleting a file restores defaults.

use std::sync::Arc;

pub mod keybindings;
pub mod paths;
pub mod schema;
pub mod theme;

pub use keybindings::KeyBindings;
pub use schema::{LabelOverflow, MinimapSettings, Settings};
pub use theme::Theme;

/// Embedded default template files — used to seed user files on first
/// "Open …" and to ship the built-in themes without requiring disk
/// writes at startup.
pub mod defaults {
    pub const SETTINGS_TEMPLATE: &str =
        include_str!("defaults/settings.toml");
    pub const KEYBINDINGS_TEMPLATE: &str =
        include_str!("defaults/keybindings.toml");
    pub const DEFAULT_DARK: &str =
        include_str!("defaults/default-dark.toml");
    pub const DEFAULT_LIGHT: &str =
        include_str!("defaults/default-light.toml");
}

/// Composed, runtime-ready configuration. Wrapped in [`Arc`] on
/// [`crate::app::AppState`] so widgets can hold a cheap clone for the
/// duration of a frame.
#[derive(Debug, Clone)]
pub struct Config {
    pub settings: Settings,
    pub theme: Theme,
    pub keybindings: KeyBindings,
    /// Monotonic counter bumped on every successful reload. Caches that
    /// depend on theme/font choices include this in their key so they
    /// invalidate when the user reloads. Stays at 0 until the first
    /// reload.
    pub epoch: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            theme: Theme::default(),
            keybindings: KeyBindings::default(),
            epoch: 0,
        }
    }
}

impl Config {
    /// Read all config files from disk, layered on top of the built-in
    /// defaults. Parse errors are returned as human-readable strings for
    /// the caller to surface as toasts; never panics, never blocks startup.
    pub fn load() -> (Arc<Self>, Vec<String>) {
        let (cfg, warnings) = Self::load_with_epoch(0);
        (Arc::new(cfg), warnings)
    }

    /// Like [`load`] but increments the epoch. Used by "Reload Config".
    pub fn reload(prev_epoch: u64) -> (Arc<Self>, Vec<String>) {
        let (cfg, warnings) = Self::load_with_epoch(prev_epoch.wrapping_add(1));
        (Arc::new(cfg), warnings)
    }

    fn load_with_epoch(epoch: u64) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let settings = read_settings(&mut warnings);
        let theme = read_theme(&settings.theme, &mut warnings);
        let keybindings = read_keybindings(&mut warnings);
        (Self { settings, theme, keybindings, epoch }, warnings)
    }
}

fn read_settings(warnings: &mut Vec<String>) -> Settings {
    let path = paths::settings_path();
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Settings::default();
    };
    match toml::from_str::<Settings>(&body) {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("settings.toml: {e}"));
            Settings::default()
        }
    }
}

fn read_theme(name: &str, warnings: &mut Vec<String>) -> Theme {
    // Try user file first.
    let user_path = paths::theme_path(name);
    if let Ok(body) = std::fs::read_to_string(&user_path) {
        match toml::from_str::<Theme>(&body) {
            Ok(t) => return t,
            Err(e) => warnings.push(format!("theme {name:?} ({}): {e}", user_path.display())),
        }
    }
    // Embedded built-ins.
    let embedded = match name {
        "default-light" => Some(defaults::DEFAULT_LIGHT),
        "default-dark" => Some(defaults::DEFAULT_DARK),
        _ => None,
    };
    if let Some(body) = embedded {
        match toml::from_str::<Theme>(body) {
            Ok(t) => return t,
            Err(e) => warnings.push(format!("embedded theme {name:?}: {e}")),
        }
    }
    // Unknown theme name and no embedded match: fall back to built-in default.
    if !user_path.exists() {
        warnings.push(format!("theme {name:?} not found; using built-in defaults"));
    }
    Theme::default()
}

fn read_keybindings(warnings: &mut Vec<String>) -> KeyBindings {
    let path = paths::keybindings_path();
    let Ok(body) = std::fs::read_to_string(&path) else {
        return KeyBindings::default();
    };
    match keybindings::parse(&body) {
        Ok(k) => k,
        Err(e) => {
            warnings.push(format!("keybindings.toml: {e}"));
            KeyBindings::default()
        }
    }
}

/// Seed a config file from an embedded template if it doesn't already
/// exist. Used by the "Open Settings" / "Open Keybindings" / "Open
/// Theme" menu commands so the user always sees a populated file when
/// they click through, while the runtime stays defaults-only until
/// they actually customise something.
pub fn ensure_file_exists(
    path: &std::path::Path,
    template: &str,
) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, template)
}

/// Open a file in the user's editor / default app. Best-effort:
/// returns `Ok(())` if a launcher was spawned; the actual editor may
/// still fail asynchronously, which we leave to the user to notice.
pub fn open_in_editor(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Prefer xdg-open so the system default app launches (a GUI text
        // editor for files, a file manager for directories). Only fall
        // back to $VISUAL/$EDITOR when running headless (no display server),
        // where xdg-open would silently fail or open the wrong thing.
        let has_display = std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some();
        if has_display {
            std::process::Command::new("xdg-open").arg(path).spawn()?;
        } else if let Some(editor) =
            std::env::var_os("VISUAL").or_else(|| std::env::var_os("EDITOR"))
        {
            std::process::Command::new(editor).arg(path).spawn()?;
        } else {
            // Headless with no editor configured — best effort.
            std::process::Command::new("xdg-open").arg(path).spawn()?;
        }
        Ok(())
    }
}
