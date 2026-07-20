//! The `Tab::Recipe` **assembly workbench** — batch-first 5'→3' authoring
//! (decision 26). Each bin sets one ordered prepare span; sources are a bulk
//! list. Ambiguity UI is a **site dropdown on an endpoint that cuts >1×** — not
//! a band picker. Read-only over the workspace; every mutation is an
//! `AppCommand` (decision 5).

use std::collections::HashMap;

use seqforge_core::commands::{EndInfo, FragmentInfo};
use seqforge_core::{
    Bin, Boundary, Expand, PrepareKind, Recipe, RecipeId, SourceRef, SpanEnds, Topology,
    TopologyIntent, default_role,
};

use crate::assembly::resolver::WorkspaceResolver;
use crate::command::assembly::RecipeOp;
use crate::command::{AppCommand, PendingCommand};
use crate::ui_icon::{
    phosphor_icon, phosphor_icon_button, phosphor_icon_colored, phosphor_labeled,
};
use crate::workspace::Workspace;

/// Session cache for per-row prepare previews (not durable).
type PreviewCache = HashMap<PreviewKey, PreviewEntry>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct PreviewKey {
    recipe: RecipeId,
    bin: usize,
    source: usize,
    /// Buffer version or content pin (0 when unknown).
    stamp: u64,
    five_prime: String,
    three_prime: String,
}

#[derive(Clone)]
struct PreviewEntry {
    name: String,
    infos: Vec<FragmentInfo>,
    warn: Option<String>,
}

/// Render the workbench for `id`. Returns without drawing if the recipe is gone.
pub(crate) fn show(
    ui: &mut egui::Ui,
    id: RecipeId,
    workspace: &Workspace,
    combo_selection: Option<&std::collections::HashSet<usize>>,
    fidelity_dataset: Option<seqforge_bio::FidelityDataset>,
    pending: &mut Vec<PendingCommand>,
) {
    let Some(recipe) = workspace.recipe(id) else {
        ui.centered_and_justified(|ui| {
            ui.label("(recipe closed)");
        });
        return;
    };

    let open_buffers: Vec<(seqforge_core::BufferId, String)> = workspace
        .views
        .values()
        .filter_map(|v| {
            let arc = workspace.buffers.get(v.buffer_id)?;
            let guard = arc.read().ok()?;
            Some((v.buffer_id, crate::workspace::display_name(&guard)))
        })
        .collect();

    // Session preview cache keyed on the egui memory for this recipe tab.
    let cache_id = egui::Id::new(("recipe_preview_cache", id));
    let mut cache: PreviewCache = ui
        .ctx()
        .data(|d| d.get_temp::<PreviewCache>(cache_id))
        .unwrap_or_default();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.heading("Assembly");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Load recipe...")
                    .on_hover_text("Replace this assembly from recipe.json")
                    .clicked()
                {
                    pending.push((AppCommand::PromptLoadRecipe { id }, None));
                }
                if ui
                    .button("Save recipe...")
                    .on_hover_text("Export recipe.json for CLI")
                    .clicked()
                {
                    pending.push((AppCommand::PromptSaveRecipe { id }, None));
                }
            });
        });
        ui.add_space(4.0);

        for (i, bin) in recipe.bins.iter().enumerate() {
            bin_card(
                ui,
                id,
                i,
                bin,
                recipe.bins.len(),
                &open_buffers,
                workspace,
                &mut cache,
                pending,
            );
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if ui.button("+ Add bin").clicked() {
                pending.push((
                    AppCommand::EditRecipe {
                        id,
                        op: RecipeOp::AddBin,
                    },
                    None,
                ));
            }
        });

        ui.add_space(8.0);
        ui.separator();
        run_footer(
            ui,
            id,
            recipe,
            workspace,
            combo_selection,
            fidelity_dataset,
            pending,
        );
    });

    ui.ctx().data_mut(|d| d.insert_temp(cache_id, cache));
}

