# UI/Audio Performance Improvement Candidates (2026-06-06)

## 目的

この文書は、現コードを読んだ上で見つけた UI / Audio 周りの高速化余地を、実装に移しやすい単位で列挙するものです。

既存の `docs/PERFORMANCE_SCALABILITY_PLAN.md` は大規模対応の方針文書として残し、本書では以下に絞ります。

- UI スレッドを止める可能性がある同期処理
- MP3 / M4A / OGG など圧縮音源の再生開始と継続再生の安定化
- 既に非同期化されているが、clone・drain・fallback で詰まり得る箇所
- ローディング、プログレス、cancel を足すべき箇所

## 現状の良い土台

- エディタ decode は `src/app/editor_decode_ops.rs` で progressive / streaming decode、部分波形、進捗、cancel が実装済み。
- リストは `egui_extras::TableBuilder::rows` で可視行のみ描画しており、大量件数対応の土台がある。
- メタ情報は `src/app/meta.rs` / `src/app/meta_ops.rs` の worker pool で段階的に処理される。
- Spectrogram / Tempogram / Chromagram は `src/app/editor_viewport.rs` で viewport 画像化し、重い描画を worker に逃がす設計が入っている。
- Top bar と editor overlay には、scan / decode / preview / apply / export / analysis の activity 表示がある。

## 優先度A: MP3/圧縮音源のリスト再生安定化

### 該当箇所

- `src/app/list_preview_ops.rs`
- `src/app/logic.rs`
- `src/audio_io.rs`
- `tests/mp3_preview_timing.rs`

### 問題

`src/audio_io.rs` には `decode_audio_multi_progressive(path, prefix_secs, emit_every_secs, cancel, on_chunk)` があり、MP3 の prefix / full handoff を検証する `tests/mp3_preview_timing.rs` も存在する。

一方で、リストプレビューの `spawn_list_preview_async` / prefetch は `wave::decode_wav_multi` による full decode になっており、関数引数の `max_secs` と `emit_every_secs` が実質使われていない。

`wave::decode_wav_multi` は内部で `audio_io::decode_audio_multi` を呼ぶため圧縮音源も読めるが、現在のリスト経路では progressive decode の prefix 表示・継続 decode・途中 cancel の利点が弱い。MP3 で「選択してから音が出るまで待つ」「full decode 完了前に次の行へ移ると古い decode が走り続ける」体験につながる。

### 改善案

- `spawn_list_preview_async` を `decode_audio_multi_progressive` ベースへ置き換える。
- `max_secs > 0` のときは prefix chunk を即座に `ListPreviewResult { is_final: false }` として流し、再生開始可能にする。
- `emit_every_secs > 0` のときは継続 chunk を受け取り、再生中 buffer を `replace_samples_keep_pos` 系で差し替える。
- stale job は generation だけで結果破棄するのではなく、`AtomicBool` cancel を渡して decode 自体を止める。
- prefetch は 0.35 秒 prefix cache と full cache を区別し、Auto Play 時は短すぎる cache を再利用しない既存方針を維持する。
- MP3 / M4A / OGG は `SrcQuality::Fast` を既定にしつつ、full handoff 後の音質は設定に従えるようにする。

### 検証

- `cargo test --test mp3_preview_timing -- --nocapture`
- 18秒MP3、数分MP3、長尺MP3でリスト選択から初回音声までの時間を確認する。
- prefix再生中に別行へ移動し、古い final chunk が再生・cache上書きされないことを確認する。
- Auto Play 有効時に prefix/full handoff で playhead が戻らないことを確認する。

## 優先度A: UIスレッド上の大きなclone削減

### 該当箇所

- `src/app/spectrogram_jobs.rs`
- `src/app/editor_viewport.rs`
- `src/app/preview.rs`
- `src/app/editor_ops.rs`
- `src/app/logic.rs`

### 問題

重い計算自体は worker に逃がしていても、worker に渡す直前に UI スレッドで巨大な `Vec<Vec<f32>>` を clone している箇所がある。

代表例:

- `queue_spectrogram_for_tab` で `tab.ch_samples.clone()` または mixdown 全生成を行う。
- `build_editor_viewport_request` で可視範囲の `channel[start..end].to_vec()` を UI スレッドで作る。
- tool preview / apply / playback rebuild で `tab.ch_samples.clone()` を多用する。
- preview overlay は全チャンネル full sample を保持し、さらに mixdown も持つ場合がある。

長尺、30ch、多タブ、Undo ありの状態では、clone だけで数十msから数百msの停止や一時メモリ増加が起き得る。

### 改善案

- EditorTab の音声本体を `Arc<AudioBuffer>` または `Vec<Arc<[f32]>>` に寄せ、job には Arc と範囲だけを渡す。
- viewport worker は UI スレッドで slice を `to_vec()` せず、worker 側で必要範囲を読む。
- Spectrogram は「mixdownをUIスレッドで生成して渡す」のではなく、worker に channel view と source Arc を渡して worker 側で mixdownする。
- preview overlay は full sample を最初から持たず、概要用 min/max と詳細用 lazy data を分ける。
- Undo は全音声 clone を既定にせず、短尺は従来、長尺は差分または file-backed snapshot に切り替える。

