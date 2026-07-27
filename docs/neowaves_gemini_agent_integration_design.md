# NeoWavesへのGemini API統合設計

## 1. 目的

Rust + eguiで構築された音声管理・編集ツール「NeoWaves」にGemini APIを統合し、以下の機能を実現する。

1. 似た音検索
2. 音声素材の `SE / Voice / Music / Other` 分類
3. SE素材へのUCSタグ付け
4. Voice素材の文字起こし
5. Music素材の歌詞文字起こし
6. 自然言語によるAgent的な複数ステップ操作

本設計では、Geminiを単なるチャット機能として追加するのではなく、音声理解・Embedding・構造化出力・Function CallingをNeoWavesの既存機能と接続する。

---

# 2. 結論

推奨構成は次の通り。

```text
Gemini API
  ├─ 音声の意味理解
  ├─ SE / Voice / Music / Other分類
  ├─ UCSタグ候補
  ├─ Voice文字起こし
  ├─ Music歌詞文字起こし
  ├─ 音声・テキストEmbedding
  └─ Agent Modeでのツール選択

Rust / NeoWaves
  ├─ ファイル管理
  ├─ DSP
  ├─ キャッシュ
  ├─ SQLite
  ├─ ベクトル検索
  ├─ ジョブ管理
  ├─ Tool Registry
  ├─ 権限管理
  ├─ 承認フロー
  └─ 書き出し・変更処理

ユーザー
  └─ 破壊的操作や確定タグの最終承認
```

基本思想は次の通り。

> Geminiは意味理解と処理計画を担当し、NeoWavesは実処理と安全性を担当する。

---

# 3. Gemini APIとAntigravityの比較

Antigravityは、Geminiと競合する音声認識APIというより、コード実行やWeb検索などを含む自律エージェント実行環境に近い。

今回の音声ツールへの組み込みでは、Gemini APIを中心にする方が適している。

| 項目 | Gemini API | Antigravity |
|---|---|---|
| 音声入力 | 適している | 音声処理の主用途ではない |
| 音声Embedding | 対応モデルを利用可能 | 主用途ではない |
| 構造化JSON | 利用可能 | 音声解析DBへの直接利用には不向き |
| Function Calling | 利用可能 | エージェント実行が中心 |
| Rustアプリ内部関数との接続 | 実装しやすい | 外部実行環境が中心 |
| ファイル変更の細かな制御 | Rust側で制御可能 | 実行権限が広くなりやすい |
| APIコスト管理 | 処理単位で管理しやすい | 自律ループで増えやすい |
| NeoWavesとの相性 | 高い | 将来の上位Agentとしては利用可能 |

したがって、最初はGemini APIだけで実装する。

将来、NeoWaves外部のWeb検索、コード生成、レポート作成まで含む広域Agentが必要になった場合のみ、Antigravityを上位層として検討する。

---

# 4. 通常AI機能とAgent Modeを分ける

Gemini APIの利用方法を2種類に分ける。

## 4.1 Direct Analysis

処理内容が決まっている場合に使用する。

```text
音声ファイル
  ↓
決められたAPI処理
  ↓
構造化結果
  ↓
保存
```

対象機能：

- 音声分類
- Embedding生成
- UCSタグ付け
- Voice文字起こし
- Music歌詞文字起こし

利点：

- 速い
- 安い
- 再現性が高い
- デバッグしやすい
- 一括処理しやすい

---

## 4.2 Agent Mode

曖昧で複数ステップの依頼に使用する。

例：

```text
このフォルダの未整理素材を調べて、
SEにはUCSタグを付け、
Voiceは文字起こしして、
似た音をまとめてください。
```

Agent Modeでは、GeminiがNeoWavesのツールを順番に選択する。

```text
Gemini
  ↓ Function Calling
NeoWaves Tool Registry
  ↓
Rust関数を実行
  ↓
結果をGeminiへ返す
  ↓
次の処理を決定
```

Agent Modeは必要な時だけ起動する。

---

# 5. 全体アーキテクチャ

