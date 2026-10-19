//! Each tool's settings. A panel draws on the tab's settings and reports
//! gestures and button presses; it never previews or applies anything
//! itself (the Inspector's footer does, for every tool alike).

mod analyze;
mod basic;
mod dynamics;
mod edit;
mod loop_markers;
mod reverb;
mod spectral;

use crate::app::inspector_ops::InspectorAction;
use crate::app::types::ToolKind;
use crate::app::WavesPreviewer;

use super::widgets::Gestures;

pub(super) fn panel(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let tool = app.tabs[tab_idx].active_tool;
    match tool {
        ToolKind::Gain => basic::gain(app, ui, tab_idx, g),
        ToolKind::Normalize => basic::normalize(app, ui, tab_idx, g),
        ToolKind::Loudness => basic::loudness(app, ui, tab_idx, g),
        ToolKind::InvertPolarity => basic::invert(app, ui, tab_idx, g),
        ToolKind::DcOffset => basic::dc_offset(app, ui, tab_idx),
        ToolKind::Reverse => basic::reverse(ui),
        ToolKind::PitchShift => basic::pitch(app, ui, tab_idx, g),
        ToolKind::TimeStretch | ToolKind::Speed => basic::rate(app, ui, tab_idx, g, tool),
        ToolKind::Eq => dynamics::eq(app, ui, tab_idx, g, actions),
        ToolKind::Compressor => dynamics::compressor(app, ui, tab_idx, g, actions),
        ToolKind::Limiter => dynamics::limiter(app, ui, tab_idx, g, actions),
        ToolKind::Clipper => dynamics::clipper(app, ui, tab_idx, g, actions),
        ToolKind::Reverb => reverb::reverb(app, ui, tab_idx, g, actions),
        ToolKind::NoiseGate => dynamics::gate(app, ui, tab_idx, g, actions),
        ToolKind::DeClick | ToolKind::DeClip => basic::declick(app, ui, tab_idx, g, actions, tool),
        ToolKind::DeHum => basic::dehum(app, ui, tab_idx, g, actions),
        ToolKind::DeNoise => basic::denoise(app, ui, tab_idx, g, actions),
        ToolKind::Fade => edit::fade(app, ui, tab_idx, g),
        ToolKind::InsertSilence => edit::insert_silence(app, ui, tab_idx, g),
        ToolKind::Pencil => edit::pencil(app, ui, tab_idx),
        ToolKind::Trim => edit::trim(app, ui, tab_idx, g, actions),
        ToolKind::ChannelRouting => edit::channel_routing(app, ui, tab_idx, actions),
        ToolKind::Panner => edit::panner(app, ui, tab_idx, actions),
        ToolKind::LoopEdit => loop_markers::loop_edit(app, ui, tab_idx, g, actions),
        ToolKind::Markers => loop_markers::markers(app, ui, tab_idx, actions),
        ToolKind::EditorNote => loop_markers::note(app, ui, tab_idx),
        ToolKind::MusicAnalyze => analyze::music(app, ui, tab_idx),
        ToolKind::PluginFx => analyze::plugin_fx(app, ui, tab_idx),
        ToolKind::SpectralWarp => spectral::warp(app, ui, tab_idx, g),
        ToolKind::SpectralBrush => spectral::brush(app, ui, tab_idx, g),
        ToolKind::SpectralMute | ToolKind::Heal => {
            spectral::mute_or_heal(app, ui, tab_idx, g, tool)
        }
    }
}
