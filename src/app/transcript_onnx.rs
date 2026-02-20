use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ort::{
    ep,
    session::Session,
    value::{DynValue, Value},
};
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tokenizers::Tokenizer;

use super::types::{
    Transcript, TranscriptAiConfig, TranscriptComputeTarget, TranscriptModelVariant,
    TranscriptSegment,
};

const WHISPER_SR: u32 = 16_000;
const CHUNK_SECONDS: usize = 30;
const N_FFT: usize = 400;
const HOP: usize = 160;
const PAD: usize = N_FFT / 2;
const N_SAMPLES: usize = (WHISPER_SR as usize) * CHUNK_SECONDS;
const TARGET_FRAMES: usize = N_SAMPLES / HOP;
const N_FREQ_BINS: usize = N_FFT / 2 + 1;
const SEGMENT_LANG_MIN_CONFIDENCE: f32 = 0.20;

#[derive(Debug, Deserialize, Default)]
struct ModelConfig {
    decoder_start_token_id: Option<i64>,
    eos_token_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct PreprocessorConfig {
    feature_size: Option<usize>,
    n_mels: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
struct GenerationConfig {
    suppress_tokens: Option<JsonValue>,
    begin_suppress_tokens: Option<JsonValue>,
    no_speech_threshold: Option<f32>,
    logprob_threshold: Option<f32>,
    lang_to_id: Option<HashMap<String, i64>>,
}

#[derive(Clone)]
struct TensorData {
    shape: Vec<usize>,
    data: Vec<f32>,
}

#[derive(Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct VadParams {
    threshold: f32,
    min_speech_duration_ms: usize,
    min_silence_duration_ms: usize,
    speech_pad_ms: usize,
}

struct ModelFiles {
    encoder_path: PathBuf,
    decoder_path: PathBuf,
    decoder_with_past_path: PathBuf,
    tokenizer_json_path: PathBuf,
    config_json_path: PathBuf,
    generation_config_path: Option<PathBuf>,
    preprocessor_config_path: Option<PathBuf>,
}

struct OnnxModel {
    encoder: Session,
    decoder: Session,
    decoder_with_past: Session,
    encoder_output_name: String,
    decoder_present_names: Vec<String>,
    decoder_with_past_input_names: Vec<String>,
    decoder_with_past_present_names: Vec<String>,
}

struct Preprocessor {
    n_mels: usize,
    window: Vec<f32>,
    mel_fb_t: Vec<f32>,
    fft: std::sync::Arc<dyn Fft<f32> + Send + Sync>,
}

struct SileroVad {
    session: Session,
    state: Vec<f32>,
    state_shape: Vec<usize>,
    context: Vec<f32>,
    sample_rate: i64,
    input_name: String,
    state_input_name: Option<String>,
    sr_input_name: Option<String>,
    output_name: String,
    state_output_name: Option<String>,
}

pub(super) struct WhisperOnnxTranscriber {
    model: OnnxModel,
    tokenizer: Tokenizer,
    tokenizer_json_path: PathBuf,
    eos_token_id: i64,
    cfg: TranscriptAiConfig,
    n_mels: usize,
    prompt_ids: Vec<i64>,
    suppress_ids: Vec<i64>,
    begin_ids: Vec<i64>,
    no_speech_threshold: Option<f32>,
    logprob_threshold: Option<f32>,
    nospeech_id: Option<i64>,
    language_token_ids: Vec<(usize, String)>,
    forced_language: Option<String>,
    preproc: Preprocessor,
    vad: Option<SileroVad>,
}

pub(super) struct TranscriptRunOutput {
    pub srt_path: PathBuf,
    pub detected_language: Option<String>,
}

#[derive(Clone, Debug)]
struct LanguageEstimate {
    code: String,
    confidence: f32,
}

impl WhisperOnnxTranscriber {
    pub(super) fn load(model_dir: &Path, cfg: TranscriptAiConfig) -> Result<Self, String> {
        let files = resolve_model_files(model_dir, cfg.model_variant)?;
        let model_cfg: ModelConfig = read_json(&files.config_json_path)?;
        let gen_cfg: GenerationConfig = files
            .generation_config_path
            .as_ref()
            .map(|p| read_json(p.as_path()))
            .transpose()?
            .unwrap_or_default();
        let pre_cfg: PreprocessorConfig = files
            .preprocessor_config_path
            .as_ref()
            .map(|p| read_json(p.as_path()))
            .transpose()?
            .unwrap_or_default();

        let n_mels = pre_cfg.feature_size.or(pre_cfg.n_mels).unwrap_or(128);
        let decoder_start_token_id = model_cfg.decoder_start_token_id.unwrap_or(50258);
        let eos_token_id = model_cfg.eos_token_id.unwrap_or(50257);
        let suppress_ids = parse_token_list(gen_cfg.suppress_tokens.as_ref());
        let begin_ids = {
            let v = parse_token_list(gen_cfg.begin_suppress_tokens.as_ref());
            if v.is_empty() {
                vec![220, 50256]
            } else {
                v
            }
        };

        let tokenizer = Tokenizer::from_file(&files.tokenizer_json_path)
            .map_err(|e| format!("Tokenizer load failed: {e}"))?;
        let no_speech_threshold = cfg.no_speech_threshold.or(gen_cfg.no_speech_threshold);
        let logprob_threshold = cfg.logprob_threshold.or(gen_cfg.logprob_threshold);
        let token_map = load_token_map(&files.tokenizer_json_path)?;
        let nospeech_id = token_map.get("<|nospeech|>").copied();
        let language_token_ids = language_token_candidates(&gen_cfg, &token_map);
        let forced_language = if cfg.language.trim().eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(cfg.language.trim().to_ascii_lowercase())
        };
        let mut prompt_ids = build_prompt_ids(
            &tokenizer,
            cfg.language.trim(),
            cfg.task.trim(),
            cfg.omit_language_token,
            cfg.omit_notimestamps_token,
        )?;
        if prompt_ids.is_empty() {
            prompt_ids.push(decoder_start_token_id);
        } else if prompt_ids[0] != decoder_start_token_id {
            prompt_ids.insert(0, decoder_start_token_id);
        }

