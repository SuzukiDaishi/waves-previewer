# NeoWaves Metadata Inspector / UCS / Chunk Viewer 仕様書

対象リポジトリ: https://github.com/SuzukiDaishi/waves-previewer

## 1. 目的

NeoWavesに、音声ファイルへ埋め込まれたメタデータやコンテナ内部構造を確認できる機能を追加する。

対象は主に以下。

- UCS / ASWGメタデータ
- WAVのRIFFチャンク
- `LIST/INFO`
- `LIST/adtl`
- `bext`
- `iXML`
- `cue `
- `smpl`
- `acid`
- MP3のID3フレーム
- M4A / MP4のbox / atom
- FLACのmetadata block
- OGG Vorbis Comment
- AIFF / AIFCのチャンク
- 既知だが未対応の構造
- 未知チャンク、未知フレーム、未知box、未知metadata block
- バイナリ内容の読み取り専用表示

本機能は単なる「WAV Chunk Viewer」ではなく、複数形式を横断する **Metadata Inspector** として設計する。

---

# 2. 基本方針

## 2.1 画面ごとの責務

NeoWavesには現在、主に次の2つの表示がある。

- **List表示**
  - 大量のファイルを一覧比較する
  - 検索、ソート、フィルターを行う
  - 必要な列だけをON/OFFする
- **Editor表示**
  - 1ファイルの詳細を見る
  - 波形やスペクトログラムを確認する
  - 編集を行う

今回追加するメタデータ機能は次のように分担する。

## List表示

主に「意味として解釈されたメタデータ」を列として表示する。

例:

- CatID
- Category
- SubCategory
- FX Name
- Library
- Creator ID
- Title
- Artist
- Comment
- BPM
- BWF Description
- Originator
- Marker Count
- Metadata Status

用途:

- 複数ファイルの比較
- UCS情報の確認
- ソート
- 検索
- メタデータ欠落ファイルの発見
- 値競合の発見
- 未知チャンクを含むファイルの発見

## Editor表示

主に「どこへ、どのように格納されているか」を調査する。

MetadataをEditorの一次ビューとして追加し、その中にStructure/Hexを配置する。
既存の波形・スペクトル用`ViewMode`にはMetadataを混在させない。

```text
Wave | Spectrum | Other | Metadata
                          ├─ Structure
                          └─ Hex
```

内部名:

```rust
enum EditorPrimaryView {
    Wave,
    Spectrum,
    Other,
    Metadata,
}

enum MetadataSubView {
    Structure,
    Hex,
}
```

---

# 3. 最終的な画面構成

```text
List
├─ 固定のファイル／音声情報列
├─ ON/OFF可能な正規化メタデータ列
├─ ON/OFF可能な形式固有メタデータ列
├─ 動的に検出されたメタデータ列
└─ Metadata Status / Conflict / Unknown Count

Editor
├─ Wave
├─ Spectrum
├─ Other
└─ Metadata
   ├─ Structure
   └─ Hex
```

---

# 4. メタデータの3階層

メタデータは以下の3レイヤーに分けて扱う。

## 4.1 物理構造

ファイル内部で実際に存在する構造。

例:

- RIFF chunk
- ID3 frame
- MP4 box
- FLAC metadata block
- Ogg packet
- AIFF chunk

## 4.2 意味的メタデータ

形式を横断して統一した意味。

例:

- Title
- Artist
- BPM
- CatID
- Category
- SubCategory
- FX Name
- Loop
- Marker
- Artwork

## 4.3 由来

各値がどこから取得されたか。

例:

```text
CatID = AMBForest
Source = RIFF / iXML / BWFXML / ASWG / catId
```

同じ意味の値が複数の場所に存在する場合は、すべての値と出所を保持する。

---

# 5. UCSの扱い

## 5.1 UCSはファイルフォーマットではない

UCSは主に次を定義する。

- Category
- SubCategory
- CatID
- ファイル命名規則
- 関連メタデータの運用

UCS準拠であっても、必ずファイル内部にUCS情報があるとは限らない。

次のケースがあり得る。

- ファイル名だけUCS準拠
- WAVのiXML / ASWGに格納
- MP3のID3 `TXXX`に独自運用で格納
- M4Aのfreeform atomに独自運用で格納
- FLAC / OGGのVorbis Commentに独自運用で格納
- 複数箇所へ重複格納
- 異なる値が競合

## 5.2 ASWG iXML拡張

WAVではASWGのiXML拡張が最も明確なUCS格納先になる。

主なUCS関連フィールド:

- `category`
- `subCategory`
- `catId`
- `userCategory`
- `userData`
- `vendorCategory`
- `fxName`
- `library`
- `creatorId`
- `sourceId`

例:

```xml
<BWFXML>
  <ASWG>
    <category>AMBIENCE</category>
    <subCategory>FOREST</subCategory>
    <catId>AMBForest</catId>
    <fxName>Forest Night Crickets</fxName>
    <library>Zukky Field Recordings</library>
    <creatorId>ZUKKY</creatorId>
    <sourceId>REC001</sourceId>
  </ASWG>
</BWFXML>
```

## 5.3 MP3 / M4A / FLACでのUCS

MP3、M4A、FLACにはUCS専用の統一された正式フィールドがあるわけではない。

実際には以下のような任意キーが使われ得る。

```text
CatID
CATID
UCSCatID
UCS_CATID
Category
UCSCategory
SubCategory
UCSSubCategory
FXName
Library
CreatorID
```

NeoWavesでは、キー名を正規化してエイリアス照合する。

例:

```text
normalize("UCS_CAT-ID") -> "ucscatid"
normalize("CatID")      -> "catid"
```

---

# 6. フォーマット別の格納実態

## 6.1 WAV / BWF / RF64 / BW64

WAVはRIFFコンテナであり、4文字のFourCCを持つチャンクが並ぶ。

### 優先して解釈するチャンク

| 分類 | チャンク | 内容 |
|---|---|---|
| 音声基本 | `fmt ` | 音声フォーマット |
| 音声基本 | `fact` | サンプル数など |
| 音声本体 | `data` | PCM等の音声データ |
| 大容量 | `ds64` | RF64 / BW64の64bitサイズ |
| BWF | `bext` | Description、Originator、TimeReference等 |
| 一般情報 | `LIST/INFO` | Title、Artist、Comment等 |
| マーカー | `cue ` | キューポイント |
| 追加情報 | `LIST/adtl` | `labl`、`note`、`ltxt` |
| ループ | `smpl` | ループ範囲、Unity Note等 |
| ACID | `acid` | BPM、拍数、ループ情報 |
| 制作情報 | `iXML` | Project、Scene、Take、Track等 |
| UCS / ASWG | `iXML`内 | Category、CatID、FX Name等 |
| XMP | `XMP `等 | Adobe系メタデータ |
| ADM | `axml` | ADM XML |
| ADM | `chna` | TrackとADM IDの関連 |
| Dolby | `dbmd` | Dolbyメタデータ |
| 波形概要 | `levl` / `PEAK` | Peak envelope等 |
| ID3 | `ID3 ` / `id3 ` | WAV内ID3 |
| Padding | `JUNK` / `PAD ` / `FLLR` | 埋め草や予約領域 |
| 不明 | 任意FourCC | 独自チャンク |

### RF64 / BW64対応

現行のRIFF専用処理だけではなく、以下を認識する。

- `RIFF`
- `RF64`
- `BW64`

RF64 / BW64では`ds64`を使用してサイズを補完する。

全offset、sizeは`u64`で扱う。

## 6.2 LISTチャンク

`LIST`はコンテナであり、ペイロード先頭4byteがList Typeになる。

### LIST/INFO

一般的なテキスト情報。

```text
LIST
└─ INFO
   ├─ INAM = "Forest Night"
   ├─ IART = "ZUKKY"
   ├─ ICMT = "Recorded in July"
   └─ ISFT = "REAPER"
```

解釈候補:

| FourCC | 表示名 |
|---|---|
| `INAM` | Title |
| `IART` | Artist |
| `ICMT` | Comment |
| `ICOP` | Copyright |
| `ICRD` | Creation Date |
| `IENG` | Engineer |
| `IGNR` | Genre |
| `IKEY` | Keywords |
| `IPRD` | Product |
| `ISBJ` | Subject |
| `ISFT` | Software |
| `ISRC` | Source |
| `ITCH` | Technician |
| `ITRK` | Track Number |

これらはList列として個別にON/OFF可能にする。

### LIST/adtl

`cue `と関連する追加情報。

```text
LIST
└─ adtl
   ├─ labl
   │  ├─ Cue ID = 1
   │  └─ Text = "Footstep Start"
   ├─ note
   │  ├─ Cue ID = 1
   │  └─ Text = "Clean transient"
   └─ ltxt
      ├─ Cue ID = 2
      └─ Sample Length = 48000
```

List表示では1マーカー1列にはせず、意味的に要約する。

- Marker Count
- Marker Labels
- Marker Notes
- Region Count
- Region Labels

長い値は省略し、ホバーで全文表示する。

## 6.3 MP3

MP3は通常、ID3v2タグとMPEG audio frameから構成される。

### 主なID3フレーム

| フレーム | 内容 |
|---|---|
| `TIT2` | Title |
| `TPE1` | Artist |
| `TALB` | Album |
| `TBPM` | BPM |
| `TCON` | Genre |
| `COMM` | Comment |
| `USLT` | Lyrics / Transcript |
| `APIC` | Artwork |
| `TXXX` | 任意キー／値 |
| `PRIV` | Owner付きバイナリ |
| `GEOB` | 汎用オブジェクト |
| `CHAP` | Chapter |
| `CTOC` | Chapter table |

