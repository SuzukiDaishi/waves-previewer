# Changelog

All notable changes in this repository (hand-written).

## Unreleased

### レビュー指摘に対応した

- **読めないだけのファイルを「削除された」と誤報し、ベースラインの行まで消していた。** `stat_of` が `.ok()?` であらゆるエラーを `None` に潰していたため、権限エラーや一時的な共有障害が「ファイルが消えた」と同じ扱いになっていた。実害は二重で、(1) 実際には存在するファイルを Removed と報告し、(2) ベースラインの行が削除されるので**次に開いたときは Added として二度目の誤報**、ハッシュも失われる。
  - probe を 3 状態（`Present` / `Missing` / `Unreadable`）に分け、`NotFound` だけがベースラインからの削除につながるようにした。読めなかった場合は**行に触れない**。
  - 報告の種類も `Unreadable` を新設した。`Changed`（= 中身が違う）に混ぜると、確かめていないことを断言することになる。「読めなかった」は事実として正確で、権限や共有の疎通という行動可能な情報でもある。
  - **これは直前のコミットでハッシュ失敗について直したのとまったく同じ種類の不具合を、stat 失敗側に残していたもの**だった。さらにスキャン用リトライを 1 回 50ms に短縮したことで、一時障害がこの経路に落ちる確率を自分で上げてしまっていた。
  - 指摘を追ううちに**3 箇所目**（`note_session_file_changed`、開いている間に watch が拾った変更を記録する経路）にも同じ不具合があり、加えてハッシュ失敗時に良いハッシュを潰す問題も残っていたことが分かった。こちらも同じ規則に揃えた。
- doc コメントが 2 行重複していたのを削除した（ダイアログ仮想化時の編集ミス）。

### 監査で見つけた不具合を修正した

上の 2 つの変更を自分で読み直して 8 件見つけ、直した。

- **【重大】スキャンのたびに、変更されていないファイルのハッシュを消していた。** ベースラインの行を常に上書きしていたが、段 2 が走らなかったとき（= ファイルが変わっていないとき）ハッシュは `None` になる。つまり**無変更のファイルは開くたびにハッシュを潰されていた**。結果として「1 回目に開く → 2 回目に無変更で開く（ハッシュ消失）→ 同じ内容で書き直す → 3 回目に開く」で **Changed と誤報**する。2 段構えの存在理由そのものが、開封 1 回分しか持たなかった。既存の統合テストが通っていたのは、書き換えをハッシュ取得の**次の**開封で行っていたため。無変更の開封を 1 回挟むテストを追加し、修正前に落ちることを確認してから直した。
  - **ベースラインの行は「新しい内容が実際に分かったときだけ」進める**規則に変更（`next_baseline_row`、純関数として単体テスト）。stat が一致するなら前回のハッシュと検知時刻をそのまま引き継ぐ。
- **一時的な読み取り失敗がベースラインを恒久的に劣化させていた。** stat は取れたがハッシュに失敗すると、既知の良いハッシュが `None` で上書きされ、しかも**以降 stat が一致するので二度とハッシュされない**。読めなかった場合は行を更新せず、次のスキャンに再評価させる。
- **競合ダイアログで Save As を選んでファイル選択をキャンセルすると、プロンプトだけが消えていた。** 競合状態を `match` の前にクリアしていたため、何も書かれていないのに質問だけが画面から消えていた。キャンセルは回答ではないので、プロンプトを残す。
- **変更一覧ダイアログが全行を毎フレーム構築していた。** `egui::Grid` は仮想化しないので、変更が数千件あるとダイアログを開いた瞬間に固まる。`ScrollArea::show_rows` で可視行だけ描くようにし、毎フレームの clone も除いた。
- **ストアが使えないときでも、10 万件のパスを UI スレッドで clone してソートしていた。** 誰も読まない答えのための純粋な無駄なので、早期に抜ける。
- **一括 stat が `PermissionDenied` を 100/300/900ms かけて再試行していた。** 保存パスでは正しい（1 回の共有違反で保存を失わないため）が、参照ファイル全件を stat する場面では恒久的な権限エラー 1 件ごとに 1.3 秒かかる。スキャン専用の短い遅延に分けた。
- **Save As で潰したドキュメントが、履歴 UI の探さないキーに入っていた。** Save As は計画作成時に `session_id` を意図的にフォークするので、その後の履歴取り込みが古い（= 空の）キーを使っていた。「いま Save As で上書きしたものを戻したい」がまさに履歴を開く動機なので、取り込みを `adopt_saved_session` の後に移した。

なお `NotFound` が再試行されない点は確認済みで、単に存在しないファイルの stat は即座に返る（当初これも遅いと疑ったが誤りだった）。

### 前回自分が開いてから、参照ファイルが変わったかを知らせるようにした

- **他人が wav を差し替えても、セッションファイルは 1 バイトも変わらない**。前回入れた競合検知は `.nwsess` そのものしか見ていないので、共有上で最も普通に起きる事故 — 参照先の音声が別物になっている — に一切気付けなかった。**自分が前回このセッションを開いた時点**の参照ファイルの状態を覚えておき、次に開いたときに差分を報告するようにした。
  - **判定は 2 段構え**。まず全件を `stat` して `(サイズ, 更新時刻)` を比べ、**食い違ったファイルだけ**内容ハッシュ（`hash_file_content`、全体 SHA-256）を取る。このアプリは 10 万ファイル規模のリストを共有上で扱う前提なので、全件ハッシュは開くたびに回線を数十分占有して成立しない。2 段構えならコストは**実際に変わった件数**に比例する。
  - **2 段目があることの意味**は「コピーし直しただけ」を弾けること。バックアップから戻した、同じ内容で再エクスポートした — 更新時刻は動くが 1 サンプルも変わっていないケースを Changed と誤報しない。
  - **初回は何も報告しない**。比較対象が無いセッションで全件を Added として出したら通知として役に立たない。黙ってベースラインを作る。
  - 削除・新規も報告する。音声ファイルに加えて、リストに結合している CSV/Excel も対象。
  - **「あらかじめ」ハッシュを持つための埋め戻し**を入れた。ハッシュ未計算の行を最低優先度で背景ハッシュする。結果は永続化されるので**起動をまたいで収束**し、放っておけば全ファイルが厳密比較できる状態に近づく。
- **開いている間に起きた変更は、次回に再通知しない**。folder watch が「リスト内のファイルの中身が変わった」と分かる唯一の場所なので、そこからベースラインを検知時刻つきで更新する。自分のエクスポートで書き換えたファイルも同様に更新するので、自分が見ていた変更を後から知らされることはない。
- **記録は共有ファイルではなく個人ローカルの SQLite** (`%LOCALAPPDATA%\NeoWaves\cache\session-state-v1.sqlite3`、`NEOWAVES_SESSION_STATE` で上書き可)。理由は 2 つあって、後者が本質的: (1)「**そのユーザが**前回開いた時点」は人ごとに違うので共有ドキュメントには置き場所が無い。(2) セッション内に持たせると**開くたびに全員がセッションを書き換える**ことになり、前回の変更で潰したばかりの「読むだけの人が書き手になる」問題がそのまま戻る。10 万ファイル分のハッシュは数 MB になり、毎回パースされるドキュメントに載せられる量でもない。キャッシュなので消えても壊れない（1 回だけ黙って再ベースライン化される）。
  - セッションの識別には前回追加した `session_id` を使う。同じ共有を `Z:\proj\a.nwsess` と `\\server\share\proj\a.nwsess` のどちらから開いても同じセッションとして扱える。
- **通知はトーストだけにしなかった**。トーストは 6 秒で消えるのに対し、スキャンが終わるのはユーザーが席を外した後かもしれない。トップバーに `⚠ N source files changed` を**行動するまで残す**。クリックで一覧（ファイル / 種別 / サイズ / 検知時刻）、行クリックでリスト側を選択、Dismiss で消える。`File > Changed Since Last Open...` からも開ける。**再読込は手動**（自動再読込は未保存の編集を捨てるため）。
- **セッションファイル自体のローカル履歴**を入れた。保存が既存のドキュメントを置き換えるたび、置き換えられた版を残す。そのバイト列は競合検知ですでに読んでいるので、追加コストは**書き込み 1 回だけ**。`File > Session History...` に revision / 保存者 / 保存時刻 / サイズが並ぶ。
  - **Restore** はその版をセッションに書き戻す。**戻す直前の版も履歴に入る**ので、戻す操作自体をやり直せる。**Save As...** は別ファイルに書き出すだけで今のセッションに触らない。
  - 1 セッション 20 世代 + 全体のバイト上限。個人ローカルなので他人の保存は自分の履歴には入らない。共有側の保険は従来どおり `<name>.nwsess.bak` の 1 世代で、役割が違うのでそのまま残した。
- **既知の限界**を `docs/NWPROJ_PLAN.md` に明記した: サイズも更新時刻も変えない改変は段 1 をすり抜けるので検知できない（全件ハッシュを選ばなかったことの直接の代償）。ハッシュ未計算のファイルの初回変更は、内容が同じでも保守的に Changed と報告する（黙るより誤報する方を選んだ。一度報告されれば以降は厳密になる）。

### セッションをファイルサーバーに置いて複数人で編集できるようにした

- **後から保存した人が、先に保存した人の作業を無言で消していた**。`.nwsess` には mtime もハッシュも世代番号も無く、保存は無条件の上書きだったので、共有フォルダに置いた瞬間から「Ctrl+S を押した順で勝つ」だけの状態だった。警告も痕跡も残らない。**保存時の競合検知**を入れて塞いだ。読み込んだ時点のファイル内容と、いま disk にある内容が一致しないと保存を中止し、`Save As... / Overwrite / Reload / Cancel` を選ばせる。**中止した時点では 1 バイトも書いていない**ので、相手のドキュメントも手元の編集も両方無事。Overwrite を選んだときは置き換える前の内容を `<name>.nwsess.bak` に残す。
  - 照合は mtime ではなく**ファイル内容の SHA-256**。共有上の mtime はサーバー側の時計・粒度・クライアントのキャッシュを通ってくるので 2 台のマシンで一致しない。新しく書くようになった `revision` / `saved_by` / `saved_at` / `session_id` は「誰の保存とぶつかったか」を表示するためのもので、判定には使わない（古いビルドや外部ツールは `revision` を上げないため）。すべて省略可能フィールドなので、既存セッションはそのまま開き、古いビルドも読める。
  - 照合はサイドカーの WAV エンコード前（無駄な数秒を使わないため）と、ドキュメント commit の直前（こちらが本番）の 2 回。ロックが無い以上、最後の照合と rename の間の数ミリ秒だけは埋められない — そこは正直に `docs/NWPROJ_PLAN.md` に書いた。
- **編集音声のサイドカーがファイル名を奪い合って、実際に上書きし合っていた**。名前が `data/tab_0000.wav` のようにタブの**インデックス由来**で、managed asset も `assets/<id>/<revision>.wav` と**単なるカウンタ由来**。id も index も共有セッションの中に入っているので、2 人が同じセッションを編集すると別々の音声が同じファイル名に書かれる。**ドキュメント側で競合を検知しても、音声はもう壊れた後**だった。名前を**内容のハッシュ**（`data/<sha16>.wav`、`assets/<id>/<rev>-<sha16>.wav`）にして根本から無くした。同じ音声は自動的に重複排除されるので、内容が変わっていないタブを再保存してもファイルは増えない。古い名前を参照する既存セッションはそのまま開き、次の保存で移行する。
- **「開くだけ」の人が、共有ファイルに書き込んでいた**。パス自己修復が成立すると、開いた側が `std::fs::write` で非アトミックに書き戻していた（GUI・CLI 両方）。共有では全員が開くたびに書き、しかも書き込み途中を他人が読む。**書き戻しを廃止**し、修復はメモリ上に留めて次の明示的な保存で反映するようにした。
- **CLI の書き込みが完全に非アトミックだった**。約 35 個の mutating コマンドが通る `write_project_file` は素の `std::fs::write` で、遅い共有への書き込みが中断されればセッションは切り詰められて壊れる。GUI と同じ「stage → 照合 → atomic replace」に統一した。競合したら**黙って上書きせず非ゼロ終了**し、誰の保存とぶつかったかを言う。意図的に上書きするときは `--force`（この場合も `.bak` を残す）。
- **他の人の保存に気付けるようにした**。`.nwsess` を低頻度でポーリングし（ローカル 5s / 共有 20s、実測コストに比例してバックオフ）、内容が変わったらトップバーに `⟳ changed on disk` を出す。トーストは 6 秒で消えてしまうので、**ユーザーが行動するまで消えない表示**にしてある。再読込は手動（`File > Reload Session from Disk...`）— 自動再読込は未保存の編集を捨ててしまうので行わない。共有が切れた場合は「変更された」ではなく読み取り失敗として扱い、誤報しない。
- **共有の一時的な失敗で保存を失わなくなった**。ウイルス対策や他クライアントが一瞬ファイルを掴んだだけで `MoveFileExW` が sharing violation を返し、保存がそのまま失敗していた。read / write / rename を最大 3 回（100ms → 300ms → 900ms 後）再試行する。
- **0 バイトのセッションを、TOML パースエラーではなく「前回の保存が中断された可能性があります」と報告する**ようにした。`.bak` があればその場所も出す。
- **マウントの仕方が人によって違っても開けるようにした**。共有に新規保存したセッションは `path_mode = relative` を既定にする（`Z:\Proj` と `\\server\share\Proj` の差を根本回避）。既存の絶対パスセッションには、修復の最終手段としてドライブレター ⇔ UNC の相互変換（`WNetGetConnectionW`）を追加した。
- 保存者名は `prefs.txt` の `display_name=` で設定できる（未設定なら OS のユーザー名 + ホスト名）。この名前はチームが既に共有しているセッションファイルにだけ書かれる。
- **既知の制限**として `docs/NWPROJ_PLAN.md` に明記した: 孤児サイドカーの自動削除は行わない（他人の最新ドキュメントが参照している可能性を判定できないため。掃除するのは 24 時間以上前の `*.stage` / `*.tmp` だけ）、テーマや選択位置などの個人設定が共有ドキュメントに入ったままであること、マージは行わないこと。

## 0.20260830.1 - 2026-08-30

### インストーラーを Inno Setup から NSIS へ移した

- **「買ってください」と言ってくるビルドツールが、商用リリースの最後の前提条件だった**: 依存グラフ側の商用ブロッカーは既に片付いていた（GPL/AGPL 依存ゼロ、LAME は LGPL-2.0 §6(b) の差し替え可能 DLL、Symphonia の MPL-2.0 は file-level）。残っていたのは**配布物に一切含まれない**ビルド専用ツール、Inno Setup だけ。公式は年商 5,000 USD 超の商用利用者全員に商用ライセンス購入を要請しており（法的には必須ではないと明記されているが）、それに従って `INNO_SETUP_LICENSE_KEY` 未設定のビルドを「非商用/テスト専用」と自称していた。つまり鍵を買うまで、この repo が出すインストーラーは全部テスト用という扱いだった。
  - **NSIS へ置き換えた**。zlib/libpng ライセンスで「商用アプリケーションを含むあらゆる目的での使用」を無償許諾しており、購入要請も謝辞義務もない。LZMA 圧縮モジュールだけは CPL-1.0 だが、Igor Pavlov と Amir Szekely による明示的なリンク例外があるため、モジュール自体を改変しない限り生成物に義務は及ばない。
  - WiX v6 も検討したが、収益を生む利用に Open Source Maintenance Fee が**必須**で、Inno Setup より条件が悪いので採らなかった。
  - `INNO_SETUP_LICENSE_KEY` secret は不要になった。release ワークフローから鍵の設定ステップごと削除している。
