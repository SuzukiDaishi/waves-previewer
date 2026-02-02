Sound List Import (CSV/Excel) Plan
==================================

Goal
----
Provide a robust 窶徭ound list窶・import that supports CSV and Excel, works via drag & drop, and can
map rows to list items using flexible key rules (regex + path variables). The import should be
fast on large sheets and never block the list UI.

Scope
-----
1) Import sources:
   - CSV (`.csv`, `.tsv`, `.txt`) and Excel (`.xlsx`, `.xls`).
   - Drag & drop should open the same import flow as File > Import.
2) Header handling:
   - Auto-detect header row by default (heuristic).
   - User can override header row index.
   - Support headerless tables (columns named A, B, C窶ｦ).
3) Key matching:
   - Primary key regex using row values + file/path variables.
   - Default regex should match filename **or** stem.
   - Secondary 窶徭cope窶・rule (e.g., restrict search to a subfolder).
4) UI/UX:
   - A preview table (first N rows).
   - Sheet dropdown for Excel.
   - Header row selector + 窶彭ata starts at窶・selector.
   - Regex editor with variable list.
   - Status summary (matched, missing, duplicates).

User Flow
---------
1) User selects File > Import CSV/Excel窶ｦ or drops a file onto the app.
2) Import dialog opens:
   - File path shown (read-only).
   - If Excel: sheet dropdown.
   - Auto-detected header row (editable).
   - Data starts row (editable).
   - Column preview.
   - Primary key regex + test field.
   - Secondary scope (optional) with regex or folder column.
3) User presses 窶廣pply窶・
4) Import result:
   - External columns are added to the list (same system as CSV today).
   - Missing items are marked (no file matched).
   - No destructive changes to files.

Data Model (Proposal)
---------------------
Add a structured 窶彳xternal source窶・config saved in session (nwsess):
- `source_kind`: `Csv | Excel`
- `source_path`: path to file
- `sheet_name`: string (Excel only)
- `header_row`: 1-based index (0 = auto)
- `data_row`: 1-based index (defaults to header_row + 1)
- `has_header`: bool
- `delimiter`: optional (CSV auto-detect)
- `primary_key_regex`: string
- `secondary_scope_regex`: optional string
- `secondary_scope_column`: optional column name
- `key_columns`: list of column names used for key lookup

Matching Rules
--------------
1) Primary key regex:
   - Regex is applied to a synthesized 窶很ey input窶・string:
     - concatenation of selected columns + path variables.
   - Variables (expand at runtime):
     - `{path}` full path
     - `{dir}` parent directory
     - `{file}` filename with extension
     - `{stem}` filename without extension
     - `{ext}` extension
   - Default regex example (match filename or stem):
     - `(?i)(?:^|[\\\\/])(?P<name>[^\\\\/]+?)(?:\\.[^.\\\\/]+)?$`
2) Secondary scope (optional):
   - Restricts search to a subset of files (faster + more accurate).
   - Example: 窶徙nly folders under {dir}/SE/窶・or a column containing relative paths.
   - If specified, pre-filter candidate list by:
     - scope regex applied to `{path}` or to a chosen column.

Header Row Detection (Heuristic)
--------------------------------
When header_row is auto:
- Scan first N rows (default N=30).
- Score each row by:
  - number of non-empty cells
  - uniqueness ratio (strings vs numbers)
  - low numeric dominance (header rows are usually text)
- Pick the highest scoring row as header.
Fallback:
- If score below threshold, treat as 窶徂eaderless窶・

Drag & Drop Behavior
--------------------
- Dropping `.csv`/`.xlsx` opens the import dialog with the file preloaded.
- Dropping audio still behaves as before (add files).
- If user drops multiple CSV/Excel files, open a chooser for which to import.

UI Details
----------
Import dialog sections:
1) Source:
   - File path
   - Sheet dropdown (Excel)
2) Table layout:
   - Header row (auto/manual)
   - Data starts at (auto/manual)
   - 窶廩as header窶・checkbox
