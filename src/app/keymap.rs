//! Central keyboard shortcut table.
//!
//! Every user-facing shortcut is described by one [`KeyBinding`] row. Simple
//! bindings are dispatched through [`consume`] so the chord lives only here;
//! complex handlers (navigation loops, chords with per-key logic) keep their
//! own dispatch and are listed as [`Dispatch::Manual`] rows so the in-app
//! shortcut list stays complete. A future rebinding UI only needs to swap the
//! chord lookup in [`binding`] for a user table.

use egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyContext {
    Global,
    List,
    Editor,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    // Global
    FocusSearch,
    TogglePlay,
    VolumeDown,
    VolumeUp,
    SwitchTab,
    SaveSession,
    SaveSessionAs,
    NewWindow,
    ExportSelected,
    CloseTab,
    Undo,
    Redo,
    // List
    ListToggleAutoplay,
    ListToggleRegex,
    ListOpenSelected,
    ListNavigate,
    ListCopyPaste,
    ListRenameInline,
    // Editor
    EditorSetLoopStart,
    EditorSetLoopEnd,
    EditorApplyLoop,
    EditorCycleViewMode,
    EditorToggleBpm,
    EditorAddMarker,
    EditorToggleZeroCross,
    EditorDeleteSelection,
    EditorTrimSelection,
    EditorVirtualTrim,
    EditorDigitSeek,
    EditorArrowKeys,
    EditorAudioClipboard,
    EditorSeekStart,
    EditorSeekEnd,
    EditorZoomToSelection,
    EditorZoomIn,
    EditorZoomOut,
    EditorViewPageBack,
    EditorViewPageForward,
    EditorCancelPreview,
}

/// Modifier sets used by the table (const-friendly subset of `egui::Modifiers`).
/// `Shift` never appears in the built-in table but is accepted for rebinds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mods {
    None,
    Shift,
    Command,
    CommandShift,
}

impl Mods {
    pub fn to_modifiers(self) -> Modifiers {
        match self {
            Mods::None => Modifiers::NONE,
            Mods::Shift => Modifiers::SHIFT,
            Mods::Command => Modifiers::COMMAND,
            Mods::CommandShift => Modifiers::COMMAND | Modifiers::SHIFT,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Mods::None => "",
            Mods::Shift => "Shift+",
            Mods::Command => "Ctrl+",
            Mods::CommandShift => "Ctrl+Shift+",
        }
    }

    /// Map real input modifiers onto the table subset. Alt/other combos are
    /// not representable and yield `None`.
    pub fn from_modifiers(m: Modifiers) -> Option<Mods> {
        if m.alt {
            return None;
        }
        let command = m.command || m.ctrl || m.mac_cmd;
        match (command, m.shift) {
            (false, false) => Some(Mods::None),
            (false, true) => Some(Mods::Shift),
            (true, false) => Some(Mods::Command),
            (true, true) => Some(Mods::CommandShift),
        }
    }
}

impl Action {
    /// Stable identifier used by the prefs file (`keymap=` lines).
    pub fn name(self) -> String {
        format!("{self:?}")
    }

    /// Inverse of [`Action::name`]; every action appears in KEYMAP exactly
    /// once, so the table doubles as the registry.
    pub fn from_name(s: &str) -> Option<Action> {
        KEYMAP
            .iter()
            .map(|b| b.action)
            .find(|a| format!("{a:?}") == s)
    }
}

/// Human/prefs text for a chord ("Ctrl+Shift+Z" style).
pub fn chord_text(mods: Mods, key: Key) -> String {
    format!("{}{}", mods.prefix(), key.name())
}