#[allow(clippy::too_many_arguments)]
fn bin_card(
    ui: &mut egui::Ui,
    id: RecipeId,
    i: usize,
    bin: &Bin,
    bin_count: usize,
    open_buffers: &[(seqforge_core::BufferId, String)],
    workspace: &Workspace,
    cache: &mut PreviewCache,
    pending: &mut Vec<PendingCommand>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut role = bin.role.clone();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut role)
                    .desired_width(120.0)
                    .hint_text(default_role(i)),
            );
            if resp.changed() {
                pending.push((
                    AppCommand::EditRecipe {
                        id,
                        op: RecipeOp::SetRole { bin: i, role },
                    },
                    None,
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if bin_count > 1
                    && ui
                        .horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let clicked = ui.button("Remove bin").clicked();
                            phosphor_icon(ui, egui_phosphor::regular::MINUS, 14.0);
                            clicked
                        })
                        .inner
                {
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::RemoveBin(i),
                        },
                        None,
                    ));
                }
            });
        });

        // Prepare kind + 5'→3' / PCR params.
        ui.horizontal(|ui| {
            ui.label("Prepare:");
            let kind_label = match &bin.prepare {
                PrepareKind::Digest { .. } => "Digest",
                PrepareKind::Pcr { .. } => "PCR",
                PrepareKind::AsIs => "As-is",
            };
            egui::ComboBox::from_id_salt(("prepare_kind", id, i))
                .selected_text(kind_label)
                .show_ui(ui, |ui| {
                    let mut set = |ui: &mut egui::Ui, label: &str, kind: PrepareKind| {
                        if ui.selectable_label(kind_label == label, label).clicked() {
                            pending.push((
                                AppCommand::EditRecipe {
                                    id,
                                    op: RecipeOp::SetPrepare {
                                        bin: i,
                                        prepare: kind,
                                    },
                                },
                                None,
                            ));
                        }
                    };
                    set(
                        ui,
                        "Digest",
                        PrepareKind::Digest {
                            five_prime: Boundary::enzyme(""),
                            three_prime: Boundary::enzyme(""),
                        },
                    );
                    set(
                        ui,
                        "PCR",
                        PrepareKind::Pcr {
                            fwd: String::new(),
                            rev: String::new(),
                        },
                    );
                    set(ui, "As-is", PrepareKind::AsIs);
                });
            prepare_params(ui, id, i, bin, workspace, pending);
        });

        // Sources grid.
        ui.add_space(2.0);
        ui.label("Sources:");
        let resolver = WorkspaceResolver { ws: workspace };
        egui::Grid::new(("sources", id, i))
            .num_columns(3)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for (si, src) in bin.sources.iter().enumerate() {
                    source_row(
                        ui, id, i, si, src, bin, &resolver, workspace, cache, pending,
                    );
                    ui.end_row();
                }
            });

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("+ files…").clicked() {
                pending.push((AppCommand::PromptAddSourceFile { recipe: id, bin: i }, None));
            }
            glob_field(ui, id, i, pending);
            egui::ComboBox::from_id_salt(("add_buffer", id, i))
                .selected_text("open buffer")
                .show_ui(ui, |ui| {
                    for (bid, name) in open_buffers {
                        if ui.selectable_label(false, name).clicked() {
                            pending.push((
                                AppCommand::EditRecipe {
                                    id,
                                    op: RecipeOp::AddSources {
                                        bin: i,
                                        sources: vec![SourceRef::Buffer(*bid)],
                                    },
                                },
                                None,
                            ));
                        }
                    }
                    if open_buffers.is_empty() {
                        ui.weak("(no open buffers)");
                    }
                });
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn source_row(
    ui: &mut egui::Ui,
    id: RecipeId,
    bin_i: usize,
    si: usize,
    src: &seqforge_core::Source,
    bin: &Bin,
    resolver: &WorkspaceResolver,
    workspace: &Workspace,
    cache: &mut PreviewCache,
    pending: &mut Vec<PendingCommand>,
) {
    use seqforge_bio::SourceResolver;

    let span = effective_span(bin, src);
    let stamp = content_stamp(src, workspace);
    let key = PreviewKey {
        recipe: id,
        bin: bin_i,
        source: si,
        stamp,
        five_prime: span
            .as_ref()
            .map(|s| s.five_prime.to_string())
            .unwrap_or_default(),
        three_prime: span
            .as_ref()
            .map(|s| s.three_prime.to_string())
            .unwrap_or_default(),
    };

    let entry = cache
        .entry(key.clone())
        .or_insert_with(|| match resolver.resolve(&src.ref_) {
            Ok(rs) => {
                let (infos, warn) = {
                    let single = Bin {
                        role: bin.role.clone(),
                        sources: vec![src.clone()],
                        prepare: bin.prepare.clone(),
                    };
                    let (infos, w) = seqforge_bio::preview_bin(&single, resolver);
                    (infos, w.first().cloned())
                };
                PreviewEntry {
                    name: rs.name,
                    infos,
                    warn,
                }
            }
            Err(e) => PreviewEntry {
                name: String::new(),
                infos: Vec::new(),
                warn: Some(e),
            },
        });

    // Column 1: name → bp · ends (or warn when 0 matches).
    if entry.infos.is_empty() {
        let msg = entry
            .warn
            .clone()
            .unwrap_or_else(|| "no fragment for 5'->3' ends".into());
        phosphor_labeled(
            ui,
            egui_phosphor::regular::WARNING,
            msg,
            14.0,
            ui.visuals().warn_fg_color,
        );
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(&entry.name);
            phosphor_icon(ui, egui_phosphor::regular::ARROW_RIGHT, 12.0);
            ui.label(format_bp(&entry.infos));
            phosphor_icon_colored(
                ui,
                egui_phosphor::regular::CHECK,
                12.0,
                ui.visuals().weak_text_color(),
            );
        });
    }

    // Column 2: per-source @pos when this source alone is multi-cut on an end.
    source_site_overrides(ui, id, bin_i, si, src, bin, workspace, pending);

    // Column 3: remove.
    if phosphor_icon_button(ui, egui_phosphor::regular::X, 12.0)
        .on_hover_text("Remove source")
        .clicked()
    {
        pending.push((
            AppCommand::EditRecipe {
                id,
                op: RecipeOp::RemoveSource {
                    bin: bin_i,
                    index: si,
                },
            },
            None,
        ));
    }
}