3) Keying:
   - Primary regex
   - Secondary scope (optional)
   - Column selection checkboxes
   - Live test preview (shows match/miss on selected row)
4) Preview:
   - First 50 rows with column headers
   - Matched file path preview column
5) Apply:
   - Import external columns
   - Show summary toast

Implementation Plan
-------------------
Phase 1: Core parsing
- Add Excel dependency: `calamine` (read-only; no write needed).
- CSV loader: current pipeline + delimiter auto-detect.
- Add a unified 窶徼able窶・struct: rows of strings, column labels.

Phase 2: Import dialog
- New dialog state in app:
  - source file, sheet, header/data rows, regex, scope, column selection.
- Row preview and test-match section.

Phase 3: Mapping
- Extend external mapping to use key regex + scope filter.
- Mark missing rows visually in list (same system as current external).

Phase 4: Drag & drop
- When drop is CSV/Excel, open import dialog instead of file add.
- Keep audio drag/drop unchanged.

Notes / Non窶賎oals
-----------------
- Excel/CSV editing or export is out of scope.
- We do not write back to CSV/Excel.
- Large tables should parse in background threads.

Open Questions
--------------
1) Do we allow multiple external sources at once (stacked columns)?
2) Preferred default header detection threshold?
3) Should sheet selection be remembered per file (session setting)?

Performance / Memory Review (Critical)
--------------------------------------
Current external mapping (existing code) reads CSV into `Vec<Vec<String>>`,
builds `external_lookup: HashMap<String, HashMap<String, String>>`, and
re-applies mapping across all list items. This is OK for small files but will
grow quickly in both time and memory.

Key risks to address:
1) **O(N * M) matching**: mapping iterates every item and builds keys on the fly.
   - For 300k items, repeated regex/string allocations will be costly.
2) **Lookup cloning**: `apply_external_mapping()` currently clones the lookup map.
   - This doubles memory and costs time on each apply.
3) **Row storage**: storing every cell as `String` duplicates repeated values.
4) **Excel loading**: naive calamine usage may load the entire sheet into RAM.
5) **UI blocking**: parsing on the UI thread will stall when large sheets or
   header detection scans too much.

Performance窶詮irst Adjustments (Implementation Notes)
----------------------------------------------------
1) **Background parse + progress**  
   - CSV/Excel parsing should run in a worker thread with progress updates.
   - The UI should show a non-blocking loader + row count as it streams.

2) **Avoid lookup cloning**  
   - Use `Arc<HashMap<..>>` or keep lookup in `self` and borrow immutably.
   - Apply mapping without cloning the map.

3) **Precomputed path keys**  
   - Cache `{path, file, stem, dir, ext}` lowercased per `MediaItem`.
   - For regex mode, compile regex once and reuse.

4) **Columnar / interned storage**  
   - Use string interning (`Arc<str>` or `SmolStr`) for headers and values.
   - Optionally store only key + visible columns for very large tables (memory cap).

5) **Streaming Excel**  
   - Read only the selected sheet.
   - Avoid materializing all cells when only a subset of columns is needed.

6) **Header detection sampling**  
   - Scan only first N rows (default 30窶・00).
   - Skip full-sheet scans; provide manual override if detection is wrong.

7) **Secondary scope filtering early**  
   - Apply folder/path filter *before* regex matching to reduce candidates.
   - If scope is a path prefix, use prefix match instead of regex.

8) **Duplicate key handling**  
   - Store `key -> Vec<Row>` (or first + count) and report collisions in UI.
   - Let user choose 窶彷irst wins / last wins / show duplicates窶・

Validation Checklist
--------------------
- 300k list items + 100k CSV rows: import completes without UI freeze.
- Memory footprint stays below a defined cap (e.g. < 500 MB for large imports).
- Regex changes do not reparse the source file; only remap from cached rows.
- Sheet switching does not keep old sheets in memory.

