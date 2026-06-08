# Loop Detection / Recording / Auto Trim 実装計画

作成日: 2026-06-07

対象:

- Inspector > Loop Edit の自動ループ区間検出
- Tools > Recording の録音用タブ
- Inspector > Auto Trim の必要区間自動検出

この文書は `docs/ループ検出GitHub三件の深層調査レポート.md` と `docs/ループ検出とリアルタイム区間分割とRust録音設計の詳細調査.md` を前提に、NeoWaves の現コードへ落とし込むための実装計画をまとめる。Rust ソースはこの文書作成時点では変更しない。

## Executive Summary

v1 では、重い音楽構造解析や ML scorer を必須にせず、決定論的で説明しやすい処理から実装する。

- Auto Loop Detection は AutoLooper 系の軽量特徴、候補生成、局所探索、ゼロクロス補正を採用する。
- PyMusicLooper 系の chroma / beat / SSM は将来の Deep mode として設計余地を残す。
- Recording は UI thread で録音データを処理せず、capture callback、worker、一時 WAV、live waveform overview を分離する。
- Auto Trim は既存の `trim_range` と `VirtualOp::Trim` に載せ、`t` / `v` / `c` や Inspector の Trim 操作と互換にする。
- すべての長時間処理は progress / cancel / stale job guard を持たせ、UI が止まる設計を避ける。

## 現コードの接続点

### Loop Edit

主に見るファイル:

- `src/app/ui/editor.rs`
- `src/app/editor_ops.rs`
- `src/app/input_ops.rs`
- `src/app/types.rs`

既存状態:

- `ToolKind::LoopEdit` が Inspector の Loop Edit UI を持つ。
- `EditorTab` は `loop_region`、`loop_region_applied`、`loop_region_committed` を持つ。
- Loop Edit の Apply / preview / xfade / unwrap は既存経路がある。
- キーボード操作でも loop region の開始、終了、apply が行われている。

実装方針:

- 自動検出は既存の `loop_region` に候補を反映するだけにし、即時 commit はしない。
- ユーザーは既存の Apply / xfade / preview 操作で最終確定する。
- これにより既存の保存、export、loop marker 書き込み経路を再利用できる。

### Trim

主に見るファイル:

- `src/app/ui/editor.rs`
- `src/app/editor_ops.rs`
- `src/app/input_ops.rs`
- `src/app/types.rs`

既存状態:

- `ToolKind::Trim` が Inspector の Trim UI を持つ。
- `EditorTab` は `trim_range` を持つ。
- `editor_apply_trim_range`、`begin_trim_virtual_job`、`add_trim_range_as_virtual` がある。
- `T` は destructive trim、`V` は virtual trim、`C` は delete/join に使われている。
- Virtual item の履歴は `VirtualOp::Trim { start, end }` で表現できる。

実装方針:

- Auto Trim は検出した範囲を `trim_range` に設定する。
- 実際の destructive / virtual / cut 操作は既存ボタンと `t` / `v` / `c` に任せる。
- Auto Trim 専用の新しい virtual operation は v1 では追加しない。

### Recording

主に見るファイル:

- `Cargo.toml`
- `src/audio.rs`
- `src/app/ui/topbar/menus.rs`
- `src/app/frame_ops.rs`
- `src/app/tab_ops.rs`
- `src/app/types.rs`
- `src/app.rs`

既存状態:

- `Cargo.toml` には `cpal`、`hound`、`rubato`、`symphonia` がある。
- `src/audio.rs` は主に再生出力の `cpal` stream を扱っている。
- `WorkspaceView` は List / Editor / EffectGraph の切り替えに使われている。
- Tools menu は `src/app/ui/topbar/menus.rs` にある。
- `MediaItem` は `virtual_audio: Option<Arc<AudioBuffer>>` と `MediaSource::Virtual` を扱える。
- virtual item は Editor tab に開けば通常の Inspector 操作に載る。

実装方針:

- 録音入力は既存の再生用 `AudioEngine` に直接詰め込まず、別モジュールとして分離する。
- Tools > Recording... で録音用 workspace tab を開く。
- 録音停止後は既存の MediaItem / EditorTab 経路へ渡し、録音音声も通常音声と同じ編集対象にする。

## Feature 1: Inspector > Loop Edit > Auto Detect

### UX

Loop Edit Inspector に以下を追加する。

- `Auto Detect` button
- `Cancel` button
- progress 表示
- status message
- top candidates list
- confidence / score / length / start / end の表示
- `Use` button
- optional settings foldout

基本動作:

