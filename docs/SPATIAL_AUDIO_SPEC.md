# オブジェクト音声（ADM BWF / TrueHD）と Spatial 表示

オブジェクト音声のファイルを、オブジェクトの名前・位置・動きを保ったまま開き、試聴し、動きを編集する。

- 対象の形式
  - ADM BWF: ITU-R BS.2076 のメタデータ（`axml`）とトラックの表（`chna`）を持つ RIFF / RF64 / BW64 の WAVE。オブジェクト音声の制作マスターの受け渡しに使われる。
  - TrueHD（`.thd` / `.mlp`、`.m2ts` / `.mts` の TrueHD トラック）: オブジェクトの presentation も読む。cargo の機能 `truehd` のときだけ。
- 試聴: スピーカー（7.1.4 の仮想スピーカー経由）と、ヘッドフォン（既存の HRTF）。
- 編集: エディターの Spatial 表示で、位置の動き（キーフレーム）を編集する。編集はセッションに保存し、元ファイルは変えない。
- 書き出し: 編集を入れた新しい ADM BWF を書き出す。

## 決定事項

| 項目 | 決定 |
|---|---|
| UI の呼び名 | 「ADM BWF」「TrueHD」「Objects」「Spatial」。「Dolby」「Atmos」は商標なので UI に出さない（AC-3 と同じ方針） |
| 試聴の方法 | 再生のコールバックの中で、その場でオブジェクトを 7.1.4 のベッドに混ぜる。前もってレンダリングしない |
| レンダラー | 自前の部屋型パンナー（Rust）。Dolby のレンダラーとも BS.2127 とも一致しない。試聴用 |
| 編集の保存先 | セッション（.nwsess）だけ。元ファイルは書き換えない |
| 新しいファイル | 「Export ADM BWF…」で書き出す。音声はそのまま写す |
| TrueHD | 機能 `truehd`（既定のビルドには入れない）。デコーダーは truehdd の `truehd` クレート（Apache-2.0、版を固定） |
| E-AC-3（JOC） | 対象外のまま。oxideav-ac3 はオブジェクトの位置を公開しておらず、E-AC-3 はデコードしない方針 |

## 1. 読み取り

### ADM BWF（`src/adm/`）
- **判定**
  - `wav_stream::read_wave_pcm_info` がチャンクを `data` の後ろまで歩き、`chna` と `axml` の両方があれば `has_adm_chunks` を立てる。ヘッダーの読み取りと同じ 1 回の走査で済む。
  - リストの行は `chna` だけを読む（`adm::probe_summary`）。パックの型の番号から「ADM · 10 bed + 118 obj」を作る。`axml` は読まない。
- **シーン**（`adm::load_scene`、ワーカー）
  - `axml` は quick-xml で流しながら読む。数百 MB でも全体をメモリに持たない。
  - 読むもの: programme → content → object → pack → channel → block。トラックとの対応は `chna` から（`AT_…` → stream → channel、BS.2076-2 の `AC_…` の直接参照、最後の手がかりとして ID の付け方）。
  - programme が複数あれば最初のものを鳴らす（診断に残す）。programme がなければ、入れ子でない全オブジェクト。
  - ベッドの共通定義（ITU-R BS.2094、`AC_00010001`〜`AC_00010028`）は表を埋め込んでいる（`adm::common_defs`）。ファイルに書かれていない共通定義も解決する。
  - HOA / Matrix / Binaural は数えるが鳴らさない（診断に残す）。
  - 解釈しない block の子要素（width、diffuse、channelLock など）は、キーフレームの `extras` に原文のまま残す。

### TrueHD（`src/audio_truehd.rs`、機能 `truehd`）
- 入れ物
  - `.thd` / `.mlp`: ファイルそのものが TrueHD。
  - `.m2ts` / `.mts`: TrueHD の PID の PES をつないで読む（`audio_mpegts::TsTrueHdReader`）。Blu-ray が互換用に混ぜる AC-3 のフレーム（`0x0B77` で始まる PES）は捨てる。映像の時刻 0 は TrueHD の最初の PTS（`timeline_zero_pts`）。
- 行の情報: ストリームの先頭（2 MB まで）をデコードして、レート、チャンネル数、ベッドとオブジェクトの数を知る。長さは推定値。
- 再生の前に、ワーカーで全体を 24 ビットの RF64 一時ファイル（一時フォルダの `NeoWaves/truehd`）にデコードする。以後はその一時ファイルをメモリマップで鳴らす（ADM と同じ）。
  - リストで再生したときは「Decoding TrueHD…」と出し、デコードが終わったら鳴らす。
  - エディターは、デコードが終わるまで読み込み中のまま。
  - 使っていない一時ファイルは、一時フォルダの掃除で消える。