- **`installer/NeoWaves.iss` → `installer/NeoWaves.nsi`**: 挙動は移植であって作り直しではない。ライセンス同意ページ（`LICENSE` の MIT 全文）、`{autopf}` / per-user のインストール先分岐、前回インストール先の再利用、スタートメニューとデスクトップのショートカット、13 拡張子の関連付けタスク（既定 OFF）、`libmp3lame.dll` を EXE に取り込まず別ファイルで配ること、`LICENSE` と `THIRD_PARTY_NOTICES.txt` の同梱、インストール後の昇格を落とした起動 — 全部そのまま。
  - **`SetupLogging` 相当だけは無い**。NSIS の標準ビルドには実行時ログ機能がないため、これ 1 つだけ意図的に落とした。
  - **関連付けの後始末が良くなった**: 従来は関連付けを解除するとき拡張子の既定値を削除するだけで、NeoWaves より前にその拡張子を持っていたアプリに戻らなかった。元の ProgId を退避しておき、アンインストール時に**まだ NeoWaves が持っていれば**元へ返すようにした。
  - **既存の Inno 版からのアップグレード経路を用意した**: 旧 `.iss` は `ArchitecturesInstallIn64BitMode` を設定していなかったので、Inno は 32bit モードで動いていた — 実際のインストール先は `Program Files (x86)\\NeoWaves`、アンインストール登録は `WOW6432Node` 側。新インストーラーは両方の registry view から旧インストール先を探し、**見つかればそこへ上書きする**（勝手に `Program Files` 側へ二重インストールしない）。同時に旧 Inno のアンインストール登録と `unins000.exe` / `unins000.dat` を撤去するので、「プログラムの追加と削除」に NeoWaves が 2 つ並ぶことはない。新規インストールは `Program Files` 側になる。
  - **起動中の NeoWaves の閉じ方**: Windows は実行中の EXE を書き込みロックするので、上書き対象を追記モードで開けるかどうかで判定する。ロックされていれば `taskkill`（`/F` なし = WM_CLOSE）で Inno と同じ穏当な終了を促し、2 巡しても閉じなければ Retry/Cancel を出す。外部プラグイン（nsProcess 等）は入れていない — バイナリを 1 個も増やさないのが今回の趣旨なので。
- **`commands/build_installer.ps1`**: `ISCC.exe` の探索・実行を `makensis.exe` に差し替え。`Resource update error (110)` のリトライループと `%TEMP%` へのフォールバックは ISCC 固有の失敗モードなので削除した。バージョン自動採番、`Sync-RuntimeDlls`、smoke checklist、`installer\out\installer_<buildid>\NeoWaves-Setup-<version>-<buildid>.exe` という出力レイアウトは変えていない（release ワークフローがこのパスを glob している）。パラメータ名は `-IssPath` / `-IsccPath` → `-NsiPath` / `-MakensisPath`、Inno 固有の `-InstallerAppId` は削除。
- **ライセンス表記を実態に合わせた**: `THIRD_PARTY_NOTICES.txt` と Help → Licenses の `INSTALLER BUILD TOOL` 節を、購入要請の話から「そもそも要請がない」話へ差し替え。`licenses::tests::the_installer_build_tool_asks_nothing_of_a_commercial_release` が、この節が NSIS と zlib/libpng に言及し続けること、そして Inno Setup が戻ってこないことを固定する。
- **検証の申し送り**: Windows版`makensis.exe`でスクリプトのコンパイルと同梱物の中身を検証する。上書きアップグレード、関連付けの登録/解除、昇格を落とした起動の3点は、`build_installer.ps1`が最後に出すsmoke checklistに従って実機確認する。

## 0.20260830.0 - 2026-08-30

- **Live frame profiler in the Debug window**: added real FPS/cadence history, a stacked UI-thread phase graph, deferred-work markers, and a recent P95 blocker table with per-stage last/average/peak timings. Capture runs only while the panel is visible and can be held or cleared for inspection.

- **モニター音量を起動後も保持**: 上部Volumeの最後の確定値をprefsへ保存し、次回起動時とオーディオエンジン再生成後に同じゲインを適用する。
- **ループ端のゼロクロス吸着をAlt専用に修正**: 通常ドラッグは任意サンプルへ動き、近接マーカーだけに吸着する。`Alt`中はマーカー優先でゼロクロスへ吸着し、`R`の常時切替は廃止した。
- **スペクトログラム色レンジを直接調整可能に**: Spectrogram / Freq Log / Melの右側へ上下限dBレールを追加。解析キャッシュを保持したまま色の割り当てだけを更新し、設定はprefsとセッションへ保存する。

- **Editorの波形が倍率によって途切れる問題を修正**: raw表示からmin/max表示へ切り替わる倍率でも、隣接するデバイスピクセル列を連結して波形を連続表示するようにした。pyramidのLODは固定倍率ではなく実際の1列あたりサンプル数とキャッシュ粒度から選択し、32 samples/px付近やHiDPI表示で波形が急に粗くなる問題も抑制した。

### 動画ポップアウト再生の安定化
- **大きい動画ウィンドウでのちらつきと `decoding...` の反復を抑制**: Media Foundationへ表示サイズのRGB32出力を要求し、フレームを小さなchunkで逐次受信するようにした。デコード不足時は音声時刻以下の最後のフレームを保持し、古い先読みはseek・resize・追いつき要求で途中キャンセルする。
- **滑らかさ優先の自動画質と再生前バッファを追加**: ポップアウトは表示サイズ・デコード時間・ring残量・性能tierのメモリ上限から640p〜1080p相当を自動選択する。Play時は最大150msだけ映像を先読みし、resize中は200ms安定するまで既存textureを拡縮して再デコードを連発しない。
- **AAC末尾の`preview failed`を修正**: AACの末尾パディングで音声尺が映像尺をわずかに超えるMP4でも、映像末尾を越えてseekせず最後のフレームを保持する。

## 0.20260827.0 - 2026-08-27

### 音声同期の動画ポップアウト

- **動画を別ウィンドウで確認できるようになった**: エディター内動画パネル右上のボタンから、映像だけをリサイズ可能なネイティブウィンドウへ表示できる。表示対象は開いた動画タブへ固定され、別の動画でボタンを押すと1つのウィンドウがその動画へ切り替わる。元タブを閉じるとウィンドウも閉じる。
- **音声source-timeを映像の唯一の時計にした**: インライン表示と別ウィンドウは同じframe ring・texture・decoder workerを共有し、音声時刻以下の最新PTSだけを描く。別の音声ソースへ移った間は最後に同期したフレームで止まり、元動画へ戻ると現在の音声時刻へ再同期する。
- **大きいウィンドウでもdecode負荷を制限**: インラインと別ウィンドウの要求サイズを分け、大きい側をworkerへ渡す一方、decode面積は横動画1920×1080・縦動画1080×1920のbox内に制限する。unsupported codecやdecode失敗は黒背景の状態表示へフォールバックし、音声再生を妨げない。

### AAC 音声付き mp4 が再生できるようになった

- **`AAC UNSUPPORTED` は「コーデックを同梱しない」という判断であって、「再生できない」理由ではなかった**: AAC の特許義務はデコーダーを**配る側**に来る。だから NeoWaves は自前の AAC コーデックを持たない。一方 Windows には Microsoft が OS の一部としてライセンス済みの AAC デコーダーが入っていて、documented な API から呼べる。映像 (H.264 / HEVC) で既にやっていたことを音声にも適用し、Media Foundation のデコーダーを**借りて** AAC を鳴らすようにした。依存グラフにコーデックは 1 つも増えていない（`windows` crate は映像プレビューで既に入っている）。
  - Windows では mp4 / mov / m4v / 3gp / 3g2 と m4a の AAC 音声が、他の音声と同じように再生・波形表示・ラウドネス計測・プレビューできる。無音タイムラインへの切り替えは起きない。
  - OS デコーダーが無い環境（Linux / macOS、および Media Feature Pack を入れていない Windows N / KN）では従来どおり `AAC UNSUPPORTED` と表示し、映像は無音タイムラインで再生・シークできる。判定は「Windows だから」ではなく、Media Foundation に AAC デコーダーが登録されているかを実際に問い合わせている。
  - 判定の入り口は `audio_io::aac_decode_available()` と `audio_io::isobmff_aac_audio_unsupported()` の 2 つだけ。リストの列・エディタのバッジ・無音タイムラインの選択はすべてそこを見る。
  - AAC の**書き出し**は引き続き全環境で非対応。借りられるエンコーダーは無く、そもそも動画は読み込み専用なので書き戻す先も無い。
  - 音声側のリーダーでは映像トラックを選択解除している。選んだままだと、音に辿り着くためだけに全フレームをもう一度デコードすることになる。
  - `cfg(any())` で全ビルドから外れていた FDK ベースのデコード実装（将来の参考として死蔵していたもの）を削除した。エンコード側の参考実装は「明示的にライセンスされた AAC エンコーダー」用にそのまま残している。
- **ライセンス表記を実態に合わせた**: 「AAC は encode / decode とも非搭載」から「AAC コーデックは同梱しない。デコードは OS のデコーダーを借りる。エンコードは非対応」へ。Help → Licenses と `THIRD_PARTY_NOTICES.txt` の両方に、OS から借りているコーデック（AAC 音声と H.264 / HEVC 映像）についての節を追加した。ソフトウェアライセンスが特許その他の権利まで許諾するわけではない、という但し書きは従来どおり。

## 0.20260825.2 - 2026-08-25

### コーデックと商用配布

- **AAC は一旦非対応**: Fraunhofer FDK と Symphonia AAC decoder を依存グラフから削除し、AAC の読み書きを無効化。AAC 音声付き動画はファイルエラーにせず `AAC UNSUPPORTED` と表示し、映像は無音タイムラインで再生・シークできる。音声トラック自体が無い動画は従来どおり `NO AUDIO`。
- **LAME を動的リンクへ変更**: LAME 3.100 を `libmp3lame.dll` として EXE から分離し、利用者が ABI 互換版へ差し替え可能にした。静的 LGPL の Rust wrapper も外し、NeoWaves の MIT FFI に置換。MP3 の CBR 設定・チャンネル処理・書き出し経路は維持。
- **配布ライセンスをインストーラーへ同梱**: `LICENSE` と自動生成 `THIRD_PARTY_NOTICES.txt` を追加し、Help → Licenses と同じ根拠をオフラインでも確認可能にした。リリース CI は Cargo.lock からライセンス一覧を再生成し、差分や拒否ライセンスがあれば停止する。
- **実行時モデルを commit 固定**: Whisper / Silero VAD / music ONNX の `main` 追従をやめ、監査済み revision を指定。
- **ライセンススナップショットを再現可能化**: `cargo-about` が環境ごとに異なる順序で返す本文を安定ソートし、Windows checkout の CRLF/LF と行末空白もプール前に正規化。CI と開発機で `MIT-N` の内部キーや通知文書が変わらないようにした。

### 波形・ループ・ショートカットの UX 改善

**波形が読めるようになった**

- **中心線が中心線に見えるようになった**: 振幅 0 の線は前から描かれていたが、±6 / ±12 dBFS のグリッドと**まったく同じ色・同じ太さ**だった。波形がどこを中心に振れているかを示す唯一の線が、目盛りにすぎない 2 本と見分けがつかなかった。中心線を明るくし、グリッドを一段落とした（振幅ナビ・タイムナビが既に使っている 0 線と同系統の色）。左のガターに `0` ラベルも出る。ガターは波形領域の外なので、波形やマーカーに重なることはない。ステレオ・マルチチャンネルではレーンごとに出る。縦ズーム / パン時にレーンの中央と振幅 0 は一致しないので、Y は `waveform_center_y` から取っている。
- **波形の色が、振幅が上がるほど暗くなっていた**: 従来は cyan → red の 1 本の補間で、(1) 中点が `rgb(167,135,162)` の**灰紫**に沈み、そこが普通の素材のいる場所だった、(2) 静かな側の輝度 170 に対して大きい側が 125 と、**明るさが振幅と逆向き**だった。つまりいちばん見たいピークがいちばん暗かった。t=0.62 に琥珀色の中継点を置き、全域で彩度 40 以上・輝度 155 以上を保つようにした。普通の素材がランプのいちばん明るいところに乗り、大きいところは従来どおり赤に寄るのでレベルも一目で分かる。編集プレビューのミントグリーンは変えていないので、プレビュー中かどうかの区別は保たれる。`amp_to_color` はリストの Wave 列とも共有なので、サムネイルも同時に読みやすくなる。
- **聞こえていないチャンネルは沈むようになった**: mute / solo は M/S ボタンとエンジンには効いていたがキャンバスには届いておらず、消しているチャンネルが鳴っているものとまったく同じ明るさで描かれていた。そのレーンだけ背景側へ沈める（選択範囲・マーカー・再生ヘッドはその上に残るので、どのレーンでも同じように読める）。可聴判定はエンジンと同じ規則（どこかに solo があれば solo 以外は無音）。

**ループ端を掴む操作が、掴む前に動かなくなった**

- **ループ端のクリックが、ループを動かしてしまっていた**: 「再生位置をループ端に合わせよう」とクリックすると、その瞬間にループ点のほうが動いた。ポインタの移動は不要で、押した瞬間に決まっていた。しかも武装と移動で座標の換算が違う（ハンドルはサンプル境界、x→サンプルは中心基準）ので、クリックした場所にすら来なかった。同じクリックが Undo ステップまで積んでいた（フレーム末尾で無条件に push されるため）。押した時点で武装し、**0.5px 動いてから**初めてループ点を動かすようにした。動かなかった押下はクリックのままなので、本来やりたかったシークになる。
- **画面外のループ端がキャンバスの縁で掴めてしまう問題**: ヒットテストが clamped な x を使っていたため、左に流れたループは両端ともキャンバスの縁にいることになり、ループから遠く離れた場所の押下に反応していた。選択範囲の端と同じく unclamped な x と `contains_boundary` を使う。ホバーカーソルも同じ判定を見るので、押しても掴めないハンドルをカーソルが約束することはない。
- **短いループの終了端が掴めなかった**: 端の選択が `if / else if` で、掴み半径 2 つぶんより短いループでは常に開始側が勝っていた。`nearest_handle`（そのテストがこのコードを反面教師として名指ししていた）に置き換えた。掴み半径の `7.0` はヒットテスト・ホバーカーソル・ストレッチグリップの 3 箇所に散っていたので 1 つの定数にした。
- **ループ端がマーカーに吸着するようになった**: しきい値は画面上のピクセル（8px）なので、ズーム率が変わっても操作感は同じ。吸着したループ点は**マーカーとまったく同じ sample index** を取り、書き出しや再生もその値を使う。通常時はマーカーだけ、`Alt`中はマーカー優先でゼロクロスにも吸着する。
- **ループのハンドルが、掴める場所に描かれるようになった**: S / E のつまみはキャンバス最上部に描かれていたが、そこは前回タイムストレッチのグリップが取った帯だった。「ここを掴めばループが動く」という唯一の目印が、掴むと別のことが起きる場所に立っていた。ストレッチ帯のすぐ下から始まる▼に描き直し、ラベルもその下に移した。
- 半サンプルの話: 極端なズームではループ端とマーカーは半サンプルぶんずれて描かれる。範囲の端はサンプルとサンプルの**間**にあり、マーカーはサンプルの**上**にあるため。内部の sample index は一致している。

**ナビゲーションと表示**

