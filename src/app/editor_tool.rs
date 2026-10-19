//! The editor's tools as rules, with no UI and no audio: what each tool is
//! called, where it sits in the Inspector, what part of the file it acts on,
//! how it is previewed, and which of its effects matter to the apply path.
//!
//! Everything else reads a tool's facts from here. The Inspector used to
//! spell each tool's name, icon and group inline, next to a separate copy in
//! `ToolKind::label`, and every panel decided for itself whether it honoured
//! the selection or the channel view -- which is how Gain came to ignore the
//! selection that Pitch Shift respected.

use super::types::{ToolKind, ViewMode};

/// Where a tool sits in the Inspector's palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Edit,
    Level,
    TimePitch,
    Dynamics,
    Repair,
    Channels,
    LoopMarkers,
    Analyze,
    /// Spectrogram-view tools; never offered in the Waveform view.
    Spectral,
}

impl ToolCategory {
    /// The palette of the Waveform view, in display order.
    pub const WAVEFORM: [ToolCategory; 8] = [
        ToolCategory::Edit,
        ToolCategory::Level,
        ToolCategory::TimePitch,
        ToolCategory::Dynamics,
        ToolCategory::Repair,
        ToolCategory::Channels,
        ToolCategory::LoopMarkers,
        ToolCategory::Analyze,
    ];

    /// The palette of the Spectrogram / Freq Log views. Mel is view-only.
    pub const SPECTRAL_VIEW: [ToolCategory; 3] = [
        ToolCategory::Spectral,
        ToolCategory::Repair,
        ToolCategory::LoopMarkers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToolCategory::Edit => "Edit",
            ToolCategory::Level => "Level",
            ToolCategory::TimePitch => "Time/Pitch",
            ToolCategory::Dynamics => "EQ/Dynamics",
            ToolCategory::Repair => "Repair",
            ToolCategory::Channels => "Channels",
            ToolCategory::LoopMarkers => "Loop/Markers",
            ToolCategory::Analyze => "Analyze/FX",
            ToolCategory::Spectral => "Spectral",
        }
    }

    /// The tools of this category, in display order.
    pub fn tools(self) -> &'static [ToolKind] {
        match self {
            ToolCategory::Edit => &[
                ToolKind::Trim,
                ToolKind::Fade,
                ToolKind::InsertSilence,
                ToolKind::Reverse,
                ToolKind::Pencil,
            ],
            ToolCategory::Level => &[
                ToolKind::Gain,
                ToolKind::Normalize,
                ToolKind::Loudness,
                ToolKind::InvertPolarity,
                ToolKind::DcOffset,
            ],
            ToolCategory::TimePitch => {
                &[ToolKind::PitchShift, ToolKind::TimeStretch, ToolKind::Speed]
            }
            ToolCategory::Dynamics => &[
                ToolKind::Eq,
                ToolKind::Compressor,
                ToolKind::Limiter,
                ToolKind::Clipper,
                ToolKind::NoiseGate,
            ],
            ToolCategory::Repair => &[
                ToolKind::DeClick,
                ToolKind::DeClip,
                ToolKind::DeHum,
                ToolKind::DeNoise,
                ToolKind::SpectralMute,
                ToolKind::Heal,
            ],
            ToolCategory::Channels => &[ToolKind::ChannelRouting, ToolKind::Panner],
            ToolCategory::LoopMarkers => {
                &[ToolKind::LoopEdit, ToolKind::Markers, ToolKind::EditorNote]
            }
            ToolCategory::Analyze => &[ToolKind::Reverb, ToolKind::MusicAnalyze, ToolKind::PluginFx],
            ToolCategory::Spectral => &[ToolKind::SpectralWarp, ToolKind::SpectralBrush],
        }
    }

    /// Tools that work on the spectrogram rather than the waveform.
    pub fn is_spectral(self) -> bool {
        matches!(self, ToolCategory::Spectral)
    }

    /// The palette for an editor view, or nothing for a view that has its
    /// own panel (Tempogram, Chromagram, World) or is view-only (Mel).
    pub fn for_view(view: ViewMode) -> &'static [ToolCategory] {
        match view {
            ViewMode::Waveform => &Self::WAVEFORM,
            ViewMode::Spectrogram | ViewMode::Log => &Self::SPECTRAL_VIEW,
            _ => &[],
        }
    }
}

