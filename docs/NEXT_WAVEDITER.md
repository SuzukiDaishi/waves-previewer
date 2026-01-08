# 次世代 Audio Editorの開発について

本ドキュメントは、waves-previewer を「次世代 Audio Editor」へ拡張するための
機能設計と実装計画をまとめたものです。要求が増えても破綻しない設計を前提に、
MCP/LLM連携や外部ツール統合まで視野に入れます。

---

## 目的と到達点

- 大量の音声素材を「検索・比較・一括処理」できる編集環境に拡張する
- 文字起こしや外部データ(CSV/Excel)を参照して編集対象を精密に絞り込む
- MCP経由でLLMと連携し、柔軟な操作・分析・自動化を可能にする

---

## ffmpeg/sox との連携（外部コマンド統合）

### 目標
- コマンドパレットから一括処理を実行できる
- 事前定義したワークフロー（変換/計測/文字起こし）を簡単に呼び出せる

### 仕様案
- コマンド登録は `tools.toml` などの設定ファイルで管理
- 変数展開をサポート
  - `{path}` / `{dir}` / `{stem}` / `{ext}` / `{outdir}` / `{basename}`
- 実行結果ログを保存し、成功/失敗を一覧で確認可能にする
- 危険なコマンド（上書きや削除）には確認ダイアログを挟む

### 将来的な拡張
- コマンドの「前処理/後処理」をチェーンできる
- 実行キューを持ち、並列数や優先度を指定できる

---

## SRT（文字起こし）ファイルの参照

### 目標
- 音声と同名の `.srt` を自動検出
- リストに表示し、検索対象にする

### 仕様案
- 例: `xxx.wav` と同じフォルダに `xxx.srt` がある場合に紐付け
- 文字起こしは段落単位で表示し、検索結果でハイライト可能にする
- リストビューに「Transcript」列（省略表示 + ホバー全文）を追加

### 実装候補
- SRTパーサ（例: `subtitles` crate など）
- 内部型: `TranscriptSegment { start_ms, end_ms, text }`
- 検索インデックス:
  - ファイル単位（全文検索）
  - セグメント単位（時間一致検索）

---

## CSV / Excel の参照

### 目標
- 外部データの列をリスト表示に追加して検索・ソートできる

### 仕様案
- CSV/Excel を読み込み、主キー列を選択
- 主キーと音声ファイル名（または別カラム）をマッピング
- マッピングがない場合は「未一致」として一覧に表示

### 実装候補
- CSV: `csv` crate
- Excel: `calamine` crate
- 内部型: `ExternalRow { key: String, columns: HashMap<String, String> }`
- UI:
  - 「データソース管理」画面
  - 主キー選択 / 列表示 ON/OFF
  - マッピングルール（ファイル名一致、正規表現置換など）

---

## MCP 経由の LLM 連携

### 目標
- アプリを MCP サーバー化し、LLM から操作・情報取得を可能にする
- 画面状態や音声情報を渡して高度な判断を支援できる

### 機能の一例
- 一通りの操作機能（選択/再生/編集/保存/書き出し）
- エディタ以上の検索（自然言語・ルールベース）
- 音声参照（波形情報、簡易メタ、エクスポート結果）
- スクリーンショットで画面把握

---

## アーキテクチャ方針

### 1) データモデル統一
- `MediaItem` にメタ/文字起こし/外部データを統合
- 例:
  - `path`
  - `meta`
  - `transcript`
  - `external_rows`
  - `status`（DecodeErrorなど）

### 2) 非同期パイプライン
- Scan → Header → Decode → Feature（RMS/LUFS/波形）
- Transcript/CSV/Excel も別レーンで読み込み
- すべて `MetaPool` 互換のワーカーキューで統一

### 3) UIの拡張性
- 列定義をデータ駆動にし、外部列追加も可能にする
- フィルタは「検索語 + 条件式」に拡張

### 4) 永続ストレージ
- prefs: UI設定/ツール設定
- cache: 解析結果/メタ/索引
- format version を持ち、破壊的変更に対応

---

## 実装計画（段階的）

### Phase 0: 基盤整理
- `MediaItem` 導入とメタ周りの統一
- エラーフラグ（DecodeErrorなど）を全UIに反映
- 既存処理の依存関係整理

### Phase 1: コマンドパレット / 外部ツール
- `tools.toml` 読み込み
- コマンド実行キュー + ログ
- UI: コマンドパレット + 実行履歴ビュー

### Phase 2: SRT統合
- SRT 読み込み/インデックス
- リスト表示/検索連携
- セグメント表示とハイライト

### Phase 3: CSV/Excel統合
- CSV/Excel 読み込み
- 主キー選択UI
- 列追加・検索・ソート