- **←/→ がループ端でも止まるようになった**: 左右キーはグリッド刻みのシークで、跨いだマーカーがあればそこで止まる仕組みだった。ループの開始 / 終了はその対象に入っていなかったが、再生位置をぴったり置きたい場所としては最も多い 2 つ（継ぎ目の確認、ループ位置の確認）。1 サンプル刻みになるまでズームするしか方法が無かった。マーカーと同格のランドマークにした。マーカーがループ点と同じ位置にあっても停止は 1 回だけで、次の押下は先へ進む。
- **Shift+ホイールの横スクロールが、表示幅に対して一定になった**: 固定 60px だったので、長尺を横断するのに必要なノッチ数がズーム率に関係なく同じで、ウィンドウが広いほど 1 ノッチで進む割合が小さかった。表示幅の 15%／ノッチにした。ホイールの回転量も捨てていた（`.signum()`）ため、egui が 1 ノッチを数フレームに平滑化するぶんだけフレームレート任せに何段も進んでいた。実際の delta に比例させたので、1 ノッチは 1 ノッチになる。長尺用に Settings の **Horizontal scroll speed**（1x / 2x / 4x）を追加。
  - 計画では Ctrl+Shift+ホイールを高速版に充てる案だったが、このコードでは使えない。egui のズーム修飾キーが COMMAND で、一致するとスクロール量がズーム側に回され、エディタに届く前にゼロにされる — Ctrl+ホイール と区別できない。
- **`Shift+Z` でループ領域へズーム**: ループを詰める作業は両端を近くで見る作業なのに、両端が画面に入るまで手でズームするしか方法が無かった。`Z`（選択範囲へズーム）と同じ挙動で、両者は 1 つの実装を共有している。**ループハンドルのダブルクリック**でも、その端を中心に 1 段ズームインする（見ている場所が中央に留まる）。
  - 計画では右クリックメニュー案だったが、キャンバスの右ボタンは既にシーク / Shift+右ドラッグの範囲選択に使われており、コンテキストメニューはそれと競合する。
- **`Tab` / `Shift+Tab` でエディタタブを巡回**: 末尾で先頭に回る。Editor が前面でタブが 2 つ以上あるときだけで、リストや単一タブでは従来どおり egui のフォーカス移動。テキスト欄・メタデータ欄・モーダル・キー再割り当て中は発火しない。
  - egui は Tab をフレーム冒頭でフォーカス移動に使うので、タブを切り替えた時点で既にどこかのウィジェットにフォーカスが移っている。放置すると**次の** Tab を奪われ、テキスト欄に入っていたら以降の無修飾キーも全部そちらへ行く。切り替え後にフォーカスを手放している。

**音量とラウドネス**

- **Volume は 0 dB で始まり、ダブルクリックで 0 dB に戻る**: 毎回 12 dB 下から始まり、戻す手段はスライダを引いて数値を読むか `D` を 12 回叩くかしか無かった。ラベル・トラック・数値表示のどこをダブルクリックしても戻る。丸めた表示では正確な値が分からないので、ホバーテキストに正確な dB と操作方法を出している。
- **スライダの上半分が効くようになった**: `set_volume` が線形値を 1.0 で頭打ちにしていたため、-80〜+6 dB を名乗るスライダの 0 dB より上は全部同じだった。既定が 0 dB になると真っ先に触る領域なので、上限をスライダ自身の +6 dB に合わせた。出力サンプルは従来どおり [-1, 1] にクランプされるので、上げれば歪む — モニタゲインとして期待される挙動。
- **LUFS がモニタ音量に振り回されなくなった**: トップバーの M / S / TP は最終出力（マスター音量・シークのアンチクリックランプ・出力クランプを全部通した後）から測っていた。音量を絞ると素材が実際より小さく見え、リストのオフライン LUFS(I) 列とも納品目標とも比較できなかった。ラウドネス表示の意味の大半がそこにある。タップを「素材そのもの」に移した — チャンネルルーティングと mute/solo は実際に鳴っているものを変えるので含み、再生音量の都合は含まない。ファイル単位の pending gain はエンジンに渡る前にバッファへ焼かれているので従来どおり乗る（リストの LUFS 列も同じく足しているので一致する）。dBFS の出力メーターは出力を見るものなので変更していない。

**取り込みと発見しやすさ**

- **エクスプローラ / Finder からコピーした音源を貼り付けられるようになった**: OS のファイルリストを読める環境でしか動かず、しかも失敗しても何も言わなかった。ファイルリストが無いときはクリップボードのテキストを見るようにした（`text/uri-list` の `file://` URI、改行区切りのパス、単一パス）。どのファイルマネージャも少なくともどれかをテキスト側に置くので、これで環境を問わず動く。`file://` のパーセントデコードは**バイト単位**で行う — 文字単位でやると非 ASCII のファイル名が壊れる。この用途では日本語のファイル名がそれに当たる。Windows のドライブレター (`file:///C:/...`) と UNC ホスト (`file://server/share/...`) も解釈する。パスの形をしていない行は捨てるので、文章を貼り付けても何も起きない。
  - **結果を報告するようになった**: 追加した件数と、重複 / 非対応 / 見つからなかった件数をトーストで出す。ドラッグ&ドロップなら何を落としたか見えているが、クリップボードは何かがリストに現れるまで不可視で、黙って弾かれると「貼り付けが壊れている」としか見えない。重複と非対応の判定規則自体は D&D と同じまま。
- **ループのショートカットが Help だけで理解できるようになった**: `L` は「Apply loop from selection/markers, else cycle loop mode」という 1 つの Action に 3 つの挙動が入っていて、ループモードの切り替えはその 3 番目としてしか呼べなかった。モード切り替えを `Shift+L` として独立させ、`L` は「ループ領域があれば Marker loop / 無ければ選択範囲からループ / どちらも無ければモード切り替え」と、従来の挙動そのままに整理した。既に `L` を再割り当てしていた人の設定は失われない（prefs 上の旧 Action 名を受け付ける）。
  - Help はコンテキストごとの 1 枚の表で、Editor の 40 行あまりが一続きに並び、ループのキーはその真ん中に埋もれていた — どのキーか知っている人しか見つけられない。全行にカテゴリ（Playback / Navigation / Selection / Loop / Editing / View / Tabs & Windows / Files）を持たせて分類した。1 行では言い切れない行には 2 行目を足している（`K` がループ終了を追い越すとどうなるか、ループ領域が無いときモード切り替えがどうなるか、`L` が何にフォールバックするか、←/→ の刻みが何で止まるか）。**Keyboard Shortcuts** と **Customize Shortcuts** の両方が同じ分類・同じ説明を使う。
- **`Ctrl+A` でファイル全体を選択**: エディタには全選択が無く、全体を選ぶにはドラッグするしかなかった。長いファイルで両端が画面に入るズーム率だと、そのドラッグは端に正確に届かない。破壊的な操作ではないので、`T` / `V` / `Delete` / `Ctrl+M` と違って読み取り専用ソースでも使える（メタデータインスペクタでは、そこにある表や入力欄のものなので発火しない）。リスト側の `Ctrl+A` はそのまま。
  - ズームアウトしていると全体選択と「両端の少し手前まで」の選択は見た目が同じなので、結果を明示する — トーストと、選択が全体を覆っている間ずっとインスペクタの範囲表示に出る `(entire file)`。

### エディタ: 範囲を伸ばすのと、音を伸ばすのを分けた

- **選択範囲の縦線をドラッグしても、もう音は書き換わらない**: 選択範囲の両端はキャンバス全高のタイムストレッチ用グリップになっていて、線のどこを掴んでも破壊的なリサンプルが走っていた。範囲をほんの少し詰めたいだけの操作が、そのたびに音声そのものを作り直していたことになる。縦線の本体は**範囲を伸縮するだけ**の操作に戻した。掴んだ側だけが動き、反対の端は固定、`Alt` でゼロクロススナップ、反対の端を追い越せば範囲が反転する（ワーカーも Undo ステップも増えない）。
- **音を伸ばすのは、上端のグリップだけになった**: タイムストレッチはキャンバス最上部の小さなつまみからのみ始まる。的の大きさを役割に合わせたということで、取り返しのつく操作（範囲の伸縮）に線の全高を、取り返しのつかない操作（音声の書き換え）に 18px のつまみを割り当てている。掴める範囲は描かれているつまみと同じ寸法で、見た目より広く反応することはない。
  - つまみは 10x18px に少し大きくし、中に左右の矢印を描いた。これまで波形の**中央**に描かれていた `<>` の山記号は削除した。あれは全高が掴める時代の目印で、いまは掴めない場所に立っているだけの記号になる。
  - ホバーしたカーソルで見分けられる: 上端のグリップは手のひら（ドラッグ中は握った手）、縦線の残りは従来どおり ↔。選択範囲の端はどの表示モードでも伸縮できるので、カーソルの表示もスペクトログラム等で出るようになった。
- **Loop Edit でループ端と選択端が重なっていても、両方使える**: 「選択範囲からループを作る」を通した直後は 2 つの端がぴったり同じ x に立つ。これまではループマーカーが高さを問わず勝っていて、その場所ではストレッチが一切できなかった。いまは高さで分担する — 上端のグリップはストレッチ、その下の縦線はループマーカーの移動。
- **掴んだ操作は離すまで持ち主が変わらない**: ループマーカーのドラッグはボタンの「押下中」で武装するため、範囲の伸縮中にポインタがたまたまループ端の 7px 以内を通ると、そこから先はループが動きはじめて範囲のほうが固まる、という取り違えが起きえた。押した時点の持ち主が離すまで持ち続ける。

### 動画ファイルが開けるようになった
- **mp4 / mov の対応音声がそのまま扱える**: 音声が動画コンテナに入ったまま届いた素材は、これまで別のツールで音声を抜き出さないとリストにすら載らなかった。`.mp4` / `.mov` / `.m4v` / `.3gp` / `.3g2` をリストへ追加でき、Symphonia が対応する音声トラックは通常どおり再生・測定できる。AAC は上記方針によりデコードせず、映像だけを無音タイムラインで扱う。
  - この対応の前提として、**映像トラックのある mp4 は音声すら読めなかった**バグを直している。symphonia の `default_track()` は「ファイルの先頭のトラック」を返し、その ISO-BMFF リーダーは映像トラックも空のコーデック情報つきでトラックとして並べる。一般的な mp4 は映像トラックが先頭なので、デコーダには「コーデック不明」のトラックが渡され、そこで失敗していた。音声コーデックを名乗る最初のトラックを選ぶようになった。これは `.mp4` を `.m4a` にリネームしただけのファイルにも効く。
  - QuickTime の `.mov` が持つ非圧縮音声 (`sowt` / `twos` / `in24` / `lpcm`) と ALAC は Symphonia の対応 codec で読む。AAC は明示的な非対応状態として分離した。
- **エディタの Mini Meter に映像が出る**: どの音を触っているのかが、波形の形ではなく絵で分かる。SCOPE の左に映像パネルが増え、再生位置のフレームを表示する。ストリップの幅が足りなくなったときに先に消えるのは SCOPE のほうで、映像は残る。
  - **音に張り付かせるために先読みしている**。1 フレームずつ「要求 → デコード → 転送」を往復すると、絵は常に音より 1〜2 フレーム遅れる。再生中はワーカーが再生位置の先を連続してデコードし、UI は手元にあるフレームから「再生位置が今まさに到達したもの」を選ぶだけなので、待ち時間が入らない。表示する時刻は描画されている再生ヘッドそのものから引いているので、絵とヘッドが画面上で食い違うことはない。
  - 映像のデコードは Windows では Media Foundation (H.264 / HEVC / VP9 など、OS の持つコーデックすべて、ハードウェア支援あり)、それ以外の環境では同梱ビルドの OpenH264 (H.264 のみ) が受け持つ。どちらでも開けないコーデック (ProRes / AV1 など) では**音声は普通に鳴り**、パネルだけが `no preview (コーデック名)` になる。映像トラックの無い `.mp4` ではパネル自体が出ない。
  - 縦向きで撮った動画は `tkhd` の回転行列を読んで正しい向きで出す。フレームはワーカー側でパネルの大きさまで縮小してから渡すので、4K の素材でも UI スレッドが持つのはパネル 1 枚分だけ。
- **サムネイルは埋め込みアートワーク、無ければ 1 フレーム目**: 動画も ISO-BMFF なので `covr` atom は m4a と同じように読める。入っていなければ最初のキーフレームを 1 枚だけデコードしてサムネイルにする。1 フレーム取り出す処理は埋め込み画像を読むのと比べて桁違いに重いので、同時に走れる本数を機械の性能ティアから決めており、2 コアの機械では 1 フレーム目の抽出そのものを行わない。フォルダを開いた直後の一覧表示が遅くなることはない。
- **動画は読み込み専用**: 映像エンコーダを持たないので、編集して書き戻せば映像を失うか、頼まれていないファイルを作ることになる。エディタのツールパネルはまるごと無効化され、パスの隣に `READ-ONLY` バッジが出る。破壊系のショートカット (`T` / `V` / `Delete` / `Ctrl+M`)、リストの per-file gain、変換メニュー、書き出しも同じ判断を見て止まる。marker と loop はサイドカー JSON に書かれるので、動画ファイル自体は一切書き換えられない。
  - この判断は拡張子の文字列比較ではなく `src/media_kind.rs` の 1 箇所に集約してある。将来編集を解放するときに書き換えるのはそこだけで、20 箇所近いゲートが追従する。

### Editor: the Trim panel explains itself
- **The Trim buttons say what they do, and which key does it**: `Mode`, `Preview` and `Apply` had no hover text at all, so nothing in the panel indicated that the buttons act on the selection directly — which is what led to a report that a range had to be "Set" first, for a Set button this tool does not have. Each control now describes its effect and names the equivalent shortcut, read through the rebinding map so a customised key shows the key you actually have. With several ranges selected, the hover text says how many will be affected.
- **"Add Trim As Virtual" is described in plain terms**: its hover text said "Add the trim range as a virtual item", which a user reported not understanding. It now says what it does — export the selected range as a separate item in the list, leaving the current file untouched, written to disk only when saved — along with the naming and the one-item-per-range behaviour.

### Editor: one range on the waveform
- **Selection edges can be dragged**: adjusting a range meant redrawing it from scratch — Shift+click only ever moved the edge away from the drag's anchor, so after a left-to-right drag the start could not be pulled back at all. Both edges now carry a grab handle and can be dragged to lengthen or shorten the range, in every tool, with the opposite edge staying put. `Alt` snaps to a zero crossing and the playhead snaps as it does when drawing a selection, so a nudged edge lands where a fresh drag would. Dragging an edge past the other flips the range, exactly like drawing one. (Both edges are draggable in every tool; the Time Stretch grip that later took over the top of the same line is described under "エディタ: 範囲を伸ばすのと、音を伸ばすのを分けた" above.)
- **The orange Trim band is gone**: the waveform drew a second, orange range for the Trim tool on top of the blue selection. Nothing in the UI had set that range for some time — only Auto Trim wrote it, and Auto Trim writes the *same* span to the selection, so one range was being drawn twice and read as "a range you had to establish separately". Hovering its edges even produced a resize cursor, for a drag that was never implemented. The selection is now the only range the waveform draws, and the header no longer labels a range it isn't drawing.

### Editor: selection shortcuts
- **`Ctrl+M` mutes the selection**: silencing a range meant switching the Trim tool to Mode=Mute and pressing Apply. It now has a key, alongside `T` (trim to selection) and `V` (export as a new list item). `Ctrl` rather than a bare `M`, which is already "add a marker".
- **Cut moved from `C` to `Delete`**: deleting the selection and closing the gap is the Trim tool's Cut, and `Delete` is the key people reach for. `C` is now unassigned — pressing it does nothing, which is the safe failure for a key that used to delete audio. Anyone who had already rebound this action keeps their binding, and `C` can be restored from Help > Customize Shortcuts.
- **The destructive selection keys no longer reach a tab you cannot see**: `T`, `V`, `Delete` and `Ctrl+M` now require the editor workspace to actually be on screen, and stand down while the Metadata inspector is showing. Previously `T` and `V` fired whenever an editor tab existed at all, so they could trim audio while the list or the recording view was in front — and `Delete`, which people press reflexively in a table of metadata fields, would have inherited that.
- **Muting several ranges at once is one undo step**: the Trim tool's Mode=Mute Apply muted each selected range as its own edit, so `Ctrl+Z` un-muted only the last one while the UI promised a single undo. Trim and Cut already had multi-range counterparts; Mute now has one too, and both the button and the new shortcut use it.