/// What part of the file a tool acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetRule {
    /// The selection when there is one, otherwise the whole file.
    SelectionOrWhole,
    /// Always the whole file; a selection is ignored (and the Inspector says so).
    WholeOnly,
    /// Needs a selection; does nothing without one.
    SelectionOnly,
    /// A point: the selection start, or the playhead without a selection.
    InsertPoint,
    /// Works on the canvas or on annotations, not on a span of audio.
    None,
}

/// How the effect of a tool is heard before it is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Rendered on a worker with the same function Apply uses, and swapped
    /// in under the running playhead.
    Rendered,
    /// Applied live by the output callback (a channel matrix): the Panner.
    Live,
    /// The tool edits a draft that is itself what is heard: the Pencil.
    Draft,
    /// The tool runs a preview pipeline of its own (a plugin host, a stem
    /// remix); the footer drives it instead of the shared engine.
    Own,
    /// Nothing to preview: a structural edit (Trim, Channel Routing) or an
    /// annotation (markers, notes) that is committed as it is made.
    None,
}

/// Everything there is to know about a tool without looking at a file.
#[derive(Clone, Copy, Debug)]
pub struct ToolSpec {
    pub label: &'static str,
    pub icon: &'static str,
    pub category: ToolCategory,
    /// One line under the tool's name.
    pub summary: &'static str,
    /// What the canvas does while this tool is active, for the `?` hint.
    pub help: &'static str,
    pub target: TargetRule,
    pub preview: PreviewKind,
    /// Can the result be longer or shorter than the input?
    pub changes_length: bool,
    /// Does a Custom channel view limit the edit to the visible channels?
    pub honors_channel_view: bool,
    /// Slow enough that re-rendering a long clip on every settle would be
    /// felt: the auto preview of such a tool starts out manual on long clips
    /// -- unless it is `progressive`.
    pub heavy: bool,
    /// What it does at any moment depends only on the audio near that
    /// moment, so a render of a window is the full render there: the auto
    /// preview renders around the playhead first and the rest after it, and
    /// stays automatic on a clip of any length.
    pub progressive: bool,
}

const fn spec(
    label: &'static str,
    icon: &'static str,
    category: ToolCategory,
    summary: &'static str,
    help: &'static str,
    target: TargetRule,
    preview: PreviewKind,
) -> ToolSpec {
    ToolSpec {
        label,
        icon,
        category,
        summary,
        help,
        target,
        preview,
        changes_length: false,
        honors_channel_view: true,
        heavy: false,
        progressive: false,
    }
}

const CURVE_HELP: &str = "With the curve on: double-click empty space on the waveform to add a point, drag a point to move it, double-click a point to remove it. A single click still seeks.";

impl ToolSpec {
    const fn length_changing(mut self) -> Self {
        self.changes_length = true;
        self.honors_channel_view = false;
        self
    }

    const fn all_channels(mut self) -> Self {
        self.honors_channel_view = false;
        self
    }

    /// The result can be longer -- every channel alike, so the channel view
    /// still holds: a channel out of scope gets silence for the extra length.
    const fn extends_at_end(mut self) -> Self {
        self.changes_length = true;
        self
    }

    const fn heavy(mut self) -> Self {
        self.heavy = true;
        self
    }

    const fn progressive(mut self) -> Self {
        self.progressive = true;
        self
    }
}

