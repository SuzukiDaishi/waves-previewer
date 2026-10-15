# Auto-run scripts

The GUI's debug automation runs a list of steps one after another, a few
frames apart, and can take screenshots along the way. Besides the built-in
sweeps (`--auto-run`, `--auto-run-editor`, `--auto-run-pitch-shift`,
`--auto-run-time-stretch`), the steps can be written out as a script. The
script can open a tool, put any setting into it, preview it, apply it and
take screenshots, all without a build that hard-codes the values.

```powershell
# a file of commands
neowaves --open-file .\demo.wav --auto-run-script .\shots.txt

# the same commands inline, separated by ';'
neowaves --open-file .\demo.wav --auto-run-steps "open-first; tool limiter; set limiter.input_gain_db 9; preview; wait 40; screenshot"
```

When both flags are given, the file's commands run first. The run quits at
the end unless `--auto-run-no-exit` is passed. `--auto-run-delay <frames>`
(default 8) is the gap before every command.

The script is checked before the window opens, and a line it cannot
understand stops the process with its line number. That covers an unknown
command, an unknown tool, a misspelt setting and a value of the wrong type.
The parser is `src/app/debug_script.rs`.

## Commands

One per line. `#` starts a comment.

| Command | What it does |
|---|---|
| `open-first` | Open the list's first file in the editor. |
| `select-next` | Select the next row of the list. |
| `tool <name>` | Switch the editor tool the way the Inspector's palette does. The name is a tool's name or its label, in any case: `EQ`, `compressor`, `noise-gate`, `Pitch Shift`. |
| `set <key> <value>` | Change one tool setting, as a drag on it would. The auto preview follows. |
| `preview` | Render the preview now (the footer's Preview button). |
| `apply` | The Inspector's Apply. |
| `undo` / `redo` | Ctrl+Z / Ctrl+Y. The adjustments are undone before committed edits, as in the editor. |
| `selection <start> <end>` | Select a span, as fractions of the file (`0.1 0.4`). |
| `clear-selection` | Drop the selection. |
| `seek <fraction>` | Move the playhead. |
| `view <mode>` | One of `wave`, `spec`, `log`, `mel`, `tempogram`, `chromagram`, `world`. |
| `play` | Start or stop playback. |
| `wait <frames>` | Wait this much longer before the next command. |
| `screenshot [path]` | Save a screenshot. Without a path it goes to the OS screenshots folder. |
| `summary [path]` | Save the debug summary. |
| `exit` | Quit. |

## Settings (`set`)

A key names a field of the session's tool settings (`ProjectToolState` in
`src/app/project.rs`), so every setting of every tool can be reached:

- Plain fields keep their own names: `gain_db`, `normalize_target_db`,
  `pitch_semitones`, `stretch_rate`, `fade_in_ms`, `denoise_reduction_db`,
  `trim_mode`, and so on.
- These prefixes stand for the settings the session stores as structs:
  - `eq.` is the parametric EQ (`crate::eq::ParametricEqParams`):
    `eq.output_db`, and `eq.bands.<n>.<field>` with bands numbered 0-7.
    A band's fields are `enabled`, `bypassed`, `kind`, `freq_hz`, `gain_db`,
    `q`, `slope`, `mode` and `cut_q` (a low or high cut's resonance, 0.707
    flat).
  - `compressor.` is the compressor (`crate::dynamics::CompressorSettings`):
    `threshold_db`, `ratio`, `knee_db`, `attack_ms`, `release_ms`,
    `lookahead_ms`, `makeup_db`, `auto_makeup`, `mix_pct`, `range_db`,
    `detection`, `rms_ms`, `link_pct`, `sidechain_hpf_hz`.
  - `limiter.` is the limiter: `input_gain_db`, `ceiling_db`, `release_ms`,
    `lookahead_ms`, `true_peak`, `link`.
  - `gate.` is the noise gate: `threshold_db`, `hysteresis_db`, `range_db`,
    `attack_ms`, `hold_ms`, `release_ms`, `lookahead_ms`, `detection`,
    `rms_ms`, `link_pct`, `key_hpf_hz`, `key_lpf_hz`.
- A value is read as JSON if it is valid JSON (`-30`, `true`, `null`, or a
  whole object), and as a word otherwise. Words are how the choices are
  written:
  - `kind`: `bell`, `low_shelf`, `high_shelf`, `low_cut`, `high_cut`, `notch`
  - `slope`: `db12` ... `db48`
  - `mode`: `stereo`, `mid`, `side`
  - `detection`: `peak`, `rms`
- A whole band can be set at once. The fields left out take their defaults:
  `set eq.bands.3 {"enabled":true,"kind":"low_cut","freq_hz":80}`
- An unknown key is refused, and the error lists the keys that exist there.
  So is a value of the wrong kind (a word for a number) and an old session
  field the structs override (`compressor_ratio`: write `compressor.ratio`).

## Example: the EQ and dynamics tools

This is what `--auto-run-editor` runs for its EQ and dynamics screenshots:

```text
open-first
wait 30
tool eq
set eq.bands.0.gain_db -4
set eq.bands.1 {"enabled":true,"freq_hz":2500,"gain_db":5,"q":1.4}
set eq.bands.3 {"enabled":true,"kind":"low_cut","freq_hz":60}
set eq.bands.4 {"enabled":true,"freq_hz":400,"gain_db":-3,"mode":"side"}
preview
wait 40
screenshot
tool compressor
set compressor.threshold_db -30
set compressor.ratio 4
preview
wait 40
screenshot
tool limiter
set limiter.input_gain_db 9
preview
wait 40
screenshot
apply
wait 20
screenshot
undo
```