```text
egui UI
  │
  ├─ Direct Analysis
  │    ├─ classify_audio
  │    ├─ embed_audio
  │    ├─ assign_ucs_tags
  │    ├─ transcribe_voice
  │    └─ transcribe_lyrics
  │
  └─ Agent Mode
       ├─ ユーザー指示
       ├─ Agent Loop
       ├─ Tool Registry
       ├─ Approval Gate
       └─ Session State

             │
             ▼

GeminiProvider
  ├─ REST API
  ├─ Function Calling
  ├─ Structured Output
  └─ Files API

             │
             ▼

NeoWaves Core
  ├─ Audio Database
  ├─ DSP
  ├─ Metadata
  ├─ Similarity Search
  ├─ Export
  └─ File Operations
```

---

# 6. 推奨ディレクトリ構成

```text
src/
├── ai/
│   ├── mod.rs
│   ├── provider.rs
│   ├── models.rs
│   ├── cache.rs
│   ├── jobs.rs
│   ├── prompts.rs
│   │
│   ├── gemini/
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   ├── audio.rs
│   │   ├── embedding.rs
│   │   ├── files.rs
│   │   ├── interactions.rs
│   │   └── response.rs
│   │
│   └── agent/
│       ├── mod.rs
│       ├── loop.rs
│       ├── registry.rs
│       ├── permissions.rs
│       ├── approval.rs
│       ├── trace.rs
│       └── tools/
│           ├── context.rs
│           ├── inspect.rs
│           ├── classify.rs
│           ├── similarity.rs
│           ├── ucs.rs
│           ├── transcript.rs
│           └── export.rs
│
├── audio/
│   ├── analysis.rs
│   ├── separation.rs
│   └── waveform.rs
│
├── database/
│   ├── embeddings.rs
│   ├── ai_analysis.rs
│   └── migrations.rs
│
└── ui/
    ├── ai_panel.rs
    ├── agent_panel.rs
    └── approval_dialog.rs
```

---

# 7. Cargo依存関係

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
thiserror = "2"
tracing = "0.1"

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
```

追加候補：

```toml
keyring = "3"
secrecy = "0.10"
zeroize = "1"

rusqlite = {
    version = "0.32",
    features = ["bundled"]
}

hnsw_rs = "0.3"
```

---

# 8. APIキー管理

開発中は環境変数を使用する。

```powershell
$env:GEMINI_API_KEY="YOUR_API_KEY"
```

Rust側：

```rust
let api_key = std::env::var("GEMINI_API_KEY")
    .map_err(|_| anyhow::anyhow!(
        "GEMINI_API_KEYが設定されていません"
    ))?;
```

リリース版では`keyring`を使用し、Windows Credential ManagerやmacOS Keychainへ保存する。

禁止事項：

- Gitへのコミット
- ログ出力
- クラッシュレポートへの混入
- 平文設定ファイルへの保存

---

# 9. Provider抽象化

Gemini固有処理をNeoWaves全体へ露出させない。

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

将来差し替え可能なバックエンド：

- Gemini API
- ローカルWhisper
- CLAP系Embedding
- 独自ONNXモデル
- 別クラウドAPI
- テスト用Mock

---

# 10. 共通データ型

## 10.1 音声種別

```rust
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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

## 10.2 音声分類

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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

歌入り楽曲の例：

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

## 10.3 Embedding

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct AudioEmbedding {
    pub model: String,
    pub dimensions: usize,
    pub values: Vec<f32>,
}
```

---

## 10.4 UCSタグ

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct UcsCandidate {
    pub category_id: String,
    pub subcategory_id: String,
    pub confidence: f32,
}

#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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

## 10.5 Voice文字起こし

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct VoiceTranscript {
    pub language: String,
    pub full_text: String,
    pub segments: Vec<TranscriptSegment>,
}
```

---

## 10.6 Music歌詞文字起こし

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct LyricsTranscript {
    pub languages: Vec<String>,
    pub full_text: String,
    pub segments: Vec<LyricsSegment>,
}
```

---

# 11. Geminiクライアント

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

            // 実装時に公式ドキュメントで現行モデル名を確認する。
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
            .context("レスポンスを読み取れませんでした")?;

        if !status.is_success() {
            let message = String::from_utf8_lossy(&bytes);

            anyhow::bail!(
                "Gemini API error {status}: {message}"
            );
        }

        serde_json::from_slice(&bytes)
            .context("APIレスポンスの解析に失敗しました")
    }
}
```