### 検証

- 長尺 stereo と 30ch fixture で、Spectrogram 切替、zoom / pan、tool preview 開始時の frame peak ms を比較する。
- Debug window の frame last / peak、editor open to shell / partial / final を記録する。
- Windows タスクマネージャまたは Debug 表示で、preview / apply / undo 時の一時メモリ増加を見る。

## 優先度A: 検索・ソートの同期実行対策

### 該当箇所

- `src/app/logic.rs`
- `src/app/search_ops.rs`
- `src/app/meta_ops.rs`

### 問題

`apply_filter_from_search` は UI スレッドで全 item を走査し、必要に応じて `to_lowercase`、regex match、transcript / external / meta summary の文字列生成を行う。

`apply_sort` も UI スレッドで `self.files.sort_by` を実行する。大量件数では O(n log n) の比較中に meta / external / override map 参照と文字列比較が繰り返される。

検索は 300ms debounce されているが、実行自体は同期で、300k件規模や transcript / external 列が多い状態では UI 停止につながる。

### 改善案

- `MediaItem` に検索用 normalized cache を持たせる。
  - display name
  - folder
  - transcript
  - external visible values
  - meta summary
- sort key は `SortKey` ごとに軽量な `SortValue` cache を作り、比較中に文字列生成しない。
- 検索とソートを worker 化し、`SearchSortJob { generation, query, sort_key, sort_dir, source_ids }` の結果を UI で差し替える。
- worker 実行中は現在の表示を維持し、Top bar に `Filtering...` / `Sorting...` と件数または経過秒を表示する。
- scan 中は現状どおり sort を遅延し、scan 完了後の初回 sort だけ worker 化する。

### 検証

- `--dummy-list 300000` 相当で検索入力、クリア、sort key 変更、sort direction 変更を行う。
- frame peak ms が操作中に大きく跳ねないことを確認する。
- 検索中に selection と scroll 位置が破綻しないことを確認する。

## 優先度B: Spectrogram / Feature Viewport の初回描画詰まり

### 該当箇所

- `src/app/spectrogram.rs`
- `src/app/spectrogram_jobs.rs`
- `src/app/editor_viewport.rs`
- `src/app/ui/editor.rs`

### 問題

Spectrogram の計算は tile 化されているが、UI 側の受信処理には次の詰まり候補がある。

- `collect_spectrogram_messages` がそのフレームで受信できる message を全件 drain する。
- 初回 tile 受信時に `vec![-120.0; frames * bins]` を UI スレッドで確保する。
- `Arc::make_mut` 後に大きな `values_db` へ copy する。
- `ui/editor.rs` に cache 未生成時の同期 fallback image 生成が残っている。

Feature viewport は worker 画像化がある一方、初回や設定変更直後は fallback が UI フレーム内で走り得る。Spectrogram / Tempogram / Chromagram の表示切替、zoom / pan 直後の体感に影響する。

### 改善案

- spectrogram message drain に frame budget を設ける。
- `values_db` の大配列確保を worker 側で行い、UI は Arc の差し替えまたは tile map の登録だけにする。
- fallback image 生成は廃止し、直前 cache または軽量 placeholder を表示する。
- coarse viewport がまだない場合は `Building...` と progress bar を出し、同期描画しない。
- tile cache は `Vec<f32>` の巨大な単一配列ではなく、tile単位で保持して viewport worker が必要範囲だけ読む形を検討する。

### 検証

- 長尺ファイルで Spectrogram / Log / Mel を切り替え、初回表示の frame peak ms を確認する。
- spectrogram cfg 変更、vertical zoom、horizontal pan を連続操作して UI が固まらないことを確認する。
- tile受信中に別タブへ切り替えて、古い tile が表示・cache復活しないことを確認する。

## 優先度B: ジョブ管理の統一

### 該当箇所

- `src/app/frame_ops.rs`
- `src/app/loading_ops.rs`
- `src/app/preview.rs`
- `src/app/editor_ops.rs`
- `src/app/plugin_ops.rs`
- `src/app/music_ai_ops.rs`
- `src/app/transcript_ai_ops.rs`
- `src/app/spectrogram_jobs.rs`

### 問題

現状は機能ごとに `std::thread::spawn + mpsc + generation` の形が分散している。

この形は実装しやすいが、以下の問題が残る。

- 古い job は結果破棄されても、計算自体は止まらない箇所がある。
- 短時間の zoom / pan / preview parameter 変更で thread が増えやすい。
- progress / cancel / Top bar 表示の粒度が機能ごとに違う。
- drain に frame budget がある箇所とない箇所が混在する。

### 改善案

- 軽量な `JobManager` を導入する。
  - `JobId`
  - `JobKind`
  - `JobState { started_at, progress, message, cancel, priority }`
  - `JobResult`
