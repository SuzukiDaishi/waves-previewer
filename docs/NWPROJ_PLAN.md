# NeoWaves Session Save (.nwsess) Spec and Plan

## Goals
- File > Session Save / Open support.
- Save editor state (open tabs, edits-in-progress, selections, tool params, view state).
- Restore the session from a single `.nwsess` file.
- If a source file is missing, show an error on that item/tab.
- Session is optional: default workflow remains opening audio files directly.

## Non-Goals (for first release)
- Cloud sync.
- Versioned history of edits inside a session.

Sessions on a shared file server are supported -- see **Shared sessions**
below. That section supersedes the original "no network" non-goal.

---

## File Format (v1)
Use TOML (already in the repo) for a human-readable session file.

File: `MySession.nwsess`

```toml
version = 1
name = "My Session"
created_at = "2025-02-01T12:34:56Z"
path_mode = "absolute"
base_dir = "C:\\Audio\\Samples"
open_first = true

[app]
theme = "dark"
list_sort_key = "File"
list_sort_dir = "None"
search_query = ""
search_regex = false
list_columns = { file = true, folder = true, transcript = false, external = false, length = true, ch = true, sr = true, bits = true, peak = true, lufs = true, gain = true, wave = true }

[spectrogram]
fft_size = 2048
window = "blackman_harris"
overlap = 0.875
max_frames = 4096
scale = "log"
mel_scale = "linear"
db_floor = -120.0
max_freq_hz = 0.0
show_note_labels = false

[[tabs]]
path = "voice\\line_001.wav"
missing = false
active_tool = "PitchShift"
view_mode = "Spectrogram"
show_waveform_overlay = false
channel_view = { mode = "mixdown", selected = [] }
tool_state = { fade_in_ms = 0.0, fade_out_ms = 0.0, gain_db = 0.0, normalize_target_db = -6.0, pitch_semitones = 3.0, stretch_rate = 1.0 }
loop = { mode = "Off", region = [0, 0], xfade_samples = 0, xfade_shape = "EqualPower" }
trim_range = [0, 0]
selection = [0, 0]
markers = [{ sample = 1234, label = "M01" }]
dirty = true
edited_audio = "data/tab_0001.wav"

[[tabs]]
path = "missing\\file.wav"
missing = true
error = "Source file missing"
```

### Notes
- `path_mode`: source/user paths use one policy for the entire session:
  `absolute` or `relative`. Per-file mixing is not written. New sessions
  default to `absolute`, except a new session saved onto a network share,
  which defaults to `relative` so it resolves from any machine. A relative
  session that cannot represent every source relative to the `.nwsess`
  location is promoted as a whole to `absolute`.
- `base_dir`: the `.nwsess` parent at the last save. In absolute mode it is also
  the relocation root used to derive a fallback path after the session moves.
- Display and runtime paths are absolute regardless of `path_mode`.
- `edited_audio`: optional sidecar file for edited waveform (see below).
- `missing`: set on load, not necessarily written on save (runtime check).

---

## Edited Waveform Storage
Current edits are destructive to in-memory samples. To restore "edited waveform" we must persist it.

### v1 approach
- Create a sidecar folder next to the session file:
  - `MySession.nwsess.d/`
- Save edited audio per tab as a 32-bit float WAV matching the sample rate
  and channel count. The name is a hash of the audio's contents --
  `data/<sha16>.wav` -- so concurrent writers of a shared session cannot
  collide; see **Shared sessions** below. Sessions written before that change
  reference `data/tab_0001.wav` and still resolve.
- `edited_audio` in the session file points to this path.
- If a tab is not dirty, omit `edited_audio` to avoid bloat.

### Rationale
- Fast restore and exact waveform reproduction.
- Avoids re-running heavy edits (pitch/time-stretch).

---

## Missing Source Files
On load:
- For an absolute source that no longer exists, derive its old `base_dir`
  relative suffix and try the same suffix from the current `.nwsess` parent.
- A successful fallback becomes the runtime absolute path. It is **not**
  written back while opening -- on a shared file server that made every
  reader a writer -- so the repair stays in memory and is persisted by the
  next explicit save.
- If the fallback still misses and the stored path names a network location,
  try the same share written the other way round (mapped drive letter vs.
  UNC). See **Shared sessions** below.
