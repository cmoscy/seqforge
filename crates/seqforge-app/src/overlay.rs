//! Transient UI surfaces: bars, dialogs, modals.
//!
//! See [`docs/focus-refactor.md`](../../../docs/focus-refactor.md) §2.5.
//!
//! Three previously-independent `AppState` fields (`active_bar`,
//! `open_dialog`, `cli_status`) are collapsed into one [`OverlayStack`].
//! The stack is the single source of truth for which transient surfaces
//! are present; rendering still fans out to the appropriate site per
//! overlay kind (inline bars in the viewer pane; dialogs and status
//! windows at the top level of `update()`).
//!
//! ## Rendering model
//!
//! Overlay variants are heterogenous: a `FindBar` renders inline at
//! the top of the viewer; a `FileDialog` and `CliStatus` render as
//! top-level egui Windows. The stack does not unify rendering — each
//! render site asks the stack for the specific overlay it knows how
//! to draw. The unified concerns are:
//!
//! - **Key-context tags** — every overlay on the stack contributes its
//!   tag to [`crate::focus::KeyContext`]. The generic `"Overlay"` tag
//!   is also present whenever the stack is non-empty, so a single
//!   Escape binding in the keymap can dismiss any overlay.
//! - **Dismiss semantics** — `AppCommand::DismissOverlay` pops the top
//!   of stack regardless of which overlay is up.
//! - **Terminal yielding** — the terminal pane reads
//!   `!overlays.is_empty()` and releases keyboard capture while any
//!   overlay is active.

use std::collections::{HashMap, HashSet};

use egui::Key;
use egui_file_dialog::FileDialog;
use seqforge_core::CutSite;

use crate::command::AppCommand;

// ── Inline bar state ──────────────────────────────────────────────────────────

pub struct FindBar {
    pub pattern: String,
    pub mismatches: u8,
    needs_focus: bool,
}

impl Default for FindBar {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            mismatches: 0,
            needs_focus: true,
        }
    }
}

pub struct GoToBar {
    pub input: String,
    needs_focus: bool,
}

impl Default for GoToBar {
    fn default() -> Self {
        Self {
            input: String::new(),
            needs_focus: true,
        }
    }
}

pub struct EnzymeBar {
    pub input: String,
    needs_focus: bool,
    /// Names of multi-site enzymes whose per-site sub-rows are expanded.
    /// Stale entries (enzyme no longer active) are harmless.
    expanded: HashSet<String>,
}

impl Default for EnzymeBar {
    fn default() -> Self {
        Self {
            input: String::new(),
            needs_focus: true,
            expanded: HashSet::new(),
        }
    }
}

/// One occurrence of an enzyme in the sequence — enough to navigate to it.
#[derive(Clone, Copy)]
pub struct EnzymeSite {
    pub recognition_start: usize,
    pub recognition_end: usize,
}

/// One row in the enzyme overlay's results list: a currently-displayed enzyme,
/// its recognition pattern, and every site it cuts (ordered by position).
pub struct EnzymeRow {
    pub name: String,
    /// IUPAC recognition pattern (e.g. `"GGTCTC"`); empty for non-cutters.
    pub recognition: String,
    pub sites: Vec<EnzymeSite>,
}

/// Collapse the active view's enzyme state into display rows. Sites are grouped
/// in one pass over `cut_sites`, then joined against the active enzyme list so
/// non-cutting enzymes (e.g. preset members with zero sites) still appear.
/// Cutters sort first, then alphabetical; sites within a row sort by position.
pub fn enzyme_rows(active_enzymes: &[String], cut_sites: &[CutSite]) -> Vec<EnzymeRow> {
    let mut by: HashMap<&str, Vec<&CutSite>> = HashMap::new();
    for s in cut_sites {
        by.entry(s.enzyme.as_str()).or_default().push(s);
    }
    let mut rows: Vec<EnzymeRow> = active_enzymes
        .iter()
        .map(|name| {
            let group = by.get(name.as_str());
            let recognition = group
                .and_then(|v| v.first())
                .map(|s| s.recognition.clone())
                .unwrap_or_default();
            let mut sites: Vec<EnzymeSite> = group
                .map(|v| {
                    v.iter()
                        .map(|s| EnzymeSite {
                            recognition_start: s.recognition_start,
                            recognition_end: s.recognition_end,
                        })
                        .collect()
                })
                .unwrap_or_default();
            sites.sort_by_key(|s| s.recognition_start);
            EnzymeRow {
                name: name.clone(),
                recognition,
                sites,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.sites.is_empty())
            .cmp(&(b.sites.is_empty()))
            .then(a.name.cmp(&b.name))
    });
    rows
}

