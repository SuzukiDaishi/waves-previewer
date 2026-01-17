# List Clipboard Copy/Paste Plan (v1)

Goal
- Replace "Copy to folder" with "Copy to Clipboard" for list items.
- Allow OS-level paste (DAW/Desktop) to create a file copy.
- If the item has edits, paste the edited audio, without modifying the source file or list item.
- Paste into the list creates a new virtual item named like "name (1)" that exists only in-app until Save.

Non-goals (for now)
- No destructive changes to the source file.
- No background sync of clipboard temp files across app sessions.

Requirements Summary
- Copy selected items -> clipboard.
- External paste should behave like file copy (CF_HDROP on Windows).
- Edited audio should be pasted if the item has edits (editor/pending).
- List paste should create a virtual item (not a real file yet).
- Save should materialize virtual items to files.

Proposed Data Model Changes
- Add a source enum for list items:
  - MediaSource::File { path: PathBuf }
  - MediaSource::Virtual { name: String, audio: AudioBuffer, origin: Option<PathBuf> }
- MediaItem gains:
  - source: MediaSource
  - display_name: String (used for list UI)
  - folder_label: String (e.g., real folder or "(virtual)")
- Replace path-only logic with helpers:
  - item_path(&MediaItem) -> Option<&Path>
  - item_audio(&MediaItem, edits_cache, active_tab) -> AudioBuffer
- Move edited_cache key from PathBuf to MediaId (or a new ItemKey enum), so virtual items can be edited and cached without a file path.

Clipboard Architecture
- Add an internal clipboard payload:
  - struct ClipboardPayload { items: Vec<ClipboardItem>, created_at: Instant }
  - ClipboardItem holds MediaId and audio data or references.
- Keep OS clipboard in sync for external paste:
  - Windows: use CF_HDROP with real file paths.
  - For edited audio, export a temp WAV to a NeoWaves temp dir and put that path in CF_HDROP.
  - For unedited audio, use the original file path in CF_HDROP.
- Provide a small abstraction for future platforms:
  - trait ClipboardBackend { set_files(Vec<PathBuf>), get_files() -> Vec<PathBuf> }
  - Windows implementation uses clipboard-win (CF_HDROP).
  - Fallback implementation uses text (newline-separated paths) for non-Windows.

Temp File Strategy (Windows)
- Use %TEMP%/NeoWaves/clipboard/<uuid>.wav for edited audio.
- Maintain a ClipboardTempManager:
  - track temp files from the latest copy
  - delete previous temp files on next copy
  - delete on app shutdown
- Export format: WAV PCM 16-bit (wide compatibility).

Copy Behavior (List -> Clipboard)
- Determine "effective audio" for each selected item:
  - If edited_cache has an entry for the item, use that.
  - Else if the editor tab for that item is open with unsaved edits, use the current edited buffer.
  - Else use the original file.
- Populate internal clipboard with audio buffers (even if OS clipboard uses files).
- Populate OS clipboard with CF_HDROP file list.
- Do not change list item state (no edits cleared, no status changes).

Paste Behavior (Clipboard -> List)
- Ctrl+V and context menu "Paste":
  - If internal clipboard is present and fresh, create virtual items from it.
  - Else if OS clipboard has file paths, add files as normal list items (existing behavior).
- Virtual item naming:
  - Base name from original filename.
  - Generate unique "name (1)" / "name (2)" within the list.
- Virtual items should be fully editable like normal items.
- List sort/filters should treat virtual items consistently (file name column uses display_name).

Save Behavior
- On Save Selected:
  - For file items: current behavior (write edits back to file or export).
  - For virtual items: prompt for destination folder and write to disk.
  - After successful write, convert MediaSource::Virtual -> MediaSource::File and update metadata.

UI/UX Changes
- Replace "Copy..." with "Copy to Clipboard" in list context menu and List menu.
- Add "Paste" to list context menu and List menu.
- Keep "Copy to Folder..." available under a submenu or separate command if needed.
- Provide lightweight status feedback in the top bar (e.g., "Copied N items" or "Clipboard export failed").

Edge Cases and Safeguards
- Large multi-select: show a confirmation when total duration or count is large.
- Missing file: skip with a log and keep other items.
- Edited audio but no editor data: fall back to original file, log warning.
- Temp files should be cleaned to avoid disk bloat.

Implementation Steps
1) Data model refactor
   - Add MediaSource and display fields.
   - Update helper functions to use item_path().
   - Move edited_cache to MediaId or ItemKey.
2) Internal clipboard
   - Add ClipboardPayload to app state.
   - Populate on copy with AudioBuffer for each item.
3) Windows clipboard backend
   - Add clipboard-win dependency.
   - Implement CF_HDROP set/get.
   - Add temp file export pipeline for edited audio.
4) List paste logic
   - Add Ctrl+V handler to list.
   - Create virtual items from internal clipboard.
5) Save virtual items
   - Extend save path handling for virtual items.
   - Convert virtual to file after save.
6) UI updates
   - Update menu labels and add Paste.
   - Add minimal status indicator.
7) Testing
   - Manual: copy -> paste into Explorer and DAW.
   - Manual: copy edited item and verify pasted audio is edited.
   - In-app: copy -> paste in list, edit virtual item, save to file.

Acceptance Criteria
- Copy creates a clipboard payload that pastes into OS as files.
- Edited items paste edited audio without modifying originals.
- Paste into list creates virtual items and does not touch disk until Save.
- Virtual items behave like normal items in edit and playback.