- メタデータ（OAMD）のシーンへの変換は `spatial::oamd`。機能なしでもテストできるよう、クレートの型を使わない。
  - 時刻: それまでに書いた標本数 + payload の `sample_offset` + `evo_sample_offset`（truehdd の DAMF 書き出しに従う）。
  - 各 payload の最初のブロックだけを使う（truehdd と同じ）。
  - 位置は `get_damf_pos()` から取る。これは ADM の Cartesian と同じ向き（前が Y = +1）。
- オブジェクトのない presentation は、チャンネル名からベッドだけのシーンにする。5.1 / 7.1 も同じミックスで正しいスピーカーへ行く。
- 壊れたアクセスユニットは飛ばし、数を診断に残す。

## 2. シーンのモデル（`src/spatial/scene.rs`）

- 時刻はすべて秒（`multi_edit.rs` と同じ）。
- **要素**（`Element`）
  - 鍵: ファイル自身の ID から作る（`adm:AO_1002:AC_00031001`、`thd:obj3`、`thd:bed:L`）。カウンターは使わない。
  - トラック: PCM のチャンネル（0 から）。
  - 種類: ベッド（`SpeakerPos`）かオブジェクト。
  - 座標: Cartesian か Polar。元の座標系のまま持つ。
  - 有効な区間（ADM の audioObject の start / duration）、全体のゲイン。
- **キーフレーム**（`Keyframe`）
  - `secs` は到達時刻。`secs − ramp_secs` までは前の値のまま、そこから直線で動く。
  - ADM の block（`rtime` / `duration` / `jumpPosition` / `interpolationLength`）も、TrueHD の更新（`sample_pos` / `ramp`）も、損なわずに表せる。
  - 同じ値が続くキーフレームは間引く（聞こえ方は変わらない）。
- **座標**（`src/spatial/coords.rs`）
  - ADM Cartesian: X 左 −1 / 右 +1、Y 後ろ −1 / 前 +1、Z 床 0 / 天井 +1（−1 は下）。
  - ADM Polar の方位は左が正。アプリの `SpeakerPos::direction` は右が正なので、変換を通す。
  - Polar と Cartesian の変換は BS.2127 §10 の考え方で書いた。M+030 は部屋の前左の角に来る。

## 3. 試聴（`src/object_mix.rs`）

- **流れ**
  1. ストリームしている素材のトラックを、再生ヘッドの位置で読む。
  2. 要素ごとに、その時刻の位置からゲインを求め、7.1.4 の 12ch に混ぜる。
  3. 12ch のベッドを、既存の経路に渡す。
     - スピーカーへの割り振り（`ChannelMixMatrix`、常に Auto）
     - HRTF（12ch 用のフィルター）
     - ラウドネスメーター（LFE は除く）
- ゲインは 64 フレームごとに実際の再生位置で求め直し、その間は直線で補間する。シーク・ループ・速度変更に追従し、動いてもクリックしない。
- 64ch の上限（`MAX_SOURCE_CHANNELS`）は関係ない。トラックは番号で直接読み、行列に届くのは 12ch だけ。
- ミックスは、作ったときの素材（パス、チャンネル数、長さ）と一致するときだけ使う。前のファイルのミックスが次のファイルにかかることはない。
- シーンの読み込み中は `pending`。無音で、再生位置も進めない（トラックをチャンネルとして鳴らさない）。
- Mute / Solo は要素ごと（セッションには保存しない）。
- ミニメーターは、コールバックが流すベッドの直近の音（`ObjectBedTap`）を、7.1.4 の SURROUND 表示で見せる。

### パンナー（`src/spatial/panner.rs`）
- 部屋型で、軸ごとに分けて 2 本ずつにパンする。
  - 高さ（z）で、耳の高さの層と上の層。
  - 層の中で前後（y）、列の中で左右（x）。
- どの段も sin / cos の組なので、パワーが保たれる。
- スピーカーの位置にある音は、そのスピーカーだけから出る。
- ベッドのチャンネルは、7.1.4 に同じスピーカーがあればそこへ 1.0 で、なければ本来の位置でパンする。

## 4. Spatial 表示（`src/app/ui/spatial_view.rs`）

