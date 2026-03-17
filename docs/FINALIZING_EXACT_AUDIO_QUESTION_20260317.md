# Finalizing Exact Audio 質問と回答

作成日: 2026-03-17

## 背景

NeoWaves では、pristine な physical WAV に対しては exact-stream で即再生しつつ、裏で editor 用の最終 exact buffer と waveform cache を作っています。

ただし `Finalizing exact audio` が長く、長尺ファイルや `44.1kHz -> 48kHz` のようなケースで待ち時間が大きくなります。

この文書は、現状コードの抜粋と、それに対する質問と回答を 1 つにまとめたものです。

## 現状の実装コード抜粋

### 1. `Finalizing exact audio` に入る箇所

`src/app/logic.rs`

```rust
let _ = tx.send(EditorDecodeResult {
    path: path_for_thread.clone(),
    event: EditorDecodeEvent::Progress,
    channels: Vec::new(),
    waveform_minmax: Vec::new(),
    waveform_pyramid: None,
    loading_waveform_minmax: loading_waveform_minmax.clone(),
    buffer_sample_rate: out_sr.max(1),
    job_id,
    error: None,
    stage: EditorDecodeStage::FinalizingAudio,
    decoded_frames: Self::convert_source_frames_to_output_frames(
        decoded_source_frames,
        source_sr,
        out_sr.max(1),
    ),
    decoded_source_frames,
    total_source_frames,
    visual_total_frames,
    progress_emit_gap_ms: last_progress_emit_at
        .map(|prev| prev.elapsed().as_secs_f32() * 1000.0),
    finalize_audio_ms: None,
    finalize_waveform_ms: None,
});
let finalize_audio_started = std::time::Instant::now();
let channels = Self::process_editor_decode_channels(
    full_source_channels,
    source_sr,
    out_sr,
    target_sr,
    bit_depth,
    resample_quality,
);
let finalize_audio_ms = finalize_audio_started.elapsed().as_secs_f32() * 1000.0;
```

### 2. `Finalizing exact audio` の中身

`src/app/logic.rs`

```rust
fn process_editor_decode_channels(
    mut chans: Vec<Vec<f32>>,
    in_sr: u32,
    out_sr: u32,
    target_sr: Option<u32>,
    bit_depth: Option<crate::wave::WavBitDepth>,
    resample_quality: crate::wave::ResampleQuality,
) -> Vec<Vec<f32>> {
    if let Some(target) = target_sr {
        let target = target.max(1);
        if in_sr != target {
            for c in chans.iter_mut() {
                *c = crate::wave::resample_quality(c, in_sr, target, resample_quality);
            }
        }
        if target != out_sr {
            for c in chans.iter_mut() {
                *c = crate::wave::resample_quality(c, target, out_sr, resample_quality);
            }
        }
    } else if in_sr != out_sr {
        for c in chans.iter_mut() {
            *c = crate::wave::resample_quality(c, in_sr, out_sr, resample_quality);
        }
    }
    if let Some(depth) = bit_depth {
        crate::wave::quantize_channels_in_place(&mut chans, depth);
    }
    chans
}
```

### 3. decode 中に全サンプルをため込んでいる箇所

`src/app/logic.rs`

```rust
let mut full_source_channels: Vec<Vec<f32>> = Vec::new();

// ...

if full_source_channels.is_empty() {
    full_source_channels = vec![Vec::new(); chunk.len().max(1)];
    if let Some(total) = total_source_frames {
        for ch in &mut full_source_channels {
            ch.reserve(total.min(1_000_000));
        }
    }
}

if full_source_channels.len() != chunk.len() {
    full_source_channels.resize_with(chunk.len(), Vec::new);
}

for (dst, src) in full_source_channels.iter_mut().zip(chunk.iter()) {
    dst.extend_from_slice(src);
}
```

### 4. resample 実装

`src/wave.rs`

