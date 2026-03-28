# UPDATE REQUEST PLAN (2026-03-27)

## 2026-03-28 Implementation Update

Implemented in code:
- vertical rail is now shared by `Wave`, `Spectrogram`, `Log`, and `Mel`
- `vertical_zoom` clamp is now `0.25..=32.0`
- time zoom floor is now `0.0025 samples_per_px`
- editor keeps `vertical_view_center` per tab and restores it via undo/session
- destructive apply paths invalidate and rebuild editor viewport caches
- spec/log/mel viewport rendering now uses async editor viewport jobs
- UI consumes `fine -> coarse -> last compatible -> bounded fallback`
- bounded fallback for spectral views now uses a small raster only, not the old full sync draw

Implementation notes:
- new async viewport worker and cache logic lives in `src/app/editor_viewport.rs`
- right rail state is stored in `EditorTab.vertical_zoom` and `EditorTab.vertical_view_center`
- cache invalidation is triggered on destructive apply, source swap, decode progress/final, and session restore
- waveform overlay in non-waveform views remains a reference overlay and does not zoom with frequency space

Validation status:
- `cargo check`
- `cargo test --features kittest --test gui_kittest_suite editor_`
- `cargo test --features kittest --test small_fix_regressions`
- manual screenshot check with `debug/mel_async_summary_after.png`

## 0. Purpose

2026-03-27 のサウンドクリエイターフィードバックを、実装者がそのまま着手できる粒度まで落とし込んだ更新計画。
この文書は「要望メモ」ではなく、Editor 入力系、ズーム/パン、再生/メーター、Inspector、prefs/session、回帰テストまで含めた decision-complete な実装仕様として扱う。

今回の対象は次の 9 件。

- 高倍率ズーム時に左右移動やパンが止まることがある
- Shift 拡張選択と右クリック選択の起点が操作ごとにずれる
- 右クリック選択の起点を button-down 時点で固定したい
- マウスホイール zoom と Shift+ホイール水平スクロールの反転設定が欲しい
- 波形の縦方向をもっと拡大したい
- Editor 再生中に pause したとき、次回再生位置を前回再生開始位置へ戻したい
- 横ズーム支点を playhead / pointer で切り替えたい
- Inspector の Preview / Apply がツールごとに不整合
- 左上 dBFS 表示が停止中と再生中で逆転することがある

---

## 1. Summary

### 1-1. 今回の主方針

- 入力系は `selection_anchor_sample` と `view_offset_exact` を導入して、選択起点と微小パン量を統一管理する
- zoom / pan / pause / meter は app-level prefs と playback state を追加し、UI ごとの場当たり処理をやめる
- Inspector の destructive apply は共通後処理ヘルパーに寄せ、trim と async apply だけ正しい状態になっている現状を解消する
- `vertical_zoom` は per-tab 状態として追加し、session に保存・復元する

### 1-2. 非目標

- Editor 全面 redesign
- List 画面や transport 全体の設計刷新
- large clip preview の新方式追加

large clip preview については今回新機能を入れず、既存の sample limit 制限は維持したまま、無効理由表示と Apply 後の反映不整合だけ直す。

---

## 2. Current Code Findings

### 2-1. 高倍率ズーム時のパン停止

現行の `src/app/ui/editor.rs` では Shift+Wheel pan と middle/alt drag pan がどちらも `samples_per_px` を掛けたあと整数へ即変換している。
代表的には次のような処理になっており、高倍率ズーム時は 1 event あたりの移動量が 1 sample 未満になって切り捨てられる。

- `(delta_px * tab.samples_per_px) as isize`
- `(-dx * tab.samples_per_px) as isize`

根本原因は `view_offset` を整数のまま直接更新していること。小数蓄積がないため、微小移動が失われる。

### 2-2. Shift 拡張選択の不整合

現行の選択関連 state は次の通り。

- `selection`
- `drag_select_anchor`
- `right_drag_anchor`
- `right_drag_mode`

`drag_select_anchor` は primary drag と Shift+Arrow 系、`right_drag_anchor` は Shift+right-drag 系で使われており、Shift+Click と同じ起点共有モデルになっていない。
そのため「Shift+左右で調整したあと、別位置で Shift+Click すると起点からではなく別の基準で伸びる」という不整合が起きる。

### 2-3. 右クリック選択の起点ずれ