fn format_bp(infos: &[FragmentInfo]) -> String {
    match infos.len() {
        0 => "-".into(),
        1 => {
            let ends = format_ends(&infos[0]);
            if ends.is_empty() {
                format!("{} bp", infos[0].length)
            } else {
                format!("{} bp | {ends}", infos[0].length)
            }
        }
        n => format!("{n} frags"),
    }
}

/// One-line 5'/3' overhang summary for a single prepared fragment.
fn format_ends(info: &FragmentInfo) -> String {
    format!(
        "{} -> {}",
        format_end_side("5'", &info.left),
        format_end_side("3'", &info.right)
    )
}

fn format_end_side(side: &str, end: &EndInfo) -> String {
    if end.kind == "blunt" || end.seq.is_empty() {
        format!("{side} blunt")
    } else {
        match &end.cut_by {
            Some(enzyme) => format!("{side} {} ({enzyme})", end.seq),
            None => format!("{side} {}", end.seq),
        }
    }
}

fn effective_span(bin: &Bin, src: &seqforge_core::Source) -> Option<SpanEnds> {
    if let Some(s) = &src.span {
        return Some(s.clone());
    }
    bin.prepare.digest_span()
}

fn content_stamp(src: &seqforge_core::Source, workspace: &Workspace) -> u64 {
    match &src.ref_ {
        SourceRef::Buffer(bid) => workspace
            .buffers
            .get(*bid)
            .and_then(|a| a.read().ok().map(|b| b.version))
            .unwrap_or(0),
        SourceRef::Path(_) => src.pin.unwrap_or(0),
    }
}

