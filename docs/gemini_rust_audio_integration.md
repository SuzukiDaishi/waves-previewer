# Gemini API × Rust 音声解析統合設計

## 1. 目的

Rust製音声管理・編集アプリケーションからGemini APIを利用し、以下の機能を実装する。

1. 似た音検索
2. 音声素材の `SE / Voice / Music / Other` 分類
3. SE素材へのUCSタグ付け
4. Voice素材の文字起こし
5. Music素材の歌詞文字起こし

対象アプリケーションは、Rust + eguiで構築された音声管理・編集ツールを想定する。

---

## 2. 基本方針

Geminiにすべての処理を任せるのではなく、役割を分離する。

| 担当 | 主な役割 |
|---|---|
| Gemini API | 意味理解、分類、タグ候補生成、文字起こし、Embedding生成 |
| Rustアプリ | ファイル管理、ジョブ管理、キャッシュ、検索、UI、編集、承認 |
| ローカルDSP | 波形解析、切り出し、音量処理、ステム分離、書き出し |
| ユーザー | UCSタグやファイル名変更などの最終承認 |

Geminiは音声の意味理解に使用し、サンプル単位の精密なDSP処理には使用しない。

---

## 3. 全体アーキテクチャ

```text
egui UI
  │
  ├─ AI解析要求
  │
  ▼
AiOrchestrator
  │
  ├─ ジョブキュー
  ├─ キャッシュ確認
  ├─ APIレート制御
  └─ 結果の検証
       │
       ▼
GeminiProvider
  │
  ├─ 音声分類
  ├─ Embedding生成
  ├─ UCSタグ付け
  ├─ Voice文字起こし
  └─ Music歌詞文字起こし
       │
       ▼
SQLite / セッションデータ
```

eguiの描画スレッドから直接HTTPリクエストを実行しない。

API呼び出しはTokioランタイム上のバックグラウンドワーカーで処理し、チャンネルを通してUIへ結果を返す。

---

## 4. 推奨ディレクトリ構成

```text
src/
├── ai/
│   ├── mod.rs
│   ├── provider.rs
│   ├── client.rs
│   ├── models.rs
│   ├── jobs.rs
│   ├── cache.rs
│   ├── prompts.rs
│   │
│   └── gemini/
│       ├── mod.rs
│       ├── audio.rs
│       ├── embedding.rs
│       ├── files.rs
│       └── response.rs
│
├── audio/
│   ├── analysis.rs
│   ├── separation.rs
│   └── waveform.rs
│
└── ui/
    └── ai_panel.rs
```

---

## 5. Cargo依存関係

```toml
[dependencies]
anyhow = "1"
async-trait = "0.1"
base64 = "0.22"
mime_guess = "2"

reqwest = {
    version = "0.12",
    features = [
        "json",
        "multipart",
        "rustls-tls"
    ]
}

schemars = "1"

serde = {
    version = "1",
    features = ["derive"]
}

serde_json = "1"
sha2 = "0.10"

tokio = {
    version = "1",
    features = [
        "rt-multi-thread",
        "macros",
        "fs",
        "sync",
        "time"
    ]
}

tracing = "0.1"
thiserror = "2"
```

APIキーをOSの資格情報ストアに保存する場合は追加する。

```toml
keyring = "3"
secrecy = "0.10"
zeroize = "1"
```

SQLiteを使用する場合の例。

```toml
rusqlite = {
    version = "0.32",
    features = ["bundled"]
}
```

大量のEmbeddingをHNSWで検索する場合の候補。

```toml
hnsw_rs = "0.3"
```

---

## 6. APIキー管理

開発時は環境変数を使用する。

### PowerShell

```powershell
$env:GEMINI_API_KEY="YOUR_API_KEY"
```

### Rust

```rust
let api_key = std::env::var("GEMINI_API_KEY")
    .map_err(|_| anyhow::anyhow!("GEMINI_API_KEYが設定されていません"))?;
```

リリース版では平文設定ファイルへの保存を避け、`keyring`でWindows Credential ManagerやmacOS Keychainに保存する。

APIキーをログへ出力してはいけない。

