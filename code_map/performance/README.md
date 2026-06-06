# Performance Reading Map

UI / Audio 高速化を調査するときの読み順と、巨大ファイルの扱いです。

## Reading Order

1. `docs/UI_PERFORMANCE_IMPROVEMENT_CANDIDATES_20260606.md` で候補と優先度を確認する。
2. `src/app/frame_ops.rs` と `src/app/logic.rs` で frame 内に残っている仕事を確認する。
3. list 系なら `src/app/ui/list.rs`, `src/app/ui/list/table.rs`, `src/app/search_ops.rs`, `src/app/list_state_ops.rs`, `src/app/meta_ops.rs` を読む。
4. list preview / MP3 系なら `src/app/list_preview_ops.rs`, `src/audio_io.rs`, `src/audio.rs`, `src/app/preview_ops.rs` を読む。
5. editor 系なら `src/app/ui/editor.rs`, `src/app/editor_decode_ops.rs`, `src/app/editor_viewport.rs`, `src/app/spectrogram_jobs.rs`, `src/app/editor_ops.rs` を読む。
6. loading / progress / cancel 表示なら `src/app/loading_ops.rs`, `src/app/ui/topbar/status.rs`, `src/app/frame_ops.rs` を読む。
7. bulk resample の frame 分割なら `src/app/resample_ops.rs` と `src/app/loading_ops.rs` を読む。
8. job 分離方針を見るなら `src/app/threading.rs`, `src/app/types.rs`, 各 `*_ops.rs` の `std::thread::spawn` / channel / generation check を横断する。

## Large Files And Split Status

| パス | 状態 | 扱い方 |
|---|---|---|
| `src/app/ui/editor.rs` | 最大の UI 面。canvas / timeline / tool panel / selection / waveform / spectrogram が同居 | 挙動を壊しやすいので、小さく読み、責務単位で段階分割する |
| `src/app/effect_graph_ops.rs` | effect graph runtime が大きいが cohesive | validation / runner / drain など自然な境界で分ける |
| `src/app/cli_ops.rs` | headless CLI 実処理が集約 | command group 単位で追う。GUI state との共有点に注意 |
| `src/app/logic.rs` | per-frame logic と job drain が多い | UI frame 内コスト、drain 順序、cancel/generation を見る入口 |
| `src/audio_io.rs` | decode/export と progressive 処理が大きい | format ごとの処理差、progress callback、`max_secs` を確認する |
| `src/app/types.rs` | state 型が集中 | 保存互換、UI 表示、job state の波及を確認する |
| `src/app/ui/effect_graph.rs` | graph UI が大きい | ops 側と UI 側の責務混在に注意 |
| `src/wave.rs` | WAV / waveform utility が大きい | `audio_io.rs` との重複や責務境界を見ながら変更する |

## UI / Audio Principles

- list は大量件数での応答性が最優先。
- editor は遅くても progress / cancel / placeholder を出し、UI thread を止めない。
- 圧縮音源や長尺音声は progressive decode、prefix 再生、stale cancel、full handoff をセットで確認する。
- 重い job は topbar status、Debug window、progress 表示まで接続して UX の納得感を出す。