### MP3のUCS候補

```text
TXXX:CatID
TXXX:CATID
TXXX:UCSCatID
TXXX:UCS_CATEGORY
TXXX:Category
TXXX:SubCategory
TXXX:FXName
TXXX:Library
```

### Structure表示例

```text
ID3v2.4
├─ TIT2
├─ TPE1
├─ TXXX:CatID
├─ APIC
└─ PRIV:com.vendor.tool

Audio Stream
├─ MPEG-1 Layer III
├─ Xing Header
└─ LAME Delay / Padding

Trailing Metadata
├─ APEv2
└─ ID3v1
```

対応優先度:

1. ID3v2
2. MPEG stream summary
3. Xing / Info / VBRI
4. LAME delay / padding
5. ID3v1
6. APEv2
7. 未知／独自フレーム

## 6.4 M4A / MP4

M4AはISO Base Media File Formatであり、階層的なbox / atom構造を持つ。

```text
ftyp
moov
├─ mvhd
├─ trak
│  └─ mdia
│     └─ minf
│        └─ stbl
└─ udta
   └─ meta
      └─ ilst
mdat
```

### 主なApple metadata atom

| Atom | 内容 |
|---|---|
| `©nam` | Title |
| `©ART` | Artist |
| `©alb` | Album |
| `©cmt` | Comment |
| `©gen` | Genre |
| `tmpo` | BPM |
| `covr` | Artwork |
| `trkn` | Track number |
| `disk` | Disc number |
| `cpil` | Compilation |
| `----` | Freeform metadata |

### UCS候補

```text
----
├─ mean = "com.apple.iTunes"
├─ name = "CatID"
└─ data = "AMBRoom"
```

`mean`や`name`はツールごとに異なる可能性がある。

### 実装方針

- `mp4ameta`
  - 意味的なタグ表示
  - `ilst`
  - freeform metadata
- 独自ISO-BMFF scanner
  - 全boxのoffset、size、階層
  - 未知box
  - 64bit extended size
  - `mdat`を読み込まず位置とサイズのみ保持

## 6.5 FLAC

FLACは先頭のmetadata blockと、その後の音声frameから構成される。

| Type | 名前 |
|---:|---|
| 0 | STREAMINFO |
| 1 | PADDING |
| 2 | APPLICATION |
| 3 | SEEKTABLE |
| 4 | VORBIS_COMMENT |
| 5 | CUESHEET |
| 6 | PICTURE |
| 7–126 | Reserved / Unknown |
| 127 | Invalid |

### Vorbis Comment

任意の`KEY=value`を持てる。

```text
VORBIS_COMMENT
├─ vendor = "reference libFLAC"
├─ TITLE = "Rain"
├─ CATID = "WATRain"
├─ LOOPSTART = "48000"
└─ LOOPEND = "192000"
```

表示対象:

- STREAMINFOの全項目
- PADDING size
- APPLICATION ID + payload
- SEEKTABLE entry
- VORBIS_COMMENT
- CUESHEET
- PICTURE metadata
- Reserved / Unknown block
- Audio frame start offset

## 6.6 OGG Vorbis

OGGはpageとpacketのコンテナ。

```text
Ogg Stream
├─ Identification Header
├─ Comment Header
├─ Setup Header
└─ Audio Packets
```

通常ユーザーが重要視するのはVorbis Comment。

Structure表示ではページを全件表示せず、通常モードでは要約する。

```text
Ogg Stream serial=0x12345678
├─ Pages: 164
├─ Packets: 820
├─ Comment Header
└─ Audio Data
```

Advanced表示のみpage単位の詳細を出す。

## 6.7 AIFF / AIFC

AIFFもIFF系チャンク構造。

```text
FORM AIFF
├─ COMM
├─ SSND
├─ MARK
├─ INST
├─ NAME
├─ AUTH
├─ ANNO
├─ COMT
└─ APPL
```

解釈対象:

- `COMM`
- `SSND`
- `MARK`
- `INST`
- `NAME`
- `AUTH`
- `ANNO`
- `COMT`
- `APPL`
- 未知チャンク

---

# 7. List表示の仕様

## 7.1 列カテゴリ

```text
Columns
├─ Basic
├─ UCS / ASWG
├─ General Metadata
├─ RIFF INFO
├─ BWF
├─ iXML
├─ ID3
├─ MP4
├─ FLAC / Vorbis Comment
├─ Marker / Loop
├─ Diagnostics
└─ Detected Metadata Fields
```

### UCS / ASWG

- CatID
- Category
- SubCategory
- FX Name
- Library
- Creator ID
- Source ID
- Vendor Category
- User Category
- User Data

### General Metadata

形式を横断して正規化した列。

- Title
- Artist
- Album
- Comment
- Genre
- BPM
- Key
- Time Signature
- Copyright
- Artwork
- Loop
- Marker Count