---

## 7. Provider抽象化

Gemini固有のコードをアプリケーション全体へ広げない。

```rust
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn classify_audio(
        &self,
        path: &Path,
    ) -> anyhow::Result<AudioClassification>;

    async fn embed_audio(
        &self,
        path: &Path,
    ) -> anyhow::Result<AudioEmbedding>;

    async fn embed_text(
        &self,
        text: &str,
    ) -> anyhow::Result<AudioEmbedding>;

    async fn assign_ucs_tags(
        &self,
        path: &Path,
        candidates: &[UcsEntry],
    ) -> anyhow::Result<UcsAnalysis>;

    async fn transcribe_voice(
        &self,
        path: &Path,
    ) -> anyhow::Result<VoiceTranscript>;

    async fn transcribe_lyrics(
        &self,
        path: &Path,
    ) -> anyhow::Result<LyricsTranscript>;
}
```

この構成にすると、将来的に以下を切り替えられる。

- Gemini API
- ローカルWhisper
- ローカル音声Embeddingモデル
- 別クラウドAPI
- テスト用Mock Provider

---

## 8. 共通データ型

### 8.1 音声種別

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "PascalCase")]
pub enum AudioKind {
    Se,
    Voice,
    Music,
    Other,
}
```

---

### 8.2 音声分類

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct AudioClassification {
    pub primary_class: AudioKind,

    pub contains_se: bool,
    pub contains_voice: bool,
    pub contains_music: bool,

    pub confidence: f32,
    pub description: String,
}
```

複合素材を扱うため、単一クラスだけでなく包含フラグを持たせる。

例：

```json
{
  "primary_class": "Music",
  "contains_se": false,
  "contains_voice": true,
  "contains_music": true,
  "confidence": 0.98,
  "description": "女性ボーカルを含むポップ楽曲"
}
```

---

### 8.3 Embedding

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
)]
pub struct AudioEmbedding {
    pub model: String,
    pub dimensions: usize,
    pub values: Vec<f32>,
}
```

---

### 8.4 UCSタグ

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct UcsCandidate {
    pub category_id: String,
    pub subcategory_id: String,
    pub confidence: f32,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct UcsAnalysis {
    pub category_id: String,
    pub subcategory_id: String,
    pub descriptors: Vec<String>,
    pub confidence: f32,
    pub alternatives: Vec<UcsCandidate>,
}
```

---

### 8.5 Voice文字起こし

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,

    pub speaker: Option<String>,
    pub text: String,
    pub emotion: Option<String>,

    pub confidence: f32,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct VoiceTranscript {
    pub language: String,
    pub full_text: String,
    pub segments: Vec<TranscriptSegment>,
}
```

---

### 8.6 Music歌詞文字起こし

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct LyricsSegment {
    pub start_ms: u64,
    pub end_ms: u64,

    pub text: String,
    pub section: Option<String>,

    pub confidence: f32,
    pub uncertain_words: Vec<String>,
}

#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    JsonSchema,
)]
pub struct LyricsTranscript {
    pub languages: Vec<String>,
    pub full_text: String,
    pub segments: Vec<LyricsSegment>,
}
```

---

## 9. Geminiクライアント

REST APIは`reqwest`で呼び出す。

APIのモデル名・エンドポイント・リクエスト形式は変更される可能性があるため、Gemini固有部分を`GeminiClient`へ閉じ込める。

```rust
use anyhow::{Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

#[derive(Clone)]
pub struct GeminiClient {
    http: Client,
    api_key: String,

    generation_model: String,
    embedding_model: String,
}

impl GeminiClient {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .context("GEMINI_API_KEYが設定されていません")?;

        let http = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .context("HTTPクライアントを作成できませんでした")?;

        Ok(Self {
            http,
            api_key,

            // 実際の導入時に利用可能な最新モデル名を確認する。
            generation_model: "GEMINI_AUDIO_MODEL".to_owned(),
            embedding_model: "GEMINI_EMBEDDING_MODEL".to_owned(),
        })
    }

    pub async fn post_json<TRequest, TResponse>(
        &self,
        url: &str,
        body: &TRequest,
    ) -> Result<TResponse>
    where
        TRequest: Serialize + ?Sized,
        TResponse: DeserializeOwned,
    {
        let response = self
            .http
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(body)
            .send()
            .await
            .context("Gemini APIへの接続に失敗しました")?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("レスポンスの読み取りに失敗しました")?;

        if !status.is_success() {
            let message = String::from_utf8_lossy(&bytes);

            anyhow::bail!(
                "Gemini API error {status}: {message}"
            );
        }

        serde_json::from_slice(&bytes)
            .context("APIレスポンスのJSON解析に失敗しました")
    }
}
```

