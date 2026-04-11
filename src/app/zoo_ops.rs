use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use image::AnimationDecoder;

use super::{WavesPreviewer, ZooFrameImage, ZooFrameTexture};

impl WavesPreviewer {
    pub(super) fn set_zoo_gif_path(&mut self, path: Option<PathBuf>) {
        self.zoo_gif_path = path;
        self.reload_zoo_gif_frames();
    }

    pub(super) fn set_zoo_voice_path(&mut self, path: Option<PathBuf>) {
        self.zoo_voice_path = path;
        self.zoo_voice_cache_path = None;
        self.zoo_voice_cache = None;
        self.zoo_last_error = None;
    }

    pub(super) fn reload_zoo_gif_frames(&mut self) {
        self.zoo_frames_raw.clear();
        self.zoo_frames_tex.clear();
        self.zoo_texture_gen = self.zoo_texture_gen.wrapping_add(1);
        self.zoo_anim_clock = 0.0;
        self.zoo_last_error = None;
        let frames_result = if let Some(path) = self.zoo_gif_path.clone() {
            decode_zoo_frames(&path)
        } else {
            decode_zoo_frames_from_bytes(
                crate::app::zoo_assets::DEFAULT_ZOO_GIF,
                crate::app::zoo_assets::DEFAULT_ZOO_GIF_NAME,
            )
        };
        match frames_result {
            Ok(frames) if !frames.is_empty() => {
                self.zoo_frames_raw = frames;
            }
            Ok(_) => {
                self.zoo_last_error = Some("Zoo image has no frames".to_string());
            }
            Err(err) => {
                self.zoo_last_error = Some(format!("Zoo image load failed: {err}"));
            }
        }
    }

    pub(super) fn ensure_zoo_textures(&mut self, ctx: &egui::Context) {
        if self.zoo_frames_raw.is_empty() {
            self.zoo_frames_tex.clear();
            return;
        }
        if self.zoo_frames_tex.len() == self.zoo_frames_raw.len() {
            return;
        }
        self.zoo_frames_tex.clear();
        for (idx, frame) in self.zoo_frames_raw.iter().enumerate() {
            let id = format!("zoo_anim_{}_{}", self.zoo_texture_gen, idx);
            let tex = ctx.load_texture(id, frame.image.clone(), egui::TextureOptions::LINEAR);
            self.zoo_frames_tex.push(ZooFrameTexture {
                texture: tex,
                delay_s: frame.delay_s.max(0.016),
            });
        }
    }

    pub(super) fn zoo_energy_level(&self) -> f32 {
        ((self.meter_db + 60.0) / 60.0).clamp(0.0, 1.0)
    }

    pub(super) fn play_zoo_voice(&mut self) {
        if !self.zoo_voice_enabled {
            return;
        }
        let use_embedded = self.zoo_voice_path.is_none();
        let requested_path = self.zoo_voice_path.clone();
        if self.zoo_voice_cache.is_none()
            || (!use_embedded && self.zoo_voice_cache_path != requested_path)
            || (use_embedded && self.zoo_voice_cache_path.is_some())
        {
            let decoded = if let Some(path) = requested_path.clone() {
                crate::audio_io::decode_audio_mono(&path)
            } else {
                decode_embedded_zoo_voice(
                    crate::app::zoo_assets::DEFAULT_ZOO_WAV,
                    crate::app::zoo_assets::DEFAULT_ZOO_WAV_NAME,
                )
            };
            match decoded {
                Ok((mut mono, src_sr)) => {
                    let out_sr = self.audio.shared.out_sample_rate.max(1);
                    if src_sr != out_sr {
                        mono = crate::wave::resample_quality(
                            &mono,
                            src_sr,
                            out_sr,
                            crate::wave::ResampleQuality::Best,
                        );
                    }
                    self.zoo_voice_cache =
                        Some(Arc::new(crate::audio::AudioBuffer::from_mono(mono)));
                    self.zoo_voice_cache_path = requested_path;
                }
                Err(err) => {
                    self.zoo_last_error = Some(format!("Zoo voice decode failed: {err}"));
                    return;
                }
            }
        }
        if self.zoo_voice_audio.is_none() {
            match crate::audio::AudioEngine::new() {
                Ok(engine) => self.zoo_voice_audio = Some(engine),
                Err(err) => {
                    self.zoo_last_error = Some(format!("Zoo voice output init failed: {err}"));
                    return;
                }
            }
        }
        if let (Some(engine), Some(buf)) = (&self.zoo_voice_audio, &self.zoo_voice_cache) {
            engine.stop();
            engine.set_samples_buffer(buf.clone());
            engine.play();
        }
    }
}

