# UPDATE REQUEST PLAN (2026-03-04)

## 0. このドキュメントの目的
この計画は、2026-03-04 要望を「実装可能な粒度」まで分解し、既存機能の安定性を守りながら段階導入するための実行計画である。

主眼は次の 2 点。

- 小規模修正の即時価値: 再生安定性、操作性、表示整合を先に改善する。
- 大規模修正の破綻回避: Editor 表示体系・分析系・NodeGraph 拡張・CutPlayer・Recorder を土台から順に導入する。

本ドキュメントでは、要望を以下の 3 レベルで定義する。

1. 要件定義（何を満たすか）
2. 実装方針（どう実現するか）
3. 受け入れ条件（どう確認するか）

---

## 1. 最優先ポリシー

### 1-1. 再生挙動の非回帰を最優先にする
以下の不具合は UX 致命項目として扱う。

- Editor 表示中に tab switch / close / 高負荷処理で再生速度が変わる
- 閉じた tab が再生ソースでなくても停止する
- List 再生開始直後に crackle（クリック/ブツブツ）が出る

方針:

- まず Transport の不変条件をコードに固定する
- その後に UI/分析機能を積み増す

### 1-2. 表示モード再編は 2 層構造で固定する
Top-level と Sub-view を明確に分ける。

- Top-level: `Wave / Spec / Other`
- Sub-view:
  - Spec: `Spec / Log / Mel`
  - Other: `F0 / Tempogram / Chromagram`

これにより、Toolbar、Inspector、Session、Shortcut の整合性を保つ。

### 1-3. 重い分析は cache-first を原則化する
F0、Tempogram、Chromagram、WORLD、Spatial 系はすべて重い。
描画フレーム内で同期実行せず、job + cache で処理する。

---

## 2. 現状コード観測（2026-03-05）

実装設計の前提として、現状の構造的リスクを明示する。

### 2-1. tab close が無条件に stop する経路
`src/app.rs` の `close_tab_at()` で `self.audio.stop()` が無条件に呼ばれている。

影響:

- 閉じる tab が再生中ソースでなくても停止する
- 「他 tab close で止まる」の直接原因になり得る

### 2-2. List preview は prefix -> full 差し替え時の状態遷移が多い
`src/app/list_preview_ops.rs` で `replace_samples_keep_pos()` / `set_samples_buffer()` / `apply_list_preview_rate()` / `stop()` / `play()` の遷移が複数分岐に散在している。

影響:

- 開始直後に不連続が発生しやすい
- crackle の温床

### 2-3. 音量上限の実装は Audio 側に存在するが UI 統一が未完
`src/audio.rs` の `set_file_gain()` は線形 gain を `0.0..16.0` に clamp（+24dB 相当）している。

影響:

- UI 側の最大値と不一致だと操作結果が分かりにくい

### 2-4. AI モデルダウンロードは秒数表示のみ
`src/app/ui/topbar.rs` では transcript/music model download の表示が `({elapsed:.1}s)` になっている。

影響:

- 終了見込みが読めない
- ユーザー要望「% か 1/n 表示」に未対応

### 2-5. Analysis jobs の土台は既にある
`spectrogram_jobs` 系は cancel token + background thread + progress 集約を持つ。

示唆:

- Phase 3 の analysis platform は既存機構を一般化するのが最短

---

## 3. スコープ定義

## 3-1. 小規模修正（先行）

A. Loop 前後窓表示
B. Editor 範囲選択ジェスチャ変更（Shift + 右ドラッグ）
C. Editor 再生速度不正変化の禁止
D. List 冒頭 crackle 解消
E. 音量上限 24dB 統一
F. 他 tab close で再生停止しない
G. 再生中表示（Playing indicator）
H. FFT hop size 追加
I. List > Effect graph > Open
J. AI モデルダウンロード進捗を `%` または `n/N` で表示
K. 設定で音声出力先（output device）を切り替え可能にする

## 3-2. 大規模修正（段階導入）

L. Editor 表示再編（Wave/Spec/Other）
M. F0 解析 + WORLD 再合成
N. Tempogram/BPM
O. Chromagram/主音+モード推定
P. Spatial Nodes（HRTF/VBAP/UpMix）
Q. CutPlayer
R. Recorder

---

## 4. 実装方針（全体アーキテクチャ）

### 4-1. PlaybackSession を UI から分離する
導入する状態:

```rust
enum PlaybackSourceKind {
    None,
    List { path: std::path::PathBuf },
    Editor { path: std::path::PathBuf },
    EffectGraphTester,
    CutPlayer(u8),
    RecorderMonitor,
}

struct PlaybackSessionState {
    id: u64,
    source: PlaybackSourceKind,
    user_speed: f32,
    src_sample_rate: u32,
    is_playing: bool,
    is_looping: bool,
}
```

不変条件:

- `source` が変わらない限り `user_speed` は変えない
- tab close/switch/workspace switch は PlaybackSessionState を直接 stop しない
- stop は「明示停止」または「再生ソースの無効化」のみ

### 4-2. rate を user_speed と sr_ratio に分離する
目的:

- デバイス SR 補正とユーザー速度変更の責務分離

設計:

- `user_speed`: UI の速度
- `sr_ratio`: source SR / device SR
- 実効再生レート: `user_speed * sr_ratio`

### 4-3. Analysis は key ベースキャッシュ
最低キー:

- `path`
- `mtime`
- `file_size`
- `analysis_kind`
- `settings_hash`
- `cache_epoch`

### 4-4. Effect Graph は capability-aware
node ごとに `supported channel layout / sample rate / skip policy` を宣言し、非対応入力は原則 warning + skip。

---

## 5. 小規模修正 詳細仕様

## A. Loop 前後窓表示

### 要件

- LoopEdit Inspector に以下を表示:
  - Pre-Loop window
  - Seam preview
  - Post-Loop window
- large clip 制限の対象外
- mono mixdown でよい（main channel view 非依存）

### 実装案

- 追加場所: `src/app/ui/editor.rs`（LoopEdit inspector 描画ブロック）
- 追加ヘルパー（例）:

```rust
struct LoopWindowPreview {
    pre: Vec<f32>,
    seam_left: Vec<f32>,
    seam_right: Vec<f32>,
    post: Vec<f32>,
    sample_rate: u32,
}
```

- データ取り出し:
  - `pre = [loop_end-W, loop_end)`
  - `seam = [loop_end-W/2, loop_end) + [loop_start, loop_start+W/2)`
  - `post = [loop_start, loop_start+W)`

### 受け入れ条件

- seam だけでなく前後窓が常時表示される
- xfade 変更が seam 表示に反映される
- 大きなファイルでも表示できる

## B. Shift + 右ドラッグ範囲選択

### 要件

- 右ドラッグ単独: seek/playhead move
- Shift + 右ドラッグ: range select
- anchor は Shift 押下時の playhead 位置

### 実装案

