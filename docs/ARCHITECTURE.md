# アーキテクチャ概要

本プロジェクトは「ファイル一覧 → クリックでエディタタブ」の最短導線と、軽快な再生/表示を目的とした構成です。

- `src/main.rs`
  - 最小のエントリポイント。`app::WavesPreviewer` を起動するだけ。
- `src/app/`
  - `app.rs`: egui アプリ本体。タブUI、リスト表示、エディタ表示、ショートカット、非同期メタ情報の受信などを担当。
  - `types.rs`: App 内部の共有型（`EditorTab`/`FileMeta`/`RateMode`/`SortKey` など）。
  - `helpers.rs`: dB/色変換、ヘッダソート、フォーマット、OS 連携ヘルパ。
  - `meta.rs`: メタ情報生成のバックグラウンドワーカー（RMS/サムネ）。
  - `logic.rs`: 走査/検索/ソート/D&D マージ/重処理スレッド起動などの非 UI ロジック。
  - リストは `egui_extras::TableBuilder` を直接使用し、内部スクロール（vscroll）＋仮想化（`TableBody::rows`）で可視行のみを描画。
  - 列のリサイズ機能（`.resizable(true)`）、長いテキストの自動切り詰め（`.truncate(true)`）、ホバーツールチップ対応。
  - `min_scrolled_height(...)` を用い、テーブルのボディが残り高さを使い切る（下端まで枠が伸びる）。
  - 行全体のクリックを受け取るため、`TableBuilder.sense(Sense::click())` と `row.response()` を使用（セルごとに `interact` は不要）。
- `src/audio.rs`
  - 再生エンジン（CPAL）と共有状態（ロックフリー）。音量、再生位置、メータ、ループ再生、再生速度の制御を内包。
  - 再生速度: `rate (0.25..4.0)` と `play_pos_f`（小数位置）を持ち、コールバックで線形補間して出力。
- `src/wave.rs`
  - デコード/リサンプル/波形(min/max)作成。`prepare_for_playback` で「再生準備＋波形作成」を一括で実行。
  - PitchShift/TimeStretch では `signalsmith-stretch` を用いたオフライン処理を実施。出力レイテンシ（`output_latency()`）と `flush()` 分を考慮して末尾欠けを防止。

## データフロー

1) リストでファイル（行）をクリック
2) `wave::prepare_for_playback()` が WAV をモノ化→（必要なら）簡易リサンプルして `AudioEngine::set_samples()` に渡す
3) `AudioEngine` は共有バッファ（`ArcSwapOption<Vec<f32>>`）を差し替え、再生位置を 0 に戻す
4) UI は別途、モノ元データから min/max 波形を作成してエディタに描画
5) コールバック内で短時間 RMS を計算→UI に dBFS 表示。再生速度は `rate` に応じて小数ステップで進める
6) PitchShift/TimeStretch の場合は別スレッドで処理→完了通知を受けて再生バッファと波形を差し替え（UI は処理中オーバーレイを表示）

## エディタのインタラクション（実装済み）
- クリック/ドラッグでシーク（スクラブ）。再生状態は維持。
- Ctrl + マウスホイールで時間ズーム（カーソル位置を固定点として再中心化）。
- Shift + ホイール（または横ホイール）で水平パン。
  - 備考: ズーム/パンは現状試験実装で、環境により挙動が不安定な場合があります（詳細は `docs/KNOWN_ISSUES.md`）。

## スレッドと共有状態

- CPAL の出力コールバックでのみ音声が消費される。
- 共有状態は `SharedAudio` に集約し、すべて `Atomic*` と `ArcSwapOption` でロックレス。
- UI スレッドとのやり取りは値の読み書きのみ。ミューテックス/動的確保をコールバックで行わない。

```
[UI] ── prepare_for_playback() ──> [AudioEngine::set_samples]
  │                                     │
  │                                     └── [SharedAudio.samples] ← ArcSwapOption
  └── request_repaint_after(16ms)

[Audio Callback] ── 共有バッファを参照・消費／RMS計測
```

## ループ再生

- 共有状態に `loop_enabled`, `loop_start`, `loop_end` を持つ。
- コールバックで `pos >= loop_end` のときに即 `loop_start` へ巻き戻す（1サンプルも空けない）。
- 現在は UI から全範囲ループのみ（将来は範囲選択を UI で指定）。