impl ToolKind {
    /// Every tool, in palette order (Spectral last).
    pub const ALL: [ToolKind; 34] = [
        ToolKind::Trim,
        ToolKind::Fade,
        ToolKind::InsertSilence,
        ToolKind::Reverse,
        ToolKind::Pencil,
        ToolKind::Gain,
        ToolKind::Normalize,
        ToolKind::Loudness,
        ToolKind::InvertPolarity,
        ToolKind::DcOffset,
        ToolKind::PitchShift,
        ToolKind::TimeStretch,
        ToolKind::Speed,
        ToolKind::Eq,
        ToolKind::Compressor,
        ToolKind::Limiter,
        ToolKind::Clipper,
        ToolKind::NoiseGate,
        ToolKind::DeClick,
        ToolKind::DeClip,
        ToolKind::DeHum,
        ToolKind::DeNoise,
        ToolKind::SpectralMute,
        ToolKind::Heal,
        ToolKind::ChannelRouting,
        ToolKind::Panner,
        ToolKind::LoopEdit,
        ToolKind::Markers,
        ToolKind::EditorNote,
        ToolKind::Reverb,
        ToolKind::MusicAnalyze,
        ToolKind::PluginFx,
        ToolKind::SpectralWarp,
        ToolKind::SpectralBrush,
    ];