```rust
pub fn resample_quality(
    mono: &[f32],
    in_sr: u32,
    out_sr: u32,
    quality: ResampleQuality,
) -> Vec<f32> {
    if in_sr == out_sr || mono.is_empty() || in_sr == 0 || out_sr == 0 {
        return mono.to_vec();
    }
    let (sinc_len, f_cutoff, oversampling_factor, interpolation, chunk_size) = match quality {
        ResampleQuality::Fast => (64, 0.90, 64, SincInterpolationType::Linear, 1024),
        ResampleQuality::Good => (128, 0.94, 128, SincInterpolationType::Quadratic, 1024),
        ResampleQuality::Best => (256, 0.96, 256, SincInterpolationType::Cubic, 2048),
    };
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff,
        oversampling_factor,
        interpolation,
        window: RubatoWindowFunction::BlackmanHarris2,
    };
    match resample_with_rubato(mono, in_sr, out_sr, params, chunk_size) {
        Ok(out) if !out.is_empty() => out,
        _ => resample_linear(mono, in_sr, out_sr),
    }
}
```

補足:

- resample コアは完全自前ではなく `rubato::SincFixedIn` を使っています
- fallback として簡易 `resample_linear()` もあります

## 質問と回答

### Q1. いまの `Finalizing exact audio` が重い主因は何か

### A1.

主因はかなりはっきりしています。

1. 固定比のオフライン変換なのに、`rubato::Sinc*` 系をチャンネルごと・全量一括で回している可能性が高いこと
2. `full_source_channels` と最終 `channels` を同時に持つため、長尺でメモリ帯域と再配置コストが大きいこと
3. `target_sr -> out_sr` の 2 段 resample があると、CPU も音質も不利になりやすいこと
4. quantize を open 時にやっていること
5. editor 用 canonical data と playback 用 device-rate data が混ざっていること

特に `44.1 -> 48kHz` は固定比の同期 resampling で、rubato 公式もこのケースでは同期 FFT resampler (`Fft`) がかなり速く、フルクリップ変換には `Fft + FixedSync::Both` を勧めています。また rubato はクリップ全体を事前確保済みバッファへ流し込む `process_all_into_buffer()` を用意しており、resampler の再生成は高コストなので再利用推奨です。いまの `SincFixedIn` 系は「可変比を含む streaming 向け」の色が強く、固定比オフライン変換の第一候補ではありません。([Docs.rs][1])

---

### Q2. resample は「必須」か

### A2.

結論から言うと、resample は「再生の都合で必要になることが多い」のであって、「editor final buffer に常に必要」ではありません。

CPAL 的にも、サンプルレートは「そのデバイスでサポートされている output stream config を開けるか」の話です。`supported_output_configs()` と `try_with_sample_rate()` で決まるのは playback boundary の制約であって、editor 内部データの必須条件ではありません。([Docs.rs][2])

したがって設計としては次が自然です。

- editor / waveform / selection / marker / loop 編集
  - source SR のまま保持してよい
- playback
  - device が source SR を開けるならそのまま
  - 開けないなら再生時だけ source -> device SR へ変換
- export / apply
  - 必要ならそのとき target SR / bit depth へ変換

つまり、pristine WAV の editor final buffer を毎回 `out_sr` にそろえる必然性は薄い、というのが本質です。([Docs.rs][2])

---

### Q3. この構成で最も重い部分はどこか

### A3.

最重はまず全量 resample です。

```rust
for c in chans.iter_mut() {
    *c = crate::wave::resample_quality(c, in_sr, out_sr, resample_quality);
}
```

この形なので、チャンネル単位で別々に resampler を回している可能性が高いです。もし `resample_quality()` の中で毎回 resampler を `new` しているならさらに重くなります。

次点はメモリ帯域です。

- decode で `full_source_channels` に全量蓄積
- finalize で全量走査して resample
- さらに waveform finalize

という流れなので、長尺では CPU 計算より巨大配列の移動・確保が効きやすいです。

---

### Q4. まず計測をどう分解すべきか

### A4.

最低でも以下は分けるべきです。

- `decode_read_ms`
- `decode_append_ms`
- `full_source_realloc_count`
- `finalize_resample_ms`
- `finalize_quantize_ms`
- `finalize_waveform_ms`
- `peak_source_bytes`
- `peak_final_bytes`
- `peak_total_bytes`

resample 自体もさらに分けるべきです。

- `resampler_construct_ms`
- `resampler_process_ms`
- `output_alloc_ms`

rubato では `process_into_buffer()` / `process_all_into_buffer()` が使えるので、事前確保バッファあり/なしの差も測る価値があります。rubato 自体も、リアルタイム用途では `process_into_buffer()` を使い、事前確保バッファを使うよう勧めています。([Docs.rs][1])

---

### Q5. `full_source_channels` に全量をためてから一括処理する設計は妥当か

