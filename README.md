# waves-previewer (WavesViewer)

外部フォルダを再帰的に走査して `.wav` を一覧表示し、Space で即試聴、波形(min/max)と dBFS メータを表示する Rust 製の軽量プレビュワーです。GUI は `eframe/egui`、オーディオ出力は `cpal` を使用しています。リストは大規模でも軽快に動作し、検索・ソート・即時プレビューに最適化しています。

現状は WAV のみ対応（`hound`）。今後 `symphonia` による mp3/ogg/flac/aac 対応を予定しています。

---

## デザイン方針（ご提案）

基本画面は「リスト表示」。項目をクリックすると「波形エディタ」を開きます。

どちらの表示方法がよいか検討中ですが、初期実装は「同一ウィンドウ内タブ」を採用します（実装容易・一体感・ショートカットが素直）。将来的にポップアウト（別ウィンドウ）も選べる設計にします。

- タブ方式（既定）: 1 ウィンドウ内で複数エディタをタブ切替。複数ファイル比較が楽。マルチモニタ利用時は後述のポップアウトで補完。
- 別ウィンドウ方式（将来オプション）: エディタを新規ウィンドウへ分離。マルチモニタで並べられる一方、ウィンドウ管理の複雑さが増します。

### モック（参考）

リスト画面（一覧）

![list](docs/要件定義_リスト.png)

波形エディタ（詳細）

![editor](docs/要件定義_波形エディタ.png)

---

## 機能

- フォルダ選択（再帰走査）で `.wav` を一覧表示（上部バーに総数を表示、読み込み中は ⏳ 表示）
- 検索バーでファイル名/フォルダを部分一致フィルタ（表示数/総数を表示）
- クリックでタブに波形エディタを開く（同一ウィンドウ内）
- Space/ボタンで再生・停止、音量スライダ、dBFS メータ表示
- モード選択（Mode: Speed / PitchShift / TimeStretch）
  - Speed: 再生速度（Speed x [0.25〜4.0]）。ピッチは変化（非保持）。リアルタイム再生で低遅延。
  - PitchShift: セミトーン（-12〜+12）でピッチのみ変更。長さは保持。signalsmith-stretch によるオフライン処理。
  - TimeStretch: 伸縮倍率（0.25〜4.0）で長さを変更。ピッチは保持。signalsmith-stretch によるオフライン処理。
  - Pitch/Stretch は処理が重い場合があるため、実行中は画面全体にローディングカバーを表示して完了後に自動反映（他の重い処理にも使い回し可能な共通オーバーレイ）。
  - UI は「セグメント化された Mode 切替 + 小型の数値ステッパ（DragValue）」で統一。文字高さを揃え、横幅占有を最小化。
- リスト列: File | Folder | Length | Ch | SR | Bits | Level(dBFS) | Wave
  - 各列はリサイズ可能で、初期幅は最適化済み
  - 長いテキスト（ファイル名・フォルダパス）は自動切り詰め（...）表示、ホバーで全文表示
  - Ch/SR/Bits/Length は可視行表示時に即ヘッダ情報を読んで反映（高速）
  - Level/Wave はバックグラウンドで逐次計算して上書き（非同期）
- ソート: ヘッダクリックで「昇順→降順→元の順」をトグル（文字列はUTF順、数値は大小順、Length列は秒数順）
- 行のどこでもクリックで選択＋音声ロード、ファイル名クリックでタブを開く、フォルダ名クリックでOSのファイルブラウザを開く
- 選択行は自動で見える位置へスクロール
- 波形は min/max の簡易描画（将来ズーム/パン/シーク対応）
- **再生方式**:
  - **リスト表示時**: 常にループ無効（一度再生で停止、試聴に最適）
  - **エディタ表示時**: ループ再生のオン/オフ切替可能（無音ギャップなしのシームレスループ）
  - Pitch/Stretch のときはアルゴリズムの出力レイテンシと残り出力（flush）を考慮して末尾が切れないよう調整。ループ継ぎ目の引っかかりを低減。

将来（ロードマップ）

- タブの「ポップアウト」＝別ウィンドウ化（マルチウィンドウ）
- ズーム/パン、シークバー、A–B ループ、波形サムネイル列、色による大まかな音量表現
- 多形式（mp3/ogg/flac/aac）と高品質リサンプル
- 出力デバイス選択、タグ/メタ表示、スペクトル表示

---

## 画面構成

- 上部バー: フォルダ選択、総数表示、音量、モード選択（Speed/Pitch/Stretch）、検索バー、dBFS メータ、再生ボタン（Space）
- リスト画面: File | Folder | Length | Ch | SR | Bits | Level(dBFS) | Wave
  - 列リサイズ可能、長いテキストは自動切り詰め＋ホバー表示、仮想化スクロール対応
- 波形エディタ（タブ）: フル波形、垂直プレイヘッド、グリッド線、ループトグル

動作イメージ