---

## 10. 音声送信方法

音声の送信方法は2種類に分ける。

### 10.1 インライン送信

短いSEやVoice向け。

```text
音声ファイル
  ↓
Base64変換
  ↓
JSONリクエスト内へ埋め込み
```

利点：

- 実装が簡単
- 1回のHTTPリクエストで完結
- 短い音声に向く

欠点：

- Base64化でデータ量が増える
- 大きい音楽ファイルには不向き
- 同じ音声を複数回解析すると毎回再送信になる

---

### 10.2 Files API

長いMusic素材や、同じファイルを複数回解析する場合に使用する。

```text
ファイルアップロード
  ↓
file URIを取得
  ↓
分類・文字起こし・タグ付けから参照
```

アップロード済みURIは永続保存せず、有効期限を考慮する。

アプリ側では次のような一時キャッシュを持つ。

```rust
pub struct UploadedFileCache {
    pub content_hash: String,
    pub remote_uri: String,
    pub uploaded_at_unix_ms: u64,
}
```

---

## 11. インライン音声の読み込み

```rust
use anyhow::{Context, Result};
use base64::{
    engine::general_purpose::STANDARD,
    Engine,
};
use std::path::Path;

pub struct InlineAudio {
    pub mime_type: String,
    pub base64_data: String,
}

pub async fn load_inline_audio(
    path: &Path,
) -> Result<InlineAudio> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| {
            format!(
                "音声ファイルを読み込めません: {}",
                path.display()
            )
        })?;

    let mime_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("audio/wav")
        .to_owned();

    Ok(InlineAudio {
        mime_type,
        base64_data: STANDARD.encode(bytes),
    })
}
```

---

## 12. SE / Voice / Music / Other分類

### 分類ルール

#### SE

- 効果音
- Foley
- 環境音
- UI音
- 衝突音
- 足音
- 機械音
- ドア音
- 武器音

#### Voice

- セリフ
- ナレーション
- 掛け声
- 呼吸
- 会話
- 群衆音声

#### Music

- 楽曲
- ジングル
- スティンガー
- リズムループ
- 楽器フレーズ
- 歌唱入り音楽

#### Other

- 無音
- テスト信号
- 破損ファイル
- 分類不能
- 目的不明のノイズ

---

### プロンプト例

```text
この音声素材をゲームサウンドライブラリ向けに分類してください。

primary_classは必ず次のいずれかです。

- Se
- Voice
- Music
- Other

歌入り楽曲はMusicにしてください。

音楽の上に音声がある場合は、
primary_classをMusicにし、
contains_voiceをtrueにしてください。

confidenceは0.0から1.0です。

descriptionは日本語で簡潔に記述してください。
```

---

### Rust関数

```rust
pub async fn classify_audio(
    provider: &dyn AiProvider,
    path: &std::path::Path,
) -> anyhow::Result<AudioClassification> {
    provider.classify_audio(path).await
}
```

構造化出力を使用し、自由形式のJSONを手作業で抽出しない。

---

## 13. 似た音検索

## 13.1 処理フロー

```text
音声ファイル
  ↓
Embedding生成
  ↓
L2正規化
  ↓
SQLiteへ保存
  ↓
コサイン類似度検索
```

テキスト検索も同じEmbedding空間へ変換する。

```text
「重い金属ドアが閉まる音」
  ↓
テキストEmbedding
  ↓
音声Embeddingとの類似度検索
```

---

## 13.2 L2正規化