### A5.

小規模実装としては妥当ですが、長尺では限界が見えています。

理由は 2 つです。

- source と final を同時に持つのでメモリピークが高い
- resample 後に source をすぐ捨てられないので帯域が悪い

また `reserve(total.min(1_000_000))` は、44.1kHz だと約 22.7 秒分しか先取りしていません。長尺では再確保が繰り返されやすいです。

---

### Q6. chunk 単位 / streaming 的に final buffer を作る構成へ変えるべきか

### A6.

はい。中規模改修として最も筋が良いです。

おすすめは次のどちらかです。

- A案: source SR の editor buffer を作るだけなら、decode chunk をそのまま最終 buffer に append
- B案: canonical SR が本当に必要なら、decode chunk -> resampler -> final buffer へ逐次書き込み

この場合、source 全量と resampled 全量を同時保持しなくて済むのが大きいです。rubato も chunk ベース処理の API を前提にしています。([Docs.rs][1])

---

### Q7. per-channel 並列化や chunk 並列化は有効か

### A7.

per-channel 並列化は有効です。ただしもっと先に効くのは「そもそも multi-channel resampler を 1 個使うこと」です。rubato の `Resampler` は channel count を持ち、active channel mask もあります。([Docs.rs][3])

優先順位は次です。

1. mono ごとに `new/process` する構成をやめる
2. multi-channel の 1 resampler にする
3. その上で必要なら rayon で job 並列
4. quantize / waveform pyramid を別スレッド並列

chunk 並列化は、stateful resampler の境界処理があるので単純にはやりにくいです。いまの設計のままなら per-channel / per-job 並列が安全です。

---

### Q8. `rubato::SincFixedIn` はこの用途に適しているか

### A8.

「使えなくはない」が、最適とは言いにくいです。

rubato 公式は、

- 同期 resampling は FFT ベース
- 固定比クリップ変換には `Fft`
- `FixedSync::Both` は内部バッファを減らせてやや効率的
- cubic sinc は高品質だが計算が重い

としています。([Docs.rs][1])

あなたの `44.1 -> 48` は固定比なので、まず `Fft` を第一候補にすべきです。

---

### Q9. 別の resampler 実装や設定のほうが向いているか

### A9.

候補はあります。

- pure Rust を維持したい
  - まず `rubato::Fft`
- ネイティブ依存 OK / オフライン品質重視
  - `libsoxr` か `libsamplerate`
- 超高速 preview / voice 寄り
  - SpeexDSP 系

`libsoxr` は high quality な 1D SRC とされ、オフライン finalization ではレイテンシはほぼ問題になりません。`libsamplerate` も arbitrary ratio の高品質 SRC として広く使われています。SpeexDSP は設計目標として very fast / low memory / good perceptual quality を掲げています。([Docs.rs][4]) ([libsndfile.github.io][5]) ([GitHub][6])

---

### Q10. pristine WAV の final exact buffer を作るとき、常に `out_sr` へ resample する必要があるか

### A10.

不要です。

必要なのは「そのバッファをどこに使うか」です。

- editor 内部表現なら source SR のままでよい
- device へそのまま流す playback cache なら device SR が必要な場合がある
- export target なら target SR が必要

`out_sr` は playback device の事情であって、editor exact buffer の必須条件ではありません。([Docs.rs][2])

---

### Q11. live playback が exact-stream で成立しているなら、editor cache 側も source SR のまま保持してよいか

### A11.

はい。むしろその方が自然です。

---

### Q12. source SR のまま editor tab に持ち、必要なときだけ別 buffer を作る設計は成立するか

### A12.

十分成立します。かなりおすすめです。

---

### Q13. `in_sr == out_sr` かつ `target_sr` も `bit_depth` override も無い場合、`process_editor_decode_channels()` を丸ごとスキップしてよいか

### A13.

はい。完全に yes です。

この条件なら finalize audio は実質 no-op でよいです。その場合 `Finalizing exact audio` 自体を飛ばして、

- `channels = full_source_channels`
- そのまま waveform finalize へ進む

で構いません。

---

### Q14. output device が source SR を開けない場合でも、editor final buffer 側まで必ず `out_sr` に揃える必要があるか

### A14.

不要です。必要なのは playback path のみです。

---

### Q15. 再生用 buffer と editor 表示/編集用 buffer の sample-rate を分離する設計は妥当か

