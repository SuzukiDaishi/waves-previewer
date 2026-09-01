# コメント機能 仕様（.nwsess 共有セッションの会話）

ファイルサーバ上の `.nwsess` を複数人で扱う運用は既に成立している
（`docs/NWPROJ_PLAN.md` の **Shared sessions**）。そこに欠けていたのは
**人と人がやり取りする場所**で、本機能がそれを埋める。

既存の注釈は 2 つとも単独作業前提だった：

| | 用途 | 保存先 | 著者 | 返信 |
|---|---|---|---|---|
| `MediaItem.note` | リストの 1 行メモ | `.nwsess` の list item | なし | なし |
| `EditorNote` | 時間・範囲・周波数への個人メモ | `.nwsess` の list item | なし | なし |
| **`ProjectComment`** | **チーム宛の会話** | **`.nwsess` トップレベル** | **あり** | **あり** |

三者は併存する。Editor Note は一切変更していない。

---

## 1. データモデル

`ProjectFile.comments: Vec<ProjectComment>`（`src/app/project.rs`）。
`#[serde(default)]` の追加のみなので **`version` は上げていない**。古いビルドでも
コメント付きドキュメントは開ける。

```toml
[[comments]]
id = "0123456789abcdef0123456789abcdef"   # 128bit 乱数。カウンタは使わない
parent = "fedcba98..."                    # 省略でスレッドの根
author_id = "daishi"                      # OS ユーザ名（trim + 小文字）。主キー
author_host = "WS-01"                     # マシン名。副キー
author_name = "鈴木 大志"                  # prefs の display_name。表示専用
created_at = "2026-09-01T12:34:56Z"        # RFC3339 UTC
edited_at  = "2026-09-01T13:00:00Z"
rev = 1                                    # 編集回数。マージの勝敗に使う
body = "リバーブが長い @[voice/line_001.wav|12.5-14.25]"
deleted = false                            # 墓標
resolved_by = "tanaka"                     # 根のみ
resolved_at = "2026-09-01T14:00:00Z"
```

### なぜフラットなのか

木にすると 2 人の追記が同じ配列位置を奪い合う。フラットな集合なら **id を鍵にした
集合和**でマージでき、可換かつ冪等になる。順序に関係なく、二重に適用しても同じ結果。

### なぜ id が乱数なのか

`EditorNote.id` は `max + 1` のカウンタで、共有セッションでは 2 人が同じ番号を
主張する。ステータス/タグの slug 化とサイドカーの content addressing が既に
避けている失敗と同じもの。

### 著者の同一性

`author_id`（OS ユーザ名）だけで「自分のか」を決める。`display_name` を変えても
過去の投稿が他人のものにならない。`author_host` は **同じ `author_id` の人が
複数いるときだけ** UI に出す（共有ドライブでは `user` が 2 台ある、が実在する形）。

### マージ規則（`src/app/comments.rs`）

id で集合和。同じ id が両側にあれば `(rev, edited_at)` の大きい方。同 rev なら
**墓標が勝つ**（「取り下げられた」の方が安全な答え）。最後の同点は body の辞書順で、
「決まること」自体が目的（決まらないと双方が上書きし合い続ける）。

---

## 2. 参照トークン

参照は **body 中のトークンが唯一の真実**。別配列に構造化して持つと本文と二重管理に
なり、片方を編集した瞬間にずれる。

```
@[voice/line_001.wav]                          ファイル
@[voice/line_001.wav|12.5]                     ファイル + 時刻
@[voice/line_001.wav|12.5-14.25]               ファイル + 時間範囲
@[voice/line_001.wav|12.5-14.25|220-880Hz]     + 周波数帯
```

