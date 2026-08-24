# NeoWaves Audio List Editor (NeoWaves)

NeoWaves は大量の音声ファイルを素早く一覧表示し、即試聴・編集できる軽量オーディオリストエディタです。UI は `eframe/egui`、オーディオ出力は `cpal` を使用しています。

対応フォーマット（デコード）:
- WAV / AIFF / FLAC / MP3 / M4A (isomp4) / AAC / ALAC / OGG (Vorbis)
- 動画コンテナ MP4 / MOV / M4V / 3GP / 3G2 — **音声トラックのみ再生**。
  エディタでは Mini Meter に再生位置の映像フレームを表示する（読み込み専用）

対応フォーマット（エンコード / 書き出し）:
- WAV / AIFF / FLAC / MP3 / M4A (AAC) / OGG (Vorbis)

フォーマットごとのメタ情報（loop marker / marker / BPM / artwork など）の対応状況は `docs/FORMAT_SUPPORT.md` を参照してください。

---

## Playback Principle

NeoWaves は「加工済み音は offline render、未加工の pristine WAV は即再生を優先」という hybrid 方針です。

- dry な physical WAV で、`Speed` モードかつ dirty state / preview overlay / SR override / bit-depth override / per-file gain が無い場合だけ、exact-stream transport を許可します。
- 上記 exact-stream では callback 側で許可する可聴処理は `source_sr / out_sr` に基づく rate 補正と master output volume のみです。ソース ch 数が出力 ch 数を超える場合のチャンネル折り畳み（余剰 ch の平均）と、エディタのチャンネル mute/solo（寄与 ch の選択。フォールドダウンは可聴 ch のみを平均）はマッピングであり DSP には含めません。
- Sample Rate 変換、PitchShift、TimeStretch、VST/CLAP preview/apply、per-file gain 反映、preview overlay、編集結果の再生はすべて full offline render 後の buffer だけを再生します。
- passive な list selection や loading UI は progressive でも構いませんが、sample が変わる経路では未完成波形をそのまま再生しません。
- callback 内 plugin / callback 内 pitch-time 処理 / callback 内 per-file gain / callback 内 sample-changing DSP は設計上禁止です。

---

## 主な機能

### リストビュー（高速）
- フォルダ/ファイルの読み込み（ドラッグ&ドロップ対応）
- 検索（Regex対応）、ソート、列の表示/非表示
- メタ情報の列表示: 長さ / チャンネル / SR / Bits / Bitrate / dBFS / LUFS / Gain / 波形
- Auto Play やキーボード操作で高速試聴

### 動画ファイル（読み込み専用）
- mp4 / mov などをリストに読み込み、音声トラックだけを再生
- エディタの Mini Meter に再生位置のフレームを表示（音と同期）
- サムネイルは埋め込みアートワーク優先、無ければ 1 フレーム目
- 映像エンコーダを持たないため、編集・書き出しは不可

### エディタ（非破壊）
- Speed / PitchShift / TimeStretch
- Fade / Trim / Normalize / LoudNorm
- Sample Rate 変換（Apply まではメモリ上のみ）
- マーカー / ループ編集 / ループ解除（Unwrap）
- スペクトログラム / メルスペクトログラム表示

### 外部データ連携（CSV / Excel）
- CSV/Excel を読み込み、列をリストにマッピング
- シート選択、ヘッダ行/データ開始行の指定
- 正規表現キー + スコープで高速マッピング
- 未参照行の表示切り替え

### セッション保存（.nwsess）
- 作業状態（開いていたファイル、選択、編集状態など）を復元
- Ctrl+S: セッション保存
- Ctrl+Shift+S: セッション Save As
- Ctrl+E: 音声の Export
- `.nwsess` はダブルクリック/ドラッグ&ドロップ対応

---

## 画面イメージ
![](docs/gamen_a.png)
![](docs/gamen_b.png)

---

## 使い方（基本）

