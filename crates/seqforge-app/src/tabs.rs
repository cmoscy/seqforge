use serde::{Deserialize, Serialize};

use crate::browser::BrowserState;
use crate::command::{AppCommand, PendingCommand};
use crate::focus::{FocusScope, FocusState};
use crate::overlay::{self, OverlayStack};
use crate::terminal::TerminalPane;
use crate::viewer::SequenceView;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    FileBrowser,
    Viewer,
    Terminal,
}

pub struct TabViewer<'a> {
    pub browser: &'a mut BrowserState,
    pub workspace: &'a mut Workspace,
    pub seq_view: &'a mut SequenceView,
    pub pending_commands: &'a mut Vec<PendingCommand>,
    pub terminal: &'a mut Option<TerminalPane>,
    pub overlays: &'a mut OverlayStack,
    pub focus: &'a mut FocusState,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::FileBrowser => "Files".into(),
            Tab::Viewer => {
                let name = self.workspace.active_view().and_then(|v| {
                    let arc = self.workspace.buffers.get(v.buffer_id)?;
                    let buf = arc.read().ok()?;
                    Some(buf.name.clone())
                });
                match name {
                    Some(n) => format!("Viewer — {n}").into(),
                    None => "Sequence Viewer".into(),
                }
            }
            Tab::Terminal => "Terminal".into(),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        // Snapshot the pane rect before rendering so click-detection covers
        // the full pane area, not just whatever sub-rect content occupies.
        let pane_rect = ui.max_rect();
        let pane_scope = match tab {
            Tab::FileBrowser => FocusScope::Browser,
            Tab::Viewer => FocusScope::Viewer,
            Tab::Terminal => FocusScope::Terminal,
        };

        match tab {
            Tab::FileBrowser => {
                if let Some(path) = self.browser.show(ui) {
                    self.pending_commands.push((AppCommand::OpenFile(path), None));
                }
            }
            Tab::Viewer => {
                // Tab strip — one row per open view in the active pane.
                render_tab_strip(self.workspace, self.pending_commands, ui);

                // Inline Find / GoTo bar at the top of the viewer pane.
                if let Some(cmd) = overlay::show_inline_bar(self.overlays, ui) {
                    self.pending_commands.push((cmd, None));
                }
                // Resolve the active view + lock its buffer for the
                // duration of the paint. No active view ⇒ placeholder.
                //
                // Split `self` into disjoint borrows so the closure can
                // capture `seq_view` and `pending_commands` while
                // `workspace` provides the locking helper.
                let TabViewer {
                    workspace,
                    seq_view,
                    pending_commands,
                    ..
                } = self;
                let rendered = workspace.with_active_buffer(|view, buf, ann| {
                    seq_view.show(ui, view, buf, ann, pending_commands);
                });
                if rendered.is_err() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            "No file open.\nDouble-click a .gb or .fasta file in the browser.",
                        );
                    });
                }
            }
            Tab::Terminal => match self.terminal.as_mut() {
                Some(term) => {
                    // Terminal yields keyboard whenever *any* overlay is
                    // active — even a non-focus-capturing one — so Escape
                    // and other dismiss bindings work uniformly.
                    let terminal_has_focus = self.focus.scope == FocusScope::Terminal
                        && self.overlays.is_empty();
                    term.show(ui, terminal_has_focus);
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label("Terminal failed to initialise.\nCheck stderr for details.");
                    });
                }
            },
        }

        // Pane click → FocusScope. Geometry-only check; does not consume the
        // click, so the actual clicked widget (button, text field, terminal
        // grid) still handles it normally. Routed through AppCommand so the
        // focus mutation goes through the same single applier as everything
        // else.
        if ui.rect_contains_pointer(pane_rect)
            && ui.ctx().input(|i| i.pointer.any_pressed())
            && self.focus.scope != pane_scope
        {
            self.pending_commands.push((AppCommand::FocusPane(pane_scope), None));
        }
    }
}

// ── Tab strip ───────────────────────────────────────────────────────────────
//
// One row of selectable labels above the viewer area. Each entry shows the
// buffer's display name plus a small × close button. Clicks enqueue
// `SwitchTab` / `CloseTab` commands; the applier handles them.
//
// Hidden when the active pane has no views (the empty-pane placeholder
// renders below this anyway).

fn render_tab_strip(
    workspace: &Workspace,
    pending_commands: &mut Vec<PendingCommand>,
    ui: &mut egui::Ui,
) {
    let Some(pane) = workspace.active_pane() else { return };
    if pane.views.is_empty() {
        return;
    }
    let pane_id = pane.id;
    let active_idx = pane.active;

    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(4, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (idx, view) in pane.views.iter().enumerate() {
                    // Cheap read lock per tab — display name only.
                    let label = workspace
                        .buffers
                        .get(view.buffer_id)
                        .and_then(|arc| arc.read().ok().map(|b| b.name.clone()))
                        .unwrap_or_else(|| format!("{}", view.id));

                    let is_active = idx == active_idx;
                    // Label + close button as a tight pair. Active tab is
                    // emphasised; clicking a non-active tab switches to
                    // it; the × closes the tab regardless of active.
                    let resp = ui.selectable_label(is_active, &label);
                    if resp.clicked() && !is_active {
                        pending_commands.push((
                            AppCommand::SwitchTab { pane: pane_id, view: view.id },
                            None,
                        ));
                    }
                    if ui.small_button("×").on_hover_text("Close tab").clicked() {
                        pending_commands.push((
                            AppCommand::CloseTab { pane: pane_id, view: view.id },
                            None,
                        ));
                    }
                }
            });
        });
    ui.separator();
}
