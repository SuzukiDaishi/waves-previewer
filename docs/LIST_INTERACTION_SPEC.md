# List Interaction Spec (v2)

Purpose
- Make selection, playback, and scrolling predictable.
- Keep list performance responsive for 300k files.

Observed Issues
- Clicking rows does not always select.
- Keyboard selection can move off-screen with no auto-scroll.
- Long files can delay playback start.

Selection Model
- Single selection is always tracked.
- Multi-selection supports:
  - Shift: range select
  - Ctrl/Cmd: toggle select
- Clicking any cell selects the row and updates selection state.

Click Behavior
- Single click:
  - Select row
  - Load for immediate playback (list preview)
- Double click:
  - Open editor tab
- Folder column double click:
  - Open OS file browser with file selected

Keyboard Behavior
- Up/Down: move selection by 1 row.
- PageUp/PageDown: move selection by visible rows.
- Home/End: jump to start/end.
- Enter: open in editor.

Auto Scroll
- When selection changes by keyboard, ensure row is visible.
- If row is outside viewport, scroll so it becomes visible with a small margin.
- If user is actively scrolling (mouse wheel within last 300 ms), defer auto-scroll.

Playback Start (List Preview)
- Goal: audible start within 100-150 ms for typical files.
- Strategy:
  - Decode minimal chunk (e.g., first 0.25 to 0.5 sec) on selection.
  - Start playback immediately from the chunk.
  - Continue decoding in background for seamless continuation.
  - If heavy processing mode is active (Pitch/Stretch), force Speed mode for list preview.

List Rendering
- Virtualized rows only.
- Metadata:
  - Quick header data on demand.
  - Full meta in background worker.
- Avoid scanning full meta for loading indicator.

Acceptance Criteria
- Click always selects row.
- Keyboard selection keeps the row in view.
- Long file selection starts playback quickly.