        let mut planner = FftPlanner::<f32>::new();
        let preproc = Preprocessor {
            n_mels,
            window: hann_periodic(N_FFT),
            mel_fb_t: build_mel_filterbank_t(WHISPER_SR as f32, N_FFT, n_mels, 0.0, 8000.0)?,
            fft: planner.plan_fft_forward(N_FFT),
        };

        let vad = if cfg.vad_enabled {
            cfg.vad_model_path
                .as_ref()
                .map(|path| SileroVad::new(path, WHISPER_SR as i64))
                .transpose()?
        } else {
            None
        };

        Ok(Self {
            model: OnnxModel::load(&files, &cfg)?,
            tokenizer,
            tokenizer_json_path: files.tokenizer_json_path,
            eos_token_id,
            cfg,
            n_mels,
            prompt_ids,
            suppress_ids,
            begin_ids,
            no_speech_threshold,
            logprob_threshold,
            nospeech_id,
            language_token_ids,
            forced_language,
            preproc,
            vad,
        })
    }

    pub(super) fn transcribe_to_srt(
        &mut self,
        audio_path: &Path,
    ) -> Result<TranscriptRunOutput, String> {
        let Some(base_srt_path) = super::transcript::srt_path_for_audio(audio_path) else {
            return Err("Could not resolve .srt output path.".to_string());
        };
        if !self.cfg.overwrite_existing_srt && base_srt_path.exists() {
            // Default policy: when an .srt already exists, skip regeneration and keep it.
            return Ok(TranscriptRunOutput {
                srt_path: base_srt_path,
                detected_language: self.forced_language.clone(),
            });
        }
        let srt_path = base_srt_path;
        let (mono, src_sr) = crate::audio_io::decode_audio_mono(audio_path)
            .map_err(|e| format!("Audio decode failed: {e}"))?;
        let audio_16k = if src_sr == WHISPER_SR {
            mono
        } else {
            resample_linear(&mono, src_sr, WHISPER_SR)
        };
        if audio_16k.is_empty() {
            return Err("Audio decode returned no samples.".to_string());
        }

        let max_window_ms = self.cfg.max_window_ms.clamp(1_000, 30_000);
        let max_window_samples = ((WHISPER_SR as usize) * max_window_ms / 1000).max(1);
        let mut windows = if self.cfg.vad_enabled {
            match self.vad.as_mut() {
                Some(vad) => match detect_speech_segments(
                    vad,
                    &audio_16k,
                    VadParams {
                        threshold: self.cfg.vad_threshold.clamp(0.01, 0.99),
                        min_speech_duration_ms: self.cfg.vad_min_speech_ms.clamp(10, 10_000),
                        min_silence_duration_ms: self.cfg.vad_min_silence_ms.clamp(10, 10_000),
                        speech_pad_ms: self.cfg.vad_speech_pad_ms.clamp(0, 5_000),
                    },
                    WHISPER_SR as usize,
                ) {
                    Ok(v) => v,
                    Err(_) => vec![Segment {
                        start: 0,
                        end: audio_16k.len(),
                    }],
                },
                None => vec![Segment {
                    start: 0,
                    end: audio_16k.len(),
                }],
            }
        } else {
            vec![Segment {
                start: 0,
                end: audio_16k.len(),
            }]
        };
        if windows.is_empty() {
            windows.push(Segment {
                start: 0,
                end: audio_16k.len(),
            });
        }
        let mut split = Vec::new();
        for seg in windows {
            split.extend(split_segment(seg, max_window_samples));
        }

        let mut segments = Vec::<TranscriptSegment>::new();
        let mut detected_language = self.forced_language.clone();
        let mut language_scores: HashMap<String, f32> = HashMap::new();
        for seg in split {
            if seg.end <= seg.start {
                continue;
            }
            let chunk = &audio_16k[seg.start..seg.end];
            if chunk.is_empty() {
                continue;
            }
            let features = log_mel_whisper_like(&self.preproc, &chunk)?;
            let (text, maybe_lang) = self.decode_segment(features)?;
            if let Some(est) = maybe_lang {
                if est.confidence < SEGMENT_LANG_MIN_CONFIDENCE {
                    continue;
                }
                let duration_sec = (seg.end.saturating_sub(seg.start) as f32) / (WHISPER_SR as f32);
                let weight = duration_sec.clamp(0.5, 8.0);
                *language_scores.entry(est.code).or_insert(0.0) +=
                    est.confidence.max(0.001) * weight;
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let start_smp = seg.start;
            let end_smp = seg.end.min(audio_16k.len());
            let start_ms = ((start_smp as f64 / WHISPER_SR as f64) * 1000.0).round() as u64;
            let end_ms = ((end_smp as f64 / WHISPER_SR as f64) * 1000.0).round() as u64;
            segments.push(TranscriptSegment {
                start_ms,
                end_ms: end_ms.max(start_ms + 1),
                text: trimmed.to_string(),
            });
        }
        if detected_language.is_none() {
            detected_language = language_scores
                .into_iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(code, _)| code);
        }

        let full_text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(script_lang) = infer_language_from_text_script(&full_text) {
            if detected_language.as_deref() != Some(script_lang.as_str()) {
                detected_language = Some(script_lang);
            }
        }
        let transcript = Transcript {
            segments,
            full_text,
        };
        super::transcript::write_srt(&srt_path, &transcript)
            .map_err(|e| format!("SRT write failed: {e}"))?;
        Ok(TranscriptRunOutput {
            srt_path,
            detected_language,
        })
    }