1) 起動するとリスト画面。フォルダ選択→一覧が埋まる。
2) 項目クリック→選択＋音声ロード。ファイル名クリック→タブに波形エディタを開く（既存タブがあれば右側に追加）。
3) フォルダ名クリック→OS のファイルブラウザでフォルダを開く。
3) Space で再生/停止。再生中はプレイヘッドが移動。上部の dBFS メータが反映される。

---

## 使い方 / ビルド

要件: Rust stable、オーディオ出力が有効な Windows/macOS/Linux。
PitchShift/TimeStretch（signalsmith-stretch）を使うには C/C++ ツールチェーンと libclang が必要です。

```bash
cargo run
```

起動後、左上の「Choose Folder」からフォルダを選択。リストをクリックしてタブで開きます。Space で再生/停止、音量スライダと Speed 入力で調整、検索バーで絞り込み。

`wgpu` で動作しない環境では、`eframe` の feature を `glow` に変更してビルドしてください。

### Windows（signalsmith-stretch を使う場合）
- LLVM をインストール（libclang を含む）
  - winget: `winget install -e --id LLVM.LLVM`
- 環境変数を設定（PowerShell）
  - 一時: `$Env:LIBCLANG_PATH = 'C:\\Program Files\\LLVM\\bin\\libclang.dll'`
  - 併せて: `$Env:CLANG_PATH = 'C:\\Program Files\\LLVM\\bin\\clang.exe'`
- 必要に応じて MSVC C++ Build Tools（Windows SDK 含む）を導入
  - `winget install -e --id Microsoft.VisualStudio.2022.BuildTools`

macOS/Linux の例:
- macOS: `brew install llvm` → `export LIBCLANG_PATH="$(brew --prefix)/opt/llvm/lib"`
- Ubuntu: `sudo apt-get install llvm-dev libclang-dev clang` → `export LIBCLANG_PATH=/usr/lib/llvm-XX/lib`

---

## 実装メモ（要点）

- 出力ストリームは CPAL で常時起動。ロックフリー共有状態（`ArcSwapOption<Vec<f32>>` と `Atomic*`）にバッファ/再生位置/音量/RMS/ループ領域を保持。
- WAV は `hound` で読み込み、モノラル化して簡易リサンプル（線形）。
- 波形表示は固定ビンの min/max を事前計算して描画。
- タブ UI は `egui` のタブ/コンテナで実装。将来 `egui` のマルチビューポートでポップアウト対応予定。
- 視覚: ダークで落ち着いた配色（背景 #121214 付近、アクセントは寒色系）。日本語フォント（Meiryo/Yu Gothic/MSGothic 等）を OS から動的読み込み（Windows）。
- スムーズな再描画: 60fps 目安で `request_repaint_after(16ms)` を使用。

### モジュール構成

- `src/audio.rs`: 再生エンジン（CPAL）と共有状態。シームレスループ/音量/メータ/再生速度（線形補間）。
- `src/wave.rs`: デコード・リサンプル・波形(min/max)作成と準備ヘルパ。Pitch/Pace 用に `signalsmith-stretch` を使用したオフライン処理（出力レイテンシ/flush を考慮）。
- `src/app.rs`: egui アプリ（タブUI、検索/ソート、リスト/エディタ、非同期メタ計算、可視行即時メタ反映）。
-  重い処理（Pitch/Stretch 等）は別スレッドで実行し、UI は全画面ローディングオーバーレイで入力をブロック。完了時に結果（波形/バッファ）を適用。

編集機能の仕様は `docs/EDITING.md` を参照してください。
- `src/main.rs`: エントリポイント。

---

## トラブルシューティング

- No default output device: OS 側で有効な出力デバイスを設定
- Unsupported sample format: 現在は `f32` 出力前提。必要に応じて変換を挟む
- GUI が起動しない: `wgpu` → `glow` へ切替を検討

---

## ロードマップ / Next

- Speed ラベルのプルダウン化（Speed / PitchShift / TimeStretch のモード選択）
- リストの dBFS 値を編集可能にして元ファイルへ反映（ノーマライズ/ゲイン適用）
- エディタ機能の拡充（トリム、ループマーカー、音量、前後クロスフェード、フェード）
- エディタの波形をチャンネルごとに分割表示
- エディタにスペクトログラム / メルスペクトログラム表示を追加
- エディタのズーム機能、シークバーのクリック移動
- 多形式（mp3/ogg/flac/aac）と高品質リサンプル（`symphonia` 予定）
- 出力デバイス選択、タグ/メタ表示

---

## 貢献

- `rustfmt` / `clippy`（`cargo clippy -- -D warnings`）
- 小さな PR 歓迎。再現手順と動作確認を明記してください

---

## ライセンス / クレジット

TBD（MIT / Apache-2.0 を想定）。各ライブラリの著作権はそれぞれのプロジェクトに帰属します。

---

## FAQ

- ショートカット: 現状 Space のみ。順次追加予定
- WAV 以外: 今は非対応。`symphonia` 組み込み後に拡張

---

Maintainers: 初期設計 @you（ハンドオフ済）。引き継ぎメンバーは追記してください。
