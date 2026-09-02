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
- A held key moves by however many presses the frame received, not by one.
  Auto-repeat outruns a frame that is sorting or decoding, and acting on one
  press while dropping the rest is what makes a held arrow stall on a row.
  Only the row actually landed on is loaded.
- The keys stay with the list. egui reads arrows as focus navigation whenever
  the focused widget has no lock filter claiming them, and that filter only
  binds to a widget that already held focus for a frame -- so the list never
  re-takes focus it holds, asks for a frame when it does take it, and takes
  focus back if an arrow it acted on moved it somewhere no pointer press or
  chord asked for. Where it otherwise lands is the search box or a topbar
  drag value, and a live caret there owns every key the list needs.

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
