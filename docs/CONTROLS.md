# Controls

このドキュメントは現行実装を基準にしたショートカット一覧です。
仕様が競合した場合は本ドキュメントを正とします。

アプリ内でも **Help > Keyboard Shortcuts...** から同じ一覧（実装のキーマップ表 `src/app/keymap.rs` から自動生成）を参照できます。

## Global
- `Ctrl+F`: Search ボックスへフォーカス
- `Ctrl+E`: Export Selected
- `Ctrl+S`: Session 保存
- `Ctrl+Shift+S`: Session 名前を付けて保存
- `Ctrl+Shift+N`: 新しいウィンドウ
- `Ctrl+W`: アクティブな Editor タブを閉じる
- `Space`: 再生/停止
- `A` / `D`: 音量 Down / Up

補足:
- **Help > Customize Shortcuts...** でテーブル定義のショートカットをクリック→キー押下で再割り当てできます（重複チョードは拒否、行の Reset / Reset All で既定に戻ります。`keymap=` 行として prefs に保存）。グレー行（複数キーからなる操作ファミリ）は固定です。
- **Edit メニュー**（File と Export の間）に Undo / Redo が追加されました。エディタ / リスト / Effect Graph の各 Undo スタック状態に応じて有効化され、`Ctrl+Z` / `Ctrl+Y` と同じ経路を通ります。

## List View
- `P`: Auto Play 切り替え
- `R`: Regex 検索切り替え
- `Enter`: 選択行を Editor で開く
- `F2`: 選択行をその場でリネーム（Enter=確定、Esc/フォーカス喪失=キャンセル。コンテキストメニューにも有り）
- `ArrowUp` / `ArrowDown`: 選択移動
- `Shift + ArrowUp/Down`: 範囲選択
- `PageUp` / `PageDown`: ページ単位移動
- `Home` / `End`: 先頭/末尾へ移動

補足:
- 行の右クリックメニューに **Open in Editor / Reveal in Folder**（先頭）と **Select All / Clear Selection**（末尾）が追加されました。複数選択の内側を右クリックしても選択は維持されます。
- フォルダ未読込かつリストが空のときは、中央に **Open Folder...** ボタンと最近のセッション（最大 5 件）を並べたオンボーディングパネルが表示されます。
- 単クリックは既定で「選択+ロード/試聴」です。Settings の「Single click auditions」を OFF にすると単クリックは選択のみになり、試聴は Space / キーボードナビ / Auto Play で行います（ダブルクリック=Editor で開く、は変わりません）。
- 列幅はドラッグでリサイズすると prefs に保存され、次回起動時も維持されます。
- **List > Inspect Files (QA)...**(行コンテキストメニューにも有り)で一括検査(ピーク超過 / LUFS 逸脱 / 無音余白 / ループ不整合)を実行できます。結果ウィンドウは severity フィルタ・行クリックでリスト選択・CSV 保存に対応。
- **List > Normalize Loudness...** で選択(または全件)のラウドネスを目標 LUFS へ非破壊で揃えられます(pending gain を設定。ファイルは書き換えません。バッチ全体で 1 回の Undo)。
- **List > Audition Selection (Round-robin / Random)**: 2 件以上選択した状態で、選択ファイルを順番（またはランダム、同一ファイル連続なし）に連続試聴します。各ファイルの自然終了で次へ進み、停止（Space / 別の行を選択 / topbar の「Audition n/m」の Cancel）で終了します。
- **List > Find Duplicates...**: 選択(または全件)を指紋化して、完全一致(exact)と知覚的に近い(similar、音量違いも検出)ファイルをグループ表示します。行クリックでリスト選択、CSV 保存対応。
- **List > Export Engine Metadata...**: Unity(JSON) / FMOD(JSON) / Wwise(TSV) 向けのメタデータテーブル(ループ・SR・ch・長さ・LUFS)を書き出します(音声変換なし)。CLI は `batch engine-export`。
- **List > Edit BWF Metadata...**: 選択した WAV に bext チャンク(Description / Originator / Reference、日時は自動)を一括書き込みします(他のチャンクは保全、非 WAV はスキップ)。
- Inspect Files (QA) に命名規則チェック(ファイル名 stem への正規表現)が追加されました。CLI は `--naming-pattern`。
- World ビューの Inspector に **Formant** スライダ(0.5x〜2.0x)が追加されました。Resynthesize 時にスペクトル包絡を周波数方向にワープし、ピッチを変えずにフォルマントだけ動かせます。
- **Tools > Plugin Manager...**: プラグインカタログの一覧・再スキャン・検索パスの管理（prefs に永続化）を行うウィンドウです。
- Plugin FX ツール / Effect Graph のプラグインノードでは、パラメータのプリセット保存/読み込み/削除と A/B 比較（Store B → A/B Swap）が使えます。Plugin FX の「Auto preview」を ON にすると、パラメータ変更の約 300 ms 後に自動でプレビューを再レンダリングします（再生位置は維持）。