- Resolve every entry independently but keep the serialization policy at
  session scope. A few unresolved files never abort restoration of the other
  list rows, overrides, external sources, cached edits, or tabs.
- If `tabs.path` cannot be found, create a placeholder item:
  - Show in list with "[Missing]" prefix and a warning color.
  - In editor, display a "Source file missing" banner.
- If `edited_audio` exists, allow editor to open the edited audio as a virtual track.
- If both source and edited audio are missing, keep the placeholder and show error only.

---

## UI/UX
Add to File menu:
- Session Save...
- Session Save As...
- Session Open...
- Session Close (clears session and returns to list view)

Behavior:
- Session Save defaults to last session path if already opened.
- Save As chooses path and writes sidecar folder if needed.
- On opening a session, restore the list, tabs, and editor state. Show a one-line toast "Session loaded".

---

## Data to Capture
- List state: root folder, sort key/dir, search query/regex, list columns.
- Open tabs: order, active tab, view mode, channel view, tool state, selection, markers, loop, trim, fade.
- Global editor options: spectrogram config.
- Dirty flag per tab + edited audio sidecar when dirty.

---

## Implementation Plan

### Phase 1: Data model + serialization
- Add `SessionFile` structs in `src/app/types.rs` (serde-friendly).
- Implement `read_session(path)` and `write_session(path)` in `src/app/project.rs` (module name is legacy).
- Use TOML for v1 serialization.

### Phase 2: Save / Open wiring
- File menu actions:
  - `Session Save` / `Save As` / `Open`.
- Save:
  - Gather state, serialize to `.nwsess`.
  - Write edited audio sidecars for dirty tabs.
- Open:
  - Clear current session safely.
  - Load list, tabs, and editor state.
  - Missing files create placeholders + warnings.

### Phase 3: Missing file UX + fallback
- Add list-level "missing source" badge.
- Editor banner with clear error message.
- Allow editing if `edited_audio` is present; otherwise read-only placeholder.

---

## Shared sessions (a `.nwsess` on a file server)

More than one process writes a session on a share: two GUI instances, or a
GUI and a `--cli` batch. There is **no lock file** -- no advisory lock, no
checkout, no heartbeat. Coordination is optimistic instead: remember exactly
what was read, and refuse to commit over a document that changed underneath.

### Version stamp

Four optional top-level fields, all `#[serde(default)]`, so older sessions
load unchanged and older builds ignore them:

```toml
version = 2
session_id = "9f2c1b..."            # document lineage; a Save As forks it
revision = 43                       # incremented on every successful save
saved_at = "2026-08-31T14:32:00Z"   # RFC3339, UTC
saved_by = "tanaka"                 # display_name= from prefs, else user@host
```

`saved_by` is written into a file the team already shares with each other,
and goes nowhere else. Set `display_name=` in `%APPDATA%/NeoWaves/prefs.txt`
to control it.

### Conflict detection

The comparison key is a **SHA-256 of the file's bytes**, not the mtime and
not `revision`. A share's mtime comes from the server's clock, at the
server's resolution, through the client's attribute cache -- two machines
cannot agree on it. And `revision` is advisory: an older build or an outside
tool can write the file without touching it. The bytes are the only thing
every writer agrees on. `revision` and `saved_by` exist to *describe* a
conflict once the hash has detected it.

A save:

1. reads the file and compares hashes **before** encoding sidecars, so an
   already-doomed save fails fast rather than spending seconds first;
2. stages the sidecars;
3. reads and compares again immediately before the document commit -- this
   is the check that decides;
4. commits, or returns a conflict having written nothing.

On conflict the GUI shows a modal: **Save As... / Overwrite / Reload
(discard my changes) / Cancel**. Overwrite keeps the document it replaces as
`<name>.nwsess.bak`. The CLI exits non-zero with the same information;
`--force` overwrites (also leaving a `.bak`).

**Known limit.** A few milliseconds separate the final hash read from the
rename. Two saves landing inside that window can both pass. Closing it needs
a lock, which this design deliberately does not have. Every other realistic
concurrent save is caught.

### External-change notice