API仕様やモデル名は更新される可能性があるため、Gemini固有処理は一箇所へ集約する。

---

# 12. 音声の送信方式

## 12.1 インライン送信

短いSEやVoice向け。

```text
音声
  ↓
Base64
  ↓
JSONリクエスト
```

利点：

- 実装が単純
- 1リクエストで完結
- 短い素材に適する

欠点：

- Base64でサイズが増える
- 長いMusicには不向き
- 複数処理で同じ音声を再送する

---

## 12.2 Files API

長いMusicや、同じ素材を複数回解析する場合に使用する。

```text
Upload
  ↓
Remote File URI
  ↓
分類
  ↓
文字起こし
  ↓
歌詞解析
```

一時キャッシュ：

```rust
pub struct UploadedFileCache {
    pub content_hash: String,
    pub remote_uri: String,
    pub uploaded_at_unix_ms: u64,
}
```

---

# 13. SE / Voice / Music / Other分類

## 13.1 分類ルール

### SE

- Foley
- UI音
- 環境音
- 足音
- 衝突音
- 機械音
- ドア音
- 武器音

### Voice

- セリフ
- ナレーション
- 掛け声
- 呼吸
- 群衆会話
- システムボイス

### Music

- 楽曲
- ジングル
- スティンガー
- リズムループ
- 楽器フレーズ
- 歌入り楽曲

### Other

- 無音
- テスト信号
- 破損
- 分類不能
- 目的不明のノイズ

---

## 13.2 プロンプト例

```text
この音声素材をゲームサウンドライブラリ向けに分類してください。

primary_classは必ず以下のいずれかです。

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

構造化出力を使用し、モデルの自由文から手作業でJSONを抽出しない。

---

# 14. 似た音検索

## 14.1 処理フロー

```text
音声ファイル
  ↓
音声Embedding
  ↓
L2正規化
  ↓
SQLiteへ保存
  ↓
コサイン類似度検索
```

テキスト検索：

```text
「重い金属のドアが閉まる音」
  ↓
テキストEmbedding
  ↓
音声Embeddingと比較
```

---

## 14.2 L2正規化

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

## 14.3 コサイン類似度

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

## 14.4 検索方式

初期：

```text
SQLite
+ Vec<f32>
+ 総当たり
```

将来：

- HNSW
- Qdrant
- LanceDB
- pgvector
- 独自SIMD検索

最初から外部ベクトルDBを必須にしない。

---

# 15. SEのUCSタグ付け

UCSタグをGeminiに自由生成させない。

正規のUCS候補をRust側で保持し、その中から選択させる。

```rust
#[derive(
    Debug,
    Clone,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct UcsEntry {
    pub category_id: String,
    pub subcategory_id: String,
    pub description: String,
}
```

## 15.1 一段階分類

候補が少ない場合：

```text
候補に存在するcategory_idとsubcategory_idだけを使用する。
候補にないIDを生成してはいけない。
```

## 15.2 二段階分類

候補が多い場合：

```text
大カテゴリ分類
  ↓
該当カテゴリ内のサブカテゴリだけを抽出
  ↓
サブカテゴリ分類
```

利点：

- プロンプトを短縮
- 幻覚を抑制
- 検証しやすい
- UCS更新に対応しやすい

---

## 15.3 AI候補と確定値を分ける

```rust
pub struct UcsMetadata {
    pub ai_suggestion: Option<UcsAnalysis>,

    pub confirmed_category_id: Option<String>,
    pub confirmed_subcategory_id: Option<String>,

    pub manually_edited: bool,
}
```

人間が確定したタグを再解析で上書きしない。

---

# 16. Voice文字起こし

取得項目：

- 言語
- 全文
- セグメント
- 開始時刻
- 終了時刻
- 話者
- 感情
- 信頼度

プロンプト例：

```text
音声をできるだけ忠実に文字起こししてください。

