# Performance + Scalability Plan (v1)

Goal
- Keep list operations fast at 300k files.
- Make long audio (3 hours) and many channels (30ch) feel responsive.
- Provide visible progress, partial results, and cancel for slow tasks.
- Avoid "app looks frozen" during background work.

Non-goals (for now)
- Full real-time preview of extremely heavy transforms in the list view.
- Perfect frame-accurate progress for every decoder backend (approx OK).

Problem Summary
- Long audio or high channel count makes pitch/time edits and editor preview slow.
- After recent loading changes, preview audio and green overlay may not appear quickly.
- On large folder scans, the UI can appear idle until the first items appear.
- Selecting a long file can block tab transition while decode happens; should switch instantly and show loading in the editor.

Principles
- List must stay fast; do minimal work per row.
- Editor can be slow, but must show progress and partial results.
- Always show "something is happening" while background tasks run.
- Provide cancel for long tasks.

Key Workstreams

1) Unified background job system with progress + cancel
- Introduce a lightweight job manager:
  - JobId, JobKind, JobState { started_at, progress (0..1), message, cancel_flag }.
  - Each heavy task registers a job and updates progress periodically.
  - UI shows active jobs in header + editor panel.
- Cancel:
  - For long tasks (spectrogram, pitch/time, full decode) expose a cancel button.
  - Cancels should stop background work and clear "processing" overlays.

2) Progressive decoding for long audio
- Use staged loading:
  - Stage 1: quick header/meta (duration, channels, SR).
  - Stage 2: downsampled waveform preview (fast coarse thumbnail).
  - Stage 3: full decode if needed (editor only, not list).
- Editor should show:
  - Coarse waveform immediately (gray).
  - Green preview overlay once processing finishes.
  - Progress bar + ETA while full decode or transform runs.

3) Spectrogram tiling + incremental render
- Compute spectrogram in chunks (time tiles).
- Show partial tiles as they arrive.
- Cache tiles by (path, channel, settings, time range).
- Allow cancel while computing.

4) Multi-channel strategy (30ch)
- Default view:
  - Mixdown for overview + per-channel toggle to view subsets.
- Avoid full multi-channel spectrogram by default:
  - Render mixed or selected channels first.
  - Allow "render all channels" with progress + cancel.
- Memory cap:
  - Keep only necessary channel buffers in memory for preview.
  - Evict or compress older channel data when idle.

5) Pitch/Time processing strategy
- List view (initial thresholds):
  - duration > 10 minutes OR channels > 8:
    - disable list-level pitch/time preview and show tooltip "Use editor".
  - duration > 60 minutes OR channels > 16:
    - disable any list-level heavy preview and force editor-only workflow.
- Editor:
  - Always allow, but show progress and partial UI.
  - Prefer chunked processing with periodic UI updates.
  - If processing exceeds a threshold, show "Continue / Cancel" prompt.

6) Large list UX improvements (300k files)
- Show "Scanning..." status with counts and elapsed time.
- Insert initial results early (already scanning) but add a visible indicator:
  - "Indexing... 12,345 / 300,000"
- Avoid sorting during scan; defer sort until scan ends or user requests.
- Display CPU-friendly skeleton rows while scan is active.

7) Input responsiveness + UI feedback
- Ensure UI remains interactive:
  - Heavy jobs run in background threads with limited priority.
  - No blocking on main thread for long tasks.
- Add small "work in progress" chip in top bar with job count.
- In editor, show:
  - Progress bar and message.
  - Partial waveform/preview overlay if available.

Metrics + Instrumentation
- Track timing for:
  - decode header
  - waveform preview
  - full decode
  - pitch/time apply
  - spectrogram tile
- Log to Debug Window (and optionally file) per job.
- Add a "Performance" section in Debug:
  - active jobs, last job durations, bytes decoded.

Phased Implementation Plan

Phase 1: Job manager + visible progress (no algorithm changes)
- Add JobManager with progress + cancel flags.
- Wire existing heavy tasks to publish progress messages.
- UI: top bar job chip + editor progress panel.
- Debug: show active jobs and last durations.

Phase 2: Progressive waveform + long-audio UX
- Coarse waveform render first, then refined.
- Enter/tab transition must be immediate; editor shows "Loading..." with progress and partial preview once available.
- Green overlay only after transform finishes, but show "processing" state.
- Add cancel for long pitch/time tasks in editor.

Phase 3: Spectrogram tiling + incremental display
- Compute tiles in background, show partial results.
- Allow cancel and caching.

Phase 4: Multi-channel scaling
- Mixed view default + channel subsets.
- Cache limits and eviction policy.

Phase 5: Large list scan UX polish
- Scan progress UI + deferred sorting.
- Skeleton rows while scanning.

Acceptance Criteria
- List remains responsive with 300k files (scroll and selection are smooth).
- Opening 3h or 30ch audio shows immediate UI feedback and partial data.
- Any long operation has a clear progress indicator and cancel.
- No "silent" hangs where nothing updates for > 1s.