```rust
pub fn normalize_l2(
    mut values: Vec<f32>,
) -> Vec<f32> {
    let norm = values
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();

    if norm > f32::EPSILON {
        for value in &mut values {
            *value /= norm;
        }
    }

    values
}
```

---

## 13.3 コサイン類似度

L2正規化済みなら、内積をコサイン類似度として使用できる。

```rust
pub fn cosine_similarity(
    a: &[f32],
    b: &[f32],
) -> anyhow::Result<f32> {
    if a.len() != b.len() {
        anyhow::bail!(
            "Embedding次元が異なります: {} != {}",
            a.len(),
            b.len()
        );
    }

    Ok(
        a.iter()
            .zip(b)
            .map(|(x, y)| x * y)
            .sum()
    )
}
```

---

## 13.4 総当たり検索

数万ファイル程度までは、最初は総当たりでも実装可能。

```rust
pub struct SearchResult<'a> {
    pub file_id: &'a str,
    pub similarity: f32,
}

pub fn search_similar<'a>(
    query: &[f32],
    records: &'a [(String, Vec<f32>)],
    limit: usize,
) -> anyhow::Result<Vec<SearchResult<'a>>> {
    let mut results = records
        .iter()
        .map(|(file_id, embedding)| {
            Ok(SearchResult {
                file_id,
                similarity: cosine_similarity(
                    query,
                    embedding,
                )?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    results.sort_by(|a, b| {
        b.similarity.total_cmp(&a.similarity)
    });

    results.truncate(limit);

    Ok(results)
}
```

---

## 13.5 将来の高速化

ファイル数が増えたら次へ移行する。

- HNSW
- Qdrant
- LanceDB
- PostgreSQL + pgvector
- 独自SIMD検索

最初から外部ベクトルDBを必須にせず、SQLite + 総当たりでMVPを作る。

---

## 14. SEのUCSタグ付け

UCSタグはGeminiに自由生成させない。

正規のUCSカテゴリ一覧をアプリ側で保持し、その候補内だけから選択させる。

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
)]
pub struct UcsEntry {
    pub category_id: String,
    pub subcategory_id: String,
    pub description: String,
}
```

---

### 14.1 一段階方式

候補数が少ない場合。

```text
以下の候補に存在するcategory_idとsubcategory_idだけを使用してください。

候補にないIDを生成してはいけません。

最適候補と次点候補を返してください。
```

---

### 14.2 二段階方式

候補数が多い場合はこちらを推奨する。

```text
第1段階
  ↓
大カテゴリ分類

第2段階
  ↓
選択した大カテゴリ内の
サブカテゴリ分類
```

利点：

- プロンプトを短くできる
- APIコストを抑えられる
- 存在しないUCS IDの生成を防げる
- 分類結果を検証しやすい

---

### 14.3 AI候補と確定タグを分ける

```rust
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
)]
pub struct UcsMetadata {
    pub ai_suggestion: Option<UcsAnalysis>,
    pub confirmed_category_id: Option<String>,
    pub confirmed_subcategory_id: Option<String>,
    pub manually_edited: bool,
}
```

人間が確定したタグを再解析で上書きしない。

---

## 15. Voice文字起こし

### 出力項目

- 言語
- 全文
- セグメント
- 開始時刻
- 終了時刻
- 話者
- 感情
- 信頼度

---

### プロンプト例

```text
音声をできるだけ忠実に文字起こししてください。

要件:

