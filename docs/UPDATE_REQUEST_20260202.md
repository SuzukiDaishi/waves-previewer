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

---

## 追加要望（2026-02-03）

### 小規模修正（v0系）
- item 背景色設定（設定から dBFS / LUFS / 標準色を切替可能）
- item のどこでも右クリックメニューを開ける（文字外でも開く）
- ショートカット `p`: Auto Play の on/off 切替（`a` / `d` は音量維持）
- `.nwsess` で Excel/CSV 読み込み状態も復元し、未復元項目を調査して原則すべて復元
- UI 文言と閉じ方の統一（Close / X などを統一。希望は X）
- 右クリックから Bits 変換（可能なフォーマットのみ）
- SampleRate Converter アルゴリズム見直し（縮小 SRC 前 LPF 含む品質検証）
- `Ctrl+E` と同等のエクスポートを右クリックメニューから実行可能に
- session ファイル内パスは session ファイル基準の相対パスで保持

#### 非破壊方針（確定）
- `Export` を除き、音声データへの実ファイル上書きは行わない。
- SR/Bits/Trim(Add Virtual) などの編集結果はメモリ状態（override/virtual）として保持し、保存は明示的な Export 操作時のみ実行する。

### 中規模修正
- VST / CLAP 読み込み対応（List 表示性能を維持し、波形ズレを回避。重処理は一時的許容）
- エディタ下部の猫アニメ表示（透過 GIF / 音・BPM 連動、無音時挙動含む、軽量実装）

### 大規模 Update: オプトイン解析 AI ① 文字起こし
- オプトインで有効化（ヘッダからモデルダウンロード。必要ファイル不足時は機能非表示）
- 文字起こし AI: `https://huggingface.co/zukky/LiteASR-ONNX-DLL`（ONNX + 前後処理 DLL）
- リスト右クリック `Transcript` で実行可能
- 結果を Transcript 行に表示
- 保存時の SRT 同時出力（設定で on/off）

### 大規模 Update: オプトイン解析 AI ② 楽曲解析
- オプトインで有効化（ヘッダからモデルダウンロード。必要ファイル不足時は機能非表示）
- 楽曲 AI: `https://huggingface.co/zukky/allinone-DLL-ONNX`（ONNX + 前後処理 DLL）
- エディタ Inspector から解析実行、結果をマーカー等へ反映

### 大規模 Update: ノードベース オフライン一括処理
- 右クリックからエフェクト適用ジョブを実行
- 処理フローは別タブで定義

#### エフェクトグラフ
- Tool からエフェクトグラフタブを開く（UE ブループリント風 UI）
- 入力ノード / 出力ノードは固定で必須
- 入力ノードで Wav を選択し、ノード接続後に実行、出力ノードで試聴
- 既存エフェクト系ノード（PitchShift / TimeStretch / SampleRateConvert など）
- ChannelSplit / MixDown / MixUp / BandSplit
- VST / CLAP ノード対応（List 表示性能への悪影響を避け、重処理は一時的許容）

---

## 追加要望の詳細化（コード調査反映）

### v0系（詳細仕様）

1) item 背景色設定（標準 / dBFS / LUFS）
- 設定項目 `item_bg_mode = standard | dbfs | lufs` を追加する。
- `dbfs/lufs` は既存メタ値を参照し、未計算時は標準色へフォールバックする。
- 選択行の可読性を優先し、選択ハイライトを最優先で描画する。

2) item のどこでも右クリック
- 行全体のインタラクションレスポンスに `context_menu` を紐づける。
- 既存ラベル個別クリック依存を避け、余白・波形列・数値列どこでも同メニューを開けるようにする。

3) Auto Play ショートカット
- `p` でトグル（現行運用を維持）。
- `a` / `d` は音量 Down / Up のまま維持する。

4) `.nwsess` の外部CSV/Excel復元
- セッション保存対象に以下を追加する:
  - 外部ソース一覧（path, sheet, header/data row, has_header）
  - キー設定（rule, input, regex, replace, scope）
  - 表示設定（visible columns, show unmatched, active source）
