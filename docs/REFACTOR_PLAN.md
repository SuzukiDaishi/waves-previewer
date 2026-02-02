# Refactor Plan (NeoWaves)

この計画は **現状の挙動を壊さずに** `app.rs` / `logic.rs` の肥大化を解消し、
保守性・テスト性・デバッグ容易性を上げることを目的とします。
既存のモジュール構成（`src/app/*`）を尊重し、**「impl WavesPreviewer の機能分割」**を軸に進めます。

※ 表記方針: 本書では「Session」を正式名称として扱います。コード上の `project*` 命名は legacy で、
`.nwsess` のセッション保存・復元を指します。

---

## 0. 前提 / ゴール

### ゴール
- `src/app.rs` の責務を **「フレーム更新の配線 + 全体構成」**に限定する
- UI / ロジック / 重処理 / I/O を **機能単位に分割**し、変更時の影響範囲を小さくする
- Hotkey / Focus / Clipboard / Session のような“バグ要因が集中しやすい領域”を **専用モジュール化**
- 既存の高速性（リスト描画、遅延読み込み、非同期処理）を維持

### 守るべき原則
- **UIスレッドで重い処理を実行しない**
- **エディタは非破壊が基本**（Apply → Save）
- **一覧の高速性は最優先**
- **Hotkeyの判定は一箇所に集約**

---

## 1. 現状構成（要約）

`src/app/`
- `types.rs` : 中核型 / 状態
- `logic.rs` : UIロジック・更新系
- `ui/*` : Topbar / List / Editor / Debug などの描画
- `render/*` : 波形 / スペクトラム系描画
- `editor_ops.rs` : エディタ適用系
- `list_ops.rs` / `list_undo.rs` : リスト操作 / Undo
- `tool_ops.rs` : Tool 系
- `project.rs` : セッション（`.nwsess`）保存・復元（legacy 命名）
- `external.rs` / `external_ops.rs` : CSV/Excel 読み込み
- `preview.rs` : プレビュー音声生成
- `debug_ops.rs` : Debug操作

ただし `src/app.rs` に **“実体ロジック” が多く残っている**。

---

## 2. リファクタリング対象（優先度順）

### 優先度A（バグの温床）
- Hotkey / Focus / Clipboard
- Session (nwsess) / File association
- Loading / Cancel / Progress

### 優先度B（肥大化領域）
- editor / list の大きな処理ブロック
- 非同期の処理フロー（スキャン / resample / loudnorm）

### 優先度C（構造整理）
- render / preview の責務整理
- types / state の局所化

---

## 3. 分割方針

### 3.1 “impl WavesPreviewer” の機能別分割
Rustは `impl WavesPreviewer` を複数ファイルで分割可能。
**新しいモジュールファイルを追加し、関数を移動**することで `app.rs` を薄くする。

**追加予定のファイル例**
```
src/app/
  input_ops.rs      // hotkeys / focus / suppress 系
  clipboard_ops.rs  // copy/paste 入口 + debug
  session_ops.rs    // nwsess open/save/drag/drop
  loading_ops.rs    // overlay / cancel / progress
  audio_ops.rs      // list->play, editor->play, resample, loudnorm
```

※ 既存の `editor_ops.rs` / `list_ops.rs` / `tool_ops.rs` と役割が競合しないよう整理。

---

### 3.2 UI と操作ロジックの境界を明確化

- **UI描画** は `ui/*` に集約
- **状態更新（操作）** は `*_ops.rs` に集約

例:
- `ui/list.rs` でクリックを検知 → `list_ops.rs` の操作関数を呼ぶ
- `ui/editor.rs` で Apply を検知 → `editor_ops.rs` を呼ぶ

---

## 4. 詳細ステップ（破壊しない進め方）

### Phase 0: 調査と棚卸し
- `app.rs` 内の関数を「UI/操作/非同期/ユーティリティ」に分類
- 依存関係グラフを作成（簡易でOK）
- 重要関数の一覧（Hotkey / Clipboard / Session / Loading）を作る

成果物:
- `docs/REFACTOR_PLAN.md` に関数マップ（簡易表）

### 関数マップ（実施済み）