/// Command to select a site's recognition range and scroll it into view.
fn reveal(s: &EnzymeSite) -> AppCommand {
    AppCommand::RevealRange {
        start: s.recognition_start,
        end: s.recognition_end,
    }
}

/// Flip an enzyme's expansion state.
fn toggle(set: &mut HashSet<String>, name: &str) {
    if !set.remove(name) {
        set.insert(name.to_string());
    }
}

// ── Overlay variants ──────────────────────────────────────────────────────────

/// All transient UI that may be present at once. Variants are
/// open-ended for future palette / confirm / agent-prompt overlays.
pub enum Overlay {
    FindBar(FindBar),
    GoToBar(GoToBar),
    EnzymeBar(EnzymeBar),
    /// Boxed because `FileDialog` is ~2.4 KB — keeping it inline would
    /// blow up every `Overlay` value to that size.
    FileDialog(Box<FileDialog>),
    CliStatus(String),
}

impl Overlay {
    pub const TAG_FIND_BAR: &'static str = "FindBar";
    pub const TAG_GOTO_BAR: &'static str = "GoToBar";
    pub const TAG_ENZYME_BAR: &'static str = "EnzymeBar";
    pub const TAG_FILE_DIALOG: &'static str = "FileDialog";
    pub const TAG_CLI_STATUS: &'static str = "CliStatus";
    /// Generic "any overlay" marker — pushed onto the KeyContext stack
    /// whenever [`OverlayStack`] is non-empty. The keymap's Escape
    /// binding matches on this so one binding dismisses every kind.
    pub const TAG_ACTIVE: &'static str = "Overlay";

    pub fn tag(&self) -> &'static str {
        match self {
            Overlay::FindBar(_) => Self::TAG_FIND_BAR,
            Overlay::GoToBar(_) => Self::TAG_GOTO_BAR,
            Overlay::EnzymeBar(_) => Self::TAG_ENZYME_BAR,
            Overlay::FileDialog(_) => Self::TAG_FILE_DIALOG,
            Overlay::CliStatus(_) => Self::TAG_CLI_STATUS,
        }
    }
}

// ── Stack ─────────────────────────────────────────────────────────────────────

/// Ordered set of currently-active overlays. Order is z-order (bottom
/// of the `Vec` is bottom of the visual stack); the most-recently
/// pushed overlay is the "top" and the target of `DismissOverlay`.
#[derive(Default)]
pub struct OverlayStack {
    overlays: Vec<Overlay>,
}