    fn decode_segment(
        &mut self,
        feature_data: Vec<f32>,
    ) -> Result<(String, Option<LanguageEstimate>), String> {
        let forced_language = self.forced_language.clone();
        let language_token_ids = self.language_token_ids.clone();
        let prompt_ids = self.prompt_ids.clone();
        let feat_shape = vec![1usize, self.n_mels, TARGET_FRAMES];
        let v_feat = Value::from_array((feat_shape, feature_data))
            .map_err(|e| format!("ort feature value error: {e}"))?;
        let enc_out = self
            .model
            .encoder
            .run([(&v_feat).into()])
            .map_err(|e| format!("encoder run failed: {e}"))?;
        let (hs_shape, hs_data) = enc_out[self.model.encoder_output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("encoder output parse failed: {e}"))?;
        let hs_dims: Vec<usize> = hs_shape.as_ref().iter().map(|&d| d as usize).collect();
        if hs_dims.len() != 3 {
            return Err(format!("unexpected encoder output shape: {hs_dims:?}"));
        }
        let hidden_shape = hs_dims.clone();
        let hidden_data = hs_data.to_vec();

        let v_ids = Value::from_array((vec![1usize, prompt_ids.len()], prompt_ids.clone()))
            .map_err(|e| format!("prompt ids value error: {e}"))?;
        let v_hidden = Value::from_array((hidden_shape.clone(), hidden_data.clone()))
            .map_err(|e| format!("hidden value error: {e}"))?;
        let dec_out = self
            .model
            .decoder
            .run([(&v_ids).into(), (&v_hidden).into()])
            .map_err(|e| format!("decoder run failed: {e}"))?;

        let mut present: HashMap<String, TensorData> = HashMap::new();
        for n in &self.model.decoder_present_names {
            let (s, d) = dec_out[n.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("decoder present parse failed ({n}): {e}"))?;
            present.insert(
                n.clone(),
                TensorData {
                    shape: s.as_ref().iter().map(|&x| x as usize).collect(),
                    data: d.to_vec(),
                },
            );
        }

        let mut logits = last_logits(&dec_out["logits"])?;
        let detected_language = Self::detect_language_from_logits(
            forced_language.as_ref(),
            &language_token_ids,
            &logits,
        );
        apply_suppression(&mut logits, &self.suppress_ids, &self.begin_ids, 0);
        let first_lp = log_softmax(&logits);
        let no_speech_prob = self
            .nospeech_id
            .and_then(|id| first_lp.get(id as usize).copied())
            .map(f32::exp);
        let mut ids = prompt_ids;
        let mut next = argmax(&logits) as i64;
        let mut sum_lp = first_lp.get(next as usize).copied().unwrap_or(0.0) as f64;
        let mut gen_count = 1usize;
        ids.push(next);

        let max_new_tokens = self.cfg.max_new_tokens.clamp(1, 512);
        for step in 1..max_new_tokens {
            if next == self.eos_token_id {
                break;
            }
            let mut run_inputs: Vec<(String, DynValue)> =
                Vec::with_capacity(self.model.decoder_with_past_input_names.len());
            run_inputs.push((
                "input_ids".to_string(),
                Value::from_array((vec![1usize, 1usize], vec![next]))
                    .map_err(|e| format!("next token value error: {e}"))?
                    .into(),
            ));
            for input_name in &self.model.decoder_with_past_input_names {
                if input_name == "input_ids" {
                    continue;
                }
                if input_name == "encoder_hidden_states" {
                    run_inputs.push((
                        input_name.clone(),
                        Value::from_array((hidden_shape.clone(), hidden_data.clone()))
                            .map_err(|e| format!("hidden value error: {e}"))?
                            .into(),
                    ));
                    continue;
                }
                if input_name == "use_cache_branch" {
                    run_inputs.push((
                        input_name.clone(),
                        Value::from_array((Vec::<usize>::new(), vec![true]))
                            .map_err(|e| format!("use_cache value error: {e}"))?
                            .into(),
                    ));
                    continue;
                }
                let present_name = map_past_to_present_name(input_name);
                let t = present
                    .get(&present_name)
                    .ok_or_else(|| format!("missing cached tensor: {present_name}"))?;
                run_inputs.push((
                    input_name.clone(),
                    Value::from_array((t.shape.clone(), t.data.clone()))
                        .map_err(|e| format!("past tensor value error: {e}"))?
                        .into(),
                ));
            }

            let out = self
                .model
                .decoder_with_past
                .run(run_inputs)
                .map_err(|e| format!("decoder_with_past run failed: {e}"))?;

            let mut next_present = present.clone();
            for n in &self.model.decoder_with_past_present_names {
                let (s, d) = out[n.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("decoder_with_past present parse failed ({n}): {e}"))?;
                next_present.insert(
                    n.clone(),
                    TensorData {
                        shape: s.as_ref().iter().map(|&x| x as usize).collect(),
                        data: d.to_vec(),
                    },
                );
            }
            present = next_present;

            let mut l = last_logits(&out["logits"])?;
            apply_suppression(&mut l, &self.suppress_ids, &self.begin_ids, step);
            let lp = log_softmax(&l);
            next = argmax(&l) as i64;
            sum_lp += lp.get(next as usize).copied().unwrap_or(0.0) as f64;
            gen_count += 1;
            ids.push(next);
        }

        let avg_lp = (sum_lp / (gen_count as f64)) as f32;
        if let Some(threshold) = self.no_speech_threshold {
            let no_speech_hit = no_speech_prob.unwrap_or(0.0) > threshold;
            let low_conf = self
                .logprob_threshold
                .map(|lp_threshold| avg_lp < lp_threshold)
                .unwrap_or(true);
            if no_speech_hit && low_conf {
                return Ok((String::new(), detected_language));
            }
        }

        let decode_ids: Vec<u32> = ids
            .into_iter()
            .filter(|id| *id >= 0)
            .map(|id| id as u32)
            .collect();
        let decoded = self.tokenizer.decode(&decode_ids, true).map_err(|e| {
            format!(
                "token decode failed ({}): {e}",
                self.tokenizer_json_path.display()
            )
        })?;
        Ok((decoded, detected_language))
    }