/// Parse [`chord_text`] output back into a chord.
pub fn parse_chord(s: &str) -> Option<(Mods, Key)> {
    let (mods, rest) = if let Some(r) = s.strip_prefix("Ctrl+Shift+") {
        (Mods::CommandShift, r)
    } else if let Some(r) = s.strip_prefix("Ctrl+") {
        (Mods::Command, r)
    } else if let Some(r) = s.strip_prefix("Shift+") {
        (Mods::Shift, r)
    } else {
        (Mods::None, s)
    };
    Key::from_name(rest).map(|k| (mods, k))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dispatch {
    /// Consumed via [`consume`]; the chord below is authoritative.
    Table,
    /// Handled by dedicated code; the row exists for the shortcut list.
    Manual,
}

pub struct KeyBinding {
    pub action: Action,
    pub context: KeyContext,
    /// Concrete chord for table-dispatched rows. `None` for manual rows whose
    /// keys are described by `keys_label` (ranges, multi-chord families).
    pub chord: Option<(Mods, Key)>,
    /// Display text for `chord: None` rows.
    pub keys_label: &'static str,
    pub desc: &'static str,
    pub dispatch: Dispatch,
}

impl KeyBinding {
    pub fn keys_text(&self) -> String {
        match self.chord {
            Some((mods, key)) => format!("{}{}", mods.prefix(), key.name()),
            None => self.keys_label.to_string(),
        }
    }
}

pub const KEYMAP: &[KeyBinding] = &[
    // ---- Global ----
    KeyBinding {
        action: Action::FocusSearch,
        context: KeyContext::Global,
        chord: Some((Mods::Command, Key::F)),
        keys_label: "",
        desc: "Focus the search box",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::TogglePlay,
        context: KeyContext::Global,
        chord: Some((Mods::None, Key::Space)),
        keys_label: "",
        desc: "Play / stop",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::VolumeDown,
        context: KeyContext::Global,
        chord: Some((Mods::None, Key::A)),
        keys_label: "",
        desc: "Master volume -1 dB",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::VolumeUp,
        context: KeyContext::Global,
        chord: Some((Mods::None, Key::D)),
        keys_label: "",
        desc: "Master volume +1 dB",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::SwitchTab,
        context: KeyContext::Global,
        chord: None,
        keys_label: "Ctrl+1..9",
        desc: "Switch workspace: 1 = List, 2..9 = editor tabs",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::SaveSession,
        context: KeyContext::Global,
        chord: Some((Mods::Command, Key::S)),
        keys_label: "",
        desc: "Save session",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::SaveSessionAs,
        context: KeyContext::Global,
        chord: Some((Mods::CommandShift, Key::S)),
        keys_label: "",
        desc: "Save session as...",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::NewWindow,
        context: KeyContext::Global,
        chord: Some((Mods::CommandShift, Key::N)),
        keys_label: "",
        desc: "Open a new window",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ExportSelected,
        context: KeyContext::Global,
        chord: Some((Mods::Command, Key::E)),
        keys_label: "",
        desc: "Export selected files",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::CloseTab,
        context: KeyContext::Global,
        chord: Some((Mods::Command, Key::W)),
        keys_label: "",
        desc: "Close the active editor tab (asks when dirty)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::Undo,
        context: KeyContext::Global,
        chord: None,
        keys_label: "Ctrl+Z",
        desc: "Undo (list or editor, scope follows focus)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::Redo,
        context: KeyContext::Global,
        chord: None,
        keys_label: "Ctrl+Shift+Z / Ctrl+Y",
        desc: "Redo",
        dispatch: Dispatch::Manual,
    },
    // ---- List ----
    KeyBinding {
        action: Action::ListToggleAutoplay,
        context: KeyContext::List,
        chord: Some((Mods::None, Key::P)),
        keys_label: "",
        desc: "Toggle auto-play on navigation",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ListToggleRegex,
        context: KeyContext::List,
        chord: Some((Mods::None, Key::R)),
        keys_label: "",
        desc: "Toggle regex search",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ListOpenSelected,
        context: KeyContext::List,
        chord: None,
        keys_label: "Enter",
        desc: "Open the selected rows in the editor",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListNavigate,
        context: KeyContext::List,
        chord: None,
        keys_label: "Up/Down, PgUp/PgDn, Home/End",
        desc: "Move the selection (Shift extends the range)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListRenameInline,
        context: KeyContext::List,
        chord: Some((Mods::None, Key::F2)),
        keys_label: "",
        desc: "Rename the selected file in place",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListCopyPaste,
        context: KeyContext::List,
        chord: None,
        keys_label: "Ctrl+C / Ctrl+V",
        desc: "Copy selected files / paste files into the list",
        dispatch: Dispatch::Manual,
    },
    // ---- Editor ----
    KeyBinding {
        action: Action::EditorSetLoopStart,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::K)),
        keys_label: "",
        desc: "Set loop start at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorSetLoopEnd,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::P)),
        keys_label: "",
        desc: "Set loop end at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorApplyLoop,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::L)),
        keys_label: "",
        desc: "Apply loop from selection/markers, else cycle loop mode",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorCycleViewMode,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::S)),
        keys_label: "",
        desc: "Cycle view mode (Waveform / Spectrogram / Log / Mel / ...)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorToggleBpm,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::B)),
        keys_label: "",
        desc: "Toggle the BPM grid",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorAddMarker,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::M)),
        keys_label: "",
        desc: "Add a marker at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorToggleZeroCross,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::R)),
        keys_label: "",
        desc: "Toggle zero-cross snap",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorDeleteSelection,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::C)),
        keys_label: "",
        desc: "Delete the selection and join (undoable)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorTrimSelection,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::T)),
        keys_label: "",
        desc: "Trim to the selection (undoable)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorVirtualTrim,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::V)),
        keys_label: "",
        desc: "Create a virtual trim item from the selection",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorDigitSeek,
        context: KeyContext::Editor,
        chord: None,
        keys_label: "1..9, 0",
        desc: "Seek across the file (1 = start, ..., 0 = end)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::EditorAudioClipboard,
        context: KeyContext::Editor,
        chord: None,
        keys_label: "Ctrl+C / X / V (+Shift/Alt)",
        desc: "Copy / cut / paste-insert audio; Shift+V mixes, Alt+V crossfades",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::EditorSeekStart,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::Home)),
        keys_label: "",
        desc: "Seek to the start of the file",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorSeekEnd,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::End)),
        keys_label: "",
        desc: "Seek to the end of the file",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomToSelection,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::Z)),
        keys_label: "",
        desc: "Zoom the view to the selection",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomIn,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::Plus)),
        keys_label: "",
        desc: "Zoom in around the playhead (= works too)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomOut,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::Minus)),
        keys_label: "",
        desc: "Zoom out around the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorViewPageBack,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::OpenBracket)),
        keys_label: "",
        desc: "Scroll the view back one page",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorViewPageForward,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::CloseBracket)),
        keys_label: "",
        desc: "Scroll the view forward one page",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorCancelPreview,
        context: KeyContext::Editor,
        chord: Some((Mods::None, Key::Escape)),
        keys_label: "",
        desc: "Discard the pending tool preview",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorArrowKeys,
        context: KeyContext::Editor,
        chord: None,
        keys_label: "Left/Right (+Shift/Alt/Ctrl)",
        desc: "Seek; Shift extends selection, Alt steps zero-cross, Ctrl steps one sample",
        dispatch: Dispatch::Manual,
    },
];

