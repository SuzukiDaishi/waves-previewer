# src/app/ui

`src/app/ui/` は egui の描画面です。UI はここで入力やクリックを検知し、実体操作は `src/app/*_ops.rs` に委譲するのが基本方針です。

## UI Map

| パス | 主な責務 | 変更時の注意 |
|---|---|---|
| `src/app/ui/topbar.rs` | top bar 全体の入口 | menus / transport / status へ委譲する |
| `src/app/ui/topbar/menus.rs` | menu items、tools、settings 導線 | 新規機能の UI 導線を追加する候補 |
| `src/app/ui/topbar/transport.rs` | 再生・停止・rate・音量など transport UI | `src/audio.rs` / `audio_ops.rs` と合わせて見る |
| `src/app/ui/topbar/status.rs` | job status、processing 表示 | loading/progress 表示を追加する入口 |
| `src/app/ui/list.rs` | list panel orchestration | 大量件数での frame cost に注意 |
| `src/app/ui/list/table.rs` | TableBuilder、行描画、列定義 | 仮想化されるが可視行の clone / stat / thumb 描画に注意 |
| `src/app/ui/list/navigation.rs` | list focus、keyboard navigation | hotkey/focus 変更時は `input_ops.rs` も見る |
| `src/app/ui/list/badges.rs` | list row badge 表示 | metadata 表示の見た目を変える |
| `src/app/ui/list/art.rs` | artwork / thumbnail 系 UI | 遅延読み込みや cache 表示に関係 |
| `src/app/ui/list/row_menu.rs` | row context menu | list item 操作の導線 |
| `src/app/ui/editor.rs` | editor canvas、timeline、tool panel、selection、wave/spec UI | 最大級の UI 面。挙動安定を優先して段階分割中 |
| `src/app/ui/effect_graph.rs` | effect graph canvas / inspector UI | `effect_graph_ops.rs` と対で見る |
| `src/app/ui/debug.rs` | Debug window UI | frame ms、job 状態、input 状態の確認導線 |
| `src/app/ui/export_settings.rs` | export / settings UI | export_ops、theme_ops、audio device 設定に波及 |
| `src/app/ui/external.rs` | CSV/Excel external data UI | external 系 ops と合わせて見る |
| `src/app/ui/transcript.rs` | transcript 表示・操作 | transcript ops / AI 生成と合わせて見る |
| `src/app/ui/transcription_settings.rs` | transcription 設定 UI | model download / config 保存に関係 |
| `src/app/ui/tools.rs` | tool dialogs / tool UI | editor tools や batch 操作の導線 |
| `src/app/ui/zoo.rs` | Zoo menu、editor overlay、touch voice | `zoo_ops.rs` / `zoo_assets.rs` と合わせて見る |

## Dialogs From Frame Ops

| パス | 主な責務 | 変更時の注意 |
|---|---|---|
| `src/app/frame_ops.rs` | rename / batch rename / resample dialog の frame 内描画 | UI はここ、実処理は `rename_ops.rs` / `resample_ops.rs` |

## UI Performance Hints

- list は大量件数での応答性が最優先。可視行以外の work を frame 内に増やさない。
- editor は遅くても progress / cancel / placeholder を出し、UI thread を止めない。
- topbar status は重い job の納得感を出す入口なので、background 化した処理は表示も接続する。
