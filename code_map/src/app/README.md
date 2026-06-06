# src/app

`src/app/` は `WavesPreviewer` の状態、1 frame ごとの orchestration、UI、操作ロジック、background job drain をまとめる中心領域です。
`src/app.rs` は state と shell を持ち、実体ロジックは `src/app/*` に分散しています。

## App Core Map

| パス | 主な責務 | 備考 |
|---|---|---|
| `src/app.rs` | `WavesPreviewer` の巨大 state と `update()` shell | 機能別実装は `src/app/*` に分散。state 追加時は影響が大きい |
| `src/app/types.rs` | 共有型、UI state、job state、editor/list/session state | 変更時は保存互換や UI 表示に波及しやすい |
| `src/app/app_init.rs` | `WavesPreviewer` の初期構築 | 起動時 default、prefs、CLI 起動条件を見る |
| `src/app/startup.rs` | 起動時補助処理 | startup-only の条件分岐を追う |
| `src/app/frame_ops.rs` | 1 frame 内の更新順序と UI 呼び出し | UI が止まる、drain 順序がおかしい場合の入口 |
| `src/app/logic.rs` | per-frame logic、検索・ソート適用、各種 job drain | 大きいファイル。UI frame 内処理と状態遷移の主要入口 |
| `src/app/threading.rs` | thread helper / background 処理補助 | worker 起動方針や thread 名を揃える候補 |
| `src/app/helpers.rs` | 小さな共通 helper / 定数 | UI/ops 間で共通化したい小物を見る |
| `src/app/dialogs.rs` | dialog state / dialog rendering helper | 確認 UI や modal 表示の入口 |
| `src/app/debug_ops.rs` | debug summary、debug state、diagnostics | Debug window や自動検証ログを追う |
| `src/app/capture.rs` | screenshot / capture 補助 | GUI screenshot automation を追う |
| `src/app/theme_ops.rs` | theme、prefs load/save、spectrogram config | 見た目やユーザー設定の永続化を見る |
| `src/app/loading_ops.rs` | processing result drain、playback FX result drain、busy overlay | progress / cancel / UI blocking 表示を追う |
| `src/app/hf_cache.rs` | Hugging Face Hub cache 探索 | transcript / music AI model のローカル解決を見る |
| `src/app/zoo_ops.rs` / `src/app/zoo_assets.rs` | Zoo overlay の asset decode、texture、voice 再生 | Zoo UI と埋め込み asset を追う |
| `src/app/assets/effect_graph_test_sample.wav` | effect graph test / preview 用の小さな WAV fixture | effect graph の headless test や sample 実行を追う |

## Sub Maps

- [ops](ops/README.md): operation modules と background job。
- [ui](ui/README.md): egui の描画面。
- [render](render/README.md): 波形・スペクトラム・feature 描画。

## Split Status

| パス | 状態 | 扱い方 |
|---|---|---|
| `src/app/ui/editor.rs` | 最大の UI 面。canvas / timeline / tool panel / selection / waveform / spectrogram が同居 | 挙動を壊しやすいので、小さく読み、責務単位で段階分割する |
| `src/app/effect_graph_ops.rs` | effect graph runtime が大きいが cohesive | validation / runner / drain など自然な境界で分ける |
| `src/app/cli_ops.rs` | headless CLI 実処理が集約 | command group 単位で追う。GUI state との共有点に注意 |
| `src/app/logic.rs` | per-frame logic と job drain が多い | UI frame 内コスト、drain 順序、cancel/generation を見る入口 |
| `src/app/types.rs` | state 型が集中 | 保存互換、UI 表示、job state の波及を確認する |