- 言語を判定する
- セグメントごとに開始・終了時刻を返す
- 複数話者の場合はspeakerを設定する
- 笑い、息、叫びなどは必要に応じて記述する
- 聞き取れない語句を推測で確定しない
- confidenceは0.0から1.0
- full_textには全体の文字起こしを入れる
```

---

### ゲームボイス向け拡張

将来的に次のタグを追加できる。

```rust
pub enum VoiceDelivery {
    Normal,
    Whisper,
    Shout,
    Scream,
    Cry,
    Laugh,
    Growl,
    Breath,
}
```

追加候補：

- Battle Voice
- Damage Voice
- Death Voice
- Narration
- Dialogue
- System Voice
- Crowd
- Radio Voice

---

### SRT出力

```rust
pub fn transcript_to_srt(
    transcript: &VoiceTranscript,
) -> String {
    transcript
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                index + 1,
                format_srt_time(segment.start_ms),
                format_srt_time(segment.end_ms),
                segment.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_srt_time(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;

    format!(
        "{hours:02}:{minutes:02}:{seconds:02},{millis:03}"
    )
}
```

---

## 16. Music歌詞文字起こし

歌詞の文字起こしはVoiceより難しい。

主な原因：

- 楽器によるマスキング
- リバーブ
- ディレイ
- ハモリ
- コーラス
- ボーカルチョップ
- 叫び声
- 発音の崩し
- 日本語と英語の混在
- 長い母音
- 同音異義語

---

### 16.1 推奨処理

```text
楽曲
  ↓
Music判定
  ↓
必要に応じて区間分割
  ↓
歌詞文字起こし
  ↓
境界部分の統合
  ↓
全体文脈で補正
  ↓
LRC / SRT出力
```

---

### 16.2 区間分割

長い楽曲は30〜60秒程度に分割し、境界を5〜10秒重複させる。

```text
Chunk 1: 00:00 - 00:45
Chunk 2: 00:38 - 01:23
Chunk 3: 01:16 - 02:01
```

重複区間を照合して、単語の欠落や重複を修正する。

---

### 16.3 ボーカルステムの利用

精度を上げるには、ローカルでボーカル分離してから送信する。

```text
原曲
  ↓
ステム分離
  ├─ Vocal
  └─ Instrumental

Vocal
  ↓
Gemini
  ↓
歌詞文字起こし
```

利用モード案：

```rust
pub enum LyricsInputMode {
    OriginalMix,
    VocalStem,
    OriginalAndVocalStem,
}
```

`OriginalAndVocalStem`では、ボーカルステムを単語認識に使い、原曲を曲構成や文脈の理解に使う。

---

### 16.4 曖昧な単語を保持する

```json
{
  "text": "きみのこえを探して",
  "confidence": 0.52,
  "uncertain_words": [
    "こえ"
  ]
}
```

聞き取れない箇所を勝手に確定しない。

---

## 17. AIジョブキュー

### ジョブ種別

```rust
#[derive(
    Debug,
    Clone,
)]
pub enum AiJobKind {
    GenerateEmbedding,
    ClassifyAudio,
    AssignUcsTags,
    TranscribeVoice,
    TranscribeLyrics,
}
```

---

### ジョブ状態

```rust
#[derive(
    Debug,
    Clone,
)]
pub enum AiJobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}
```

---

### ジョブ本体

```rust
#[derive(Debug, Clone)]
pub struct AiJob {
    pub id: u64,
    pub file_id: String,
    pub path: std::path::PathBuf,
    pub content_hash: String,

    pub kind: AiJobKind,
    pub status: AiJobStatus,

    pub retry_count: u32,
}
```

---

### UIからワーカーへのコマンド

```rust
pub enum AiCommand {
    Classify {
        file_id: String,
        path: std::path::PathBuf,
    },

    Embed {
        file_id: String,
        path: std::path::PathBuf,
    },

    AssignUcs {
        file_id: String,
        path: std::path::PathBuf,
    },

    TranscribeVoice {
        file_id: String,
        path: std::path::PathBuf,
    },

    TranscribeLyrics {
        file_id: String,
        path: std::path::PathBuf,
    },

    Cancel {
        job_id: u64,
    },
}
```

---

### ワーカーからUIへのイベント

```rust
pub enum AiEvent {
    Started {
        job_id: u64,
        file_id: String,
    },

    Progress {
        job_id: u64,
        file_id: String,
        message: String,
    },

    Completed {
        job_id: u64,
        file_id: String,
        result: AiAnalysisResult,
    },