pub fn binding(action: Action) -> Option<&'static KeyBinding> {
    KEYMAP.iter().find(|b| b.action == action)
}

/// Consume the table-defined chord for `action`. Returns false for manual or
/// unbound actions.
pub fn consume(ctx: &egui::Context, action: Action) -> bool {
    let Some(b) = binding(action) else {
        return false;
    };
    let Some((mods, key)) = b.chord else {
        return false;
    };
    ctx.input_mut(|i| i.consume_key(mods.to_modifiers(), key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_roundtrip_for_every_row() {
        for b in KEYMAP {
            let name = b.action.name();
            assert_eq!(
                Action::from_name(&name),
                Some(b.action),
                "action name should round-trip: {name}"
            );
        }
        assert_eq!(Action::from_name("NoSuchAction"), None);
    }

    #[test]
    fn chord_text_roundtrips_for_representative_chords() {
        let cases = [
            (Mods::None, Key::Z),
            (Mods::None, Key::Plus),
            (Mods::Shift, Key::F5),
            (Mods::Command, Key::S),
            (Mods::CommandShift, Key::Z),
            (Mods::CommandShift, Key::Plus),
        ];
        for (mods, key) in cases {
            let text = chord_text(mods, key);
            assert_eq!(
                parse_chord(&text),
                Some((mods, key)),
                "chord should round-trip: {text}"
            );
        }
        assert_eq!(parse_chord("Ctrl+NoSuchKey"), None);
    }

    #[test]
    fn keymap_has_no_duplicate_chords_per_context() {
        for (i, a) in KEYMAP.iter().enumerate() {
            let Some(ca) = a.chord else { continue };
            for b in KEYMAP.iter().skip(i + 1) {
                let Some(cb) = b.chord else { continue };
                // Global chords must also not collide with List/Editor ones.
                let contexts_overlap = a.context == b.context
                    || a.context == KeyContext::Global
                    || b.context == KeyContext::Global;
                assert!(
                    !(contexts_overlap && ca == cb),
                    "duplicate chord {:?} for {:?} and {:?}",
                    ca,
                    a.action,
                    b.action
                );
            }
        }
    }

    #[test]
    fn keymap_every_action_has_one_row() {
        for (i, a) in KEYMAP.iter().enumerate() {
            for b in KEYMAP.iter().skip(i + 1) {
                assert!(
                    a.action != b.action,
                    "action {:?} appears twice in KEYMAP",
                    a.action
                );
            }
        }
    }

    #[test]
    fn keymap_rows_have_key_text() {
        for b in KEYMAP {
            assert!(
                !b.keys_text().is_empty(),
                "binding {:?} renders empty key text",
                b.action
            );
            assert!(!b.desc.is_empty(), "binding {:?} has no description", b.action);
        }
    }

    #[test]
    fn table_rows_have_chords_and_manual_rows_have_labels() {
        for b in KEYMAP {
            match b.dispatch {
                Dispatch::Table => assert!(
                    b.chord.is_some(),
                    "table-dispatched {:?} must define a chord",
                    b.action
                ),
                Dispatch::Manual => {
                    if b.chord.is_none() {
                        assert!(
                            !b.keys_label.is_empty(),
                            "manual {:?} without chord needs keys_label",
                            b.action
                        );
                    }
                }
            }
        }
    }
}