- 修正候補: `src/app/input_ops.rs`、必要なら `src/app/ui/editor.rs`
- 追加 state（例）:
  - `right_drag_mode`
  - `shift_press_anchor_sample`
  - `range_anchor_sample`

判定順:

1. 右押下時は未確定
2. drag 閾値超えで mode 確定
3. Shift ありなら range
4. Shift なしなら seek

### 受け入れ条件

- Shift 押下時の playhead が anchor になる
- 右ドラッグ単独で範囲選択にならない

## C/F. tab close/switch でも再生不変

### 要件

- 他 tab close で stop しない
- tab switch/workspace switch で rate 不変

### 実装案（最小）

- `src/app.rs::close_tab_at()` の stop を条件化
- close 対象 path と playback source path が一致するときのみ stop

### 実装案（本命）

- `src/app/transport.rs` を追加し、stop/play/rate 更新を transport 層に集約

### 受け入れ条件

- 再生中に他 tab close しても継続
- 再生速度が勝手に変化しない

## D. List crackle 解消

### 要件

- 再生冒頭の click/pop を減らす

### 実装案

- `src/audio.rs`
  - `play()` 開始時に 5-20ms fade-in
  - buffer swap 直後にも短い ramp
- `src/app/list_preview_ops.rs`
  - prefix/full 切替の stop/play 連打を整理

### 受け入れ条件

- 冒頭 crackle が体感で低減
- prefix -> full 切替で不連続が目立たない

## E. 音量上限 24dB 統一

### 要件

- すべての gain UI を +24dB 上限に統一

### 実装箇所

- `src/app/gain_ops.rs`
- `src/app/ui/*`（volume slider）
- `src/app/effect_graph_ops.rs`（Gain node UI/validation）

### 受け入れ条件

- UI 上限と内部 clamp が一致

## G. Playing indicator

### 要件

- Top bar 音量表示左に `Playing` を表示

### 実装箇所

- `src/app/ui/topbar.rs`

### 受け入れ条件

- 再生中のみ表示

## H. FFT hop size

### 要件

- FFT window と hop を分離設定
- overlap は derived 表示

### 実装箇所

- `src/app/types.rs`（設定構造）
- `src/app/spectrogram_jobs.rs`（計算パラメータ）
- `src/app/ui/editor.rs`（UI）
- `src/app/project.rs` / `session_ops.rs`（保存互換）

### 互換方針

- 旧 session は overlap から hop へ移行

## I. List > Effect graph > Open

### 要件

- 右クリックメニューから Effect Graph を開く
- selected item を target/Input に設定
- 既存 target は置換

### 実装箇所

- `src/app/ui/list.rs`（メニュー）
- `src/app/effect_graph_ops.rs`（open_with_target 相当）

### 受け入れ条件

- 1 操作で Graph が開き target が反映される

## J. AI モデルダウンロード進捗表示（新規）

### 要件（必須）

- 秒数表示を主表示から外す
- 進捗を `%` か `n/N` で表示する
- transcript model download / music model download の両方に適用

### 現状

- `src/app/ui/topbar.rs` で `Downloading ... ({elapsed:.1}s)` を表示
- ダウンロード state は `started_at + rx` のみで、進捗情報を持たない（`src/app.rs`）
- ダウンロード処理は `repo.get(rel)` ループ（`src/app/transcript_ai_ops.rs`, `src/app/music_onnx.rs`）

### 表示仕様

優先順位:

1. `known total`: `ProgressBar + "42% (13/31)"`
2. `% 不明 / 件数既知`: `"13/31"`
3. 件数不明: spinner + phase（将来拡張）

補足:

- 秒数は tooltip や debug へ退避し、主表示にしない

### 状態構造（追加）

```rust
struct ModelDownloadProgress {
    phase: String,
    done: usize,
    total: usize,
    bytes_done: Option<u64>,
    bytes_total: Option<u64>,
    current_file: Option<String>,
}

enum ModelDownloadEvent {
    Progress(ModelDownloadProgress),
    Finished { model_dir: Option<PathBuf>, error: Option<String> },
}
```

Transcript/Music で共通化できる場合は共通 event に寄せる。

### 実装方針

- worker thread から `Progress` を都度送る
- `repo.get(rel)` 1 件ごとに `done += 1` して `n/N` を確実に出す
- `%` は `done/total` で計算（必要ファイル集合を先に決める）
- manifest 展開のある music では
  - seed files + manifest files の合計を `total` に反映

### 実装箇所

- `src/app.rs`
  - download state/result struct 拡張
- `src/app/transcript_ai_ops.rs`
  - download worker を progress event 送信型へ
- `src/app/music_ai_ops.rs`
  - queue/drain を progress event 対応
- `src/app/music_onnx.rs`
  - download helper に progress callback を追加
- `src/app/ui/topbar.rs`
  - 秒数表示を置換して bar + n/N 表示

### 受け入れ条件

- transcript/music ダウンロード中に `%` または `n/N` が表示される
- 旧秒数のみ表示が消える
- 完了時に 100% か N/N で終わる

## K. 設定で音声出力切り替え（新規）

### 要件（必須）

- Settings から出力デバイスを選択できる
- 選択変更時に AudioEngine を再初期化して新デバイスへ切り替える
- デバイスが無効/切断された場合は安全に fallback（既定デバイス）する
- 設定を保存し、次回起動時に復元する

### 現状

- `src/audio.rs` は `default_output_device()` 固定で初期化している
- `AudioEngine::new()` はデバイス選択を受け取れない
- `out_sample_rate` はデバイス依存で決まるため、切替時は関連 state 再適用が必要

### UI 仕様

- Settings パネルに `Output Device` を追加
- 表示:
  - 利用可能デバイス一覧（表示名）
  - 現在選択中デバイス
  - `Refresh` ボタン
  - 切替失敗時のエラーメッセージ
- 反映:
  - 選択即時適用（Apply ボタンなし）
  - 適用中は短い busy 表示

### 実装方針

- `src/audio.rs`
  - `list_output_devices()` 追加
  - `AudioEngine::new_with_output_device_name(name: Option<&str>)` 追加
  - `new()` は `new_with_output_device_name(None)` を呼ぶ
- `src/app/types.rs`
  - `AudioOutputPrefs { device_name: Option<String> }` 相当を追加
- `src/app/theme_ops.rs`（既存 prefs 保存経路）
  - 出力デバイス設定の save/load を追加
- `src/app/ui/tools.rs` か `src/app/ui/topbar.rs`（設定 UI 実装位置に合わせる）
  - デバイス選択コンボ + refresh + エラー表示
- `src/app/logic.rs` or 専用 `audio_device_ops.rs`
  - デバイス変更時に AudioEngine 差し替え
  - 既存の再生状態は原則 stop して安全に再開可能状態へ
  - `out_sample_rate` 依存の preview/loop/spectrogram state を再同期

### 受け入れ条件

- Settings で出力デバイスを切替できる
- 切替後に再生が新デバイスから出る
- 再起動後も選択が維持される（存在しない場合は default fallback）
- 切替失敗時に UI で原因が分かる