    fn detect_language_from_logits(
        forced_language: Option<&String>,
        language_token_ids: &[(usize, String)],
        logits: &[f32],
    ) -> Option<LanguageEstimate> {
        if let Some(lang) = forced_language {
            return Some(LanguageEstimate {
                code: lang.clone(),
                confidence: 1.0,
            });
        }
        if language_token_ids.is_empty() {
            return None;
        }
        let mut candidates = Vec::<(String, f32)>::new();
        let mut max_logit = f32::NEG_INFINITY;
        for (id, code) in language_token_ids {
            let Some(score) = logits.get(*id).copied() else {
                continue;
            };
            if score > max_logit {
                max_logit = score;
            }
            candidates.push((code.clone(), score));
        }
        if candidates.is_empty() {
            return None;
        }
        let mut sum = 0.0f32;
        for (_, score) in &mut candidates {
            let v = (*score - max_logit).exp();
            *score = v;
            sum += v;
        }
        if sum <= 0.0 || !sum.is_finite() {
            return None;
        }
        candidates
            .into_iter()
            .map(|(code, v)| LanguageEstimate {
                code,
                confidence: (v / sum).clamp(0.0, 1.0),
            })
            .max_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

fn infer_language_from_text_script(text: &str) -> Option<String> {
    let mut hiragana_katakana = 0usize;
    let mut hangul = 0usize;
    let mut thai = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3040..=0x30FF).contains(&cp) {
            hiragana_katakana = hiragana_katakana.saturating_add(1);
            continue;
        }
        if (0xAC00..=0xD7AF).contains(&cp) {
            hangul = hangul.saturating_add(1);
            continue;
        }
        if (0x0E00..=0x0E7F).contains(&cp) {
            thai = thai.saturating_add(1);
        }
    }
    if hiragana_katakana >= 2 {
        return Some("ja".to_string());
    }
    if hangul >= 2 {
        return Some("ko".to_string());
    }
    if thai >= 2 {
        return Some("th".to_string());
    }
    None
}

impl OnnxModel {
    fn load(files: &ModelFiles, cfg: &TranscriptAiConfig) -> Result<Self, String> {
        let encoder = commit_session(&files.encoder_path, cfg, "encoder")?;
        let decoder = commit_session(&files.decoder_path, cfg, "decoder")?;
        let decoder_with_past =
            commit_session(&files.decoder_with_past_path, cfg, "decoder_with_past")?;

        let encoder_output_name = encoder
            .outputs()
            .iter()
            .find(|o| o.name() == "encoder_hidden_states" || o.name() == "last_hidden_state")
            .map(|o| o.name().to_string())
            .or_else(|| encoder.outputs().first().map(|o| o.name().to_string()))
            .ok_or_else(|| "encoder session has no outputs".to_string())?;
        let decoder_present_names: Vec<String> = decoder
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .filter(|n| n.starts_with("present."))
            .collect();
        if decoder_present_names.is_empty() {
            return Err("decoder session has no present.* outputs".to_string());
        }
        let decoder_with_past_input_names: Vec<String> = decoder_with_past
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let decoder_with_past_present_names: Vec<String> = decoder_with_past
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .filter(|n| n.starts_with("present."))
            .collect();

        Ok(Self {
            encoder,
            decoder,
            decoder_with_past,
            encoder_output_name,
            decoder_present_names,
            decoder_with_past_input_names,
            decoder_with_past_present_names,
        })
    }
}

fn commit_session(path: &Path, cfg: &TranscriptAiConfig, label: &str) -> Result<Session, String> {
    let providers = build_execution_providers(cfg);
    let first_try = commit_session_with_providers(path, cfg, label, providers);
    if first_try.is_ok() {
        return first_try;
    }
    if matches!(cfg.compute_target, TranscriptComputeTarget::Cpu) {
        return first_try;
    }

    // Keep minimum-works-first: if accelerator path fails, force CPU and retry once.
    let cpu_only = vec![ep::CPU::default().build().fail_silently()];
    match commit_session_with_providers(path, cfg, label, cpu_only) {
        Ok(session) => Ok(session),
        Err(cpu_err) => {
            let accel_err = first_try.err().unwrap_or_default();
            Err(format!("{accel_err}; CPU fallback failed: {cpu_err}"))
        }
    }
}

fn commit_session_with_providers(
    path: &Path,
    cfg: &TranscriptAiConfig,
    label: &str,
    providers: Vec<ep::ExecutionProviderDispatch>,
) -> Result<Session, String> {
    let mut builder = Session::builder().map_err(|e| format!("ORT {label} builder failed: {e}"))?;
    builder = builder
        .with_parallel_execution(false)
        .map_err(|e| format!("ORT {label} parallel config failed: {e}"))?;
    builder = builder
        .with_inter_threads(1)
        .map_err(|e| format!("ORT {label} inter-thread config failed: {e}"))?;

    let cpu_threads = if cfg.cpu_intra_threads > 0 {
        cfg.cpu_intra_threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get().clamp(1, 8))
            .unwrap_or(1)
    };
    builder = builder
        .with_intra_threads(cpu_threads)
        .map_err(|e| format!("ORT {label} intra-thread config failed: {e}"))?;

    if !providers.is_empty() {
        builder = builder
            .with_execution_providers(providers)
            .map_err(|e| format!("ORT {label} execution provider setup failed: {e}"))?;
    }

    builder
        .commit_from_file(path)
        .map_err(|e| format!("ORT {label} load failed: {e}"))
}

fn build_execution_providers(cfg: &TranscriptAiConfig) -> Vec<ep::ExecutionProviderDispatch> {
    let mut out = Vec::new();
    let dml = ep::DirectML::default()
        .with_device_id(cfg.dml_device_id.max(0))
        .build()
        .fail_silently();
    let cpu = ep::CPU::default().build().fail_silently();

    match cfg.compute_target {
        TranscriptComputeTarget::Auto => {
            #[cfg(windows)]
            {
                out.push(dml.clone());
            }
            out.push(cpu);
        }
        TranscriptComputeTarget::Cpu => {
            out.push(cpu);
        }
        TranscriptComputeTarget::Gpu => {
            #[cfg(windows)]
            {
                out.push(dml);
            }
            out.push(cpu);
        }
        TranscriptComputeTarget::Npu => {
            // Windows desktop: prefer DirectML as the safest NPU-adjacent runtime path.
            // (QNN EP is platform/backend specific; keep CPU fallback for guaranteed execution.)
            #[cfg(windows)]
            {
                out.push(dml);
            }
            out.push(cpu);
        }
    }
    out
}