- 保存時はパスのみ保持し、表データ本体は保存しない（復元時に再ロード）。
- 復元時にファイル未存在ならエラー表示のみで継続する（クラッシュさせない）。

5) 復元漏れ調査（原則すべて復元）
- 「保存対象」と「ランタイムのみ（非保存）」を明示して漏れを潰す。
- 少なくとも `sample_rate_override`、`auto_play_list_nav`、外部データ設定群は保存対象に含める。

6) UI 文言と閉じ方の統一（X寄せ）
- 非破壊系ウィンドウは `X` で閉じる挙動に統一し、`Close` ボタンを廃止する。
- 破壊的確認ダイアログは `Cancel` を維持しつつ、`X` でも `Cancel` 相当で閉じる。

7) 右クリック Bits 変換
- 第1段階は実ファイル WAV のみ対象（16/24/32f）。
- 動作は新規書き出しを基本とし、非破壊方針を守る。
- 対象外フォーマットはメニューを無効化表示する。

8) SampleRate Converter 見直し
- 現行の線形補間ベースから、品質プリセット（Fast/Good/Best）方式へ更新する。
- ダウンサンプル時はアンチエイリアスLPFを必須にする。
- 主要経路（プレビュー、エクスポート、セッション復元時リサンプル）で同品質方針を使う。

9) 右クリック Export
- リスト右クリックに `Export Selected...` を追加し、`Ctrl+E` と同じ処理を呼ぶ。

10) セッション相対パス
- 保存時はセッションファイル基準で相対化を試行する。
- 相対化不能（別ドライブ等）の場合は絶対パスフォールバックを許可する。
- ロード時は「相対 -> 絶対」の順に解決する。

### 中規模（詳細仕様）

11) VST / CLAP 読み込み
- プラグイン探索は非同期化し、List描画・スクロールをブロックしない。
- 波形ズレ回避のため、オフライン適用時はレイテンシ情報を考慮した位置合わせを行う。
- 導入初期は「オフライン処理優先」で段階的に拡張する。

12) 猫アニメ
- 透過GIF常時デコードは避け、軽量フレームアニメ（4コマ想定）を前提にする。
- BPM有効時はテンポ同期、無音時は低速アイドル挙動にする。
- UIフレーム時間に影響しない描画更新頻度（低FPS）で実装する。

### 大規模（詳細仕様）

13) オプトイン解析AI（共通）
- ヘッダからモデル導入、必要ファイル不在時は機能を非表示/無効化する。
- モデル配置は Hugging Face 標準キャッシュ準拠を前提とする。

14) 文字起こしAI
- リスト右クリック `Transcript` でジョブ投入。
- 結果を Transcript 列に反映し、設定ON時は SRT を同時出力する。
- セッション復元では再生成せず、既存SRTや保存済み結果を優先利用する。

15) 楽曲解析AI
- Editor Inspector から解析実行し、結果をマーカーへ反映する。
- マーカー種別（拍/区間など）を区別可能な内部表現にする。

16) ノードベース一括処理
- 右クリックからノード定義済みジョブを実行可能にする。
- グラフ編集UIと実行エンジン（オフライン処理）を分離する。
- 入力/出力ノード固定、既存エフェクトノード・VST/CLAPノードを段階導入する。

---

## 実装詳細（変更コードと追加内容）

この章は「どのコードをどう変えるか」を実装単位で固定化するための作業メモ。

### A. v0系（優先）

#### A-1. List 行背景色モード（Standard / dBFS / LUFS）

対象コード
- `src/app/types.rs`
- `src/app.rs`
- `src/app/ui/export_settings.rs`
- `src/app/theme_ops.rs`
- `src/app/ui/list.rs`

追加/変更内容
- `types.rs`
  - `ItemBgMode` enum を追加:
    - `Standard`
    - `Dbfs`
    - `Lufs`