---

## 6. 大規模修正 詳細計画

## L. Editor 表示再編（Wave/Spec/Other）

### データ構造

```rust
enum EditorPrimaryView { Wave, Spec, Other }
enum EditorSpecSubView { Spec, Log, Mel }
enum EditorOtherSubView { F0, Tempogram, Chromagram }
```

### セッション移行

- 旧 `Wave` -> `Wave`
- 旧 `Spec` -> `Spec + Spec`
- 旧 `Mel` -> `Spec + Mel`

## M. F0 + WORLD

### 段階導入

1. F0 可視化
2. backend selector
3. F0 edit tool
4. WORLD 再合成
5. スペクトル包絡 warp

### backend 候補

- WORLD DIO/Harvest
- libf0 YIN/pYIN
- CREPE

### feature gating

- `f0-world`
- `f0-libf0`
- `f0-crepe`

## N. Tempogram/BPM

### MVP

- Analyze BPM
- Apply BPM
- confidence 表示

## O. Chromagram

### MVP

- chroma heatmap
- key estimate
- mode estimate

## P. Spatial Nodes

### HRTF (SOFA)

- any-ch -> 2ch
- channel ごとの球面位置
- spill/leakage option

### VBAP

- any-ch -> any-ch
- output speaker layout 指定

### UpMix

- 要件未確定
- 別 design doc で先に仕様凍結

## Q. CutPlayer

### 機能

- `Ctrl+0..9`: slot 登録
- `0..9`: 再生
- main transport と独立して重ね再生
- session 保存

### 追加前提

- AudioEngine の voice/mixer 複数化

## R. Recorder

### 機能

- Recorder tab
- 録音 + 波形表示
- take 一覧
- Apply で virtual list item 化
- tab close で未適用 take 破棄

---

## 7. PR 分割（実装イテレーション）

### PR-0A: Transport state 導入

- `PlaybackSessionState` / `PlaybackSourceKind` を追加
- 既存動作は極力変更しない

### PR-0B: close_tab_at stop 条件化

- 無条件 stop を廃止
- source 一致時のみ stop

### PR-0C: crackle 対策

- play/swap fade-in
- list preview の start/swap 遷移整理

### PR-0D: AI download progress 表示刷新

- download progress event 導入
- topbar 秒数表示 -> `% / n/N` 表示

### PR-1A..1F: 小規模 UX（A/B/E/G/H/I）

- Loop 前後窓
- Shift+右ドラッグ
- 24dB 統一
- Playing 表示
- FFT hop
- Effect graph open

### PR-2A..: View 再編
### PR-3A..: Analysis platform
### PR-4..: Tempogram/Chromagram
### PR-5..: F0 可視化
### PR-6..: WORLD 再合成
### PR-7..: Spatial nodes
### PR-8..: CutPlayer
### PR-9..: Recorder

---

## 8. テスト計画（詳細）

## 8-1. Phase 0 再生不変条件

- 再生中 tab close（非ソース）で継続
- 再生中 tab switch で rate 不変
- heavy job 中も rate 不変

## 8-2. List crackle

- 再生冒頭波形の差分ピークを測定
- prefix/full 切替点で不連続を閾値判定

## 8-3. AI download progress

- transcript download:
  - ProgressBar が増加
  - 完了時 100% or N/N
- music download:
  - manifest 展開時も total が更新
  - n/N が逆行しない

## 8-4. Loop/Selection

- Shift 押下時 anchor 固定
- 右ドラッグ単独は seek のみ

## 8-5. Session 互換

- 旧 overlap 設定の migration
- view mode migration

---

## 9. 受け入れ条件

## 9-1. 小規模修正

- 他 tab close/tab switch/workspace switch で rate 不変
- 非ソース tab close で再生継続
- List crackle が実用上問題ないレベルまで低減
- Loop 前後窓表示
- Shift+右ドラッグ範囲選択
- 音量上限 24dB 統一
- Playing 表示
- FFT hop size 設定
- List > Effect graph > Open が機能
- AI ダウンロード進捗が `%` または `n/N` 表示
- Settings で音声出力デバイスを切替できる

## 9-2. 大規模修正

- Wave/Spec/Other が安定動作
- Tempogram/Chromagram MVP 利用可能
- F0 -> 再合成が段階導入
- Spatial node が capability-aware
- CutPlayer/Recorder が独立 transport として成立

---

## 10. リスクと回避策

- WORLD/libf0/CREPE の依存肥大:
  - Cargo feature で段階導入
- AI download の `%` 精度:
  - 最低 n/N を保証し、bytes% は optional
- 出力デバイス切替時の再初期化失敗:
  - fallback を default device に固定し、UI に明示エラーを表示
- UpMix 要件未確定:
  - 先に設計文書を分離
- 再生系変更の副作用:
  - Phase 0 を最小 PR で細分化し、kittest 回帰を先に作る

---

## 11. 直近の着手順

1. PR-0A: PlaybackSessionState 導入
2. PR-0B: close_tab_at の stop 条件化
3. PR-0C: List crackle 対策（fade-in/swap）
4. PR-0D: AI ダウンロード進捗の `% / n/N` 表示対応
5. PR-0E: 設定からの音声出力デバイス切替対応
6. PR-1: 小規模 UX の残りを順次実装

---

## 12. 実装状況（2026-03-05 初版 / 2026-03-06 E/K 反映更新）

本節は、`A/B/C/D/E/F/G/H/I/J/K` の実装反映状況をコードとテストの両面で記録する。

### 12-1. ステータス一覧

- A. Loop 前後窓表示: **実装済み**
  - `src/app/ui/editor.rs` に `Pre-Loop window / Seam preview / Post-Loop window` を追加。
  - kittest: `tests/gui_kittest_suite.rs::loop_inspector_shows_three_windows` で検証。
- B. Shift + 右ドラッグ範囲選択: **実装済み**
  - 右ドラッグ単独を seek 専用化、Shift+右ドラッグを selection 化。
  - anchor はドラッグ開始時 playhead。
  - 関連 state: `right_drag_mode`, `right_drag_anchor`。
  - 回帰: `tests/small_fix_regressions.rs::shift_right_drag_selects_from_playhead_anchor`、
    `right_drag_seek_keeps_existing_selection`。
- C. Editor 再生速度不正変化の禁止: **実装済み**
  - `PlaybackSessionState`/`PlaybackSourceKind` 導入、`rate` 計算を
    `playback_rate_from_values()` に集約。
  - `EditorTab/CachedEdit` に `buffer_sample_rate` を追加し、
    preview restore / session sidecar save-load / tab reopen で一貫して使用。
  - 回帰: `tests/small_fix_regressions.rs::speed_mode_rate_stays_stable_across_workspace_switch`。
