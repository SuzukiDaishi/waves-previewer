//! Built-in starting points for the EQ and the dynamics tools. A preset sets
//! the tool's settings as one draft step (`ToolCommand::Preset`), so Ctrl+Z
//! takes it back and the auto preview plays it at once. Rules only.

use super::types::{ToolKind, ToolState};
use crate::dynamics::{ClipperSettings, CompressorSettings, Detection, GateSettings, LimiterSettings};
use crate::oversample::Oversampling;
use crate::reverb::ReverbSettings;
use crate::eq::{CutSlope, EqBand, EqChannelMode, EqKind, ParametricEqParams};

/// One named setting.
pub struct ToolPreset {
    pub name: &'static str,
    pub apply: fn(&mut ToolState),
}

/// The presets offered for `tool` (none for most tools).
pub fn presets(tool: ToolKind) -> &'static [ToolPreset] {
    match tool {
        ToolKind::Eq => EQ,
        ToolKind::Compressor => COMPRESSOR,
        ToolKind::Limiter => LIMITER,
        ToolKind::Clipper => CLIPPER,
        ToolKind::Reverb => REVERB,
        ToolKind::NoiseGate => GATE,
        _ => &[],
    }
}

fn band(kind: EqKind, freq_hz: f32, gain_db: f32, q: f32) -> EqBand {
    EqBand {
        enabled: true,
        kind,
        freq_hz,
        gain_db,
        q,
        ..EqBand::default()
    }
}

fn eq_with(bands: &[EqBand]) -> ParametricEqParams {
    let mut p = ParametricEqParams {
        bands: [EqBand::default(); crate::eq::EQ_MAX_BANDS],
        output_db: 0.0,
    };
    for (slot, b) in p.bands.iter_mut().zip(bands) {
        *slot = *b;
    }
    p
}

static EQ: &[ToolPreset] = &[
    ToolPreset {
        name: "Flat",
        apply: |st| st.eq = ParametricEqParams::default(),
    },
    ToolPreset {
        name: "Low Cut 80 Hz",
        apply: |st| {
            st.eq = eq_with(&[EqBand {
                slope: CutSlope::Db24,
                ..band(EqKind::LowCut, 80.0, 0.0, 0.707)
            }])
        },
    },
    ToolPreset {
        name: "De-mud",
        apply: |st| {
            st.eq = eq_with(&[
                EqBand {
                    slope: CutSlope::Db12,
                    ..band(EqKind::LowCut, 60.0, 0.0, 0.707)
                },
                band(EqKind::Bell, 320.0, -3.5, 1.2),
            ])
        },
    },
    ToolPreset {
        name: "Presence",
        apply: |st| {
            st.eq = eq_with(&[
                EqBand {
                    slope: CutSlope::Db12,
                    ..band(EqKind::LowCut, 70.0, 0.0, 0.707)
                },
                band(EqKind::Bell, 3_500.0, 3.0, 0.9),
                band(EqKind::HighShelf, 10_000.0, 2.0, 0.707),
            ])
        },
    },
    ToolPreset {
        name: "Wider air (side)",
        apply: |st| {
            st.eq = eq_with(&[
                EqBand {
                    mode: EqChannelMode::Side,
                    ..band(EqKind::HighShelf, 8_000.0, 3.0, 0.707)
                },
                EqBand {
                    mode: EqChannelMode::Side,
                    slope: CutSlope::Db12,
                    ..band(EqKind::LowCut, 150.0, 0.0, 0.707)
                },
            ])
        },
    },
];