`src/app/session_watch.rs` polls the open session (a `stat`, re-reading the
body only when size or mtime moved) and reports when the document on disk
stops matching the one in memory. Polling, like `watch.rs`, and for the same
reason: uniform behavior on a network drive.

It **only reports**. Reloading is a deliberate action -- an automatic reload
would discard unsaved edits. The notice is a toast *plus* a standing amber
indicator in the topbar (`⟳ changed on disk`), because a toast expires long
before the user comes back to the window. `File > Reload Session from Disk...`
does the same thing.

Interval comes from `PerfProfile::session_watch_interval_ms()` (5 s local,
20 s remote) and is stretched by `watch::next_walk_delay` in proportion to
what a pass actually cost. The probe suspends while this process is opening
or saving, so it never reports our own write back to us.

### Reads never write

Opening a session used to rewrite it whenever the path repair resolved
something. On a share that made every reader a writer, racing the people
actually saving. The repair now stays in memory and is persisted by the next
explicit save. Same in the CLI's `load_session`.

### Sidecar naming

Session-owned audio is named by a hash of its contents:

- `<name>.nwsess.d/data/<sha16>.wav` (was `data/tab_0000.wav`)
- `<name>.nwsess.d/assets/<id>/<revision>-<sha16>.wav` (was `<revision>.wav`)

The old names were index- and counter-based, and both the index and the
asset id live *in the shared session*, so two people writing the same
session wrote different audio to the same filename -- destroying each
other's takes even when the document-level check later refused their
document. Content addressing removes the collision at the source and
deduplicates identical audio for free.

Sessions written by older builds still reference the old names and keep
opening: nothing resolves a sidecar except through the string stored in the
document. They migrate on their next save.

### Durability

- Every write is `tmp` + atomic replace (`MoveFileExW` with
  `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` on Windows, `rename`
  elsewhere), including the CLI's, which used to be a bare `fs::write`.
- Transient share failures -- sharing violations from a virus scanner or
  another client, an SMB session reconnecting -- are retried three times
  (after 100 ms, 300 ms, 900 ms) rather than losing the save.
- A zero-length session is reported as
  "a previous save may have been interrupted", naming the `.bak` if one is
  there, instead of an opaque TOML parse error.
- A committed save sweeps `*.stage` / `*.tmp` leftovers older than 24 h.

### Paths across machines

A session **saved to a share for the first time** defaults to
`path_mode = "relative"`, because colleagues mount the same share
differently (`Z:\Proj` here, `\\server\share\Proj` there) and absolute
paths would resolve only for whoever saved it. Existing sessions keep the
policy they were written with. For those, a missing absolute path also gets
a UNC-to-drive-letter fallback (`WNetGetConnectionW`) as the last step of
the repair chain.

### Known limitations

- **Orphaned `data/*.wav` accumulate.** With content addressing, audio no
  longer referenced by *this* document may still be referenced by a
  colleague's newer one, and nothing here can tell the two apart. Only
  unambiguous staging garbage is swept.
- **Personal state lives in the shared document.** `app.theme`,
  `app.selected_path`, `app.list_columns_window_pos`, `active_tab` and
  `search_query` are per-user but stored in the session, so a colleague's
  save changes them for everyone on the next reload. Annoying, not
  corrupting; out of scope so far.
- **No merging.** Conflicts are resolved by choosing a whole document:
  Save As, Overwrite, or Reload.

---

## Changed since *you* last opened it

The conflict detection above watches the `.nwsess`. It cannot see the thing
that actually goes wrong most often on a share: somebody replaces a wav the
session points at. That touches no byte of the document.

So the app keeps its own record of what every referenced file looked like the
last time **this person** opened **this session**, and diffs against it on the
next open.

### Where the record lives, and why not in the session

In a per-user SQLite database beside the metadata cache
(`.../neowaves/session-state-v1.sqlite3`, override with
`NEOWAVES_SESSION_STATE`), never in the `.nwsess`.

Two reasons, and the second is the load-bearing one:

1. "Changed since **you** last opened it" is per-person. A shared document has
   nowhere to put a different answer for each colleague.
2. Recording a baseline on open would make **every reader a writer again** --
   exactly the failure the section above exists to remove. A hundred thousand
   file hashes would also add megabytes to a document that is parsed on every
   open.

