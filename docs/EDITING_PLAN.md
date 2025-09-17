# Editing UX/Implementation Plan (MVP → Extend)

This document captures the concrete plan for waveform editing in waves-previewer.
It follows a hierarchical UX (View → Tool). First pick a View (Waveform /
Spectrogram / Mel / WORLD), then pick a Tool for that view. All views share
time/playhead and an independent Loop region. A/B markers and range Selection
are no longer part of the MVP (updated).

- Goals: instant preview, non-destructive ops, seamless loop, background heavy jobs.
- References: Sound Forge (time selection, zero-cross, AB loop), iZotope RX (spectral
  view and region operations), Wwise (loop markers and non-destructive workflow).

Hierarchy overview
- View selector (Wave/Spec/Mel/WORLD) right under the editor toolbar.
- Tool selector (contextual) appears next to it; contents change with the view.
- Inspector (right) shows the active tool’s parameters. Loop region is edited
  in LoopEdit (Start/End as samples) and also available in the top bar (seconds).

MVP scope（updated）
- Range Selection removed（クリックは常にシーク）。
- Loop region（Start/End）を独立管理。Loop toggle は Off / OnWhole / Marker（L）。
- ループ編集は Inspector の LoopEdit に集約（サンプル単位）。Top bar の秒指定は廃止。
  - プレイヘッド位置からの Start/End 設定ボタンは LoopEdit 内に配置。
- Inspector の操作（Trim / Gain / Normalize / Fade / Reverse / Silence）は「Whole」に適用。
- Export Selection は撤廃。重い処理（Pitch/Stretch）は引き続きバックグラウンドワーカー。

Data model (per EditorTab, updated)
- loop_region: Option<(usize, usize)>  // 再生ループ用（編集選択は無し）
- view_mode: Waveform | Spectrogram | Mel
- snap_zero_cross: bool（現状未使用; Selection 撤廃のため将来再評価）

Interactions (updated)
- Click to seek（常時）; Ctrl+Wheel to zoom; Shift+Wheel or Middle/Right drag to pan。
- Loop: K=Set Start @ playhead、P=Set End @ playhead、L=Off/OnWhole 切替。

Rendering（updated）
- Waveform: zoom に応じて 2 方式
  - Line（spp < 1.0）: 折れ線 + stems（pps>=6）。
  - Aggregated（spp >= 1.0）: px 列ロックの min/max bins（build_minmax と同等）。
- Overlay: base と同じルールに統一（Line/Aggregated ともに一致）。
  - Time‑stretch 時は visible window を比率でマップし、px 列に対して overlay の min/max を算出。
  - LoopEdit の境界帯は同じ列で太線上書き。
  - Debug ビルドでは可視範囲の薄帯やズームログを出力可能。

Phases (updated)
1) MVP above（Loop region 独立・Whole 編集）
2) Spectrogram visualization（閲覧）
3) Pitch/Stretch のオフライン適用（Whole）と安定化（flush/tail）
4) SR/BitDepth conversion + TPDF dither; streaming export for long files
5) Undo/Redo; regions; spectral tools（矩形/ブラシ）; noise reduction