### List view
- **The list ends with a row that states the total**: scrolling to the bottom left the user judging "am I at the end?" from the last row on screen, and a row clipped by the viewport edge looks exactly like a row with more below it. The list now closes with an `End of list - 178 files` row (`End of list - 12 of 178 files` while a search is active). Reaching it is the signal, and the count answers "how many are there" without reading it off the top bar.
  - It also removes the failure mode entirely rather than only reporting it: the closing row occupies the last scroll position, so the row sitting against the bottom edge is always the marker. A file can no longer be the row that a rounding error in the viewport arithmetic clips — the marker absorbs it. Regression coverage asserts the marker is painted fully inside the viewport at maximum scroll for lists of 1, 5, 400 and 4,000 rows, and for a list shorter than the viewport.
- **The row waveform is a seek bar**: playing a list as a playlist meant every file started from the beginning — there was no way to skip into a file or past a section without opening it in the editor. The Wave column now shows where playback is (a progress fill plus a playhead line) and takes a click or a drag anywhere along it to play from that point. Clicking the waveform of a row that is not currently sounding selects it and starts at the clicked position, so "listen from the middle" is one click.
  - Positions are whole-file fractions, matching the markers and loop region already drawn in the same cell. That matters because the list preview is usually a truncated prefix of the file rather than the whole thing, so the transport's own position is a fraction of the *prefix* — converting through the duration from the header pass keeps the two from disagreeing. It also means the seek bar works on a row whose waveform has not been drawn yet.
  - A plain wav plays through the whole-file streaming transport, so seeking anywhere in it is immediate. For a compressed file, seeking past what has been decoded shades the undecoded tail, marks the requested position, quietly starts a full decode, and jumps there once it arrives — rather than playing from the beginning while it waits. If the decode ends without reaching the target (a shorter file than its header claimed, or a decode error) the position lands on what was decoded instead of waiting forever.
  - Clicking the waveform arms the position but only *starts* playback if Auto Play is on or something was already playing, so the existing transport preferences still hold; the list keeps keyboard focus, so Space plays from the armed position.
  - A left-drag on the waveform scrubs instead of starting a drag-out to the OS. Dragging a file to another application still works from the file name, folder, and other columns.
- **Every filename is listed before any waveform is decoded**: the row waveform comes from a thumbnail that requires reading the whole audio file, and with the Wave column on by default those reads were queued for every visible row from the first frame of a folder load — competing with the directory walker for the same disk and the same worker pool, so on a large folder the file names appeared more slowly than they needed to. Knowing how many files are loaded matters more than seeing their waveforms, so while a scan is still appending rows the list asks only for header data (name, length, format, duration — no decode). The waveforms fill in for the visible rows as soon as the listing is complete, using the re-queue path that already existed. On a folder of a few hundred files nothing changes; on a hundred thousand it is the difference between a list that keeps up and one that does not.
  - The two copies of the "how much metadata may this row ask for" test — one in the row loop, one in the prefetch pass — are now a single decision, evaluated once per frame instead of once per visible row.
  - The scan's per-frame append budget was a hard-coded 3 ms tuned on a developer machine; it now comes from the machine tier like every other per-frame budget, so a two-core laptop gets a smaller slice and an eight-core machine a larger one. The cap that applies while audio is playing is unchanged.
  - **Debug window**: list metadata tasks are counted by depth (`header_only` / `header` / `decode`), so "waveform decodes are being withheld during the load" is something a report can show rather than assert.
- **The row count says what it means**: the status bar showed `Files: N ?`, where the `?` stood for both "the walker is still finding files" and "the metadata workers are still reading tags" — two different waits, only one of which changes the number of rows. During a scan it now reads `Files: 128540 (+383460)`, counting the rows listed so far and how many more have already been found, updated live. Metadata and waveform readiness moved to their own indicator next to it (`Meta: 192`, `Waves: after listing`).
- **Search covers the Note column**: the inline list note is edited in the list and stored in the session, but the top search box matched only the file name, folder, transcript, metadata summary and external columns — so a note written to find a file again could not find it. Notes are now searched alongside those fields, in both substring and Regex mode. Rows with no note cost nothing extra (the match is skipped before it would allocate), and the sliced filter used for lists above the synchronous threshold shares the same predicate, so large lists match identically.
- **The last rows of a long list can be reached again**: scrolling a list longer than one screen stopped a few rows short of the end — the rows existed and were even handed to the table, but were laid out below the clip rect where nothing could bring them into view. The list does its own vertical virtualization (the table is built with `vscroll(false)` so egui never sees a million rows of content height), and its "how many rows fit" calculation divided the available height by the row height alone. `egui_extras` actually advances by the row height *plus* `item_spacing.y`, and the header strip consumes a spacing of its own, so the count came out too high and the scroll clamp derived from it — `total - visible` — stopped early. With the default 26 px rows and 3 px spacing that hid about four rows at the bottom of an 800 px list, and more on a taller window. The count is now computed from the real row pitch, which also fixes three things that shared the same number: the mouse wheel advanced by slightly less than one row per notch, the custom scrollbar's thumb reached the end of its track while rows were still hidden, and End selected the final row but scrolled it just off-screen.
  - Regression coverage asserts the *painted* position of the last row rather than its index in the rendered window. The previous assertion ("the last row is within 200 rows of the scroll offset") held throughout the bug, because the row genuinely was in the window — only its pixels were outside the viewport.

### Responsiveness on low-spec machines
- **Session opening no longer blocks the window**: opening a `.nwsess` painted one "Opening session..." frame and then ran the entire restore inside the next one — reading and repairing the document (which stats every file, tab and external source it references), rebuilding the list (which statted every row again), and decoding every virtual item, cached edit, tab sidecar and preview overlay. On a large session, a slow disk or a network share that is seconds with no message pump, which is what Windows reports as "not responding". The open is now three stages: the document is read and repaired on a worker (computing the per-row existence map while it is already statting those paths), the session's audio is decoded across workers, and only the state update runs on the UI thread. The topbar names the current stage and offers Cancel; saving and destructive edits are refused while a restore is in flight so a half-applied document is never written back. Opening a second session supersedes the first instead of racing it.
- **Adaptive work sizes**: the list sort/filter threshold (50,000 rows), the metadata pool size and the inspection/duplicate worker counts were fixed numbers tuned on a developer workstation; on a two-core laptop the same numbers put seconds of work inside one frame. They now come from a machine tier derived from the core count, which also demotes itself after sustained slow frames. A two-core machine gets one metadata worker and a 2,000-row synchronous-sort ceiling where it used to get three workers and a 50,000-row ceiling. **Settings > Performance > Responsiveness** pins a tier for what the core count cannot see (a VM, a remote desktop, a machine that is busy with something else).
- **One frame budget instead of several**: about sixty drain/pump calls run at the top of every frame. Six capped their own work, but those caps are additive, so a metadata backlog, a spectrogram render, a folder-watch batch and a feature analysis landing together still produced a long frame. Deferrable drains now share one deadline sized by the tier and leave their work queued when it is spent. Latency-critical work (playback, audio device recovery, IPC, input, editor decode/apply completion) is never deferred. Three drains that had no cap at all — editor feature analyses, editor waveform-cache results, and folder-watch events — got one.
- **An idle window now sleeps**: every frame ended by asking for another one 80ms later, so an untouched window sat at ~12fps forever. On a weak integrated GPU that is real CPU the UI thread has to win back from background workers. The loop now asks for a frame only when something needs one; the IPC listener and the folder watcher wake it directly so nothing waits for the user to move the mouse. A window with an open output stream keeps a 1Hz heartbeat so device changes are still noticed.
- **Session Close no longer blocks**: its autosave encoded a WAV sidecar per edited tab and virtual item on the UI thread. It now uses the existing async save and closes when that lands; a failed autosave keeps the session open, as before.
- **Startup paints sooner**: a multi-megabyte system CJK font was read before the first frame. The window opens on the embedded NotoSansJP — which already covers the UI — and a worker swaps the system face in when ready.
- **List row existence checks are spread out**: every visible row's cache entry expired in the same frame it was filled, so a screenful re-statted together — hundreds of milliseconds on a network share. At most eight refresh per frame now; the rest keep their previous answer for a frame or two.

### Sessions on a file server
- **No filesystem call runs on the UI thread any more**. On a network share a single `stat` against an unresponsive server blocks for the SMB timeout — tens of seconds — so a per-frame budget is not enough; the count has to be zero. A background service now answers "does this path exist", and a path it has not resolved yet reads as present so the list does not flash grey on every open. Three callers moved onto it: the list's row check (was 8 stats/frame), the Metadata Inspector's source lookup (one per frame it was visible, uncapped), and the row context menu (one per selected file per frame — 500 selected files meant 500 stats a frame).
- **Session restore does no I/O and no per-clip work on the UI thread**. The four remaining existence checks — one per tab, per virtual source, per managed asset, per external source — are answered by the parse worker, which was already statting those paths for the path repair. Resampling each edited clip to the output rate and building its waveform overview and pyramid also move to the decode workers, so the restore only moves finished buffers into place.
- **A long load shows what it is doing**: "Decoding session audio 12/48 — kick_01.wav" with a progress bar, instead of a bare elapsed counter that made a working load look identical to a hung one. When nothing completes for ten seconds the line becomes "Waiting on kick_01.wav", naming the file the share is not answering for.
- **Background sweeps stop competing with the load**. The folder watcher slept a fixed 3s and re-walked the whole tree; on a share, where one walk can take longer than that, it effectively walked continuously. The delay now tracks the walk's own cost (4x the last one, floored at the configured interval and capped at five minutes), and the watch suspends entirely while a session is opening. The metadata pool and its prefetch drop to two workers and a quarter of their budget when the list's root is remote — more concurrent readers do not make a share faster, they put the file the user opened behind them.

### Diagnostics
- **Long-frame log**: frames over 250ms are recorded with what was running while they ran (session-open, scan, sort, editor-decode, metadata, spectrogram, and whether the frame budget deferred anything). The last 16 appear in the Debug window (F12) alongside the active performance tier, so a report of "it freezes on my machine" can name the cause without the reporter reproducing it under a profiler.

## 0.20260819.0 - 2026-08-19

### List columns and notes
- **Unified list-column manager**: moved column management to `Tools > List Columns...` and replaced the split, arrow-driven interface with one ordered checklist. Built-in, Note, and metadata columns can be shown or hidden and reordered together with vertical drag and drop; layouts remain global- and session-aware.
- **Editable Note column**: added an inline-editable Note field to list items, with Enter/focus commit and Escape cancel behavior. Notes survive path changes, list undo, session round trips, and CLI session inspection.

### Editor notes
- **Position-aware Editor Note tool**: added an Inspector tool for comments attached to the playhead, a time selection, or a time-frequency selection. Notes can be edited or deleted, displayed in time or musical bars/beats, and double-clicked to restore their seek position and selection without changing the active Wave/Spec/Mel view.
- **Edit and session integration**: Editor Notes are stored in `.nwsess`, included in editor undo/redo, and remapped alongside markers and regions by trim, cut/paste, time-stretch, speed, and sample-rate edits while retaining frequency ranges.

### Surround output
- **Speaker-aware channel mapping**: playback used to map source channels onto the device by index, and any output beyond the source's channel count repeated the source's *last* channel. On a 5.1 device a stereo clip put the right channel into the centre, the LFE and both surrounds; on 7.1.4 the right channel came out of eleven speakers at once, so the clip appeared to be playing from behind the listener. Source and device channels are now labelled with the standard WAVE layout for their channel count and routed by speaker position:
  - A stereo clip reaches only the front pair. Centre, LFE, surrounds and heights stay silent.
  - A mono clip goes to both front speakers (not to the centre, which the listener may not have).
  - A surround clip on a narrower device folds down with ITU-style coefficients — centre and surrounds enter their neighbours at -3 dB, sides fall back to the backs (and vice versa), heights drop onto the bed speaker below them, and LFE is dropped when the device has none. Previously this was a modulo average (`L = (c0 + c2 + c4) / 3` for 5.1), which mixed centre, LFE and surround content at equal weight. The fold is not gain-normalized, so heavily loaded surround material clips against the existing output limiter rather than being quietly attenuated.
  - Channel counts with no agreed layout (9, 11, 13+) keep the previous index arithmetic.
  - Muting or soloing a source channel still removes only that channel's contribution; the remaining channels hold their gains instead of being scaled up to compensate.
  - The output callback now interpolates only the source channels the routing actually reads, so a stereo clip on a 7.1.4 device costs two reads per frame instead of twelve.
- **Settings > Audio Output > Direct channel mapping**: opt out of the mapping and send source channel N straight to output channel N. Extra outputs stay silent and source channels past the device's count are dropped. Off by default; persisted as `audio_channel_map` in prefs.

### Fixes
- **Output device follow during playback**: with the output left on `Default`, a change to the OS default device was only picked up while playback was stopped — switching devices mid-playback left audio going to the endpoint the user had just switched away from. The swap now happens immediately and playback resumes at the same position on the new device. An explicitly pinned device is still never overridden, and the switch is still deferred while recording.
- **Dead output stream recovery**: cpal stream errors were printed to stderr and otherwise ignored, so an endpoint unplugged mid-playback produced silence until the next manual device change. The error now flags the engine and the device is reopened on the next frame, honouring the pinned/default preference, instead of waiting on the 1 Hz device poll.
- **Dragging files out of long or network paths**: dragging an item to another application panicked whenever Windows could not express the file's path in the legacy form its shell requires — paths over 260 characters, files on a network share, and names the shell rejects. Such a file is now copied to a short temporary path first, so the drag completes instead of failing (the copy is removed by the existing 10-minute sweep).
- **Crash reports for failures that were already handled**: the native drag path catches its own panics and reports them in the status bar, but the panic hook still wrote a crash report, so a recovered failure looked like a crash. Panics inside a scope that handles them no longer produce a report, and the panic's message is carried into the status line and the debug log instead.

### Diagnostics
- **Crash reports keep source-relative paths**: panic locations inside a dependency were anonymized down to the bare file name, which for a common name like `mod.rs` identified nothing. Paths that carry no user data — under the cargo registry, a git checkout, the Rust standard library, or the project's own `src/` — now keep their crate-relative part (`drag-2.1.1/src/platform_impl/windows/mod.rs`). Everything else, including the user's media paths, is still reduced to a file name.

## 0.20260802.0 - 2026-08-02

### Metadata inspection and scalable sessions
- **Metadata Inspector**: added a dedicated GUI and CLI workflow for inspecting normalized and raw metadata, summarizing fields, searching payloads, hashing or extracting embedded data, and reviewing UCS-backed metadata at scale.
- **Portable session restore**: strengthened relative/absolute path handling, virtual-audio persistence, list-column state, and editor-tab restoration so large sessions reopen more predictably across locations.

### Large audio and editor reliability
- **File-backed audio assets**: introduced stable asset/revision descriptors plus streaming WAV readers and writers, allowing long recordings and edits to stay file-backed instead of requiring every workflow to materialize the full clip in memory.
- **Long-running workflows**: hardened recording, export, clipboard, preview/decode, native drag-and-drop, markers, loop metadata, and effect-graph handoff paths for large or virtual audio.
- **Input focus routing**: centralized keyboard and scroll ownership across list, editor, dialogs, and tool surfaces, with expanded regression coverage for focus-sensitive shortcuts.
- **Editor workflow polish**: improved edge-fade authoring, scalable waveform/editor behavior, list metadata integration, and recovery of in-progress or restored audio state.

### Plugins
- **Plugin catalog and worker sessions**: added a persistent plugin catalog, richer worker protocol/session state, chain metrics, and more reliable native VST3/CLAP probing, processing, timeout, and failure reporting.