現行の右 drag 選択は button-down 時点のサンプルではなく、drag started 時に playhead 基準や hover 位置を使う分岐が入っている。
要求仕様は「secondary button down の瞬間の座標を anchor に固定」であり、drag 開始後の hover や playhead は使わない。

### 2-4. zoom/pan/pause の prefs 基盤

`src/app/theme_ops.rs` には `prefs.txt` の `key=value` load/save が既にあり、新しい editor prefs はここへ追加するのが最小コスト。
一方で current code では次の設定が未実装。

- wave zoom wheel inversion
- Shift+wheel horizontal pan inversion
- horizontal zoom anchor mode
- editor pause resume mode

### 2-5. 縦ズーム未実装

波形描画の振幅スケールは `src/app/ui/editor.rs` 内で `wave_rect.height() * 0.42` や `0.46` のような固定倍率を使っている。
per-tab の縦倍率 state がなく、overlay / marker / selection と揃った見た目で大きく表示する手段がない。

### 2-6. Pause 後の再開位置

`request_workspace_play_toggle()` は editor source でも基本的に単純 toggle で、前回再生開始位置を覚える state を持っていない。
そのため pause 後の次回再生位置を「再生開始位置へ戻す」モードを実装する足場が無い。

### 2-7. Inspector の Apply 後未更新

`src/app/editor_ops.rs` では trim と async apply 完了 (`drain_editor_apply_jobs`) だけが waveform cache を再構築している。
一方、以下の destructive apply は `tab.ch_samples` を更新しても waveform cache 再構築が共通化されていない。

- `editor_apply_fade_in_explicit`
- `editor_apply_fade_out_explicit`
- `editor_apply_reverse_range`
- `editor_apply_gain_range`
- `editor_apply_normalize_range`
- `editor_apply_mute_range`
- `editor_apply_fade_range`
- `editor_apply_loop_xfade`
- `editor_apply_loop_unwrap`
- `editor_delete_range_and_join`

この差が「Apply したのに波形表示だけ古い」「overlay が残る」「selection/anchor の見え方が揃わない」原因になっている。

### 2-8. dBFS 表示の逆転

`current_output_meter_db()` は buffer が無い/無音のとき floor 値を返し、topbar 側はその数値だけを見て `-inf dBFS` を描画している。
停止中か再生中かの判定を UI 表示条件に入れていないため、停止中の stale な meter 値や source swap 後の表示が不自然になる。

---

## 3. Decisions

## 3-A. Types / State

### 3-A-1. `EditorTab` 追加項目

`src/app/types.rs` の `EditorTab` に次を追加する。

```rust
pub selection_anchor_sample: Option<usize>,
pub view_offset_exact: f64,
pub vertical_zoom: f32,
```

意味は以下。

- `selection_anchor_sample`
  - Shift+Arrow / Shift+Click / primary drag / secondary drag で共有する選択起点
  - transient state。session 保存しない
- `view_offset_exact`
  - 水平パンの authoritative state
  - `view_offset` は描画・保存用の整数スナップ値として維持する
  - transient state。session 保存しない
- `vertical_zoom`
  - 波形縦倍率
  - per-tab state として保持し、session 保存対象にする

### 3-A-2. 既存 anchor state の扱い

`drag_select_anchor` と `right_drag_anchor` は今回の変更で置き換える。
同一変更内で call site を全て `selection_anchor_sample` ベースへ移行し、最終的に削除する。
`right_drag_mode` は secondary drag のモード判定用として残す。

### 3-A-3. `vertical_zoom` の仕様

- default: `1.0`
- clamp: `0.25..=8.0`
- step: `1.1` 倍
- session 保存する
- undo/redo 復元対象に含める

### 3-A-4. app prefs 追加項目

`WavesPreviewer` の app-level prefs として次を追加する。

```rust
pub invert_wave_zoom_wheel: bool,
pub invert_shift_wheel_pan: bool,
pub horizontal_zoom_anchor_mode: EditorHorizontalZoomAnchorMode,
pub editor_pause_resume_mode: EditorPauseResumeMode,
```

enum は次で固定する。

```rust
enum EditorHorizontalZoomAnchorMode {
    Pointer,
    Playhead,
}

enum EditorPauseResumeMode {
    ReturnToLastStart,
    ContinueFromPause,
}
```

### 3-A-5. prefs.txt key 名

`src/app/theme_ops.rs` で次の key を load/save する。