fn resolve_model_files(
    model_dir: &Path,
    variant: TranscriptModelVariant,
) -> Result<ModelFiles, String> {
    let onnx_dir = model_dir.join("onnx");
    if !onnx_dir.is_dir() {
        return Err(format!("onnx directory not found: {}", onnx_dir.display()));
    }
    let (encoder_candidates, decoder_candidates, decoder_past_candidates) =
        onnx_candidates(variant);
    Ok(ModelFiles {
        encoder_path: select_local_onnx_path(&onnx_dir, &encoder_candidates)?,
        decoder_path: select_local_onnx_path(&onnx_dir, &decoder_candidates)?,
        decoder_with_past_path: select_local_onnx_path(&onnx_dir, &decoder_past_candidates)?,
        tokenizer_json_path: find_model_file(model_dir, &["tokenizer.json"])?,
        config_json_path: find_model_file(model_dir, &["config.json"])?,
        generation_config_path: find_model_file(model_dir, &["generation_config.json"]).ok(),
        preprocessor_config_path: find_model_file(model_dir, &["preprocessor_config.json"]).ok(),
    })
}

type OnnxCandidate<'a> = &'a str;

fn onnx_candidates(
    variant: TranscriptModelVariant,
) -> (
    Vec<OnnxCandidate<'static>>,
    Vec<OnnxCandidate<'static>>,
    Vec<OnnxCandidate<'static>>,
) {
    match variant {
        TranscriptModelVariant::Auto => (
            vec![
                "encoder_model.onnx",
                "encoder_model_fp16.onnx",
                "encoder_model_quantized.onnx",
            ],
            vec![
                "decoder_model.onnx",
                "decoder_model_fp16.onnx",
                "decoder_model_quantized.onnx",
            ],
            vec![
                "decoder_with_past_model.onnx",
                "decoder_with_past_model_fp16.onnx",
                "decoder_with_past_model_quantized.onnx",
            ],
        ),
        TranscriptModelVariant::Fp16 => (
            vec!["encoder_model_fp16.onnx"],
            vec!["decoder_model_fp16.onnx"],
            vec!["decoder_with_past_model_fp16.onnx"],
        ),
        TranscriptModelVariant::Quantized => (
            vec!["encoder_model_quantized.onnx"],
            vec!["decoder_model_quantized.onnx"],
            vec!["decoder_with_past_model_quantized.onnx"],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{infer_language_from_text_script, onnx_candidates, TranscriptModelVariant};

    #[test]
    fn fp16_variant_uses_only_fp16_models() {
        let (enc, dec, past) = onnx_candidates(TranscriptModelVariant::Fp16);
        assert_eq!(enc, vec!["encoder_model_fp16.onnx"]);
        assert_eq!(dec, vec!["decoder_model_fp16.onnx"]);
        assert_eq!(past, vec!["decoder_with_past_model_fp16.onnx"]);
    }

    #[test]
    fn quantized_variant_uses_only_quantized_models() {
        let (enc, dec, past) = onnx_candidates(TranscriptModelVariant::Quantized);
        assert_eq!(enc, vec!["encoder_model_quantized.onnx"]);
        assert_eq!(dec, vec!["decoder_model_quantized.onnx"]);
        assert_eq!(past, vec!["decoder_with_past_model_quantized.onnx"]);
    }

    #[test]
    fn script_hint_prefers_japanese_when_kana_exists() {
        let hint = infer_language_from_text_script("これはテストです。音声の確認をします。");
        assert_eq!(hint.as_deref(), Some("ja"));
    }

    #[test]
    fn script_hint_prefers_korean_for_hangul() {
        let hint = infer_language_from_text_script("안녕하세요 테스트 문장입니다");
        assert_eq!(hint.as_deref(), Some("ko"));
    }

    #[test]
    fn script_hint_is_none_for_latin_only() {
        let hint = infer_language_from_text_script("this is a simple english sentence");
        assert_eq!(hint, None);
    }
}

fn find_model_file(root: &Path, candidates: &[&str]) -> Result<PathBuf, String> {
    for rel in candidates {
        let p = root.join(rel);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "missing model file under {} (checked: {})",
        root.display(),
        candidates.join(", ")
    ))
}

fn select_local_onnx_path(onnx_dir: &Path, candidates: &[&str]) -> Result<PathBuf, String> {
    for name in candidates {
        let p = onnx_dir.join(name);
        if !p.is_file() {
            continue;
        }
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(format!(
        "missing ONNX file under {} (checked: {})",
        onnx_dir.display(),
        candidates.join(", ")
    ))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid JSON {}: {e}", path.display()))
}

fn parse_token_list(v: Option<&JsonValue>) -> Vec<i64> {
    match v {
        None => vec![],
        Some(JsonValue::Number(n)) => n.as_i64().filter(|x| *x >= 0).into_iter().collect(),
        Some(JsonValue::Array(a)) => a.iter().filter_map(|x| x.as_i64()).collect(),
        _ => vec![],
    }
}

fn build_prompt_ids(
    tokenizer: &Tokenizer,
    language: &str,
    task: &str,
    omit_language_token: bool,
    omit_notimestamps_token: bool,
) -> Result<Vec<i64>, String> {
    let mut ids = Vec::with_capacity(4);
    ids.push(token_id(tokenizer, "<|startoftranscript|>")?);
    let language_is_auto =
        language.trim().is_empty() || language.trim().eq_ignore_ascii_case("auto");
    if !omit_language_token && !language_is_auto {
        ids.push(token_id(tokenizer, &format!("<|{}|>", language))?);
    }
    ids.push(token_id(tokenizer, &format!("<|{}|>", task))?);
    if !omit_notimestamps_token {
        ids.push(token_id(tokenizer, "<|notimestamps|>")?);
    }
    Ok(ids)
}

fn token_id(tokenizer: &Tokenizer, token: &str) -> Result<i64, String> {
    tokenizer
        .token_to_id(token)
        .map(|id| id as i64)
        .ok_or_else(|| format!("token not found in tokenizer: {token}"))
}

fn load_token_map(path: &Path) -> Result<HashMap<String, i64>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let json: JsonValue =
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON {}: {e}", path.display()))?;
    let mut map = HashMap::new();
    if let Some(vocab) = json
        .get("model")
        .and_then(|m| m.get("vocab"))
        .and_then(|v| v.as_object())
    {
        for (k, v) in vocab {
            if let Some(id) = v.as_i64() {
                map.insert(k.clone(), id);
            }
        }
    }
    if let Some(added) = json.get("added_tokens").and_then(|v| v.as_array()) {
        for item in added {
            if let (Some(c), Some(id)) = (
                item.get("content").and_then(|x| x.as_str()),
                item.get("id").and_then(|x| x.as_i64()),
            ) {
                map.insert(c.to_string(), id);
            }
        }
    }
    Ok(map)
}

fn normalize_special_token_code(token: &str) -> Option<String> {
    let raw = token.trim();
    let inner = raw
        .strip_prefix("<|")
        .and_then(|v| v.strip_suffix("|>"))
        .unwrap_or(raw)
        .trim();
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_ascii_lowercase())
}