1. ユーザーが Loop Edit を開く。
2. `Auto Detect` を押す。
3. background job が解析する。
4. 候補が返る。
5. 最上位候補は preview 用に `tab.loop_region` へ仮反映する。
6. 候補一覧から別候補を選ぶと `tab.loop_region` が更新される。
7. 最終確定は既存の Apply / loop marker 保存経路で行う。

自動検出は「提案」であり、ファイルや session を即時変更しない。

### v1 Algorithm

v1 は AutoLooper 系の軽量な時間領域処理を中心にする。

前処理:

- 対象は active editor tab の `ch_samples`。
- 解析用に mono downmix する。
- DC offset を除去する。
- peak または RMS で正規化する。
- 極端に短い音声は early return する。
- 長尺では解析用に downsample した feature stream を使う。

既定値:

| 項目 | 初期値 | 備考 |
| --- | ---: | --- |
| minimum loop length | 3.0 s | AutoLooper 系の既定に合わせる |
| match window | 1.5 s | 境界前後の比較窓 |
| feature bins | 48 or 64 | mean-abs / RMS ベース |
| coarse candidate limit | 64 | UI応答性優先 |
| zero-cross snap radius | 256 samples | click低減 |
| local refine coarse radius | 2048 samples | 128 sample stride |
| local refine fine radius | 128 samples | 8 sample stride |

候補生成:

- 既存 loop marker がある場合は候補に入れて再採点する。
- 選択範囲がある場合は候補に入れて再採点する。
- RMS flux から onset-like peaks を抽出する。
- regular grid fallback を併用する。
- loop length が短すぎる候補を除外する。
- 長尺では候補点数に上限を設ける。

粗スコア:

- feature vector は block ごとの mean-abs と RMS を混ぜて作る。
- feature は平均 0、L2 norm 1 に正規化する。
- start/end 候補の feature dot product で粗く絞る。

局所精密化:

- start/end 近傍で correlation と RMS error similarity を評価する。
- loudness difference penalty を加える。
- loop length prior で短すぎる候補を抑える。
- zero-cross snap 後に再採点し、スコア悪化が大きい場合は補正前を使う。

confidence:

- `score >= 0.90`: High
- `0.75 <= score < 0.90`: Medium
- `score < 0.75`: Low

Low confidence の場合は `tab.loop_region` へ自動反映せず、候補一覧だけ表示する。

### Deep Mode の後続拡張

v1 では必須にしないが、設計上は以下を追加できる形にする。

- chroma / HPCP
- beat / PLP / onset strength
- 粗い self-similarity matrix
- STFT phase continuity
- repeated-loop perceptual continuity score
- 軽量 ML seam scorer

Deep mode は処理時間が長くなりやすいため、progress、cancel、推定残り時間を必須にする。

### Proposed Types

```rust
pub struct LoopDetectConfig {
    pub min_loop_secs: f32,
    pub max_loop_secs: Option<f32>,
    pub match_window_secs: f32,
    pub candidate_limit: usize,
    pub zero_cross_radius: usize,
    pub mode: LoopDetectMode,
}

pub enum LoopDetectMode {
    Fast,
    Deep,
}

pub struct LoopDetectCandidate {
    pub start: usize,
    pub end: usize,
    pub score: f32,
    pub confidence: LoopDetectConfidence,
    pub reason: String,
}

pub enum LoopDetectConfidence {
    High,
    Medium,
    Low,
}

pub struct LoopDetectState {
    pub generation: u64,
    pub running: bool,
    pub progress: f32,
    pub message: String,
    pub candidates: Vec<LoopDetectCandidate>,
}
```

配置候補:

- pure DSP: `src/app/loop_detect.rs`
- app glue: `src/app/loop_detect_ops.rs`
- UI追加: `src/app/ui/editor.rs`
- state: `src/app/types.rs`

`src/app/ui/editor.rs` は大きいため、可能なら Loop Edit UI の分割と同時に `src/app/ui/editor/loop_edit.rs` 相当へ切り出す。ただし分割が大きくなる場合は、v1 では最小差分で追加する。

## Feature 2: Tools > Recording

### UX

Tools menu に `Recording...` を追加する。

Recording tab の要素:

- source mode
  - System
  - Microphone
  - System + Microphone
- input device select
- system audio availability status
- sample rate display
- channel display
- input level meter
- live waveform overview
- elapsed time
- record / pause / stop / discard
- last recording status
- `Open in Editor`

録音中も UI は通常操作可能にする。録音 tab を閉じる、別 tab へ移動する、Editor を操作する場合も capture は worker 側で継続できるようにする。