| 移動先 | 関数 |
|---|---|
| `src/app/input_ops.rs` | `list_focus_id`, `search_box_id`, `request_list_focus`, `handle_global_shortcuts`, `handle_undo_redo_hotkeys` |
| `src/app/clipboard_ops.rs` | `set_clipboard_files*`, `set_clipboard_files_with_marker`, `set_clipboard_marker_text`, `get_clipboard_files`, `copy_selected_to_clipboard`, `paste_clipboard_to_list`, `handle_clipboard_hotkeys` |
| `src/app/editor_ops.rs` | `spawn_editor_apply_for_tab`, `drain_editor_apply_jobs` |
| `src/app/kittest_ops.rs` | `test_*` helpers (non-apply) |
| `src/app/meta_ops.rs` | `reset_meta_pool`, `ensure_meta_pool`, `queue_meta_for_path`, `queue_full_meta_for_path`, `queue_transcript_for_path`, `drain_meta_updates` |
| `src/app/loudnorm_ops.rs` | `schedule_lufs_for_path`, `drain_lufs_recalc_results`, `pump_lufs_recalc_worker` |
| `src/app/resample_ops.rs` | `open_resample_dialog`, `apply_resample_dialog`, `tick_bulk_resample`, `refresh_audio_after_sample_rate_change` |
| `src/app/session_ops.rs` | `is_session_path`, `process_ipc_requests`, `handle_dropped_files`, `queue_project_open`, `tick_project_open`, `save_project`, `save_project_as`, `open_project_file` |
| `src/app/theme_ops.rs` | `theme_visuals`, `apply_theme_visuals`, `set_theme`, `init_egui_style`, `ensure_theme_visuals`, `prefs_path`, `normalize_spectro_cfg`, `apply_spectro_config`, `load_prefs`, `save_prefs` |
| `src/app/loading_ops.rs` | `tick_processing_state`, `ui_busy_overlay` |
| `src/app/export_ops.rs` | `spawn_export_gains`, `spawn_save_selected`, `drain_export_results` |
| `src/app/external_load_ops.rs` | `drain_external_load_results` |
| `src/app/external_load_jobs.rs` | `begin_external_load` |
| `src/app/list_preview_ops.rs` | `spawn_list_preview_full`, `drain_list_preview_results` |
| `src/app/spectrogram_jobs.rs` | `ensure_spectro_channel`, `spawn_spectrogram_job`, `queue_spectrogram_for_tab` |
| `src/app/preview_ops.rs` | `drain_heavy_preview_results`, `drain_heavy_overlay_results` |
| `src/app/search_ops.rs` | `schedule_search_refresh`, `apply_search_if_due` |
| `src/app/scan_ops.rs` | `start_scan_folder`, `append_scanned_paths`, `process_scan_messages` |
| `src/app/transcript_ops.rs` | `request_transcript_seek`, `apply_pending_transcript_seek` |
| `src/app/mcp_ops.rs` | `process_mcp_commands`, `mcp_list_files` |
| `src/app/gain_ops.rs` | `pending_gain_db_for_path`, `set_pending_gain_db_for_path`, `has_pending_gain`, `pending_gain_count` |
| `src/app/list_state_ops.rs` | `is_dotfile_path`, `is_decode_failed_path`, `item_for_id*`, `item_for_row`, `item_for_path*`, `is_virtual_path`, `meta_for_path`, `effective_sample_rate_for_path`, `set_meta_for_path`, `clear_meta_for_path`, `transcript_for_path`, `set_transcript_for_path`, `clear_transcript_for_path`, `display_name_for_path`, `display_folder_for_path`, `rebuild_item_indexes`, `path_for_row`, `row_for_path`, `selected_path_buf`, `selected_paths`, `selected_real_paths`, `selected_item_ids`, `ensure_sort_key_visible`, `request_list_autoplay`, `current_active_path` |
| `src/app/temp_audio_ops.rs` | `clear_clipboard_temp_files`, `export_audio_to_temp_wav`, `edited_audio_for_path`, `decode_audio_for_virtual` |
| `src/app/rename_ops.rs` | `open_rename_dialog`, `open_batch_rename_dialog`, `replace_path_in_state`, `rename_file_path`, `batch_rename_paths` |
| `src/app/audio_ops.rs` | `apply_effective_volume` |

※ `set_clipboard_files` は OS ごとの `cfg` 実装あり。
※ `save_project` / `open_project_file` などの関数名は legacy だが、意味はセッション保存・復元。

---

### Phase 1: Hotkey / Focus / Clipboard の分離（最重要）

**移動先候補:**
- `input_ops.rs`: `handle_shortcuts`, `handle_list_keys`, `handle_undo_redo_hotkeys`
- `clipboard_ops.rs`: `handle_clipboard_hotkeys`, `copy_selected_to_clipboard`, `paste_clipboard_to_list`

**具体作業**
1. `src/app/input_ops.rs` 新規作成
2. `src/app/clipboard_ops.rs` 新規作成
3. `app.rs` から関数を移動
4. `app.rs` は `self.handle_shortcuts(ctx)` などのみ呼ぶ

**テスト観点**
- Ctrl+C/V でイベントが拾える
- Search focus でも Enter/Arrow の挙動が壊れない

---

### Phase 2: Session / File association を分離

**移動先候補:**
- `session_ops.rs`

対象:
- nwsess の open/save / drag & drop
- CLI 引数 / IPC からの open