- `app.rs`
  - `WavesPreviewer` に `item_bg_mode: ItemBgMode` フィールド追加。
  - 初期値は `ItemBgMode::Standard`。
- `theme_ops.rs`
  - `prefs.txt` に `item_bg_mode=` の load/save を追加。
  - 不明値は `Standard` にフォールバック。
- `ui/export_settings.rs`
  - Settings ウィンドウに `Item Background` 設定UIを追加（3択）。
  - 値変更時は即時反映（repaint）。
- `ui/list.rs`
  - 行描画時に `item_bg_mode` を参照し背景色を決定。
  - `Dbfs` は `peak_db + pending_gain_db`、`Lufs` は `lufs_override` 優先で描画。
  - 未計算（None）は標準色で描画。
  - 選択行ハイライトは背景色より優先（選択可読性維持）。

受け入れ条件
- 設定変更で即時に全行背景が切り替わる。
- メタ未計算行でちらつきや色化けがない。
- 選択行の視認性が落ちない。

---

#### A-2. item のどこでも右クリック

対象コード
- `src/app/ui/list.rs`

追加/変更内容
- 右クリックメニューは `row.response().context_menu(...)` を継続利用する。
- 各セル個別 `sense` に依存せず、行全体レスポンスにメニューを統一。
- 必要に応じて行インタラクション rect を補強し、余白領域でも `secondary_clicked` を拾う。
- 右クリック時に未選択行なら選択更新する既存挙動を維持。

受け入れ条件
- ファイル名/フォルダ/波形/空白のどこでも同じメニューが開く。
- 複数選択の挙動を壊さない。

---

#### A-3. ショートカット（Auto Play = `p` 維持）

対象コード
- `src/app/input_ops.rs`
- `docs/CONTROLS.md`（必要なら）

方針
- `p`: Auto Play on/off（現行維持）
- `a` / `d`: 音量 Down / Up（現行維持）

追加/変更内容
- 実装コード変更は原則不要（現状仕様どおり）。
- ドキュメントとの差異があればドキュメントを実装に合わせる。

---

#### A-4. 右クリックメニューに Export

対象コード
- `src/app/ui/list.rs`
- `src/app.rs`（既存 `trigger_save_selected` 呼び出し）

追加/変更内容
- 右クリックメニューに `Export Selected...` を追加。
- 実行は `self.trigger_save_selected()` を呼ぶ（`Ctrl+E` と同一処理）。
- `first_prompt`（初回 Export 設定確認）挙動をそのまま利用。

受け入れ条件
- 右クリック Export と `Ctrl+E` が同じ結果になる。

---

#### A-5. 右クリックメニューに Bits 変換（第1段階）

対象コード
- `src/app/ui/list.rs`
- `src/app.rs`
- `src/wave.rs`
- `src/app/types.rs`（必要ならダイアログ状態型）

追加/変更内容
- 右クリックメニューに `Convert Bits` サブメニュー追加:
  - `16-bit PCM`
  - `24-bit PCM`
  - `32-bit float`
- 第1段階は「実ファイルかつ WAV」のみ有効。
- 変換対象外（mp3/m4a/virtual/external）は disabled 表示。
- 実装は新規書き出し（非破壊）を基本にし、命名は `"<name> (16bit).wav"` のようにする。
- `wave.rs` に「指定bit-depthでWAV書き出し」関数を追加:
  - 16-bit: `i16`
  - 24-bit: `i24`相当（`i32`書き込み時に24bitレンジへ量子化）
  - 32f: `f32`

受け入れ条件
- WAVのみ変換可能。
- 元ファイルは上書きしない。
- 複数選択でまとめて実行可能。

---

#### A-6. Session 相対パス（相対化不能時は絶対フォールバック）

対象コード
- `src/app/project.rs`
- `src/app/session_ops.rs`

追加/変更内容
- 保存時:
  - まず session 基準で相対化を試行。
  - 相対化不能（別ドライブ等）の場合は絶対パス保存を許可。
- 読込時:
  - 相対パスは session 親を基準に解決。
  - 絶対パスはそのまま使用。