fn decode_zoo_frames(path: &Path) -> anyhow::Result<Vec<ZooFrameImage>> {
    let bytes = std::fs::read(path).with_context(|| format!("open image failed: {}", path.display()))?;
    decode_zoo_frames_from_bytes(&bytes, &path.display().to_string())
}

fn decode_zoo_frames_from_bytes(bytes: &[u8], source_label: &str) -> anyhow::Result<Vec<ZooFrameImage>> {
    let ext = source_label
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "gif" {
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .with_context(|| format!("gif decoder init failed: {source_label}"))?;
        let frames = decoder
            .into_frames()
            .collect_frames()
            .with_context(|| format!("gif frame decode failed: {source_label}"))?;
        let mut out = Vec::with_capacity(frames.len().min(128));
        for frame in frames.into_iter().take(128) {
            let (num, den) = frame.delay().numer_denom_ms();
            let delay_ms = if den == 0 {
                80.0
            } else {
                (num as f32 / den as f32).max(16.0)
            };
            let rgba = frame.into_buffer();
            let size = [rgba.width() as usize, rgba.height() as usize];
            out.push(ZooFrameImage {
                image: egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                delay_s: (delay_ms / 1000.0).clamp(0.016, 0.25),
            });
        }
        if out.is_empty() {
            anyhow::bail!("gif has no frames: {source_label}");
        }
        return Ok(out);
    }
    let image = image::load_from_memory(bytes)
        .with_context(|| format!("open image failed: {source_label}"))?;
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(vec![ZooFrameImage {
        image: egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
        delay_s: 0.08,
    }])
}

fn decode_embedded_zoo_voice(bytes: &[u8], source_label: &str) -> anyhow::Result<(Vec<f32>, u32)> {
    let reader = hound::WavReader::new(Cursor::new(bytes))
        .with_context(|| format!("open embedded zoo wav failed: {source_label}"))?;
    let spec = reader.spec();
    let sr = spec.sample_rate.max(1);
    let channels = spec.channels.max(1) as usize;
    let mono = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => {
            let samples = reader
                .into_samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("read embedded zoo wav samples failed: {source_label}"))?;
            mixdown_interleaved_f32(&samples, channels)
        }
        _ => {
            let reader = hound::WavReader::new(Cursor::new(bytes))
                .with_context(|| format!("reopen embedded zoo wav failed: {source_label}"))?;
            let samples = reader
                .into_samples::<i32>()
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("read embedded zoo wav samples failed: {source_label}"))?;
            let denom = match spec.bits_per_sample {
                0..=8 => 127.0,
                9..=16 => i16::MAX as f32,
                17..=24 => 8_388_607.0,
                _ => i32::MAX as f32,
            };
            let normalized = samples
                .into_iter()
                .map(|v| (v as f32 / denom).clamp(-1.0, 1.0))
                .collect::<Vec<_>>();
            mixdown_interleaved_f32(&normalized, channels)
        }
    };
    Ok((mono, sr))
}

fn mixdown_interleaved_f32(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let mut out = Vec::with_capacity(samples.len() / channels.max(1));
    for frame in samples.chunks(channels) {
        if frame.is_empty() {
            continue;
        }
        let sum = frame.iter().copied().sum::<f32>();
        out.push(sum / frame.len() as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn zoo_energy_mapping() {
        let map = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        assert_eq!(map(-80.0), 0.0);
        assert!(map(-30.0) > 0.0);
        assert_eq!(map(6.0), 1.0);
    }
}
