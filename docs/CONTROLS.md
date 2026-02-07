# Controls

このドキュメントは現行実装を基準にしたショートカット一覧です。
仕様が競合した場合は本ドキュメントを正とします。

## Global
- `Ctrl+F`: Search ボックスへフォーカス
- `Ctrl+E`: Export Selected
- `Ctrl+S`: Session 保存
- `Ctrl+Shift+S`: Session 名前を付けて保存
- `Ctrl+W`: アクティブな Editor タブを閉じる
- `Space`: 再生/停止
- `A` / `D`: 音量 Down / Up

## List View
- `P`: Auto Play 切り替え
- `R`: Regex 検索切り替え
- `Enter`: 選択行を Editor で開く
- `ArrowUp` / `ArrowDown`: 選択移動
- `Shift + ArrowUp/Down`: 範囲選択
- `PageUp` / `PageDown`: ページ単位移動
- `Home` / `End`: 先頭/末尾へ移動

## Editor View
- `K`: ループ開始位置を現在再生位置で設定
- `P`: ループ終了位置を現在再生位置で設定
- `L`: ループモード切り替え（選択範囲がある場合はその範囲を適用）
- `S`: 表示モード切り替え（Waveform -> Spectrogram -> Mel）
- `R`: Zero Cross Snap 切り替え
- `B`: BPM 有効/無効
- `M`: 再生位置にマーカー追加
- `T`: 選択範囲を Trim 適用
- `C`: 選択範囲を削除して詰める
- `0..9`: 波形上の相対位置へシーク（`0` は末尾、`1` は先頭）

## Notes
- `S` は Editor では View 切り替え専用です。Zero Cross Snap は `R` を使います。
- List と Editor で同じキーでも意味が異なるものがあります（例: `P`, `R`）。
