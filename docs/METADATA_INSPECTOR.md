# Metadata Inspector

NeoWavesのMetadata Inspectorは、入力音声を変更せずにコンテナ構造、
埋め込みメタデータ、raw byteを確認するEditorビューです。拡張子ではなく
ファイルsignatureからWAV/RIFF/RF64/BW64、MP3/ID3、M4A/MP4、FLAC、
Ogg/Vorbis、AIFF/AIFCを判定します。判定できないファイルもgeneric binary
として開けます。

## Editor

Editor上部の`View`から`Metadata`を選び、`Structure`または`Hex`へ切り替えます。

- `Structure`
  - 左側は物理コンテナツリー、右側は選択nodeの詳細です。
  - WAVの`fmt `chunkは解析結果とCLIには保持しますが、Structure一覧では
    重複する音声形式情報として表示しません。
  - node名は見出し、decoded値はその下の独立したカードとして表示します。
    折りたたんだ状態でも主要値を確認でき、Propertiesもラベルを上、値を下に
    配置したカードで区別します。
  - 詳細は`Properties / Decoded / Text / Waveform / Hex`を切り替えられます。
  - text、XML、binary先頭32 byte、artwork、音声payloadのミニ波形を内容に
    応じて表示します。
  - `cue `のsample、`smpl`のloop範囲、`acid`のtempoから、既存のWave再生位置・
    選択範囲・BPM gridへ移動できます。仮想音声や編集済みbufferでは無効です。
  - `Add as List Column`で正規化値またはraw logical pathを一覧列へ登録できます。
- `Hex`
  - 16/32 byte行、ASCII、offset jump、node範囲、検索結果を表示します。
  - OFFSET、byte index、Hex、ASCIIは固定座標の等幅グリッドで描画し、行方向・
    列方向の位置を揃えます。
  - 縦スクロールバーはHex内容の幅に関係なく画面右端へ固定し、常時表示します。
  - ASCIIの右隣には、全音声のsource timeを上から下へ配置した固定の縦波形と
    シークバーを表示します。Hexをスクロールしても時間軸は固定され、クリックまたは
    ドラッグでsource timeへシークできます。
  - 64 KiB単位でバックグラウンド読込し、前後ページをprefetchします。
  - 行を選ぶと、そのbyteを含む最小Structure nodeを逆引きできます。
  - 再生中のPCM/Float frameを含む行だけを単一の背景色で強調します。
    frame/bit/channelの詳細はHex表の上には重複表示しません。
  - `再生位置に自動スクロール`をONにすると、再生中の行が表示領域の中央付近へ
    追従します。OFFでは手動スクロール位置を維持します。

PCM/IEEE Float WAVを同じ未編集の実ファイルから再生している場合、
source frame、絶対offset、`data`相対offset、チャンネル別byte範囲、raw bits、
デコード値をStructure詳細で表示します。自動スクロールは既定でOFFです。圧縮音声や編集
previewではsource timeだけを表示し、圧縮bitstreamとの厳密対応は行いません。

## List

Metadataの右詳細または`Detected Fields`から追加した列は、global registryへ
保存されます。列の表示状態・順序・幅はSessionに保存されます。

- `normalized:<key>`: `normalized:ucs.cat_id`などの正規化値
- `raw:<format>:<logical-path>`: 形式固有nodeの値

Partialな値には`~`、競合値には`!`を表示します。tooltipには候補値とsource
path/offsetを表示します。

## CLI

```powershell
neowaves --cli item metadata inspect --input .\audio.wav
neowaves --cli item metadata summary --input .\audio.wav --field ucs.cat_id
neowaves --cli item metadata payload read --input .\audio.wav --offset 0 --length 64 --format hex
neowaves --cli item metadata payload search --input .\audio.wav --offset 0 --length 65536 --kind fourcc --query iXML
neowaves --cli item metadata payload hash --input .\audio.wav --offset 0 --length 65536 --algorithm sha256
neowaves --cli item metadata payload extract --input .\audio.wav --node-path /RIFF/WAVE/iXML --output .\ixml.xml
```

同じlogical pathが複数ある場合は`--occurrence`で選びます。JSONのbyte offset
とsizeは10進文字列と`*_hex`で返すため、JavaScriptの整数精度を超える値も
失われません。extractは既存出力に`--overwrite`がない場合は失敗します。

## 安全性と制限

- inspect、summary、read、search、hashは入力をread-onlyで開きます。
- extractは`.part`へ書いて完了後にrenameします。入力自身や入力と同じ
  file identityを持つ出力は拒否します。
- XMLはDOCTYPE/DTDを拒否し、16 MiB、深度128、100,000 node、text 4 MiBを
  上限にします。上限到達は`Partial`とdiagnosticで示します。
- Oggのcomment packetがpageをまたぐ場合、logical valueは再構成しますが、
  packet nodeのpayloadはpage headerを含む物理spanです。
- 公式のUCS 8.2.1 Full List（753 CatID）を、カテゴリ、サブカテゴリ、説明、
  synonymとともにオフライン同梱しています。版、取得元workbookのSHA-256、
  変換後データのSHA-256をcache keyへ固定し、正式表記への正規化、
  `WOOD-HANDLE`系aliasの解決、membership検証をネットワークなしで行います。
- 仮想音声は元実ファイルが残っている場合だけ`Source metadata`を表示します。
  現在の編集bufferと一致しない可能性があり、合成Structureは作りません。