- CPU系 job は worker pool に投入する。
- 最新のみ必要な job は enqueue 時に同種の古い job を cancel する。
- UI は Top bar / editor overlay / list loading で同じ JobState を読む。
- drain はすべて frame budget を持ち、未処理 message が残る場合だけ repaint を要求する。

### 検証

- preview parameter を連続変更し、古い job が完走までCPUを使い続けないことを確認する。
- Spectrogram zoom / pan を連続操作し、thread 数が増え続けないことを確認する。
- Top bar の表示が `Working...` で止まらず、progress または elapsed を出すことを確認する。

## 優先度B: List UIフレーム内処理の削減

### 該当箇所

- `src/app/ui/list.rs`
- `src/app/ui/list/table.rs`
- `src/app/list_state_ops.rs`
- `src/app/meta_ops.rs`

### 問題

List は仮想化されているが、可視行の描画中に以下を行っている。

- `PathBuf`、display name、folder、item の clone
- `path.is_file()` による存在確認
- 行ごとの meta / transcript queue 投入判断
- wave thumb の全 bin line 描画
- `resolve_list_wave_overlay_info` で tab / edited cache / meta を探索

通常件数では問題になりにくいが、列数が多い、cover art / wave / external / transcript を表示、ネットワークドライブ、遅いディスク、大量スクロールの条件で frame time を押し上げる。

### 改善案

- 可視行用 `ListRowViewModel` を作り、表示文字列、色、badge、wave overlay summary を cache する。
- `is_file()` は UI フレームで直接呼ばず、scan / meta worker / low priority existence checker で状態化する。
- meta queue 投入は描画中ではなく、visible range の差分更新として frame 前後でまとめる。
- wave thumb 描画は列幅に応じて最大描画本数を固定し、thumb がそれ以上なら間引く。
- `list_header_dirty` は全 items 走査を避け、dirty counter / pending gain counter で判定する。

### 検証

- 8,000件、100,000件、300,000件でスクロール時の frame peak ms を比較する。
- wave / transcript / external / cover art 列を順に有効化し、どの列で悪化するか記録する。
- missing file を含むリストで、存在確認がUIを止めないことを確認する。

## 優先度C: Apply/Preview/Undo のメモリ・待ち時間

### 該当箇所

- `src/app/editor_ops.rs`
- `src/app/preview.rs`
- `src/app/logic.rs`
- `src/app/types.rs`

### 問題

Editor apply と preview は非同期化されている箇所が多いが、完了反映時や準備時に重い処理が残る。

- apply完了後に UI スレッドで `build_editor_waveform_cache` を実行する箇所がある。
- Undo は `EditorUndoState` に `ch_samples` 全体を持つ。
- preview overlay は full sample 全チャンネルと mixdown を保持し得る。
- preview/apply worker に渡す前の `ch_samples.clone()` が長尺で重い。

### 改善案

- apply worker 側で waveform cache まで生成し、UI は結果差し替えだけにする。
- apply progress を channel単位または block単位で送る。
- Undo は短尺は現状維持、長尺は範囲差分または temp file snapshot に切り替える。
- preview overlay は概要 min/max を先に表示し、詳細波形は viewport要求時に遅延生成する。
- Loudness / normalize / fade など軽く見える処理も、長尺閾値を超えたら worker + progress に統一する。

### 検証

- 長尺ファイルで PitchShift / TimeStretch / Loudness apply を実行し、apply完了瞬間の frame peak ms を確認する。
- Undo / Redo を連続実行し、メモリ増加と復帰時間を確認する。
- Preview overlay 表示中に zoom / pan しても追加停止がないことを確認する。

## 実装順序案

1. リストプレビューを `decode_audio_multi_progressive` に統一し、MP3再生開始とcancelを安定化する。
2. 検索・ソートの cache 化を入れ、worker化前でも比較中の文字列生成を減らす。
3. Spectrogram / viewport の同期 fallback を消し、placeholder + background render に統一する。
4. UIスレッド上の大きな audio clone を Arc ベースへ段階移行する。
5. JobManager / worker pool を導入し、preview / viewport / spectrogram から順に載せ替える。
6. Undo / preview overlay のメモリ構造を長尺対応へ切り替える。

## 検証シナリオ

- MP3 18秒 / 数分 / 長尺MP3で、リスト選択から初回音声までの時間、prefix/full handoff、cancelを確認する。
- `--dummy-list 300000` 相当で、検索、ソート、スクロール、選択の frame time を確認する。
- 長尺 / 多チャンネル音声で、エディタ open、Spectrogram 切替、zoom / pan、preview / apply 中のUI応答を確認する。
- Debug window の frame ms、decode progress gap、first audio latency、editor open to partial / final を記録する。

## 非対象

- 本書作成時点ではコード変更、公開API変更、CLI変更、保存形式変更は行わない。
- VST3 / CLAP ホスト安定化は別文書の範囲とする。
- 音質を落とす最適化は、MP3 prefix など明確に暫定表示・暫定再生と分かる箇所以外では採用しない。
