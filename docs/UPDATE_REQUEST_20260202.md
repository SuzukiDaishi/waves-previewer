# UPDATE REQUEST (2026-02-02)

## 目的
`docs/UPDATE_REQUEST_20260202.md` の要望を、実装リスクが低いものから段階的に反映する。

## 要望整理

### リスト
- 読み込み時にファイル総数をできるだけ早く表示
- 音声読み込み体感の最適化
- OGG 対応
- ショートカット
  - `Ctrl+F`: Search フォーカス
  - `p`: Auto Play 切替
  - `a` / `d`: 音量 Down / Up
  - `r`: Regex 切替
  - `m`: Mode 切替（Speed -> PitchShift -> TimeStretch）

### エディタ
- 左右キー移動でシークバーが表示範囲外へ出る際、表示タイムラインを追従
- 最大拡大時に 1 サンプル移動の整合性を改善
- 範囲選択の汎用化（LoopEdit/Trim 専用実装から拡張）
- 移動時にマーカー位置で止まる
- Pos 表示にサンプル番号を併記
- ショートカット
  - `l`: LoopMode `Off <-> On <-> Marker`
  - 範囲選択中 `l`: その範囲へ Marker ループ適用
  - `0..9`: 波形 `1/n` 位置へ移動（`0` は末尾）
  - `b`: BPM 切替
  - `s`: `Waveform <-> Spectrogram <-> Mel`
  - `a` / `d`: 音量 Down / Up

---

## 実装計画

### Phase A（低リスク・先行実装）
1. ショートカット拡張（`input_ops.rs`）
2. エディタのシーク追従 + Pos サンプル併記（`ui/editor.rs`）
3. OGG 読み込み拡張（`audio_io.rs`）

### Phase B（中リスク・検証重視）
1. 1サンプル整合性の厳密化（display/audio mapping の全面確認）
2. 範囲選択ショートカット群（Shift/Ctrl/Alt 組み合わせ）
3. 汎用範囲選択 UI（Undo/Redo 下に sample/time 表示）

### Phase C（設計変更を伴う）
1. 「総数の早期表示」改善（scan 経路のメッセージ拡張）
2. 音声読み込み最適化（prefix/full decode 戦略の再調整）

---

## 進捗（今回反映）

### 完了
- `Ctrl+F` で Search フォーカス
- `a` / `d` で音量調整（List/Editor 共通）
- List で `p`（Auto Play）, `r`（Regex）, `m`（Mode）
- Editor で `l` 3状態循環 + 範囲選択時 Marker ループ優先
- Editor で `s` 表示モード循環
- Editor で `b`（BPM on/off）
- Editor で `0..9` シーク（`0`/`1` は末尾）
- Editor 左右キー移動時、マーカー位置で停止
- Editor シーク時に表示範囲外なら自動追従
- Pos に sample 併記
- OGG をサポート拡張子に追加
- display/audio サンプル変換を整数比で統一し、1サンプル整合性を強化
- 矢印キー範囲選択ショートカットを追加（Shift/Ctrl/Alt 組み合わせ）
- 汎用 Range HUD を Undo/Redo 直下に追加（sample/time 表示）
- 相対移動/相対範囲選択を仕様に合わせて調整（Ctrl+Alt / Ctrl+Alt+Shift）

### 未完（次段）
- 総数表示のさらなる前倒し
- 読み込み体感の追加最適化

