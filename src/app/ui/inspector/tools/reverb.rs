//! The Reverb: the impulse response -- picked from the library or any file,
//! drawn as its decay -- then how it is used. The IR is a setting like the
//! others (`ReverbSettings::ir`), so picking one is a draft step.

use egui::{RichText, Vec2};

use super::dynamics::{automation_section, hz_or_off_row, preset_menu, AutoView};
use crate::app::inspector_ops::InspectorAction;
use crate::app::reverb_ops::IrStatus;
use crate::app::tool_automation::AutoParam;
use crate::app::ui::inspector::plots;
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;
use crate::impulse::{IrId, IrRef};
use crate::reverb::{ReverbParam, ReverbSettings, ReverbTail};

const IR_PLOT_H: f32 = 110.0;
/// The browser's list: about eight rows, scrolled.
const BROWSER_H: f32 = 170.0;
/// The cuts as their controls show them: 0 is off.
const LOW_CUT_CONTROL_HZ: std::ops::RangeInclusive<f32> = 0.0..=2_000.0;
const HIGH_CUT_CONTROL_HZ: std::ops::RangeInclusive<f32> = 0.0..=20_000.0;

pub(super) fn reverb(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let auto = AutoView::new(app, tab_idx);
    let status = app.reverb_ir_status(tab_idx);
    let width = ui.available_width();
    let chosen = app.tabs[tab_idx].tool_state.reverb.ir_or_default().get();

    widgets::section(ui, "Impulse response");
    match &status {
        IrStatus::Ready(ir) => {
            ui.label(RichText::new(&ir.name).strong())
                .on_hover_text(ir.path.display().to_string());
            ui.label(RichText::new(ir.describe()).small().weak());
            let shown = auto.at.reverb;
            plots::ir_plot(ui, ir, &shown, Vec2::new(width, IR_PLOT_H));
        }
        IrStatus::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Loading {}...", chosen.name()));
            });
        }
        IrStatus::Failed(message) => {
            widgets::warn(
                ui,
                &format!("{} could not be used: {message}", chosen.name()),
            );
        }
    }

    // The library.
    let mut picked: Option<IrRef> = None;
    let catalog = app.reverb.catalog.clone();
    match catalog.as_deref() {
        None => {
            if let Some(message) = app.reverb.scan_failed.clone() {
                widgets::warn(ui, &message);
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking through the IR library...");
                });
            }
        }
        Some(catalog) if catalog.entries.is_empty() => {
            widgets::warn(
                ui,
                "No IR library here. Choose its folder below, or open an IR file.",
            );
        }
        Some(catalog) => {
            let categories = catalog.categories();
            ui.horizontal(|ui| {
                let shown = app
                    .reverb
                    .category
                    .clone()
                    .unwrap_or_else(|| "All".to_string());
                egui::ComboBox::from_id_salt(("reverb_category", tab_idx))
                    .selected_text(shown)
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(app.reverb.category.is_none(), "All")
                            .clicked()
                        {
                            app.reverb.category = None;
                        }
                        for c in &categories {
                            let on = app.reverb.category.as_deref() == Some(*c);
                            if ui.selectable_label(on, *c).clicked() {
                                app.reverb.category = Some(c.to_string());
                            }
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut app.reverb.query)
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
            });
            let rows = catalog.filter(app.reverb.category.as_deref(), &app.reverb.query);
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical()
                .id_salt(("reverb_irs", tab_idx))
                .max_height(BROWSER_H)
                .auto_shrink([false, true])
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for e in &rows[range] {
                        let on = matches!(&chosen, IrRef::Library(rel) if *rel == e.rel);
                        if ui
                            .selectable_label(on, e.name.as_str())
                            .on_hover_text(e.rel.as_str())
                            .clicked()
                            && !on
                        {
                            picked = Some(IrRef::Library(e.rel.clone()));
                        }
                    }
                });
            let roots: Vec<String> = catalog
                .roots
                .iter()
                .map(|r| r.display().to_string())
                .collect();
            ui.label(
                RichText::new(format!("{} of {} IRs", rows.len(), catalog.entries.len()))
                    .small()
                    .weak(),
            )
            .on_hover_text(format!("From:\n{}", roots.join("\n")));
        }
    }
    let (mut open_file, mut choose_folder, mut installed_only) = (false, false, false);
    ui.horizontal_wrapped(|ui| {
        open_file = ui
            .button("Open IR file...")
            .on_hover_text("Use any audio file as the impulse response")
            .clicked();
        choose_folder = ui
            .button("IR folder...")
            .on_hover_text("Where the whole IR library is (or any folder of IRs). Without one, the selection installed with NeoWaves is used.")
            .clicked();
        if app.reverb.user_dir.is_some() {
            installed_only = ui
                .small_button("Installed set only")
                .on_hover_text("Forget the folder: use the selection installed with NeoWaves")
                .clicked();
        }
    });
    if open_file {
        if let Some(path) = app.pick_ir_file_dialog() {
            picked = Some(IrRef::File(path));
        }
    }
    if choose_folder {
        if let Some(dir) = app.pick_folder_dialog() {
            app.set_ir_library_dir(Some(dir));
        }
    }
    if installed_only {
        app.set_ir_library_dir(None);
    }
    if let Some(r) = picked {
        app.tabs[tab_idx].tool_state.reverb.ir = Some(IrId::of(&r));
        g.began = true;
        g.ended = true;
    }

    let s = &mut app.tabs[tab_idx].tool_state.reverb;
    let p = AutoParam::Reverb;
    widgets::section(ui, "Mix");
    widgets::settings_grid(ui, ("insp_reverb_mix", tab_idx), |ui| {
        if auto.is(p(ReverbParam::Mix)) {
            auto.row(ui, p(ReverbParam::Mix), "Mix");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Mix",
                "0: the target as it was; 100: the reverb only",
                &mut s.mix_pct,
                ReverbSettings::MIX_PCT,
                " %",
                0,
                false,
            );
        }
        if auto.is(p(ReverbParam::Output)) {
            auto.row(ui, p(ReverbParam::Output), "Output");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Output",
                "Gain on the target as it comes out, and on the tail after it",
                &mut s.output_db,
                ReverbSettings::OUTPUT_DB,
                0.1,
                " dB",
                1,
            );
        }
        if auto.is(p(ReverbParam::Width)) {
            auto.row(ui, p(ReverbParam::Width), "Width");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Width",
                "The reverb's stereo width: 0 mono, 100 as the IR has it, 200 wider",
                &mut s.width_pct,
                ReverbSettings::WIDTH_PCT,
                " %",
                0,
                false,
            );
        }
        if auto.is(p(ReverbParam::LowCut)) {
            auto.row(ui, p(ReverbParam::LowCut), "Low cut");
        } else {
            hz_or_off_row(
                ui,
                g,
                "Low cut",
                "Take the low end out of the reverb (12 dB/oct)",
                &mut s.low_cut_hz,
                LOW_CUT_CONTROL_HZ,
            );
        }
        if auto.is(p(ReverbParam::HighCut)) {
            auto.row(ui, p(ReverbParam::HighCut), "High cut");
        } else {
            hz_or_off_row(
                ui,
                g,
                "High cut",
                "Darken the reverb (12 dB/oct)",
                &mut s.high_cut_hz,
                HIGH_CUT_CONTROL_HZ,
            );
        }
    });
    widgets::section(ui, "Response");
    widgets::settings_grid(ui, ("insp_reverb_ir", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Pre-delay",
            "Wait this long before the reverb starts",
            &mut s.predelay_ms,
            ReverbSettings::PREDELAY_MS,
            0.5,
            " ms",
            0,
        );
        widgets::slider_row(
            ui,
            g,
            "Length",
            "How much of the impulse response is used: shorter cuts the tail (faded)",
            &mut s.length_pct,
            ReverbSettings::LENGTH_PCT,
            " %",
            0,
            false,
        );
        widgets::check_row(
            ui,
            g,
            &mut s.reverse,
            "Reverse",
            "Play the impulse response backwards: the reverb swells into the sound",
        );
        widgets::check_row(
            ui,
            g,
            &mut s.normalize,
            "Normalize",
            "Level the impulse response, so changing it does not change how loud the reverb is",
        );
        ui.label("Tail")
            .on_hover_text("What the reverb does after the target ends");
        ui.horizontal_wrapped(|ui| {
            for t in ReverbTail::ALL {
                if ui
                    .selectable_label(s.tail == t, t.label())
                    .on_hover_text(t.hint())
                    .clicked()
                    && s.tail != t
                {
                    s.tail = t;
                    g.began = true;
                    g.ended = true;
                }
            }
        });
        ui.end_row();
    });
    automation_section(app, ui, tab_idx, g, 0);
    preset_menu(app, ui, tab_idx, actions);
}
