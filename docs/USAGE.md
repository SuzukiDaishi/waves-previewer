# Usage Notes (Menu, D&D, Scrollbar)

This document summarizes the recent UX changes: the top-bar menu, drag & drop, and the list scrollbar layout.

## Top Menu: "Choose"

- Folder...
  - Select a folder and replace the list with its WAV files (recursively).
  - Internally sets `root` and runs a rescan.

- Files...
  - Select multiple files and replace the list with those WAV files.
  - Does not set `root` (clears it). Useful for ad‑hoc multi‑file preview.

Notes
- Only `.wav` is supported at the moment. Other formats will come with `symphonia`.
- Duplicates are skipped automatically.

For details of the upcoming editor features (multichannel, dB grid, seek, zoom), see `docs/EDITOR_SPEC.md`.

## Drag & Drop

- Dropping files or folders onto the window adds them to the list (WAV only).
- Folders are scanned recursively; only `.wav` files are added.
- Existing entries are de‑duplicated.
- Search and sort are preserved; metadata (RMS/thumbnail) is refreshed asynchronously.

## List Scrollbar at Right Edge

- The list view now always shows its vertical scrollbar at the right edge of the window.
- Implementation detail: a rightmost spacer column (remainder) is used so the table fills the available width while keeping the Wave column position unchanged.

## Quick Reference (unchanged)

- Space: Play/Pause
- L: Toggle Loop (editor tab)
- Arrow Up/Down: Move selection in list
- Enter: Open selected file in editor tab

For more context, see also:
- `CHANGELOG.md` (latest changes)
- `docs/CONTROLS.md` (full controls)
- `docs/UX.md` (design/UX notes)