- パスはセッションの `path_mode` に従う。共有先を `Z:\` で開く人と `\\server\` で
  開く人が同じトークンを解決できるのはこのため。既存の修復チェーンも通る。
- パス中の `\` `]` `|` は `\` でエスケープ。**解釈できないトークンはただの文字**。
- **時刻は秒**。`EditorNote` はサンプル番号だが、あれは編集中バッファの座標系で
  破壊編集や SR 変換で動く（`remap_editor_notes_for_replacement`）。コメントは
  「同僚が開く元ファイル」を指すので秒が安定で、人間にも読める。

---

## 3. 書き込み（`src/app/comment_ops.rs`）

コメントは共有ドキュメントに入るが、**通常のセッション保存とは別経路**で書く。
同じ経路にすると (1) 著者の未保存編集まで押し出す (2) 2 人が同時に打つたびに
競合モーダルが出る、の 2 つが同時に起きる。

```
run_comment_write_job(path, ops, saved_by)
  最大 5 回（0 / 50 / 150 / 400 ms のバックオフ）:
    1. ディスクのドキュメントを読む
    2. deserialize
    3. comments::merge_into(disk.comments, ops)      集合和
    4. revision / saved_at / saved_by を通常保存と同じ規則で更新
    5. 直前にもう一度読んで fingerprint 照合（CAS）
       ずれていたら 1 へ（＝相手のドキュメントに入れ直す）
    6. tmp → atomic_replace
```

- **CAS ミスは競合ではない。** 誰かが先にコミットしただけなので、読み直して
  マージし直す。マージが決定的なので収束し、ユーザーにプロンプトは出ない。
- 5 回使い切ったときだけ outbox に残り、UI に「まだ共有されていません」と出る。
  outbox の中身は `has_unsaved_work` に算入されるので、終了時に黙って消えない。
- **セッション未保存のときは書かない。** メモリに留まり、最初の Save で一緒に出る。
- 書き終えたら `session_disk_fingerprint` を更新して watch を張り直す。忘れると
  自分の投稿が `⟳ changed on disk` として跳ね返る。
- **通常のフルセーブもディスク側の `comments` を集合和で取り込む。** CAS のため
  既にバイト列を読んでいるので追加コストはゼロ。取り込まないと Overwrite を選んだ
  瞬間に同僚のコメントが消える。

---

## 4. 読み込みと「コメントだけの変更」判定

投稿はドキュメントを書き換えるので、素直にやると同僚が発言するたび
`⟳ changed on disk`（＝未保存編集を捨てるリロードを促す警告）が立つ。それを
避けるのが **comment-free fingerprint**（`project::comment_free_fingerprint`）。

> ドキュメントから `comments` **と保存スタンプ**（`revision` / `saved_at` /
> `saved_by`）を抜いてハッシュする。スタンプはコメント投稿を含む全ての書き込みで
> 動くので、残すと何も一致しなくなる。

これを 2 か所で使う：

1. **watch**（`session_watch.rs`）— 変化を検知しても警告は保留し、バックグラウンドの
   pull に判定させる。コメントだけならマージして黙り、fingerprint を更新して
   watch を張り直す。それ以外、および pull 失敗時は今まで通り警告する
   （読み損ねを理由に本物の保存を握り潰さないため、失敗は警告側に倒す）。
2. **保存の競合判定**（`session_ops::session_conflict_from`）— 同じ免除を与える。
   これが無いと、同僚が 1 行書いた瞬間に全員の Ctrl+S が競合プロンプト行きになる。
   パースできないドキュメントは免除せず競合扱い（保守的側に倒す）。

取り込みの契機：watch の検知 / ウィンドウを開いたとき / Refresh ボタン /
自分の投稿の直後。共有ドライブの watch 間隔は 20 秒（`perf_profile.rs`）。

---

## 5. 未読（per-user）

`AGENTS.md` の原則どおり、per-user の状態はドキュメントに入れず
`session_store`（ローカル SQLite）へ。

```sql
CREATE TABLE comment_read (
    session_key TEXT NOT NULL,
    comment_id  TEXT NOT NULL,
    read_at     INTEGER NOT NULL,
    PRIMARY KEY (session_key, comment_id)
);
```

- 自分の投稿は未読に数えない。
- トップバーの `💬 N new` は「まだ読んでいない数」。ウィンドウが開いていれば出ない。
- ウィンドウ内の青いドットは「このウィンドウを開いた時点で新しかったもの」。
  描画と同時に既読にすると同じフレームでドットが消えるので、別集合で持っている。
  ウィンドウを閉じるとリセット。
- キャッシュなので、消えても再読になるだけ。

---

## 6. UI（`src/app/ui/comments.rs`）

- ドッキング時は `egui::Window`。`⧉` で **別 OS ウィンドウ**へ切り出す
  （`show_viewport_immediate`。前例は `ui/video_viewport.rs`）。別モニタに置いて
  波形を操作しながら読むための機能なので、切り出しが本命。閉じるとドックに戻る。
- フィルタ: All / This file（選択に追従）/ Unresolved / Mine。
  **スレッド単位で判定**するので、返信がヒットすれば根も残る。
- 編集・削除は著者のみ。解決/再オープンは誰でも（スレッドはチームのもの）。
- 描画は会話を書き換えない。ボタンは `CommentAction` を記録し、木の走査が
  終わってから適用する。

### 参照の入れ方（4 通り）

| 方法 | 用途 |
|---|---|
| `🔗 Reference` メニュー | 今見ているファイル / 再生位置 / 選択範囲 |
| `@` タイプアヘッド | 他の全ファイル。単語の先頭の `@` でのみ開く |
| **Alt + リスト行ドラッグ** | 素のドラッグは OS へのファイルドラッグ（DAW への配置）で埋まっているため |
| ウィンドウへのファイルドロップ | Explorer から |

### Markdown（`src/app/ui/comment_markdown.rs`）

依存追加なしの最小サブセット。`**太字**` `*斜体*` `~~打消~~` `` `code` ``、
`#`〜`###`、`-`/`*`/`1.` のリスト、`>` 引用、``` フェンス、素の URL の自動リンク。

