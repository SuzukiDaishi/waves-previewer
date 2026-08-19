# 低スペックPCの「応答なし」/ UIフリーズ対策 (2026-08-19)

`docs/UI_PERFORMANCE_IMPROVEMENT_CANDIDATES_20260606.md` の続編。前者が候補の
列挙だったのに対し、本書は「低スペックPCで起動時・セッションを開いた時に
Windows が『応答なし』を出す」という具体的な報告に対して実施した対策の記録。

## 診断

UI スレッドが 5 秒以上メッセージポンプを回さないと Windows は「応答なし」を
出す。実測で以下が判明した。

1. **セッションを開く処理が1フレームで全部走っていた**（最大要因）。
   `tick_project_open()` は「Opening session...」を1フレーム描いた直後に、約
   1000 行の復元処理を次の1フレームで完走していた。内訳:
   - パス修復が**参照ファイル全件に `exists()`**
   - リスト再構築が**全行に `is_file()`**
   - virtual item / cached edit / tab sidecar / preview overlay の
     **件数分の decode + resample + 波形ピラミッド生成**
   - 旧リストのインライン解放
2. **list の sort / filter が 5 万件まで完全同期**だった。開発機基準の固定
   閾値で、2コア機では数秒。
3. **フレーム全体の予算が無かった**。個別予算を持つ drain はあったが合計を
   抑えるものが無く、複数が同時に着地すると加算された。
4. **アイドル時も 80ms ごとに再描画し続けていた**。ウィンドウが完全に眠る
   ことが無く、弱い iGPU では常時 CPU を消費してワーカーと奪い合っていた。
5. セッションを閉じる時の autosave、起動時のシステムフォント読み込み、行の
   存在確認が同期のまま残っていた。

## 対策

### 1. マシン性能ティア (`src/app/perf_profile.rs`)

コア数から Low / Normal / High を決め、**UI スレッドの予算をすべてここから
導出**する。固定定数をやめたのが要点。

| | Low (≦2コア) | Normal | High (≧8コア) |
|---|---|---|---|
| フレーム予算 | 4ms | 8ms | 12ms |
| sort/filter 同期閾値 | 2,000 | 20,000 | 50,000 |
| list job スライス | 1.0ms | 2.0ms | 3.0ms |
| 復元同時実行 | 1 | cores-1 (≦3) | cores-1 (≦4) |
| meta pool | 1 | cores-1 (≦4) | cores-1 (≦6) |

- フレームが継続して遅い場合は自動で1段下げる。**戻しはしない**（数フレーム
  速いだけで方針が往復すると編集中に挙動が揺れるため）。
- Settings > Performance > Responsiveness で手動固定できる（`perf_tier` として
  prefs に保存）。VM・リモートデスクトップ・他が重い環境など、コア数から
  読めないケース用。

### 2. フレーム全体の予算 (`src/app/frame_budget.rs`)

`run_frame_pre_ui` の約60個の drain のうち、**ユーザーが同期的に待っていない
もの**（meta、metadata summary、spectrogram、viewport、external load、
inspection、duplicate、transcript/music AI、folder watch、LUFS recalc）を
共有デッドラインでガードする。予算切れなら次フレームに持ち越し、repaint を
要求する。

**ガードしないもの**（遅延に直結する）: 再生同期、audio device 復帰、IPC、
入力・ショートカット、editor decode/apply の完了反映、export。

無予算だった drain にも上限を入れた: editor feature 解析と editor 波形キャッシュ
（1件ごとに全チャンネル波形＋ピラミッドを複製する）は2件/フレーム、folder
watch イベントは512件/フレーム。

### 3. セッション読み込みの段階化 (`src/app/session_ops.rs`)

`open_project_file` を3段階に分けた。

1. **Parse**（ワーカー）— 読み込み・デシリアライズ・バージョン確認・パス修復。
   app state に触らないので丸ごとワーカーへ出せる。ついでに**行ごとの存在
   マップ**もここで作り、リスト再構築側の `is_file()` を消した。