- D. List 冒頭 crackle 解消: **実装済み**
  - `AudioEngine` は明示 `play()` 時だけ短い ramp を維持。
  - buffer 差し替え (`replace_samples_keep_pos`) の `0->1 ramp` 再注入を廃止し、
    96-frame swap crossfade へ変更。
  - list prefix->full 差し替えは `replace_samples_keep_pos()` + crossfade で連続性を維持。
  - 回帰: `tests/mp3_preview_timing.rs`（timing/continuity/handoff 系）、
    `cargo test --lib audio::tests` の audio unit test。
- E. 音量上限 24dB 統一: **実装済み**
  - `src/app/helpers.rs` に `GAIN_DB_MIN/GAIN_DB_MAX` の共通定数を導入し、
    gain UI の上限を統一。
  - `src/app/ui/editor.rs` の `Stem Preview (dB)` 4スライダーと clamp を
    `-80..=24` に更新。
  - Global Volume は従来どおり `+6dB` 上限を維持。
  - 回帰: `tests/small_fix_regressions.rs::music_stem_preview_gain_clamps_to_plus_24_db`、
    `tests/gui_kittest_suite.rs::music_stem_preview_gain_clamps_to_plus_24_in_editor_ui`。
- F. 他 tab close で再生停止しない: **実装済み**
  - `close_tab_at()` は無条件 stop を廃止し、再生ソース invalid 時のみ stop。
  - 回帰: `tests/small_fix_regressions.rs::close_non_source_tab_keeps_playback_running`、
    `close_source_tab_stops_playback`。
- G. Playing indicator: **実装済み**
  - Topbar に再生中のみ `Playing` 表示を追加。
  - kittest: `tests/gui_kittest_suite.rs::topbar_playing_indicator_tracks_playback_state`。
- H. FFT hop size 追加: **実装済み**
  - `SpectrogramConfig` に `hop_size` を追加、UI は hop 編集 + overlap derived 表示。
  - prefs/session 互換（旧 overlap からの移行）を実装。
  - 回帰: `tests/small_fix_regressions.rs::spectrogram_hop_roundtrip_via_session_keeps_derived_overlap`、
    `src/app/project.rs` の migration test。
- I. List > Effect Graph > Open: **実装済み**
  - List 右クリックに `Effect Graph > Open` を追加し、target/input を即反映。
  - kittest: `tests/gui_kittest_suite.rs::list_context_effect_graph_open_sets_target_path`。
- J. AI モデルダウンロード進捗表示: **実装済み**
  - Transcript/Music ともに `Progress{done,total}` + `Finished` イベント駆動へ変更。
  - Topbar 表示を `n/N + ProgressBar(%)` へ更新（主表示の秒数依存を撤廃）。
  - 回帰: `src/app/transcript_ai_ops.rs` / `src/app/music_ai_ops.rs` の
    monotonic progress unit test、
    `tests/gui_kittest_suite.rs::model_download_progress_labels_show_n_over_n`。
- K. 出力デバイス切替: **実装済み**
  - `src/audio.rs` に `list_output_devices()` と
    `new_with_output_device_name(name: Option<&str>)` を追加。
  - app state に `audio_output_device_name / audio_output_devices / audio_output_error`
    を追加し、Settings で `Default + device list + Refresh + error` を表示。
  - 切替手順は `stop -> engine再生成 -> state再同期` で固定し、失敗時は
    default device へ安全フォールバック。
  - prefs に `audio_output_device=` を追加し、再起動復元に対応。
  - 回帰: `tests/small_fix_regressions.rs::audio_output_device_pref_roundtrip_and_fallback`、
    `tests/gui_kittest_suite.rs::settings_output_device_controls_visible`。

### 12-2. スコープ外/未実施

- L 以降（大規模項目）: **未着手**。

### 12-3. 2026-03-05 再実行テスト結果

- `cargo test --features kittest --test small_fix_regressions` -> **12 passed**
- `cargo test --features kittest --test mp3_preview_timing` -> **5 passed**
- `cargo test --features kittest --test ui_focus_input_regressions` -> **6 passed**
- `cargo test --features kittest --test gui_kittest_suite` -> **43 passed / 5 ignored**
- `cargo test --features kittest_render --test gui_kittest_suite kittest_render_saves_editor_screenshot_png`
  -> **1 passed**

補足:
- `kittest_render_saves_editor_screenshot_png` は GUI レンダリングを行い、PNG 保存まで検証。

### 12-4. 2026-03-06 再確認結果（最新）

2026-03-06 時点で、小規模項目の再確認を実施した。

- `cargo check` -> **ok**
- `cargo test --no-run` -> **ok**
- `cargo test --lib audio::tests -- --nocapture` -> **2 passed**
- `cargo test --features kittest --test small_fix_regressions -- --nocapture` -> **17 passed**
- `cargo test --features kittest --test mp3_preview_timing -- --nocapture` -> **5 passed**
- `cargo test --features kittest --test ui_focus_input_regressions -- --nocapture` -> **6 passed**
- `cargo test --features kittest --test gui_kittest_suite -- --nocapture` -> **47 passed / 5 ignored**
- `cargo test --features kittest_render --test gui_kittest_suite kittest_render_saves_editor_screenshot_png -- --nocapture`
  -> **1 passed**
- `cargo test --features kittest_render --test gui_kittest_suite kittest_render_zoom_ctrl_wheel_saves_before_after_screenshots -- --nocapture`
  -> **1 passed**

判定:

- `A/B/C/D/E/F/G/H/I/J/K`: **完了（テスト通過）**

実装根拠コード（2026-03-06 確認）:

- E: `src/app/ui/editor.rs` の Stem Preview slider/clamp が `-80.0..=24.0` へ更新
- K: `src/audio.rs` の device 指定初期化 + `src/app/ui/export_settings.rs` の
  Settings UI + `src/app/theme_ops.rs` の prefs 保存/復元対応

### 12-5. 2026-03-06 追加修正（再生速度変動 / ぶつぶつ音 / 波形表示遅延）

2026-03-06 の追加調査を受け、C/D/F と editor 波形表示に追補修正を入れた。

- 再生速度変動:
  - `PlaybackSessionState.src_sr` 相当を `buffer_sr` として再定義。
  - `EditorTab/CachedEdit` に `buffer_sample_rate` を追加し、
    preview restore / session sidecar save-load / undo/redo / reopen で共通利用。
  - `preview_restore_audio_for_tab()` は元ファイル SR ではなく
    `tab.buffer_sample_rate` で復帰するよう修正。
- session sidecar:
  - 保存時は `tab/cached.buffer_sample_rate` を sidecar WAV header に書き出す。
  - `ProjectTab/ProjectEdit` に `buffer_sample_rate` を追加し、
    reopen 時はそれを優先して current output SR へ正規化する。
  - 旧 session（`buffer_sample_rate` なし）は output SR 前提で救済し、
    debug log に記録する。
- ぶつぶつ音:
  - `replace_samples_keep_pos()` の再生中 `start_output_ramp(0->1)` を廃止。
  - callback 側で old/new buffer を 96-frame crossfade する swap state を追加。