- `editor_invert_wave_zoom_wheel=true|false`
- `editor_invert_shift_wheel_pan=true|false`
- `editor_horizontal_zoom_anchor=pointer|playhead`
- `editor_pause_resume_mode=return_to_last_start|continue_from_pause`

default は次で固定する。

- wheel inversion 2 種: `false`
- horizontal zoom anchor: `pointer`
- pause resume mode: `return_to_last_start`

### 3-A-6. session / project 保存対象

`src/app/project.rs` / `src/app/session_ops.rs` の editor-tab 保存項目に `vertical_zoom` を追加する。

- 保存する: `vertical_zoom`
- 保存しない: `selection_anchor_sample`, `view_offset_exact`

session 読み込み時と tab 初期化時は、`view_offset_exact = view_offset as f64` で同期する。

### 3-A-7. playback state 追加項目

`PlaybackSessionState` 相当の再生状態に次を追加する。

```rust
pub last_play_start_display_sample: Option<usize>,
```

秒ではなく display sample 単位で保持する。理由は editor 側の seek/selection/zoom が sample 単位で統一されており、変換誤差を避けやすいため。

---

## 3-B. Input / Selection / Zoom / Pan

### 3-B-1. 選択起点ルール

`selection_anchor_sample` は「現在の選択範囲をどこから伸ばしているか」を示す persistent transient state とする。
mouse release では消さない。次の場合だけ clear する。

- 非 Shift の単独 click で selection を解除したとき
- 明示的な selection clear
- destructive apply 後
- tab 再初期化 / file reload / undo state 全復元時に旧 anchor が不正になるとき

### 3-B-2. 操作別の anchor 更新規則

- primary drag selection
  - button-down sample を `selection_anchor_sample` に設定
  - drag 中はその anchor から current sample まで `selection` を更新
- secondary drag selection
  - secondary button-down sample を `selection_anchor_sample` に設定
  - drag 中はその anchor から current sample まで `selection` を更新
- Shift+Arrow
  - anchor があればそれを使う
  - anchor が無ければ current display sample を anchor として確定し、その後の extension に使う
- Shift+Click
  - anchor があれば anchor から clicked sample まで `selection` を再計算
  - anchor が無ければ current display sample を anchor として確定してから clicked sample へ伸ばす

### 3-B-3. 右クリック anchor の決定タイミング

secondary button による範囲選択は `button down` の瞬間に anchor を確定する。
`drag_started` 時や playhead 位置で上書きしない。
button-down 後に pointer が動いても、anchor は固定のまま end 側だけが更新される。

### 3-B-4. Shift+Click 再定義

右クリック選択や Shift+Arrow で一度 anchor が確定したあと、別の場所で Shift+Click した場合は、常に「保持中 anchor から clicked sample まで」で範囲を再計算する。
これにより、選択を伸ばす/縮める基準が操作ごとに変わらない。

### 3-B-5. パンの小数蓄積

水平パンは `view_offset_exact` を正とする。

- Shift+Wheel pan
- middle drag pan
- alt+left drag pan
- 将来 keyboard pan を足す場合も同じ helper を使う

更新手順は以下で固定する。

1. 小数 sample delta を `view_offset_exact` に加算
2. clamp した `view_offset_exact` から `view_offset = round(view_offset_exact)` を導出
3. 描画・session 保存は `view_offset` を使う
4. view を直接整数で再配置したときは `view_offset_exact = view_offset as f64` へ同期する

### 3-B-6. zoom anchor ルール

zoom anchor の判定は helper へ寄せる。
対象は次の zoom 操作。

- Ctrl/Cmd + wheel
- pinch / gesture zoom
- ArrowUp / ArrowDown による水平 zoom

`Pointer` モード:

1. pointer が wave rect 内で取得できれば pointer sample
2. 取得できなければ playhead sample
3. それも使えなければ viewport center sample

`Playhead` モード:

1. playhead が current viewport 内にあれば playhead sample
2. 無ければ viewport center sample

### 3-B-7. wheel inversion

- `invert_wave_zoom_wheel`
  - plain wheel の zoom 方向だけを反転する
  - pinch / `zoom_delta()` の意味は変えない
- `invert_shift_wheel_pan`
  - Shift+wheel の水平 pan 方向だけを反転する
  - middle drag や keyboard pan 方向は変えない

### 3-B-8. 縦ズーム操作入口

今回の実装では vertical zoom の操作入口を editor header のボタンではなく、波形キャンバス右端の `Amplitude` ナビゲータへ置き換える。