- 既存 `rel_path` / `resolve_path` は上記仕様に沿って明示コメントを追加。

受け入れ条件
- 同一ドライブは相対保存される。
- 別ドライブは絶対パスで保存・復元できる。

---

#### A-7. `.nwsess` 復元拡張（外部CSV/Excel + 復元漏れ対策）

対象コード
- `src/app/project.rs`
- `src/app/session_ops.rs`
- `src/app/external_ops.rs`
- `src/app/external_load_jobs.rs`
- `src/app.rs`

追加/変更内容
- `ProjectFile` に外部データ復元用構造体を追加（例: `ProjectExternalState`）:
  - `sources[]`:
    - `path`
    - `sheet_name`
    - `has_header`
    - `header_row`
    - `data_row`
  - 全体設定:
    - `active_source`
    - `key_rule`
    - `match_input`
    - `visible_columns`
    - `scope_regex`
    - `match_regex`
    - `match_replace`
    - `show_unmatched`
- 追加で保存対象にする状態:
  - `auto_play_list_nav`
  - `sample_rate_override`
- 読込時の処理:
  - 外部設定を先に復元。
  - `sources` は `begin_external_load(...)` のキューで順次再読込。
  - ファイル欠損時は `external_load_error` を残して継続。

受け入れ条件
- Session再読込で外部データ設定が再現される。
- 欠損ファイルがあってもアプリは継続動作する。

---

#### A-8. Close/X 統一（X寄せ）

対象コード
- `src/app/ui/external.rs`
- `src/app/ui/export_settings.rs`
- `src/app.rs`（rename/batch/resample/leave prompt など）
- `src/app/ui/tools.rs`（confirm dialog）

追加/変更内容
- 非破壊ウィンドウ:
  - `Window::open(&mut open)` を使い、`Close` ボタン削除。
- 破壊的確認ダイアログ:
  - `Cancel` ボタンは維持。
  - Xで閉じた場合は `Cancel` 相当として扱う。

受け入れ条件
- 「閉じる」は原則 X で統一される。
- 確認ダイアログの安全性は維持される。

---

#### A-9. SRC アルゴリズム見直し

対象コード
- `src/wave.rs`
- `src/app/logic.rs`
- `src/app/list_preview_ops.rs`
- `src/app/export_ops.rs`
- `src/app/session_ops.rs`
- `src/app/theme_ops.rs` / `src/app/ui/export_settings.rs`（品質設定UIを持たせる場合）

追加/変更内容
- `resample_linear` は互換用に残しつつ、新実装を追加:
  - `resample_quality(mono, in_sr, out_sr, quality)`
- ダウンサンプル時はLPF入りのバンド制限リサンプルを利用。
- 適用経路を統一:
  - list preview
  - editor preview/apply
  - export
  - session復元時再サンプル
- 品質プリセット（Fast/Good/Best）を設定に持たせる。

受け入れ条件
- ダウンサンプル時の高域折り返しが軽減。
- 主要経路で同品質方針が使われる。

---

### B. 中規模（設計先行）

#### B-1. VST / CLAP 読み込み

対象コード（新規中心）
- `src/plugin/`（新設）
  - `scan.rs`
  - `host.rs`
  - `cache.rs`
- `src/app/`（UI導線）
  - `ui/list.rs`（右クリック導線）
  - `ui/editor.rs`（Inspector導線）

基本方針
- List性能維持のため、スキャン/ロードは完全非同期。
- 初期段階はオフライン適用中心（リアルタイム追従は後段）。

---

#### B-2. 猫アニメ

対象コード（新規中心）
- `src/app/ui/editor.rs`
- `src/app/render/`（必要なら新規モジュール）

基本方針
- GIF直接再生ではなく、軽量フレーム描画を前提にする。
- BPM連動・無音時低速のルールを実装する。

---

## 追加ライブラリ（候補）

### v0〜v1で導入候補
- `rubato`（SRC高品質化）
  - 用途: LPF込みの高品質リサンプル（特にダウンサンプル）
  - 反映先: `src/wave.rs` の新SRC関数

