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
- `L`: Apply 済みの loop marker があればそれを使って Marker loop を有効化。無ければ従来どおりループ切り替え
- `S`: 表示モード切り替え（Waveform -> Spectrogram -> Freq Log -> Mel -> Tempogram -> Chromagram -> World (F0/Env)）
- `R`: Zero Cross Snap 切り替え
- `B`: BPM 有効/無効
- `M`: 再生位置にマーカー追加
- `T`: 選択範囲を Trim 適用
- `C`: 選択範囲を削除して詰める
- `1..9, 0`: 波形上の相対位置へシーク（キーボード並び順で先頭→末尾: `1` = 先頭 0%、`2` = 1/9、…、`9` = 8/9、`0` = 末尾 100%）

### ツール別キャンバス操作（Waveform ビュー）
- Gain ツール + 「Gain curve (draw on waveform)」有効時: 波形上のオレンジの折れ線をクリックでポイント追加、ドラッグで移動（±24 dB）、ダブルクリック / 右クリックでポイント削除。カーブはプレビューに即反映され、Apply で焼き込み。
- Pitch Shift ツール: 波形上の水平ピッチラインを上下にドラッグ（上 = 高く、±12 st）。マウスを離すとプレビューを描画・試聴。
- Speed / Time Stretch ツール: 範囲選択後、選択範囲の右端ハンドルを左右にドラッグして伸縮（0.25x〜4x）。ドラッグ中はゴースト領域とレート表示、離すと処理後の波形をプレビュー。選択範囲のみが処理され、境界はクロスフェードで滑らかに接続。
- Reverse ツール: 範囲選択があればその範囲のみ反転（境界は短いクロスフェードでクリックノイズを防止）。

### スペクトログラム操作（Spec / Log ビュー）
- Inspector の「Spectral Warp」で「Edit warp points on spectrogram」を有効にすると、スペクトログラム上をドラッグして周波数成分を上下に押し流せます（Liquify風の画像的ワープ）。ストロークは矢印（起点リング→目標ドット）として表示され、矢印を掴んで再調整、ダブルクリック / 右クリックで削除。Radius (ms / Hz) で時間・周波数方向の影響範囲を調整。ドラッグを離すとワープをレンダリングして即試聴、Apply で破壊的に焼き込み（Undo対応、スペクトログラムは自動再解析）。Mel ビューは閲覧専用のため対象外。

### 音量(Gain)の統一フレームワーク
- リストの Gain 列 / Left・Right キーでの音量変更は、対象ファイルの Editor タブが開いていればエディタの破壊的編集として適用されます(波形に反映、dirty、Editor 側の Undo 対象)。タブが無いファイルは従来どおり pending gain として保持。
- pending gain を持つファイルを Editor で開くと、その時点でゲインがバッファへ焼き込まれ(Undo 可)、以降はエディタ編集として一元管理されます(再生・保存・書き出しで二重適用されません)。

### EQ / Compressor / Noise Gate のグラフィカル操作
- EQ: 周波数応答カーブ上の3つのハンドルをドラッグ(横=周波数、縦=ゲイン)。緑のMidハンドル上でスクロールするとQを調整。
- Compressor: 伝達カーブのニー(オレンジ)を横ドラッグでThreshold、上端ポイント(緑)を縦ドラッグでRatio。
- Noise Gate: しきい値ハンドルをドラッグ。Inspector と Effect Graph ノードの両方で使えます。

## Notes
- `S` は Editor では View 切り替え専用です。Zero Cross Snap は `R` を使います。
- List と Editor で同じキーでも意味が異なるものがあります（例: `P`, `R`）。