### Capture Scope

v1 の対象:

- microphone: `cpal` input device
- system audio: Windows WASAPI loopback
- system + microphone: worker 側で resample / mix

後続対象:

- macOS: ScreenCaptureKit
- Linux: PipeWire

macOS / Linux で system audio を選んだ場合は、v1 では未対応の明示メッセージを表示する。microphone は `cpal` で可能な範囲で対応する。

### Data Flow

```text
Tools > Recording...
  -> WorkspaceView::Recording
  -> device enumeration
  -> capture stream start
  -> realtime callback
  -> lock-free or bounded channel
  -> recording worker
  -> temp WAV write
  -> waveform overview / level event
  -> UI drain
  -> Stop
  -> finalize WAV
  -> create MediaItem
  -> open EditorTab
```

重要な分離:

- capture callback では allocation、file I/O、log、heavy resample をしない。
- worker が temp WAV 書き込み、resample、mix、waveform overview 生成を行う。
- UI は受信済み overview と状態だけ描画する。

### Recorded Clip Handling

録音後の音声は既存の編集経路に載せる。

短尺:

- `Arc<AudioBuffer>` を作る。
- `MediaSource::Virtual` として list に追加する。
- `VirtualSourceRef::Sidecar("recording:<id>")` または将来の `VirtualSourceRef::Recording` を使う。
- Editor tab を開く。

長尺:

- temp WAV backing を維持する。
- file-like item として list に追加する。
- Editor decode は通常 file decode 経路を使う。
- 必要に応じて progressive decode と waveform overview を使う。

この分岐により、長尺録音で `Arc<AudioBuffer>` を巨大化させて UI やメモリを不安定にしない。

### Persistence

v1 では `.nwsess` に raw audio を埋め込まない。

- 録音直後の temp WAV は session 内の unsaved asset として扱う。
- ユーザーが export / save as を行うまで、終了時に未保存警告を出す。
- session 保存時は、temp WAV の path と recorded item metadata を保持できるかを検討する。
- temp cleanup は「Editor tab / list item が参照していない録音ファイルだけ」を対象にする。

### Proposed Types

```rust
pub enum RecordingSourceKind {
    System,
    Microphone,
    SystemAndMicrophone,
}

pub struct RecordingDeviceInfo {
    pub id: String,
    pub display_name: String,
    pub channels: u16,
    pub default_sample_rate: u32,
}

pub enum RecordingState {
    Idle,
    Armed,
    Recording,
    Paused,
    Finalizing,
    Error(String),
}

pub struct RecordingTabState {
    pub source: RecordingSourceKind,
    pub selected_mic_id: Option<String>,
    pub state: RecordingState,
    pub elapsed_secs: f32,
    pub level_l: f32,
    pub level_r: f32,
    pub waveform_overview: Vec<(f32, f32)>,
    pub progress_message: String,
    pub last_recording_path: Option<PathBuf>,
}
```

配置候補:

- lower-level capture: `src/audio_capture.rs`
- app glue: `src/app/recording_ops.rs`
- UI: `src/app/ui/recording.rs`
- menu hook: `src/app/ui/topbar/menus.rs`
- frame workspace: `src/app/frame_ops.rs`
- state: `src/app/types.rs`

### Error Handling

表示すべきエラー:

- input device not found
- device permission denied
- unsupported system audio on current OS
- WASAPI loopback initialization failed
- sample format unsupported
- disk write failed
- worker lag / dropped buffer
- temp WAV finalize failed

録音中に worker が詰まった場合は drop count を Debug window に出し、UI には「録音は継続中だが一部 buffer drop が発生した」ことを表示する。

## Feature 3: Inspector > Auto Trim

### UX

Trim Inspector に以下を追加する。

- `Auto Trim` button
- `Cancel` button
- progress 表示
- status message
- threshold / pre-roll / post-roll settings foldout
- detected range display

基本動作:

1. ユーザーが Trim tool を開く。
2. `Auto Trim` を押す。
3. background job が波形全体を解析する。
4. 検出結果を `tab.trim_range` に設定する。
5. ユーザーは既存の `Apply trim`、`Add Trim As Virtual`、`T`、`V`、`C` で実行する。

Auto Trim は destructive 操作を直接実行しない。

### v1 Algorithm

前処理:

- active editor tab の全チャンネルを解析する。
- mono downmix する。
- block RMS と peak を計算する。
- 必要なら DC offset を除去する。

threshold:

- noise floor を低 percentile から推定する。
- peak に対する相対 threshold を併用する。
- 初期値は `max(noise_floor + 12 dB, peak - 40 dB)` 相当を基準にする。

区間検出:

- threshold を超える block を active とする。
- 短い active gap を結合する。
- 短すぎる isolated active を除外する。
- leading silence / trailing silence を落とす。
- pre-roll / post-roll を付ける。
- 境界を zero-cross へ寄せる。

既定値:

| 項目 | 初期値 |
| --- | ---: |
| block size | 1024 samples |
| hop size | 512 samples |
| noise percentile | 10% |
| threshold above noise | +12 dB |
| threshold below peak | -40 dB |
| pre-roll | 50 ms |
| post-roll | 100 ms |
| min active duration | 30 ms |
| gap merge | 80 ms |
| zero-cross radius | 256 samples |

失敗時:

- 全無音なら `No active region detected` を表示して `trim_range` は変更しない。
- 全体が active なら `Already tight` を表示し、必要に応じて全体範囲を候補として表示する。
- confidence が低い場合は `trim_range` へ自動反映せず、preview 候補として表示する。

### Proposed Types

```rust
pub struct AutoTrimConfig {
    pub threshold_above_noise_db: f32,
    pub threshold_below_peak_db: f32,
    pub pre_roll_secs: f32,
    pub post_roll_secs: f32,
    pub min_active_secs: f32,
    pub gap_merge_secs: f32,
    pub zero_cross_radius: usize,
}

pub struct AutoTrimResult {
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
    pub leading_silence_secs: f32,
    pub trailing_silence_secs: f32,
}

pub struct AutoTrimState {
    pub generation: u64,
    pub running: bool,
    pub progress: f32,
    pub message: String,
    pub result: Option<AutoTrimResult>,
}
```

配置候補:

- pure DSP: `src/app/auto_trim.rs`
- app glue: `src/app/auto_trim_ops.rs`
- UI追加: `src/app/ui/editor.rs`
- state: `src/app/types.rs`

## Shared Background Job Policy

Loop Detection、Auto Trim、Recording finalization は UI thread で重い処理をしない。

共通方針:

- `generation` で stale result を捨てる。
- cancel flag を持つ。
- progress は 0.0 から 1.0 の範囲で更新する。
- message は UI と Debug window に出せる文字列にする。
- active tab が閉じられた場合は結果を破棄する。
- 元 audio buffer が変更された場合は結果を破棄する。

将来、既存の機能別 `std::thread::spawn + mpsc + generation` が増えすぎる場合は、以下のような共通 job manager へ寄せる。

```rust
pub struct AnalysisJobState<T> {
    pub kind: AnalysisJobKind,
    pub generation: u64,
    pub running: bool,
    pub progress: f32,
    pub message: String,
    pub result: Option<T>,
}

pub enum AnalysisJobKind {
    LoopDetect,
    AutoTrim,
    RecordingFinalize,
}
```

ただし v1 では既存パターンに合わせ、過度な基盤化で差分を膨らませない。

## Implementation Phases

### Phase 1: Auto Trim

最初に実装する理由:

- 既存 `trim_range` と `VirtualOp::Trim` を再利用できる。
- destructive 操作を自動実行しないため安全。
- テスト用の合成音声を作りやすい。

作業:

- `auto_trim` pure function を作る。
- Trim Inspector に `Auto Trim` UI を追加する。
- worker / progress / cancel を追加する。
- `tab.trim_range` に結果を反映する。
- unit test と kittest を追加する。

### Phase 2: Auto Loop Detection Fast Mode

作業:

- `loop_detect` pure function を作る。
- Loop Edit Inspector に `Auto Detect` UI を追加する。
- 候補一覧と confidence 表示を追加する。
- `tab.loop_region` に候補を仮反映する。
- unit test と Editor integration test を追加する。

### Phase 3: Recording Tab

作業:

- `WorkspaceView::Recording` 相当の workspace を追加する。
- Tools > Recording... を追加する。
- `cpal` microphone capture を実装する。
- temp WAV worker と live waveform overview を実装する。
- 録音停止後に list item / Editor tab へ渡す。
- fake input test と Windows 実機確認を行う。

### Phase 4: Windows WASAPI Loopback

作業:

- Windows system audio capture を追加する。
- Recording tab に System source を有効化する。
- System + Microphone mix を worker 側で実装する。
- buffer drop / resample / channel conversion のテストを行う。

### Phase 5: Deep Loop Detection

作業:

- chroma / beat / SSM 候補を追加する。
- Deep mode UI を追加する。
- 重い解析向けの progress / cancel / ETA を強化する。
- 評価データを増やす。