### A15.

非常に妥当です。むしろ長期的にはそれが本命です。

---

### Q16. `in_sr -> target_sr -> out_sr` の 2 段 resample は本当に必要か

### A16.

通常は不要です。

2 段にすると、

- 2 回フィルタがかかる
- 2 回大配列を走査する
- 2 回出力バッファを作る
- 品質劣化リスクも増える

ので、本当に 2 種類の persistent representation が必要なときだけにすべきです。

---

### Q17. editor 内部の canonical SR は `target_sr` にするべきか、`out_sr` にするべきか

### A17.

`out_sr` ではなく、`source_sr` か `target_sr` です。

`out_sr` は device 依存で変わります。editor canonical は device に引っ張られるべきではありません。

おすすめ順は次です。

- pristine / no-op open: `source_sr`
- project 全体で canonical SR を持ちたい: `target_sr`
- `out_sr`: canonical にしない

---

### Q18. override 指定時でも最終 playback buffer 用だけ `out_sr` を作り、editor data は `target_sr` で保持する設計のほうが良いか

### A18.

はい。こちらの方が筋が良いです。

---

### Q19. bit-depth quantize は editor open 時点でやるべきか

### A19.

基本的にはやるべきではないです。

---

### Q20. quantize は export / apply の瞬間まで遅延したほうがよいか

### A20.

はい。これが推奨です。

Audacity も内部は 32-bit float を強く推奨し、重い編集や中間処理に対して float を使い、低 bit 深度への変換は export 側の話として説明しています。([Audacity Manual][7])

---

### Q21. editor internal buffer は常に `f32` のまま持ち、最後だけ quantize するのが妥当か

### A21.

妥当です。実務上これが標準的です。([Audacity Manual][7])

---

### Q22. pristine WAV の場合、この final exact buffer 自体を省略できるか

### A22.

条件付きで省略できます。

省略してよい条件の例:

- no-op open
- source が pristine WAV
- editor が source-backed random access で十分
- waveform は別 cache で持てる
- まだ destructive / offline effect を適用していない

---

### Q23. final exact buffer を省略できない場合、何のために持つべきか

### A23.

主な理由は次です。

- source 読み出しを毎回したくない
- 非同期 seek を速くしたい
- 後段 DSP が contiguous `f32` を要求する
- 編集結果を source から独立した materialized state として持ちたい

---

### Q24. waveform pyramid 生成のためだけなら、resample 済み full buffer を作らず source SR ベースで pyramid を作れないか

### A24.

作れます。むしろその方がよいです。

waveform min/max は「時間区間ごとの極値」なので、source frame domain で作っておき、表示時に time mapping するだけで十分です。display 用に `out_sr` buffer を先に作る必要はありません。

---

### Q25. editor の seek / selection / marker / loop 編集は source SR のままでも成立するか

### A25.

成立します。

おすすめは、

- clip-local position は `source_frame: u64`
- timeline 表示は `seconds = source_frame / source_sr`
- 別 SR へ投影するときだけ変換

です。

## 3 段階の推奨案

### Q26. 最小改修で `Finalizing exact audio` を短くするには

### A26.

優先順に次です。

1. no-op fast path を入れる
2. quantize を export/apply へ遅延
3. 2 段 resample をやめて 1 段にする
4. `rubato::SincFixedIn` ではなく `rubato::Fft` を試す
5. per-channel ではなく multi-channel 1 resampler に変える
6. 出力バッファを事前確保する

例:

```rust
let target = target_sr.map(|v| v.max(1));
let needs_resample = match target {
    Some(t) => in_sr != t || t != out_sr,
    None => in_sr != out_sr,
};
let needs_quantize = bit_depth.is_some();

if !needs_resample && !needs_quantize {
    return chans;
}
```

ただし設計上は、`out_sr` を editor finalize から外す方がもっと良いです。

---

### Q27. 中規模改修でボトルネックを減らすには

### A27.

1. `full_source_channels` をやめて「decode chunk -> final buffer」へ変える
2. waveform pyramid を decode 時に直接育てる
3. editor canonical SR と playback SR を分離する
4. resampler cache / pool を持つ

例:

```rust
struct EditorClip {
    source_sr: u32,
    editor_sr: u32,
    data: Arc<[Vec<f32>]>,
    waveform: WavePyramid,
    playback_cache: Option<PlaybackCache>,
}
```

