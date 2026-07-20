//! Read-only **Fragments** view — a list projection of a restriction digest
//! (Restriction Tier 2).
//!
//! The list is **virtual**: recomputed on demand from the source buffer, never
//! materialized (ROADMAP decision 25). `command::file::apply_digest` uses
//! [`compute`] for the CLI/agent response; the `ViewKind::Fragments` renderer
//! calls the same fn, so the two projections cannot drift. Nothing here reaches
//! into the sequence-canvas `Track` stack — it is its own simple list, the shape
//! the assembly-track recipe picker will reuse at multi-source scale.

use seqforge_core::commands::{EndInfo, FragmentInfo};
use seqforge_core::{Annotations, Buffer, MethylContext, ViewId};

use crate::command::{AppCommand, PendingCommand};

/// Digest `buf` under an enzyme `query` + methylation context. Returns the
/// fragment projection, any methylation warnings, and the **canonical**
/// enzyme-name string (stored on the Fragments view so a re-run is identical).
pub(crate) fn compute(
    buf: &Buffer,
    ann: &Annotations,
    query: &str,
    methyl: &MethylContext,
) -> (Vec<FragmentInfo>, Vec<String>, String) {
    let circular = buf.is_circular();
    let parsed = seqforge_bio::parse_enzyme_query(query);
    let names = seqforge_bio::resolve_query_names(&parsed, &buf.text, circular);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (frags, warnings) =
        seqforge_bio::digest_fragments(&buf.text, ann, &refs, circular, &buf.name, methyl);
    let infos = frags
        .iter()
        .enumerate()
        .map(|(i, f)| f.to_info(i))
        .collect();
    (infos, warnings, names.join(" "))
}

/// Render the read-only fragment list for a `ViewKind::Fragments` tab.
pub(crate) fn show(
    ui: &mut egui::Ui,
    source_view: ViewId,
    infos: &[FragmentInfo],
    warnings: &[String],
    pending: &mut Vec<PendingCommand>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.strong(format!(
                "{} fragment{}",
                infos.len(),
                if infos.len() == 1 { "" } else { "s" }
            ));
        });
        for w in warnings {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.weak(format!("⚠ {w}"));
            });
        }
        ui.add_space(2.0);
        ui.separator();

        if infos.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.weak("No fragments — no cut sites for this enzyme set.");
            });
            return;
        }

        for info in infos {
            fragment_row(ui, source_view, info, pending);
        }
    });
}

fn fragment_row(
    ui: &mut egui::Ui,
    source_view: ViewId,
    info: &FragmentInfo,
    pending: &mut Vec<PendingCommand>,
) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(format!("Fragment {}", info.index + 1));
                ui.weak(topology_glyph(info));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Open as buffer")
                        .on_hover_text("Materialize this fragment as its own document")
                        .clicked()
                    {
                        pending.push((
                            AppCommand::ExportFragment {
                                source_view,
                                index: info.index,
                            },
                            None,
                        ));
                    }
                    ui.add_space(8.0);
                    ui.weak(format!("{} bp", info.length));
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.weak(format!(
                    "5′ {}   3′ {}",
                    end_label(&info.left),
                    end_label(&info.right)
                ));
            });
        });
}

fn topology_glyph(info: &FragmentInfo) -> &'static str {
    match info.topology {
        seqforge_core::Topology::Circular => "○ circular",
        seqforge_core::Topology::Linear => "— linear",
    }
}

fn end_label(end: &EndInfo) -> String {
    let by = end
        .cut_by
        .as_deref()
        .map(|e| format!(" ({e})"))
        .unwrap_or_default();
    if end.kind == "blunt" {
        format!("blunt{by}")
    } else {
        format!("{} {}{by}", end.kind, end.seq)
    }
}