### RIFF INFO

- Title `[INAM]`
- Artist `[IART]`
- Comment `[ICMT]`
- Copyright `[ICOP]`
- Creation Date `[ICRD]`
- Engineer `[IENG]`
- Genre `[IGNR]`
- Keywords `[IKEY]`
- Product `[IPRD]`
- Subject `[ISBJ]`
- Software `[ISFT]`
- Source `[ISRC]`
- Technician `[ITCH]`
- Track Number `[ITRK]`

### BWF

- BWF Description
- Originator
- Originator Reference
- Origination Date
- Origination Time
- Time Reference
- UMID
- Coding History
- Loudness Value
- Loudness Range
- Max True Peak
- Max Momentary Loudness
- Max Short-term Loudness

### iXML

- Project
- Scene
- Take
- Tape
- Note
- File UID
- Family UID
- Original Filename
- Circled
- No Good
- Track Count

### Marker / Loop

- Marker Count
- Marker Labels
- Marker Notes
- Region Count
- Region Labels
- Loop Start
- Loop End
- Loop Length
- MIDI Unity Note

### Diagnostics

- Metadata Types
- Metadata Status
- UCS Status
- UCS Filename Match
- Conflict Count
- Unknown Chunk Count
- Warning Count
- Malformed Count

## 7.2 列プリセット

候補:

```text
Basic
UCS
Sound Library
BWF
Music
Dialogue
Diagnostics
Custom
```

### Sound Libraryプリセット

```text
File
CatID
Category
SubCategory
FX Name
Library
Creator ID
Duration
Channels
Sample Rate
LUFS
```

### デフォルト表示候補

```text
File
Duration
Channels
Sample Rate
CatID
Category
FX Name
Title
BPM
Metadata Status
```

## 7.3 列設定UI

必要な操作:

- 列名検索
- 個別ON/OFF
- グループ単位の全ON
- グループ単位の全OFF
- プリセット適用
- Custom構成保存
- 列順序変更
- 列幅保存

## 7.4 固定列と動的列

```rust
enum ListColumnSpec {
    Builtin(ColumnId),
    Metadata(MetadataFieldId),
    RawMetadata(RawMetadataKey),
}

struct RawMetadataKey {
    container: MetadataContainer,
    path: String,
}
```

### Built-in列

```text
File
Duration
Sample Rate
LUFS
```

### Normalized Metadata列

```text
Title
CatID
Category
BPM
Comment
```

### Raw Metadata列

```text
RIFF/INFO/INAM
ID3/TXXX:CatID
MP4/----:com.apple.iTunes:CatID
VORBIS_COMMENT/CATID
```

現行の固定`ColumnId`だけで全メタデータを増やさず、固定列と動的列のハイブリッドにする。

## 7.5 動的に検出したメタデータ

例:

```text
RIFF INFO / ZCAT
RIFF INFO / X123
ID3 TXXX / PROJECT_CODE
MP4 Freeform / com.company.assetId
VORBIS_COMMENT / TEAM
```

UI:

```text
Detected Metadata Fields
├─ RIFF INFO / ZCAT
├─ ID3 TXXX / PROJECT_CODE
└─ MP4 Freeform / com.company.assetId

[+ Add detected metadata column]
```

追加後は他ファイルにも同じキーを検索する。

## 7.6 値の出所

```rust
struct ListMetadataValue {
    value: String,
    source: MetadataSource,
    status: MetadataValueStatus,
}
```

ツールチップ例:

```text
Forest Night
Source: RIFF / LIST / INFO / INAM
Offset: 0x000002B4
```

## 7.7 正規化値と優先順位

Titleの例:

```text
WAV  : LIST/INFO/INAM
MP3  : ID3/TIT2
M4A  : ilst/©nam
FLAC : VORBIS_COMMENT/TITLE
AIFF : NAME
```

同一ファイル内で複数候補がある場合、全候補を保持する。

候補順位例:

```text
1. LIST/INFO/INAM
2. ID3/TIT2
3. iXML/ASWG/songTitle
4. bext/Description
5. Filename
```

意味が完全一致しないフォールバックは推定扱いにする。

## 7.8 競合表示

セル:

```text
⚠ AMBForest
```

ツールチップ:

```text
Conflicting values:
iXML/ASWG/catId = AMBForest
ID3/TXXX:CatID  = AMBWind
Filename        = AMBForest
```

## 7.9 Metadata Status列

表示例:

```text
[UCS] [BWF] [iXML] [!]
```

または:

```text
BWF + UCS
Unknown ×2
Conflict
Malformed
—
```

---

# 8. Editor / Structure表示

## 8.1 基本UI

```text
┌─────────────────────────────────────────────────────┐
│ Wave | Spectrum | Mel | Structure | Hex            │
├─────────────────────────────────────────────────────┤
│                                                     │
│                Main visualization area              │
│                                                     │
└─────────────────────────────────────────────────────┘
```