**具体作業**
1. `src/app/session_ops.rs` 作成
2. `queue_project_open`, `save_project`, `open_project_file` など（セッション open/save）を移動
3. 呼び出しを `app.rs` で一本化

---

### Phase 3: Loading / Cancel の分離

**移動先候補:**
- `loading_ops.rs`

対象:
- `processing` 状態の切り替え
- overlay 表示・キャンセル処理

**具体作業**
1. `processing` 管理ロジックを `loading_ops.rs` に移動
2. UI側 (`ui/overlay.rs`) は状態参照だけにする

---

### Phase 4: エディタ適用処理の整理

- `editor_ops.rs` / `tool_ops.rs` に残っている Apply 系を再分類
- Preview/Apply/Save の3段階パイプラインを統一

成果:
- Apply系の実装箇所が 1〜2 ファイルに集約

---

### Phase 5: 非同期ジョブの責務分離

対象:
- resample / loudnorm / meta / spectrogram

方針:
- **job生成** と **結果反映**を明確化
- 結果反映は `*_ops.rs` に集約

---

### Phase 6: app.rs 最終整理

- `app.rs` は「フレーム更新」と「UI呼び出し配線」だけにする
- 内部関数は全て `impl WavesPreviewer` in `src/app/*` に移動

#### 残りの分割候補（app.rs からの移動）

| 移動先候補 | 関数 |
|---|---|
| `render` / `spectrogram` | `draw_spectrogram` |
| `editor_ops.rs`（もしくは `editor_state_ops.rs`） | `mixdown_channels`, `editor_mixdown_mono`, `effective_loop_xfade_samples`, `apply_loop_mode_for_tab`, `set_marker_sample`, `update_loop_markers_dirty`, `next_marker_label`, `time_stretch_ratio_for_tab`, `map_audio_to_display_sample`, `map_display_to_audio_sample`, `estimate_state_bytes`, `capture_undo_state`, `push_state_to_stack`, `pop_state_from_stack`, `push_editor_undo_state`, `push_undo_state_from`, `push_redo_state`, `restore_state_in_tab`, `undo_in_tab`, `redo_in_tab` |
| `list_ops.rs`（もしくは `list_state_ops.rs`） | `clear_all_pending_gains_with_undo`, `make_media_item`, `build_meta_from_audio`, `make_virtual_item`, `add_virtual_item`, `unique_virtual_display_name` |
| `export_ops.rs` / `csv_export_ops.rs` | `export_list_csv`, `csv_meta_ready`, `begin_export_list_csv`, `update_csv_export_progress_for_path`, `check_csv_export_completion`, `trigger_save_selected`, `clamp_gain_db`, `adjust_gain_for_indices` |
| `mcp_ops.rs` | `handle_mcp_command`, `setup_mcp_server`, `start_mcp_from_ui`, `start_mcp_http_from_ui` |
| `startup` / `app_factory` | `build_app` |

---

## 5. 具体的な関数移動案（例）

| 現在 | 移動先 | 理由 |
|------|--------|------|
| `handle_clipboard_hotkeys` | `clipboard_ops.rs` | Clipboard専用 | 
| `copy_selected_to_clipboard` | `clipboard_ops.rs` | Clipboard専用 | 
| `paste_clipboard_to_list` | `clipboard_ops.rs` | Clipboard専用 | 
| `handle_undo_redo_hotkeys` | `input_ops.rs` | Hotkey系 |
| `handle_list_keys` | `input_ops.rs` | Hotkey系 |
| `save_project` / `open_project_file` | `session_ops.rs` | Session系 |
| `queue_project_open` | `session_ops.rs` | Session系 |
| `begin_processing` / `end_processing` | `loading_ops.rs` | Loading系 |

---

## 6. 依存/注意点

- **外部依存**: `signalsmith-stretch`, `symphonia`, `cpal`, `egui`
- **Hotkeyはイベント順序に依存**するため、移動後も呼び出し順を維持すること
- `ctx.input_mut(|i| ...)` を呼ぶ関数は **UI描画後**に実行する

---

## 7. 完了条件

- `src/app.rs` が 1,000 行以内程度まで縮小
- Hotkey/Clipboard/Session/Loading が各モジュールに分離されている
- リスト/エディタの主要機能が維持されている
- `cargo build` / `cargo test` が通る

---

## 8. 追加（将来オプション）

- `Model` / `DerivedCache` を明示して MVU 風に整理
- `Msg` / `Cmd` の導入（大規模化した場合）

---

## 9. 実行順序まとめ（安全順）

1. Hotkey / Clipboard 分離
2. Session / File association 分離
3. Loading / Cancel 分離
4. エディタApply系整理
5. 重処理ジョブの整理
6. app.rs の最終薄型化

---

この計画に沿って段階的に進めれば、現在の機能を壊さずに、
長期的な保守性と拡張性を確保できます。