## Test Plan

### Unit Tests

Loop Detection:

- perfect synthetic loop
- loop with click risk near boundary
- loop with different loudness at candidate seam
- existing loop marker candidate rescoring
- selection candidate rescoring
- too short audio
- silence / noise-only low confidence
- stereo input downmix

Auto Trim:

- leading and trailing silence
- low-level noise floor
- fade-in / fade-out
- short transient
- all silence
- already tight audio
- stereo input with activity on one channel only
- zero-cross snap boundary

Recording:

- fake capture source writes expected sample count
- sample format conversion
- mono to stereo conversion
- system + mic mix with gain
- temp WAV finalize
- dropped buffer accounting
- worker cancel / discard

### Integration / GUI Tests

- Editor で Auto Trim を実行し、`trim_range` が更新される。
- Auto Trim 後に `T` で destructive trim できる。
- Auto Trim 後に `V` で virtual trim item が作られる。
- Loop Edit で Auto Detect を実行し、candidate が表示される。
- Loop Detect candidate の `Use` で `loop_region` が更新される。
- Loop Detect 後に既存 Apply が動く。
- Tools > Recording... で録音 tab が開く。
- 録音中に live waveform と meter が更新される。
- 録音停止後に Editor tab が開く。
- 録音後音声に Inspector、`T`、`V`、`C` が使える。

### Manual Verification

Windows 実機:

- microphone device list が表示される。
- microphone recording ができる。
- WASAPI loopback で PC 内音が録音できる。
- system + microphone mix が破綻しない。
- 録音中に UI が止まらない。
- 10秒、1分、10分の録音で memory と temp file の挙動を確認する。

Loop material:

- 既存 loop marker 付き WAV
- 短尺 BGM
- 長尺 BGM
- ambience
- one-shot SFX
- MP3 / M4A / OGG decode 後の editor buffer

Auto Trim material:

- 前後無音の voice
- BGM with fade tail
- noisy field recording
- one-shot SFX
- all silence

### Debug Metrics

Debug window に記録したい値:

- loop detect elapsed ms
- loop detect candidate count
- loop detect best score
- auto trim elapsed ms
- auto trim detected leading/trailing silence
- recording dropped buffers
- recording worker lag
- recording temp WAV path
- first waveform update latency
- finalization latency

## Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| loop detection が長尺で重い | candidate limit、worker、progress、cancel、Fast/Deep 分離 |
| Low confidence 候補を勝手に適用してしまう | Low confidence は候補表示のみ、commit は既存 Apply |
| Auto Trim が必要な tail を削る | post-roll、confidence、実行前 preview、destructive 直実行禁止 |
| 録音 callback が詰まる | callback は channel push のみに限定 |
| 長尺録音でメモリが膨らむ | temp WAV backing と短尺/長尺分岐 |
| system audio capture が OS 依存 | v1 は Windows loopback、他 OS は明示的に未対応 |
| recorded temp file の寿命が曖昧 | unsaved warning、参照中 file は cleanup しない |
| `src/app/ui/editor.rs` がさらに肥大化 | 可能なら loop_edit / trim UI を小分けにする |

## Acceptance Criteria

Auto Loop Detection:

- UI 操作で Auto Detect job を開始、cancel できる。
- UI は解析中も操作可能。
- 候補一覧が表示される。
- candidate を選ぶと `loop_region` が更新される。
- Apply は既存 Loop Edit 経路で動く。

Recording:

- Tools > Recording... から録音 tab が開く。
- microphone recording が動く。
- Windows で system audio recording が動く。
- 録音中に waveform と meter が更新される。
- stop 後に Editor で開ける。
- 録音後の音声に Inspector と `T` / `V` / `C` が使える。

Auto Trim:

- Auto Trim job を開始、cancel できる。
- 検出結果が `trim_range` に入る。
- `Apply trim`、`Add Trim As Virtual`、`T`、`V`、`C` が既存通り動く。
- all silence や already tight audio で破壊的な変更を勝手に行わない。

## Assumptions

- v1 の system audio capture は Windows を対象にする。
- UI 表記は `Recoding` ではなく `Recording...` にする。
- v1 では ML scorer を実装しない。
- v1 では Auto Trim 専用の virtual op を追加しない。
- `.nwsess` に raw recording audio を埋め込まない。
- 録音後の音声は、短尺では virtual item、長尺では temp WAV backed item として扱う。
- 実装時は既存の user changes を巻き戻さず、`src/app/ui/editor.rs` の肥大化を必要最小限に抑える。
