# フォーマット別対応マトリクス (FORMAT_SUPPORT)

最終更新: 2026-07-03 (FLAC 対応追加時)

NeoWaves が扱う音声フォーマットごとの、デコード / エンコード / メタ情報
(loop marker・marker・BPM・artwork 等) の対応状況と、
「そのフォーマットが native に持てないメタ情報を書き出し時にどう扱うか」の方針をまとめる。

対応拡張子の一覧は `src/audio_io.rs` の `SUPPORTED_EXTS`
(`wav / aiff / aif / flac / mp3 / m4a / ogg`) に一元化されており、
ファイルダイアログ・ドラッグ&ドロップ・フォルダスキャン・セッション復元・CLI は
すべてここを参照する。拡張子を増やす場合はこの定数と
`installer/NeoWaves.iss` の関連付け、`badges.rs` / `row_menu.rs` の UI を更新する。

## 1. オーディオ本体

| フォーマット | デコード | エンコード (書き出し) | 備考 |
| --- | --- | --- | --- |
| WAV | hound + symphonia (`pcm`) | hound: 16/24-bit PCM, 32-bit float | 唯一 exact-stream 再生・sparse proxy 読みに対応 |
| AIFF / AIF | symphonia (`aiff`) | 自前 writer: 16/24-bit PCM (AIFF), 32-bit float (AIFC `fl32`) | |
| FLAC | symphonia (`flac`) | flacenc: 16-bit / 24-bit 整数 | FLAC は float 非対応のため 32f 指定・未指定は 24-bit に量子化。9ch 以上は非対応 (仕様上限 8ch) |
| MP3 | symphonia (`mp3`) | mp3lame CBR (96–320 kbps, 設定値) | ステレオまで (3ch 以上は先頭 2ch) |
| M4A (AAC) | fdk-aac (mp4 demux) → symphonia fallback (`isomp4`/`aac`/`alac`) | fdk-aac AAC-LC CBR | ステレオまで。ALAC はデコードのみ |
| OGG (Vorbis) | symphonia (`ogg`/`vorbis`) | vorbis_rs quality-VBR | ステレオまで |

## 2. Loop marker (単一サスティンループ)

読み書きの入口は `src/loop_markers.rs` (`read_loop_markers` / `write_loop_markers`)。

| フォーマット | 格納先 | 読み | 書き |
| --- | --- | --- | --- |
| WAV | `smpl` チャンク (native) | ✓ | ✓ |
| AIFF | `MARK` + `INST` チャンク (native) | ✓ | ✓ |
| FLAC | Vorbis comment `LOOPSTART` / `LOOPEND` | ✓ | ✓ |
| MP3 | ID3v2.4 `TXXX` `LOOPSTART` / `LOOPEND` | ✓ | ✓ |
| M4A | freeform atom `com.apple.iTunes:LOOPSTART/LOOPEND` | ✓ | ✓ |
| OGG | sidecar `<stem>.loop.json` | ✓ | ✓ |

- FLAC / MP3 / M4A の `LOOPSTART`/`LOOPEND` はサンプル単位の値で、
  RPG ツクール等で使われる一般的な慣習に合わせている。
- OGG は既存ファイルの Vorbis comment を書き換えるには Ogg ページの再構築
  (再 mux) が必要なため、当面 sidecar JSON にフォールバックする。
  以前は「unsupported loop marker format」エラーになり保存全体が失敗扱い
  だったが、sidecar 書き込みに変更した。
  将来 native 対応するなら、エンコード時に comment を埋め込む
  (`VorbisEncoderBuilder` にタグを渡す) + 既存ファイルは再 mux、が候補。

## 3. Marker (cue ポイント列)

読み書きの入口は `src/markers.rs` (`read_markers` / `write_markers`)。

| フォーマット | 格納先 | 読み | 書き |
| --- | --- | --- | --- |
| WAV | `cue ` + `LIST/adtl` `labl` チャンク (native) | ✓ | ✓ |
| それ以外 (AIFF / FLAC / MP3 / M4A / OGG) | sidecar `<stem>.markers.json` | ✓ | ✓ |

- 検討メモ:
  - AIFF は `MARK` チャンクで native 表現が可能 (現在 loop 用に 2 点のみ使用)。
    任意個の marker を `MARK` に載せる拡張は将来候補。
  - FLAC の `CUESHEET` ブロックは CD-DA 前提 (588 サンプル境界等) のため
    汎用 marker には不向き。native 化するなら Vorbis comment に独自キー
    (例: `NEOWAVES_MARKERS=json`) を載せる方が安全。
  - MP3/M4A/OGG に native の cue 表現は事実上無いため sidecar 継続。

## 4. その他メタ情報 (読み取り)