## Editor View
- `K`: ループ開始位置を現在再生位置で設定
- `P`: ループ終了位置を現在再生位置で設定
- `L`: Apply 済みの loop marker があればそれを使って Marker loop を有効化。無ければ従来どおりループ切り替え
- `S`: 表示モード切り替え（Waveform -> Spectrogram -> Freq Log -> Mel -> Tempogram -> Chromagram -> World (F0/Env)）
- `R`: Zero Cross Snap 切り替え
- `B`: BPM 有効/無効
- `M`: 再生位置にマーカー追加
- `T`: 選択範囲を Trim 適用（実行後に Ctrl+Z 案内トーストを表示）
- `C`: 選択範囲を削除して詰める（実行後に Ctrl+Z 案内トーストを表示）
- `1..9, 0`: 波形上の相対位置へシーク（キーボード並び順で先頭→末尾: `1` = 先頭 0%、`2` = 1/9、…、`9` = 8/9、`0` = 末尾 100%）
- `Home` / `End`: 先頭 / 末尾へシーク
- `Z`: 選択範囲へズーム（5% マージン付きでフィット）
- `+` / `=`: プレイヘッドを基準にズームイン（ホイールと同じ 0.9 倍ステップ）
- `-`: プレイヘッドを基準にズームアウト
- `[` / `]`: 表示幅 1 ページぶん前 / 後へスクロール
- `Esc`: 未適用のツールプレビューを破棄（プレビューが無いときは何もしない）
- `Ctrl+C` / `Ctrl+X`: 選択範囲の音声をアプリ内オーディオクリップボードへコピー / カット
- `Ctrl+V`: クリップボードの音声を選択開始位置（無選択時は再生位置）へ挿入ペースト（SR 変換・ch 適応あり、Undo 可）
- `Alt+ドラッグ`（波形上）: スクラブ再生。ポインタ位置 ±40ms の小窓をループ再生し、離すと元のループ / 再生状態に戻ります
- `Ctrl+クリック`（Spec / Log ビュー）: クリックした部分音の倍音アクション。f0 を近傍ピークへ精緻化し、倍音バンド（k×f0 ±3%）をハイライト+ポップアップから一括 Mute / Attenuate（選択範囲があればその範囲、無ければ全体。Undo 可）
- `Ctrl+C` / `Ctrl+V`（Spec / Log ビューで周波数帯選択中）: スペクトル領域のコピー / ペースト（置換。`Ctrl+Shift+V` は加算）。ペースト位置は hop グリッドへスナップ、同一サンプルレートのバッファのみ

