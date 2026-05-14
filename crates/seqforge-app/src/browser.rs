use egui_file_dialog::FileDialog;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const SEQUENCE_EXTS: &[&str] = &["gb", "gbk", "genbank", "fasta", "fa", "fna"];

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BrowserState {
    pub root: Option<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    #[serde(skip)]
    pub file_dialog: Option<FileDialog>,
}

impl BrowserState {
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut open_file: Option<PathBuf> = None;

        ui.horizontal(|ui| {
            if ui.button("Open Folder…").clicked() {
                let mut dialog = FileDialog::new();
                dialog.pick_directory();
                self.file_dialog = Some(dialog);
            }
        });

        // Handle drag-and-drop
        if let Some(dropped) = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .find(|f| f.path.as_ref().is_some_and(|p| p.is_dir()))
                .and_then(|f| f.path.clone())
        }) {
            self.root = Some(dropped);
            self.expanded.clear();
        }

        // File dialog update
        if let Some(dialog) = &mut self.file_dialog {
            dialog.update(ui.ctx());
            if let Some(picked) = dialog.picked() {
                self.root = Some(picked.to_owned());
                self.expanded.clear();
                self.file_dialog = None;
            } else if matches!(
                dialog.state(),
                egui_file_dialog::DialogState::Closed | egui_file_dialog::DialogState::Cancelled
            ) {
                self.file_dialog = None;
            }
        }

        ui.separator();

        if let Some(root) = self.root.clone() {
            egui::ScrollArea::vertical().show(ui, |ui| {
                open_file = show_tree(ui, &root, &mut self.expanded, &mut self.selected);
            });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Open a folder to browse files");
            });
        }

        open_file
    }
}

fn is_sequence_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SEQUENCE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Render a directory tree node. Returns a path if the user double-clicked a sequence file.
fn show_tree(
    ui: &mut egui::Ui,
    dir: &Path,
    expanded: &mut HashSet<PathBuf>,
    selected: &mut Option<PathBuf>,
) -> Option<PathBuf> {
    let mut open_file: Option<PathBuf> = None;

    let entries: Vec<PathBuf> = WalkDir::new(dir)
        .min_depth(1)
        .max_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .collect();

    for path in entries {
        if path.is_dir() {
            let is_expanded = expanded.contains(&path);
            let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

            let icon = if is_expanded { "▼ " } else { "▶ " };
            let header = format!("{icon}{label}");

            if ui.selectable_label(false, header).clicked() {
                if is_expanded {
                    expanded.remove(&path);
                } else {
                    expanded.insert(path.clone());
                }
            }

            if is_expanded {
                ui.indent(path.to_string_lossy().as_ref(), |ui| {
                    if let Some(p) = show_tree(ui, &path, expanded, selected) {
                        open_file = Some(p);
                    }
                });
            }
        } else if is_sequence_file(&path) {
            let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

            let is_selected = selected.as_deref() == Some(&path);
            let response = ui.selectable_label(is_selected, label);

            if response.clicked() {
                *selected = Some(path.clone());
            }

            if response.double_clicked() {
                println!("OpenFile: {}", path.display());
                open_file = Some(path);
            }
        }
    }

    open_file
}
