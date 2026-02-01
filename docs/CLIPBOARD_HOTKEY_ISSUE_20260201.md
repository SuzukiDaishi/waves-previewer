# Clipboard Hotkey Issue (Ctrl+C/V not received, Ctrl+Z works)

## Summary
List view hotkeys are mostly fixed (arrow keys, Enter, Space), but **Ctrl+C / Ctrl+V are not being detected**.
Ctrl+Z **does work**, which implies the app is receiving at least some Ctrl-modified key events.
Debug UI indicates **Ctrl modifier is seen**, but **C/V key events are not**.

This document summarizes the environment, current behavior, instrumentation, and the exact code path.
We are preparing to ask external maintainers for help (egui/eframe/winit).

---

## Environment
- OS: Windows (user environment)
- UI framework: `eframe 0.33.3`, `egui 0.33.3`, `egui_extras 0.33.3`
  - Cargo.toml:
    - `eframe = { version = "0.33.3", default-features = false }`
    - `egui = "0.33.3"`
    - `egui_extras = "0.33.3"`

---

## Current User Symptoms
- **Ctrl+Z works** (Undo triggers)
- **Ctrl+C / Ctrl+V do not trigger** copy/paste in list
- List arrow keys + Enter + Space now work correctly
- Debug output shows:
  - `clip_allow: true`
  - `ctrl: true`
  - `clip_events: copy:false paste:false`
  - `clip_raw_keys: c:false v:false`
  - `clip_consumed: copy:false paste:false`
  - This suggests: **Ctrl is seen, C/V key events are not**

---

## Repro Steps
1) Launch app → list view
2) Click a list item so it is selected
3) Press Ctrl+C / Ctrl+V
4) Observe Debug window Clipboard block:
   - `clip_allow: true`
   - `ctrl: true`
   - `clip_events: copy:false paste:false`
   - `clip_raw_keys: c:false v:false`
5) Press Ctrl+Z → works (undo triggers)

---

## Instrumentation in Code

### Clipboard Hotkeys (src/app.rs)
Function: `handle_clipboard_hotkeys`

This function currently:
1) Checks focus/selection:
   - allow = **NOT search focused** AND (list focus OR selection exists)
2) Tries 3 detection paths:
   - `consume_key(Cmd/Ctrl + C/V)`
   - `key_down` edge with Ctrl held
   - raw events scan from `ctx.input(|i| i.raw.events)`
3) Triggers copy/paste if any of these fire

Relevant excerpt:

```
let search_focused = ctx.memory(|m| m.has_focus(Self::search_box_id()));
let list_focus = self.list_has_focus || ctx.memory(|m| m.has_focus(Self::list_focus_id()));
let allow = !search_focused && (list_focus || self.selected.is_some() || !self.selected_multi.is_empty());

let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
let down_c = ctx.input(|i| i.key_down(egui::Key::C));
let down_v = ctx.input(|i| i.key_down(egui::Key::V));

// raw input events
let mut raw_copy = false;
let mut raw_paste = false;
let mut raw_ctrl = false;
ctx.input(|i| {
    for ev in &i.raw.events {
        if let egui::Event::Key { key, pressed: true, modifiers, .. } = ev {
            if modifiers.ctrl || modifiers.command {
                raw_ctrl = true;
                match key {
                    egui::Key::C => raw_copy = true,
                    egui::Key::V => raw_paste = true,
                    _ => {}
                }
            }
        }
    }
});

let edge_c = allow && ctrl && down_c && !self.clipboard_c_was_down;
let edge_v = allow && ctrl && down_v && !self.clipboard_v_was_down;

let consumed_copy = allow && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C));
let consumed_paste = allow && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::V));

let copy_trigger = consumed_copy || edge_c || (allow && raw_copy);
let paste_trigger = consumed_paste || edge_v || (allow && raw_paste);
```

Despite this, Ctrl+C/V **never reaches any trigger** (raw and consumed both false).

---

## Other Relevant Focus Logic

### Search focus ID
Search TextEdit is given a fixed Id to reliably detect focus:

```
TextEdit::singleline(&mut self.search_query)
    .hint_text("Search...")
    .id(WavesPreviewer::search_box_id());
```

### List keyboard handling (src/app/ui/list.rs)
List arrow keys now bypass `wants_keyboard_input` and only gate on search focus:

```
let search_focused = ctx.memory(|m| m.has_focus(WavesPreviewer::search_box_id()));
let allow_list_keys = self.active_tab.is_none() && !self.files.is_empty() && !search_focused;
```

Arrows/Enter now work, so this part is likely OK.

---

## Why This Is Puzzling
If Ctrl+Z is received, **Ctrl-modifier and key events are reaching egui**.
But Ctrl+C/V never appear, **even in raw input events**.

This suggests:
1) **OS or IME might be intercepting Ctrl+C/V before egui**  
2) **Some upstream accelerator / global shortcut in the app may be consuming C/V**  
3) **Specific key combinations (C/V) are filtered by winit/eframe in certain focus states**  

Yet Ctrl+Z working suggests that keyboard event delivery is not universally broken.

---

## Questions for External Maintainers
1) Is there any known issue where **Ctrl+C / Ctrl+V do not appear in egui raw events** on Windows?
2) Are there cases where **Clipboard commands are filtered by platform integration** (winit/eframe)?
3) Does egui ever convert Ctrl+C/V to `Event::Copy` / `Event::Paste` without a raw `Key` event?
4) If so, is there a recommended way to detect copy/paste independent of key events?
5) Could a focused widget (even invisible) be swallowing Ctrl+C/V before raw events are surfaced?
6) Is `raw.events` expected to be free of consumed events, or can it be empty for copy/paste?

---

## Current Debug Fields (Clipboard Block)
We log:
- `clip_allow`
- `wants_kb`
- `ctrl`
- `clip_events` (raw events: Ctrl+C/V)
- `clip_raw_keys` (raw C/V)
- `clip_os_keys` (edge detection)
- `clip_consumed`
- `clip_triggers`

In the failing state:
```
clip_allow: true
wants_kb: true
ctrl: true
clip_events: copy:false paste:false
clip_raw_keys: c:false v:false
clip_consumed: copy:false paste:false
clip_triggers: copy:false paste:false
```

---

## Notes
- We intentionally removed `wants_keyboard_input` gating for list navigation.
- Copy/paste allow is gated only by **search focus** and selection.
- We are using `Modifiers::COMMAND` for consume_key; this should map to Ctrl on Windows.
- We already added edge-detection and raw event fallbacks.

---

## What We Need Help With
We need to understand **why Ctrl+C/V do not show up in `ctx.input().raw.events`** while Ctrl+Z does, on the same machine and same app state.

If there is a recommended egui/eframe/winit approach to detect clipboard commands in Windows (e.g., `Event::Copy`, `Event::Paste`, or dedicated clipboard commands), please advise.