fn prepare_params(
    ui: &mut egui::Ui,
    id: RecipeId,
    i: usize,
    bin: &Bin,
    workspace: &Workspace,
    pending: &mut Vec<PendingCommand>,
) {
    match &bin.prepare {
        PrepareKind::Digest {
            five_prime,
            three_prime,
        } => {
            let mut five_s = enzyme_edit_text(five_prime);
            let mut three_s = enzyme_edit_text(three_prime);
            ui.label("5'");
            let rf = ui.add(
                egui::TextEdit::singleline(&mut five_s)
                    .desired_width(90.0)
                    .hint_text("EcoRI"),
            );
            site_menu(
                ui,
                id,
                i,
                "five",
                five_prime,
                three_prime,
                EndSlot::Five,
                bin,
                workspace,
                pending,
            );
            phosphor_icon(ui, egui_phosphor::regular::ARROW_RIGHT, 12.0);
            ui.label("3'");
            let rt = ui.add(
                egui::TextEdit::singleline(&mut three_s)
                    .desired_width(90.0)
                    .hint_text("PstI"),
            );
            site_menu(
                ui,
                id,
                i,
                "three",
                five_prime,
                three_prime,
                EndSlot::Three,
                bin,
                workspace,
                pending,
            );

            let flip_positions = two_site_positions(bin, five_prime, three_prime, workspace);
            if phosphor_icon_button(ui, egui_phosphor::regular::ARROWS_LEFT_RIGHT, 14.0)
                .on_hover_text("Swap 5' and 3' ends")
                .clicked()
            {
                let (five, three) =
                    flip_ends(five_prime.clone(), three_prime.clone(), flip_positions);
                pending.push((
                    AppCommand::EditRecipe {
                        id,
                        op: RecipeOp::SetPrepareEnds {
                            bin: i,
                            five_prime: five,
                            three_prime: three,
                        },
                    },
                    None,
                ));
            }

            if rf.changed() || rt.changed() {
                let five_b = parse_end_keeping_at(&five_s, five_prime);
                let three_b = parse_end_keeping_at(&three_s, three_prime);
                pending.push((
                    AppCommand::EditRecipe {
                        id,
                        op: RecipeOp::SetPrepareEnds {
                            bin: i,
                            five_prime: five_b,
                            three_prime: three_b,
                        },
                    },
                    None,
                ));
            }
        }
        PrepareKind::Pcr { fwd, rev } => {
            let mut f = fwd.clone();
            let mut r = rev.clone();
            ui.label("5'");
            let rf = ui.add(
                egui::TextEdit::singleline(&mut f)
                    .desired_width(80.0)
                    .hint_text("fwd"),
            );
            phosphor_icon(ui, egui_phosphor::regular::ARROW_RIGHT, 12.0);
            ui.label("3'");
            let rr = ui.add(
                egui::TextEdit::singleline(&mut r)
                    .desired_width(80.0)
                    .hint_text("rev"),
            );
            if rf.changed() || rr.changed() {
                pending.push((
                    AppCommand::EditRecipe {
                        id,
                        op: RecipeOp::SetPrepare {
                            bin: i,
                            prepare: PrepareKind::Pcr { fwd: f, rev: r },
                        },
                    },
                    None,
                ));
            }
        }
        PrepareKind::AsIs => {
            ui.weak("(whole source)");
        }
    }
}

#[derive(Clone, Copy)]
enum EndSlot {
    Five,
    Three,
}

/// Enzyme name field (omit `@pos` so typing a new enzyme clears the pin).
fn enzyme_edit_text(b: &Boundary) -> String {
    match b {
        Boundary::EnzymeSite { enzyme, .. } => enzyme.clone(),
        other => other.to_string(),
    }
}

fn parse_end_keeping_at(text: &str, previous: &Boundary) -> Boundary {
    let text = text.trim();
    if text.is_empty() {
        return Boundary::enzyme("");
    }
    // If the user typed a full token with @pos, honour it.
    if text.contains('@') || text.parse::<usize>().is_ok() || text.contains('^') || text == "*" {
        return text
            .parse::<Boundary>()
            .unwrap_or_else(|_| Boundary::enzyme(text));
    }
    // Plain enzyme rename: keep prior `@pos` only when the enzyme name matches.
    match previous {
        Boundary::EnzymeSite {
            enzyme,
            at: Some(p),
        } if enzyme.eq_ignore_ascii_case(text) => Boundary::enzyme_at(text, *p),
        _ => Boundary::enzyme(text),
    }
}

/// Per-row `@pos` menus when *this* source's endpoint enzyme cuts >1×.
#[allow(clippy::too_many_arguments)]
fn source_site_overrides(
    ui: &mut egui::Ui,
    id: RecipeId,
    bin_i: usize,
    si: usize,
    src: &seqforge_core::Source,
    bin: &Bin,
    workspace: &Workspace,
    pending: &mut Vec<PendingCommand>,
) {
    let Some(span) = effective_span(bin, src) else {
        ui.label("");
        return;
    };
    let resolver = WorkspaceResolver { ws: workspace };
    let Ok(rs) = seqforge_bio::SourceResolver::resolve(&resolver, &src.ref_) else {
        ui.label("");
        return;
    };
    let circular = matches!(rs.topology, Topology::Circular);
    let mut drew = false;
    ui.horizontal(|ui| {
        for (slot, boundary) in [
            (EndSlot::Five, &span.five_prime),
            (EndSlot::Three, &span.three_prime),
        ] {
            let Boundary::EnzymeSite { enzyme, at } = boundary else {
                continue;
            };
            if enzyme.is_empty() {
                continue;
            }
            let sites = seqforge_bio::cut_boundaries(&rs.bytes, enzyme, circular);
            let Some(positions) = sites
                .into_iter()
                .find(|s| s.enzyme.eq_ignore_ascii_case(enzyme))
                .map(|s| s.positions)
            else {
                continue;
            };
            if positions.len() <= 1 {
                continue;
            }
            drew = true;
            let label = match slot {
                EndSlot::Five => "5'",
                EndSlot::Three => "3'",
            };
            let selected = at
                .map(|p| format!("{label}@{p}"))
                .unwrap_or_else(|| format!("{label} site…"));
            egui::ComboBox::from_id_salt(("src_site", id, bin_i, si, label))
                .selected_text(selected)
                .width(72.0)
                .show_ui(ui, |ui| {
                    for &pos in &positions {
                        if ui
                            .selectable_label(*at == Some(pos), format!("@{pos}"))
                            .clicked()
                        {
                            let mut next = span.clone();
                            match slot {
                                EndSlot::Five => {
                                    next.five_prime = Boundary::enzyme_at(enzyme.clone(), pos);
                                }
                                EndSlot::Three => {
                                    next.three_prime = Boundary::enzyme_at(enzyme.clone(), pos);
                                }
                            }
                            pending.push((
                                AppCommand::EditRecipe {
                                    id,
                                    op: RecipeOp::SetSourceSpan {
                                        bin: bin_i,
                                        index: si,
                                        span: Some(next),
                                    },
                                },
                                None,
                            ));
                        }
                    }
                });
        }
    });
    if !drew {
        ui.label("");
    }
}