### Phase 4: MCP/LLM連携
- MCP サーバー化
- API 定義と認可
- スクリーンショット/音声メタ取得

---

## リスク/注意点

- 外部コマンド実行の安全性（削除/上書き）
- 文字起こしや外部データの更新時に再スキャン負荷が高い
- MCP経由操作は誤操作防止のため安全確認が必要

---

## 次アクション（直近）

- Phase 0 を具体化（構造体整理と責務分離）
- `tools.toml` の仕様草案を作成
- SRT/CSV/Excel の最小実装プロトタイプを作成

---

## MCP実装計画（関数レベル）

本セクションでは MCP 実装を関数レベルで設計します。
stdio/SSE どちらにも対応可能な設計を前提とします。

### 1) モジュール構成（案）

```
src/
  mcp/
    mod.rs
    server.rs           # MCP サーバー起動・transport選択
    state.rs            # 共有状態・権限・許可リスト
    bridge.rs           # UI側への命令キューと応答ルーティング
    tools/
      mod.rs
      list.rs           # list系
      playback.rs       # play/stop/volume/mode
      edit.rs           # gain/loop/markers
      export.rs         # export/overwrite
      filesystem.rs     # open-folder/open-files
      debug.rs          # screenshot/log/summary
    resources/
      mod.rs
      read.rs           # resources/read 実装
      list.rs           # resources/list 実装
    prompts/
      mod.rs
      templates.rs      # prompt定義
```

### 2) 共有状態

```
struct McpState {
    allow_paths: Vec<PathBuf>,
    allow_write: bool,
    allow_export: bool,
    read_only: bool,
    last_screenshot: Option<PathBuf>,
}
```

### 3) UI ブリッジ

```
enum UiCommand {
    ListFiles(ListFilesArgs),
    GetFileMeta(PathBuf),
    SelectFile(PathBuf),
    Play,
    Stop,
    SetMode(ModeArgs),
    ApplyGain(ApplyGainArgs),
    SetLoopMarkers(LoopMarkersArgs),
    Export(ExportArgs),
    Screenshot(ScreenshotArgs),
}

struct UiCommandResult {
    ok: bool,
    payload: serde_json::Value,
    error: Option<String>,
}
```

### 4) Tools（関数レベル）

```
fn tool_list_files(state: &McpState, args: ListFilesArgs) -> Result<ListFilesResult>;
fn tool_get_selection(state: &McpState) -> Result<SelectionResult>;
fn tool_set_selection(state: &McpState, args: SelectionArgs) -> Result<SelectionResult>;

fn tool_play(state: &McpState) -> Result<()>;
fn tool_stop(state: &McpState) -> Result<()>;
fn tool_set_volume(state: &McpState, args: VolumeArgs) -> Result<()>;
fn tool_set_mode(state: &McpState, args: ModeArgs) -> Result<()>;
fn tool_set_speed(state: &McpState, args: SpeedArgs) -> Result<()>;
fn tool_set_pitch(state: &McpState, args: PitchArgs) -> Result<()>;
fn tool_set_stretch(state: &McpState, args: StretchArgs) -> Result<()>;

fn tool_apply_gain(state: &McpState, args: GainArgs) -> Result<()>;
fn tool_clear_gain(state: &McpState, args: GainClearArgs) -> Result<()>;
fn tool_set_loop_markers(state: &McpState, args: LoopArgs) -> Result<()>;
fn tool_write_loop_markers(state: &McpState, args: WriteLoopArgs) -> Result<()>;

fn tool_export_selected(state: &McpState, args: ExportArgs) -> Result<ExportResult>;

fn tool_open_folder(state: &McpState, args: OpenFolderArgs) -> Result<()>;
fn tool_open_files(state: &McpState, args: OpenFilesArgs) -> Result<()>;

fn tool_screenshot(state: &McpState, args: ScreenshotArgs) -> Result<ScreenshotResult>;
fn tool_get_debug_summary(state: &McpState) -> Result<DebugSummary>;
```

### 4.1 List/Search 関数詳細

- `tool_list_files`
  - 目的: 現在のリスト状態（フィルタ済み）からファイル一覧を返す
  - 入力: `query`/`regex` は UI の検索と互換にする（無指定なら全件）
  - 出力: path/name/folder/length/sr/ch/bits/peak/lufs/gain/status を含める
  - 例外: 正規表現が無効なら `isError=true` でメッセージ返却

- `tool_get_selection`
  - 目的: 選択行（複数）とアクティブタブを返す
  - 出力: `selected_paths`, `active_tab_path`
  - 注意: 選択が無い場合は空配列

