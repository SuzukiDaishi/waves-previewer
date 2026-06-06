# src/app/ops

このフォルダは実在の Rust フォルダではなく、`src/app/*_ops.rs` と関連 state/data module を探索するためのコードマップです。
UI からの入力を受けて state を更新する処理、worker 起動、channel result drain、I/O 起動の入口をここにまとめます。

## Operation And Data Modules

| 領域 | 主なパス | 役割 |
|---|---|---|
| Input / Clipboard | `src/app/input_ops.rs`, `src/app/clipboard_ops.rs` | global shortcut、focus、copy/paste、clipboard temp audio |
| Playback / Processing | `src/app/audio_ops.rs`, `src/app/loading_ops.rs` | output volume、playback FX result、processing result、busy overlay |
| List state | `src/app/list_state_ops.rs`, `src/app/list_ops.rs`, `src/app/list_undo.rs` | list item lookup、selection、row/path index、undo |
| Scan / Search | `src/app/scan_ops.rs`, `src/app/search_ops.rs` | folder scan、dummy list、filter、sort、検索反映 |
| Metadata | `src/app/meta.rs`, `src/app/meta_ops.rs`, `src/app/gain_ops.rs`, `src/app/loudnorm_ops.rs` | duration/sample rate/artwork/transcript/LUFS/gain の非同期取得 |
| List preview | `src/app/list_preview_ops.rs`, `src/app/preview.rs`, `src/app/preview_ops.rs` | list 選択時の再生、重い preview / overlay job drain |
| Editor decode | `src/app/editor_decode_ops.rs`, `src/app/editor_viewport.rs`, `src/app/editor_features.rs` | progressive decode、viewport worker、feature cache |
| Editor apply | `src/app/editor_ops.rs`, `src/app/tool_ops.rs`, `src/app/tooling.rs`, `src/app/temp_audio_ops.rs` | destructive apply、preview、virtual audio、tool 実行 |
| Rename / Resample | `src/app/rename_ops.rs`, `src/app/resample_ops.rs` | file rename、batch rename、sample-rate override、bulk resample chunking |
| Spectrogram | `src/app/spectrogram.rs`, `src/app/spectrogram_jobs.rs`, `src/app/render/spectrogram.rs` | spectrogram 計算、tile job、描画 |
| Session | `src/app/project.rs`, `src/app/session_ops.rs` | `.nwsess` 保存・復元。`project*` 命名は legacy |
| Export | `src/app/export_ops.rs`, `src/audio_io.rs` | save selected、gain export、file export |
| External data | `src/app/external.rs`, `src/app/external_ops.rs`, `src/app/external_load_jobs.rs`, `src/app/external_load_ops.rs` | CSV/Excel import、merge、非同期 load |
| Plugin bridge | `src/app/plugin_ops.rs`, `src/plugin/*` | plugin scan、session draft、preview/apply、worker protocol |
| Transcript AI | `src/app/transcript.rs`, `src/app/transcript_ops.rs`, `src/app/transcript_ai_ops.rs`, `src/app/transcript_onnx.rs`, `src/app/hf_cache.rs` | transcript model、generation、seek、SRT export、HF cache 解決 |
| Music AI | `src/app/music_ai_ops.rs`, `src/app/music_onnx.rs`, `src/app/hf_cache.rs`, `src/app/render/music_features.rs` | music analysis、markers、stems、feature render、HF cache 解決 |
| Zoo | `src/app/zoo_ops.rs`, `src/app/zoo_assets.rs`, `src/app/ui/zoo.rs` | Zoo overlay、GIF decode、embedded voice、texture cache |
| CLI workspace | `src/app/cli_ops.rs`, `src/app/cli_workspace.rs` | headless session/list/editor/render/export command 実装 |

## Worker / Drain Reading Hints

- UI が止まる場合は `frame_ops.rs` と `logic.rs` で drain 順序と frame 内処理量を見る。
- 古い job の結果が反映される場合は generation / stale check / cancel flag を探す。
- `std::thread::spawn`、`mpsc`、progress message が機能別に散っているため、共通化するときは `types.rs` の job state も確認する。
