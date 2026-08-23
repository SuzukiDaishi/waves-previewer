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

/// What a shortcut is *for*, within its context.
///
/// The help was one flat table per context, which made the Editor's forty-odd
/// rows a wall to read and buried the loop keys somewhere in the middle of it.
/// Grouping by task is what lets someone find "the loop ones" without already
/// knowing which key they are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCategory {
    Files,
    Playback,
    Navigation,
    Selection,
    Loop,
    Editing,
    View,
    Tabs,
}

impl KeyCategory {
    pub fn title(self) -> &'static str {
        match self {
            KeyCategory::Files => "Files",
            KeyCategory::Playback => "Playback",
            KeyCategory::Navigation => "Navigation",
            KeyCategory::Selection => "Selection",
            KeyCategory::Loop => "Loop",
            KeyCategory::Editing => "Editing",
            KeyCategory::View => "View",
            KeyCategory::Tabs => "Tabs & Windows",
        }
    }

    /// Display order within a context.
    pub const ALL: [KeyCategory; 8] = [
        KeyCategory::Playback,
        KeyCategory::Navigation,
        KeyCategory::Selection,
        KeyCategory::Loop,
        KeyCategory::Editing,
        KeyCategory::View,
        KeyCategory::Tabs,
        KeyCategory::Files,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    // Global
    FocusSearch,
    TogglePlay,
    VolumeDown,
    VolumeUp,
    SwitchTab,
    CycleEditorTab,
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
    EditorToggleMarkerLoop,
    EditorCycleLoopMode,
    EditorCycleViewMode,
    EditorToggleBpm,
    EditorAddMarker,
    EditorToggleZeroCross,
    EditorSelectAll,
    EditorDeleteSelection,
    EditorTrimSelection,
    EditorMuteSelection,
    EditorVirtualTrim,
    EditorDigitSeek,
    EditorArrowKeys,
    EditorAudioClipboard,
    EditorSeekStart,
    EditorSeekEnd,
    EditorZoomToSelection,
    EditorZoomToLoopRegion,
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
        // This action was `EditorApplyLoop` until the mode cycle it had been
        // tangled with was split out of it. Prefs written before that name it
        // the old way, and without the alias a user's rebinding of it would be
        // dropped on the next load without a word.
        let s = if s == "EditorApplyLoop" {
            "EditorToggleMarkerLoop"
        } else {
            s
        };
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
    /// What this shortcut is for, used to group the help.
    pub category: KeyCategory,
    /// A sentence or two on what the key actually does, for rows whose one-line
    /// `desc` cannot say enough -- most often what happens when the thing it
    /// acts on is not there. Empty where the one-liner is the whole story.
    pub detail: &'static str,
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
        category: KeyCategory::Navigation,
        detail: "",
        chord: Some((Mods::Command, Key::F)),
        keys_label: "",
        desc: "Focus the search box",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::TogglePlay,
        context: KeyContext::Global,
        category: KeyCategory::Playback,
        detail: "",
        chord: Some((Mods::None, Key::Space)),
        keys_label: "",
        desc: "Play / stop",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::VolumeDown,
        context: KeyContext::Global,
        category: KeyCategory::Playback,
        detail: "",
        chord: Some((Mods::None, Key::A)),
        keys_label: "",
        desc: "Master volume -1 dB",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::VolumeUp,
        context: KeyContext::Global,
        category: KeyCategory::Playback,
        detail: "",
        chord: Some((Mods::None, Key::D)),
        keys_label: "",
        desc: "Master volume +1 dB",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::SwitchTab,
        context: KeyContext::Global,
        category: KeyCategory::Tabs,
        detail: "",
        chord: None,
        keys_label: "Ctrl+1..9",
        desc: "Switch workspace: 1 = List, 2..9 = editor tabs",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::CycleEditorTab,
        context: KeyContext::Global,
        category: KeyCategory::Tabs,
        detail: "Only while an editor tab is in front and more than one is open; elsewhere Tab still moves focus between controls.",
        chord: None,
        keys_label: "Tab / Shift+Tab",
        desc: "Next / previous editor tab (wraps; editor workspace only)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::SaveSession,
        context: KeyContext::Global,
        category: KeyCategory::Files,
        detail: "",
        chord: Some((Mods::Command, Key::S)),
        keys_label: "",
        desc: "Save session",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::SaveSessionAs,
        context: KeyContext::Global,
        category: KeyCategory::Files,
        detail: "",
        chord: Some((Mods::CommandShift, Key::S)),
        keys_label: "",
        desc: "Save session as...",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::NewWindow,
        context: KeyContext::Global,
        category: KeyCategory::Tabs,
        detail: "",
        chord: Some((Mods::CommandShift, Key::N)),
        keys_label: "",
        desc: "Open a new window",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ExportSelected,
        context: KeyContext::Global,
        category: KeyCategory::Files,
        detail: "",
        chord: Some((Mods::Command, Key::E)),
        keys_label: "",
        desc: "Export selected files",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::CloseTab,
        context: KeyContext::Global,
        category: KeyCategory::Tabs,
        detail: "",
        chord: Some((Mods::Command, Key::W)),
        keys_label: "",
        desc: "Close the active editor tab (asks when dirty)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::Undo,
        context: KeyContext::Global,
        category: KeyCategory::Editing,
        detail: "",
        chord: None,
        keys_label: "Ctrl+Z",
        desc: "Undo (list or editor, scope follows focus)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::Redo,
        context: KeyContext::Global,
        category: KeyCategory::Editing,
        detail: "",
        chord: None,
        keys_label: "Ctrl+Shift+Z / Ctrl+Y",
        desc: "Redo",
        dispatch: Dispatch::Manual,
    },
    // ---- List ----
    KeyBinding {
        action: Action::ListToggleAutoplay,
        context: KeyContext::List,
        category: KeyCategory::Playback,
        detail: "",
        chord: Some((Mods::None, Key::P)),
        keys_label: "",
        desc: "Toggle auto-play on navigation",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ListToggleRegex,
        context: KeyContext::List,
        category: KeyCategory::Navigation,
        detail: "",
        chord: Some((Mods::None, Key::R)),
        keys_label: "",
        desc: "Toggle regex search",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::ListOpenSelected,
        context: KeyContext::List,
        category: KeyCategory::Navigation,
        detail: "",
        chord: None,
        keys_label: "Enter",
        desc: "Open the selected rows in the editor",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListNavigate,
        context: KeyContext::List,
        category: KeyCategory::Navigation,
        detail: "",
        chord: None,
        keys_label: "Up/Down, PgUp/PgDn, Home/End",
        desc: "Move the selection (Shift extends the range)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListRenameInline,
        context: KeyContext::List,
        category: KeyCategory::Editing,
        detail: "",
        chord: Some((Mods::None, Key::F2)),
        keys_label: "",
        desc: "Rename the selected file in place",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::ListCopyPaste,
        context: KeyContext::List,
        category: KeyCategory::Files,
        detail: "Paste takes files copied in the OS file browser, as a file list or as pasted paths, and reports what it added and what it skipped.",
        chord: None,
        keys_label: "Ctrl+C / Ctrl+V",
        desc: "Copy selected files / paste files into the list",
        dispatch: Dispatch::Manual,
    },
    // ---- Editor ----
    KeyBinding {
        action: Action::EditorSetLoopStart,
        context: KeyContext::Editor,
        category: KeyCategory::Loop,
        detail: "Moves the loop start to the playhead. The end stays where it is; if that would put the start past it, the two swap.",
        chord: Some((Mods::None, Key::K)),
        keys_label: "",
        desc: "Set loop start at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorSetLoopEnd,
        context: KeyContext::Editor,
        category: KeyCategory::Loop,
        detail: "Moves the loop end to the playhead. The start stays where it is.",
        chord: Some((Mods::None, Key::P)),
        keys_label: "",
        desc: "Set loop end at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorToggleMarkerLoop,
        context: KeyContext::Editor,
        category: KeyCategory::Loop,
        detail: "With a loop region set, turns marker looping on from it. With no loop region but a selection, makes the loop out of the selection. With neither, falls back to cycling the loop mode.",
        chord: Some((Mods::None, Key::L)),
        keys_label: "",
        desc: "Apply loop from selection/markers, else cycle loop mode",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorCycleLoopMode,
        context: KeyContext::Editor,
        category: KeyCategory::Loop,
        detail: "Off, then the whole file, then the marker loop, then off again. Marker loop needs a loop region; with none set it lands back on off.",
        chord: Some((Mods::Shift, Key::L)),
        keys_label: "",
        desc: "Cycle the loop mode",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorCycleViewMode,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::S)),
        keys_label: "",
        desc: "Cycle view mode (Waveform / Spectrogram / Log / Mel / ...)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorToggleBpm,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::B)),
        keys_label: "",
        desc: "Toggle the BPM grid",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorAddMarker,
        context: KeyContext::Editor,
        category: KeyCategory::Navigation,
        detail: "",
        chord: Some((Mods::None, Key::M)),
        keys_label: "",
        desc: "Add a marker at the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorToggleZeroCross,
        context: KeyContext::Editor,
        category: KeyCategory::Selection,
        detail: "Snaps a dragged loop edge to the nearest zero crossing. A marker within reach still wins over it.",
        chord: Some((Mods::None, Key::R)),
        keys_label: "",
        desc: "Toggle zero-cross snap",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorSelectAll,
        context: KeyContext::Editor,
        category: KeyCategory::Selection,
        detail: "The inspector range readout keeps saying entire file for as long as the selection covers everything.",
        chord: Some((Mods::Command, Key::A)),
        keys_label: "",
        desc: "Select the whole file",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorDeleteSelection,
        context: KeyContext::Editor,
        // Delete rather than `C`: this is the Trim inspector's Cut, and Delete
        // is what a sound designer reaches for. The list and the effect graph
        // also use Delete, but each is scoped to its own workspace.
        category: KeyCategory::Editing,
        detail: "",
        chord: Some((Mods::None, Key::Delete)),
        keys_label: "",
        desc: "Delete the selection and join (undoable)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorTrimSelection,
        context: KeyContext::Editor,
        category: KeyCategory::Editing,
        detail: "",
        chord: Some((Mods::None, Key::T)),
        keys_label: "",
        desc: "Trim to the selection (undoable)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorMuteSelection,
        context: KeyContext::Editor,
        // Bare `M` is already "add a marker", so mute takes the Ctrl chord.
        category: KeyCategory::Editing,
        detail: "",
        chord: Some((Mods::Command, Key::M)),
        keys_label: "",
        desc: "Mute the selection (undoable)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorVirtualTrim,
        context: KeyContext::Editor,
        category: KeyCategory::Files,
        detail: "",
        chord: Some((Mods::None, Key::V)),
        keys_label: "",
        desc: "Create a virtual trim item from the selection",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorDigitSeek,
        context: KeyContext::Editor,
        category: KeyCategory::Navigation,
        detail: "",
        chord: None,
        keys_label: "1..9, 0",
        desc: "Seek across the file (1 = start, ..., 0 = end)",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::EditorAudioClipboard,
        context: KeyContext::Editor,
        category: KeyCategory::Editing,
        detail: "",
        chord: None,
        keys_label: "Ctrl+C / X / V (+Shift/Alt)",
        desc: "Copy / cut / paste-insert audio; Shift+V mixes, Alt+V crossfades",
        dispatch: Dispatch::Manual,
    },
    KeyBinding {
        action: Action::EditorSeekStart,
        context: KeyContext::Editor,
        category: KeyCategory::Navigation,
        detail: "",
        chord: Some((Mods::None, Key::Home)),
        keys_label: "",
        desc: "Seek to the start of the file",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorSeekEnd,
        context: KeyContext::Editor,
        category: KeyCategory::Navigation,
        detail: "",
        chord: Some((Mods::None, Key::End)),
        keys_label: "",
        desc: "Seek to the end of the file",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomToSelection,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::Z)),
        keys_label: "",
        desc: "Zoom the view to the selection",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomToLoopRegion,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "Fits the loop region to the view with a small margin. Double-clicking a loop handle instead zooms in one step around that end.",
        chord: Some((Mods::Shift, Key::Z)),
        keys_label: "",
        desc: "Zoom the view to the loop region",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomIn,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::Plus)),
        keys_label: "",
        desc: "Zoom in around the playhead (= works too)",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorZoomOut,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::Minus)),
        keys_label: "",
        desc: "Zoom out around the playhead",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorViewPageBack,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::OpenBracket)),
        keys_label: "",
        desc: "Scroll the view back one page",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorViewPageForward,
        context: KeyContext::Editor,
        category: KeyCategory::View,
        detail: "",
        chord: Some((Mods::None, Key::CloseBracket)),
        keys_label: "",
        desc: "Scroll the view forward one page",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorCancelPreview,
        context: KeyContext::Editor,
        category: KeyCategory::Editing,
        detail: "",
        chord: Some((Mods::None, Key::Escape)),
        keys_label: "",
        desc: "Discard the pending tool preview",
        dispatch: Dispatch::Table,
    },
    KeyBinding {
        action: Action::EditorArrowKeys,
        context: KeyContext::Editor,
        category: KeyCategory::Navigation,
        detail: "The step follows the time or BPM grid. Markers and loop points stop it, so a step that would cross one lands on it exactly.",
        chord: None,
        keys_label: "Left/Right (+Shift/Alt/Ctrl)",
        desc: "Seek, stopping on markers and loop points; Shift extends selection, Alt steps zero-cross, Ctrl steps one sample",
        dispatch: Dispatch::Manual,
    },
];