補足:
- ツールバーの `M/S` メニューでチャンネル毎の再生 mute / solo を切り替えられます（モニタリング専用。編集・保存・書き出しには影響せず、Undo 対象外。リスト再生には適用されません）。
- ツール一覧に **Invert Polarity**（位相反転）、**DC Offset**（DC 除去、測定値表示付き）、**Insert Silence**（無音挿入。選択開始位置 / 再生位置に挿入し、以降のマーカー・ループは右へシフト）が追加されています。
- **De-click** ツール: Sensitivity を調整して Scan すると検出クリックが波形上に赤帯で表示され、Apply で修復（選択範囲があればその範囲のみ、Undo 対応）。
- **De-noise** ツール: ノイズだけの区間を選択して「Learn from Selection」でプロファイル学習 → Reduction（最大減衰量）/ Strength を調整して Preview / Apply。選択範囲があればその範囲のみ処理（端はクロスフェード）。
- カスタムチャンネルビュー（表示チャンネルを絞った状態）では、Gain / Normalize / Fade / Mute / Noise Gate / EQ / Compressor / DC / 位相反転などの範囲編集が表示中のチャンネルにのみ適用されます（インスペクタに「Applies to: ch N」表示。リストの Gain 列からのファイルゲインは常に全チャンネル）。
- エディタのオーディオクリップボードは `Ctrl+V`（挿入）に加えて `Ctrl+Shift+V`（ミックス: 長さ不変で加算）/ `Ctrl+Alt+V`（クロスフェード挿入: 両接合部を等パワーで滑らかに）に対応。
- 16bit 整数 PCM への書き出し（WAV/AIFF/FLAC）は Settings の「TPDF dither on 16-bit export」（デフォルト ON）でディザされます。
- ツール選択は Waveform ビューのインスペクタ上部の**アイコンツールバー**で行います（グループ: ナビ/レベル系 → タイム/ピッチ系 → ダイナミクス/解析系 → サンプル操作/リストア系。ホバーでツール名表示、狭い幅では折り返し）。
- Settings の「Wheel scrolls the view (Ctrl+wheel zooms)」を ON にすると、素のホイールは横スクロール、ズームは Ctrl+ホイール / ピンチになります（既定 OFF = 従来どおりホイールでズーム）。
- Invert Polarity に「Smooth boundaries (~2 ms)」オプションが追加されました（既定 OFF）。ON にすると選択範囲の内側境界で極性反転を約 2 ms かけてクロスフェードし、部分反転のクリックノイズを防ぎます（範囲がファイル端に接する側は正確な符号反転のまま）。
- **De-clip** ツール: クリッピングでレールに張り付いたフラットランを検出（Scan で赤帯表示）し、エルミート補間で潰れた山を再構築します（修復はレールを超えて伸びます。float バッファのためヘッドルームは保持）。矩形波や滑らかな低域の山は誤検出しません。
- **De-hum** ツール: 電源ハム除去。基本周波数（Detect で 45〜65 Hz を自動検出）とその倍音（最大16本）に狭帯域ノッチ（RBJ biquad カスケード）をかけます。Q / 深さ調整可、選択範囲があればクロスフェードスプライスで範囲限定。
- **Edit > History...**: アクティブタブの編集履歴パネル。操作名付きの Undo / Redo 一覧から行クリックで複数ステップ一括ジャンプ。
- **Edit > Regions...**: ラベル付きリージョン一覧。選択範囲から追加 / 名前編集 / クリックで選択 / サイドカー保存（`<file>.regions.json`）/ CSV 書き出し。リージョンはマーカーと同様に Undo と破壊的編集のリマップに追従し、セッションにも保存されます。
- **World ビュー**: Aperiodicity（ブレス成分）スライダが追加されました。Set All / Set Selection でフレーム毎倍率ドラフトに書き込み、Resynthesize 時に 0..1 クランプで焼き込まれます。
- Settings のディザ設定は**モード選択**になりました（Off / TPDF / TPDF + noise shaping。ノイズシェイピングは可聴帯域外へ量子化ノイズを押し出す 2 次エラーフィードバック）。「Also dither 24-bit exports」で 24bit 書き出しにも適用できます。
- 重い適用処理（Pitch/Stretch/Speed/LoudNorm/De-click/De-noise/Spectral 系/WORLD 再合成）は**アプリ全体をブロックしません**。処理中はそのタブのみ操作が無効化され（タブ内にメッセージ表示）、他のタブ・リスト・他ソースの再生は通常どおり使えます。進捗と Cancel はトップバーの activity 表示から。処理中のタブを閉じた場合、結果は破棄されます。同時に実行できる適用は 1 件です。

### ツール別キャンバス操作（Waveform ビュー）
- Gain ツール + 「Gain curve (draw on waveform)」有効時: 波形上のオレンジの折れ線をクリックでポイント追加、ドラッグで移動（±24 dB）、ダブルクリック / 右クリックでポイント削除。カーブはプレビューに即反映され、Apply で焼き込み。
- Pitch Shift ツール: 波形上の水平ピッチラインを上下にドラッグ（上 = 高く、±12 st）。マウスを離すとプレビューを描画・試聴。
- Speed / Time Stretch ツール: 範囲選択後、選択範囲の右端ハンドルを左右にドラッグして伸縮（0.25x〜4x）。ドラッグ中はゴースト領域とレート表示、離すと処理後の波形をプレビュー。選択範囲のみが処理され、境界はクロスフェードで滑らかに接続。
- Reverse ツール: 範囲選択があればその範囲のみ反転（境界は短いクロスフェードでクリックノイズを防止）。
- Pencil ツール: 高ズーム時（1サンプルあたり 2px 超）に波形上をドラッグするとサンプル値を直接描画できます（ドラッグ点間は線形補間、ポインタのあるレーンが対象、1ストローク = 1 Undo）。

### スペクトログラム操作（Spec / Log ビュー）
- Inspector の「Spectral Warp」で「Edit warp points on spectrogram」を有効にすると、スペクトログラム上をドラッグして周波数成分を上下に押し流せます（Liquify風の画像的ワープ）。ストロークは矢印（起点リング→目標ドット）として表示され、矢印を掴んで再調整、ダブルクリック / 右クリックで削除。Radius (ms / Hz) で時間・周波数方向の影響範囲を調整。ドラッグを離すとワープをレンダリングして即試聴、Apply で破壊的に焼き込み（Undo対応、スペクトログラムは自動再解析）。Mel ビューは閲覧専用のため対象外。
- Inspector の「Spectral Brush」で「Paint attenuation on spectrogram」を有効にすると、ドラッグした軌跡の成分を消しゴムのように減衰できます（RX 風）。Strength (dB) と Radius (ms / Hz) はスタンプ毎に焼き込まれ、重ね塗りは dB 加算（80 dB 上限）。ドラッグを離すとプレビューをレンダリングして即試聴、Apply で焼き込み（Undo 対応）。Warp 編集とは排他です。
- 時間範囲（+任意の周波数帯選択）を選んで「Heal Selection」を押すと、選択部を周囲のスペクトルから再構築します（インペイント。ドロップアウトやノイズの穴埋めに。120 秒超の選択は不可）。

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