/// Site dropdown for one endpoint when that enzyme cuts >1× on the probe source.
#[allow(clippy::too_many_arguments)]
fn site_menu(
    ui: &mut egui::Ui,
    id: RecipeId,
    bin_i: usize,
    salt: &str,
    five: &Boundary,
    three: &Boundary,
    slot: EndSlot,
    bin: &Bin,
    workspace: &Workspace,
    pending: &mut Vec<PendingCommand>,
) {
    let boundary = match slot {
        EndSlot::Five => five,
        EndSlot::Three => three,
    };
    let Boundary::EnzymeSite { enzyme, at } = boundary else {
        return;
    };
    if enzyme.is_empty() {
        return;
    }
    let Some(positions) = cut_positions_for(bin, enzyme, workspace) else {
        return;
    };
    if positions.len() <= 1 {
        return;
    }

    let selected = at
        .map(|p| format!("@{p}"))
        .unwrap_or_else(|| "site…".into());
    egui::ComboBox::from_id_salt(("site", id, bin_i, salt))
        .selected_text(selected)
        .width(64.0)
        .show_ui(ui, |ui| {
            for &pos in &positions {
                let label = format!("@{pos}");
                if ui.selectable_label(*at == Some(pos), &label).clicked() {
                    let (five_prime, three_prime) = match slot {
                        EndSlot::Five => (Boundary::enzyme_at(enzyme.clone(), pos), three.clone()),
                        EndSlot::Three => (five.clone(), Boundary::enzyme_at(enzyme.clone(), pos)),
                    };
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::SetPrepareEnds {
                                bin: bin_i,
                                five_prime,
                                three_prime,
                            },
                        },
                        None,
                    ));
                }
            }
        });
}

fn cut_positions_for(bin: &Bin, enzyme: &str, workspace: &Workspace) -> Option<Vec<usize>> {
    let src = bin.sources.first()?;
    let resolver = WorkspaceResolver { ws: workspace };
    let rs = seqforge_bio::SourceResolver::resolve(&resolver, &src.ref_).ok()?;
    let circular = matches!(rs.topology, Topology::Circular);
    let sites = seqforge_bio::cut_boundaries(&rs.bytes, enzyme, circular);
    sites
        .into_iter()
        .find(|s| s.enzyme.eq_ignore_ascii_case(enzyme))
        .map(|s| s.positions)
}