- 言語を判定する
- セグメントごとに開始・終了時刻を返す
- 複数話者の場合はspeakerを設定する
- 笑い、息、叫びなどを必要に応じて記述する
- 聞き取れない語句を推測で確定しない
- confidenceは0.0から1.0
```

ゲーム向け拡張例：

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

追加可能な分類：

- Battle Voice
- Damage Voice
- Death Voice
- Narration
- Dialogue
- System Voice
- Crowd
- Radio Voice

---

# 17. Music歌詞文字起こし

Voice文字起こしより難しい。

主な要因：

- 楽器によるマスキング
- リバーブ
- ディレイ
- ハモリ
- コーラス
- 叫び
- ボーカルチョップ
- 崩した発音
- 多言語混在
- 長い母音

---

## 17.1 推奨パイプライン

```text
楽曲
  ↓
Music判定
  ↓
区間分割
  ↓
歌詞文字起こし
  ↓
重複区間の統合
  ↓
全体文脈で補正
  ↓
LRC / SRT
```

区間例：

```text
Chunk 1: 00:00 - 00:45
Chunk 2: 00:38 - 01:23
Chunk 3: 01:16 - 02:01
```

---

## 17.2 ボーカルステム

精度を上げるには、ローカルでボーカル分離してから送信する。

```text
Original Mix
  ├─ 曲構成・文脈
  │
  └─ Source Separation
       └─ Vocal Stem
            └─ 単語認識
```

入力モード：

```rust
pub enum LyricsInputMode {
    OriginalMix,
    VocalStem,
    OriginalAndVocalStem,
}
```

---

## 17.3 不確実性の保持

```json
{
  "text": "きみのこえを探して",
  "confidence": 0.52,
  "uncertain_words": [
    "こえ"
  ]
}
```

曖昧な語を無理に確定しない。

---

# 18. Agent Mode

Gemini API単体でも、Function CallingとRust側Agent Loopを組み合わせればAgent的な挙動を実現できる。

Gemini自身がRust関数を実行するわけではない。

```text
Gemini
  ↓
関数名と引数を返す
  ↓
Rustが検証
  ↓
Rustが実行
  ↓
結果をGeminiへ返す
  ↓
Geminiが次の処理を選ぶ
```

---

# 19. NeoWavesに公開するツール

## 19.1 読み取りツール

```text
get_current_context
list_audio_files
get_selected_files
inspect_audio_file
inspect_selection
query_metadata
get_analysis_status
search_similar_audio
```

## 19.2 AI解析ツール

```text
classify_audio
generate_embedding
assign_ucs_tags
transcribe_voice
transcribe_lyrics
```

## 19.3 提案ツール

```text
propose_metadata_updates
propose_file_names
propose_file_moves
create_edit_plan
create_export_plan
```

## 19.4 確定ツール

```text
apply_metadata_plan
rename_audio_files
move_audio_files
export_transcript
export_audio
delete_audio_files
```

確定ツールは原則として承認必須にする。

---

# 20. Tool Registry

```rust
#[async_trait::async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn parameter_schema(
        &self,
    ) -> serde_json::Value;

    fn permission(&self) -> ToolPermission;

    async fn execute(
        &self,
        arguments: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;
}
```

```rust
pub struct ToolRegistry {
    tools: std::collections::HashMap<
        String,
        std::sync::Arc<dyn AgentTool>,
    >,
}
```

実行：

```rust
impl ToolRegistry {
    pub async fn execute(
        &self,
        call: ValidatedToolCall,
    ) -> anyhow::Result<serde_json::Value> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "未登録のツールです: {}",
                    call.name
                )
            })?;

        tool.execute(call.arguments).await
    }
}
```

---

# 21. Agent Loop

```rust
pub async fn run_agent(
    client: &GeminiClient,
    registry: &ToolRegistry,
    user_request: &str,
) -> anyhow::Result<AgentResult> {
    const MAX_STEPS: usize = 12;

    let mut previous_interaction_id: Option<String> = None;
    let mut next_input = serde_json::json!(user_request);

    for _ in 0..MAX_STEPS {
        let interaction = client
            .create_interaction(
                next_input,
                registry.declarations(),
                previous_interaction_id.as_deref(),
            )
            .await?;

        previous_interaction_id =
            Some(interaction.id.clone());

        let calls = interaction.function_calls();

        if calls.is_empty() {
            return Ok(
                AgentResult::Completed {
                    message: interaction.output_text(),
                }
            );
        }

        let mut results = Vec::new();

        for call in calls {
            let validated =
                registry.validate_call(&call)?;

            let result =
                registry.execute(validated).await?;

            results.push(
                serde_json::json!({
                    "type": "function_result",
                    "name": call.name,
                    "call_id": call.id,
                    "result": [{
                        "type": "text",
                        "text": serde_json::to_string(
                            &result
                        )?
                    }]
                })
            );
        }

        next_input =
            serde_json::Value::Array(results);
    }

    anyhow::bail!(
        "Agentが最大ステップ数を超えました"
    )
}
```

必ず最大ステップ数を設定する。

---

# 22. Agentの権限設計

```rust
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
)]
pub enum ToolPermission {
    Read,
    Propose,
    Commit,
}
```

## Read

自動実行可能。

- ファイル一覧
- メタデータ参照
- 波形情報
- 類似検索
- 解析状況確認

## Propose

変更計画だけ生成。

- UCS候補
- 名前変更案
- 移動案
- タグ追加案
- エクスポート案

## Commit

ユーザー承認必須。

- タグ確定
- ファイル名変更
- ファイル移動
- 上書き
- 削除
- 書き出し

---

# 23. PlanとCommitの分離

ユーザー指示：

```text
このフォルダを全部整理して
```

即座に変更しない。

```text
調査
  ↓