impl OverlayStack {
    pub fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Push if no overlay of this kind is already present. Returns the
    /// tag that was pushed, or `None` if the kind was a duplicate.
    /// Callers (mostly `command::apply`) emit `OverlayPushed` events
    /// based on the returned tag.
    pub fn push_unique(&mut self, overlay: Overlay) -> Option<&'static str> {
        let tag = overlay.tag();
        if self.overlays.iter().any(|o| o.tag() == tag) {
            return None;
        }
        self.overlays.push(overlay);
        Some(tag)
    }

    /// Pop the topmost overlay. Returns its tag, or `None` if empty.
    pub fn pop(&mut self) -> Option<&'static str> {
        self.overlays.pop().map(|o| o.tag())
    }

    /// Remove the first overlay with the given tag (regardless of
    /// position). Used to dismiss a specific kind even when it isn't
    /// on top — e.g. `DismissCliStatus` doesn't care about z-order.
    pub fn pop_kind(&mut self, tag: &'static str) -> Option<&'static str> {
        let idx = self.overlays.iter().position(|o| o.tag() == tag)?;
        let removed = self.overlays.remove(idx);
        Some(removed.tag())
    }

    /// Tags to layer onto `KeyContext` before keymap dispatch. Emits
    /// every overlay's specific tag plus `"Overlay"` once if any
    /// overlay is present.
    pub fn context_tags(&self) -> impl Iterator<Item = &'static str> + '_ {
        let active = if self.overlays.is_empty() {
            None
        } else {
            Some(Overlay::TAG_ACTIVE)
        };
        self.overlays.iter().map(|o| o.tag()).chain(active)
    }

    /// Typed accessor: returns the active `FindBar` if any. The Find
    /// and GoTo bars are mutually exclusive — `push_unique` rejects a
    /// second bar of either kind.
    pub fn find_bar_mut(&mut self) -> Option<&mut FindBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::FindBar(b) => Some(b),
            _ => None,
        })
    }

    pub fn goto_bar_mut(&mut self) -> Option<&mut GoToBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::GoToBar(b) => Some(b),
            _ => None,
        })
    }

    /// Cheap, non-mutating check used by the renderer to skip building the
    /// enzyme results list when the bar isn't open.
    pub fn has_enzyme_bar(&self) -> bool {
        self.overlays
            .iter()
            .any(|o| matches!(o, Overlay::EnzymeBar(_)))
    }

    pub fn enzyme_bar_mut(&mut self) -> Option<&mut EnzymeBar> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::EnzymeBar(b) => Some(b),
            _ => None,
        })
    }

    pub fn file_dialog_mut(&mut self) -> Option<&mut FileDialog> {
        self.overlays.iter_mut().find_map(|o| match o {
            Overlay::FileDialog(d) => Some(d.as_mut()),
            _ => None,
        })
    }

    pub fn cli_status(&self) -> Option<&str> {
        self.overlays.iter().find_map(|o| match o {
            Overlay::CliStatus(m) => Some(m.as_str()),
            _ => None,
        })
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render whichever Find / GoTo bar is currently active, inline at
/// the top of the viewer pane. Returns the [`AppCommand`] produced
/// by the bar (submission or dismiss); `None` if no bar is active or
/// the user didn't interact this frame.
///
/// The bar's own keyboard handling covers Enter (submit from focused
/// input) but **no longer** handles Escape — that lives in the keymap
/// (`keymap::KEYMAP`) gated on the `"Overlay"` context tag, so Escape
/// works regardless of which widget has egui focus.
/// `enzyme_rows` describes the active view's currently-displayed enzymes; it
/// hydrates the enzyme bar's results list so re-opening the bar (⌘E) shows
/// what's on screen. Ignored by the Find / GoTo bars. Pass `&[]` where there
/// is no active view (e.g. the Welcome tab).
pub fn show_inline_bar(
    stack: &mut OverlayStack,
    ui: &mut egui::Ui,
    enzyme_rows: &[EnzymeRow],
) -> Option<AppCommand> {
    if let Some(b) = stack.find_bar_mut() {
        return render_find_bar(b, ui);
    }
    if let Some(b) = stack.goto_bar_mut() {
        return render_goto_bar(b, ui);
    }
    if let Some(b) = stack.enzyme_bar_mut() {
        return render_enzyme_bar(b, ui, enzyme_rows);
    }
    None
}

fn bar_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 4))
}

fn render_find_bar(b: &mut FindBar, ui: &mut egui::Ui) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Find:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.pattern)
                    .hint_text("IUPAC pattern…")
                    .desired_width(200.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            ui.label("Mismatches:");
            let mismatch_resp = ui.add(egui::DragValue::new(&mut b.mismatches).range(0..=5));

            if ui.button("Find").clicked() {
                command = Some(AppCommand::SubmitFind {
                    pattern: b.pattern.clone(),
                    mismatches: b.mismatches,
                });
            }
            if ui.button("Clear").clicked() {
                command = Some(AppCommand::SubmitFind {
                    pattern: String::new(),
                    mismatches: 0,
                });
            }

            // Enter submits. egui idiom: a singleline `TextEdit` gives up
            // focus on Enter, so by the time control returns to us the
            // field's `has_focus()` is already false. `lost_focus()` is
            // the one-frame transition that catches the exact moment;
            // gating on `key_pressed(Enter)` distinguishes Enter-dismiss
            // from click-away and Tab-out.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && (text_resp.lost_focus() || mismatch_resp.lost_focus()) {
                command = Some(AppCommand::SubmitFind {
                    pattern: b.pattern.clone(),
                    mismatches: b.mismatches,
                });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    command = Some(AppCommand::DismissOverlay);
                }
            });
        });
    });

    ui.separator();
    command
}