## 非同期メタ情報（2段階）

- Stage 1（即時）: WAV ヘッダのみ読取り→`channels`/`sample_rate`/`bits_per_sample` を送信（可視行は UI からも即時反映可能）。
- Stage 2（後追い）: モノ化デコード→RMS(dBFS)/128bin サムネを作成して上書き送信。
- UI は逐次受信し、行ごとに更新＆再描画。未計算の値は `...` で表示。

## コールバック内の禁止事項（リアルタイム安全）

- 動的確保（Vec::push/extend など）
- ロック取得（Mutex/RwLock）
- 重い処理（I/O、ファイルアクセス、ログスパム等）

---

## Heavy Processing（Pitch/Stretch）

- `app` 側で重い処理専用のワーカー（std::thread）を起動し、`mpsc::channel` で結果（処理済み samples と waveform）を受信。
- UI は `processing` ステートが Some の間、全画面カバー（前景レイヤ）で入力をブロックし、スピナー＋メッセージを表示。
- 受信後は `AudioEngine::set_samples` で差し替え、波形を更新、必要ならループ領域を全体に設定。

---

# モジュール詳細

## audio
- `AudioEngine::new()` でデフォルト出力デバイス/設定を開く。
- `build_stream<T>()` で出力ストリームを構築（`SizedSample + FromSample<f32>`）。
- コールバックはモノラル→全出力チャンネルへ複製し、`vol` を乗算、[-1,1] でクランプ。
- RMS を逐次計算して `meter_rms` に保存。
- ループ再生は `loop_enabled` と `loop_start/end` で制御。

## wave
- `decode_wav_mono()`：WAV をチャンネル平均でモノ化（Int の場合は bit depth で正規化）。
- `resample_linear()`：出力 SR へ簡易線形リサンプル。
- `build_minmax()`：固定ビンの min/max 計算。
- `prepare_for_playback()`：上記をまとめて呼び、再生準備＋波形作成。

## app
- リスト: `egui_extras::TableBuilder`（内部スクロール）。`min_scrolled_height` で最下部まで枠を延ばし、`TableBody::rows` で仮想化。
  - 列構成: File | Folder | Length | Ch | SR | Bits | Level(dBFS) | Wave
  - 全列がリサイズ可能（`.resizable(true)`）で初期幅を最適化済み
  - 長いテキスト（ファイル名・フォルダパス）は `.truncate(true)` で自動切り詰め、ホバーで全文表示
  - Wave 列は幅に応じてサムネ高さを比例拡大（1フレーム遅延で行高反映）
- エディタ: 横幅に比例して縦も拡大（`wave_h = width * 0.35` を余白に収まる範囲で使用）。
- 色分け: サムネは振幅（max(|min|,|max|)）を青→赤のグラデーションへ。dBFS 列は dB 値で着色。
- ショートカット: Space（再生/停止）、L（ループトグル。エディタ時）、↑/↓（選択）、Enter（タブを開く）。
- 検索/ソート: 検索バーで部分一致フィルタ。ソートは昇順→降順→元順をトグル（Length列は秒数順）。元順復帰のため `original_files` を保持。
- スクロール: 選択行は `row.response().scroll_to_me()` で常に可視範囲へ。
- 上部バー: Mode はセグメント切替（Speed/Pitch/Stretch）。値はコンパクトな DragValue（Speed/Stretch: 0.25–4.0, Pitch: -12–+12）。高さ・間隔を統一し横幅を節約。

## パフォーマンス設計（大規模リスト）
- リストは仮想化（可視範囲のみ）で O(可視行) の描画に抑制。
- データ側はバックグラウンドワーカーでメタ（RMS/サムネ）を逐次生成。UIは受信ごとに最小限の再描画。
- 毎フレームの `clone()` を避け、`iter()` で参照走査して割当てを削減。

---

# パフォーマンス設計
- UI は 16ms 間隔で `request_repaint_after()`、CPU を占有しない程度に滑らかさを確保。
- コールバックは O(1) 作業（ゲイン、補間、複製、RMS）に限定。補間は線形、追加の割当てなし。
- サムネ/RMS はバックグラウンドで逐次計算（UI は受信次第更新）。
