# Native drag & the Windows shell path limit (2026-08-18)

Origin: crash report `crash_20260818_104119_17300` (v0.20260802.0, Windows, GUI).

## Summary

Dragging an item out of NeoWaves panicked inside the `drag` crate whenever Windows
could not express the file's path in the legacy form its shell requires. The panic
was caught, so the app kept running, but the drag failed and a crash report was
written for a failure that had already been handled.

## Root cause

`drag::start_drag` normalizes every path with `dunce::canonicalize` and hands the
result to `ILCreateFromPathW`:

```
drag-2.1.1/src/platform_impl/windows/mod.rs
  235:  paths.push(dunce::canonicalize(f)?);
  370:  let shell_item_array = get_shell_item_array(paths).unwrap();   // <- panic, column 60
  375:  fn get_file_item_id(path: &Path) -> *mut ITEMIDLIST { ILCreateFromPathW(..) }
```

`ILCreateFromPathW` cannot parse the `\\?\` verbatim prefix, and `dunce` keeps that
prefix whenever dropping it would change which file the path refers to
(`dunce-1.0.5/src/lib.rs:100,157,178`):

| Case | Why `dunce` keeps `\\?\` |
|---|---|
| Path longer than 260 characters | Past `MAX_PATH`, the legacy form cannot address it |
| UNC network share (`\\server\share\...`) | Verbatim UNC is not a plain path |
| Reserved names, trailing dots/spaces | The legacy APIs reject the file name |

So `ILCreateFromPathW` returns NULL, `SHCreateShellItemArrayFromIDLists` fails,
`get_shell_item_array` returns `None`, and `drag` unwraps it.

`drag` 2.1.1 is the latest release and no version fixes this, so NeoWaves works
around it.

## Fix

`src/app/native_drag.rs`

- `shell_compatible_drag_paths` runs the same normalization `drag` will run and
  checks the result with `is_verbatim_path`. A path the shell accepts is passed
  through untouched.
- A path that stays verbatim is copied into the drag temp directory under a short
  name and the copy is dragged instead. The copy is registered in
  `external_drag_temp_files`, so the existing 10-minute retention sweep removes it.
  The copy keeps the source extension, because the receiving application picks its
  handler from it.
- `start_native_file_drag_guarded` holds a `crash_report::suppress_panic_reports()`
  guard, so a panic it catches no longer produces a crash report, and the panic's
  message is carried into the status line and the debug log.

`src/crash_report.rs`

- Panic locations keep their crate-relative path when the path carries no user data
  (cargo registry, git checkouts, the standard library, the project's own `src/`).
  Anonymizing `.../drag-2.1.1/src/platform_impl/windows/mod.rs` down to `mod.rs`
  is what made this report take a tarball download and a column count to place.

## Trade-off

For a file on a slow network share, the copy runs synchronously on the UI thread, so
the drag does not begin until it finishes. The alternative was the previous
behaviour, where the drag never worked at all for those files. If the wait becomes a
problem, the next step is a size threshold that refuses the drag with an explanatory
status instead of copying.

## Coverage

Automated, runs everywhere (`src/app/native_drag.rs` tests):

| Test | Covers |
|---|---|
| `verbatim_paths_are_recognized_as_shell_hostile` | `\\?\` and `\\?\UNC\` are flagged; plain UNC and ordinary paths are not |
| `shell_hostile_paths_are_dragged_as_a_short_temp_copy` | The copy fallback end to end, with the normalization stubbed to return what Windows would. Checks the handed-over path is not verbatim, the copy exists with identical bytes, the extension survives, and the copy is registered for cleanup |
| `shell_compatible_paths_are_passed_through_without_a_copy` | An acceptable path costs no copy |
| `shell_compatible_paths_leave_ordinary_files_untouched` | Same, through the real normalization |
| `drag_temp_paths_keep_the_source_extension` | Non-wav sources keep their extension |
| `external_drag_guard_converts_native_panic_to_error` | The panic message reaches the caller |
| `external_drag_guard_keeps_the_panic_out_of_crash_reports` | Reporting is suppressed while the panic unwinds |

Automated, Windows only (`#[cfg(windows)]`, so they run on a Windows build):

| Test | Covers |
|---|---|
| `windows_long_paths_fall_back_to_a_copy` | Scenario 1 — a real path past `MAX_PATH` is copied |
| `windows_short_paths_are_dragged_in_place` | Scenario 3 — a real short path is not copied |

The long-path test is not vacuous: with its `cfg` flipped it fails on Linux, because
no verbatim path is produced there.

Still manual — needs a real network share:

1. Put a wav on a UNC share (`\\server\share\...`), open it in NeoWaves, and drag it
   to Explorer. It should drop successfully, no crash report should appear in
   `%APPDATA%\NeoWaves\crash-reports`, and the debug log should record
   `external drag: copied <name> to a short temp path`.
2. Repeat with the share disconnected mid-drag; the status line should read
   `Drag failed: <name>: ...` rather than the app dying.