The database is a cache, not user data. Losing it costs one silent
re-baseline and nothing else.

Sessions are keyed by their `session_id`, so the same session opened as
`Z:\proj\a.nwsess` one day and `\\server\share\proj\a.nwsess` the next is
recognised as the same session. Documents older than that field fall back to
their path.

### Two tiers

A session here can reference a hundred thousand files on a network share.
Hashing all of them on every open would cost more than the work the user came
to do, so:

1. **stat everything** for `(size, mtime)`. One syscall each, and it settles
   the overwhelming majority: nothing moved, nothing to do.
2. **hash only the files whose stat moved**, with
   `session_sync::hash_file_content`. The cost is proportional to what
   actually changed, and it is what separates a real edit from a file that was
   merely copied back or touched.

| tier 1 | baseline | reported |
|---|---|---|
| gone | present | **Removed** |
| present | absent | **Added** |
| `(size, mtime)` match | present | nothing -- no hash taken |
| moved, hash matches | hashed | **nothing** (touched, not changed) |
| moved, hash differs | hashed | **Changed** |
| moved | never hashed | **Changed**, conservatively |

`recorded_at` on each row is when the change was noticed, and it is what the
list shows as "detected".

**A baseline row is only advanced when the new content is actually known.**
That rule matters more than it looks:

- Tier 1 said nothing moved → the stored hash still describes these bytes, so
  it is carried forward along with its original detection time. Overwriting it
  with "no hash" would leave tier 2 nothing to compare against on the next
  real change, and the whole distinction between *touched* and *changed*
  would quietly decay into a false alarm.
- We meant to hash and could not (unreadable file, a share that dropped) →
  the row is left exactly as it was. Advancing it would trade a known-good
  hash for nothing, and since the stored stat would then match the file,
  nothing would ever hash it again.

**The first open of a session reports nothing.** There is no previous visit to
compare against, and announcing every file as new would be noise. It records a
baseline and stays quiet.

### Hashes held in advance

A file with no stored hash cannot be compared exactly, so a background pass
hashes never-hashed files at the lowest priority, capped per open. Because the
result is persisted, it resumes across runs and the baseline converges toward
exact comparisons everywhere.

### Changes while the session is open

The folder watch already knows when a listed file's bytes change. Those paths
are re-probed and re-recorded immediately -- including files this app wrote
itself -- so a change the user watched happen is not announced back to them on
the next open as though it were a colleague's.

### What it looks like

A toast once, plus a standing amber `⚠ N source files changed` in the topbar,
for the same reason the session badge is standing: a toast is gone in seconds
and the user may be away. Clicking it opens the list -- file, kind, size,
detected -- with `File > Changed Since Last Open...` as the other way in.
Clicking a row selects it in the list; **Dismiss** clears the indicator.
Nothing here reloads anything on its own.

### Session history

Every save that replaces an existing document stores the replaced bytes, which
the compare-and-swap has already read -- so the only added cost is one write.
`File > Session History...` lists what is stored: revision, who saved it, when,
size.

- **Restore** writes that version back over the session. The document it
  replaces is stored on the way past, so restoring is itself undoable.
- **Save As...** writes the version elsewhere and leaves the session alone.

History is **per user**: 20 versions per session under a global byte cap, in
the same local database. A colleague's saves are not in your history, and the
shared-side insurance stays the single `.nwsess.bak`.

### Known limitations

- **A change that alters neither size nor mtime is invisible.** Tier 1 never
  fires, so tier 2 never runs. That is the direct cost of not hashing
  everything on every open.
- **The first change to a never-hashed file is reported even if the bytes are
  identical.** There is nothing to compare against, and staying silent would be
  the wrong way to be wrong. The hash taken then makes every later comparison
  exact.
- **A file the session stops referencing is dropped from the baseline
  silently.** It is a change to the session, not to the file.
- **A file that could not be read is reported as changed.** There is no way
  to prove otherwise, and its baseline row is left alone so the next scan
  re-decides.
- Personal state in the shared document (theme, selection, active tab) is
  unchanged by any of this -- see the limitation above.

---

## Follow-ups (v2)
- Optional "Save edits" checkbox to avoid sidecar audio.
- Portable sessions (path remapping dialog).
- Compact storage using FLAC or OGG for edited audio.
