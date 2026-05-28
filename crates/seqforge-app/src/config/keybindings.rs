//! User key-binding overrides.
//!
//! The file format is a flat TOML table mapping a chord string
//! (e.g. `"cmd+shift+f"`) to an action name (e.g. `"find"`). Unknown
//! actions emit a warning at load time and are dropped.
//!
//! Lookups happen *before* the built-in [`crate::keymap::KEYMAP`] is
//! consulted, so an override always wins. Anything not listed falls
//! through to the built-in binding.
//!
//! TOML tables are parsed in document order (via `IndexMap`). When two
//! entries bind the same chord, **the first one listed wins**.

use egui::{Key, Modifiers};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::command::{AppCommand, SplitDirection};

/// User-facing action names. Subset of `AppCommand` variants that don't
/// carry user-supplied data — those are the only ones a bare keybinding
/// can fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PromptOpenFile,
    CloseDoc,
    Find,
    GoTo,
    NextTab,
    PrevTab,
    SplitHorizontal,
    SplitVertical,
    DismissOverlay,
    ReloadConfig,
}

impl Action {
    fn from_name(s: &str) -> Option<Action> {
        match s {
            "open_file" => Some(Action::PromptOpenFile),
            "close_doc" => Some(Action::CloseDoc),
            "find" => Some(Action::Find),
            "goto" => Some(Action::GoTo),
            "next_tab" => Some(Action::NextTab),
            "prev_tab" => Some(Action::PrevTab),
            "split_horizontal" => Some(Action::SplitHorizontal),
            "split_vertical" => Some(Action::SplitVertical),
            "dismiss_overlay" => Some(Action::DismissOverlay),
            "reload_config" => Some(Action::ReloadConfig),
            _ => None,
        }
    }

    pub fn to_command(self) -> AppCommand {
        match self {
            Action::PromptOpenFile => AppCommand::PromptOpenFile,
            Action::CloseDoc => AppCommand::CloseDoc,
            Action::Find => AppCommand::OpenFind,
            Action::GoTo => AppCommand::OpenGoTo,
            Action::NextTab => AppCommand::NextTab,
            Action::PrevTab => AppCommand::PrevTab,
            Action::SplitHorizontal => AppCommand::SplitPane {
                direction: SplitDirection::Horizontal,
            },
            Action::SplitVertical => AppCommand::SplitPane {
                direction: SplitDirection::Vertical,
            },
            Action::DismissOverlay => AppCommand::DismissOverlay,
            Action::ReloadConfig => AppCommand::ReloadConfig,
        }
    }
}

/// Parsed user overrides. `entries` preserves document order; first match wins.
#[derive(Debug, Clone, Default)]
pub struct KeyBindings {
    pub entries: Vec<(Modifiers, Key, Action)>,
}

#[derive(Deserialize)]
struct RawFile(IndexMap<String, String>);

/// Parse a `keybindings.toml` body. Unknown actions and unparseable
/// chords are logged to stderr and skipped, never fatal.
pub fn parse(body: &str) -> Result<KeyBindings, toml::de::Error> {
    let raw: RawFile = toml::from_str(body)?;
    let mut entries = Vec::with_capacity(raw.0.len());
    for (chord, action_name) in raw.0 {
        let Some(action) = Action::from_name(&action_name) else {
            eprintln!("[seqforge config] unknown action {action_name:?} in keybindings.toml");
            continue;
        };
        let Some((mods, key)) = parse_chord(&chord) else {
            eprintln!("[seqforge config] unparseable chord {chord:?} in keybindings.toml");
            continue;
        };
        entries.push((mods, key, action));
    }
    Ok(KeyBindings { entries })
}

/// Parse a chord like `"cmd+shift+f"` into `(Modifiers, Key)`.
/// Recognises `cmd`/`command`/`meta`, `ctrl`/`control`, `alt`/`option`,
/// `shift`. The last token is the key.
fn parse_chord(s: &str) -> Option<(Modifiers, Key)> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    if parts.is_empty() {
        return None;
    }
    let (key_tok, mod_toks) = parts.split_last()?;
    let mut mods = Modifiers::NONE;
    for tok in mod_toks {
        match tok.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" => mods = mods.plus(Modifiers::COMMAND),
            "ctrl" | "control" => mods = mods.plus(Modifiers::CTRL),
            "alt" | "option" | "opt" => mods = mods.plus(Modifiers::ALT),
            "shift" => mods = mods.plus(Modifiers::SHIFT),
            _ => return None,
        }
    }
    let key = parse_key(key_tok)?;
    Some((mods, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_preserves_document_order() {
        // First-listed binding wins, so document order must round-trip.
        let toml = r#"
"cmd+n" = "next_tab"
"cmd+p" = "prev_tab"
"cmd+f" = "find"
"#;
        let kb = parse(toml).expect("valid");
        let actions: Vec<_> = kb.entries.iter().map(|(_, _, a)| *a).collect();
        assert_eq!(actions, vec![Action::NextTab, Action::PrevTab, Action::Find]);
    }
}

fn parse_key(s: &str) -> Option<Key> {
    let lower = s.to_ascii_lowercase();
    // Letters and digits.
    if lower.len() == 1 {
        let c = lower.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Key::from_name(&c.to_ascii_uppercase().to_string());
        }
        if c.is_ascii_digit() {
            return Key::from_name(&format!("Num{c}"));
        }
    }
    // Common named keys; egui's `Key::from_name` covers many of these,
    // but punctuation and aliases need a hand here.
    let name = match lower.as_str() {
        "esc" | "escape" => "Escape",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "space" => "Space",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "[" => "OpenBracket",
        "]" => "CloseBracket",
        "\\" => "Backslash",
        "/" => "Slash",
        "-" => "Minus",
        "=" => "Equals",
        "," => "Comma",
        "." => "Period",
        ";" => "Semicolon",
        "'" => "Quote",
        "`" => "Backtick",
        other => other,
    };
    Key::from_name(name)
}