- transport 統一:
  - list preview も `PlaybackSourceKind::ListPreview` を実使用し、
    `playback_mark_source()` 経由で rate を決めるよう統一。
- 波形表示:
  - `EditorDecodeResult` に `waveform_minmax` を追加し、worker 側で生成。
  - UI thread の full `mixdown + build_minmax` は撤去。
  - 圧縮音源 editor decode は prefix 後も `0.75s` ごとに中間 emit して、
    波形が段階的に伸びるよう変更。

追加回帰:

- `tests/small_fix_regressions.rs::preview_restore_keeps_rate_for_resampled_editor_buffer`
- `tests/small_fix_regressions.rs::session_sidecar_roundtrip_keeps_editor_rate_stable`
- `tests/small_fix_regressions.rs::list_preview_rate_uses_source_buffer_sample_rate`

実測メモ（`debug/long_load_test.mp3`）:

- `editor_open_to_partial_ms`: **201.8ms**
- `editor_open_to_first_paint_ms`: **204.7ms**
- `editor_open_to_final_ms`: **6060.5ms**
- `editor_mixdown_build_ms`: **0**（UI thread から除去済み）

注記:

- full 完了時間は decode 本体に強く依存する。
- ただし UI thread 側の full waveform 生成は撤去済みで、
  初回表示と途中更新は background decode の進行に追従する。

### 12-6. 2026-03-06 追加実装（Editor 波形高速化 / Peak-first LOD）

2026-03-06 の追加依頼を受け、Editor の base waveform 描画を
`Peak Pyramid + visible range only + allocation reuse` へ置き換えた。
対象は **Editor の波形表示のみ** とし、List 行サムネイル、Effect Graph preview、
スペクトログラム本体描画は今回スコープ外とした。

- 実装要点:
  - `src/app/render/waveform_pyramid.rs` を新設し、
    `Peak / PeakLevel / PeakPyramid / WaveformPyramidSet / WaveformScratch`
    を追加。
  - `WaveformPyramidSet` は runtime-only cache とし、session/prefs/project/sidecar
    には保存しない。
  - `build_editor_waveform_cache()` を追加し、
    `waveform_minmax(2048-bin overview)` と `waveform_pyramid` を同時生成する
    方式へ統一。
  - `EditorTab / CachedEdit / EditorUndoState / EditorDecodeResult` に
    `waveform_pyramid` を追加し、decode / session reopen / undo-redo /
    editor apply / plugin apply / music AI / effect graph apply / virtual reset
    の全経路で再利用する。
  - `WavesPreviewer` に `waveform_scratch` を 1 つ持たせ、描画時の
    `Vec<Shape> / line points / peaks / mono mix` を再利用する。
- 描画 LOD:
  - `spp < 2.0`: raw polyline / stems
  - `2.0 <= spp < 32.0`: 可視範囲のみを直接 min/max 化
  - `spp >= 32.0`: `PeakPyramid::query_columns()` を使う広域 LOD
  - 広域表示で毎フレームの長い `build_minmax` を廃止し、
    visible range と px-column 数だけを処理するよう変更。
- 互換維持:
  - `samples_per_px`、`view_offset`、Ctrl/Cmd+Wheel zoom、Shift+Wheel pan、
    Middle/Alt+Left drag pan、右ドラッグ系、fit whole、resize anchor は維持。
  - `waveform_minmax` は削除せず、既存 overview / 互換用途として残す。
  - waveform overlay、playhead、selection、trim、loop、fade、marker overlay の
    座標系は変更しない。
- debug/perf:
  - `debug_summary` に `waveform_render_ms / waveform_query_ms / waveform_draw_ms /
    waveform_lod_counts` を追加。
  - 広域表示時に `pyramid`、中間で `visible`、深い zoom で `raw` が使われたかを
    確認できるようにした。

追加テスト:

- unit:
  - `cargo test --lib waveform_pyramid -- --nocapture` -> **4 passed**
    - level0 固定 bin size
    - pairwise merge
    - `query_columns()` の direct build 整合
    - `build_mixdown_minmax_visible()` の実 mixdown 整合
- GUI / integration:
  - `cargo test --features kittest --test small_fix_regressions -- --nocapture`
    -> **18 passed**
  - `cargo test --features kittest --test gui_kittest_suite -- --nocapture`
    -> **54 passed / 5 ignored**
  - 追加した主な GUI テスト:
    - `editor_shift_wheel_pan_changes_view_offset`
    - `editor_middle_drag_pan_changes_view_offset`
    - `editor_zoom_then_pan_then_zoom_preserves_anchor_reasonably`
    - `editor_channel_view_switch_all_custom_mixdown_keeps_waveform_visible`
    - `editor_undo_redo_keeps_waveform_cache_renderable`
    - `editor_waveform_overlay_in_spec_mode_survives_zoom_and_pan`
    - `editor_waveform_lod_counters_cover_raw_visible_and_pyramid`
- kittest_render:
  - `cargo test --features kittest_render --test gui_kittest_suite kittest_render_pan_changes_waveform_position_png -- --nocapture`
    -> **1 passed**
  - `cargo test --features kittest_render --test gui_kittest_suite kittest_render_channel_view_all_vs_mixdown_differs_png -- --nocapture`
    -> **1 passed**
  - `cargo test --features kittest_render --test gui_kittest_suite kittest_render_waveform_overlay_spec_zoom_png -- --nocapture`
    -> **1 passed**

実測メモ:

- `debug/summary_waveform_perf_20260306.txt`
  - `editor_open_to_partial_ms: n=1 p50=99.3`
  - `editor_open_to_first_paint_ms: n=1 p50=101.6`
  - `waveform_render_ms: n=139 p50=0.2 p95=0.2 max=0.3`
  - `waveform_query_ms: n=139 p50=0.0 p95=0.0 max=0.1`
  - `waveform_draw_ms: n=139 p50=0.1 p95=0.2 max=0.2`
  - `waveform_lod_counts: raw=0 visible=0 pyramid=139`
- この summary 取得時点では full decode は完了前で、`editor_open_to_final_ms` は
  未計測だった。
- ただし広域表示で `pyramid` LOD が継続使用され、描画時間が sub-millisecond
  帯へ落ちていることは確認できた。

---

## 13. 追加修正リクエスト（2026-03-05）

本節は、追加依頼 2 件を「症状 / 原因 / 実装 / 検証」で管理する。

### 13-1. 追加依頼項目

- Z1. 音声の拡大表示（Zoom）が壊れており拡大できない問題を修正する。
- Z2. `Inspector > Trim > Set > Add Trim As Virtual` 後、Editor 再生が virtual 音声に寄る問題を修正し、
  常に「表示中タブの波形」を再生する。

### 13-2. 詳細実装計画

#### Z1. Zoom 修正

1. 目的
   - Ctrl/Cmd + ホイール、ピンチ、通常ホイールいずれでも Zoom が安定して効くこと。