/// When both ends are the same unbound enzyme with exactly two sites, return
/// `[lo, hi]` so ⇄ can pin the complementary arc.
fn two_site_positions(
    bin: &Bin,
    five: &Boundary,
    three: &Boundary,
    workspace: &Workspace,
) -> Option<[usize; 2]> {
    match (five, three) {
        (
            Boundary::EnzymeSite {
                enzyme: a,
                at: None,
            },
            Boundary::EnzymeSite {
                enzyme: b,
                at: None,
            },
        ) if a == b && !a.is_empty() => {
            let positions = cut_positions_for(bin, a, workspace)?;
            match positions.as_slice() {
                [lo, hi] => Some([*lo, *hi]),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Swap 5'/3'. Unbound same-enzyme with two sites → pin max..min so the flip
/// is observable (engine defaults unbound to min..max).
fn flip_ends(
    five: Boundary,
    three: Boundary,
    two_sites: Option<[usize; 2]>,
) -> (Boundary, Boundary) {
    if let Some([lo, hi]) = two_sites {
        if let (
            Boundary::EnzymeSite {
                enzyme: a,
                at: None,
            },
            Boundary::EnzymeSite {
                enzyme: b,
                at: None,
            },
        ) = (&five, &three)
            && a == b
        {
            return (
                Boundary::enzyme_at(a.clone(), hi),
                Boundary::enzyme_at(a.clone(), lo),
            );
        }
    }
    let flipped = SpanEnds::new(five, three).flipped();
    (flipped.five_prime, flipped.three_prime)
}

fn glob_field(ui: &mut egui::Ui, id: RecipeId, i: usize, pending: &mut Vec<PendingCommand>) {
    let salt = ("glob", id, i);
    let mut text = ui
        .memory_mut(|m| m.data.get_temp::<String>(egui::Id::new(salt)))
        .unwrap_or_default();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut text)
            .desired_width(160.0)
            .hint_text("parts/*.gb"),
    );
    let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if resp.changed() || submit {
        ui.memory_mut(|m| m.data.insert_temp(egui::Id::new(salt), text.clone()));
    }
    if submit && !text.trim().is_empty() {
        let sources: Vec<SourceRef> = seqforge_bio::expand_glob(text.trim())
            .into_iter()
            .map(SourceRef::Path)
            .collect();
        if !sources.is_empty() {
            pending.push((
                AppCommand::EditRecipe {
                    id,
                    op: RecipeOp::AddSources { bin: i, sources },
                },
                None,
            ));
        }
    }
}

fn run_footer(
    ui: &mut egui::Ui,
    id: RecipeId,
    recipe: &Recipe,
    workspace: &Workspace,
    combo_selection: Option<&std::collections::HashSet<usize>>,
    fidelity_dataset: Option<seqforge_bio::FidelityDataset>,
    pending: &mut Vec<PendingCommand>,
) {
    let resolver = WorkspaceResolver { ws: workspace };
    let (summaries, _enum_warnings) =
        seqforge_bio::enumerate_combos(recipe, &resolver, fidelity_dataset);
    let probe = seqforge_bio::probe_recipe(recipe, &resolver);
    let first_ok = probe
        .first
        .as_ref()
        .is_some_and(|p| p.compatible_for(recipe.intent));

    const DISPLAY_CAP: usize = 256;
    let display_summaries = if summaries.len() > DISPLAY_CAP {
        &summaries[..DISPLAY_CAP]
    } else {
        &summaries[..]
    };

    let default_selected: std::collections::HashSet<usize> =
        summaries.iter().filter(|c| c.ok).map(|c| c.index).collect();
    let effective = combo_selection.unwrap_or(&default_selected);

    // ── Join strip: Join verb + settings + Topology + Expand ───────────────
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.label("Join:");
        let join_label = match &recipe.join {
            seqforge_core::JoinKind::Ligate => "Ligate",
            seqforge_core::JoinKind::GoldenGate { .. } => "Golden Gate",
        };
        egui::ComboBox::from_id_salt(("join_kind", id))
            .selected_text(join_label)
            .show_ui(ui, |ui| {
                let is_ligate = matches!(recipe.join, seqforge_core::JoinKind::Ligate);
                if ui.selectable_label(is_ligate, "Ligate").clicked() && !is_ligate {
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::SetJoin(seqforge_core::JoinKind::Ligate),
                        },
                        None,
                    ));
                }
                let is_gg = matches!(recipe.join, seqforge_core::JoinKind::GoldenGate { .. });
                if ui.selectable_label(is_gg, "Golden Gate").clicked() && !is_gg {
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::SetJoin(seqforge_core::JoinKind::GoldenGate {
                                enzyme: "BsaI".into(),
                            }),
                        },
                        None,
                    ));
                    if fidelity_dataset.is_none() {
                        if let Some(ds) = seqforge_bio::dataset_for_enzyme("BsaI") {
                            pending.push((
                                AppCommand::SetRecipeFidelity {
                                    id,
                                    dataset: Some(ds),
                                },
                                None,
                            ));
                        }
                    }
                }
            });

        match &recipe.join {
            seqforge_core::JoinKind::Ligate => {}
            seqforge_core::JoinKind::GoldenGate { enzyme } => {
                ui.label("Enzyme:");
                let mut enz = enzyme.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut enz)
                        .desired_width(72.0)
                        .hint_text("BsaI"),
                );
                if resp.lost_focus() && enz != *enzyme {
                    let enz = enz.trim().to_string();
                    if !enz.is_empty() {
                        pending.push((
                            AppCommand::EditRecipe {
                                id,
                                op: RecipeOp::SetJoin(seqforge_core::JoinKind::GoldenGate {
                                    enzyme: enz.clone(),
                                }),
                            },
                            None,
                        ));
                        // When leaving Off, preselect enzyme table if available.
                        if fidelity_dataset.is_none() {
                            if let Some(ds) = seqforge_bio::dataset_for_enzyme(&enz) {
                                pending.push((
                                    AppCommand::SetRecipeFidelity {
                                        id,
                                        dataset: Some(ds),
                                    },
                                    None,
                                ));
                            }
                        }
                    }
                }
            }
        }

        fidelity_dropdown(ui, id, recipe, fidelity_dataset, pending);

        ui.label("Topology:");
        egui::ComboBox::from_id_salt(("intent", id))
            .selected_text(match recipe.intent {
                TopologyIntent::Circular => "Circular",
                TopologyIntent::Linear => "Linear",
                TopologyIntent::Any => "Any",
            })
            .show_ui(ui, |ui| {
                for (label, intent) in [
                    ("Circular", TopologyIntent::Circular),
                    ("Linear", TopologyIntent::Linear),
                    ("Any", TopologyIntent::Any),
                ] {
                    if ui
                        .selectable_label(recipe.intent == intent, label)
                        .clicked()
                    {
                        pending.push((
                            AppCommand::EditRecipe {
                                id,
                                op: RecipeOp::SetIntent(intent),
                            },
                            None,
                        ));
                    }
                }
            });
        ui.label("Expand:");
        egui::ComboBox::from_id_salt(("expand", id))
            .selected_text(match recipe.expand {
                Expand::AllToAll => "All-to-all",
                Expand::Zip => "Zip",
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(recipe.expand == Expand::AllToAll, "All-to-all")
                    .on_hover_text("Cartesian product across bins (library width)")
                    .clicked()
                {
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::SetExpand(Expand::AllToAll),
                        },
                        None,
                    ));
                }
                if ui
                    .selectable_label(recipe.expand == Expand::Zip, "Zip")
                    .on_hover_text("Positional 1:1 pairing (bins must share fragment count)")
                    .clicked()
                {
                    pending.push((
                        AppCommand::EditRecipe {
                            id,
                            op: RecipeOp::SetExpand(Expand::Zip),
                        },
                        None,
                    ));
                }
            })
            .response
            .on_hover_text(
                "All-to-all: Cartesian product across bins. Zip: positional 1:1 pairing.",
            );
    });

    // Advisory join status (identity-only; does not block Run for libraries).
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        if probe.combos == 0 {
            phosphor_labeled(
                ui,
                egui_phosphor::regular::WARNING,
                "No fragments to join yet",
                14.0,
                ui.visuals().warn_fg_color,
            );
        } else if first_ok {
            let msg = if probe.compatible_combos == probe.combos {
                format!(
                    "Ends match in bin order ({}/{} combos)",
                    probe.compatible_combos, probe.combos
                )
            } else {
                format!(
                    "Default combo OK — {}/{} combos compatible",
                    probe.compatible_combos, probe.combos
                )
            };
            phosphor_labeled(
                ui,
                egui_phosphor::regular::CHECK,
                msg,
                14.0,
                ui.visuals().weak_text_color(),
            );
        } else {
            let detail = probe
                .first
                .as_ref()
                .and_then(|p| p.junctions.iter().find(|j| !j.ok))
                .map(|j| j.detail.as_str())
                .or_else(|| {
                    probe
                        .first
                        .as_ref()
                        .filter(|p| !p.closes && matches!(recipe.intent, TopologyIntent::Circular))
                        .map(|_| "circular ends do not close")
                })
                .unwrap_or("authored 5'/3' ends do not match");
            phosphor_labeled(
                ui,
                egui_phosphor::regular::WARNING,
                format!(
                    "{detail} (0/{} compatible — use complementary walks or both orients as sources)",
                    probe.combos
                ),
                14.0,
                ui.visuals().warn_fg_color,
            );
        }
    });

    if summaries.is_empty() {
        return;
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.weak(format!(
            "Combos ({}/{} selected)",
            effective.len(),
            summaries.len()
        ));
        if ui.small_button("Select all compatible").clicked() {
            pending.push((
                AppCommand::SetRecipeComboSelection {
                    id,
                    selected: default_selected.clone(),
                },
                None,
            ));
        }
        if ui.small_button("Clear").clicked() {
            pending.push((
                AppCommand::SetRecipeComboSelection {
                    id,
                    selected: std::collections::HashSet::new(),
                },
                None,
            ));
        }
    });
    if summaries.len() > DISPLAY_CAP {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Showing first {DISPLAY_CAP} of {} combos", summaries.len()),
            );
        });
    }

    let show_fid = fidelity_dataset.is_some();
    let cols = 2 + recipe.bins.len() + usize::from(show_fid); // check + # + bins + status [+ fid]
    egui::Grid::new(("combo_grid", id))
        .num_columns(cols + 1)
        .striped(true)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            ui.label("");
            ui.weak("#");
            for bin in &recipe.bins {
                ui.weak(&bin.role);
            }
            if show_fid {
                ui.weak("Fidelity")
                    .on_hover_text("Informational only — not in recipe.json; never gates Run.");
            }
            ui.weak("Status");
            ui.end_row();

            for combo in display_summaries {
                let mut checked = effective.contains(&combo.index);
                if ui.checkbox(&mut checked, "").changed() {
                    let mut next = effective.clone();
                    if checked {
                        next.insert(combo.index);
                    } else {
                        next.remove(&combo.index);
                    }
                    pending.push((
                        AppCommand::SetRecipeComboSelection { id, selected: next },
                        None,
                    ));
                }
                ui.label(format!("{}", combo.index + 1));
                for (bi, part) in combo.parts.iter().enumerate() {
                    let role = recipe.bins.get(bi).map(|b| b.role.as_str()).unwrap_or("?");
                    ui.label(format!("{}: {} {} bp", role, part.source_name, part.length));
                }
                if show_fid {
                    match combo.fidelity {
                        Some(f) if combo.fidelity_three_prime => {
                            ui.label(format!("{:.1}%*", f * 100.0)).on_hover_text(
                                "Includes 3′ overhang(s); Potapov/Pryor tables are 5′-assay data.",
                            );
                        }
                        Some(f) => {
                            ui.label(format!("{:.1}%", f * 100.0));
                        }
                        None => {
                            ui.weak("—");
                        }
                    };
                }
                if combo.ok {
                    ui.weak("ok");
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        combo.detail.as_deref().unwrap_or("incompatible"),
                    );
                }
                ui.end_row();
            }
        });

    // Run under the preview table — never gated by fidelity.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let run_clicked = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                phosphor_icon(ui, egui_phosphor::regular::PLAY, 14.0);
                ui.button("Run").clicked()
            })
            .inner;
        if run_clicked {
            pending.push((AppCommand::RunRecipe { id }, None));
        }
    });
}