Structure:

```text
┌────────────────────────────────────┬────────────────┐
│ Structure Tree                     │ Properties     │
│                                    │                │
│ RIFF                               │ ID: iXML       │
│ ├─ fmt                             │ Offset: ...    │
│ ├─ bext                            │ Size: ...      │
│ └─ iXML                            │ Encoding: UTF8 │
│                                    │ [Open in Hex]  │
└────────────────────────────────────┴────────────────┘
```

## 8.2 Structureツリー例

```text
RIFF/WAVE
├─ fmt                          40 bytes
│  ├─ Format                   PCM
│  ├─ Channels                 2
│  ├─ Sample Rate              48000 Hz
│  └─ Bits                     24
├─ bext                        602 bytes
│  ├─ Description              Forest ambience
│  ├─ Originator               ZUKKY
│  └─ Time Reference           0
├─ LIST                        184 bytes
│  └─ INFO
│     ├─ INAM                  Forest Night
│     ├─ IART                  ZUKKY
│     └─ ICMT                  Recorded outside
├─ iXML                        2.4 KB
│  ├─ PROJECT                  My Game
│  └─ ASWG
│     ├─ category              AMBIENCE
│     ├─ subCategory           FOREST
│     ├─ catId                 AMBForest
│     └─ fxName                Forest Night Crickets
├─ Xx01                        128 bytes [Unknown]
└─ data                        154.8 MB
```

## 8.3 Structure行の情報

- 名前／ID
- 意味
- offset
- header size
- payload size
- actual readable size
- short summary
- parse status
- source path
- warning count

## 8.4 解析状態

```text
Parsed
Known / Opaque
Unknown
Malformed
Duplicate
Conflict
Unsupported
```

同一IDが複数存在しても別ノードとして保持する。

## 8.5 Structure操作

- クリック: Properties表示
- ダブルクリック: Hexへ切り替えてoffsetジャンプ
- `Open in Hex`
- UCS値から元XML要素へジャンプ
- `cue `から波形markerへジャンプ
- `smpl`からloop範囲へジャンプ
- `acid`からBPM／beat情報へジャンプ
- ListセルからStructureへジャンプ
- Hex選択からStructure nodeへ逆引き
- Structure nodeをList列へ追加

## 8.6 Properties例

```text
Selected: iXML / ASWG / catId

Value
AMBForest

Type
UTF-8 text

Source
iXML chunk

Offset
0x00000A32

Length
9 bytes

Validation
✓ Known UCS CatID
```

---

# 9. Editor / Hex表示

最初は編集機能を付けず、**Hex Viewer / Binary Viewer — Read Only** とする。

## 9.1 表示形式

```text
Offset      00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F   ASCII
00000320    69 58 4D 4C 68 09 00 00 3C 42 57 46 58 4D 4C 3E   iXMLh...<BWFXML>
00000330    0A 20 20 3C 41 53 57 47 3E 0A 20 20 20 20 3C 63   .  <ASWG>.    <c
00000340    61 74 49 64 3E 41 4D 42 46 6F 72 65 73 74 3C 2F   atId>AMBForest</
```

## 9.2 初期機能

- offset指定ジャンプ
- Structureから該当範囲へジャンプ
- 選択範囲ハイライト
- 1行16byte / 32byte切り替え
- ASCII表示
- UTF-8プレビュー
- UTF-16LE / UTF-16BEプレビュー
- Little Endian / Big Endian解釈
- `u8 / i8 / u16 / i16 / u32 / i32 / u64 / i64 / f32 / f64`
- ASCII検索
- UTF-8文字列検索
- Hex検索
- FourCC検索
- 選択範囲コピー
- payload抽出
- hash計算
- Structureへ逆ジャンプ

## 9.3 読み込み方式

ファイル全体をメモリへ読み込まない。

```rust
struct BinaryViewState {
    visible_offset: u64,
    bytes_per_row: usize,
    loaded_window: Vec<u8>,
    loaded_range: std::ops::Range<u64>,
}
```

表示位置の前後のみ読み、スクロール時に`seek + read`する。

## 9.4 巨大データ

以下は展開しない。

- WAV `data`
- M4A `mdat`
- FLAC audio frame
- OGG audio packet群
- 巨大Artwork
- 巨大未知チャンク

---

# 10. XML / Text / Binary表示

## 10.1 XML

対象:

- iXML
- XMP
- aXML
- XMLらしい未知チャンク

機能:

- 整形表示
- Raw表示
- Tree表示
- namespace表示
- 属性表示
- 未知要素の保持
- parse error位置表示

単純な文字列検索ではなく、名前空間対応XML parserを使用する。

候補:

- `quick-xml`
- `roxmltree`

## 10.2 テキスト

表示内容:

```text
Encoding
UTF-8

Terminator
NUL

Value
Forest ambience recorded at midnight.
```