2. 原因仮説
   - `Event::Zoom` 由来の係数解釈が `samples_per_px`（時間軸倍率の逆数）と逆方向になりうる。
   - `raw_scroll_delta` が 0 の環境で scroll 取得が不安定になる可能性がある。
3. 実装
   - `src/app/ui/editor.rs` の zoom 処理を `input.zoom_delta()` ベースへ統合。
   - `samples_per_px` には `zoom_delta` を反転適用（`1.0 / zoom_delta`）して方向を正規化。
   - wheel は `raw_scroll_delta` 優先、0 のとき `smooth_scroll_delta` を利用。
4. 受け入れ条件
   - Ctrl/Cmd + Wheel で `samples_per_px` が期待方向に変化する。
   - 画面描画としてもズーム前後の見え方が変化する（スクリーンショット差分確認）。

#### Z2. Add Trim As Virtual 後の再生ソース修正

1. 目的
   - `Add Trim As Virtual` 実行後に Space/Play したとき、Editor は現在表示中タブの波形を再生すること。
2. 原因仮説
   - `Trim > Set` で preview バッファ（trim mono）に切り替わったまま、
     `Add Trim As Virtual` 実行時に preview が解除されず transport に残る。
3. 実装
   - `src/app/editor_ops.rs::add_trim_range_as_virtual` 冒頭で
     `clear_preview_if_any(tab_idx)` を呼び、preview を解除して tab 音声へ復帰。
4. 受け入れ条件
   - `Set` 実行で preview が有効でも、`Add Trim As Virtual` 後の再生は source tab 音声になる。
   - Active tab/path が source のまま維持される。

### 13-3. テスト計画（追加）

- 自動テスト（kittest）
  - `tests/gui_kittest_suite.rs::editor_ctrl_wheel_zoom_in_changes_samples_per_px`
  - `tests/gui_kittest_suite.rs::trim_set_add_virtual_keeps_editor_waveform_playback_source`
- 自動テスト（kittest_render）
  - `tests/gui_kittest_suite.rs::kittest_render_zoom_ctrl_wheel_saves_before_after_screenshots`
  - before/after PNG 保存 + changed pixel 数でズーム反映を検証
- 回帰
  - `cargo test --features kittest --test gui_kittest_suite`
  - `cargo test --features kittest --test small_fix_regressions`
  - `cargo test --features kittest --test ui_focus_input_regressions`
  - `cargo test --features kittest --test mp3_preview_timing`

### 13-4. 実装結果メモ（2026-03-05）

- Z1: 実装済み（`editor.rs` zoom 入力統合 + 係数反転適用）
- Z2: 実装済み（`add_trim_range_as_virtual` で preview 復帰）
- 追加テスト: 実装済み（GUI/kittest/kittest_render）

---

## 14. 非progressive Editor Decode 改善（2026-03-07）

### 14-1. 目的

- `wav/m4a` の Editor decode で `Loading full audio 3%` 付近に張り付く問題を解消する。
- loading 中も波形概形がリアルタイムに伸びるようにし、1 秒以上の「止まって見える」状態を減らす。
- final 完了後は既存の高精度 waveform LOD へ自動復帰し、zoom/pan/seek の仕様は維持する。

### 14-2. 実装内容

- decode 戦略を 2 系統へ整理:
  - `CompressedProgressiveFull`
    - `mp3/ogg` の既存 progressive full 更新を継続
  - `StreamingOverviewFinalAudio`
    - `wav/m4a` 用に新設
    - source decode を chunk 単位で進め、progress と coarse overview だけを逐次通知
    - full PCM resample/quantize と full waveform cache build は final で 1 回だけ実行
- `AudioInfo.total_frames` と `FileMeta.total_frames` を追加。
  - `wav` は header `duration()` から取得
  - `m4a` は track duration と sample rate から換算
  - 進捗率と loading 中の visual 尺推定に使用
- Editor decode を event 駆動へ拡張:
  - `EditorDecodeEvent`
    - `Progress`
    - `FinalReady`
    - `Failed`
  - `EditorDecodeStage`
    - `Preview`
    - `StreamingFull`
    - `FinalizingAudio`
    - `FinalizingWaveform`
- `wav/m4a` 用 source chunk helper を追加:
  - `decode_audio_multi_streaming_chunks()`
  - `decode_audio_multi_symphonia_chunks()`
  - `decode_m4a_fdk_progressive_chunks()`
  - callback には「累積全量」ではなく「今回増えた chunk」のみを渡す
- loading 用 runtime-only overview を追加:
  - `StreamingWaveformOverview`
  - 固定 `2048` bins の mixdown min/max を chunk append で更新
  - progress event 送信時だけ snapshot を UI に渡す
- `EditorTab` に loading 時専用 state を追加:
  - `samples_len_visual`
  - `loading_waveform_minmax`
  - loading 中は表示長に `samples_len_visual` を使い、再生本体は `samples_len/ch_samples` のまま維持
- Editor render を loading-aware に変更:
  - `tab.loading && !loading_waveform_minmax.is_empty()` では loading overview を描画
  - final 完了後は既存 `raw / visible minmax / pyramid` の 3 LOD へ戻す
- progress bar を stage-aware 化:
  - `Preview: 0.00..0.15`
  - `StreamingFull: 0.15..0.92`
  - `FinalizingAudio: 0.95`
  - `FinalizingWaveform: 0.99`
  - `%` 表示は維持しつつ、non-progressive 形式でも単調増加するようにした
- debug/perf:
  - `editor_decode_progress_emit_ms`
  - `editor_decode_finalize_audio_ms`
  - `editor_decode_finalize_waveform_ms`
  - `editor_loading_progress_max_gap_ms`
  - `editor_loading_waveform_updates`
  - を `debug_summary` に追加

### 14-3. 互換・非変更点

- session/project/prefs/sidecar 形式は変更しない。
- loading 用 overview と `samples_len_visual` は runtime-only とする。
- `mp3/ogg` の既存 progressive full 更新方針は維持する。
- Ctrl/Cmd + Wheel zoom、Shift+Wheel pan、Middle/Alt+Left drag pan、右ドラッグ系、overlay、loop/trim/fade/marker の仕様は変更しない。

### 14-4. 追加テスト

- unit:
  - `cargo test --lib waveform_pyramid -- --nocapture` -> **5 passed**
    - `StreamingWaveformOverview` の bin 更新単調性を追加
- regression / kittest:
  - `cargo test --features kittest --test small_fix_regressions -- --nocapture`
    -> **21 passed**
  - 追加した主なテスト:
    - `audio_info_wav_reports_total_frames`
    - `wav_streaming_decode_emits_progressive_chunks`
    - `editor_wav_loading_progress_advances_and_waveform_updates_before_final`
- 既存回帰:
  - `cargo test --features kittest --test ui_focus_input_regressions -- --nocapture`
    -> **6 passed**
  - `cargo test --features kittest --test mp3_preview_timing -- --nocapture`
    -> **5 passed**
  - `cargo test --features kittest --test gui_kittest_suite -- --nocapture`
    -> **54 passed / 5 ignored**

