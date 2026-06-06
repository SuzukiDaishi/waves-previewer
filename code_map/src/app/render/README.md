# src/app/render

`src/app/render/` は波形、spectrogram、overlay、music feature などの描画補助を持ちます。
UI は `src/app/ui/editor.rs` から呼ばれることが多く、重い計算は `editor_viewport.rs` や `spectrogram_jobs.rs` の worker と合わせて追います。

## Render Map

| パス | 主な責務 | まず見る場面 |
|---|---|---|
| `src/app/render/waveform_pyramid.rs` | 長尺波形向けの pyramid / minmax 補助 | zoom/pan 時の波形描画負荷を下げたい |
| `src/app/render/spectrogram.rs` | spectrogram の描画補助 | spectrogram texture / tile 描画を追う |
| `src/app/render/overlay.rs` | waveform overlay / preview overlay | preview apply 前後の比較表示を見る |
| `src/app/render/music_features.rs` | music feature / chroma / tempogram 系描画 | music analysis 結果の可視化を見る |
| `src/app/render/binning.rs` | sample binning / downsample 補助 | 描画用集約の計算量を確認する |
| `src/app/render/colors.rs` | 描画色 / palette | waveform / spectrogram の色を調整する |

## Related Modules

- `src/app/ui/editor.rs`: canvas と render 呼び出しの主要入口。
- `src/app/editor_viewport.rs`: viewport 単位の画像生成 worker。
- `src/app/spectrogram_jobs.rs`: spectrogram 計算 job / tile result。
- `src/wave.rs`: min/max 波形や WAV 系 utility。
