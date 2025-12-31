# Tab and Session Spec (v1)

Purpose
- Define how tabs behave, especially around edit state.
- Avoid multiple conflicting edit sessions.

Observed Issues
- Multiple edits can overlap without clear ownership.
- Tab behavior is not well defined.

Definitions
- List view: no editor tab active.
- Editor tab: a file opened for detailed view and editing.
- Edit session: active editor tab with tools enabled.

Rules
- Only one edit session at a time.
- Switching tabs:
  - If active tab is dirty, prompt user.
  - If active tab is previewing, cancel preview first.
- Background tabs are read-only:
  - Waveform is visible.
  - Transport is disabled (or only allows play with no edits).

Tab States
- Clean: no edits applied.
- Dirty: destructive edits applied.
- Previewing: temporary audio and overlay.
- Processing: background worker running.

State Transitions
- Clean -> Dirty: apply tool.
- Dirty -> Clean: export/save replaces buffer and resets dirty.
- Any -> Previewing: start preview.
- Previewing -> Clean/Dirty: cancel or apply.
- Any -> Processing: heavy job started.

User Prompts
- Leaving dirty tab:
  - Leave (keep edits in memory)
  - Cancel
- Closing dirty tab:
  - Close (keep edits in memory if reopen is allowed)
  - Cancel

Acceptance Criteria
- No more than one editable tab.
- Tool changes in inactive tabs are blocked or ignored.
- Switching tabs does not corrupt playback state.