## 0.20260729.0 - 2026-07-29

### List QA columns
- **Edge0 / Over0 / Blank**: three optional list columns that answer "is this file safe to ship?". Only problems draw attention — a passing file renders an empty cell, a failing one an `NG` on a red background with the measured values in its tooltip, and an unresolved row `...`. All three are sortable (first click is descending, so the NG rows come to the top) and, like the other full-decode columns, session/CLI aware. Default off.
  - **Edge0**: the first or last frame of any channel is louder than the zero-cross epsilon. The raw edge amplitudes are stored, so changing the epsilon in Settings re-evaluates every row with no re-decode.
  - **Over0**: the sample peak exceeds 0 dBFS. Includes the pending gain so it agrees with the adjacent Peak column, and reports "unresolved" rather than guessing from the header pass's 0.25 s estimate.
  - **Blank**: leading or trailing blank reaches the configured limits. New Settings > Blank Pad column: threshold (default -45 dBFS, deliberately higher than the Sil.Head/Sil.Tail columns' fixed -60) and minimum length (default 10 ms, so a file that correctly starts on a zero sample isn't flagged). Each measurement records the threshold it was taken at, so a settings change is detected per-row instead of by walking every item — which also means a decode already in flight when the setting changed is recognised as stale when it lands. Minimum-length changes are display-side and cost no re-decode.
- **Length column hours**: when any loaded file reaches an hour, every row switches to `h:mm:ss`. Previously the field was total minutes, so a two-hour file read `120:11`. CSV export follows the same rule.

### Editor
- **Channel Routing tool**: a patchbay for rewiring, duplicating and dropping channels — the file's channels on the left, the routed result on the right. Drag from an input pin to an output pin to connect (or click one, then the other); click a cable to cut it; right-click a pin to clear its cables. Output count is settable 1-8 with `Swap L/R` / `Mono -> Stereo` / `-> Mono` / `Identity` presets. Outputs fed by several inputs are averaged, so folding L+R to mono cannot clip; an unconnected output is silence. Grab radii are much larger than the drawn pins (18 px pins, 11 px cables, pins winning ties) with a hover disc showing the active range.
  - This is the only editor tool that changes a tab's channel count, so applying it resets the mute/solo flags and channel view, both of which are sized from that count.

### Fixes
- **Top-bar meter overflow**: the right-hand meter group reserved a hard-coded 440 px for roughly 636 px of content, pushing the `-inf dBFS` readout past the panel's right margin where the window clipped it, and hiding the realtime loudness readout entirely. The reservation is now derived from the parts' own widths, and both readouts are drawn inside their budgets.

## 0.20260726.0 - 2026-07-26

<!-- Shipped under the v0.20260726.0 tag; the section was left titled
     "Unreleased" at the time. -->

### List & Pipeline (P7)
- **Folder watch**: the open folder is polled every ~3 s (low-priority thread, scan filters shared); files added/removed/changed on disk merge into, leave, or refresh the list automatically with a summary toast. Files open in an editor tab are never touched, the app's own writes are suppressed via a 5 s registry, and bulk operations pause polling. Settings toggle, default on.
- **Column reorder + per-project layout**: list columns can be reordered (Settings > List Columns > Column Order, up/down per column); the order is a stable ColumnId permutation driving definitions, headers, and cells from one loop. Sessions (.nwsess) now store per-project column order and widths (serde-defaulted; old files unchanged).
- **Silence columns**: optional Sil.Head / Sil.Tail columns (leading/trailing silence ms at -60 dBFS, full-decode metadata), sortable, session/CLI aware.
- **Offset-tolerant duplicates**: fingerprints are content-aligned (leading silence trimmed and recorded), so silence-padded copies match frame-for-frame; offset matches need +2.5% similarity, groups show the offset, and O(1) duration/centroid gates skip hopeless comparisons. Toggleable per scan (default on).
- **RIFF INFO + iXML batch write**: the BWF dialog also writes INAM/IART/ICMT and PROJECT/SCENE/TAKE/TAPE/NOTE via dependency-free chunk builders (round-trip tested; empty sections leave existing chunks alone).
- **Meta backlog progress**: a "Meta n/m" topbar item appears when more than 200 metadata jobs are queued (visible rows were already prioritized to the queue front).

### Playback & Metering (P6)
- **Realtime LUFS + true peak**: the audio callback feeds a lock-free tap ring; a low-priority thread runs BS.1770 K-weighting (recomputed for the device sample rate and pinned to the ITU 48 kHz table by test), publishes momentary (400 ms) / short-term (3 s) LUFS and 4x-oversampled true peak, shown as a compact "M / S / TP" readout next to the topbar output meter. Readings invalidate ~500 ms after playback stops.
- **Goniometer polish**: the STEREO pane's Lissajous mapping is now a unit-tested pure function (mono collapses to the mid axis, L = -R to the side axis) and the smoothed correlation value is shown numerically beside the pane title.
- **Play Selected Together** (List menu): decode up to 16 selected files, align sample rates, mix at 1/sqrt(n), and play the sum once — a quick layering check without leaving the list.
- **Resampler quality**: the Pitch/Time-Stretch offline pre-stage and lossy-encode paths now use rubato sinc SRC (Good), and the LUFS 48 kHz conversion uses Fast sinc instead of linear interpolation (loudness goldens unchanged). Multichannel LUFS weighting (5.1/7.1 surround x1.41, LFE excluded) confirmed shipped and its stale docs corrected.

### DSP & RX Parity (P5)
- **Noise-shaped dither + 24-bit**: export dither is now a mode (Off / TPDF / TPDF + noise shaping); noise shaping adds per-channel 2nd-order error-feedback (NTF = (1 - z^-1)^2) pushing quantization noise out of the most audible band. A unified Quantizer backs all PCM paths (WAV/AIFF/converter/FLAC two-pass, determinism preserved), and 24-bit exports can opt into dithering. Prefs migrate from the old boolean key.
- **De-clip tool**: detects flat runs pinned at the clipping rails (peak-relative threshold + corner test that rejects smooth low-frequency crests, square-wave rails rejected by run length) and rebuilds the chopped crests with the de-click Hermite bridge — the repair can rise above the rail (float headroom preserved). Scan overlay + async Apply + CLI support.
- **De-hum tool**: cascade of narrow RBJ biquad cuts at the mains fundamental and up to 16 harmonics (STFT rejected: 2048-bin resolution is too coarse for 50/60 Hz). Detect sweeps 45-65 Hz with Goertzel probes; Hz/harmonics/Q/depth adjustable; a selection limits the apply via crossfaded splice. CLI supported.
- **Edit history panel** (Edit > History...): labeled undo/redo entries (operation names from the concrete apply paths), click to jump multiple steps through the existing undo/redo machinery.
- **Region list** (Edit > Regions...): labeled ranges on the editor tab that ride undo and destructive-edit remapping like markers; add-from-selection, inline rename, click-to-select, sidecar (<file>.regions.json) + .nwsess persistence, CSV export.
- **Scrub playback**: Alt+drag on the waveform loops a ±40 ms window under the pointer via the existing loop atomics; release restores the previous loop/transport state exactly.
- **WORLD aperiodicity editing**: per-frame breathiness multiplier draft (Set All / Set Selection / Reset) baked in at Resynthesize, clamped into 0..1 per band; fine 5 ms re-analysis resamples the curve.
- **Spectral region copy/paste**: with a frequency selection in Spec/Log views, Ctrl+C copies band-masked STFT frames and Ctrl+V replaces (Ctrl+Shift+V adds) the band content at the selection start/playhead, snapped to the hop grid, same-sample-rate only.
- **Harmonic action**: Ctrl+click a partial in Spec/Log — f0 refines onto the nearest peak, harmonic bands highlight, and one multi-band STFT pass mutes or attenuates the whole stack over the selection.

### Usability Completion (P4)
- **Non-blocking heavy applies**: pitch/stretch/speed/loudness, de-click/de-noise, spectral warp/brush/heal, and WORLD resynthesis no longer raise the app-wide modal overlay. Only the target tab is gated (in-tab banner); the list, other tabs, and playback of other sources stay interactive. Progress + Cancel live in the topbar activity slot. Tabs are tracked by a stable id, so closing a tab mid-apply discards the result instead of corrupting whichever tab shifted into its index. One apply runs at a time.
- **Rebindable shortcuts**: Help > Customize Shortcuts... lets table-dispatched chords be reassigned by clicking a row and pressing the new chord (conflicts across overlapping contexts refused, per-row Reset / Reset All, persisted as `keymap=` prefs lines). The read-only shortcut list shows the effective (overridden) chords.
- **Tool icon toolbar**: the editor's 22-item Tool ComboBox is now a grouped icon toolbar (hover for names, wraps in narrow panels) with the active tool highlighted; selection semantics (preview discard, gesture reset) unchanged.
- **Editor zoom/nav keys**: `+`/`=` zoom in, `-` zooms out around the playhead; `[`/`]` page the view by one visible width.
- **Wheel behavior option**: Settings > "Wheel scrolls the view (Ctrl+wheel zooms)" turns a plain vertical wheel into horizontal view scrolling (Ctrl+wheel / pinch still zooms). Default stays zoom-on-wheel.
- **Edit menu** (File | Edit | Export) with Undo/Redo wired to the same dispatch as `Ctrl+Z`/`Ctrl+Y`, enabled from the editor/list/effect-graph undo stacks.
- **List context menu**: Open in Editor and Reveal in Folder at the top; Select All / Clear Selection at the bottom. Right-clicking inside a multi-selection keeps it.
- **Empty-state onboarding**: with no folder and no items, the list shows a centered panel with Open Folder... and up to five recent sessions.
- **Polarity invert boundary smoothing** (option, default off): ~2 ms polarity crossfade at interior range boundaries so partial inverts don't click; edge-touching ranges and the default path stay bit-exact.

### Pipeline & QA (Stage B / P3)
- **Naming-rule check** in batch inspection (GUI dialog + CLI `--naming-pattern`): file stems failing the regex get warnings; an invalid pattern reports a config error on every row. Pattern persists to prefs.
- **Find Duplicates** (List menu): worker-pool fingerprinting (gain-invariant spectral-shape hashes + exact content hash) clusters exact duplicates and perceptually similar files into a results window with click-to-select and CSV export.
- **Export Engine Metadata** (List menu + CLI `batch engine-export`): Unity JSON / FMOD JSON / Wwise TSV metadata tables (loops, sample rate, channels, length, LUFS) for the selection or list — no audio conversion.
- **Edit BWF Metadata** (List menu): batch-write the bext chunk (description/originator/reference, auto-stamped date/time) into selected WAVs, preserving all other chunks; non-WAV files are skipped and counted. iXML remains out of scope.
- **WORLD formant editing**: a Formant slider (0.5x-2.0x) in the World view warps the spectral envelope along frequency at resynthesis — formant shifts without pitch changes, applied in both the display-grid and fine 5 ms re-analysis paths.
- **Light theme pass**: hand-painted widgets (list selection/markers, dirty/error accents, volume slider, output meter) now draw through a theme-aware palette; the editor's audio canvas intentionally stays dark (DAW-style) in both themes.

### Spectral Repair & Restoration (Stage A)
- **Spectral Brush** (Spec/Log views, next to Spectral Warp): drag on the spectrogram to paint content out RX-eraser-style. Stamps attenuate magnitude with Gaussian falloff in time and frequency (Strength 3-80 dB, Radius ms/Hz baked per stamp), stack additively in dB (clamped at 80 dB), render a preview on release, and Apply through the async pipeline with undo. Only the influenced region is processed; audio outside the stroke stays bit-identical.
- **Heal Selection** (beside the spectral Mute button): rebuilds the selected time range (optionally band-limited by a frequency selection) from the surrounding audio — per-bin magnitudes interpolate across the gap between the context averages and phase advances at the measured per-bin velocity, so steady tones bridge dropouts coherently. Selections over 120 s are refused with a toast.
- **De-click tool**: second-difference residual detection with per-window MAD-adaptive threshold (sensitivity slider), Hermite-bridge repair. Scan marks the detected spans in red on the waveform (invalidated by any edit or sensitivity change); Apply repairs whole file or selection with undo. Also available via CLI `apply`.
- **De-noise tool**: learn a per-channel noise profile from a noise-only selection, then reduce it via power spectral subtraction (Reduction = max attenuation floor, Strength = over-subtraction) with asymmetric gain smoothing against musical noise. Preview/Apply through the shared worker pipelines; selection-scoped applies crossfade their edges.
- Shared STFT engine refactor: `stft_process_frames` (reflect-padded Hann WOLA, 2048/512) now backs the band gain, brush, heal, and noise-profile paths.

### Waveform Editing Completion (Stage A)
- **Mix paste** (`Ctrl+Shift+V`) sums the clipboard into the buffer without changing length; **crossfade-insert paste** (`Ctrl+Alt+V`) splices the clip in with equal-power joins at both seams.
- **Pencil tool**: at high zoom (> 2 px per sample) drag on the waveform to draw sample values directly (linear interpolation between drag points, lane-targeted, one undo step per stroke).
- **Channel-scoped edits**: with a Custom channel view active, gain / normalize / fade / mute / noise gate / EQ / compressor / DC removal / polarity invert apply only to the visible channels (normalize measures its peak within them). Light previews follow the same mask and the inspector shows "Applies to: ch N". File-level list gain deliberately ignores the mask.
- `EditorTab` construction deduplicated into `EditorTab::new_base` (one place to default new fields).

### Plugins (Stage A)
- **Presets & A/B**: save/load/delete named parameter presets (JSON per plugin under the NeoWaves config dir, state blob included) from both the effect-graph plugin node and the editor's Plugin FX tool; an A/B slot stores a second parameter set and swaps on demand.
- **Plugin Manager window** (Tools menu): catalog overview, rescan with status/error display, and search-path management persisted to prefs.
- **Auto preview**: a Plugin FX toggle re-renders the preview ~300 ms after any parameter change (sliders, Enable/Bypass, presets, A/B, native-GUI edits) with a position-preserving buffer swap, so tweaking parameters feels continuous.

### List (Stage A)
- **Multi-variation audition**: with 2+ rows selected, List > Audition Selection plays them in round-robin or random order (never the same file twice in a row), advancing on each natural playback end. Stop playback, select another row, or press Cancel on the topbar "Audition n/m" item to end it.

### Batch QA (P2 batch)
- **Inspect Files (QA)**: batch inspection over the selection or the whole list — effective true-peak ceiling, integrated-loudness window, leading/trailing silence thresholds, and loop-marker validity (bounds checking that the readers never did). Runs on up to four low-priority worker threads with topbar progress + cancel; results open in a severity-filtered window (click a row to select the file, Save CSV...). Same checks are exposed as `--cli batch inspect` with json/csv/md/txt reports.
- **Normalize Loudness (GUI)**: batch loudness normalize to a target LUFS (default -14) for the selection or whole list. Measures via the async metadata pool, then routes each file's gain delta through the unified gain framework — pending list gain (one undo action for the whole batch) or a destructive edit for files open in editor tabs. Non-destructive: no audio files are written; clip-risk files are counted and reported in the completion toast.

### Waveform Editing Basics (P2 batch)
- New editor tools: **Invert Polarity** (flip sample polarity over the selection or whole file) and **DC Offset** removal (per-channel mean subtraction with a live measured-DC readout), both with preview, undo, session restore, and CLI apply support.
- **Insert Silence** tool inserts N ms of zeros at the selection start (or the playhead); markers, loop regions, selections, and fade ranges after the insert point shift right. Built on a shared insert infrastructure (`editor_insert_channels_at`).
- **In-editor audio cut/copy/paste-insert**: Ctrl+C/X/V in the editor workspace operate on an in-app audio clipboard. Paste splices at the selection start / playhead with undo; cross-tab pastes are resampled to the target buffer rate and channel-adapted.
- **TPDF dither** (default on, Settings toggle) when quantizing to 16-bit integer PCM in the WAV/AIFF/FLAC/gain-export writers. Deterministic generator keeps FLAC's two-pass MD5 self-consistent.