    Failed {
        job_id: u64,
        file_id: String,
        error: String,
    },
}
```

---

## 18. eguiとの連携

egui側はイベントを毎フレーム非同期に確認する。

```rust
while let Ok(event) = self.ai_worker.event_rx.try_recv() {
    match event {
        AiEvent::Completed {
            file_id,
            result,
            ..
        } => {
            self.apply_ai_result(
                &file_id,
                result,
            );

            ctx.request_repaint();
        }

        AiEvent::Failed {
            file_id,
            error,
            ..
        } => {
            self.ai_errors.insert(
                file_id,
                error,
            );

            ctx.request_repaint();
        }

        _ => {}
    }
}
```

`.await`を`eframe::App::update`内で直接実行しない。

---

## 19. キャッシュ設計

APIコスト削減のため、音声内容のハッシュをキーにする。

パスをキーにすると、ファイル移動や名前変更で再解析が発生する。

```rust
use sha2::{
    Digest,
    Sha256,
};

pub async fn calculate_content_hash(
    path: &std::path::Path,
) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(path).await?;

    let mut hasher = Sha256::new();
    hasher.update(bytes);

    Ok(format!("{:x}", hasher.finalize()))
}
```

キャッシュキー：

```text
content_hash
+ model
+ operation
+ schema_version
+ prompt_version
```

例：

```rust
pub struct AiCacheKey {
    pub content_hash: String,
    pub model: String,
    pub operation: String,
    pub schema_version: u32,
    pub prompt_version: u32,
}
```

プロンプト変更後に古い結果を誤利用しないよう、`prompt_version`を含める。

---

## 20. SQLite設計

### 20.1 Embedding

```sql
CREATE TABLE audio_embeddings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    file_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,

    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,

    vector BLOB NOT NULL,

    created_at TEXT NOT NULL,

    UNIQUE (
        content_hash,
        model,
        dimensions
    )
);
```

---

### 20.2 AI解析結果

```sql
CREATE TABLE audio_ai_analysis (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    file_id TEXT NOT NULL,
    content_hash TEXT NOT NULL,

    operation TEXT NOT NULL,
    model TEXT NOT NULL,

    schema_version INTEGER NOT NULL,
    prompt_version INTEGER NOT NULL,

    result_json TEXT NOT NULL,
    created_at TEXT NOT NULL,

    UNIQUE (
        content_hash,
        operation,
        model,
        schema_version,
        prompt_version
    )
);
```

---

## 21. EmbeddingのBLOB変換

```rust
pub fn embedding_to_bytes(
    values: &[f32],
) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| {
            value.to_le_bytes()
        })
        .collect()
}

pub fn embedding_from_bytes(
    bytes: &[u8],
) -> anyhow::Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        anyhow::bail!(
            "不正なEmbedding BLOBです"
        );
    }

    Ok(
        bytes
            .chunks_exact(4)
            .map(|chunk| {
                f32::from_le_bytes([
                    chunk[0],
                    chunk[1],
                    chunk[2],
                    chunk[3],
                ])
            })
            .collect()
    )
}
```

---

## 22. 推奨処理フロー

### インポート直後

```text
1. ファイルハッシュ計算
2. キャッシュ確認
3. Embedding生成
4. SE / Voice / Music / Other分類
5. 結果保存
```

---

### 分類後の遅延処理

```text
SE
  ↓
UCSタグ付け

Voice
  ↓
文字起こし

Music
  ↓
歌詞文字起こし

Other
  ↓
説明のみ
```

すべてをインポート時に実行しない。

必要な解析のみ遅延実行することで、APIコストと待ち時間を削減する。

---

## 23. 解析関数の例

```rust
pub enum DetailedAnalysis {
    Ucs(UcsAnalysis),
    Voice(VoiceTranscript),
    Lyrics(LyricsTranscript),
    None,
}

pub struct CompleteAnalysis {
    pub embedding: AudioEmbedding,
    pub classification: AudioClassification,
    pub detailed: DetailedAnalysis,
}