### 中規模以降の導入候補
- `clack-host`（CLAPホスト）
  - 用途: CLAPプラグイン読み込み基盤
- `vst3`（VST3バインディング）
  - 用途: VST3読み込み基盤
- `egui-snarl`（ノードグラフUI）
  - 用途: エフェクトグラフ画面
- `hf-hub`（HF標準キャッシュ）
  - 用途: AIモデルのダウンロードと存在確認

### Cargo.toml 追記案（段階導入）
```toml
# v0
rubato = "0.15"

# 中規模以降（段階導入）
# clack-host = "..."
# vst3 = "..."
# egui-snarl = "..."
# hf-hub = "..."
```

注記
- 実際のバージョンは導入時点で固定し、`cargo check` で互換確認してから確定する。
- ライセンス確認（配布形態含む）は導入PRで実施する。

---

## セッション保存対象（確定案）

### 保存する
- list:
  - root / files / list item gain
  - sort/search/list_columns
  - `auto_play_list_nav`
  - `sample_rate_override`
- app:
  - theme
  - spectrogram settings
- tabs:
  - 既存編集状態一式
- external:
  - source path/sheet/header/data row
  - key/match/scope/visible columns/show unmatched/active source

### 保存しない（ランタイム）
- 非同期ジョブ進捗（inflight/receiver）
- キャッシュ本体（spectrogram tile実体など）
- 一時ダイアログ開閉状態

---

## PR分割（推奨）

1. `list-context-menu-export`
- 右クリック行全域化 + Export追加

2. `list-item-bg-mode`
- 背景色モード enum/settings/prefs/UI

3. `session-external-restore`
- `.nwsess` 外部復元 + auto_play/sample_rate_override 保存
- 相対パス+絶対フォールバック

4. `ui-close-x-unify`
- Close/X統一

5. `bits-convert-wav`
- Bits変換（WAV限定）

6. `src-quality-upgrade`
- 新SRC導入 + 経路統一

---

## 追加修正（2026-02-04）

### 追加要望（確定）
- wav, m4a, mp3, ogg 周りの読み込み、保存のテストコードの作成と修正
- virtualのセッションファイルからの復元と、そのテスト
- m4aのサンプルレート、チャンネル表示、mp3やwav, oggも同様
- virtualの生成時に元の音声とサンプルレート表示が変わる
- virtualの挙動の諸々の確認と安定化
- virtualの保存機能の確認(m4a,mp3,ogg,wav)
- wav, m4a, mp3, oggの相互変換機能(リストで右クリックから選択)
- エディタでトリム機能など波形長さが変わる際に、シークバーの移動範囲がそのままになる(一度閉じて再度開くとなおる)
- trim機能に追加、選択範囲を新たなバーチャルファイルとしてリストに追加。
- その他、ファイルの読み込み、書き出し周りの実装要件を改めて定義して、デバッグを厳密に追加

### 実装方針（I/O基盤を先に統一）
1. `audio_io` の責務を統一し、`probe` / `decode` / `encode` / `convert` の4系統APIを一本化する。
2. List表示は軽量 `probe`、編集/再生は `decode` を使い分け、メタ表示差異を解消する。
3. 書き出しは以下を基本方針とする。
   - WAV: ネイティブ実装（PCM16/24/32f）
   - MP3/M4A/OGG: export backend 経由（既存実装を優先、必要に応じてffmpeg backendを許容）
4. 相互変換（右クリック）は専用実装を増やさず、Exportパイプラインを呼ぶ共通導線にする。

### virtual 安定化方針
1. virtualは `audio_spec(sample_rate/channels)` を生成時に固定保持し、後段の再probeで上書きしない。
2. `.nwsess` には virtual の復元に必要な情報（source参照、処理チェーン、audio_spec）を加算保存する。
3. セッション復元時は virtual を再構築し、欠損入力があってもクラッシュせず継続する。
4. virtual保存（wav/m4a/mp3/ogg）は `decode(virtual) -> encode(target)` の同一経路で検証する。