分類
  ↓
処理計画
  ↓
ユーザーへ提示
  ↓
承認
  ↓
適用
```

UI例：

```text
AI整理プラン

対象: 184ファイル

SE:     123
Voice:   31
Music:   24
Other:    6

予定される変更:
- UCSタグ追加: 118件
- 文字起こし: 31件
- 名前変更: 42件
- 確認が必要: 7件

[詳細]
[適用]
[キャンセル]
```

---

# 24. Agent用ツールの粒度

悪い例：

```text
read_file_byte
write_json_property
execute_shell
run_arbitrary_command
```

良い例：

```text
inspect_audio
search_similar_audio
propose_ucs_tags
create_metadata_plan
apply_metadata_plan
export_transcript
```

Geminiには「何をするか」を選ばせる。

Rustには「どう安全に処理するか」を担当させる。

---

# 25. Agent状態管理

## 25.1 Gemini側状態

前回のInteraction IDを引き継ぐ。

```text
previous_interaction_id
```

利点：

- 実装が簡単
- ツール履歴を引き継げる
- マルチターンに向く

## 25.2 Rust側状態

実行履歴をローカルに保存する。

```rust
pub struct AgentTrace {
    pub session_id: String,
    pub steps: Vec<AgentTraceStep>,
}
```

```rust
pub struct AgentTraceStep {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result: serde_json::Value,

    pub approved: bool,
    pub timestamp_ms: u64,
}
```

監査、デバッグ、再現性のため、ツール実行履歴はRust側にも残す。

---

# 26. Function CallingとStructured Output

## Function Calling

途中でNeoWavesの関数を呼び出す場合。

```text
list_audio_files
inspect_audio
classify_audio
search_similar_audio
create_edit_plan
```

## Structured Output

最終結果をUI用スキーマへ整形する場合。

```json
{
  "summary": "...",
  "completed_actions": [],
  "pending_approvals": [],
  "warnings": []
}
```

使い分け：

```text
途中の処理
  = Function Calling

最後の表示
  = Structured Output
```

---

# 27. AIジョブキュー

```rust
pub enum AiJobKind {
    GenerateEmbedding,
    ClassifyAudio,
    AssignUcsTags,
    TranscribeVoice,
    TranscribeLyrics,
    RunAgent,
}
```

```rust
pub enum AiJobStatus {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}
```

```rust
pub struct AiJob {
    pub id: u64,
    pub file_id: Option<String>,
    pub path: Option<std::path::PathBuf>,

