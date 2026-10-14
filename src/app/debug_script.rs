//! Auto-run scripts: the debug automation's steps written as text, so a
//! run can open a tool, put settings into it, preview, apply and take
//! screenshots without a build that hard-codes the values. Rules only -- no
//! UI, no disk: `cli.rs` reads the file, `debug_ops` runs the steps.
//!
//! One command per line (or `;`-separated on the command line); `#` starts
//! a comment. The commands, and what they do:
//!
//! ```text
//! open-first                 open the list's first file in the editor
//! select-next                select the next row of the list
//! tool <name>                switch the editor tool (EQ, Compressor, noise-gate, ...)
//! set <key> <value>          change one tool setting, as a gesture would (the preview follows)
//! preview                    render the preview now (the Preview button)
//! apply                      the Inspector's Apply
//! undo | redo                Ctrl+Z / Ctrl+Y
//! selection <start> <end>    select a span, as fractions of the file (0..1)
//! clear-selection            drop the selection
//! seek <fraction>            move the playhead
//! view <mode>                wave | spec | log | mel | tempogram | chromagram | world
//! play                       start or stop playback
//! wait <frames>              wait this many frames before the next command
//! screenshot [path]          save a screenshot (the screenshots folder without a path)
//! summary [path]             save the debug summary
//! exit                       quit
//! ```
//!
//! `set` names a field of the session's tool settings (`ProjectToolState`),
//! so every setting of every tool is reachable without a command per tool:
//! `gain_db`, `pitch_semitones`, `compressor.threshold_db`,
//! `eq.bands.3.freq_hz`, `gate.detection`. `compressor.`, `limiter.`,
//! `gate.` and `eq.` stand for the settings structs the session stores;
//! array elements are numbered from 0. A value is JSON when it parses as JSON
//! (`-30`, `true`, `{"enabled":true,"kind":"low_cut"}`) and a string
//! otherwise (`rms`, `low_cut`).

use std::path::PathBuf;

use super::project::{project_tool_state_to_tool_state, ProjectToolState};
use super::types::{DebugAction, ToolKind, ToolState, ViewMode};

/// One parsed command: what to do, and how many frames to wait before it
/// (`None`: the run's default delay).
#[derive(Debug)]
pub(crate) struct ScriptStep {
    pub wait_frames: Option<u32>,
    pub action: DebugAction,
}

/// A line that could not be understood: where, and why.
#[derive(Debug, PartialEq)]
pub(crate) struct ScriptError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "auto-run script line {}: {}", self.line, self.message)
    }
}

