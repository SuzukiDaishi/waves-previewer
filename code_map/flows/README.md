# Flows

主要な操作がどのファイルを通るかの流れです。

## 起動

1. `src/main.rs` が native entrypoint。
2. `src/cli.rs` が GUI flags と `--cli` command を解析。
3. GUI の場合は `src/app/app_init.rs` / `src/app/startup.rs` で `WavesPreviewer` を構築。
4. `src/app.rs` の `eframe::App::update()` から `src/app/frame_ops.rs` 経由で各 UI / job drain が進む。

確認ポイント:
- 起動時の初期フォルダ、open file、open session は `src/cli.rs`, `src/app/app_init.rs`, `src/app/session_ops.rs`。
- prefs / theme は `src/app/theme_ops.rs`。

## フォルダスキャンとリスト構築

1. UI または CLI から folder open。
2. `src/app/scan_ops.rs` が scan job を開始し、結果を段階的に app state へ反映。
3. `src/app/list_state_ops.rs` が row/path/item index を保つ。
4. `src/app/meta_ops.rs` が duration、sample rate、artwork、transcript などを worker pool で補完。
5. `src/app/ui/list.rs` と `src/app/ui/list/table.rs` が可視範囲を描画。

確認ポイント:
- 大量件数の UI 負荷は `src/app/search_ops.rs`, `src/app/logic.rs`, `src/app/ui/list/table.rs`, `src/app/meta_ops.rs`。
- path 表示や selection lookup は `src/app/list_state_ops.rs`。

## 検索・ソート

1. Search box / sort UI から検索条件や sort key が更新される。
2. `src/app/search_ops.rs` が debounce / scheduled refresh を扱う。
3. `src/app/logic.rs` 側で filter / sort 結果が反映される。
4. `src/app/ui/list/table.rs` が filtered rows を仮想化描画する。

確認ポイント:
- `--dummy-list` 相当の大量件数では UI frame 内 O(n) / O(n log n) が問題になりやすい。
- sort key の見える化や row lookup は `src/app/list_state_ops.rs` と合わせて確認する。

## リスト再生

1. list selection / autoplay から再生対象が決まる。
2. `src/app/list_preview_ops.rs` が preview decode / full decode job を扱う。
3. `src/audio_io.rs` が音声 decode、`src/audio.rs` が playback buffer と出力を扱う。
4. `src/app/logic.rs` / `src/app/preview_ops.rs` が重い preview result を drain する。

確認ポイント:
- MP3/M4A/OGG など圧縮音源は `src/audio_io.rs` の progressive decode と `src/app/list_preview_ops.rs` の接続を見る。
- stale job cancel、prefix 再生、full handoff は UI 応答性に直結する。

## エディタ open / decode / render

1. tab open は `src/app/tab_ops.rs`、editor decode 起動は `src/app/editor_decode_ops.rs`。
2. progressive / streaming decode の結果が `EditorTab` state に入る。
3. 波形 cache / pyramid / spectrogram / feature viewport が必要に応じて構築される。
4. `src/app/ui/editor.rs` が canvas、timeline、tool panel、selection、overlay を描画する。

確認ポイント:
- 初回表示の詰まりは `src/app/editor_decode_ops.rs`, `src/app/editor_viewport.rs`, `src/app/spectrogram_jobs.rs`, `src/app/ui/editor.rs`。
- 大きな `clone()` や viewport slice copy は UI thread の frame drop 要因になりやすい。

## エディタ preview / apply

1. `src/app/ui/editor.rs` の tool 操作から `src/app/tool_ops.rs` / `src/app/editor_ops.rs` に入る。
2. 重い preview / overlay は `src/app/preview.rs` / `src/app/preview_ops.rs` が担当。
3. destructive apply は `src/app/editor_ops.rs` で background job と結果反映を扱う。
4. apply 後は waveform / playback buffer / tab state を再構築する。

確認ポイント:
- Undo snapshot、overlay 保持、apply 後 cache rebuild は memory と待ち時間に影響する。
- 再生 buffer の整合性は `src/audio.rs`, `src/app/audio_ops.rs`, `src/app/editor_ops.rs` を合わせて確認する。

## Spectrogram / Feature Viewport

1. `src/app/ui/editor.rs` が表示モードや viewport に応じて job を要求。
2. `src/app/spectrogram_jobs.rs` / `src/app/editor_viewport.rs` が worker へ計算を投げる。
3. `src/app/render/spectrogram.rs` / `src/app/render/music_features.rs` が画像化や描画補助を行う。
4. 結果は tab state / texture cache に入り、次 frame 以降で描画される。

確認ポイント:
- tile result の drain 量、fallback image 生成、texture 更新は frame budget と合わせて見る。
- cancel / generation check がない経路は古い job の計算継続に注意。

## Rename / Resample

1. list context menu や topbar menu から rename / resample が要求される。
2. dialog の表示は `src/app/frame_ops.rs` が担当。
3. rename 実処理は `src/app/rename_ops.rs` が担当し、path index、cache、tab、selection を更新する。
4. sample-rate override と bulk resample は `src/app/resample_ops.rs` が担当し、大量件数は frame budget 付きで分割処理する。

確認ポイント:
- list focus / keyboard の抑止は `src/app/ui/list/navigation.rs` の modal 判定も見る。
- rename 後の cache 移行は `replace_path_in_state()` の対象漏れに注意する。
- bulk resample の progress / cancel 表示は `src/app/loading_ops.rs` と `src/app/ui/topbar/status.rs`。

## Session 保存・復元

1. UI / CLI / IPC から session open/save が要求される。
2. `src/app/session_ops.rs` が drag-drop、IPC、open queue、save dialog などを扱う。
3. `src/app/project.rs` が serde-friendly な保存形式を読み書きする。
4. `src/app/types.rs` の保存対象 state 変更は migration / 互換性確認が必要。

確認ポイント:
- ファイル名は `.nwsess` が現行名。
- `project*` 関数名や型名は legacy だが、意味は session。

## CLI / Headless Render

1. `src/cli.rs` が command tree と args を定義する。
2. `src/app/cli_ops.rs` が session/list/editor/render/export/effect graph などの実処理を持つ。
3. `src/app/cli_workspace.rs` が headless 用 workspace state を補助する。
4. render 系は `src/app/render/*`, `src/audio_io.rs`, editor/list state と接続する。

確認ポイント:
- CLI 仕様を変える時は `src/cli.rs` の args と `src/app/cli_ops.rs` の実装を両方見る。
- GUI と CLI の出力差分は session/list/editor state の共有箇所を確認する。