fn render_enzyme_bar(
    b: &mut EnzymeBar,
    ui: &mut egui::Ui,
    rows: &[EnzymeRow],
) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Enzymes:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.input)
                    .hint_text("unique • unique+dual • non-cutters • type IIs • golden gate • moclo • EcoRI BamHI • none")
                    .desired_width(380.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            let has_input = !b.input.trim().is_empty();
            // Show = replace the set (Set). Add = union into it. Both leave the
            // overlay open so the set can be refined in place.
            if ui.button("Show").clicked() {
                command = Some(AppCommand::SubmitEnzymes { query: b.input.clone() });
            }
            if ui
                .add_enabled(has_input, egui::Button::new("＋ Add"))
                .on_hover_text("Add these enzymes to the current set")
                .clicked()
            {
                command = Some(AppCommand::AddEnzymes { query: b.input.clone() });
            }
            if ui.add_enabled(!rows.is_empty(), egui::Button::new("Clear All")).clicked() {
                command = Some(AppCommand::SubmitEnzymes { query: String::new() });
            }

            // Enter submits as Show (Set) — same lost_focus + key_pressed(Enter)
            // idiom as `render_find_bar`.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && text_resp.lost_focus() {
                command = Some(AppCommand::SubmitEnzymes { query: b.input.clone() });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    command = Some(AppCommand::DismissOverlay);
                }
            });
        });

        // ── Results list ────────────────────────────────────────────────
        // Hydrated from the active view, so opening the bar always reflects
        // what's currently drawn. Per row: ✕ removes that enzyme; clicking the
        // name jumps to its site (single) or expands per-site rows (multiple);
        // each site row jumps to that occurrence.
        if !rows.is_empty() {
            let total: usize = rows.iter().map(|r| r.sites.len()).sum();
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} enzyme{}, {} site{}",
                    rows.len(),
                    if rows.len() == 1 { "" } else { "s" },
                    total,
                    if total == 1 { "" } else { "s" },
                ))
                .weak()
                .small(),
            );
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for r in rows {
                        let n = r.sites.len();
                        let is_expanded = b.expanded.contains(&r.name);
                        ui.horizontal(|ui| {
                            if ui.small_button("✕").on_hover_text("Remove from set").clicked() {
                                command =
                                    Some(AppCommand::RemoveEnzyme { name: r.name.clone() });
                            }
                            // ▸/▾ for multi-site (expandable); spacer otherwise.
                            let prefix = match n {
                                0 | 1 => "   ",
                                _ if is_expanded => "▾ ",
                                _ => "▸ ",
                            };
                            let name = egui::RichText::new(format!("{prefix}{}", r.name)).monospace();
                            let name = if n == 0 { name.weak() } else { name };
                            let hover = match n {
                                0 => "No sites",
                                1 => "Jump to site",
                                _ if is_expanded => "Collapse",
                                _ => "Show sites",
                            };
                            let resp = ui
                                .add_enabled(n > 0, egui::SelectableLabel::new(is_expanded, name))
                                .on_hover_text(hover);
                            if resp.clicked() {
                                if n == 1 {
                                    command = Some(reveal(&r.sites[0]));
                                } else if n > 1 {
                                    toggle(&mut b.expanded, &r.name);
                                }
                            }
                            ui.label(egui::RichText::new(format!("×{n}")).small().weak());
                            if !r.recognition.is_empty() {
                                // Normal foreground (not weak) for readability;
                                // theme-adaptive across light/dark.
                                ui.label(
                                    egui::RichText::new(&r.recognition).monospace().small(),
                                );
                            }
                        });
                        // Per-site sub-rows for an expanded multi-site enzyme.
                        if n > 1 && is_expanded {
                            for s in &r.sites {
                                ui.horizontal(|ui| {
                                    ui.add_space(24.0);
                                    // 1-based position for display.
                                    if ui
                                        .small_button(format!("@ {}", s.recognition_start + 1))
                                        .on_hover_text("Jump to this site")
                                        .clicked()
                                    {
                                        command = Some(reveal(s));
                                    }
                                });
                            }
                        }
                    }
                });
        }
    });

    ui.separator();
    command
}

fn render_goto_bar(b: &mut GoToBar, ui: &mut egui::Ui) -> Option<AppCommand> {
    let mut command: Option<AppCommand> = None;

    bar_frame(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("Go to position:");
            let text_resp = ui.add(
                egui::TextEdit::singleline(&mut b.input)
                    .hint_text("1-based…")
                    .desired_width(100.0),
            );
            if b.needs_focus {
                text_resp.request_focus();
                b.needs_focus = false;
            }

            if ui.button("Go").clicked() {
                if let Ok(pos) = b.input.trim().parse::<usize>() {
                    command = Some(AppCommand::SubmitGoTo { position: pos });
                }
            }

            // Enter submits. See `render_find_bar` for the lost_focus +
            // key_pressed(Enter) idiom rationale.
            let enter_pressed = ui.input(|i| i.key_pressed(Key::Enter));
            if enter_pressed && text_resp.lost_focus() {
                if let Ok(pos) = b.input.trim().parse::<usize>() {
                    command = Some(AppCommand::SubmitGoTo { position: pos });
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    command = Some(AppCommand::DismissOverlay);
                }
            });
        });
    });

    ui.separator();
    command
}