---

### Q28. 設計を見直してでも最終的に目指すべき形は

### A28.

`source-backed pristine clip + lazy materialization + lazy playback cache` です。

イメージ:

```rust
enum ClipStorage {
    SourceBackedPristine {
        path: Arc<PathBuf>,
        source_sr: u32,
        channels: u16,
        waveform: WavePyramid,
    },
    MaterializedEditor {
        sr: u32,
        channels: Arc<[Vec<f32>]>,
        waveform: WavePyramid,
    },
    DerivedPlaybackCache {
        sr: u32,
        channels: Arc<[Vec<f32>]>,
    },
}
```

ポリシー:

- pristine WAV open 時
  - `SourceBackedPristine`
- 編集開始 / effect apply 時
  - `MaterializedEditor` を lazy に生成
- 再生時に device SR 不一致
  - `DerivedPlaybackCache` を lazy に生成、または streaming resample
- export 時
  - target SR / bit depth へ render

## 現実的に知りたい結論への回答

### Q29. 「resample は再生時に必須」なのか、「再生の都合でそうしているだけ」なのか

### A29.

再生の都合で必要なことが多いだけです。editor buffer 自体の必須条件ではありません。([Docs.rs][2])

---

### Q30. pristine WAV に限れば、final exact buffer を source SR のまま持つ設計が妥当か

### A30.

はい。かなり妥当です。むしろ第一候補にしてよいです。

## 現行コードへの具体的な批評

### Q31. いまの API / 実装でまず問題なのは何か

### A31.

問題は少なくとも 3 つあります。

#### 1. `target_sr` と `out_sr` が同じレイヤにいる

`process_editor_decode_channels()` に `out_sr` が入っている時点で、editor canonicalization と playback adaptation が混線しています。

分けるべき API 例:

```rust
fn finalize_editor_channels(
    chans: Vec<Vec<f32>>,
    in_sr: u32,
    editor_target_sr: Option<u32>,
) -> EditorAudio;

fn build_playback_cache(
    editor: &EditorAudio,
    playback_sr: u32,
    quality: ResampleQuality,
) -> PlaybackAudio;
```

#### 2. per-channel `Vec -> Vec` 置換

```rust
for c in chans.iter_mut() {
    *c = crate::wave::resample_quality(c, in_sr, out_sr, resample_quality);
}
```

これは

- channel ごとに独立処理
- output `Vec` を毎回新規確保
- multi-channel 一括最適化を使っていない

ので、かなり改善余地があります。

#### 3. `reserve(total.min(1_000_000))`

長尺では少なすぎます。source 全量を持つ設計を続けるなら、少なくとも `reserve_exact(total)` に近い方針の方が良いです。

## 実務的な優先順位

### Q32. まず 1 週間で何を変えるべきか

### A32.

- `quantize` を finalize から外す
- `in_sr == out_sr && target_sr.is_none()` は完全 skip
- `rubato::Fft` ベースの fixed-ratio offline resample を別実装で追加
- mono-per-channel resample をやめ、multi-channel 化
- timing を細かく計測

---

### Q33. その次に何をやるべきか

### A33.

- `out_sr` を editor finalize から外す
- waveform pyramid を source chunk から直接作る
- pristine WAV は source SR canonical にする

---

### Q34. 外部相談に出すなら、追加で何を添えるとよいか

### A34.

次の 4 つを足すと、回答の質がかなり上がります。

- 代表ケースの実測
  - 30 秒 / 5 分 / 60 分
  - mono / stereo
  - 44.1 -> 48, 48 -> 48
- `Finalizing exact audio` の内訳
  - resample / quantize / alloc / waveform
- メモリピーク
- `resample_quality()` の中で
  - resampler を毎回 `new` しているか
  - mono-only 実装か
  - `process_into_buffer` を使っているか

## 参考リンク

[1]: https://docs.rs/rubato
[2]: https://docs.rs/cpal/latest/cpal/traits/trait.DeviceTrait.html
[3]: https://docs.rs/rubato/latest/rubato/trait.Resampler.html
[4]: https://docs.rs/libsoxr
[5]: https://libsndfile.github.io/libsamplerate/quality.html
[6]: https://github.com/xiph/speexdsp/blob/master/include/speex/speex_resampler.h
[7]: https://manual.audacityteam.org/man/sample_format_bit_depth.html