pub async fn analyze_imported_audio(
    provider: &dyn AiProvider,
    path: &std::path::Path,
    ucs_candidates: &[UcsEntry],
) -> anyhow::Result<CompleteAnalysis> {
    let embedding = provider
        .embed_audio(path)
        .await?;

    let classification = provider
        .classify_audio(path)
        .await?;

    let detailed = match classification.primary_class {
        AudioKind::Se => {
            DetailedAnalysis::Ucs(
                provider
                    .assign_ucs_tags(
                        path,
                        ucs_candidates,
                    )
                    .await?,
            )
        }

        AudioKind::Voice => {
            DetailedAnalysis::Voice(
                provider
                    .transcribe_voice(path)
                    .await?,
            )
        }

        AudioKind::Music => {
            DetailedAnalysis::Lyrics(
                provider
                    .transcribe_lyrics(path)
                    .await?,
            )
        }

        AudioKind::Other => {
            DetailedAnalysis::None
        }
    };

    Ok(CompleteAnalysis {
        embedding,
        classification,
        detailed,
    })
}
```

実運用では、この一括関数よりも、分類後の処理を個別ジョブとして遅延実行する方がよい。

---

## 24. UI案

### AI Analysisパネル

```text
AI Analysis
────────────────────────────────

Type:
SE                         97%

UCS:
DOOR / METAL               89%

Description:
重い金属製ドアが室内で閉まる音。

Suggested descriptors:
Heavy / Close / Interior

Similar sounds:
1. Door_Metal_Close_03.wav   0.94
2. Hatch_Close_Heavy_01.wav  0.91
3. Machine_Lock_02.wav       0.87

[Find Similar]
[Accept UCS]
[Re-analyze]
```

---

### Voice表示

```text
Type:
Voice                      99%

00:00.000 - 00:01.140
敵が来るぞ！

00:01.320 - 00:02.510
全員、下がれ！

[Edit Transcript]
[Export SRT]
[Re-analyze]
```

---

### Music表示

```text
Type:
Music                      98%

Section:
Chorus

00:12.340 - 00:15.820
シュワッと弾ける
らむねブルースカイ

Confidence:
82%

[Use Vocal Stem]
[Export LRC]
[Edit Lyrics]
```

---

## 25. 承認フロー

AI結果を直接確定値として扱わない。

### 自動適用してよいもの

- Embedding
- 説明文
- 分類候補
- 文字起こし候補
- 類似検索インデックス

### 承認を要求するもの

- UCS確定タグ
- ファイル名変更
- ファイル移動
- 上書き保存
- 大量一括編集
- メタデータ書き込み
- 書き出し

---

## 26. エラー処理

最低限、以下を区別する。

```rust
#[derive(
    Debug,
    thiserror::Error,
)]
pub enum AiError {
    #[error("APIキーが設定されていません")]
    MissingApiKey,

    #[error("音声ファイルを読み込めません: {0}")]
    FileRead(String),

    #[error("ネットワークエラー: {0}")]
    Network(String),

    #[error("APIレート制限に達しました")]
    RateLimited,

    #[error("APIレスポンスが不正です: {0}")]
    InvalidResponse(String),

    #[error("構造化出力の検証に失敗しました: {0}")]
    SchemaValidation(String),

    #[error("処理がキャンセルされました")]
    Cancelled,
}
```

HTTP 429では指数バックオフを行う。

```text
1秒
2秒
4秒
8秒
最大待機時間へ到達したら失敗
```

無限リトライをしない。

---

## 27. セキュリティ上の注意

- APIキーをGitへコミットしない
- APIキーをログへ出さない
- ユーザーへアップロード対象を明示する
- 音声を自動で全件アップロードしない
- 機密音声を扱う場合はAPI利用を無効化できるようにする
- ファイル名やメタデータに個人情報が含まれる可能性を考慮する
- 解析開始前にクラウド送信であることをUIで示す
- APIレスポンスを信頼せず、JSON Schemaとアプリ側enumで検証する

---

## 28. コスト削減

### 必須対策

1. コンテンツハッシュによるキャッシュ
2. 解析結果の再利用
3. 分類後の遅延処理
4. 短いプレビュー音声だけを送るモード
5. 長時間ファイルの区間分割
6. 同時実行数制限
7. 一括処理前の件数・推定コスト表示

---

### 音声プレビュー生成

分類やUCSタグ付けでは、常に音声全体が必要とは限らない。

```text
短いSE:
全体を送信