文字コードが不明なら推定であることを明示する。

## 10.3 バイナリ

未知チャンクでは以下を表示する。

- offset
- size
- first bytes
- last bytes
- Hex
- ASCII
- guessed content type
- hash
- export payload

---

# 11. 共通データモデル

```rust
pub struct MetadataDocument {
    pub container: ContainerKind,
    pub nodes: Vec<MetadataNode>,
    pub normalized: Vec<NormalizedField>,
    pub warnings: Vec<MetadataWarning>,
}

pub struct MetadataNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub path: String,
    pub metadata_id: MetadataId,
    pub offset: u64,
    pub header_size: u64,
    pub declared_size: u64,
    pub readable_size: u64,
    pub kind: NodeKind,
    pub knownness: Knownness,
    pub parse_status: ParseStatus,
    pub summary: Option<String>,
    pub payload: PayloadRef,
    pub children: Vec<NodeId>,
}

pub struct PayloadRef {
    pub file_offset: u64,
    pub length: u64,
}

pub struct NormalizedField {
    pub key: MetadataFieldId,
    pub values: Vec<SourcedValue>,
}

pub struct SourcedValue {
    pub value: MetadataValue,
    pub source_path: String,
    pub raw_key: Option<String>,
    pub confidence: ValueConfidence,
    pub offset: Option<u64>,
    pub length: Option<u64>,
}
```

重要:

- `MetadataNode`へ巨大な`Vec<u8>`を持たせない
- payloadはoffsetとlengthで参照
- 必要なときだけ読み込む
- 同じ意味の複数値を保持
- raw valueとnormalized valueを分ける

---

# 12. Metadata Registry

```rust
registry.register_riff(*b"bext", decode_bext);
registry.register_riff(*b"iXML", decode_ixml);
registry.register_riff(*b"LIST", decode_list);
registry.register_id3("TXXX", decode_txxx);
registry.register_flac(4, decode_vorbis_comment);
registry.register_mp4(*b"ilst", decode_ilst);
```

登録がないものはUnknownとしてStructure / Hexで表示する。

---

# 13. モジュール構成案

```text
src/metadata/
├─ mod.rs
├─ model.rs
├─ scanner.rs
├─ registry.rs
├─ normalize.rs
├─ cache.rs
├─ diagnostics.rs
├─ search.rs
├─ formats/
│  ├─ riff.rs
│  ├─ aiff.rs
│  ├─ id3.rs
│  ├─ isobmff.rs
│  ├─ flac.rs
│  └─ ogg.rs
└─ decoders/
   ├─ fmt.rs
   ├─ bext.rs
   ├─ list.rs
   ├─ list_info.rs
   ├─ list_adtl.rs
   ├─ cue.rs
   ├─ smpl.rs
   ├─ acid.rs
   ├─ ixml.rs
   ├─ aswg.rs
   ├─ xmp.rs
   ├─ axml.rs
   ├─ chna.rs
   ├─ id3_frames.rs
   ├─ mp4_items.rs
   ├─ vorbis_comment.rs
   └─ artwork.rs
```

UI:

```text
src/app/ui/editor/
├─ metadata_structure.rs
├─ hex_viewer.rs
├─ metadata_properties.rs
└─ metadata_search.rs

src/app/ui/list/
├─ metadata_columns.rs
└─ metadata_column_settings.rs
```

---

# 14. パフォーマンス設計

## 14.1 一覧読み込み時

読み込むもの:

- container / codec summary
- ONになっている正規化メタデータ
- メタデータ種類
- warning count
- unknown count
- Artwork有無
- loop / marker summary

読み込まないもの:

- WAV `data`
- MP4 `mdat`
- FLAC audio frame
- OGG audio packet群
- Artwork本体
- 巨大XML全文
- 巨大未知chunk payload
- `JUNK` / `PAD ` payload

## 14.2 Editorを開いた時

- structure headerを遅延走査
- payloadは選択時のみ読む
- XMLは展開時のみparse
- Artworkは表示時のみdecode
- Hexはwindow単位
- hashはstream計算

## 14.3 キャッシュ

```rust
struct MetadataCacheEntry {
    file_size: u64,
    modified_time: SystemTime,
    normalized: HashMap<MetadataFieldId, CachedValue>,
    raw_text_fields: HashMap<RawMetadataKey, CachedValue>,
    diagnostics: MetadataDiagnostics,
    detected_types: Vec<MetadataType>,
}
```

キャッシュキー:

- canonical path
- file size
- modified time

## 14.4 必要な列だけ解析

```text
CatID列 ON
→ WAV: iXML / ASWG候補
→ MP3: ID3 TXXX候補
→ M4A: ilst / freeform
→ FLAC: Vorbis Comment
```

## 14.5 バックグラウンド解析

- worker thread
- visible row優先
- selected file最優先
- cancellation
- generation IDで古い結果を破棄
- row単位で部分反映

---