### Usability (P1 batch)
- New Help menu with a read-only Keyboard Shortcuts window, generated from a central keymap table (`src/app/keymap.rs`); simple shortcut dispatch now goes through the table so a future rebinding UI only needs to swap the lookup.
- Destructive editor keys `C` (delete+join) and `T` (trim) show an info toast pointing at Ctrl+Z after they fire.
- Editor: `Home`/`End` seek to start/end, `Z` zooms to the selection, `Esc` discards a pending tool preview.
- Editor: per-channel playback mute/solo (M/S menu next to the channel view toggles). Monitoring only - the masks resolve to channel selection inside the callback's fold-down mapping, are excluded from undo/dirty/save, and never apply to list playback.
- Topbar output meter shows per-output-channel RMS bars with peak-hold ticks while the callback reports multichannel levels (falls back to the old single bar otherwise).
- List: optional "Single click auditions" setting (default on = current behavior). When off, a single click only selects; Space, keyboard navigation, and Auto Play still audition. Double-click still opens the editor.
- List: inline rename via `F2` (or the context menu) with Enter to commit and Esc to cancel; errors surface as toasts. The modal rename stays for batch use.
- List: column widths persist across sessions (saved when a resize drag ends; window-squeeze relayouts are never saved). Column reorder and per-project widths remain out of scope.

### Data Safety
- Windows file overwrite now uses `ReplaceFileW` (atomic swap), removing the crash window where the destination could be left missing during the park-and-rename fallback (which remains as last resort).
- Gain / Normalize / Loudness applies no longer hard-clip the editing buffer to +/-1.0; editing buffers keep full float headroom (boost then cut round-trips losslessly). Clipping only happens at export/quantize and playback output. An info toast reports when an edit leaves peaks above 0 dBFS.
- Closing the window with unsaved in-memory edits (dirty tabs, cached edits, pending gains) now asks for confirmation instead of silently discarding them; Ctrl+W on a dirty tab routes through the Leave Editor prompt. Screenshot/debug automation exits bypass the prompt.

### Notifications
- New toast overlay (below the topbar, click to dismiss, auto-expiring) surfaces failures that previously only reached the debug log or stderr: session save/save-as/close errors, export failures, editor tab-limit skips, and resampler quality fallbacks.

### Playback
- Sources with more channels than the output device are folded down (each output channel averages the source channels congruent to it) instead of dropping the surplus channels.
- Tool previews (Fade / Gain / Normalize / Loudness / Reverse / NoiseGate / EQ / Compressor / LoopEdit unwrap / MusicAnalyze) now play the per-channel buffer instead of a mono mixdown, preserving stereo imaging. Normalize previews measure peak across all channels, matching the destructive apply.

### Correctness
- Loop edits via the K/P shortcuts now push editor undo states (matching L / Inspector loop applies).
- Digit-key seek fixed: both `0` and `1` used to jump to the end. Keys `1..9,0` now span start (0%) to end (100%) in keyboard row order.
- 16-bit PCM encode/decode uses symmetric 32768 scaling (standard convention; -1.0 maps to -32768). The generic integer writer quantizes symmetrically for all depths.
- Spectral/feature lanes (Spec/Log/Mel/Tempogram/Chromagram/World) no longer drift up to one STFT hop against the waveform lane at high zoom (fractional per-column frame mapping).
- Meta pool and VST3 state-stream mutexes recover from poisoning instead of cascading panics; removed per-event wheel debug prints.

## 0.20260709.0 - 2026-07-09

### Unified Gain Framework: List Volume Changes Are Editor Edits
- Per-file volume changes made in the list (gain column DragValue, Left/Right arrow keys) and the Editor's Gain tool now live in one edit framework. When a file has an open, fully loaded editor tab, a list gain change is applied as a destructive editor edit: the waveform updates, the tab goes dirty, and Ctrl+Z in the editor undoes it - exactly like using the Gain tool. Files without an open tab keep the fast pending-gain path (essential for very large lists), unchanged.
- Opening an editor tab for a file that has a pending list gain now bakes that gain into the tab's buffer as a regular editor edit (with undo) the moment decoding finishes, so the editor's waveform finally shows what you will hear and export. The pending value is cleared at that point - playback, save, and export apply the gain exactly once, through the edited samples.

### Graphical EQ / Compressor / Noise Gate
- The EQ, Compressor, and Noise Gate tools (Editor Inspector) and their Effect Graph nodes now lead with interactive plots instead of only numeric fields (the DragValues/sliders stay for exact entry):
  - EQ: log-frequency response curve (20 Hz - 20 kHz, +/-24 dB) computed from the actual RBJ biquad chain, with three draggable band handles (orange low shelf, green mid, purple high shelf) - horizontal drag sets frequency, vertical sets gain, scrolling over the mid handle adjusts Q.
  - Compressor: static transfer curve (input dB -> output dB with a unity reference diagonal); drag the orange knee horizontally to set the threshold and the green top endpoint vertically to set the ratio.
  - Noise Gate: gate transfer curve with the closed region shaded; drag the handle to move the threshold.

### Effect Graph: Band Split / Band Join and MS Split / MS Join
- Band Split (Routing) splits audio into low / mid / high bands at two adjustable crossovers (log sliders, defaults 200 Hz / 2 kHz). The split is complementary around zero-phase Butterworth low-passes (filtfilt), so the three bands sum back to the input bit-for-bit: Band Split wired straight into Band Join returns the original audio. Each band keeps the input's full channel layout, so per-band processing (e.g. compress only the lows) preserves stereo.
- Band Join sums whatever bands are connected back into one bus (unconnected bands are simply absent).
- MS Split encodes stereo into mid (L+R)/2 and side (L-R)/2 buses; mono passes through as mid with a silent side, and inputs wider than stereo use the first two channels (with a runtime warning). MS Join decodes mid + side back to L/R - straight from MS Split it reconstructs the original stereo exactly, and with only mid connected it produces a mono-in-stereo signal, enabling classic MS tricks (widen/narrow, mid-only EQ) as graph routing.

### Spectrogram: Image-Like Spectral Warp (Spec / Log views)
- New "Spectral Warp" section in the Inspector for the linear and log spectrogram views (the views that resynthesize back to a waveform; Mel stays view-only). Enable "Edit warp points on spectrogram" and drag directly on the spectrogram to push frequency content up or down, liquify-style: each stroke becomes an arrow (origin ring -> target dot) with Gaussian falloff in time and frequency, controlled by the Radius (ms / Hz) fields. Grab an arrow to re-adjust it; double-click or right-click removes it.
- Processing runs in the STFT domain (2048/75% Hann WOLA, same engine as the RX-style spectral mute): a backward frequency remap per analysis frame with complex-bin interpolation and per-bin cumulative phase rotation (phase-vocoder style) so shifted partials stay coherent; only the influenced time region is processed and its edges crossfade against the original. Releasing a drag renders the warp on a worker thread and auditions it immediately (green waveform overlay with "Waveform overlay" enabled); Apply bakes it destructively with full undo and re-analyzes the spectrogram.

### Editor Inspector: Gain Curve, Speed Tool, and Selection-Aware Pitch/Stretch/Reverse
- The Gain tool can now apply a DAW-automation-style gain curve instead of only a uniform value: enable "Gain curve (draw on waveform)" and click the orange polyline on the waveform to add breakpoints, drag them to shape the curve (piecewise-linear in dB, +/-24 dB), double-click or right-click a point to remove it. The curve previews live (green overlay + audition) and Apply bakes it destructively with full undo. Long clips preview the curve by scaling the overview bins.
- New Speed tool (Inspector, between Time Stretch and LoudNorm): tape-style playback-rate change (0.25x-4x) that shifts pitch and length together, using the existing offline resampler. Same preview/apply flow as Time Stretch, including background preview for long clips and session persistence of the rate.
- PitchShift, TimeStretch, and Speed now apply to the current selection when one exists (whole file otherwise). The selection is processed on its own and spliced back with short equal-power crossfades at both joins, so the audio connects cleanly even when the segment shrinks or grows; preview renders the exact same splice you get on Apply.
- Canvas gestures for the preview workflow: with PitchShift active, drag the horizontal pitch line up/down over the waveform (up = higher, +/-12 st, live semitone readout) and release to render the preview. With Speed/TimeStretch active and a selection, grab the selection's right edge and drag left/right to shrink/stretch it - a ghost region and "x1.25 (slower/longer)" readout track the drag, and releasing the mouse renders the stretched waveform and audition.
- Reverse is selection-aware: with a range selected, Preview/Apply reverse only that range, blending a few milliseconds at each join so the reversed span connects without clicks; without a selection it reverses the whole file as before.

## 0.20260708.0 - 2026-07-08

### Forge-Style Processing Chain: Noise Gate / EQ / Compressor / Trim / Bit Depth / Resampler
- Six new nodes in the Effect Graph - Noise Gate, EQ, Compressor, Trim, Bit Depth, and Resampler - plus matching Noise Gate/EQ/Compressor tools in the Editor's Inspector panel, so the same "Forge-style" mastering chain (gate -> EQ -> compress -> trim silence) can be built either as a reusable node graph or applied directly to a tab. Noise Gate and Compressor are envelope-follower designs (threshold/attack/release, plus ratio/makeup for the compressor); EQ is a fixed low-shelf/mid-bell/high-shelf topology (RBJ biquads in series) rather than a freeform band count. Trim reuses the existing Auto Trim detector to remove leading/trailing silence only (internal quiet gaps are left alone). Bit Depth previews 16/24-bit quantization in-buffer (true kbps bitrate remains an export-only concept, since it only applies to a lossy codec, not floating-point audio in the graph); Resampler exposes target rate and Fast/Good/Best quality via the existing rubato-based resampler. All three level/dynamics tools share one DSP implementation (`wave.rs`) between the graph node and the Inspector tool, with live preview/audition and full undo on Apply.

### PluginFX Reliability and a Shared Probe-Status UI
- Native VST3/CLAP parameter probing now retries up to 3 times before falling back to the zero-parameter generic backend, fixing the most common cause of "the plugin's parameters show up sometimes and not other times" - native probing launches the plugin in a separate process and is inherently racy (module load / COM init / plugin init timing), and a single transient failure used to permanently downgrade that probe to Generic.
- Added a "Load from file..." picker to both the Effect Graph's Plugin FX node and the Editor's Plugin FX tool, so an empty (never-scanned) plugin catalog is no longer a dead end - picking a `.vst3`/`.clap` directly adds it to the catalog and probes it immediately.
- The two Plugin FX UIs (graph node and Editor tool) now share one `ui_plugin_probe_status` widget for the error / generic-fallback-warning / backend-log display, so a probe failure reads identically in both places.

### Clipboard/Export Consistency, Clear Edit, and Loop Edit / Inspector Polish
- Fixed clipboard copy (Ctrl+C) silently using a file's original bytes when it only had a pending list-level gain change (no open Editor tab) - drag-export already applied that gain correctly, and copy now shares the same `apply_gain_and_resample` logic instead of a narrower path, so Copy, drag-out, and Export always agree on what "the current version of this file" means.
- Added a "Clear Edit" button next to Undo/Redo in the Editor: reverts a tab's audio to the original file on disk and wipes its undo/redo history in one step (selection, markers, and loop points are left untouched).
- Recent Sessions now remembers the last 10 sessions instead of 3.
- Loop Edit panel: Auto Detect's candidate list is capped to the top 3 (already score-ranked) results instead of every candidate found, and the whole Auto Detect section moved below Seam Check, since it was the least reliable, most-scrolled part of the panel. The "Loop Range" status rows no longer render as their own sub-section header.
- The Inspector panel no longer reserves a tall empty box under short tool content (e.g. Loop Edit with only a couple of Auto Detect candidates) - it now sizes to its actual content.
- The ambiguous single "Edge fade" control in the spectral selection tools (which silently mixed a time-domain and a frequency-domain parameter under one label) is now two clearly labeled "Time fade" / "Freq fade" rows under a "Spectral Mute Fade" heading.
- Investigated (root cause identified, not yet fixed) an occasional waveform/spectrogram visual misalignment at high zoom: the spectral viewport renderer snaps its sample-range bounds down to the nearest analysis-frame boundary via integer division, while the waveform lane renders the exact requested sample range - a real but separate bug from this release's fixes.

### Hitch-Free Loading (no stalls during or right after big loads)
- Loading a 1M-file folder no longer produces multi-hundred-ms frame stalls mid-scan. The path->id index is now keyed by a precomputed 64-bit hash (`types::PathIndex`): growing a plain `HashMap<PathBuf, _>` re-hashes every key, which cost ~270ms in one frame at 640k entries; growing the u64-keyed table only moves slots (the worst load-time frame at 1M drops from ~650ms to ~64ms). Hash collisions degrade a slot to a tiny vector, never to a wrong answer. The remaining per-item maps (id index, folder intern, inflight set, stat cache, SR probe cache) switch to FxHash.
- The list containers pre-reserve toward the scanner's live discovery count (shared via an atomic, not the message channel, so it runs ahead of the budgeted appends).
- Loading a new folder over an existing large list no longer freezes while ~1GB of old items drop: the old containers are handed to a low-priority thread.
- Finishing a scan with no active search no longer re-collects the whole id list (files/original_files are already maintained incrementally during the scan).
- The async sort's snapshot (1M keys + names) is now returned to the UI thread and freed a slice per frame; freeing it wholesale on the sort worker contended with the UI thread inside the allocator and showed up as a ~200-260ms frame right when a background sort finished (now worst ~15ms).
- `NEOWAVES_BENCH_TRACE=1` enables coarse per-stage frame tracing (scan ingest, list jobs, reserve, workspace pass) used to find these; it is compiled in but env-gated.

### 1M-File Responsiveness Pass (priority scheduling, async sort/filter, windowed list)
- Background workers no longer compete with the UI thread for CPU. `lower_current_thread_priority` now works on Linux (per-thread nice) and macOS (utility QoS) in addition to Windows, and is applied to the workers that previously ran at normal priority: the metadata decode pool (also capped at cores-1), list-preview prefetch, LUFS recalc, exports, auto-trim, loop detect, and the folder scan walker. This was the root cause of "buttons stop responding while background work runs".
- Sorting and search filtering never block the UI thread on large lists anymore: the sort snapshot is built in 2 ms slices per frame and the O(n log n) sort runs on a worker thread (results are dropped if the list changed meanwhile); the search filter runs as a sliced per-frame job. Lists <= 50k rows keep the synchronous path.
- The metadata pool queue was rebuilt as a per-path task map with high/low priority lanes: enqueue / promote / dedupe / cancel are all O(1) (promoting a visible row used to scan the whole queue under the mutex every frame), and tasks are now cancellable (list removals and renames cancel their pending decodes; running tasks stop at the header/decode stage boundary).
- Repaint policy: progress-only states (scanning, exports, CSV, AI analysis, sort/filter jobs) repaint at 50 ms instead of forcing 60 fps; metadata streaming repaints at 15 fps; the per-frame metadata drain is time-capped (~1 ms).
- The list is now rendered as a row-index window with an app-managed scrollbar instead of one giant egui scroll area. egui stores scroll offsets as f32, which quantizes above ~16.7M px of content - at 1M rows (48 px cover-art rows = 48M px) scrolling and scroll-to-row broke down. The window start row is a usize, the custom scrollbar maps in f64, and only the visible rows are ever handed to the table, so precision is exact at any list size. Wheel scrolling snaps to whole rows.
- MediaItem slimmed for 1M-file lists (~40% smaller resident footprint): FileMeta is boxed (rows without metadata no longer pay ~200 inline bytes), the three per-item lowercased search-cache strings are gone (the filter lowercases on the fly inside its budgeted slices), external CSV/Excel values are Option<Box<...>>, and folder display names are interned per directory (Arc<str>).
- select_and_load uses the TTL-cached file-exists check instead of a blocking stat() per click/keypress.