### 14-5. 手動確認ポイント

- `E:\PC移行\sounds\music\学マス` の長い `wav` を Editor で開く
  - `Loading full audio` の `%` が進む
  - coarse overview が loading 中に伸びる
  - final 後に高精度波形へ切り替わる
- loading 中に zoom/pan しても view が壊れない
- `debug_summary` で
  - `editor_decode_progress_emit_ms`
  - `editor_loading_progress_max_gap_ms`
  - `editor_loading_waveform_updates`
  - を確認できる

---

## 15. Editor 軽量化と当初性能仕様の再確認（2026-03-08）

### 15-1. 当初仕様の確認結果

- `docs/PERFORMANCE_SCALABILITY_PLAN.md`
  - `300k files` でも list を軽く保つ
  - `3 hours` 級 long audio でも responsive に見せる
  - `Stage 1: quick header/meta`
  - `Stage 2: downsampled waveform preview`
  - `Stage 3: full decode`
- `AGENTS.md`
  - list は常に速く保つ
  - editor は多少重くてもよいが、必ず progress/feedback/cancel を出す
  - long audio は preview first / full decode later の progressive loading を優先する

### 15-2. 現状アーキテクチャ上の整理

- list 側は `scan_ops.rs` の time budget drain と `meta/header` 分離で、
  「大量件数でも UI を止めない」方向には沿っている。
- editor 側は描画 LOD 最適化で `waveform_render_ms` 自体はかなり軽くなったが、
  開始直後の体感は decode / proxy 準備にまだ支配される。
- 再生 transport は `src/audio.rs` の `AudioBuffer` 常駐再生が基本であり、
  **真に 3 時間級を高音質のまま即時全域再生するには file-backed / streamed transport が別途必要**。
- そのため今回の実装方針は、
  - UI をまず 1 秒未満で反応させる
  - 全体波形を粗く先に出す
  - 再生は exact audio 完了までロックする
  - full PCM / full waveform は後から埋める
  という 3 段ロードへ寄せる。

### 15-3. 今回の追加実装

- WAV editor open の順序を次で固定した。
  1. tab open 直後:
     - `samples_len_visual` と placeholder overview で全体タイムラインを即表示
  2. ultra-fast whole overview:
     - `EDITOR_PROXY_OVERVIEW_MAX_TOTAL_SAMPLES = 16_384`
     - very small sparse WAV proxy から `loading_waveform_minmax` を先に送る
     - これにより full audio 前でも「全体の粗い波形」が出る
  3. final exact audio:
     - background で full source decode
     - loading 中は `Progress` で overview だけ更新する
     - `FinalReady` 到着までは playback を許可しない
  4. final detail:
     - final で full PCM + `waveform_pyramid` を差し替える
- `EditorDecodeUiStatus` は overview が届いた時点で `3%` 固定に見えにくいよう、
  pre-preview でも小さく前進するよう調整した。

### 15-4. 実測（実データ / 学マス最大 WAV）

- 対象:
  - `E:\PC移行\sounds\music\学マス\学園アイドルマスター_初星学園_十王星南_小さな野望\学園アイドルマスター_初星学園_十王星南_小さな野望.wav`
  - size: `188,878,080 bytes`
- 旧実測:
  - `debug/wav_loading_real_late3.txt`
    - `editor_open_to_partial_ms: 2956.3`
    - `editor_open_to_first_paint_ms: 2960.3`
  - `debug/wav_proxy_measure_openfirst_v2.txt`
    - `editor_open_to_partial_ms: 2455.3`
    - `editor_open_to_first_paint_ms: 2465.1`
- 今回実測:
  - `debug/editor_wave_ui_summary_20260308_openfirst_30f.txt`
    - `editor_open_to_partial_ms: 687.6`
    - `editor_open_to_first_paint_ms: 692.4`
    - `editor_decode_progress_emit_ms: n=82 p50=5.2 p95=5.7 max=6.5`
    - `editor_loading_waveform_updates: total=0 live=84`
    - `waveform_render_ms: n=30 p50=0.2 p95=0.2 max=0.3`
- スクリーンショット:
  - `debug/editor_wave_ui_30_20260308_openfirst.png`
    - `30 frame` 時点で全体尺 `5:26.2` が表示され、
      coarse whole waveform と `Loading full audio 20%` が確認できる

### 15-5. 現時点の評価

- 達成できたこと
  - 1 秒未満で editor 初回表示に到達
  - 全体タイムラインは open 直後から表示
  - coarse whole waveform は final 前に見える
  - loading 中は再生をブロックし、exact audio 完了後のみ再生する
  - 既存 zoom/pan/overlay/selection は維持
- まだ未達のこと
  - 3 時間級音源を **高音質そのまま** 即時全域再生すること
  - これは現 transport が in-memory `AudioBuffer` 前提のため、
    proxy ではなく file-backed streaming を別計画で入れる必要がある

### 15-6. 次段でやるべきこと

- Phase A
  - current overview-first editor open を他形式にも広げる
  - `m4a` の ultra-fast overview 経路を追加する
- Phase B
  - editor transport を file-backed / paged streaming 化する
  - 3 時間級でも「高音質のまま全域再生」を proxy なしで行えるようにする
- Phase C
  - edit apply を region/segment ベースへ寄せ、
    full buffer 再構築を減らして編集自体も軽くする

---

## 16. File-backed Editor Transport 第1段（2026-03-08）

### 16-1. 目的

- `AudioBuffer` 常駐前提の限界を緩和し、長尺 source audio の exact playback を
  RAM 常駐に依存せず行えるようにする。
- 表示は従来どおり rough overview / full decode worker を維持しつつ、
  再生だけを file-backed transport へ切り出す。

### 16-2. 今回の実装範囲

- 対象は **Editor / Speed mode / pristine source WAV tab** に限定。
- 条件:
  - source file が `wav`
  - virtual ではない
  - destructive edit 未適用 (`dirty == false`)
  - preview audio / preview overlay なし
  - sample-rate / bit-depth override なし
- 上記条件を満たす active tab は `Vec<Vec<f32>>` 常駐ではなく、
  **memory-mapped WAV transport** を使って exact audio を再生する。
- loading 中でも stream transport が立てば再生可能。
- final decode 完了後も pristine source WAV のままなら stream transport を維持する。

### 16-3. 実装内容

- `Cargo.toml`
  - `memmap2` を追加
- `src/audio.rs`
  - `MappedWavSource` を追加
  - WAV `fmt/data` chunk を読んで `mmap` する file-backed source を実装
  - callback は `AudioBuffer` が無い場合、`MappedWavSource` から直接 sample interpolate する
  - `WAVE_FORMAT_EXTENSIBLE` (`PCM` / `float`) を transport 側でも解釈
  - `AudioEngine` に以下を追加:
    - `set_streaming_wav_path()`
    - `has_audio_source()`
    - `current_source_len()`
    - `streaming_wav_sample_rate()`
    - `is_streaming_wav_path()`