- `tool_set_selection`
  - 目的: 選択対象を設定し、必要ならタブを開く
  - 入力: `paths`, `open_tab`
  - 挙動: リストに存在しないパスは無視し、成功件数を返す
  - 副作用: 選択が変わるため、再生対象が切り替わる

### 4.2 Playback 関数詳細

- `tool_play`
  - 目的: 現在の選択を再生
  - 挙動: モードに応じて `Speed` なら即時、`Pitch/Stretch` は重い処理完了後に再生
  - 例外: 選択がない場合は no-op

- `tool_stop`
  - 目的: 再生停止
  - 挙動: 再生位置は維持（UIの仕様に従う）

- `tool_set_volume`
  - 目的: マスター音量を dB で設定
  - バリデーション: 範囲は UI と同じ（例: -80..+6）にクランプ

### 4.3 Mode / Rate 関数詳細

- `tool_set_mode`
  - 目的: 再生モード切替（Speed/Pitch/Stretch）
  - 挙動: 現在選択のバッファを再構築（`rebuild_current_buffer_with_mode`）

- `tool_set_speed`
  - 目的: Speed モードの再生レートを設定
  - 例: 0.25..4.0 にクランプして保存
  - 挙動: Speed モード時は即時反映、他モードは次回再構築時に反映

- `tool_set_pitch`
  - 目的: PitchShift のセミトーンを設定
  - 例: -12..+12 にクランプ
  - 挙動: PitchShift モードなら再処理を実行

- `tool_set_stretch`
  - 目的: TimeStretch の倍率を設定
  - 例: 0.25..4.0 にクランプ
  - 挙動: TimeStretch モードなら再処理を実行

### 4.4 Edit 関数詳細

- `tool_apply_gain`
  - 目的: 指定ファイルに pending gain を設定
  - 仕様: 変更は即時保存せず、一覧の pending に積む
  - 副作用: LUFS 再計算の予約

- `tool_clear_gain`
  - 目的: 指定ファイルの pending gain をクリア

- `tool_set_loop_markers`
  - 目的: エディタ上のループ範囲を設定（書き込みはしない）
  - 仕様: タブが開いている場合はそのタブに反映、無い場合はキャッシュに保存

- `tool_write_loop_markers`
  - 目的: 現在のループ範囲をファイルへ書き込み
  - 仕様: wav/mp3/m4a で書式を変える（loop_markers モジュール）
  - 例外: Decode failed の行は拒否（再生と同じ安全ポリシー）

### 4.5 Export 関数詳細

- `tool_export_selected`
  - 目的: 選択ファイルを一括保存/上書き
  - 入力: `mode`, `dest_folder`, `name_template`, `conflict`
  - 出力: success/failed の配列と件数

### 4.6 File system 関数詳細

- `tool_open_folder`
  - 目的: フォルダ再帰走査でリストを置き換え
  - 仕様: dotfiles のスキップ設定に従う

- `tool_open_files`
  - 目的: 指定ファイルでリストを置き換え
  - 仕様: フォルダが渡された場合は再帰追加

### 4.7 Debug / Screenshot 関数詳細

- `tool_screenshot`
  - 目的: 現在のUIをPNGで保存
  - 入力: `path` が無い場合は既定の `screenshots/` に保存
  - 出力: 保存パス

- `tool_get_debug_summary`
  - 目的: デバッグ情報（選択/再生状態/モード）を取得

### 4.8 権限チェックとパス検証

- すべての I/O 系ツールは `allow_paths` の範囲内か検証する
- 書き込み系は `allow_write` が false の場合は拒否する
- パス検証は共通関数化する

```
fn validate_read_path(state: &McpState, path: &Path) -> Result<()>;
fn validate_write_path(state: &McpState, path: &Path) -> Result<()>;
```

### 4.9 共通データ型（詳細）

```
// 主要な引数/戻り値
struct ListFilesArgs {
    query: Option<String>,     // 文字列検索
    regex: Option<bool>,       // 正規表現モード
    limit: Option<u32>,        // 最大件数
    offset: Option<u32>,       // ページング
    include_meta: Option<bool>,// メタ情報を含める
}

struct FileItem {
    path: String,
    name: String,
    folder: String,
    length_secs: Option<f32>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    bits: Option<u16>,
    peak_db: Option<f32>,
    lufs_i: Option<f32>,
    gain_db: Option<f32>,
    status: Option<String>,    // "ok" / "decode_failed" / "missing" など
}

struct ListFilesResult {
    total: u32,
    items: Vec<FileItem>,
}

struct SelectionArgs {
    paths: Vec<String>,
    open_tab: Option<bool>,
}

struct SelectionResult {
    selected_paths: Vec<String>,
    active_tab_path: Option<String>,
}

struct ModeArgs { mode: String }  // "Speed" | "PitchShift" | "TimeStretch"
struct SpeedArgs { rate: f32 }
struct PitchArgs { semitones: f32 }
struct StretchArgs { rate: f32 }

struct GainArgs { path: String, db: f32 }
struct GainClearArgs { path: String }

struct LoopArgs { path: String, start_samples: u64, end_samples: u64 }
struct WriteLoopArgs { path: String }

struct ExportArgs {
    mode: String,              // "Overwrite" | "NewFile"
    dest_folder: Option<String>,
    name_template: Option<String>,
    conflict: Option<String>,  // "Rename" | "Overwrite" | "Skip"
}

struct ExportResult {
    ok: u32,
    failed: u32,
    success_paths: Vec<String>,
    failed_paths: Vec<String>,
}
```