### New Loudness Metrics: dBTP / LUFS-S / LUFS-M (+ BS.1770 audit)
- Three new default-hidden list columns - "dBTP" (true peak), "LUFS-S" (max short-term, 3 s), "LUFS-M" (max momentary, 400 ms) - with sorting, CSV export, session persistence and column-picker support. Values are computed in the same full-decode metadata pass as LUFS (I) and shift with pending gain like the existing LUFS/peak columns.
- True peak follows BS.1770-4 Annex 2: polyphase windowed-sinc oversampling (4x below 96 kHz, 2x below 192 kHz) on the original-rate channels; momentary/short-term follow EBU Tech 3341 (ungated maxima).
- Audited the existing LUFS implementation against BS.1770-4: the 48 kHz K-weighting coefficients, 400 ms / 75% overlap blocks, and the -70 LUFS absolute + -10 LU relative gates are spec-correct. Two deviations found: (1) surround channel weighting was missing - now fixed for assumed 5.1/7.1 film layouts (LFE excluded, surrounds x1.41 power weight); (2) the internal 48 kHz conversion uses linear interpolation (~0.1 LU worst case vs a sinc resampler) - kept for speed and documented in code.
- New unit tests: EBU Tech 3341 reference tones (-23 / -33 LUFS at 48k and 44.1k), gating vs silence, burst momentary > short-term, inter-sample true-peak recovery (fs/4 sine at 45 deg phase reads ~0 dBTP from -3.01 dBFS samples), and 5.1 surround weighting (+1.49 dB, LFE gated out).

### 500k-File List Scalability Pass
- Fixed the app effectively locking up after loading very large libraries (reported with 500k FLAC files) once a sort column was involved:
  - Clicking a metadata sort header (Length / SR / Bits / LUFS...) used to enqueue one decode job per row up front - at 500k rows that meant half a million full-file decodes queued in one frame, days of background CPU, and a UI-thread promote scan over the giant queue every frame. Sort prefetch now streams through the existing per-frame pump under its queue budget and inflight cap.
  - Sort keys that are fully answerable from the file header (duration, channels, sample rate, bits, bitrate, BPM tag, created/modified) no longer trigger full-file decodes at all during sort prefetch; only dBFS (Peak) and LUFS sorts still decode, since they need sample data. Files whose header cannot resolve a duration still fall back to one decode pass under a Length sort.
  - The one-shot list sort is ~3x faster at 500k rows (unstable sort with no merge buffer; the equal-key tie-break is now display name then MediaId instead of display name then component-wise `Path::cmp` - numeric sorts like SR tie constantly, so the tie-break dominated the whole sort). A 500k-row File-header click drops ~1.7 s -> ~0.55 s in release. Rows with identical primary key and name now order by scan order rather than full path; the order stays deterministic.
  - Fixed a hidden double sort: the first metadata batch arriving right after a header click passed the "never sorted yet" debounce check and re-sorted the entire list again in the same frame (another ~1.3 s at 500k). Any explicit sort now stamps the debounce clock. Re-sorts while metadata streams in also scale their debounce with the measured cost of the previous sort (8x, capped at 8 s), so the UI thread can never spend the majority of its time re-sorting.
- Bounded the visible-row metadata decode backlog on large lists (>= 8k files): fast scrolling used to enqueue an unbounded pile of full decodes (one per row that ever became visible). New tasks are rejected past a cap and visible rows self-heal by re-requesting, so the queue - and the per-frame promote scan over it - stays small.
- The idle sort-prefetch walk is capped per frame (8192 rows, wrapping cursor) so a fully-resolved 500k list no longer pays an O(n) scan every frame while a metadata sort is active.
- Select-all + Enter on a huge list no longer funnels every selected path through the tab-open path (and its per-path skip log) once the editor tab limit is reached.
- CSV export now streams its metadata jobs to the worker pool frame by frame instead of mass-enqueueing every row up front (new regression test). This keeps huge exports compatible with the backlog cap - and fixes a pre-existing stall where a large-list export with a dBFS/LUFS background mode active could drop most of its decode jobs at the old cap and never finish.
- Added an opt-in headless benchmark (`tests/large_list_bench.rs`) that loads a 500k-file fixture and reports scan/append frame times, steady-state frame cost, sort latency, and RSS.

## 0.20260706.0 - 2026-07-06

### Fix: List Randomly Turning Red (dev builds)
- Fixed the file list sometimes getting 2px red outlines around every cell in debug builds. egui keeps separate dark/light styles and follows the OS theme by default; the startup style patch (app text sizes + disabling the `warn_if_rect_changes_id` debug heuristic that false-positives on the virtualized list) only landed in the style slot active at startup. When Windows later reported the other theme, egui swapped in the unpatched style - the app still looked dark (visuals were re-applied every frame) but the debug heuristic came back on and painted red outlines after scroll jumps. Styles are now patched via `all_styles_mut` (both slots), theme visuals likewise, and a kittest regression simulates the OS theme flip and asserts no red debug rects are painted.

### RX-Style Time-Frequency Selection: Spectral Mute + Play Selection
- The spectral views (Spec / Freq Log / Mel) now support time-frequency rectangle selection: dragging selects both the time range and a frequency band (drawn as a band-limited highlight per channel lane), like iZotope RX / Adobe Audition's marquee. Dragging edge-to-edge across the whole frequency axis - or dragging in the Wave view - keeps the classic full-band time selection. The Y->Hz mapping follows the active view's axis exactly (linear / log / mel, including vertical zoom and the display max-frequency cap), and the band survives undo/redo.
- New "Freq" row in the Inspector shows the selected band with editable low/high Hz fields and a "Full band" reset; it appears in the spectral views (and anywhere a band is set).
- "Mute Selection (Spectral)": destructively mutes only the selected band inside the selected time range. The band is removed with an STFT band-stop (Hann, 75% overlap, weighted overlap-add resynthesis) with raised-cosine transition bands at the frequency edges, and the filtered result is crossfaded against the original with raised-cosine time fades just inside the selection - no clicks, no brick-wall ringing (the same edge-smoothing approach RX and Audition use). Without a band it is a click-free full-band mute. Fully undoable; edge fade lengths (ms / Hz) are adjustable in the Inspector.
- "Play Selection" (Wave view too): plays only the selected time range, band-passed to the selected frequency band when one is set (RX-style selection audition). Follows the offline-render playback principle (never filters in the audio callback), auto-stops at the selection end, loops the selection while loop mode is on, and restores the tab's real audio when playback stops. Band-pass/band-stop DSP is covered by unit tests (band isolation, STFT round-trip transparency, click-free edge ramps).

### Harvest F0 Estimator (switchable) + Resynthesis Quality Audit
- New "F0 estimator" setting in the World inspector: `DIO (fast)` (default, unchanged) or `Harvest (accurate)` - a full pure-Rust port of WORLD's Harvest (filter-bank candidate detection on a 1 ms grid, instantaneous-frequency refinement, unreliable-candidate removal, contour fixing, zero-lag Butterworth smoothing). Harvest replaces DIO+StoneMask before CheapTrick/D4C and also drives resynthesis re-analysis. Cross-validated against pyworld 0.3.5: 100% voiced/unvoiced agreement and 0.06-cent median F0 difference on a vibrato test tone. The refinement stage (the heavy part) fans out across worker threads, and progress reporting stays live. Persisted in prefs; switching estimators drops cached World analyses so views re-analyze.
- Audited F0-edit -> resynthesize quality against the reference implementation: no beyond-spec defects found. Sample rate is guarded end-to-end (spawn-time mismatch check; pitch roundtrips exactly at 44.1 kHz and 48 kHz), and long-clip smearing was already fixed by the 5 ms fine re-analysis. The one surprise - pure sine tones come back ~+4 dB hot - reproduces bit-for-bit in the reference vocoder (pyworld measures the same +3.98 dB; harmonic-rich material roundtrips at ~+0.2 dB), so it is inherent CheapTrick envelope behavior, not a port bug. New regression tests pin all three facts (harmonic roundtrip within 1.5 dB, sine matches the reference gain, flat-envelope synthesis calibration).

### Background Session Save / Clipboard Copy + Cheaper Undo & Edits
- Session save no longer freezes the UI: the document and Arc snapshots of every edited/virtual audio buffer are gathered instantly, then all sidecar WAV encodes, TOML serialization, and file writes run on a worker while the busy overlay shows progress ("Saving session... (N audio sidecars)"). Close-with-autosave, CLI, and tests keep a synchronous variant so completion stays observable where it matters.
- Copying items to the clipboard is backgrounded the same way: decoding file-backed items and exporting edited audio to temp WAVs happen on a worker; the OS clipboard and in-app payload are set on completion. Large multi-selections no longer lock the app for seconds.
- Undo snapshots are Arc-shared with the tab's worker mirror: capturing an undo point before an edit is now copy-free (was a full multi-MB buffer clone per edit), and undo/redo drop from three full-buffer copies to one plus the engine hand-off.
- In-place destructive edits (trim / fade / gain / delete / reverse...) defer the waveform overview + pyramid rebuild to a background worker with generation guarding; the edit itself lands immediately and the refreshed overview swaps in when ready instead of stalling the frame for the rebuild.

### List & Apply-Path Performance Pass (large libraries, long clips)
- Removed two full item-array scans that ran every frame (pending-gain count in the topbar and the list-header dirty check): both now read a 250 ms-throttled cached count that gain edits invalidate immediately. At ~140k files these two scans strided tens of MB of item structs per frame - the main reason the list felt heavy while background jobs forced 60 fps repaints. Idle frame time on a 140k-row list drops ~4.1 ms -> ~1.9 ms avg.
- Meta-driven re-sorts are debounced adaptively: lists over 20k items re-sort at most every 750 ms while metadata streams in (was 120 ms; each pass is an O(n log n) decorate+sort costing tens of ms at 140k). Transcript-triggered search refilters follow the same adaptive debounce.
- List rows no longer deep-clone the whole MediaItem (strings, external map, inline FileMeta with thumbnail) for every visible row every frame; rows now borrow the item once and keep only the cheap pieces (badge, cover-art Arc, transcript Arc).
- Pitch/stretch/loudness applies and WORLD resynthesis now build the waveform overview + pyramid and the worker-facing Arc mirror on the worker thread; adopting a finished apply no longer re-scans and re-clones the full buffers on the UI thread (~35-80 ms saved per apply on a 3-minute stereo clip).
- Rate-mode processing results (Speed/Pitch/Stretch previews) also prebuild the editor waveform cache on the worker, and two wasteful full-buffer clones in the completion handler were removed (the engine now takes the processed buffers by move).

### Spectrogram Display Fixes: Stale Partial Render + Resolution
- Fixed the spectrogram (and Freq Log / Mel) showing a partially-filled image with a black tail when a view is opened while analysis tiles are still streaming in - the first render stuck around until a zoom/pan happened to change the render key. Tile arrival and completion now retire the cached viewport image, so the heatmap fills in progressively and always ends complete.
- Feature-view render resolution unlocked: the fine pass now renders at native pixel size (up to 2048x1024; previously hard-capped at 384x192 and stretched), so Spec/Freq Log/Mel/Tempogram/Chromagram/World are sharp on large canvases. The coarse preview pass got a matching bump.

### WORLD Responsiveness / Undo Correctness Pass
- Fixed Ctrl+Z after destructive edits (including WORLD resynthesis): undo/redo now refreshes the worker-facing buffer mirror and drops stale spectrogram/feature analyses, so the World view (and Spec/Tempo/Chroma) re-analyze the audio that is actually restored instead of showing the pre-undo analysis.
- WORLD analysis now reports live progress (DIO -> StoneMask -> CheapTrick -> D4C weighted 0-100%): the inspector progress bar animates, the canvas overlay shows a percentage, and the frame loop keeps ticking during analysis so feedback never freezes.
- Removed every UI-thread stall in the World pipeline: analysis mixdown moved onto the worker thread (applies to Tempogram/Chromagram too), viewport render requests share the cached analysis via Arc instead of deep-cloning tens of MB per pan/zoom, and the envelope maximum is precomputed at analysis time instead of rescanned on every render.
- F0 curve drawing is decimated to ~2 points per pixel (window-aware so unvoiced gaps still break the line), keeping long clips smooth while editing.
- Dev builds (`cargo run`) now compile at opt-level 1 with hot DSP crates at full optimization - the WORLD/FFT paths were 10-20x slower unoptimized, which made debug builds feel hung; lib test wall time dropped from ~20 s to ~2 s as a side effect.

### F0 Editing + WORLD Resynthesis
- The World view is now an editor: enable "Edit F0 on canvas" and draw the pitch curve with the mouse (left-drag draws, right-drag erases to unvoiced; strokes interpolate in log-frequency so fast drags leave no gaps). Canvas seek/select pause while editing.
- Curve transforms in the inspector: semitone shift (drag value + apply), 5-frame median smooth, flatten-to-median (monotone), and reset to the analyzed curve. The edited draft renders in orange over the dimmed analyzed curve.
- "Resynthesize (replace audio)" rebuilds the tab audio with WORLD synthesis using the edited F0 - ported D4C aperiodicity analysis and the reference synthesis engine (pulse/noise excitation through minimum-phase spectra, fractional pulse alignment, deterministic noise) join the analysis port in `render/world_features.rs`. Runs as a background job through the shared editor-apply pipeline: full undo (Ctrl+Z), busy overlay with cancel, engine buffer swap, and cache invalidation; the mono result is written to every channel so the tab keeps its channel count. Roundtrip unit tests confirm pitch is preserved and that editing the contour actually shifts the resynthesized pitch (1 s of 48 kHz synthesizes in ~30 ms release).
- F0 readability: pitch curves now draw over a dark halo so they stay visible on bright envelope areas, and a new "F0 zoom" toggle switches the vertical axis to 50 Hz-1.1 kHz so the pitch range fills the canvas (heatmap, ticks, and pencil mapping all follow).

### Spectrogram dB Reference Option
- New "Spectrogram Values" setting: `dB (0 dBFS ref)` (previous behavior) or `dB (normalized to max)` - librosa-style `ref=max` mapping where the loudest bin tops the color ramp, keeping harmonic detail visible on quiet material. Persisted in prefs and sessions; applies to Spec/Freq Log/Mel views.

### New WORLD Feature View (F0 / Spectral Envelope)
- New editor view "World (F0/Env)" alongside Tempogram/Chromagram: a CheapTrick spectral-envelope heatmap on a log-frequency axis with the DIO+StoneMask F0 trajectory overlaid as a cyan polyline, a live F0 readout at the playhead, and frequency-axis ticks in the gutter.
- The analysis is an independent pure-Rust port of the WORLD vocoder's core algorithms (mmorise/World, BSD-3-Clause) in `render/world_features.rs` — DIO band-wise zero-crossing F0 candidates, StoneMask instantaneous-frequency refinement, CheapTrick pitch-adaptive envelope — with unit tests covering sines (55/100/440 Hz), sweeps, silence/noise voicing decisions, and envelope peak placement.
- Runs as a cached background job like the other feature views (auto-starts on view switch, cancel/progress wired, invalidated on edits); frame period scales with clip length so long files stay bounded. Inspector shows median F0, voiced ratio, hop size, and a Re-analyze button.
- Wired through session persistence (`other_view: "world"`, legacy `"f0"` accepted), the `S` view-cycle hotkey, `--open-view-mode world`, export-settings view picker, and kittest coverage (view switching + an end-to-end analysis test).

