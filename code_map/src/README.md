# src

`src/` は crate の公開境界、GUI/CLI 起動、音声 I/O、plugin、marker などの top-level 実装を持ちます。
GUI の詳細は [app](app/README.md)、plugin worker の詳細は [plugin](plugin/README.md) を参照します。

## Top-Level Crate Map

| パス | 主な責務 | まず見る場面 |
|---|---|---|
| `src/main.rs` | ネイティブ起動の入口 | GUI 起動前後の流れを確認する |
| `src/lib.rs` | crate 公開モジュールの束ね | integration test や CLI から見える境界を確認する |
| `src/cli.rs` | CLI 引数定義、GUI legacy flags、headless command tree | CLI 仕様、`--dummy-list`、render/export コマンドを追う |
| `src/app.rs` | `WavesPreviewer` state、app module 宣言、`eframe::App` 実装 | app 全体 state と frame entrypoint を確認する |
| `src/audio.rs` | 再生エンジン、出力デバイス、playback buffer 操作 | 音が鳴らない、停止しない、音量・rate がおかしい |
| `src/audio_io.rs` | WAV/MP3/M4A/OGG などの decode/export、progressive decode | 圧縮音源、長尺 decode、export を追う |
| `src/wave.rs` | WAV 系ユーティリティ、min/max 波形、sample 変換 | 波形表示、WAV 読み込み、ピーク計算を見る |
| `src/markers.rs` / `src/loop_markers.rs` | marker / loop marker の読み書き | loop tag、marker 保存互換を確認する |
| `src/ipc.rs` | 多重起動や外部要求の IPC message | 既存プロセスへの open 要求を追う |
| `src/kittest.rs` | GUI test helper | kittest や自動 UI 検証を追う |
| `src/bin/*` | plugin worker / debug utility binaries | worker binary や debug MP3 生成を追う |
| `src/bin/debug_generate_long_mp3.rs` | 長尺 MP3 debug fixture 生成 utility | MP3 decode / preview の負荷検証素材を作る |

## High-Level Flow

```text
src/main.rs
  -> src/cli.rs
  -> src/app.rs
  -> src/app/app_init.rs
  -> src/app/frame_ops.rs
  -> src/app/logic.rs
  -> src/app/ui/*
  -> src/app/*_ops.rs
```

## Next

- GUI app state と frame 処理: [app](app/README.md)
- VST3 / CLAP backend: [plugin](plugin/README.md)
- 主要フロー: [flows](../flows/README.md)