static COMPRESSOR: &[ToolPreset] = &[
    ToolPreset {
        name: "Vocal",
        apply: |st| {
            st.compressor = CompressorSettings {
                threshold_db: -20.0,
                ratio: 3.0,
                knee_db: 6.0,
                attack_ms: 5.0,
                release_ms: 80.0,
                detection: Detection::Rms,
                rms_ms: 10.0,
                auto_makeup: true,
                sidechain_hpf_hz: 80.0,
                ..CompressorSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Bus glue",
        apply: |st| {
            st.compressor = CompressorSettings {
                threshold_db: -16.0,
                ratio: 2.0,
                knee_db: 8.0,
                attack_ms: 30.0,
                release_ms: 200.0,
                detection: Detection::Rms,
                rms_ms: 30.0,
                auto_makeup: true,
                ..CompressorSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Peak control",
        apply: |st| {
            st.compressor = CompressorSettings {
                threshold_db: -10.0,
                ratio: 8.0,
                knee_db: 2.0,
                attack_ms: 0.5,
                release_ms: 50.0,
                lookahead_ms: 2.0,
                ..CompressorSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Parallel",
        apply: |st| {
            st.compressor = CompressorSettings {
                threshold_db: -30.0,
                ratio: 6.0,
                knee_db: 3.0,
                attack_ms: 3.0,
                release_ms: 100.0,
                mix_pct: 40.0,
                auto_makeup: true,
                ..CompressorSettings::default()
            }
        },
    },
];

static LIMITER: &[ToolPreset] = &[
    ToolPreset {
        name: "Master -1 dBTP",
        apply: |st| st.limiter = LimiterSettings::default(),
    },
    ToolPreset {
        name: "Broadcast -2 dBTP",
        apply: |st| {
            st.limiter = LimiterSettings {
                ceiling_db: -2.0,
                release_ms: 100.0,
                lookahead_ms: 7.0,
                ..LimiterSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Loud",
        apply: |st| {
            st.limiter = LimiterSettings {
                input_gain_db: 6.0,
                release_ms: 30.0,
                lookahead_ms: 3.0,
                ..LimiterSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Loud, clean (8x)",
        apply: |st| {
            st.limiter = LimiterSettings {
                input_gain_db: 9.0,
                release_ms: 20.0,
                lookahead_ms: 2.0,
                oversampling: Oversampling::X8,
                ..LimiterSettings::default()
            }
        },
    },
];

static CLIPPER: &[ToolPreset] = &[
    ToolPreset {
        name: "Safety -1 dB",
        apply: |st| st.clipper = ClipperSettings::default(),
    },
    ToolPreset {
        name: "Soft knee",
        apply: |st| {
            st.clipper = ClipperSettings {
                input_gain_db: 3.0,
                knee_db: 3.0,
                ..ClipperSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Drums, before a limiter (8x)",
        apply: |st| {
            st.clipper = ClipperSettings {
                input_gain_db: 4.0,
                ceiling_db: -0.3,
                oversampling: Oversampling::X8,
                hard_ceiling: false,
                ..ClipperSettings::default()
            }
        },
    },
];

// The Reverb's presets keep the IR: they are about how it is used.
static REVERB: &[ToolPreset] = &[
    ToolPreset {
        name: "Ambience",
        apply: |st| {
            st.reverb = ReverbSettings {
                ir: st.reverb.ir,
                mix_pct: 15.0,
                low_cut_hz: 250.0,
                length_pct: 60.0,
                ..ReverbSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Room",
        apply: |st| st.reverb = ReverbSettings { ir: st.reverb.ir, ..ReverbSettings::default() },
    },
    ToolPreset {
        name: "Wash",
        apply: |st| {
            st.reverb = ReverbSettings {
                ir: st.reverb.ir,
                mix_pct: 60.0,
                predelay_ms: 30.0,
                high_cut_hz: 8_000.0,
                width_pct: 140.0,
                ..ReverbSettings::default()
            }
        },
    },
];

static GATE: &[ToolPreset] = &[
    ToolPreset {
        name: "Dialog",
        apply: |st| {
            st.gate = GateSettings {
                threshold_db: -45.0,
                hysteresis_db: 4.0,
                range_db: -24.0,
                attack_ms: 2.0,
                hold_ms: 60.0,
                release_ms: 150.0,
                key_hpf_hz: 80.0,
                ..GateSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Drums",
        apply: |st| {
            st.gate = GateSettings {
                threshold_db: -30.0,
                hysteresis_db: 3.0,
                range_db: -80.0,
                attack_ms: 0.2,
                hold_ms: 10.0,
                release_ms: 60.0,
                lookahead_ms: 2.0,
                ..GateSettings::default()
            }
        },
    },
    ToolPreset {
        name: "Silence the gaps",
        apply: |st| {
            st.gate = GateSettings {
                threshold_db: -50.0,
                hysteresis_db: 3.0,
                range_db: *GateSettings::RANGE_DB.start(),
                hold_ms: 30.0,
                ..GateSettings::default()
            }
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_changes_its_own_tool_only() {
        for tool in [
            ToolKind::Eq,
            ToolKind::Compressor,
            ToolKind::Limiter,
            ToolKind::Clipper,
            ToolKind::Reverb,
            ToolKind::NoiseGate,
        ] {
            assert!(!presets(tool).is_empty(), "{tool:?}");
            for preset in presets(tool) {
                let mut st = ToolState::default_values();
                (preset.apply)(&mut st);
                let mut back = st;
                match tool {
                    ToolKind::Eq => back.eq = ToolState::default_values().eq,
                    ToolKind::Compressor => {
                        back.compressor = ToolState::default_values().compressor
                    }
                    ToolKind::Limiter => back.limiter = ToolState::default_values().limiter,
                    ToolKind::Clipper => back.clipper = ToolState::default_values().clipper,
                    ToolKind::Reverb => back.reverb = ToolState::default_values().reverb,
                    _ => back.gate = ToolState::default_values().gate,
                }
                assert_eq!(back, ToolState::default_values(), "{}", preset.name);
            }
        }
        assert!(presets(ToolKind::Gain).is_empty());
    }
}