### MiniMeter Overhaul: Vectorscope, Per-Channel Peaks, Better Analyzer
- New STEREO panel in the editor bottom strip: goniometer/vectorscope (Lissajous, auto-gain, L/R diagonal guides) plus a smoothed correlation bar (-1..+1). Mono files collapse onto the mid axis and show a MONO badge; files with 3+ channels visualize the first pair and show a CH1+2 badge.
- PEAK panel now draws one bar per channel for any channel count (L/R labels for stereo, numbered otherwise), each with its own peak-hold and RMS tick; the readout shows the loudest channel.
- Spectrum analyzer is dual-resolution: a long FFT (~170 ms window) feeds the low band so bass peaks are localized instead of smeared, a short FFT keeps the high band fast, with a log-domain blend across the crossover; sub-bin columns are interpolated so lows render as a smooth curve.
- Analyzer ballistics: fast attack (~10 ms) with a prompt release (~100 ms) so bars fall cleanly back to the floor when the signal goes quiet, and the strip keeps animating until the decay settles after playback stops.
- Meter DSP moved to `render/mini_meter.rs` with unit tests (low/mid/high peak accuracy, dBFS calibration, ballistics, correlation) and a frame-budget test; per-frame state lives on the tab (no per-frame allocations), keeping the strip comfortably inside a 30 fps budget.
- Fixed Linux link failure of the `neowaves` binary: the DirectML execution provider was referenced unconditionally in transcription session setup (Windows-only symbol).

## 0.20260704.0 - 2026-07-04

### UI Overhaul: Effect Graph, Resizable Panels, Seam Check, MiniMeter (Latest)
- Effect Graph console now docks under the canvas only (left palette stays full height), with a drag-resizable, height-clamped panel so it can no longer swallow half the window; rows are monospace, severity-colored, truncated with tooltips, and the header shows a validation-issue count.
- Effect Graph nodes restyled: soft drop shadow, accent-tinted header with underline, slimmer status border with a selection glow, pill-shaped elapsed-time badge, ringed port pins, and cables with a dark underlay for depth. Left/right panels gained sane min/max resize ranges.
- Editor inspector width is drag-resizable via a divider between canvas and inspector (remembered for the session); Effect Graph side panels are also clamped-resizable.
- Loop Inspector replaced with a real seam-continuity check: the audio running into the loop end and out of the loop start is drawn as one continuous trace joined at the jump, with the crossfaded result overlaid, a log-scale window zoom (2–250 ms), auto-gain, and a click-risk verdict (amplitude step vs. local motion).
- New MiniMeter strip fills the empty space under the editor overview: realtime oscilloscope, log-frequency spectrum analyzer with hue-swept bars, and a peak/RMS meter with peak-hold, all following the playhead.

### Inspector Overhaul: Loop Edit / Auto Trim / Tempogram / Chromagram
- Loop Edit no longer overflows the inspector: a mis-nested layout row swallowed the Apply button and the whole Auto Detect section into one wrapping row; crossfade controls now sit on two rows, loop-range readouts truncate with tooltips, and detect candidates render as fixed-width color-coded rows.
- Loop auto-detect scoring got stricter and more musical: anti-correlated seams no longer earn a baseline score, a long-range loudness-envelope similarity term rewards structurally matching sections, near-silent seams are penalized, and refined candidates deduplicate within ~20 ms so the list shows distinct alternatives.
- Auto Trim is now live: thresholds are sliders with units and plain-language tooltips, the measured noise floor / peak / effective threshold are shown in dB after a run, and edits re-run detection automatically (debounced) so the selected ranges update as you drag.
- Tempogram is readable: values are normalized globally (silence stays dark instead of amplifying noise), the BPM axis is always drawn, and a green guide line + label marks the estimated BPM with half/double-tempo hints in the panel.
- Chromagram is readable: displayed values use per-frame raw chroma (key estimation still runs on the CENS profile), pitch-class bands are equal-height and aligned with always-visible note labels, and the detected key's row is highlighted.
- Inspector styling pass: consistent accent-bar section headers, unified spacing, confidence meters for BPM/key estimates.

### FLAC Support + Format/Metadata Matrix
- Added FLAC decode via symphonia (`flac` feature) and FLAC encode via `flacenc` (16/24-bit; 32-bit float sources are quantized to 24-bit since FLAC has no float representation).
- FLAC now works across list/editor load, save/overwrite, format convert ("To FLAC"), gain export, and virtual-item export; list shows a FLAC badge.
- Loop markers for FLAC are stored as Vorbis comments (`LOOPSTART`/`LOOPEND`, same convention as MP3/M4A); BPM (`BPM`/`TEMPO` comment) and cover art (`PICTURE` block) are read.
- FLAC→FLAC saves carry `VORBIS_COMMENT` + `PICTURE` blocks over (stream-dependent `SEEKTABLE`/`CUESHEET` are intentionally dropped).
- OGG loop markers no longer fail the whole save: formats without in-file loop support now fall back to a `<stem>.loop.json` sidecar (read + write).
- Installer: added missing `.aiff`/`.aif`/`.ogg` file associations and new `.flac`.
- Documented the per-format support matrix and export policy for unsupported metadata in `docs/FORMAT_SUPPORT.md`; updated README format list.

### CLI Replacement / MCP Removal
- Added docs-first CLI replacement specs under `docs/CLI_*.md`.
- Default startup remains GUI; headless automation now enters through `--cli`.
- Replaced the handwritten startup parser with `clap`, including richer `--help` output for GUI mode and CLI subcommands.
- Added Phase 1 headless commands for session/item/list/editor/render/export/debug with JSON stdout envelopes.
- Added direct waveform/spectrum PNG rendering and GUI-backed list/editor screenshot rendering for CLI workflows.
- Removed runtime MCP wiring from the app shell and menus; repo/docs now point to `--cli` as the supported automation surface.

### Refactor: Large File / Large Function Split
- Split app startup and frame orchestration out of `src/app.rs` into `src/app/app_init.rs` and `src/app/frame_ops.rs`.
- Moved tab open/activate and editor decode orchestration into `src/app/tab_ops.rs` and `src/app/editor_decode_ops.rs`.
- Split top bar UI into `src/app/ui/topbar/{menus,transport,status}.rs` and reduced the large status-row renderer into smaller activity helpers.
- Split CLI parsing out of `src/main.rs` into `src/cli.rs`, keeping `main.rs` focused on native startup.
- Split list UI support code into `src/app/ui/list/navigation.rs` and `src/app/ui/list/table.rs`; `ui_list_view` now acts as the main orchestration entry instead of carrying focus logic and table definition inline.
- Documented current staged large-file exceptions in `README.md` and `AGENTS.md` so remaining big files are explicit rather than implicit.

### Settings/Theme + Undo/Redo + List UX (Latest)
- Added Appearance setting (Dark/Light), default Dark; preference persists across restarts.
- Fixed initial theme application so startup respects the saved theme.
- Added editor Undo/Redo (Ctrl+Z / Ctrl+Shift+Z) with toolbar buttons; destructive ops are tracked.
- List UX: click selection no longer auto-centers; keyboard selection still auto-centers.
- Metadata loading now prioritizes visible rows when jump-scrolling.

### Waveform/Overlay Consistency + Loop UI Simplification (Latest)
- Overlay rendering reworked to match base waveform across all zoom modes.
  - Line (spp < 1.0): per-sample polyline + stems (pps >= 6) — identical to base.
  - Aggregated (spp >= 1.0): pixel-locked min/max bins per px column — identical to base.
  - Time-stretched overlays map visible window via ratio; binning uses base px columns to avoid drift.
  - LoopEdit boundaries are emphasized by drawing the same bins again with a thicker stroke.
  - Fixed overlay-window mapping: start/end now derived from the visible window, not the whole file.
- Loop controls in the top bar are simplified: keep only Loop mode toggles (Off / On / Marker).
  - Numeric seconds for Start/End and Set Start/End/Clear were removed from the top bar.
  - Loop region editing is now centralized in Inspector > LoopEdit (samples), K/P keys still supported.
- Added debug prints for zoom/overlay mapping in dev builds to diagnose platform-specific input/rounding.

### Editor Loop/Selection Rework (Breaking)
- Removed range Selection and the Seek/Select tool. The canvas always seeks on click.
- Introduced independent `loop_region` per editor tab. Loop playback uses:
  - `Off` / `OnWhole` / `Marker` (Marker uses `loop_region`), toggled via `L`.
  - Start/End can be edited as samples in Inspector > LoopEdit.
  - (Changed) The top bar no longer offers numeric Start/End editing.
  - Added buttons to set Start/End from current playhead position.
  - New: Loop crossfade. Configure duration (ms) and shape (Linear/EqualPower) in
    LoopEdit. Playback blends end→start inside the last N samples for click‑free loops.
- WAV `smpl` loop markers are now read on load and mapped into `loop_region` (SR conversion considered).
- Inspector changes:
  - LoopEdit shows Start/End (samples), Set Start/End @ Playhead, Clear Loop.
  - Trim/Fade/Gain/Normalize/Reverse/Silence now apply to Whole only.
  - Export Selection removed.
- Keyboard changes:
  - K = Set Loop Start @ playhead, P = Set Loop End @ playhead
  - L = Loop Off ⇄ OnWhole toggle
  - Removed A/B and I/O bindings (Selection removed)
- Fixed pending action wiring: Reverse/Gain/Normalize/Silence are now correctly applied and update playback/loop state.
- Play position can be edited numerically (seconds) from the top bar.

### UI Improvements (Latest)
- Editor zoom/pan reliability: fixed cases where Ctrl+Wheel zoom didn't fire on some environments.
  - Hover detection now uses canvas-rect hit test instead of `Response::hovered`.
  - Wheel input combines `raw_scroll_delta` with low-level `Event::Scroll` and pinch `Event::Zoom`.
  - Added optional debug trace in dev builds to log incoming deltas.
- Split `src/app.rs` into submodules: `src/app/{types,helpers,meta,logic}.rs` for clearer responsibilities.
- Documented upcoming Editor 2.0 spec (multichannel lanes, dB grid, mouse seek, time zoom). See `docs/EDITOR_SPEC.md`.
- "Choose" メニューを2項目に整理し操作を明確化:
  - "Folder...": フォルダを選択して一覧を置き換え（rootに設定して再走査）
  - "Files...": 複数ファイルを選択して一覧を置き換え（rootはクリア）
- ドラッグ&ドロップでファイル/フォルダ追加に対応（WAVのみ、重複は自動スキップ）。追加時は検索/ソートを保ちつつメタを非同期再計算。
- 縦スクロールバーを常に右端に配置: テーブルに非表示の余白列（Column::remainder）を追加して右端まで広げるように変更。Wave列の表示位置は従来どおり維持。
- **Enhanced Keyboard Controls**: Added more intuitive keyboard shortcuts
  - **Ctrl+W**: Close active editor tab (with automatic audio stop)
  - Maintains existing shortcuts (Space for play/pause, L for loop toggle, arrow keys for navigation)
- **Improved Mouse Interaction**: Better click and double-click behavior
  - **Single-click**: All text columns (File/Folder/Length/Ch/SR/Bits/Level/Wave) now selectable for easier navigation
  - **Double-click on File name**: Opens file in editor tab (was single-click before)
  - **Double-click on Folder**: Opens folder in system file browser with the WAV file pre-selected
  - **Single-click on row background**: Selects row and loads audio (unchanged)
- **Tab Navigation Audio Control**: Enhanced audio control for better user experience
  - Switching between tabs (List ⇔ Editor) now automatically stops audio playback
  - Closing editor tabs with the "x" button also stops audio playback
  - Prevents confusion from audio continuing when user switches context
- **Playback Behavior**: List view now always disables loop playback for better audio previewing
  - List display: Always plays once and stops (optimal for quick audio preview)
  - Editor tabs: Loop toggle available via L key (for detailed editing work)
- **Table Layout Fixes**: Fixed text overflow and header collision issues
  - Added Length column (mm:ss format) with proper sorting by duration in seconds
  - Made all columns resizable with optimized initial widths
  - Long text (file names, folder paths) now truncates with "..." and shows full text on hover
  - Improved cell layout to prevent text from appearing behind headers

### Editor View
- Implemented mouse seek/scrub and time zoom/pan interactions
  - Click/drag to seek; Ctrl+Wheel to zoom; Shift+Wheel (or horizontal wheel) to pan.

### Core Features
- Refactor into modules: `audio`, `wave`, `app`, minimal `main`.
- Add seamless loop playback (no gap) with loop toggle (button + `L`).
- Replace global volume with dB slider (-80..+6 dB), internally converted to linear gain.
- Smooth playhead updates using `request_repaint_after(16ms)`.
- Add Mode dropdown: Speed / PitchShift / TimeStretch.
  - Speed: realtime playback-rate change (0.25–4.0), pitch not preserved.
  - PitchShift: semitone shift (-12..+12), duration preserved, offline using `signalsmith-stretch`.
  - TimeStretch: stretch factor (0.25–4.0), pitch preserved, offline using `signalsmith-stretch`.
- Heavy processing system:
  - Pitch/Stretch run on a background thread; UI shows a full-screen blocking overlay with spinner and message until completion.
  - Results (processed buffer + waveform) are applied atomically on completion.
- Stretch/pitch tail handling:
  - Consider `output_latency()` and append `flush()` tail to avoid truncated endings; reduce loop boundary hiccups.
- List/UX tweaks:
  - Level (dBFS) palette expanded (black→deep blue→blue→cyan/green→yellow→orange→red) for clearer differences.
  - File name click opens editor tab (row also becomes selected). Background click continues to select + preload audio.
  - Folder cell click opens the folder in the OS file browser.
  - Disabled global hover brightening to avoid sluggish hover-follow effect; clickable cells now use button styling with pointer cursor.
  - Switching tabs now reloads the active tab's audio and loop state so playback always reflects the selected editor.
  - Columns added: Ch/SR/Bits に加えて LUFS (I) と Gain (dB) を表示。LUFS は近似→非同期再計算で更新し、すべての列で tri-state ソートに対応。
  - Added Search bar (filters by filename/folder), tri-state sorting (asc/desc/original), and auto-scroll to keep the selected row visible.
  - Top bar shows file counts (visible/total) with loading indicator (⏳) while metadata is still arriving.
  - Speed control moved to input field: "Speed x [1.0]" (0.25–4.0) with validation; audio engine supports fractional-rate playback with linear interpolation.
- List view rework/perf:
  - Use `TableBuilder` with internal vscroll and `min_scrolled_height(...)` to fill to bottom.
  - Virtualized rows via `TableBody::rows` (render only visible rows) for 10k–30k entries.
  - Whole-row click selection by setting `.sense(Sense::click())` and using `row.response()`.
  - Resizable columns; Wave column expands thumbnails (height tracks width).
  - Per-row background color for Level (dBFS) with overlaid text.
  - Async metadata worker (RMS + 128-bin thumbnails) with incremental updates.
  - Keyboard: Up/Down selection, Enter to open, click loads audio, double-click opens tab.
- Editor view improvements:
  - Waveform height grows with width; grid lines; amplitude-based coloring (blue→red).
- Fonts: Load Meiryo/Yu Gothic/MS Gothic on Windows to avoid tofu.
- Build notes: On Windows, install LLVM and set `LIBCLANG_PATH` when enabling `signalsmith-stretch`.
- Known issues documented (Windows EXE lock, UTF-8, etc.).

## 0.1.0 (initial)

- Basic egui app with WAV decoding (hound), CPAL output, min/max waveform, RMS meter.
- Docs: Added editing roadmap (planned) to README/UX/EDITOR_SPEC
- Dependency bumps (compat)
  - cpal: 0.15 → 0.16 (no code changes required here)
  - rfd: 0.14 → 0.15.4
  - egui/eframe/egui_extras remain at 0.27 series intentionally for now to avoid
    a large breaking migration to 0.32+. We will plan that upgrade separately.