- header の `Y-` / `Y+` / `Y Reset` は削除する
- 下部 overview は `Time` ラベルを付けた time navigator として維持する
- 右端 `Amplitude` ナビゲータは canvas 内に置き、Inspector には複製しない
- `Amplitude` ナビゲータは centered viewport card と small reset link を持ち、click / drag で `vertical_zoom` を更新する
- キーボードショートカット追加は今回の必須要件にしない

縦ズームは base waveform、preview overlay、loop seam preview、selection tint、marker stem/label が同じ倍率系で見えることを要件にする。

---

## 3-C. Playback / Pause / Meter

### 3-C-1. Pause resume モード

default は `ReturnToLastStart`。

仕様:

- editor source が `not playing -> playing` に遷移する瞬間に `last_play_start_display_sample` を保存する
- `playing -> pause` の toggle 時:
  - `ReturnToLastStart`
    - pause 直後に `last_play_start_display_sample` へ seek して playhead を戻す
  - `ContinueFromPause`
    - pause 時点の playhead をそのまま維持する
- EOF、source swap、destructive apply、buffer 差し替え、tab reload 時は `last_play_start_display_sample` を clear する

### 3-C-2. dBFS 表示

左上 dBFS 表示は次で固定する。

- stopped: `-inf dBFS`
- playing + silent: `-inf dBFS`
- playing + signal: 実測 dBFS

停止中に直前の値を保持しない。
meter text と meter bar は同じ playback state 判定を使う。

### 3-C-3. meter 更新ポリシー

- `current_output_meter_db()` は playback state を見て stopped 時は floor 扱いへ寄せる
- source swap / buffer replace / stop 時は meter hold をクリアする
- topbar 側では数値だけでなく再生状態も見て表示を決める

---

## 3-D. Inspector / Destructive Apply

### 3-D-1. 共通後処理ヘルパー

`src/app/editor_ops.rs` に destructive apply 共通の後処理ヘルパーを追加し、全 destructive apply から必ず通す。

後処理内容は次で固定する。

1. `samples_len` 更新
2. waveform min/max 再構築
3. waveform pyramid 再構築
4. audio buffer 更新
5. `editor_clamp_ranges()` 実行
6. preview overlay / preview tool / stale heavy preview state の invalidation
7. selection anchor の clear または clamp
8. marker / loop dirty 再計算
9. 再生中だった場合の seek / stop 整合

trim と `drain_editor_apply_jobs()` は既存で近い挙動を持つため、そこを基準実装にして他の destructive apply を寄せる。

### 3-D-2. 共通後処理の対象

最低限次の apply を共通化対象に含める。

- Fade In
- Fade Out
- Reverse
- Gain
- Normalize
- Mute
- range Fade
- Loop xfade
- Loop unwrap
- Delete/Join

加えて async apply 系も最終的に同じ共通後処理へ寄せる。

### 3-D-3. Inspector Preview の扱い

Preview ボタンの無効理由は次の 3 種に分ける。

- 未対応ツール
- large clip の live preview 制限
- busy / stale job state

UI では「押せない」だけにせず、どの理由で無効かを明示する。
sample limit 制限そのものは今回変更しない。

### 3-D-4. 修正確認対象のツール

今回の確認対象は次で固定する。

- Gain
- Normalize
- Loudness
- Reverse
- Fade
- Loop Edit
- PitchShift
- TimeStretch
- Plugin FX

各ツールについて、Preview 有効/無効理由、Apply 後の waveform 更新、overlay 破棄、dirty 更新を確認する。

---

## 4. File Touchpoints

主な変更対象は以下。

- `src/app/ui/editor.rs`
  - zoom / pan / pointer anchor helper
  - selection / right-drag / Shift+Click
  - time / amplitude navigator UI
  - Inspector Preview / Apply の enable/disable 整理
- `src/app/editor_ops.rs`
  - destructive apply 共通後処理
  - selection anchor / preview invalidation / waveform rebuild
- `src/app/types.rs`
  - `EditorTab`
  - prefs enum
  - undo/view state
- `src/app/theme_ops.rs`
  - prefs load/save
- `src/app/project.rs`
  - `vertical_zoom` serialize
- `src/app/session_ops.rs`
  - `vertical_zoom` restore
- `src/app.rs`
  - `PlaybackSessionState`
  - meter state
- `src/app/logic.rs`
  - play/pause toggle behavior
- `src/app/ui/topbar.rs`
  - dBFS 表示条件