fn is_language_code(code: &str) -> bool {
    if code.len() < 2 || code.len() > 12 {
        return false;
    }
    let reserved = [
        "startoftranscript",
        "transcribe",
        "translate",
        "notimestamps",
        "nospeech",
        "endoftext",
        "startofprev",
        "startoflm",
    ];
    if reserved.contains(&code) {
        return false;
    }
    let mut chars = code.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    let mut letter_count = 1usize;
    let mut prev_hyphen = false;
    for c in chars {
        if c == '-' {
            if prev_hyphen {
                return false;
            }
            prev_hyphen = true;
            continue;
        }
        prev_hyphen = false;
        if c.is_ascii_lowercase() {
            letter_count += 1;
            continue;
        }
        if c.is_ascii_digit() {
            continue;
        }
        return false;
    }
    !prev_hyphen && letter_count >= 2
}

fn language_token_candidates(
    cfg: &GenerationConfig,
    token_map: &HashMap<String, i64>,
) -> Vec<(usize, String)> {
    let mut out = Vec::<(usize, String)>::new();
    if let Some(lang_to_id) = cfg.lang_to_id.as_ref() {
        for (token, id) in lang_to_id {
            if *id < 0 {
                continue;
            }
            if let Some(code) = normalize_special_token_code(token).filter(|v| is_language_code(v))
            {
                out.push((*id as usize, code));
            }
        }
    }
    if out.is_empty() {
        for (token, id) in token_map {
            if *id < 0 {
                continue;
            }
            if let Some(code) = normalize_special_token_code(token).filter(|v| is_language_code(v))
            {
                out.push((*id as usize, code));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    out
}

impl SileroVad {
    fn new(model_path: &Path, sample_rate: i64) -> Result<Self, String> {
        let session = Session::builder()
            .map_err(|e| format!("VAD builder failed: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("VAD load failed ({}): {e}", model_path.display()))?;
        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();

        let input_name = input_names
            .iter()
            .find(|n| {
                let k = n.to_ascii_lowercase();
                !k.contains("state") && !k.contains("sr")
            })
            .cloned()
            .or_else(|| input_names.first().cloned())
            .ok_or_else(|| "VAD has no input tensor".to_string())?;
        let state_input_name = input_names
            .iter()
            .find(|n| n.to_ascii_lowercase().contains("state"))
            .cloned();
        let sr_input_name = input_names
            .iter()
            .find(|n| {
                let lower = n.to_ascii_lowercase();
                lower == "sr"
                    || lower.contains("sample")
                    || lower.contains("rate")
                    || lower.ends_with("_sr")
            })
            .cloned();

        let state_output_name = output_names
            .iter()
            .find(|n| n.to_ascii_lowercase().contains("state"))
            .cloned();
        let output_name = output_names
            .iter()
            .find(|n| {
                state_output_name
                    .as_ref()
                    .map(|state| *n != state)
                    .unwrap_or(true)
            })
            .cloned()
            .or_else(|| output_names.first().cloned())
            .ok_or_else(|| "VAD has no output tensor".to_string())?;

        Ok(Self {
            session,
            state: vec![0.0; 2 * 1 * 128],
            state_shape: vec![2, 1, 128],
            context: vec![0.0; 64],
            sample_rate,
            input_name,
            state_input_name,
            sr_input_name,
            output_name,
            state_output_name,
        })
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.state_shape = vec![2, 1, 128];
        self.context.fill(0.0);
    }

    fn predict(&mut self, frame: &[f32]) -> Result<f32, String> {
        let mut combined = Vec::with_capacity(self.context.len() + frame.len());
        combined.extend_from_slice(&self.context);
        combined.extend_from_slice(frame);

        let v_in = Value::from_array((vec![1usize, combined.len()], combined))
            .map_err(|e| format!("VAD input tensor error: {e}"))?;
        let mut run_inputs: Vec<(String, DynValue)> = vec![(self.input_name.clone(), v_in.into())];
        if let Some(state_input_name) = self.state_input_name.as_ref() {
            let v_state = Value::from_array((self.state_shape.clone(), self.state.clone()))
                .map_err(|e| format!("VAD state tensor error: {e}"))?;
            run_inputs.push((state_input_name.clone(), v_state.into()));
        }
        if let Some(sr_input_name) = self.sr_input_name.as_ref() {
            let v_sr = Value::from_array((vec![1usize], vec![self.sample_rate]))
                .map_err(|e| format!("VAD sr tensor error: {e}"))?;
            run_inputs.push((sr_input_name.clone(), v_sr.into()));
        }
        let outputs = self
            .session
            .run(run_inputs)
            .map_err(|e| format!("VAD run failed: {e}"))?;

        if let Some(state_output_name) = self.state_output_name.as_ref() {
            let (state_shape, state_data) = outputs[state_output_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("VAD state output parse failed: {e}"))?;
            self.state_shape = state_shape.as_ref().iter().map(|&d| d as usize).collect();
            self.state = state_data.to_vec();
        }

        if frame.len() >= 64 {
            self.context.copy_from_slice(&frame[frame.len() - 64..]);
        } else {
            self.context.fill(0.0);
            let offset = 64 - frame.len();
            self.context[offset..].copy_from_slice(frame);
        }

        let (_, probs) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("VAD probability parse failed: {e}"))?;
        probs
            .first()
            .copied()
            .ok_or_else(|| "VAD returned empty output".to_string())
    }
}

fn detect_speech_segments(
    vad: &mut SileroVad,
    audio: &[f32],
    params: VadParams,
    sample_rate: usize,
) -> Result<Vec<Segment>, String> {
    if audio.is_empty() {
        return Ok(Vec::new());
    }
    let frame = 512usize;
    let min_speech = sample_rate * params.min_speech_duration_ms / 1000;
    let min_silence = sample_rate * params.min_silence_duration_ms / 1000;
    let speech_pad = sample_rate * params.speech_pad_ms / 1000;
    let neg_threshold = (params.threshold - 0.15).max(0.01);

    vad.reset();
    let mut probs = Vec::new();
    for start in (0..audio.len()).step_by(frame) {
        let end = (start + frame).min(audio.len());
        let mut block = [0.0_f32; 512];
        block[..(end - start)].copy_from_slice(&audio[start..end]);
        probs.push(vad.predict(&block)?);
    }

    let mut segs = Vec::<Segment>::new();
    let mut triggered = false;
    let mut temp_end = 0usize;
    let mut cur_start = 0usize;
    for (idx, prob) in probs.into_iter().enumerate() {
        let cur = idx * frame;
        if prob >= params.threshold {
            if !triggered {
                triggered = true;
                cur_start = cur;
            }
            temp_end = 0;
            continue;
        }
        if !triggered {
            continue;
        }
        if prob < neg_threshold {
            if temp_end == 0 {
                temp_end = cur;
            }
            if cur.saturating_sub(temp_end) >= min_silence {
                if temp_end.saturating_sub(cur_start) >= min_speech {
                    segs.push(Segment {
                        start: cur_start,
                        end: temp_end,
                    });
                }
                triggered = false;
                temp_end = 0;
            }
        } else {
            temp_end = 0;
        }
    }
    if triggered && audio.len().saturating_sub(cur_start) >= min_speech {
        segs.push(Segment {
            start: cur_start,
            end: audio.len(),
        });
    }

    let mut padded = Vec::<Segment>::new();
    for seg in segs {
        let start = seg.start.saturating_sub(speech_pad);
        let end = (seg.end + speech_pad).min(audio.len());
        if let Some(last) = padded.last_mut() {
            if start <= last.end + min_silence {
                if end > last.end {
                    last.end = end;
                }
                continue;
            }
        }
        padded.push(Segment { start, end });
    }
    Ok(padded)
}

fn split_segment(seg: Segment, max_samples: usize) -> Vec<Segment> {
    if seg.end <= seg.start {
        return Vec::new();
    }
    if seg.end - seg.start <= max_samples {
        return vec![seg];
    }
    let mut out = Vec::new();
    let mut pos = seg.start;
    while pos < seg.end {
        let end = (pos + max_samples).min(seg.end);
        out.push(Segment { start: pos, end });
        pos = end;
    }
    out
}

fn log_mel_whisper_like(preproc: &Preprocessor, wav_16k: &[f32]) -> Result<Vec<f32>, String> {
    let mut x = vec![0.0f32; N_SAMPLES];
    if wav_16k.len() >= N_SAMPLES {
        x.copy_from_slice(&wav_16k[..N_SAMPLES]);
    } else {
        x[..wav_16k.len()].copy_from_slice(wav_16k);
    }
    let padded = reflect_pad(&x, PAD);
    let frames = (padded.len().saturating_sub(N_FFT)) / HOP + 1;
    if frames < TARGET_FRAMES + 1 {
        return Err(format!("unexpected frame count: {frames}"));
    }

    let mut mel = vec![0.0f32; preproc.n_mels * (TARGET_FRAMES + 1)];
    let mut buf = vec![Complex32::new(0.0, 0.0); N_FFT];
    for t in 0..(TARGET_FRAMES + 1) {
        let start = t * HOP;
        let frame = &padded[start..start + N_FFT];
        for i in 0..N_FFT {
            buf[i].re = frame[i] * preproc.window[i];
            buf[i].im = 0.0;
        }
        preproc.fft.process(&mut buf);

        let mut power = [0.0f32; N_FREQ_BINS];
        for k in 0..N_FREQ_BINS {
            let c = buf[k];
            power[k] = c.re * c.re + c.im * c.im;
        }
        for m in 0..preproc.n_mels {
            let row = &preproc.mel_fb_t[m * N_FREQ_BINS..(m + 1) * N_FREQ_BINS];
            let mut acc = 0.0f32;
            for k in 0..N_FREQ_BINS {
                acc += row[k] * power[k];
            }
            mel[m * (TARGET_FRAMES + 1) + t] = acc;
        }
    }

    let mut out = vec![0.0f32; preproc.n_mels * TARGET_FRAMES];
    for m in 0..preproc.n_mels {
        out[m * TARGET_FRAMES..(m + 1) * TARGET_FRAMES].copy_from_slice(
            &mel[m * (TARGET_FRAMES + 1)..m * (TARGET_FRAMES + 1) + TARGET_FRAMES],
        );
    }

    let mut maxv = f32::NEG_INFINITY;
    for v in &mut out {
        *v = v.max(1e-10).log10();
        if *v > maxv {
            maxv = *v;
        }
    }
    let floor = maxv - 8.0;
    for v in &mut out {
        *v = (*v).max(floor);
        *v = (*v + 4.0) / 4.0;
    }
    Ok(out)
}

fn reflect_pad(signal: &[f32], pad: usize) -> Vec<f32> {
    if pad == 0 {
        return signal.to_vec();
    }
    let n = signal.len();
    if n == 0 {
        return vec![0.0; pad * 2];
    }
    if n == 1 {
        let mut out = vec![signal[0]; pad];
        out.push(signal[0]);
        out.extend(vec![signal[0]; pad]);
        return out;
    }
    let mut out = Vec::with_capacity(n + pad * 2);
    for i in 0..pad {
        let idx = pad - i;
        let src = if idx < n { idx } else { 1 };
        out.push(signal[src]);
    }
    out.extend_from_slice(signal);
    for i in 0..pad {
        let idx = n - 2 - i;
        out.push(signal[idx]);
    }
    out
}

fn hann_periodic(n: usize) -> Vec<f32> {
    let n_f = n as f32;
    (0..n)
        .map(|i| 0.5 - 0.5 * ((2.0 * std::f32::consts::PI * i as f32) / n_f).cos())
        .collect()
}

fn hertz_to_mel_slaney(freq: f32) -> f32 {
    let f_sp = 200.0 / 3.0;
    let mut mel = freq / f_sp;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 27.0 / 6.4_f32.ln();
    if freq >= min_log_hz {
        mel = min_log_mel + (freq / min_log_hz).ln() * logstep;
    }
    mel
}

fn mel_to_hertz_slaney(mel: f32) -> f32 {
    let f_sp = 200.0 / 3.0;
    let mut hz = mel * f_sp;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4_f32.ln() / 27.0;
    if mel >= min_log_mel {
        hz = min_log_hz * ((mel - min_log_mel) * logstep).exp();
    }
    hz
}

fn build_mel_filterbank_t(
    sr: f32,
    n_fft: usize,
    n_mels: usize,
    fmin: f32,
    fmax: f32,
) -> Result<Vec<f32>, String> {
    let n_freq_bins = n_fft / 2 + 1;
    let mut fft_freqs = vec![0.0f32; n_freq_bins];
    for k in 0..n_freq_bins {
        fft_freqs[k] = (k as f32) * (sr / 2.0) / ((n_freq_bins - 1) as f32);
    }
    let mel_min = hertz_to_mel_slaney(fmin);
    let mel_max = hertz_to_mel_slaney(fmax);
    let mut mel_points = vec![0.0f32; n_mels + 2];
    for i in 0..(n_mels + 2) {
        mel_points[i] = mel_min + (mel_max - mel_min) * (i as f32) / ((n_mels + 1) as f32);
    }
    let mut hz_points = vec![0.0f32; n_mels + 2];
    for i in 0..(n_mels + 2) {
        hz_points[i] = mel_to_hertz_slaney(mel_points[i]);
    }

    let mut fb = vec![0.0f32; n_freq_bins * n_mels];
    for m in 0..n_mels {
        let f_left = hz_points[m];
        let f_center = hz_points[m + 1];
        let f_right = hz_points[m + 2];
        for (k, &f) in fft_freqs.iter().enumerate() {
            let up = (f - f_left) / (f_center - f_left);
            let down = (f_right - f) / (f_right - f_center);
            fb[k * n_mels + m] = up.min(down).max(0.0);
        }
    }
    for m in 0..n_mels {
        let f_left = hz_points[m];
        let f_right = hz_points[m + 2];
        let norm = 2.0 / (f_right - f_left).max(1e-12);
        for k in 0..n_freq_bins {
            fb[k * n_mels + m] *= norm;
        }
    }
    let mut fb_t = vec![0.0f32; n_mels * n_freq_bins];
    for k in 0..n_freq_bins {
        for m in 0..n_mels {
            fb_t[m * n_freq_bins + k] = fb[k * n_mels + m];
        }
    }
    Ok(fb_t)
}

fn last_logits(v: &Value) -> Result<Vec<f32>, String> {
    let (shape, data) = v
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("logits parse failed: {e}"))?;
    let dims: Vec<usize> = shape.as_ref().iter().map(|&x| x as usize).collect();
    if dims.len() != 3 || dims[0] != 1 || dims[1] == 0 {
        return Err(format!("unexpected logits shape: {dims:?}"));
    }
    let vocab = dims[2];
    let start = (dims[1] - 1) * vocab;
    Ok(data[start..start + vocab].to_vec())
}

fn map_past_to_present_name(past_input_name: &str) -> String {
    if past_input_name.starts_with("past_key_values.") {
        return past_input_name.replacen("past_key_values.", "present.", 1);
    }
    if past_input_name.starts_with("past.") {
        return past_input_name.replacen("past.", "present.", 1);
    }
    past_input_name.to_string()
}

fn argmax(v: &[f32]) -> usize {
    let mut best_i = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &x) in v.iter().enumerate() {
        if x > best_v {
            best_v = x;
            best_i = i;
        }
    }
    best_i
}

fn log_softmax(v: &[f32]) -> Vec<f32> {
    if v.is_empty() {
        return Vec::new();
    }
    let m = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f64 = v.iter().map(|&x| ((x - m) as f64).exp()).sum();
    let l = sum.ln() as f32;
    v.iter().map(|&x| x - m - l).collect()
}

fn apply_suppression(logits: &mut [f32], suppress: &[i64], begin: &[i64], step: usize) {
    for &id in suppress {
        if id >= 0 {
            let idx = id as usize;
            if idx < logits.len() {
                logits[idx] = f32::NEG_INFINITY;
            }
        }
    }
    if step == 0 {
        for &id in begin {
            if id >= 0 {
                let idx = id as usize;
                if idx < logits.len() {
                    logits[idx] = f32::NEG_INFINITY;
                }
            }
        }
    }
}

fn resample_linear(input: &[f32], src: u32, dst: u32) -> Vec<f32> {
    if input.is_empty() || src == 0 || dst == 0 || src == dst {
        return input.to_vec();
    }
    let ratio = dst as f64 / src as f64;
    let out_len = ((input.len() as f64) * ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}
