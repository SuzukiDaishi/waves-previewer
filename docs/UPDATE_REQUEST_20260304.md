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
  - 回帰: `tests/small_fix_regressions.rs::speed_mode_rate_stays_stable_across_workspace_switch`。
- D. List 冒頭 crackle 解消: **実装済み**
  - `AudioEngine` に再生開始/差し替え時 ramp を導入（`start_output_ramp`）。
  - list prefix->full 差し替えは `replace_samples_keep_pos()` 優先で連続性を維持。
  - 回帰: `tests/mp3_preview_timing.rs`（timing/continuity/handoff 系）、
    `src/audio.rs` の ramp unit test。
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
- `cargo test --features kittest --test small_fix_regressions -- --nocapture` -> **14 passed**
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