# 15. 現行実装からの重要な変更点

## 15.1 WAVを全読み込みしない

Metadata Inspector用scannerは`Read + Seek`を使う。

保持するもの:

- ID
- offset
- header size
- declared size
- actual readable size
- parent / hierarchy
- parse status

payloadは必要時のみ読む。

## 15.2 iXMLをXML parserで読む

単純な`<TAG>text</TAG>`検索では以下に弱い。

- namespace
- prefix
- whitespace
- CDATA
- entity
- 同名要素
- attribute
- nested field
- ASWG lowerCamelCase

## 15.3 Fixed ColumnIdの拡張

固定列だけでなく動的列specを導入する。

---

# 16. 正規化ルール

## 16.1 キーの正規化

```rust
fn normalize_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}
```

UCS alias候補:

```text
catid
ucscatid
categoryid
ucscategoryid

category
ucscategory

subcategory
ucssubcategory

fxname
ucsfxname

library
ucslibrary

creatorid
ucscreatorid

sourceid
ucssourceid
```

## 16.2 Value confidence

```rust
enum ValueConfidence {
    EmbeddedExact,
    EmbeddedAlias,
    CrossFormatMapped,
    FilenameParsed,
    Inferred,
}
```

---

# 17. ListとEditorの連携

## 17.1 ListからStructure

セルのコンテキストメニュー:

```text
Open Metadata Source
Open in Structure
Open in Hex
Copy Value
Copy Source Path
```

## 17.2 StructureからList

ノード上で:

```text
Add as List Column
```

を提供する。

## 17.3 StructureとHex

- Structure選択からoffsetジャンプ
- payload範囲ハイライト
- Hex選択から最小Structure nodeを逆引き
- parent path表示

---

# 18. Waveformとの連携

- `cue `選択 → markerへ移動
- `LIST/adtl/labl` → 対応markerを強調
- `smpl` → loop範囲表示
- `acid` → BPM / beat grid
- `bext.TimeReference` → time reference表示
- M4A edit list → timeline offset表示
- encoder delay / padding → timing risk表示

---

# 19. 保存・編集方針

初期は読み取り専用。

編集を追加する場合、書き込み先を明示する。

```text
Write CatID to:
[x] iXML / ASWG
[ ] ID3 TXXX
[ ] LIST/INFO
[ ] Filename

[Preview Changes]
```

勝手に全フィールドを同期しない。

---

# 20. 未知チャンクの保持

未知チャンクは:

- 表示する
- 削除しない
- 勝手に解釈しない
- payloadを任意変更しない
- export可能
- hash可能
- malformedなら警告

---

# 21. Diagnostics

## Container

- chunk size exceeds file
- invalid padding
- truncated header
- invalid FourCC
- overlapping box
- recursive box depth over limit
- invalid extended size
- duplicate singleton chunk
- missing required chunk
- RF64 without ds64
- ds64 mismatch

## Metadata

- invalid UTF-8
- invalid XML
- unknown XML namespace
- duplicate key
- conflicting value
- invalid numeric format
- invalid UCS CatID
- UCS filename mismatch
- invalid date
- invalid timecode
- marker references missing cue ID
- loop end <= loop start
- unknown ID3 encoding
- malformed APIC
- unknown MP4 data type

## Security / robustness

- XML entity expansion禁止
- maximum nesting depth
- maximum child count
- maximum text size
- maximum artwork preview size
- maximum payload preview size
- malformed fileでpanicしない

---

# 22. 実装フェーズ

## Phase 1: WAV Structure / Hex基盤

1. 共通`MetadataDocument`
2. `PayloadRef`
3. streaming RIFF scanner
4. RIFF / RF64 / BW64
5. Editor `Structure`
6. Editor `Hex`
7. Structure→Hexジャンプ
8. unknown chunk表示
9. payload export
10. diagnostics基盤

対象decoder:

- `fmt `
- `fact`
- `data`
- `ds64`
- `bext`
- `LIST`
- `LIST/INFO`
- `LIST/adtl`
- `cue `
- `smpl`
- `acid`
- `iXML`

## Phase 2: UCS / ASWG / List列

1. namespace対応iXML parser
2. ASWG 1.1 parser
3. UCS alias resolver
4. CatID / Category / SubCategory / FX Name列
5. LIST/INFO列
6. BWF列
7. Marker / Loop summary
8. Metadata Status列
9. Conflict表示
10. List→Structureジャンプ
11. metadata cache

## Phase 3: MP3 / M4A / FLAC / AIFF / OGG

### MP3

- ID3v2 frame tree
- TXXX
- APIC
- COMM
- USLT
- PRIV
- GEOB
- Xing / VBRI / LAME
- ID3v1
- APEv2

### M4A

- ISO-BMFF box tree
- extended size
- `ilst`
- freeform
- `covr`
- `mdat` lazy表示

### FLAC