    pub content_hash: Option<String>,
    pub kind: AiJobKind,
    pub status: AiJobStatus,

    pub retry_count: u32,
}
```

---

# 28. egui連携

`eframe::App::update`内で直接`.await`しない。

```rust
while let Ok(event) =
    self.ai_worker.event_rx.try_recv()
{
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

        AiEvent::WaitingApproval {
            action,
            ..
        } => {
            self.pending_approvals.push(action);
            ctx.request_repaint();
        }

        _ => {}
    }
}
```

---

# 29. キャッシュ

ファイルパスではなくコンテンツハッシュをキーにする。

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

```rust
pub struct AiCacheKey {
    pub content_hash: String,
    pub model: String,
    pub operation: String,
    pub schema_version: u32,
    pub prompt_version: u32,
}
```

---

# 30. SQLite設計

## 30.1 Embedding

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

## 30.2 AI解析結果

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

## 30.3 Agent履歴

```sql
CREATE TABLE agent_trace (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    session_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,

    tool_name TEXT NOT NULL,
    arguments_json TEXT NOT NULL,
    result_json TEXT,

    approved INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
```

---

# 31. 推奨処理フロー

## インポート直後

```text
1. コンテンツハッシュ
2. キャッシュ確認
3. Embedding生成
4. SE / Voice / Music / Other分類
5. 結果保存
```

## 遅延処理

```text
SE
  ↓
UCSタグ候補

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

## Agent Mode

```text
自然言語指示
  ↓
コンテキスト取得
  ↓
必要ツール選択
  ↓
Readツール実行
  ↓
解析ツール実行
  ↓
変更計画生成
  ↓
承認
  ↓
Commit
```

---

# 32. UI案

## AI Analysis

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

## Agent Panel

```text
AI Agent
────────────────────────────────

User:
選択中の素材を整理して、
Voiceは文字起こししてください。

Agent:
24件を確認しました。

- SE: 14
- Voice: 7
- Music: 2
- Other: 1

提案:
- UCSタグ追加: 14件
- Voice文字起こし: 7件
- 確認が必要: 2件

[詳細]
[適用]
[キャンセル]
```

---

# 33. エラー処理

```rust
#[derive(
    Debug,
    thiserror::Error,
)]
pub enum AiError {
    #[error("APIキーが設定されていません")]
    MissingApiKey,

    #[error("ファイルを読み込めません: {0}")]
    FileRead(String),

    #[error("ネットワークエラー: {0}")]
    Network(String),

    #[error("APIレート制限に達しました")]
    RateLimited,

    #[error("APIレスポンスが不正です: {0}")]
    InvalidResponse(String),

    #[error("構造化出力の検証に失敗しました: {0}")]
    SchemaValidation(String),

    #[error("ツールの実行権限がありません: {0}")]
    PermissionDenied(String),

    #[error("ユーザー承認が必要です")]
    ApprovalRequired,

    #[error("Agentが最大ステップ数を超えました")]
    MaxStepsExceeded,

    #[error("処理がキャンセルされました")]
    Cancelled,
}
```

HTTP 429：

```text
1秒
2秒
4秒
8秒
失敗
```

無限リトライを行わない。

---

# 34. セキュリティ

- APIキーをログへ出さない
- アップロード対象をUIで明示する
- 音声を自動で全件アップロードしない
- クラウド解析を無効化できるようにする
- 機密音声向けローカルモードを残す
- APIレスポンスを必ず検証する
- 任意Shell実行ツールを公開しない
- ファイル削除は承認必須
- ファイル名変更・移動も承認必須
- Agentに絶対パスを不用意に渡さない
- Tool引数をJSON Schemaで制限する
- 最大ステップ数、最大処理件数を設定する
- Agent Traceを保存する

---

# 35. コスト削減

1. コンテンツハッシュキャッシュ
2. 分類後の遅延処理
3. 短いプレビュー送信
4. 長時間素材の区間分割
5. 同時実行数制限
6. 一括処理前の件数表示
7. Agent最大ステップ数
8. ツール結果の要約
9. ファイル一覧を全件プロンプトに入れない
10. 詳細は必要になった時にツールで取得する

---

# 36. 実装ロードマップ

## Phase 1: 基盤

- `AiProvider`
- `GeminiClient`
- APIキー管理
- Tokioワーカー
- エラー処理
- キャッシュ
- JSON Schema構造化出力

## Phase 2: 分類

- 4分類
- 複合属性
- 信頼度
- 一括分類
- 手動修正

## Phase 3: 似た音検索

- 音声Embedding
- テキストEmbedding
- SQLite保存
- コサイン類似度
- 検索UI

## Phase 4: Voice

- 文字起こし
- タイムスタンプ
- 話者
- 感情
- SRT

## Phase 5: UCS

- UCSデータ読み込み
- 大カテゴリ分類
- サブカテゴリ分類
- 承認UI
- 一括タグ付け

## Phase 6: Music

- 歌詞文字起こし
- 区間分割
- 重複統合
- ボーカルステム
- LRC

## Phase 7: Agent Mode

- Tool Registry
- Function Calling
- Agent Loop
- Read / Propose / Commit
- Approval Gate
- Agent Trace
- Agent UI

## Phase 8: 高速化

- HNSW
- バッチ処理
- キャンセル
- 進捗表示
- API使用量表示
- 並列数制御

---

# 37. 最初のMVP

最初のPR：

1. `GeminiClient`
2. APIキー管理
3. Base64音声入力
4. 4分類
5. 音声Embedding
6. テキストEmbedding
7. SQLite保存
8. 類似検索
9. コンテンツハッシュ
10. eguiバックグラウンドワーカー

次のPR：

1. Voice文字起こし
2. SRT
3. UCSタグ付け
4. Files API
5. 歌詞文字起こし
6. ボーカルステム

Agent PR：

1. `AgentTool`
2. `ToolRegistry`
3. `run_agent`
4. `ToolPermission`
5. Approval UI
6. Agent Trace
7. 最大ステップ数
8. Agent Modeのオン・オフ

---

# 38. 最終推奨構成

```text
NeoWaves
│
├─ Direct Analysis
│   ├─ Gemini音声分類
│   ├─ Gemini Embedding
│   ├─ Gemini UCS候補
│   ├─ Gemini Voice文字起こし
│   └─ Gemini歌詞文字起こし
│
├─ Agent Mode
│   ├─ Gemini Function Calling
│   ├─ Agent Loop
│   ├─ Tool Registry
│   ├─ Approval Gate
│   └─ Trace
│
├─ Rust Core
│   ├─ DSP
│   ├─ ファイル操作
│   ├─ SQLite
│   ├─ 検索
│   └─ Export
│
└─ Optional Local Models
    ├─ Whisper
    ├─ Source Separation
    ├─ Local Embedding
    └─ Music Analysis
```

---

# 39. 重要原則

## GeminiはDSPではない

波形編集、ラウドネス、ループ、位相、チャンネル処理はRust側で行う。

## Agent Modeは常時使わない

決まった処理はDirect Analysisを使う。

## Agentには高レベルツールだけを渡す

任意Shell、任意ファイル書き込みを与えない。

## 変更処理は承認制にする

Read、Propose、Commitを分離する。

## キャッシュはハッシュ基準

パス変更後も再利用できる。

## 構造化出力を使用する

自由文JSON抽出を避ける。

## Gemini依存を隔離する

`AiProvider`と`GeminiClient`へ閉じ込める。

---

# 40. 結論

NeoWavesにAgent的な挙動を追加するために、Antigravityは必須ではない。

Gemini APIの以下の機能を組み合わせればよい。

```text
音声入力
+ Embedding
+ Structured Output
+ Function Calling
+ マルチターン状態
```

Rust側では次を実装する。

```text
Tool Registry
+ Agent Loop
+ Approval Gate
+ Job Queue
+ SQLite
+ Cache
+ Trace
```

最終的な設計方針は次の通り。

> Geminiは考える。  
> NeoWavesは実行する。  
> Rustは安全性を保証する。  
> ユーザーは破壊的変更を承認する。