- `tests/gui_kittest_suite.rs`
  - GUI 回帰テスト
- `tests/small_fix_regressions.rs`
  - ピンポイント回帰テスト
- `src/app/kittest_ops.rs`
  - prefs / playback / right-drag / session roundtrip helper

---

## 5. Test Plan

### 5-1. 追加する GUI / kittest

`tests/gui_kittest_suite.rs` に次を追加する。

- `editor_high_zoom_shift_wheel_pan_does_not_stall`
- `editor_high_zoom_middle_drag_pan_does_not_stall`
- `editor_shift_arrow_then_shift_click_reuses_anchor`
- `editor_right_drag_then_shift_click_reuses_anchor`
- `editor_secondary_selection_anchor_is_button_down_sample`
- `editor_zoom_inversion_pref_roundtrip`
- `editor_shift_pan_inversion_pref_roundtrip`
- `editor_horizontal_zoom_anchor_pointer_keeps_pointer_sample`
- `editor_horizontal_zoom_anchor_playhead_keeps_playhead_sample`
- `editor_vertical_zoom_roundtrip_in_session`
- `editor_time_navigator_label_visible`
- `editor_amplitude_navigator_label_visible`
- `editor_amplitude_navigator_drag_changes_vertical_zoom`
- `editor_amplitude_navigator_reset_label_restores_default`
- `editor_pause_resume_return_to_last_start`
- `editor_pause_resume_continue_from_pause`
- `editor_apply_gain_rebuilds_waveform_cache`
- `editor_apply_reverse_rebuilds_waveform_cache`
- `editor_apply_loop_unwrap_rebuilds_waveform_cache`
- `editor_stopped_meter_shows_neg_inf`

### 5-2. 追加する regression test

`tests/small_fix_regressions.rs` に次を追加する。

- `shift_click_after_shift_arrow_uses_saved_anchor`
- `secondary_drag_anchor_is_not_replaced_by_playhead`
- `stopped_meter_does_not_show_stale_value`

### 5-3. helper 追加

`src/app/kittest_ops.rs` に次の helper を足す。

- editor prefs save/load helper の getter
- current selection anchor の読取 helper
- `last_play_start_display_sample` の読取 helper
- `vertical_zoom` の設定/読取 helper
- right-drag の button-down sample 注入 helper

### 5-4. 手動確認項目

- pointer anchor / playhead anchor の体感差
- large clip で Preview 無効理由が正しく出ること
- vertical zoom 時に marker label / loop seam preview / overlay が崩れないこと

---

## 6. Acceptance Criteria

- 最大ズーム付近でも Shift+Wheel と drag pan が停止せず、event を重ねれば必ず少しずつ移動する
- Shift+Arrow、Shift+Click、right-drag が同一 anchor を共有する
- 右クリック選択の起点は secondary button-down 座標で固定される
- wheel inversion 2 種と horizontal zoom anchor mode が prefs に保存される
- vertical zoom が per-tab で効き、session reopen 後も復元される
- default の pause/play は前回再生開始位置へ戻る
- 設定切替で pause 位置継続に変えられる
- Inspector の Apply 後に waveform/overlay/selection 表示が古いまま残らない
- 停止中 dBFS は常に `-inf dBFS` で、再生中のみ実 meter 値へ遷移する

---

## 7. Recommended Implementation Order

### Phase 1

1. destructive apply 共通後処理
2. dBFS 表示修正
3. `view_offset_exact` 導入と微小パン修正

### Phase 2

4. `selection_anchor_sample` 導入
5. secondary button-down anchor 化
6. Shift+Click / Shift+Arrow の統一

### Phase 3

7. prefs 4 種追加
8. horizontal zoom anchor helper 化
9. pause resume mode 実装
10. per-tab `vertical_zoom` 実装

### Phase 4

11. kittest / regression test 追加
12. `docs/CONTROLS.md` と `CHANGELOG.md` 更新

---

## 8. Assumptions / Defaults

- 停止中 dBFS 表示は `-inf dBFS`
- `vertical_zoom` は per-tab 状態
- `vertical_zoom` は session 保存対象
- horizontal zoom anchor の既定値は `Pointer`
- pause resume mode の既定値は `ReturnToLastStart`
- wheel inversion は zoom 用と Shift+pan 用を分離し、既定値は両方 off
- 今回は large clip preview の新機能を足さず、Inspector の有効/無効判定と Apply 後反映整合に限定する