- エディターの View で「Spatial」を選ぶ。オブジェクト音声のタブでだけ出る。Metadata と同じく、波形の代わりに全面に出る画面。
- **左**: 要素の一覧。ベッドとオブジェクトに分け、色、名前、M / S を出す。編集した要素には「•」。
- **右上**: 部屋の上面図（X / Y）と側面図（Y / Z）。
  - 再生ヘッドの時刻の、全要素の位置。
  - 選んだ要素の、前後 2 秒の軌跡。
  - 7.1.4 の仮想スピーカー。
- **右下**: 時間軸と 3 本のレーン（Cartesian は X / Y / Z、Polar は Azimuth / Elevation / Distance）。
  - ルーラーをクリック・ドラッグ: シーク。
  - ホイール: 時間方向のスクロール。Ctrl + ホイール: 拡大縮小。
- **編集**（オブジェクトだけ。ベッドは v1 では編集しない）
  - 部屋の図で点をドラッグ: 再生ヘッドの時刻にキーフレームを置く（その時刻にあれば動かす）。動く時間は「Glide」で決める。
  - レーンの点をドラッグ: 時刻と値を動かす（Shift で値だけ）。隣のキーフレームを越えない。
  - レーンをダブルクリック: 点を足す。点をダブルクリック: 消す。
  - Delete / Backspace: 選んだ点を消す。
  - 「Reset element」: その要素をファイルのとおりに戻す。
  - Ctrl+Z / Ctrl+Y: Spatial 表示の編集の履歴（タブごと、100 段）。ドラッグ 1 回が 1 段。
- 音声は読み取り専用（`media_kind::SourceContent`）。トリミング、ゲイン、形式やレートの変換、Multi Edits への追加は受け付けない。メタデータが指す標本が変わってしまうため。

## 5. セッションへの保存

- `ProjectList.spatial_edits` に、編集した要素のキーフレームだけを保存する。編集していない要素は書かない。

```json
{"path": "master.wav",
 "source": {"tracks": 128, "frames": 14400000, "file_sr": 48000},
 "elements": [{"key": "adm:AO_1002:AC_00031001",
   "keyframes": [{"t": 12.5, "ramp": 0.25, "pos": [-0.4, 0.8, 0.0]}]}]}
```

- 値が 1 のゲインと、0 のランプは書かない。
- `source` は編集したときの素材の形。形が変わったファイルでは編集を残すが当てず、Spatial 表示に「別の版のファイル用」と出す。開いただけでは書き換えない。
- 合流の規則はない。二人が同時に編集したら、文書全体の競合の選択になる（`channel_layouts` と同じ）。
- 未保存の編集があれば、終了時に確認が出る。

## 6. ADM BWF の書き出し（`src/adm/export.rs`）

- Spatial 表示の「Export ADM BWF…」、または `--cli adm export --input … --output … [--session …]`。ワーカーで動き、進捗とキャンセルがある。
- 元ファイルと同じパスには書かない。`.partial` に書いてから名前を変える。
- **チャンク**: 元ファイルと同じ並びで書く。
  - `data`: バイト単位でそのまま写す（デコードしない）。
  - `chna`、`bext`、`dbmd`、`iXML`、`LIST`、未知のチャンク: そのまま写す。
  - `axml`: 読みながら書き写す。編集した要素の `audioChannelFormat` の `audioBlockFormat` だけを、元の座標系で書き直す（`extras` も書き戻す）。ほかの部分は変えない。
  - RF64 / BW64 では `ds64` を作り直す。
- 編集がなければ、元ファイルとバイト単位で同じになる。
- TrueHD から ADM BWF への書き出しは v1 では対象外。
- 書き出した ADM BWF が、Dolby の製品で読めるとは限らない。目標は BS.2076 の一般形。

## 7. CLI

- `--cli adm inspect --input <wav> [--keyframes]`: シーン（programme、要素、トラック、キーフレーム）と診断を JSON で出す。
- `--cli adm export --input <wav> --output <wav> [--session <nwsess>]`: セッションの編集を入れて書き出す。

## 対象外（v1）

- size、divergence、zone、screenRef、trims は鳴らし方に反映しない（データには残し、書き出しでも保つ）。
- 補間するのは位置（BS.2127 はゲインを補間する）。
- ループの継ぎ目のクロスフェードの間は、末尾の位置を使う。
- ベッドの編集、オブジェクトの追加・削除、トラックの並べ替え。
- E-AC-3（JOC）、TrueHD の `.mkv`。
- TrueHD のデコード結果の使い回し（起動のたびにデコードし直す）。