- `src/app/logic.rs`
  - `editor_stream_transport_source_sr()`
  - `try_activate_editor_stream_transport_for_tab()`
  - を追加
  - `active_editor_exact_audio_ready()` は
    - exact stream active
    - または final buffer ready
    のどちらでも true になるよう変更
  - `rebuild_current_buffer_with_mode()` と final decode apply は
    pristine source WAV の場合 stream transport を優先する
- `src/app/preview.rs`
  - preview restore は eligible な source WAV なら buffer ではなく stream へ戻す
- `src/app.rs` / `src/app/ui/editor.rs`
  - playhead / seek / loop の audio length 参照を
    `AudioBuffer.len()` 固定から current transport length 参照へ変更
  - loading 中の visual length と source-stream length の変換を維持

### 16-4. 非変更点 / 制限

- `mp3/m4a/ogg` はまだ file-backed transport 対象外。
- dirty tab / edited tab / effect result / preview tool は従来どおり buffer transport。
- つまり今回の段階は
  - **長尺 pristine WAV source の exact playback**
  に限った transport 改修。
- 「3時間級の全形式・全編集状態を stream で再生」はまだ未達。

### 16-5. 追加テスト

- `cargo test --lib audio::tests -- --nocapture` -> **3 passed**
  - `streaming_wav_source_reports_length_and_rate_without_heap_buffer`
- `cargo test --features kittest --test small_fix_regressions -- --nocapture` -> **23 passed**
  - `editor_wav_loading_progress_advances_and_waveform_updates_before_final`
    - loading 中に exact stream transport が立つこと
    - loading 中でも playback 開始できること
    - final decode 完了後も継続すること
- `cargo test --features kittest --test gui_kittest_suite -- --nocapture` -> **54 passed / 5 ignored**

### 16-6. 次段の候補

- `m4a` / `mp3` へ file-backed transport を広げる
- dirty tab を segment graph + paged render にして edit 後も常駐 buffer 依存を減らす
- loop / seek の stream crossfade を強化して source->buffer 切替時の click をさらに減らす

---

## 17. Editor 再生途中のピッチ上昇 修正（2026-03-08）

### 17-1. 原因

- `Editor exact stream` 再生中に、後から完了した `processing` 結果が
  current audio source を `output-SR buffer` へ上書きし得る経路が残っていた。
- このとき `playback_mark_source(..., out_sr)` が再実行され、
  `buffer_sr / out_sr` 比が `source_sr / out_sr -> 1.0` に変わるため、
  44.1kHz source / 48kHz output のようなケースで
  再生途中に pitch / speed が上がる危険があった。
- さらに `processing` は `path` 基準で apply していたため、
  stale job や list preview 向け result が active editor へ漏れる余地もあった。

### 17-2. 今回の修正

- `ProcessingState` / `ProcessingResult` に以下を追加
  - `job_id`
  - `mode`
  - `target` (`EditorTab(PathBuf)` / `ListPreview(PathBuf)`)
- `tick_processing_state()` は
  - `job_id`
  - `mode`
  - `target`
  - current workspace / current target
  - exact stream active 状態
  を照合し、条件不一致の result を **discard** するよう変更
- `Speed` mode では heavy processing job 自体を新規 spawn しない
- `try_activate_editor_stream_transport_for_tab()` と
  `rebuild_current_buffer_with_mode()` の `Speed` 経路では、
  同 target 向け pending processing を軽量 invalidate するよう変更
- `ListPreview` 向け processing result は
  active editor tab の waveform cache / audio source を更新しないよう修正

### 17-3. 追加テスト

- `cargo check` -> **passed**
- `cargo test --features kittest --test small_fix_regressions -- --nocapture` -> **27 passed**
  - `editor_stream_discards_stale_processing_result_without_rate_jump`
  - `editor_processing_result_job_id_mismatch_is_discarded`
  - `list_processing_result_does_not_leak_into_active_editor_tab`
  - `speed_mode_does_not_spawn_heavy_processing_for_editor_tab`
- `cargo test --features kittest --test gui_kittest_suite -- --nocapture` -> **54 passed / 5 ignored**

### 17-4. 現時点の保証

- exact stream 再生中に stale `processing` result が current source を上書きしない
- `Speed -> Pitch/Stretch -> Speed` と戻した後でも、旧 job 完了で rate が跳ねない
- list preview 用 processing result が active editor へ漏れない
- `Speed` mode で不要な heavy processing job を積まない

---

## 18. `Finalizing exact audio` 中の pitch 変化 修正（2026-03-08）

### 18-1. 原因

- `Finalizing exact audio` の完了時に `stream -> buffer` あるいは `buffer -> buffer` の source 差し替えが走る経路では、
  playhead を「秒」ではなく「サンプル番号」のまま持ち回していた。
- そのため `44.1kHz source -> 48kHz output buffer` のような切替で、
  `play_pos_f=44100` をそのまま新 buffer に適用してしまい、
  `1.0 秒地点` ではなく `0.91875 秒地点` を再生する危険があった。
- これが再生途中の pitch / speed 変化として聞こえていた。

### 18-2. 今回の修正

- `AudioEngine` に `set_samples_buffer_keep_time_pos()` /
  `set_samples_channels_keep_time_pos()` を追加
  - 旧 source の `play_pos_f` と `from_sr`
  - 新 source の `to_sr`
  から **時間基準で新 playhead を再計算** して適用する
- `replace_samples_keep_pos()` とは別に、
  **sample-rate が変わる source 差し替え専用** の API として分離
- Editor の以下の経路をこの helper に切替
  - `Finalizing exact audio` 完了時の active tab 反映
  - `rebuild_current_buffer_with_mode()` の `Speed` 経路
  - active tab 再アクティベート時の `Speed` buffer fallback
  - `preview_restore_audio_for_tab()`
- kittest helper の `sample_rate_override` 設定は、
  list selection が無い editor active 状態でも active tab を対象にできるよう補強

### 18-3. 追加テスト

- `cargo test --lib audio::tests -- --nocapture` -> **4 passed**
  - `remap_play_pos_to_sample_rate_preserves_time_when_switching_sources`
- `cargo test --features kittest --test small_fix_regressions -- --nocapture` -> **29 passed**
  - `stream_to_buffer_rebuild_preserves_playback_timebase`
  - `editor_wav_finalizing_exact_audio_keeps_stream_rate_while_playing`
- `cargo test --features kittest --test gui_kittest_suite -- --nocapture` -> **54 passed / 5 ignored**

### 18-4. 現時点の保証

- `Finalizing exact audio` 完了時に source が差し替わっても playhead は時間基準で維持される
- pristine WAV の exact stream 再生中は、final 完了後も `rate` が途中で変わらない
- `stream -> buffer` / `buffer -> buffer` の切替で pitch / speed が途中から跳ねない