### 4.10 JSON Schema 指針（例）

```
// tool_list_files の例
{
  "type": "object",
  "properties": {
    "query": { "type": "string" },
    "regex": { "type": "boolean" },
    "limit": { "type": "integer", "minimum": 1, "maximum": 10000 },
    "offset": { "type": "integer", "minimum": 0 },
    "include_meta": { "type": "boolean" }
  },
  "additionalProperties": false
}
```

### 4.11 具体的なリクエスト/レスポンス例

```
// list_files
Request:
{
  "query": "footstep",
  "regex": false,
  "limit": 100
}

Response:
{
  "total": 3,
  "items": [
    { "path": "E:\\...\\footstep_01.mp3", "name": "footstep_01.mp3", "folder": "E:\\...\\Foley", "length_secs": 1.02, "sample_rate": 44100, "channels": 2, "bits": 0, "peak_db": -3.1, "lufs_i": -16.4, "gain_db": 0.0, "status": "ok" }
  ]
}
```

```
// set_mode
Request:
{ "mode": "PitchShift" }
Response:
{ "ok": true }
```

```
// set_pitch
Request:
{ "semitones": 3.0 }
Response:
{ "ok": true }
```

```
// apply_gain
Request:
{ "path": "E:\\...\\footstep_01.mp3", "db": 1.5 }
Response:
{ "ok": true }
```

### 4.12 エラーコード設計

- `INVALID_ARGS`: スキーマ不一致 / 範囲外
- `NOT_FOUND`: path が存在しない
- `PERMISSION_DENIED`: allowlist 外 / 書き込み不可
- `DECODE_FAILED`: decode_error のファイルに対する重い処理
- `BUSY`: UI 側で処理中（処理完了待ちが必要）

レスポンス例:
```
{ "ok": false, "error": "DECODE_FAILED: skip pitch/stretch for this file" }
```

### 4.13 同期/非同期の扱い

- 重い処理（Pitch/Stretch）は非同期。`BUSY` を返し、完了後に状態が反映される
- 軽い操作（選択/停止/音量）は即時反映
- エクスポートは非同期。結果は `ExportResult` で返す

### 4.14 状態遷移（再生）

- `Stopped` -> `Playing` : `tool_play`
- `Playing` -> `Stopped` : `tool_stop`
- `Playing` 中に `tool_set_mode` が来た場合:
  - Speed: 即時反映
  - Pitch/Stretch: 再構築後に `Playing` へ戻る（または停止）

### 4.15 UI ブリッジ実装の責務

- MCP側の tool は `UiCommand` を投げるだけにする
- UI 側で既存の `select_and_load` / `rebuild_current_buffer_with_mode` などを利用
- 例: `tool_set_mode` -> `UiCommand::SetMode` -> UI で `rebuild_current_buffer_with_mode`

### 4.16 ログと監査

- すべての tool 実行をログに残す
  - 実行者（MCPクライアントID）
  - パス、変更内容、結果
- 失敗時の `error` は必ず返す

### 5) Resources

- `resource://files`
- `resource://file/{path}/meta`
- `resource://file/{path}/thumb`
- `resource://file/{path}/transcript` (将来)
- `resource://screenshot/latest`

```
fn list_resources(state: &McpState) -> Result<Vec<ResourceDescriptor>>;
fn read_resource(state: &McpState, uri: &str) -> Result<ResourceContent>;
```

### 6) Prompts

```
fn list_prompts() -> Result<Vec<PromptDescriptor>>;
fn get_prompt(name: &str, args: serde_json::Value) -> Result<PromptResult>;
```

### 7) 実装フェーズ

- Phase 1: MCP サーバー骨格（stdio）
- Phase 2: UI ブリッジ
- Phase 3: Tool 実装（list/play/stop/edit/export）
- Phase 4: Resources/Prompts
- Phase 5: SSE 対応

### 8) テスト方針

- Unit: tool input validation
- Integration: stdio transport で `list_tools` / `call_tool`
- UI: egui_kittest で一部動作を確認