### Editor trim / seek 修正方針
1. 波形長変更時に `seek_pos` / `seek_max` / `view_window` / `selection` を同フレームで再計算・clampする。
2. trim適用時の追加機能として「選択範囲を新規virtualとしてリスト追加」を実装する（非破壊）。

### テスト方針（厳密化）
1. フォーマット別I/Oテスト: wav/m4a/mp3/ogg の `probe/decode` 成功、SR/Ch/Duration の検証。
2. 保存往復テスト: encode後に再decodeしてメタ整合を確認（lossyは許容差を定義）。
3. 相互変換テスト: 各入力->各出力の最小マトリクスを自動テスト化。
4. virtualセッション復元テスト: 保存->再読込で item数、spec、再生可能性を検証。
5. trim長変更テスト: seek範囲が即時更新されることを検証。

### デバッグ強化
- I/O境界で path/container/codec/sr/ch/frames を構造化ログ出力する。
- virtual生成/復元時に source参照・処理チェーン・最終spec をログ出力する。
- debugビルドでPCM健全性チェック（NaN/Inf/peak/rms）を追加する。
  - 実装: `NEOWAVES_IO_TRACE=1` で `io_trace` を出力（probe/decode境界）。
  - 実装: debugビルドでは decode後に非有限値（NaN/Inf）を0へ置換し、件数をログ出力。

### 実機UI確認・計測手順（追加修正）
1. List高速性の確認（dummy 70k）
   - `cargo run -- --dummy-list 70000 --screenshot debug\\zz_dummy_70k.png --screenshot-delay 20 --exit-after-screenshot`
   - 起動直後のスクロール/選択でフリーズがないことを確認。
2. AutoPlay遅延計測（実ファイル）
   - `cargo run -- --open-file debug\\gui_test_440.wav --auto-run --debug --debug-summary debug\\zz_auto_list_summary.txt --debug-summary-delay 120`
   - summaryの `select_to_preview_ms` / `select_to_play_ms` の `p95` を比較する。
3. Editor自動操作の回帰確認
   - `cargo run -- --open-file debug\\gui_test_440.wav --auto-run-editor --auto-run-delay 20 --debug --debug-summary debug\\zz_auto_editor_summary.txt`
   - Trim/適用後にseek範囲が即時反映されるか確認する。
4. 相互変換確認（右クリック）
   - List右クリック `Convert Format` から `WAV/MP3/M4A/OGG` を順次実行。
   - 生成物を再読込し、`Ch/SR/Bits` が `-` にならないことを確認。
5. virtual復元確認
   - virtualを作成して `.nwsess` 保存→再起動→再読込。
   - virtualが欠落せず再生可能で、SR表示が保存前後で一致することを確認。

---

## 仕様確定（安定化フェーズ基準 / 2026-02-06）

この節は、実装と文書の不一致を解消するための優先仕様です。
以降の安定化作業は本節を基準にします。

### 1. 再生と SRC
- 通常の List 再生は体感優先の軽量経路を使う。
- 明示的な Sample Rate 変換（右クリック Resample）と Export は品質優先で `Best` SRC を使う。
- 元 SR と同一の場合は不要な SRC を行わない。

### 2. Bits 変換
- v0 では `bit_depth_override` によるメモリ上の非破壊オーバーライドを正仕様とする。
- 実ファイルへの反映は Save/Export 実行時に行う。

### 3. ショートカット確定
- Editor の `S` は View 切り替え専用（Waveform/Spectrogram/Mel）。
- Zero Cross Snap は `R` で切り替える。
- List の `P` は Auto Play 切り替え、List の `R` は Regex 切り替え。

### 4. 計測運用
- `debug summary` の `select_to_preview_ms` / `select_to_play_ms` が `n=0` の場合は「計測不足」と扱う。
- 速度比較は p50/p95/max で記録する。