/// One Fidelity dropdown: Off | tables whose length can match current junctions.
fn fidelity_dropdown(
    ui: &mut egui::Ui,
    id: RecipeId,
    recipe: &Recipe,
    selected: Option<seqforge_bio::FidelityDataset>,
    pending: &mut Vec<PendingCommand>,
) {
    ui.label("Fidelity:");
    let label = selected.map(|d| d.label()).unwrap_or("Off");
    // Infer overhang length from join: SapI tables only for SapI enzyme, else 4.
    let prefer_3 = matches!(
        &recipe.join,
        seqforge_core::JoinKind::GoldenGate { enzyme }
            if enzyme.eq_ignore_ascii_case("SapI")
    );
    egui::ComboBox::from_id_salt(("fidelity", id))
        .selected_text(label)
        .show_ui(ui, |ui| {
            if ui.selectable_label(selected.is_none(), "Off").clicked() {
                pending.push((AppCommand::SetRecipeFidelity { id, dataset: None }, None));
            }
            for ds in seqforge_bio::FidelityDataset::ALL {
                let len_ok = if prefer_3 {
                    ds.overhang_len() == 3
                } else {
                    ds.overhang_len() == 4
                };
                if !len_ok {
                    continue;
                }
                if ui
                    .selectable_label(selected == Some(ds), ds.label())
                    .clicked()
                {
                    pending.push((
                        AppCommand::SetRecipeFidelity {
                            id,
                            dataset: Some(ds),
                        },
                        None,
                    ));
                }
            }
        })
        .response
        .on_hover_text("Informational only — not in recipe.json; never gates Run.");
}