CommonMark と違うところは常に「単純な方」に倒してある。強調はネストするが
`***both***` の短縮形は特別扱いしない（run-length ルールが要る）。閉じない記号は
書かれた文字のまま残る — `*.wav` について書く人が求める挙動。

---

## 7. 既知の限界

- **コメント投稿は `.nwsess` 全体を書き直す。** 数キロバイトのドキュメントなら
  共有ドライブでも数十〜数百 ms で、ワーカーで走るので UI は止まらない。ただし
  非常に大きいセッションでは投稿のレイテンシがサイズに比例する。
- **`.nwsess` を保存する前のコメントは共有されない。** メモリに留まり、最初の
  Save で出る。UI に明示している。
- **波形/スペクトログラム上にコメントのピンは描かれない。** `EditorNote` にも無く、
  `render/` にオーバーレイ描画器が存在しないため。別途設計する。
- **`@ユーザー` メンションと通知は未実装。** ファイル参照の `@` とは別物。
- **CLI 面が無い。** `--cli session comments list` は読み取り専用なので安価な追加。
- 秒での参照は、同僚が元ファイルを差し替えて長さが変われば当然ずれる。それは
  `docs/NWPROJ_PLAN.md` の「Changed since you last opened it」が報告する範疇。

---

## 8. 関連ファイル

| ファイル | 役割 |
|---|---|
| `src/app/comments.rs` | 純ロジック（id / マージ / 木構築 / トークン） |
| `src/app/comment_ops.rs` | 投稿・編集・削除・解決、書き込みジョブ、取り込み、参照ジャンプ |
| `src/app/ui/comments.rs` | ウィンドウ、切り出し、スレッド描画、コンポーザー、`@` ピッカー |
| `src/app/ui/comment_markdown.rs` | 最小 Markdown のパーサ |
| `src/app/project.rs` | `ProjectComment` と `comment_free_fingerprint` |
| `src/app/session_ops.rs` | フルセーブでの集合和、コメントのみ変更の競合免除 |
| `src/app/session_watch.rs` | 変化検知と警告の保留 |
| `src/app/session_store.rs` | `comment_read`（per-user） |
| `tests/session_shared_comments.rs` | 2 人同時操作 |
| `tests/comments_ui.rs` | ウィンドウの操作 |