    pub fn spec(self) -> ToolSpec {
        use PreviewKind as P;
        use TargetRule as T;
        use ToolCategory as C;
        match self {
            ToolKind::Trim => spec(
                "Trim",
                "✂",
                C::Edit,
                "Keep, cut or silence the selected range",
                "Select a range on the waveform. T trims to it and Delete cuts it out (both undoable).",
                T::SelectionOnly,
                P::None,
            )
            .length_changing(),
            ToolKind::Fade => spec(
                "Edge Fade",
                "◢",
                C::Edit,
                "Fade the start in and the end out",
                "Drag the blue (start) and orange (end) handles on the waveform to set the fade lengths.",
                T::WholeOnly,
                P::Rendered,
            ),
            ToolKind::InsertSilence => spec(
                "Insert Silence",
                "∅",
                C::Edit,
                "Insert silence at the selection start or the playhead",
                "Markers and loop points after the insert point move right with the audio.",
                T::InsertPoint,
                P::Rendered,
            )
            .length_changing(),
            ToolKind::Reverse => spec(
                "Reverse",
                "◀",
                C::Edit,
                "Play the target backwards",
                "A partial range is joined with short crossfades so its edges do not click.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::Pencil => spec(
                "Pencil",
                "✏",
                C::Edit,
                "Draw sample values by hand",
                "Zoom in until samples are more than 2 px apart, then drag on a channel lane. One stroke is one undo step.",
                T::None,
                P::Draft,
            ),
            ToolKind::Gain => spec(
                "Gain",
                "🔊",
                C::Level,
                "Raise or lower the level",
                CURVE_HELP,
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::Normalize => spec(
                "Normalize",
                "⬆",
                C::Level,
                "Scale so the peak reaches a target",
                "The peak is measured over the target, on the channels the edit applies to.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::Loudness => spec(
                "LoudNorm",
                "🔉",
                C::Level,
                "Scale to an integrated loudness (LUFS)",
                "Measured over the whole file, all channels.",
                T::WholeOnly,
                P::Rendered,
            )
            .all_channels(),
            ToolKind::InvertPolarity => spec(
                "Invert Polarity",
                "±",
                C::Level,
                "Flip the waveform upside down",
                "Smooth boundaries ramps the flip over ~2 ms where a partial range meets untouched audio.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::DcOffset => spec(
                "DC Offset",
                "≡",
                C::Level,
                "Remove a constant offset from zero",
                "Subtracts each channel's mean over the target.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::PitchShift => spec(
                "Pitch Shift",
                "♪",
                C::TimePitch,
                "Change the pitch, keep the length",
                "Drag the horizontal pitch line on the waveform up or down. With the curve on, edit points as on a gain curve.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .heavy(),
            ToolKind::TimeStretch => spec(
                "Time Stretch",
                "⏳",
                C::TimePitch,
                "Change the length, keep the pitch",
                "Or drag the grip at the top of a selection edge to stretch just that range.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .length_changing()
            .heavy(),
            ToolKind::Speed => spec(
                "Speed",
                "⏩",
                C::TimePitch,
                "Change length and pitch together, like tape",
                "A partial range is spliced back with crossfades.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .length_changing()
            .heavy(),
            ToolKind::Eq => spec(
                "EQ",
                "📊",
                C::Dynamics,
                "Parametric EQ: up to eight bands, on stereo, mid or side",
                "Double-click the graph to add a band. Drag a handle for frequency and gain -- a cut's height is its resonance Q (Shift: fine) -- scroll on it for Q, Alt+click to bypass it, right-click for its shape, mode and Delete.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .progressive(),
            ToolKind::Compressor => spec(
                "Compressor",
                "⬇",
                C::Dynamics,
                "Reduce the dynamic range",
                "Drag the knee for the threshold, the top point for the ratio, the knee's edge for its width. The lower graph is the gain reduction over the target; click it to go there.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .progressive(),
            ToolKind::Limiter => spec(
                "Brickwall Limiter",
                "⏏",
                C::Dynamics,
                "Hold every peak under a ceiling by turning the gain down just in time",
                "Drag the ceiling line on the level graph. Input gain pushes the audio into it; True peak also catches the peaks between samples; Oversampling runs it at a higher rate so fast gain changes make no aliasing.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .heavy()
            .progressive(),
            ToolKind::Clipper => spec(
                "Clipper",
                "Π",
                C::Dynamics,
                "Cut whatever passes a ceiling, on the spot",
                "Drag the corner of the curve: up and down for the ceiling, sideways for the input gain; the lower handle sets the knee. Oversampling clips at a higher rate so the cut corners do not fold back as aliasing.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .heavy()
            .progressive(),
            ToolKind::NoiseGate => spec(
                "Noise Gate",
                "🔇",
                C::Dynamics,
                "Turn down what falls below a threshold",
                "Drag the open threshold on either graph. Audio below the close line (shaded) is turned down to the floor (Range).",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .progressive(),
            ToolKind::DeClick => spec(
                "De-click",
                "⚡",
                C::Repair,
                "Find and repair clicks",
                "Scan marks the clicks it would repair in red on the waveform.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::DeClip => spec(
                "De-clip",
                "△",
                C::Repair,
                "Rebuild peaks flattened by clipping",
                "Scan marks the clipped runs it would rebuild in red on the waveform.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::DeHum => spec(
                "De-hum",
                "〰",
                C::Repair,
                "Notch out mains hum and its harmonics",
                "Detect finds the hum fundamental between 45 and 65 Hz.",
                T::SelectionOrWhole,
                P::Rendered,
            ),
            ToolKind::DeNoise => spec(
                "De-noise",
                "≈",
                C::Repair,
                "Reduce steady background noise",
                "Select a stretch of noise only and Learn it first.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .heavy(),
            ToolKind::SpectralMute => spec(
                "Spectral Mute",
                "✖",
                C::Repair,
                "Silence the selection, or only its frequency band",
                "Select a time range; in a spectrogram view drag up or down as well to pick a band. Without a band every frequency is muted, with fades at the edges.",
                T::SelectionOnly,
                P::Rendered,
            )
            .heavy(),
            ToolKind::Heal => spec(
                "Heal",
                "✚",
                C::Repair,
                "Rebuild the selection from the audio around it",
                "Select a time range of at most 120 s; in a spectrogram view pick a band to rebuild only that band.",
                T::SelectionOnly,
                P::Rendered,
            )
            .heavy(),
            ToolKind::ChannelRouting => spec(
                "Channel Routing",
                "⇄",
                C::Channels,
                "Swap, copy or drop channels",
                "Drag from an input pin to an output pin to connect; click a cable to cut it; right-click a pin to clear it.",
                T::WholeOnly,
                P::None,
            )
            .all_channels(),
            ToolKind::Panner => spec(
                "Panner",
                "◎",
                C::Channels,
                "Balance, or turn the sound field",
                "Stereo pans by balance; multichannel turns the field with Yaw / Pitch / Roll (VBAP). The LFE never moves.",
                T::WholeOnly,
                P::Live,
            ),
            ToolKind::LoopEdit => spec(
                "Loop Edit",
                "🔁",
                C::LoopMarkers,
                "Loop points, the loop seam, and baking it",
                "Drag the ▼ handles to move a loop point (Alt snaps to zero crossings); click one to seek there.",
                T::None,
                P::Rendered,
            )
            .all_channels(),
            ToolKind::Markers => spec(
                "Markers",
                "📍",
                C::LoopMarkers,
                "Named positions in the file",
                "M adds a marker at the playhead. Markers are saved with the file.",
                T::None,
                P::None,
            )
            .all_channels(),
            ToolKind::EditorNote => spec(
                "Editor Note",
                "📝",
                C::LoopMarkers,
                "Notes pinned to a time or a range",
                "Double-click a note's position to seek there.",
                T::None,
                P::None,
            )
            .all_channels(),
            ToolKind::Reverb => spec(
                "Reverb",
                "\u{26EA}",
                C::Analyze,
                "Put the sound in a space: a convolution reverb over impulse responses",
                "Pick an impulse response in the browser (or open any WAV). Only the target is sent into the reverb; its tail rings on after it, keeps the length, or stops at its end, as Tail says.",
                T::SelectionOrWhole,
                P::Rendered,
            )
            .extends_at_end()
            .heavy(),
            ToolKind::MusicAnalyze => spec(
                "Music Analyze",
                "♫",
                C::Analyze,
                "Beats, sections and stems",
                "Analyze runs the model; the stem sliders preview a remix.",
                T::WholeOnly,
                P::Own,
            )
            .all_channels()
            .heavy(),
            ToolKind::PluginFx => spec(
                "Plugin FX",
                "🔧",
                C::Analyze,
                "Run VST3 / CLAP plugins over the file",
                "Slots run top to bottom; bypass a slot to hear the chain without it.",
                T::WholeOnly,
                P::Own,
            )
            .all_channels()
            .heavy(),
            ToolKind::SpectralWarp => spec(
                "Spectral Warp",
                "🌀",
                C::Spectral,
                "Push frequencies up or down by dragging",
                "Drag on the spectrogram to push content; drag an arrow to adjust it, double-click it to remove it.",
                T::None,
                P::Rendered,
            )
            .heavy(),
            ToolKind::SpectralBrush => spec(
                "Spectral Brush",
                "🎨",
                C::Spectral,
                "Paint attenuation onto the spectrogram",
                "Drag over what should be quieter; overlapping strokes add up (80 dB at most).",
                T::None,
                P::Rendered,
            )
            .heavy(),
        }
    }

    /// The categories a tool can be picked from in `view`.
    pub fn offered_in(self, view: ViewMode) -> bool {
        ToolCategory::for_view(view).contains(&self.spec().category)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_in_exactly_one_category() {
        let all_categories = [
            ToolCategory::Edit,
            ToolCategory::Level,
            ToolCategory::TimePitch,
            ToolCategory::Dynamics,
            ToolCategory::Repair,
            ToolCategory::Channels,
            ToolCategory::LoopMarkers,
            ToolCategory::Analyze,
            ToolCategory::Spectral,
        ];
        for tool in ToolKind::ALL {
            let homes: Vec<_> = all_categories
                .iter()
                .filter(|cat| cat.tools().contains(&tool))
                .collect();
            assert_eq!(homes.len(), 1, "{tool:?} is listed in {homes:?}");
            assert_eq!(*homes[0], tool.spec().category, "{tool:?}");
        }
        let listed: usize = all_categories.iter().map(|cat| cat.tools().len()).sum();
        assert_eq!(listed, ToolKind::ALL.len());
    }

    #[test]
    fn labels_and_icons_are_unique() {
        let mut labels: Vec<_> = ToolKind::ALL.iter().map(|t| t.spec().label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), ToolKind::ALL.len());
        let mut icons: Vec<_> = ToolKind::ALL.iter().map(|t| t.spec().icon).collect();
        icons.sort_unstable();
        icons.dedup();
        assert_eq!(icons.len(), ToolKind::ALL.len());
    }

    #[test]
    fn every_icon_is_drawn_by_the_app_fonts() {
        // A glyph no font has is drawn as an empty box: the palette would be
        // a row of identical squares. Checked against the fonts the app
        // really installs (the bundled Japanese face plus egui's defaults).
        let ctx = egui::Context::default();
        crate::app::WavesPreviewer::init_egui_style(&ctx);
        let _ = ctx.run_ui(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        let mut missing = Vec::new();
        for tool in ToolKind::ALL {
            let icon = tool.spec().icon;
            if !ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, icon)) {
                missing.push((tool, icon));
            }
        }
        // The editor toolbar's undo / redo buttons.
        for glyph in ["⟲", "⟳"] {
            if !ctx.fonts_mut(|fonts| fonts.has_glyphs(&font, glyph)) {
                missing.push((ToolKind::Trim, glyph));
            }
        }
        assert!(missing.is_empty(), "no font draws {missing:?}");
    }

    #[test]
    fn the_label_method_and_the_spec_agree() {
        for tool in ToolKind::ALL {
            assert_eq!(tool.label(), tool.spec().label);
        }
    }

    #[test]
    fn progressive_tools_keep_the_length_and_render_in_place() {
        // A window render is only the full render's middle when the tool
        // neither moves audio in time nor measures the whole target.
        for tool in ToolKind::ALL {
            let spec = tool.spec();
            if spec.progressive {
                assert!(!spec.changes_length, "{tool:?}");
                assert_eq!(spec.preview, PreviewKind::Rendered, "{tool:?}");
                assert_eq!(spec.target, TargetRule::SelectionOrWhole, "{tool:?}");
            }
        }
        assert!(ToolKind::Limiter.spec().progressive);
        assert!(ToolKind::Clipper.spec().progressive);
        assert!(!ToolKind::Normalize.spec().progressive, "measures the whole target");
    }

    #[test]
    fn length_changing_tools_never_claim_the_channel_view() {
        // A channel view cannot shorten some channels and not others. The
        // Reverb only ever adds a tail at the end, which every channel gets
        // (silence where it is out of scope), so it may.
        for tool in ToolKind::ALL {
            let spec = tool.spec();
            if spec.changes_length && tool != ToolKind::Reverb {
                assert!(!spec.honors_channel_view, "{tool:?}");
            }
        }
        assert!(ToolKind::Reverb.spec().honors_channel_view);
        assert!(!ToolKind::Reverb.spec().progressive, "a window cannot hold the tails reaching into it");
    }

    #[test]
    fn spectral_tools_stay_out_of_the_waveform_palette() {
        assert!(!ToolKind::SpectralWarp.offered_in(ViewMode::Waveform));
        assert!(ToolKind::SpectralWarp.offered_in(ViewMode::Spectrogram));
        assert!(ToolKind::Gain.offered_in(ViewMode::Waveform));
        assert!(!ToolKind::Gain.offered_in(ViewMode::Spectrogram));
        assert!(ToolKind::DeNoise.offered_in(ViewMode::Log));
        // Mute and Heal work on a plain time selection too.
        assert!(ToolKind::Heal.offered_in(ViewMode::Waveform));
        assert!(ToolKind::SpectralMute.offered_in(ViewMode::Spectrogram));
    }
}