/// The lines of a script given on the command line as `a; b; c`.
pub(crate) fn inline_to_lines(inline: &str) -> String {
    inline
        .split(';')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a whole script. Every `set` is tried on default settings, so a
/// misspelt key or a value of the wrong type is reported here, by line,
/// before anything runs.
pub(crate) fn parse_script(text: &str) -> Result<Vec<ScriptStep>, ScriptError> {
    let mut steps = Vec::new();
    let mut pending_wait: Option<u32> = None;
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let err = |message: String| ScriptError {
            line: line_no,
            message,
        };
        let (command, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        let fraction = |text: &str| -> Result<f32, ScriptError> {
            text.parse::<f32>()
                .ok()
                .filter(|v| (0.0..=1.0).contains(v))
                .ok_or_else(|| err(format!("`{text}` is not a fraction between 0 and 1")))
        };
        let action = match command.to_ascii_lowercase().as_str() {
            "open-first" => DebugAction::OpenFirst,
            "select-next" => DebugAction::SelectNext,
            "tool" => DebugAction::SetActiveTool(parse_tool(rest).map_err(err)?),
            "set" => {
                let (key, value) = rest
                    .split_once(char::is_whitespace)
                    .map(|(k, v)| (k.trim(), v.trim()))
                    .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                    .ok_or_else(|| err("`set` needs a key and a value: set compressor.ratio 4".into()))?;
                let mut scratch = ToolState::default_values();
                set_tool_setting(&mut scratch, key, value).map_err(err)?;
                DebugAction::SetToolSetting {
                    key: key.to_string(),
                    value: value.to_string(),
                }
            }
            "preview" => DebugAction::RequestPreview,
            "apply" => DebugAction::ApplyTool,
            "undo" => DebugAction::Undo,
            "redo" => DebugAction::Redo,
            "selection" => {
                let mut parts = rest.split_whitespace();
                let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
                    return Err(err("`selection` needs a start and an end: selection 0.1 0.4".into()));
                };
                let (start_frac, end_frac) = (fraction(a)?, fraction(b)?);
                if end_frac <= start_frac {
                    return Err(err("the selection must end after it starts".into()));
                }
                DebugAction::SetSelection {
                    start_frac,
                    end_frac,
                }
            }
            "clear-selection" => DebugAction::ClearSelection,
            "seek" => DebugAction::Seek(fraction(rest)?),
            "view" => DebugAction::SetViewMode(parse_view(rest).map_err(err)?),
            "play" => DebugAction::PlayPause,
            "wait" => {
                let frames = rest
                    .parse::<u32>()
                    .map_err(|_| err(format!("`wait` needs a number of frames, not `{rest}`")))?;
                pending_wait = Some(pending_wait.unwrap_or(0) + frames);
                continue;
            }
            "screenshot" => {
                if rest.is_empty() {
                    DebugAction::ScreenshotAuto
                } else {
                    DebugAction::ScreenshotPath(PathBuf::from(rest))
                }
            }
            "summary" => {
                if rest.is_empty() {
                    DebugAction::DumpSummaryAuto
                } else {
                    DebugAction::DumpSummaryPath(PathBuf::from(rest))
                }
            }
            "exit" => DebugAction::Exit,
            other => {
                return Err(err(format!(
                    "unknown command `{other}` (open-first, select-next, tool, set, preview, apply, undo, redo, selection, clear-selection, seek, view, play, wait, screenshot, summary, exit)"
                )))
            }
        };
        steps.push(ScriptStep {
            wait_frames: pending_wait.take(),
            action,
        });
    }
    Ok(steps)
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// A tool by its name (`NoiseGate`) or its label (`Noise Gate`, `noise-gate`),
/// in any case.
pub(crate) fn parse_tool(name: &str) -> Result<ToolKind, String> {
    let want = normalized(name);
    ToolKind::ALL
        .iter()
        .copied()
        .find(|t| normalized(&format!("{t:?}")) == want || normalized(t.label()) == want)
        .ok_or_else(|| {
            let names: Vec<String> = ToolKind::ALL.iter().map(|t| format!("{t:?}")).collect();
            format!("unknown tool `{name}` (one of {})", names.join(", "))
        })
}

fn parse_view(name: &str) -> Result<ViewMode, String> {
    Ok(match normalized(name).as_str() {
        "wave" | "waveform" => ViewMode::Waveform,
        "spec" | "spectrogram" => ViewMode::Spectrogram,
        "log" | "freqlog" => ViewMode::Log,
        "mel" => ViewMode::Mel,
        "tempogram" => ViewMode::Tempogram,
        "chromagram" => ViewMode::Chromagram,
        "world" => ViewMode::World,
        _ => {
            return Err(format!(
                "unknown view `{name}` (wave, spec, log, mel, tempogram, chromagram, world)"
            ))
        }
    })
}

/// The session field a script key names: the short prefixes stand for the
/// settings structs.
fn session_path(key: &str) -> Vec<String> {
    let mut parts: Vec<String> = key.split('.').map(str::to_string).collect();
    if let Some(first) = parts.first_mut() {
        let full = match first.as_str() {
            "eq" => Some("eq_parametric"),
            "compressor" | "comp" => Some("compressor_settings"),
            "limiter" => Some("limiter_settings"),
            "gate" | "noise_gate" => Some("gate_settings"),
            _ => None,
        };
        if let Some(full) = full {
            *first = full.to_string();
        }
    }
    parts
}

/// The flat fields older sessions used, which the settings structs now
/// override: writing one would change nothing.
fn superseded(key: &str) -> Option<&'static str> {
    if key.starts_with("noise_gate_") {
        Some("gate.<field>")
    } else if key.starts_with("eq_low_")
        || key.starts_with("eq_mid_")
        || key.starts_with("eq_high_")
    {
        Some("eq.bands.<n>.<field>")
    } else if matches!(
        key,
        "compressor_threshold_db"
            | "compressor_ratio"
            | "compressor_attack_ms"
            | "compressor_release_ms"
            | "compressor_makeup_db"
    ) {
        Some("compressor.<field>")
    } else {
        None
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Change one setting of `st` by name, the way a script's `set` does.
pub(crate) fn set_tool_setting(
    st: &mut ToolState,
    key: &str,
    raw_value: &str,
) -> Result<(), String> {
    if let Some(instead) = superseded(key) {
        return Err(format!(
            "`{key}` is an old session field and changes nothing; use `{instead}`"
        ));
    }
    let mut doc = serde_json::to_value(ProjectToolState::from(&*st))
        .map_err(|e| format!("tool settings: {e}"))?;
    let path = session_path(key);
    // Messages speak the user's spelling (`compressor`), not the session's.
    let spelled: Vec<&str> = key.split('.').collect();
    let mut slot = &mut doc;
    for (depth, part) in path.iter().enumerate() {
        let walked = spelled[..depth].join(".");
        slot = match slot {
            serde_json::Value::Object(map) => {
                if !map.contains_key(part.as_str()) {
                    let mut known: Vec<&str> = map.keys().map(String::as_str).collect();
                    known.sort_unstable();
                    let place = if walked.is_empty() {
                        String::from("the tool settings")
                    } else {
                        format!("`{walked}`")
                    };
                    return Err(format!(
                        "{place} has no `{part}` (it has: {})",
                        known.join(", ")
                    ));
                }
                map.get_mut(part.as_str()).expect("checked above")
            }
            serde_json::Value::Array(items) => {
                let len = items.len();
                let index = part
                    .parse::<usize>()
                    .ok()
                    .filter(|i| *i < len)
                    .ok_or_else(|| {
                        format!(
                            "`{walked}` is a list of {len}: `{part}` is not an index 0..{}",
                            len.saturating_sub(1)
                        )
                    })?;
                &mut items[index]
            }
            other => {
                return Err(format!(
                    "`{walked}` is {}, not something with a `{part}`",
                    json_kind(other)
                ));
            }
        };
    }
    let value: serde_json::Value = serde_json::from_str(raw_value)
        .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
    // A value of a different kind than the field (a word where a number
    // belongs) is a typo, not a request; null stands for "none" anywhere.
    let fits = std::mem::discriminant(&value) == std::mem::discriminant(slot)
        || slot.is_null()
        || value.is_null()
        || (slot.is_object() && value.is_object());
    if !fits {
        return Err(format!(
            "`{key}` takes {}, not `{raw_value}`",
            json_kind(slot)
        ));
    }
    *slot = value;
    let back: ProjectToolState =
        serde_json::from_value(doc).map_err(|e| format!("`{key}` = `{raw_value}`: {e}"))?;
    *st = project_tool_state_to_tool_state(&back);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_parses_into_steps_with_their_waits() {
        let steps = parse_script(
            "# a comment\nopen-first\ntool compressor\nset compressor.ratio 4   # inline comment\nwait 30\npreview\nscreenshot out/a.png\nexit\n",
        )
        .unwrap();
        assert_eq!(steps.len(), 6);
        assert!(matches!(
            steps[1].action,
            DebugAction::SetActiveTool(ToolKind::Compressor)
        ));
        assert!(
            matches!(&steps[2].action, DebugAction::SetToolSetting { key, value } if key == "compressor.ratio" && value == "4")
        );
        assert_eq!(
            steps[3].wait_frames,
            Some(30),
            "the wait belongs to the next command"
        );
        assert!(matches!(steps[3].action, DebugAction::RequestPreview));
        assert!(matches!(&steps[4].action, DebugAction::ScreenshotPath(p) if p.ends_with("a.png")));
        assert_eq!(steps[0].wait_frames, None);
    }

    #[test]
    fn errors_name_the_line() {
        let e = parse_script("open-first\n\ntool compresor\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("unknown tool"), "{}", e.message);
        let e = parse_script("set compressor.thresold_db -30").unwrap_err();
        assert!(
            e.message.contains("threshold_db"),
            "the known keys are listed: {}",
            e.message
        );
        assert!(
            e.message.starts_with("`compressor` has no"),
            "in the user's spelling: {}",
            e.message
        );
        let e = parse_script("set compressor.ratio loud").unwrap_err();
        assert!(e.message.contains("a number"), "{}", e.message);
        assert!(parse_script("selection 0.5 0.2").is_err());
        assert!(parse_script("fly away").is_err());
    }

    #[test]
    fn tools_are_found_by_name_or_label() {
        assert_eq!(parse_tool("NoiseGate").unwrap(), ToolKind::NoiseGate);
        assert_eq!(parse_tool("noise-gate").unwrap(), ToolKind::NoiseGate);
        assert_eq!(parse_tool("Noise Gate").unwrap(), ToolKind::NoiseGate);
        assert_eq!(parse_tool("eq").unwrap(), ToolKind::Eq);
        assert_eq!(parse_tool("pitch-shift").unwrap(), ToolKind::PitchShift);
        assert_eq!(parse_tool("Spectral Mute").unwrap(), ToolKind::SpectralMute);
    }

    #[test]
    fn a_set_changes_its_field_and_nothing_else() {
        let mut st = ToolState::default_values();
        let before = st;
        set_tool_setting(&mut st, "compressor.threshold_db", "-30").unwrap();
        assert_eq!(st.compressor.threshold_db, -30.0);
        let mut expected = before;
        expected.compressor.threshold_db = -30.0;
        assert_eq!(st, expected);

        set_tool_setting(&mut st, "gain_db", "-3.5").unwrap();
        assert_eq!(st.gain_db, -3.5);
        set_tool_setting(&mut st, "gate.detection", "rms").unwrap();
        assert_eq!(st.gate.detection, crate::dynamics::Detection::Rms);
        set_tool_setting(&mut st, "limiter.true_peak", "false").unwrap();
        assert!(!st.limiter.true_peak);
        set_tool_setting(
            &mut st,
            "eq.bands.4",
            r#"{"enabled":true,"kind":"low_cut","freq_hz":80}"#,
        )
        .unwrap();
        assert!(st.eq.bands[4].enabled);
        assert_eq!(st.eq.bands[4].kind, crate::eq::EqKind::LowCut);
        assert_eq!(st.eq.bands[4].freq_hz, 80.0);
        set_tool_setting(&mut st, "eq.bands.4.mode", "side").unwrap();
        assert_eq!(st.eq.bands[4].mode, crate::eq::EqChannelMode::Side);
    }

    #[test]
    fn bad_keys_and_values_are_refused_without_changing_anything() {
        let mut st = ToolState::default_values();
        let before = st;
        assert!(set_tool_setting(&mut st, "eq.bands.9.gain_db", "3").is_err());
        assert!(
            set_tool_setting(&mut st, "gate.detection", "loud").is_err(),
            "not a variant"
        );
        assert!(set_tool_setting(&mut st, "compressor_ratio", "4")
            .unwrap_err()
            .contains("compressor.<field>"));
        assert!(set_tool_setting(&mut st, "nothing", "1").is_err());
        assert_eq!(st, before);
    }

    #[test]
    fn an_inline_script_is_split_at_semicolons() {
        let steps = parse_script(&inline_to_lines(
            "open-first; tool limiter; set limiter.input_gain_db 9; preview",
        ))
        .unwrap();
        assert_eq!(steps.len(), 4);
    }
}