- 全metadata block
- APPLICATION
- CUESHEET
- PICTURE
- unknown block

### AIFF

- chunk tree
- MARK / INST
- NAME / AUTH / ANNO / COMT
- APPL

### OGG

- logical stream summary
- Vorbis Comment
- optional advanced page view

## Phase 4: Dynamic Metadata Columns

1. Detected Metadata Fields
2. raw metadata列追加
3. StructureからAdd as List Column
4. custom column persistence
5. session／global設定
6. metadata field search

## Phase 5: 編集

1. metadata edit preview
2. 書き込み先選択
3. safe rewrite
4. backup
5. validation
6. unknown metadata保持検証
7. batch edit

---

# 23. MVP提案

## List

- CatID
- Category
- SubCategory
- FX Name
- Title
- BWF Description
- Originator
- Marker Count
- Metadata Status
- Unknown Chunk Count

## Structure

- WAV
- RIFF / RF64 / BW64
- `fmt `
- `bext`
- `LIST/INFO`
- `LIST/adtl`
- `cue `
- `smpl`
- `acid`
- `iXML`
- ASWG
- unknown chunk

## Hex

- read-only
- offset jump
- selection
- ASCII
- integer interpretation
- search
- payload export

---

# 24. 推奨UI用語

```text
Wave
Spectrum
Mel
Structure
Hex
```

設定項目:

```text
Metadata Columns
Metadata Status
Detected Fields
Open Metadata Source
Add as List Column
```

---

# 25. 最終判断

## Metadata Summaryの主表示先

**List表示を主とする。**

理由:

- 複数ファイルを比較できる
- CatIDなどをソートできる
- 欠落や競合を見つけやすい
- Sound Library用途に適している
- Editorを開かず確認できる

## Editor側

**Structure / Hexを詳細調査用に追加する。**

理由:

- 格納位置を確認できる
- LISTやiXMLの階層を理解できる
- 未知チャンクを調べられる
- バイナリを直接確認できる
- Listセルからsourceへジャンプできる

## 最終構成

```text
List
└─ メタデータの意味・比較・検索

Editor / Structure
└─ コンテナ構造・チャンク階層・解釈結果

Editor / Hex
└─ 実バイト列・offset・未知データ
```

この分離により、普段の作業はListだけで完結し、問題のあるファイルや未知メタデータだけEditorで深掘りできる。

---

# 26. 実装確定事項（2026-07-29）

本章は、それ以前の案と競合する場合に優先する。

## 26.1 準拠資料

- RF64/BW64: ITU-R BS.2088-2
  - https://www.itu.int/rec/R-REC-BS.2088/en
- UCS: Universal Category System 8.2.1
  - https://universalcategorysystem.com/
- ASWG iXML Extension: 1.1
  - https://github.com/Sony-ASWG/iXML-Extension

キャッシュキーにはparser schema、UCS版、aliasデータchecksum、ASWG版を含める。
これらのいずれかが変わった場合、旧キャッシュを再利用しない。

## 26.2 Read-only境界

- inspect、summary、payload read/search/hashは入力ファイルを開く際に書き込み権限を要求しない。
- extractだけはユーザーが指定した出力先へ書き込む。
- extractは同じフォルダーの`.part`へ1 MiB単位で書き、完了後にrenameする。
- 既存出力の置換には`--overwrite`を必須とする。
- NeoWaves自身のmetadata cacheは入力音声とは別のcacheディレクトリだけに書く。
- 入力ファイルのsize、mtime、内容を変更しないことをCLI統合テストで保証する。

## 26.3 解析境界

- コンテナ判定は拡張子ではなくsignatureを使用する。
- すべてのoffset、size、checked arithmeticは`u64`を使用する。
- nodeは`PayloadRef { file_offset, length }`だけを保持し、巨大payloadを保持しない。
- XMLはDOCTYPE/DTDを拒否し、16 MiB、深度128、100,000 node、text 4 MiBを上限とする。
- 上限到達や切り詰めは`Failed`ではなく原則`Partial`とdiagnosticで表現する。
- PCM/IEEE Float WAVだけがsource frameとraw byteの厳密対応を提供する。
- 圧縮音声はsource timeと音声コンテナを表示し、圧縮bitstreamとの厳密対応不可を明示する。

## 26.4 公開CLI

既存の`item meta`契約は維持し、次を追加する。

```text
item metadata inspect --input AUDIO
item metadata summary --input AUDIO [--field KEY]... [--include-raw]
item metadata payload read --input AUDIO (--node-path PATH | --offset BYTE --length BYTES)
item metadata payload search --input AUDIO ... --kind ascii|utf8|hex|fourcc --query QUERY
item metadata payload hash --input AUDIO ... --algorithm md5|sha256
item metadata payload extract --input AUDIO ... --output FILE [--overwrite]
```

JSONのbyte offset/sizeは精度を失わない10進文字列と`*_hex`を返す。
