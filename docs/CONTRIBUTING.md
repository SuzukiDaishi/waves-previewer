# 貢献ガイド

## セットアップ / ビルド

- Rust stable を使用。
- 実行中の `NeoWaves.exe` があると Windows では再ビルドに失敗する（OS エラー 5）。再ビルド前にアプリを終了してください。
- 依存：`egui`, `eframe`, `egui_extras`, `cpal`, `hound`, `walkdir`, `arc-swap` など。

```
cargo build
cargo run
```

## コーディング指針

- 音声コールバックはリアルタイム安全（RT-safe）に：
  - 動的確保やミューテックス取得は禁止。
  - 短い計算（ゲイン、複製、RMS）に限定。
- 共有状態は `SharedAudio` に集約し、`Atomic*` と `ArcSwapOption` を使用。
- UI は 16ms 間隔を目安に `request_repaint_after` を利用。
- 大量リストは仮想化で：`egui_extras::TableBody::rows` を用いて可視行のみ描画し、`TableBuilder::sense(Sense::click())` で行全体のクリックを受け取る。`row.response()` でクリック/ダブルクリック判定を行う。
- スクロール領域は `min_scrolled_height(...)` で残り高さまで埋め、行が少ない場合はフィラー行で見た目の枠を下端まで延ばす。

## スタイル

- `rustfmt` を既定に。
- `cargo clippy -- -D warnings` を無警告に。
- コミットは Conventional Commits 推奨（例：`feat: add loop playback`、`fix: smooth playhead`）。

## 変更の進め方

- 小さな PR を歓迎します。再現手順と確認観点を明記してください。
- UI 変更はスクリーンショットがあると助かります。
- 新規機能は `docs/` に仕様メモを追加してください。