- **Folder... / Files...** から読み込み
- **ドラッグ&ドロップ** で追加読み込み
- **Space** で再生/停止
- **Enter** でエディタを開く

> 詳細な操作は `docs/CONTROLS.md` を参照してください。

---

## ビルド

```bash
git submodule update --init --recursive
cargo build
```

ビルド後の実行ファイル:

- `.\target\debug\neowaves.exe`
- `.\target\release\neowaves.exe`

### Windows ビルド前提
- Rust toolchain (`stable-x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools（MSVC C++ / Windows SDK）
- このプロジェクトは **MSVC 動的ランタイム (/MD)** 前提です（ONNX Runtime と整合させるため）

このリポジトリには `.cargo/config.toml` で `-Ctarget-feature=-crt-static` を固定しています。  
環境変数 `RUSTFLAGS` で `+crt-static` を上書きするとリンクエラーになります。

### Windows での典型エラー（LNK2038 RuntimeLibrary mismatch）
`MD_DynamicRelease` と `MT_StaticRelease` の不一致が出る場合は以下を確認してください。
1. `echo %RUSTFLAGS%`（PowerShell は `$env:RUSTFLAGS`）で `+crt-static` が入っていないこと
2. `cargo clean`
3. 再度 `cargo build --release`

`signalsmith-stretch` は git submodule として管理しています。  
初回 clone 後は `git submodule update --init --recursive` を実行してください。

### Installer (Windows)
```powershell
.\commands\build_installer.ps1
```

出力:
- `installer\\out\\installer_<buildid>\\NeoWaves-Setup-<version>-<buildid>.exe`

補足:
- `build_installer.ps1` は `ISCC` の `Resource update error ... EndUpdateResource failed (110)` を検知した場合、再試行します。
- 再試行中に失敗が続く場合は出力先を `%TEMP%` 配下へ切り替えて継続します（最終 `OutputDir` はログに表示）。
- スクリプトの最後に更新 smoke checklist を出します。既存版の上書きインストール、設定保持、関連付け、shell-open の確認に使ってください。

---

## CLI / 自動化

通常起動は GUI です。

```bash
neowaves.exe
neowaves.exe --open-folder "C:\\path\\to\\wav" --open-first
```

headless CLI は `--cli` で入ります。`stdout` は JSON、画像系は PNG を保存して絶対パスを返します。

```bash
neowaves.exe --cli --help
neowaves.exe --cli list query --folder "C:\\path\\to\\wav"
neowaves.exe --cli batch loudness plan --session ".\\work.nwsess" --query "_BGM" --target-lufs -24
neowaves.exe --cli item inspect --input ".\\debug\\gui_test_440.wav"
neowaves.exe --cli render waveform --input ".\\debug\\gui_test_440.wav" --output ".\\debug\\cli-renders\\wave.png"
neowaves.exe --cli effect-graph list
```

repo 内から直接使う場合:

```powershell
.\target\release\neowaves.exe --cli --help
```

CLI の仕様書:
- `docs/CLI_MASTER_PLAN.md`
- `docs/CLI_COMMAND_REFERENCE.md`
- `docs/CLI_MIGRATION_MATRIX.md`
- `docs/CLI_HELP_SPEC.md`

## Repo-Local Skills

repo には NeoWaves CLI を LLM と人間の両方で扱いやすくする repo-local skill を `.agents/skills/` 配下に同梱しています。

- `cli-session-workflow`
- `batch-loudness`
- `effect-graph-authoring`
- `loop-authoring`
- `list-query-review`
- `external-merge-review`
- `transcript-batch-generate`
- `music-analysis-markers`
- `plugin-draft-preview`
- `render-editor-review`
- `verify-loop-tags`
- `effect-graph-test`
- `plugin-search-paths`

これらの skill も `neowaves.exe` 前提で書かれており、詳細な command surface は `docs/CLI_COMMAND_REFERENCE.md` と `docs/CLI_AGENT_WORKFLOWS.md` を参照します。

---

## Docs

全ドキュメント一覧:
- `docs/INDEX.md`

## ライセンス

NeoWaves is released under the MIT License. See `LICENSE` for details.
If this software was useful to you, you have the right to buy the author a drink.

## Code Layout

- `src/main.rs` はネイティブ起動の入口だけを持ち、CLI 引数解析は `src/cli.rs` に分離しています。
- `src/app.rs` は app state / trait shell を持ち、起動時構築は `src/app/app_init.rs`、フレーム進行は `src/app/frame_ops.rs`、タブ起動は `src/app/tab_ops.rs`、editor decode orchestration は `src/app/editor_decode_ops.rs` に委譲しています。
- top bar UI は `src/app/ui/topbar/` 配下の `menus.rs` / `transport.rs` / `status.rs` に分割されています。
- list UI は `src/app/ui/list.rs` を orchestration に寄せ、フォーカス/キーボード制御とテーブル定義を `src/app/ui/list/navigation.rs` / `src/app/ui/list/table.rs` へ切り出しています。
- staged split を継続中の大物ファイルは `src/app/ui/editor.rs`、`src/app/ui/effect_graph.rs`、`src/app/effect_graph_ops.rs` です。局所性の高い backend 系は巨大関数を先に削りつつ段階分割します。

---

## ライセンス補足（Third-party）

アプリ内の **Help → Licenses...** に、依存している全 657 コンポーネントのライセンス全文と、
商用配布時に別途対応が要る項目の一覧を表示します。表示データは
`assets/licenses/third_party.json` にコミット済みのスナップショットで、ビルド時に
`include_str!` で埋め込まれます（ビルド時・実行時ともネットワーク不要）。

依存を追加・更新したら再生成してコミットしてください:

```powershell
git submodule update --init --recursive   # 初回のみ
cargo install cargo-about --locked --features cli
pwsh ./commands/generate_licenses.ps1
```

`cargo-about` は `about.toml` の `accepted` に無いライセンスを見つけると失敗します。
GPL 依存が紛れ込んだらリリースではなくここで止まる、という設計です。

- 生成対象外（crate ではないもの）は `assets/licenses/extra.json` に手書きで管理します。
  `-sys` crate が同梱ビルドする C/C++ ソース、インストーラが配る DLL、フォント、
  埋め込みデータ、実行時ダウンロードするモデル、Steinberg VST 3 の扱いなど。
- `signalsmith-stretch` は submodule で取り込み、上流ライセンスをそのまま保持しています。
  - `vendor/signalsmith-stretch/LICENSE.md`
  - `vendor/signalsmith-stretch/signalsmith-stretch/LICENSE.txt`

### 商用配布で別途対応が要るもの

NeoWaves 本体は MIT ですが、**配布バイナリ全体が MIT というわけではありません**。
詳細と最新の状態は Help → Licenses の "Commercial distribution notes" を参照してください。

| 対象 | 内容 |
| --- | --- |
| Cisco OpenH264（feature `video`、既定 ON） | ソースからビルドしているため、Cisco の特許料肩代わり（Cisco 配布バイナリ限定）の対象外。AVC/H.264 の特許義務は配布者側 |
| Fraunhofer FDK AAC（M4A/AAC 書き出し） | ライセンス第 3 条が特許不許諾を明記。商用の AAC エンコード/デコードには Via LA の別途ライセンスが必要 |
| LAME / `mp3lame-encoder`（MP3 書き出し） | LGPL-3.0。静的リンクした配布物には再リンク手段の提供と LGPL/GPL 全文の同梱が必要（MP3 特許は 2017 年失効済） |
| Steinberg VST 3（feature `plugin_native_vst3`） | SDK ソースは非同梱だが、商用 VST 3 ホストは Steinberg のライセンス契約（無償・要署名）か GPLv3 が通例 |
