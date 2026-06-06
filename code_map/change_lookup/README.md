# Change Lookup

変更したい内容から、最初に読むファイルを逆引きする表です。

| やりたい変更 | まず見るパス | 併せて見るパス |
|---|---|---|
| リスト表示を高速化したい | `src/app/ui/list/table.rs`, `src/app/ui/list.rs` | `src/app/list_state_ops.rs`, `src/app/search_ops.rs`, `src/app/meta_ops.rs` |
| 大量件数の検索・ソートを改善したい | `src/app/search_ops.rs`, `src/app/logic.rs` | `src/app/list_state_ops.rs`, `src/app/ui/list/table.rs` |
| MP3/M4A/OGG のリスト再生を安定化したい | `src/app/list_preview_ops.rs`, `src/audio_io.rs` | `src/audio.rs`, `src/app/preview_ops.rs`, `src/app/logic.rs` |
| playback device / 音量 / rate を直したい | `src/audio.rs`, `src/app/audio_ops.rs` | `src/app/ui/topbar/transport.rs`, `src/app/list_preview_ops.rs`, `src/app/editor_ops.rs` |
| エディタ初回表示を軽くしたい | `src/app/editor_decode_ops.rs`, `src/app/ui/editor.rs` | `src/app/editor_viewport.rs`, `src/app/spectrogram_jobs.rs`, `src/app/render/waveform_pyramid.rs` |
| Spectrogram / Mel / feature 描画を改善したい | `src/app/spectrogram_jobs.rs`, `src/app/editor_viewport.rs` | `src/app/spectrogram.rs`, `src/app/render/spectrogram.rs`, `src/app/render/music_features.rs` |
| editor tool preview/apply を直したい | `src/app/editor_ops.rs`, `src/app/tool_ops.rs` | `src/app/preview.rs`, `src/app/preview_ops.rs`, `src/app/temp_audio_ops.rs`, `src/app/types.rs` |
| Undo / 非破壊編集を改善したい | `src/app/editor_ops.rs`, `src/app/types.rs` | `src/app/temp_audio_ops.rs`, `src/app/project.rs` |
| rename / batch rename を直したい | `src/app/rename_ops.rs`, `src/app/frame_ops.rs` | `src/app/ui/topbar/menus.rs`, `src/app/ui/list/row_menu.rs`, `src/app/list_state_ops.rs` |
| sample rate override / bulk resample を直したい | `src/app/resample_ops.rs`, `src/app/frame_ops.rs` | `src/app/ui/list/row_menu.rs`, `src/app/loading_ops.rs`, `src/app/ui/topbar/status.rs` |
| loading / progress / busy overlay を直したい | `src/app/loading_ops.rs`, `src/app/ui/topbar/status.rs` | `src/app/frame_ops.rs`, `src/app/types.rs` |
| Session 保存形式を変えたい | `src/app/project.rs`, `src/app/session_ops.rs` | `src/app/types.rs`, `src/app/cli_ops.rs` |
| CSV/Excel import を直したい | `src/app/external_load_jobs.rs`, `src/app/external_ops.rs` | `src/app/external.rs`, `src/app/external_load_ops.rs`, `src/app/ui/external.rs` |
| plugin scan / preview / apply を直したい | `src/app/plugin_ops.rs`, `src/plugin/*` | `src/app/effect_graph_ops.rs`, `src/app/cli_ops.rs` |
| effect graph を直したい | `src/app/effect_graph_ops.rs`, `src/app/ui/effect_graph.rs` | `src/app/plugin_ops.rs`, `src/app/cli_ops.rs` |
| transcript 生成や SRT を直したい | `src/app/transcript_ai_ops.rs`, `src/app/transcript_ops.rs` | `src/app/transcript_onnx.rs`, `src/app/ui/transcript.rs`, `src/app/cli_ops.rs` |
| music analysis / marker 生成を直したい | `src/app/music_ai_ops.rs`, `src/app/music_onnx.rs` | `src/app/render/music_features.rs`, `src/app/cli_ops.rs` |
| HF model cache 解決を直したい | `src/app/hf_cache.rs` | `src/app/transcript_ai_ops.rs`, `src/app/music_onnx.rs` |
| Zoo overlay / voice を直したい | `src/app/ui/zoo.rs`, `src/app/zoo_ops.rs` | `src/app/zoo_assets.rs`, `src/app/frame_ops.rs` |
| Debug window の情報を増やしたい | `src/app/debug_ops.rs`, `src/app/ui/debug.rs` | `src/app/frame_ops.rs`, `src/app/logic.rs`, `src/app/types.rs` |
| CLI command を増やしたい | `src/cli.rs`, `src/app/cli_ops.rs` | `src/app/cli_workspace.rs`, 対象機能の `*_ops.rs` |
| GUI screenshot / kittest を調整したい | `src/kittest.rs`, `src/app/kittest_ops.rs` | `src/app/capture.rs`, `src/app/debug_ops.rs`, `src/app/ui/*` |
