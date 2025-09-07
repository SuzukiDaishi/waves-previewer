# UX / デザインメモ

## 基本方針
- 一覧→クリック→即試聴（Space）→必要ならタブで詳細、の最短導線。
- ダーク基調 + 寒色アクセント。コントラストは高め、罫線は控えめ。
- 小画面でも崩れない：上部ツールバーは折り返し（`horizontal_wrapped`）。

## リスト
- `TableBuilder` を直接使用（内部スクロール）。`min_scrolled_height` でテーブルの枠を最下部まで伸ばす。
- 行は仮想化（`TableBody::rows`）で可視範囲のみ描画。1万〜3万件規模でも軽快。
- 列はリサイズ可。Wave 列を広げると、サムネの縦サイズも幅に比例して大きくなる。
  - `thumb_h ≒ available_width * 0.22` を目安。
  - 行高は 1 フレーム遅延で更新（先頭行の計測値を `wave_row_h` に保存）。
- dBFS(Peak) と LUFS(I) は背景を値で着色（静音=黒 → 深青 → 青 → シアン/緑 → 黄 → 橙 → 赤）。
- 列: File | Folder | Length | Ch | SR | Bits | dBFS (Peak) | LUFS (I) | Gain(dB) | Wave。
- ヘッダクリックで並び替え（文字列はUTF順、数値は大小順）。同じヘッダを再クリックで「昇順→降順→元の順」の三段階トグル。
- 上部バー: 総数表示（例: `Files: 123`／読み込み中は `⏳` 表示）、音量、Mode（Speed/Pitch/Stretch）セグメント、各モードの数値（小型ステッパ: DragValue）、検索バー、dBFS メータ、再生ボタン、未保存ゲイン件数（Unsaved Gains: N）。
  - Speed/Stretch: 0.25〜4.0 の倍率、2桁小数、`x` サフィックス。
  - Pitch: -12.0〜+12.0 st、1桁小数、`st` サフィックス。
  - 入力中の自動上書きを避け、確定値のみ反映。全コントロールの高さを統一し、横幅占有を最小化。
- クリック動作:
  - 行背景: 選択（同時に音声ロード）。
  - ファイル名セル（ダブルクリック）: エディタタブで開く（同時に選択）。
  - フォルダセル（ダブルクリック）: OS のファイルブラウザで、そのWAVを選択状態で開く（同時に選択）。
  - Gain 列（DragValue）: dB を直接編集可能。複数選択中は対象行での変更量が選択全体に反映（相対調整）
- ↑/↓：選択移動、Enter：タブを開く。

インジケータ
- 未保存のゲインがある行は、ファイル名末尾に " •" を表示。
- 上部バーに "Unsaved Gains: N" を表示。

## エディタ
- 大波形は `wave_h = clamp(width * 0.35, 180, available_height)`。
- グリッドは 5 分割の水平線、波形は縦棒(min/max)で高速描画。
- ループ：ボタンまたは L キーで On/Off（現状は全範囲）。Pitch/Stretch の処理後は出力レイテンシ/flush を考慮して末尾欠けを防止し、継ぎ目の引っかかりを低減。
- プレイヘッドは 60fps 目安で更新（`request_repaint_after(16ms)`）。

## 編集 UI（計画）
- 目的: 非破壊編集をエディタタブ内で行えるようにする。
- レイアウト案:
  - トップバー直下に編集タブ（例: Waveform / Spectrogram / Mel / WORLD）。
  - その下に編集コントロール（トリム、ループマーカー、フェード、クロスフェード等）。
  - さらに下に可視化ペインを縦に積む（波形、スペクトログラム、メルスペクトログラム、WORLD: F0/包絡）。
  - すべて同じ時間軸とプレイヘッドを共有。ズーム/シークは同期。
- 初期スコープ:
  - 波形: トリム、フェード、クロスフェード、ループマーカー（ループ境界クロスフェード）。
  - スペクトログラム: 選択ノイズ除去、周波数方向ワープ。
  - メル: 閲覧のみ。
  - WORLD: F0 サンプルレベル編集、包絡ワープ。

## ローディング / 重い処理の扱い
- PitchShift/TimeStretch はファイルサイズやパラメータにより時間がかかるため、処理中は画面全体にカバー（半透明）＋スピナーを表示して入力をブロック。
- 処理完了後、波形と再生バッファを差し替え、自動でループ範囲を全体に再設定。

## 色設計
- サムネ/エディタの各棒の色は振幅 `a = max(|min|,|max|)` を 0..1 に正規化し、
  - `lerp(Blue(80,200,255), Red(255,70,70), a^0.6)` で補間。
- dBFS 列は `-80..+6 dBFS` を、黒→青→シアン→赤のトーンで補間。

## フォント
- Windows では Meiryo / Yu Gothic / MS Gothic を動的読み込み（なければデフォルト）。
- 非 UTF-8 が混入した場合は ASCII 表示へフォールバック。
# UX / チェックインメモ

## Editor 2.0 — Multichannel/Zoom/Seek (spec)

This section documents the planned UX for the editor view update. Goals: read‑at‑a‑glance waveform per channel, intuitive seek/zoom, and minimal clutter.

- Layout: multi‑channel stacked view
  - Each channel is rendered as its own lane stacked vertically.
  - Time axis is shared; one common playhead/seek bar across all channels.
  - Left side keeps a fixed narrow gutter for dB grid labels.

- dB grid (lightweight)
  - Per channel, draw 2–3 horizontal reference lines (e.g., 0 dBFS, −6 dB, −12 dB).
  - Labels are small, muted color; lines are subtle to avoid overpowering the waveform.

- Seek interactions (mouse)
  - Click in the waveform area to jump (seek) to that time; play state is preserved.
  - Click‑and‑drag horizontally to scrub; the playhead follows the cursor.
  - The single playhead (vertical line) is shared across channels.

- Time zoom + pan
  - Ctrl + Mouse Wheel: zoom in/out centered at the cursor position.
  - Shift + Mouse Wheel: horizontal scroll (pan).
  - Right‑drag or Middle‑drag: horizontal pan (fallback; optional but recommended).
  - Double‑click: toggle between “fit whole” and “restore last zoom”.

- Visual details
  - Waveform color uses existing amplitude‑based palette; per‑channel lanes share the style.
  - Playhead is drawn on top (2px, accent color) across all lanes.
  - Channel labels (CH1/CH2/…) may be shown left in small text; future work can map to L/R names where known.

- Performance notes
  - On initial implementation, min/max bins for the visible range are computed per frame per channel (bins ≈ panel width).
  - If needed, introduce simple caches (by zoom scale and range) or prebuilt multi‑scale min/max (mip‑style).

This UX keeps editing fast on large lists, while making per‑file inspection substantially clearer and more precise.
- 波形表示は Volume の影響を受けません（常に 0 dB として描画）。Gain(dB) のみ視覚反映します。