| フォーマット | BPM | アートワーク | その他 |
| --- | --- | --- | --- |
| WAV | `acid` チャンク → ID3 fallback | ID3 `APIC` | `bext`/`iXML` 等は上書き保存時に保持 (下記 §5) |
| AIFF | – | – | |
| FLAC | Vorbis comment `BPM` / `TEMPO` | `PICTURE` ブロック (先頭) | |
| MP3 | ID3 `TBPM` | ID3 `APIC` | |
| M4A | `tmpo` | `covr` | |
| OGG | – | – | |

## 5. 書き出し時のメタ情報引き継ぎ (carry-over)

音声を再エンコードして保存 (上書き / 新規 / フォーマット変換) した後の
メタ情報の扱い。入口は `wave::copy_audio_metadata_from_source` と
export 側の marker / loop 再書き込み (`export_ops.rs`)。

| 変換 | 引き継がれるもの |
| --- | --- |
| WAV → WAV | `fmt `/`data`/`fact` 以外の全チャンクをマージ保持 (`bext`, `iXML`, `acid`, `smpl`, `cue `, `LIST`, `JUNK`…) |
| MP3 → MP3 | ID3 タグ全体 (タイトル・アートワーク・loop TXXX 含む) |
| M4A → M4A | mp4ameta タグ全体 (title・bpm・covr・freeform 含む) |
| FLAC → FLAC | `VORBIS_COMMENT` + `PICTURE` ブロック |
| AIFF → AIFF | (未対応 — 将来: `COMM`/`SSND` 以外のチャンク保持) |
| クロスフォーマット (例 wav → flac) | タグ類は引き継がれない。ただしエディタ上の marker / loop region は保存フローが書き出し後に書き直すため失われない |

- FLAC → FLAC で `SEEKTABLE` / `CUESHEET` はコピーしない
  (音声ストリームのオフセットに依存し、再エンコード後は不正になるため)。

### 書き出しポリシー (整理)

1. native 表現があるメタは native に書く (上表 §2〜§4)。
2. native 表現が無いメタは sidecar JSON に書く
   (`<stem>.markers.json` / `<stem>.loop.json`)。読み込み時に自動で拾う。
   sidecar はファイル移動時に一緒に運ぶ必要がある点に注意。
3. メタ書き込みの失敗は保存失敗としてカウントする (音声は書けている場合でも
   UI 上 failed 表示)。sidecar 化により「フォーマット非対応」での失敗は
   発生しなくなり、失敗は実 I/O エラーのみになる。
4. 音声ストリームに依存するメタ (FLAC `SEEKTABLE` 等) は再エンコード時に
   破棄 (エンコーダが必要なら再生成) する。
5. lossy (MP3/M4A/OGG) は 2ch までなので 3ch 以上は先頭 2ch を書き出す
   (stderr に警告)。FLAC は 8ch まで、WAV/AIFF は制限なし。
6. FLAC は float を表現できないため、32-bit float 指定は 24-bit 整数へ量子化。

## 6. フォーマット依存のふるまい (現状の意図的な差)

| 挙動 | 対象 | 理由 |
| --- | --- | --- |
| exact-stream 再生 (offline render を経ない直接再生) | WAV のみ | README「Playback Principle」参照 |
| Convert Bits メニュー | WAV のみ | bit-depth override は WAV writer の概念。FLAC 16/24 対応は将来候補 |
| list preview の SRC 品質を Fast に落とす | MP3 / M4A / OGG | lossy デコードのレイテンシ対策。FLAC は lossless なので通常品質 |
| editor デコード戦略 CompressedProgressiveFull | MP3 / OGG | フレーム境界が不定なため。FLAC は streaming overview 経路 |
| stem タイミングリスク警告 (`source_audio_has_timing_risk`) | MP3 / AAC / M4A / MP4 / OGG / Opus / WMA | encoder delay があるフォーマットのみ。FLAC/WAV/AIFF は正確 |
| 録音の保存 | WAV 固定 | 録音パイプラインの仕様 |

## 7. インストーラ / OS 関連付け

`installer/NeoWaves.iss` の "assoc" タスクで
`.wav / .aiff / .aif / .flac / .mp3 / .m4a / .ogg / .nwsess` を
ProgId `NeoWaves.Audio` に関連付け + `OpenWithProgids` / `SupportedTypes` 登録。
(2026-07-03: それまで `.aiff/.aif/.ogg` が漏れていたのを修正、`.flac` を追加)

ドラッグ&ドロップ:

- **アプリへのドロップ**: `session_ops::handle_dropped_files` →
  `SUPPORTED_EXTS` 判定。全対応フォーマットが対象。
- **アプリからのドラッグ (Windows)**: 実ファイルは canonical パスをそのまま
  ドラッグ (拡張子非依存)。virtual アイテムや pending gain 付きは
  一時 WAV に実体化してからドラッグ。