2. **Decode**（ワーカー、同時実行数はティア依存）— sidecar と virtual source
   を全部先に decode する。各呼び出し箇所は元のインライン decode を
   フォールバックとして残してあるので、収集漏れ・decode 失敗・同一参照の
   2回目は従来どおりの挙動になる。バッファは clone ではなく take するので
   ピークメモリは増えない。
3. **Apply**（UI スレッド）— 復元済みの音声を引き当てながら state に反映。

- `open_project_file` は CLI / kittest / 単体テスト用にブロッキングのまま残置。
- 世代番号を持ち、復元中に別セッションを開いたら旧ワーカーの結果は破棄する。
- トップバーにフェーズ名と Cancel を出す。
- 復元中は**読み取り専用**: スクロール・選択・再生は可、保存と apply は拒否。

### 4. アイドル時に眠る (`src/app/frame_ops.rs`)

やることが本当に何も無いときは `request_repaint_after` を呼ばない。ポーリング
していたチャンネルのうち、外部から入ってくる2つは**スレッド側から起こす**
方式に変えた（`src/ui_wake.rs`）:

- IPC listener（2つ目のインスタンスがファイルを転送してくる）
- folder watch

出力ストリームがある間だけ 1Hz のハートビートを残す（デバイス抜けの検出は
このループが回らないと進まないため）。再生中は従来どおり 16ms。

### 5. 残りの同期 I/O

- **Session Close の autosave**: 編集タブ・virtual item ごとに WAV を同期書き
  出ししていた。既存の非同期保存に載せ替え、書き終わってからセッションを
  破棄する。保存に失敗した場合はセッションを開いたままにする（従来どおり）。
- **システムフォント**: 起動時に数MBの CJK フォントを最初のフレームの前に
  読んでいた。埋め込みの NotoSansJP だけでウィンドウを出し、システムフォント
  はワーカーで読んで差し替える。
- **行の存在確認**: TTL 2秒だが可視行が一斉に期限切れするため、1フレーム
  8件までに制限した。

### 6. 長フレームの記録 (`src/app/frame_ops.rs`, `debug_ops.rs`)

250ms を超えたフレームを、そのとき動いていたもの（session-open / scan / sort /
editor-decode / meta / spectrogram / 予算持ち越しの有無）とセットで直近16件
記録し、Debug ウィンドウ（F12）に現在のティアと並べて表示する。

**「自分の環境で固まる」という報告を、再現やプロファイラ無しで切り分ける**
ための情報源。報告を受けたら F12 の `long_frames` と `perf_tier` を見る。

## 検証

- `cargo test --lib` / `cargo test --features kittest`
- 新規テスト:
  - `perf_profile`: ティア判定、ワーカー数がコア数を超えないこと、降格が
    片方向であること、pin したティアが降格しないこと、prefs 往復
  - `frame_budget`: 予算切れのラッチ、リセット、持ち越し件数
  - `session_ops`: 段階復元と一括復元の結果一致、prefetch 有無での一致、
    存在確認がワーカーに移っても欠損ファイルが検出されること、キャンセル
- 手動:
  1. 数百ファイル＋編集タブ複数の `.nwsess` を `--open-session` で起動し、
     復元中にウィンドウ移動・スクロール・Cancel ができること
  2. `--dummy-list 30000` でソート/検索/スクロールし、F12 の frame peak を比較
  3. 無操作時のタスクマネージャ CPU がほぼ 0 になること
  4. Settings で Low 固定にして 1〜3 を再実行

## 積み残し

- Apply 段階の resample・`build_meta_from_audio`・`build_editor_waveform_cache`
  は UI スレッドに残っている。decode を外に出したことで支配的ではなくなったが、
  極端に長いセッションではここをフレームまたぎでスライスする余地がある。
- リスト構築自体（`reset_list_from_project` の行生成ループ）はまだ1フレーム。
  存在確認を外したので大幅に軽くなったが、数十万件では分割の価値がある。