長いAmbience:
冒頭・中央・末尾を抽出

長いMusic:
区間分割または必要部分のみ送信
```

類似検索用Embeddingと文字起こしでは必要条件が異なるため、同じ送信戦略を使わない。

---

## 29. 実装ロードマップ

## Phase 1: 基盤

- `AiProvider`トレイト
- `GeminiClient`
- APIキー管理
- バックグラウンドジョブ
- エラー処理
- キャッシュ
- JSON Schema構造化出力

---

## Phase 2: 分類

- SE / Voice / Music / Other分類
- 複合属性
- 信頼度表示
- 手動修正
- 一括分類

---

## Phase 3: 似た音検索

- 音声Embedding生成
- テキストEmbedding生成
- SQLite保存
- コサイン類似度検索
- 類似結果UI
- ハッシュキャッシュ

---

## Phase 4: Voice

- Voice文字起こし
- タイムスタンプ
- 話者分離
- 感情タグ
- SRT出力
- テキスト編集UI

---

## Phase 5: UCS

- UCSカテゴリデータ読み込み
- 大カテゴリ分類
- サブカテゴリ分類
- descriptors生成
- 承認UI
- 一括タグ付け

---

## Phase 6: Music

- 歌詞文字起こし
- 区間分割
- 重複区間統合
- ボーカルステム入力
- LRC出力
- 曖昧語編集UI

---

## Phase 7: 高速化

- HNSW
- バッチAPI
- 並列数制御
- キャンセル
- 進捗表示
- API使用量表示

---

## 30. 最初のMVP

最初の実装は以下に限定する。

1. `GeminiClient`
2. Base64インライン音声入力
3. SE / Voice / Music / Other分類
4. 音声Embedding生成
5. 768次元前後のEmbedding保存
6. コサイン類似検索
7. コンテンツハッシュキャッシュ
8. eguiバックグラウンドワーカー
9. 類似音検索UI
10. 分類結果の手動修正

次のPRで追加する。

1. Voice文字起こし
2. SRT出力
3. UCSタグ付け
4. Files API
5. Music歌詞文字起こし
6. ボーカルステム対応

---

## 31. 実装上の重要ポイント

### Gemini APIへ依存しすぎない

`AiProvider`を介して呼び出す。

### UIスレッドを止めない

Tokioワーカーとチャンネルを使用する。

### 音声パスではなくハッシュでキャッシュする

移動・改名後も解析結果を再利用する。

### UCSは候補制約を行う

自由生成させない。

### 歌詞は不確実性を保持する

聞き取れない箇所を無理に確定しない。

### 破壊的操作は承認制にする

ファイル名変更や上書きはユーザー確認を必須にする。

### Geminiは意味理解に使う

精密DSP、波形編集、ラウドネス測定、ループ処理などはRust側で行う。

---

## 32. 結論

RustでGemini音声機能を統合する場合は、次の構成が最も扱いやすい。

```text
reqwest
+ serde
+ schemars
+ tokio
+ SQLite
+ コンテンツハッシュ
+ Provider抽象化
+ バックグラウンドジョブ
```

役割分担は次のようにする。

```text
Gemini
  = 音声の意味理解
  = 分類
  = タグ候補
  = 文字起こし
  = Embedding

Rustアプリ
  = ファイル管理
  = DSP
  = 検索
  = キャッシュ
  = UI
  = 承認
  = 書き出し
```

この構成により、Gemini APIを利用しつつ、将来的なローカルモデル対応や別APIへの切り替えも可能になる。

---

## 33. 導入時の確認事項

Gemini APIはモデル名、利用可能な入力形式、エンドポイント、ファイル制限が更新される可能性がある。

実装開始時には、必ず公式ドキュメントで次を確認する。

- 音声入力に対応した現行モデル
- マルチモーダルEmbedding対応モデル
- 構造化出力の最新形式
- Files APIのアップロード方式
- 音声ファイルの容量制限
- 対応MIMEタイプ
- レート制限
- 料金
- データ保持ポリシー