### Deprecated 記載（参照無効）
- 旧記載の「Editor の `S` で Zero Cross Snap 切り替え」は無効。
- 旧記載の「Bits 変換は常に新規ファイル生成」は無効（v0 仕様ではオーバーライド）。
- 旧記載の「SRC を全経路で同一品質で適用」は無効（軽量経路と品質経路を分離）。

---

## セッション/エクスポート仕様（安定化追記 / 2026-02-06）

### セッション復元（.nwsess）
- 復元対象:
  - リスト内容（files/items）
  - virtual（source/op_chain/spec/sidecar）
  - sample_rate_override / bit_depth_override
  - external 設定（source群、key/match、visible/show_unmatched）
  - tabs / cached edits / active tab
  - `selected_path`（選択行）
  - export policy（save_mode/conflict/backup_bak/name_template/dest_folder）
- 復元非対象（ランタイムのみ）:
  - Editor Undo/Redo スタック
  - List Undo/Redo スタック
  - 非同期ジョブ状態（inflight receiver 等）

### Ctrl+Z と保存の扱い
- `Ctrl+Z` は次の優先順で適用:
  1. List Undo/Redo
  2. Editor Undo/Redo
  3. Overwrite export 復元（`.bak` が存在する場合のみ）
- Overwrite export の復元は `.bak backup on overwrite` が有効な場合にのみ可能。
- セッション保存時に Undo スタック自体は永続化しない（メモリコストと復元複雑度を抑えるため）。

### エクスポート仕様
- SaveMode:
  - `Overwrite`: 元ファイルを置換（必要時 `.bak` 作成）
  - `NewFile`: 新規ファイル生成（conflict で Rename/Overwrite/Skip）
- 反映優先順位:
  1. 明示設定（sample_rate_override / bit_depth_override）
  2. 編集済みタブ・virtual の現在状態
  3. 元ファイルメタ
- マーカー:
  - `markers`: 書き出し時に各フォーマットの対応方式で保存
    - WAV: 埋め込み
    - MP3/M4A/OGG: sidecar/tag 方式
  - `loop markers`: WAV/MP3/M4A は書き込み対象、OGG は非対応

### テストゲート（必須）
- `cargo test -q`
- `cargo test -q --features kittest`
- 重点テスト:
  - `tests/session_virtual_restore.rs`
  - `tests/export_overwrite_undo.rs`
  - `tests/audio_convert_matrix.rs`
  - `tests/virtual_export_behavior.rs`
  - `tests/editor_trim_seek_bounds.rs`

---

# 以下を実装したい

## 小規模修正
- List表示で32floatと32intのbitの違いを表現できるようにしたいです。
- Editer表示で、Trim > Add Trim As Virtual でトリムしたときに、その選択してAdd Trim As Virtualした範囲の音声しか再生されません。Add Trim As Virtualでは元波形には手を加えず新たにファイルを作るだけにしたいです(元ファイルのEditer内での再生範囲もそのままで)。
- virtualもRename Fileできるようにしてください。
- Rename File機能では拡張子はいじれないようにしてください
- Rename FileのUIで変更後のファイル名を入力しようとしても、すぐにフォーカスが外れて入力できません。修正お願いします。
- Spec, MelSpec表示するときにサンプルレート(ナイキスト周波数)は考慮されていますか？もし考慮できていないなら修正してください
- 原因がわかんないのですが...エディタ上で少し長い曲のloopマーカーの表示の位置と音声のループ位置が違っていることがありました。(その事例では表示のループ位置があってそうで音声のループEnd位置がおかしかったです)
- Editerでシークバーの位置付近からマウスで範囲選択した場合に範囲の先頭の位置からずれます。シークバー付近からスタートしたり、シークバー付近で範囲のエンドに行くときに吸い付いたほうがいい？
- Editerで範囲選択して範囲外を押すと範囲が消えます。範囲内をクリックした際も同じように消えるようにしたいです。
- 上記デバッグもできる範囲お願いします、コード上でのデバッグのほかにもコマンド引数、スクリーンショットを駆使して確認もお願いします。