/// Chords owned by manually-dispatched handler families (raw `consume_key`
/// paths: undo/redo, the audio clipboard, Ctrl+digit tab switching, digit
/// seek, the `=` zoom alias). They never appear as table chords, so the
/// rebind overlap check cannot see them — the rebinding UI refuses them via
/// this list instead.
pub const RESERVED_CHORDS: &[(Mods, Key)] = &[
    (Mods::Command, Key::Z),
    (Mods::Command, Key::Y),
    (Mods::CommandShift, Key::Z),
    (Mods::Command, Key::C),
    (Mods::Command, Key::X),
    (Mods::Command, Key::V),
    (Mods::CommandShift, Key::V),
    (Mods::Command, Key::Num1),
    (Mods::Command, Key::Num2),
    (Mods::Command, Key::Num3),
    (Mods::Command, Key::Num4),
    (Mods::Command, Key::Num5),
    (Mods::Command, Key::Num6),
    (Mods::Command, Key::Num7),
    (Mods::Command, Key::Num8),
    (Mods::Command, Key::Num9),
    (Mods::None, Key::Num0),
    (Mods::None, Key::Num1),
    (Mods::None, Key::Num2),
    (Mods::None, Key::Num3),
    (Mods::None, Key::Num4),
    (Mods::None, Key::Num5),
    (Mods::None, Key::Num6),
    (Mods::None, Key::Num7),
    (Mods::None, Key::Num8),
    (Mods::None, Key::Num9),
    (Mods::None, Key::Equals),
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
            assert!(
                !b.desc.is_empty(),
                "binding {:?} has no description",
                b.action
            );
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

    #[test]
    fn the_old_name_for_the_split_loop_action_still_resolves() {
        // A user who rebound this before the mode cycle was split out of it has
        // `keymap=EditorApplyLoop=...` in their prefs. Dropping the alias would
        // discard that binding on the next load without saying anything.
        assert_eq!(
            Action::from_name("EditorApplyLoop"),
            Some(Action::EditorToggleMarkerLoop)
        );
        assert_eq!(
            Action::from_name("EditorToggleMarkerLoop"),
            Some(Action::EditorToggleMarkerLoop)
        );
        assert_eq!(Action::from_name("NoSuchAction"), None);
    }

    #[test]
    fn every_category_that_claims_a_row_is_in_the_display_order() {
        // A row whose category is missing from `ALL` is simply not drawn, and
        // nothing else would notice.
        for binding in KEYMAP {
            assert!(
                KeyCategory::ALL.contains(&binding.category),
                "{:?} is in {:?}, which the help never renders",
                binding.action,
                binding.category
            );
        }
    }

    #[test]
    fn the_loop_keys_explain_what_happens_when_there_is_no_loop() {
        // The whole point of the Loop group: its keys behave differently
        // depending on what is already set, and the one-line desc has no room
        // to say so.
        for binding in KEYMAP.iter().filter(|b| b.category == KeyCategory::Loop) {
            assert!(
                !binding.detail.is_empty(),
                "{:?} needs a detail line",
                binding.action
            );
        }
    }
}
