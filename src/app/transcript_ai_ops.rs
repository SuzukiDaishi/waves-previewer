use std::collections::{BTreeSet, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::types::{
    TranscriptAiConfig, TranscriptComputeTarget, TranscriptModelVariant, TranscriptPerfMode,
};

const TRANSCRIPT_MODEL_ID: &str = "onnx-community/whisper-large-v3-turbo";
const TRANSCRIPT_MODEL_REVISION: &str = "main";
const VAD_MODEL_ID: &str = "deepghs/silero-vad-onnx";
const VAD_MODEL_REVISION: &str = "main";
const WHISPER_ALLOWED_LANGUAGES: &[&str] = &[
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da",
    "de", "el", "en", "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "ha", "haw", "he",
    "hi", "hr", "ht", "hu", "hy", "id", "is", "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la",
    "lb", "ln", "lo", "lt", "lv", "mg", "mi", "mk", "ml", "mn", "mr", "ms", "mt", "my", "ne", "nl",
    "nn", "no", "oc", "pa", "pl", "ps", "pt", "ro", "ru", "sa", "sd", "si", "sk", "sl", "sn", "so",
    "sq", "sr", "su", "sv", "sw", "ta", "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz",
    "vi", "yi", "yo", "yue", "zh",
];

fn canonical_transcript_languages() -> Vec<String> {
    WHISPER_ALLOWED_LANGUAGES
        .iter()
        .map(|v| (*v).to_string())
        .collect()
}

fn hf_cache_root() -> PathBuf {
    if let Some(path) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(path);
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut push_unique = |path: PathBuf| {
        if !candidates.iter().any(|p| p == &path) {
            candidates.push(path);
        }
    };

    if let Some(path) = std::env::var_os("HF_HOME") {
        push_unique(PathBuf::from(path).join("hub"));
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        push_unique(PathBuf::from(path).join("huggingface").join("hub"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        push_unique(
            PathBuf::from(home)
                .join(".cache")
                .join("huggingface")
                .join("hub"),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        push_unique(
            PathBuf::from(home)
                .join(".cache")
                .join("huggingface")
                .join("hub"),
        );
    }
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut home = std::ffi::OsString::from(drive);
        home.push(path);
        push_unique(
            PathBuf::from(home)
                .join(".cache")
                .join("huggingface")
                .join("hub"),
        );
    }

    if let Some(existing) = candidates.iter().find(|p| p.is_dir()) {
        return existing.clone();
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

fn model_snapshots_root() -> PathBuf {
    hf_cache_root()
        .join("models--onnx-community--whisper-large-v3-turbo")
        .join("snapshots")
}

fn model_repo_root() -> PathBuf {
    hf_cache_root().join("models--onnx-community--whisper-large-v3-turbo")
}

fn vad_snapshots_root() -> PathBuf {
    hf_cache_root()
        .join("models--deepghs--silero-vad-onnx")
        .join("snapshots")
}

fn vad_repo_root() -> PathBuf {
    hf_cache_root().join("models--deepghs--silero-vad-onnx")
}

fn has_required_model_files(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if !dir.join("config.json").is_file() {
        return false;
    }
    if !dir.join("tokenizer.json").is_file() {
        return false;
    }
    if !dir.join("preprocessor_config.json").is_file() {
        return false;
    }
    let encoder_ok = dir.join("onnx/encoder_model_quantized.onnx").is_file()
        || dir.join("onnx/encoder_model_fp16.onnx").is_file()
        || dir.join("onnx/encoder_model.onnx").is_file();
    let decoder_ok = dir.join("onnx/decoder_model_quantized.onnx").is_file()
        || dir.join("onnx/decoder_model_fp16.onnx").is_file()
        || dir.join("onnx/decoder_model.onnx").is_file();
    let decoder_past_ok = dir
        .join("onnx/decoder_with_past_model_quantized.onnx")
        .is_file()
        || dir.join("onnx/decoder_with_past_model_fp16.onnx").is_file()
        || dir.join("onnx/decoder_with_past_model.onnx").is_file();
    encoder_ok && decoder_ok && decoder_past_ok
}

fn has_required_model_files_for_variant(dir: &Path, variant: TranscriptModelVariant) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if !dir.join("config.json").is_file()
        || !dir.join("tokenizer.json").is_file()
        || !dir.join("preprocessor_config.json").is_file()
    {
        return false;
    }
    match variant {
        TranscriptModelVariant::Auto => has_required_model_files(dir),
        TranscriptModelVariant::Fp16 => {
            dir.join("onnx/encoder_model_fp16.onnx").is_file()
                && dir.join("onnx/decoder_model_fp16.onnx").is_file()
                && dir.join("onnx/decoder_with_past_model_fp16.onnx").is_file()
        }
        TranscriptModelVariant::Quantized => {
            dir.join("onnx/encoder_model_quantized.onnx").is_file()
                && dir.join("onnx/decoder_model_quantized.onnx").is_file()
                && dir
                    .join("onnx/decoder_with_past_model_quantized.onnx")
                    .is_file()
        }
    }
}

fn find_latest_snapshot<F>(root: &Path, mut predicate: F) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !predicate(&path) {
            continue;
        }
        let ts = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((ts, path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, p)| p)
}

fn resolve_model_dir_for_variant(variant: TranscriptModelVariant) -> Option<PathBuf> {
    if let Some(override_dir) = std::env::var_os("NEOWAVES_TRANSCRIPT_MODEL_DIR") {
        let path = PathBuf::from(override_dir);
        if has_required_model_files_for_variant(&path, variant) {
            return Some(path);
        }
    }
    let snapshots = model_snapshots_root();
    if !snapshots.is_dir() {
        return None;
    }
    find_latest_snapshot(&snapshots, |path| {
        has_required_model_files_for_variant(path, variant)
    })
}

fn normalize_special_token_code(token: &str) -> Option<String> {
    let t = token.trim();
    let inner = t
        .strip_prefix("<|")
        .and_then(|v| v.strip_suffix("|>"))
        .unwrap_or(t)
        .trim();
    if inner.is_empty() {
        return None;
    }
    let lower = inner.to_ascii_lowercase();
    Some(lower)
}

fn is_language_code_token(code: &str) -> bool {
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

fn transcript_catalog_from_generation_config(
    model_dir: &Path,
) -> Option<(Vec<String>, Vec<String>)> {
    let path = model_dir.join("generation_config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut langs = BTreeSet::<String>::new();
    let mut tasks = BTreeSet::<String>::new();
    if let Some(obj) = json.get("lang_to_id").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            if let Some(code) =
                normalize_special_token_code(key).filter(|v| is_language_code_token(v))
            {
                langs.insert(code);
            }
        }
    }
    if let Some(obj) = json.get("task_to_id").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            if let Some(task) = normalize_special_token_code(key) {
                let task = task.trim().to_ascii_lowercase();
                if task == "transcribe" || task == "translate" {
                    tasks.insert(task);
                }
            }
        }
    }
    if langs.is_empty() && tasks.is_empty() {
        None
    } else {
        Some((langs.into_iter().collect(), tasks.into_iter().collect()))
    }
}

fn transcript_catalog_from_tokenizer(model_dir: &Path) -> (Vec<String>, Vec<String>) {
    let path = model_dir.join("tokenizer.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (
            Vec::new(),
            vec!["transcribe".to_string(), "translate".to_string()],
        );
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (
            Vec::new(),
            vec!["transcribe".to_string(), "translate".to_string()],
        );
    };
    let mut langs = BTreeSet::<String>::new();
    let mut tasks = BTreeSet::<String>::new();
    let mut push_token = |token: &str| {
        if let Some(code) = normalize_special_token_code(token) {
            if code == "transcribe" || code == "translate" {
                tasks.insert(code);
            } else if is_language_code_token(&code) {
                langs.insert(code);
            }
        }
    };
    if let Some(vocab) = json
        .get("model")
        .and_then(|m| m.get("vocab"))
        .and_then(|v| v.as_object())
    {
        for key in vocab.keys() {
            push_token(key);
        }
    }
    if let Some(added) = json.get("added_tokens").and_then(|v| v.as_array()) {
        for item in added {
            if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
                push_token(content);
            }
        }
    }
    if tasks.is_empty() {
        tasks.insert("transcribe".to_string());
        tasks.insert("translate".to_string());
    }
    (langs.into_iter().collect(), tasks.into_iter().collect())
}

fn transcript_catalog_from_model_dir(model_dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut langs = BTreeSet::<String>::new();
    let mut tasks = BTreeSet::<String>::new();
    if let Some((cfg_langs, cfg_tasks)) = transcript_catalog_from_generation_config(model_dir) {
        langs.extend(cfg_langs);
        tasks.extend(cfg_tasks);
    }
    let (tok_langs, tok_tasks) = transcript_catalog_from_tokenizer(model_dir);
    langs.extend(tok_langs);
    tasks.extend(tok_tasks);
    if tasks.is_empty() {
        tasks.insert("transcribe".to_string());
    }
    (langs.into_iter().collect(), tasks.into_iter().collect())
}

fn sanitize_transcript_language_task(
    language: &str,
    task: &str,
    allowed_languages: &[String],
    allowed_tasks: &[String],
) -> (String, String) {
    let mut next_language = language.trim().to_ascii_lowercase();
    if next_language.is_empty() {
        next_language = "auto".to_string();
    }
    let mut language_options = vec!["auto".to_string()];
    language_options.extend(allowed_languages.iter().cloned());
    if !language_options.iter().any(|v| v == &next_language) {
        next_language = "auto".to_string();
    }

    let mut next_task = task.trim().to_ascii_lowercase();
    if next_task.is_empty() {
        next_task = "transcribe".to_string();
    }
    let fallback_tasks = vec!["transcribe".to_string(), "translate".to_string()];
    let task_options = if allowed_tasks.is_empty() {
        &fallback_tasks
    } else {
        allowed_tasks
    };
    if !task_options.iter().any(|v| v == &next_task) {
        next_task = "transcribe".to_string();
    }
    (next_language, next_task)
}

fn fold_download_progress(
    prev_done: usize,
    prev_total: usize,
    next_done: usize,
    next_total: usize,
) -> (usize, usize) {
    let total = prev_total.max(next_total.max(1));
    let done = prev_done.min(total).max(next_done.min(total));
    (done, total)
}

fn has_vad_model_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("onnx"))
            .unwrap_or(false)
}

fn find_latest_vad_model(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut candidates = Vec::<(std::time::SystemTime, PathBuf)>::new();
    for entry in entries.flatten() {
        let snapshot = entry.path();
        if !snapshot.is_dir() {
            continue;
        }
        for rel in ["silero_vad.onnx", "silero_vad_half.onnx"] {
            let candidate = snapshot.join(rel);
            if !has_vad_model_file(&candidate) {
                continue;
            }
            let ts = std::fs::metadata(&candidate)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((ts, candidate));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, p)| p)
}

fn resolve_vad_model_path(cfg: &super::types::TranscriptAiConfig) -> Option<PathBuf> {
    if let Some(path) = cfg.vad_model_path.as_ref() {
        if has_vad_model_file(path) {
            return Some(path.clone());
        }
    }
    if let Some(path) = std::env::var_os("NEOWAVES_TRANSCRIPT_VAD_MODEL") {
        let path = PathBuf::from(path);
        if has_vad_model_file(&path) {
            return Some(path);
        }
    }
    find_latest_vad_model(&vad_snapshots_root())
}

fn transcribable_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if !crate::audio_io::is_supported_audio_path(&path) {
            continue;
        }
        out.push(path);
    }
    out
}

fn download_model_snapshot_with_progress<F>(mut on_progress: F) -> Result<PathBuf, String>
where
    F: FnMut(usize, usize),
{
    use hf_hub::api::sync::Api;
    use hf_hub::{Repo, RepoType};
    let api = Api::new().map_err(|e| format!("hf-hub init failed: {e}"))?;
    let repo = api.repo(Repo::with_revision(
        TRANSCRIPT_MODEL_ID.to_string(),
        RepoType::Model,
        TRANSCRIPT_MODEL_REVISION.to_string(),
    ));
    let required = [
        "config.json",
        "generation_config.json",
        "preprocessor_config.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "special_tokens_map.json",
        "onnx/encoder_model.onnx",
        "onnx/encoder_model_fp16.onnx",
        "onnx/encoder_model_quantized.onnx",
        "onnx/encoder_model.onnx_data",
        "onnx/encoder_model_fp16.onnx_data",
        "onnx/decoder_model.onnx",
        "onnx/decoder_model_fp16.onnx",
        "onnx/decoder_with_past_model.onnx",
        "onnx/decoder_with_past_model_fp16.onnx",
        "onnx/decoder_model_quantized.onnx",
        "onnx/decoder_with_past_model_quantized.onnx",
        "onnx/decoder_model.onnx_data",
        "onnx/decoder_model_fp16.onnx_data",
        "onnx/decoder_with_past_model.onnx_data",
        "onnx/decoder_with_past_model_fp16.onnx_data",
    ];
    let total = required.len().saturating_add(2);
    let mut done = 0usize;
    on_progress(done, total.max(1));
    let mut any = None::<PathBuf>;
    for rel in required {
        if let Ok(path) = repo.get(rel) {
            any = Some(path);
        }
        done = done.saturating_add(1).min(total.max(1));
        on_progress(done, total.max(1));
    }
    let vad_repo = api.repo(hf_hub::Repo::with_revision(
        VAD_MODEL_ID.to_string(),
        hf_hub::RepoType::Model,
        VAD_MODEL_REVISION.to_string(),
    ));
    for rel in ["silero_vad.onnx", "silero_vad_half.onnx"] {
        let _ = vad_repo.get(rel);
        done = done.saturating_add(1).min(total.max(1));
        on_progress(done, total.max(1));
    }
    let Some(path) = any else {
        return Err("No required model files could be downloaded.".to_string());
    };
    let mut cur = path.as_path();
    loop {
        if has_required_model_files(cur) {
            return Ok(cur.to_path_buf());
        }
        let Some(parent) = cur.parent() else {
            break;
        };
        cur = parent;
    }
    resolve_model_dir_for_variant(TranscriptModelVariant::Auto)
        .ok_or_else(|| "Model download finished but snapshot was not found.".into())
}

fn download_model_snapshot() -> Result<PathBuf, String> {
    download_model_snapshot_with_progress(|_, _| {})
}

fn is_probably_oom(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("out of memory")
        || e.contains("insufficient memory")
        || e.contains("e_outofmemory")
        || e.contains("cuda out of memory")
        || e.contains("oom")
}

fn env_force_cpu_enabled() -> bool {
    std::env::var("NEOWAVES_TRANSCRIPT_FORCE_CPU")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn apply_runtime_overrides(cfg: &TranscriptAiConfig) -> TranscriptAiConfig {
    let mut out = cfg.clone();
    if env_force_cpu_enabled() {
        out.compute_target = TranscriptComputeTarget::Cpu;
    }
    out
}

fn transcript_parallelism(cfg: &TranscriptAiConfig) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(1)
        .max(1);
    let base = match cfg.compute_target {
        TranscriptComputeTarget::Cpu => match cfg.perf_mode {
            TranscriptPerfMode::Stable => 1,
            TranscriptPerfMode::Balanced => 2,
            TranscriptPerfMode::Boost => 4,
        },
        TranscriptComputeTarget::Auto
        | TranscriptComputeTarget::Gpu
        | TranscriptComputeTarget::Npu => 1,
    };
    let mut workers = base.min(cores).clamp(1, 4);
    if cfg.compute_target == TranscriptComputeTarget::Cpu && cfg.cpu_intra_threads > 0 {
        let per_worker_threads = cfg.cpu_intra_threads.max(1);
        let max_workers = (cores / per_worker_threads).max(1);
        workers = workers.min(max_workers);
    }
    workers.max(1)
}

fn load_transcriber_with_retry(
    model_dir: &Path,
    cfg: &TranscriptAiConfig,
) -> Result<super::transcript_onnx::WhisperOnnxTranscriber, String> {
    let mut resolved = cfg.clone();
    resolved.vad_model_path = resolve_vad_model_path(&resolved);
    match super::transcript_onnx::WhisperOnnxTranscriber::load(model_dir, resolved.clone()) {
        Ok(v) => Ok(v),
        Err(init_err) => {
            if init_err.contains(".onnx_data") {
                let downloaded_dir = download_model_snapshot()?;
                super::transcript_onnx::WhisperOnnxTranscriber::load(&downloaded_dir, resolved)
                    .map_err(|e| format!("{init_err}; retry failed: {e}"))
            } else {
                Err(init_err)
            }
        }
    }
}

fn run_transcribe_path(
    path: PathBuf,
    model_dir: &Path,
    cfg: &TranscriptAiConfig,
) -> super::TranscriptAiItemResult {
    let run_path = path.clone();
    match catch_unwind(AssertUnwindSafe(|| {
        let mut transcriber = match load_transcriber_with_retry(model_dir, cfg) {
            Ok(v) => v,
            Err(err) => {
                return super::TranscriptAiItemResult {
                    path: run_path.clone(),
                    srt_path: None,
                    detected_language: None,
                    error: Some(format!("Transcriber init failed: {err}")),
                };
            }
        };
        match transcriber.transcribe_to_srt(&run_path) {
            Ok(result) => super::TranscriptAiItemResult {
                path: run_path.clone(),
                srt_path: Some(result.srt_path),
                detected_language: result.detected_language,
                error: None,
            },
            Err(first_err) => {
                if !matches!(cfg.compute_target, TranscriptComputeTarget::Cpu) {
                    let mut cpu_cfg = cfg.clone();
                    cpu_cfg.compute_target = TranscriptComputeTarget::Cpu;
                    if let Ok(mut cpu_transcriber) =
                        load_transcriber_with_retry(model_dir, &cpu_cfg)
                    {
                        match cpu_transcriber.transcribe_to_srt(&run_path) {
                            Ok(result) => {
                                return super::TranscriptAiItemResult {
                                    path: run_path.clone(),
                                    srt_path: Some(result.srt_path),
                                    detected_language: result.detected_language,
                                    error: None,
                                };
                            }
                            Err(cpu_err) => {
                                return super::TranscriptAiItemResult {
                                    path: run_path.clone(),
                                    srt_path: None,
                                    detected_language: None,
                                    error: Some(format!(
                                        "{first_err}; CPU retry failed: {cpu_err}"
                                    )),
                                };
                            }
                        }
                    }
                }
                super::TranscriptAiItemResult {
                    path: run_path.clone(),
                    srt_path: None,
                    detected_language: None,
                    error: Some(first_err),
                }
            }
        }
    })) {
        Ok(item) => item,
        Err(payload) => super::TranscriptAiItemResult {
            path,
            srt_path: None,
            detected_language: None,
            error: Some(format!(
                "Transcript worker panic: {}",
                panic_payload_to_string(payload)
            )),
        },
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "unknown panic payload".to_string()
}

impl super::WavesPreviewer {
    pub(super) fn transcript_language_options(&self) -> Vec<String> {
        let mut out = vec!["auto".to_string()];
        out.extend(canonical_transcript_languages());
        out
    }

    pub(super) fn transcript_task_options(&self) -> Vec<String> {
        if self.transcript_supported_tasks.is_empty() {
            return vec!["transcribe".to_string(), "translate".to_string()];
        }
        self.transcript_supported_tasks.clone()
    }

    pub(super) fn sanitize_transcript_ai_config(&mut self) {
        let allowed_languages = canonical_transcript_languages();
        let (lang, task) = sanitize_transcript_language_task(
            &self.transcript_ai_cfg.language,
            &self.transcript_ai_cfg.task,
            &allowed_languages,
            &self.transcript_supported_tasks,
        );
        self.transcript_ai_cfg.language = lang;
        self.transcript_ai_cfg.task = task;
    }

    pub(super) fn refresh_transcript_ai_status(&mut self) {
        // Transcript AI is model-availability driven (no explicit opt-in toggle).
        self.transcript_ai_opt_in = true;
        self.transcript_ai_model_dir =
            resolve_model_dir_for_variant(self.transcript_ai_cfg.model_variant);
        self.transcript_supported_languages = canonical_transcript_languages();
        if let Some(dir) = self.transcript_ai_model_dir.as_ref() {
            let (_langs, tasks) = transcript_catalog_from_model_dir(dir);
            self.transcript_supported_tasks = tasks;
        } else {
            self.transcript_supported_tasks =
                vec!["transcribe".to_string(), "translate".to_string()];
        }
        self.sanitize_transcript_ai_config();
        self.transcript_ai_available = self
            .transcript_ai_model_dir
            .as_ref()
            .map(|dir| {
                has_required_model_files_for_variant(dir, self.transcript_ai_cfg.model_variant)
            })
            .unwrap_or(false);
    }

    pub(super) fn transcript_ai_menu_enabled(&self) -> bool {
        self.transcript_ai_state.is_none()
            && self.transcript_model_download_state.is_none()
            && self.transcript_ai_has_model()
    }

    pub(super) fn transcript_ai_unavailable_reason(&self) -> Option<String> {
        if self.transcript_model_download_state.is_some() {
            return Some("Transcript model is downloading...".to_string());
        }
        if self.transcript_ai_state.is_some() {
            return Some("Transcription is already running.".to_string());
        }
        let Some(model_dir) = self.transcript_ai_model_dir.as_ref() else {
            return Some(format!(
                "Transcript model is not installed for {:?}.",
                self.transcript_ai_cfg.model_variant
            ));
        };
        if !has_required_model_files_for_variant(model_dir, self.transcript_ai_cfg.model_variant) {
            return Some(format!(
                "Selected model variant is missing ({:?}).",
                self.transcript_ai_cfg.model_variant
            ));
        }
        None
    }

    pub(super) fn transcript_ai_has_model(&self) -> bool {
        self.transcript_ai_model_dir
            .as_ref()
            .map(|dir| {
                has_required_model_files_for_variant(dir, self.transcript_ai_cfg.model_variant)
            })
            .unwrap_or(false)
    }

    pub(super) fn transcript_ai_can_uninstall(&self) -> bool {
        self.transcript_ai_state.is_none() && self.transcript_model_download_state.is_none()
    }

    pub(super) fn transcript_ai_is_running(&self) -> bool {
        self.transcript_ai_state.is_some()
    }

    pub(super) fn cancel_transcript_ai_run(&mut self) {
        if let Some(state) = &self.transcript_ai_state {
            state.cancel_requested.store(true, Ordering::Relaxed);
            self.debug_log("transcript ai cancel requested".to_string());
        }
    }

    pub(super) fn transcript_ai_effective_vad_model_path(&self) -> Option<PathBuf> {
        resolve_vad_model_path(&self.transcript_ai_cfg)
    }

    pub(super) fn transcript_estimated_parallel_workers(&self) -> usize {
        let cfg = apply_runtime_overrides(&self.transcript_ai_cfg);
        transcript_parallelism(&cfg)
    }

    pub(super) fn queue_transcript_model_download(&mut self) {
        if self.transcript_model_download_state.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<super::TranscriptModelDownloadEvent>();
        std::thread::spawn(move || {
            let result = match download_model_snapshot_with_progress(|done, total| {
                let _ = tx.send(super::TranscriptModelDownloadEvent::Progress {
                    done: done.min(total.max(1)),
                    total: total.max(1),
                });
            }) {
                Ok(dir) => super::TranscriptModelDownloadResult {
                    model_dir: Some(dir),
                    error: None,
                },
                Err(err) => super::TranscriptModelDownloadResult {
                    model_dir: None,
                    error: Some(err),
                },
            };
            let _ = tx.send(super::TranscriptModelDownloadEvent::Finished(result));
        });
        self.transcript_model_download_state = Some(super::TranscriptModelDownloadState {
            _started_at: std::time::Instant::now(),
            done: 0,
            total: 1,
            rx,
        });
        self.transcript_ai_last_error = None;
    }

    pub(super) fn uninstall_transcript_model_cache(&mut self) {
        if !self.transcript_ai_can_uninstall() {
            self.transcript_ai_last_error =
                Some("Cannot uninstall while transcription/download is running.".to_string());
            return;
        }
        let mut removed_any = false;
        let mut errors = Vec::new();
        for dir in [model_repo_root(), vad_repo_root()] {
            if !dir.exists() {
                continue;
            }
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => removed_any = true,
                Err(e) => errors.push(format!("{}: {e}", dir.display())),
            }
        }
        if !errors.is_empty() {
            self.transcript_ai_last_error = Some(format!(
                "Transcript model uninstall failed: {}",
                errors.join(" | ")
            ));
            return;
        }
        self.refresh_transcript_ai_status();
        if removed_any {
            self.transcript_ai_last_error = None;
            self.debug_log("transcript model cache removed".to_string());
        }
    }

    pub(super) fn run_transcript_ai_for_selected(&mut self, paths: Vec<PathBuf>) {
        self.refresh_transcript_ai_status();
        if self.transcript_ai_state.is_some() || self.transcript_model_download_state.is_some() {
            return;
        }
        let Some(model_dir) = self.transcript_ai_model_dir.clone() else {
            self.transcript_ai_last_error = Some("Transcript model is not available.".to_string());
            return;
        };
        if !has_required_model_files_for_variant(&model_dir, self.transcript_ai_cfg.model_variant) {
            self.transcript_ai_last_error = Some(format!(
                "Selected model variant is missing ({:?}).",
                self.transcript_ai_cfg.model_variant
            ));
            return;
        }
        let targets = transcribable_paths(paths);
        if targets.is_empty() {
            self.transcript_ai_last_error =
                Some("No transcribable audio files were selected.".to_string());
            return;
        }
        let total = targets.len();
        let transcript_cfg = apply_runtime_overrides(&self.transcript_ai_cfg);
        let mut processing_targets = Vec::<PathBuf>::new();
        let mut pre_done = 0usize;
        let mut had_preloaded_success = false;
        for path in targets {
            let should_skip_existing = !transcript_cfg.overwrite_existing_srt
                && super::transcript::srt_path_for_audio(&path)
                    .map(|p| p.is_file())
                    .unwrap_or(false);
            if should_skip_existing {
                if let Some(srt_path) = super::transcript::srt_path_for_audio(&path) {
                    if let Some(t) = super::transcript::load_srt(&srt_path) {
                        if self.set_transcript_for_path(&path, Some(t)) {
                            if !self.transcript_ai_cfg.language.eq_ignore_ascii_case("auto") {
                                self.set_transcript_language_for_path(
                                    &path,
                                    Some(self.transcript_ai_cfg.language.clone()),
                                );
                            }
                            had_preloaded_success = true;
                        }
                    } else {
                        self.queue_transcript_for_path(&path, true);
                    }
                }
                pre_done = pre_done.saturating_add(1);
                continue;
            }
            processing_targets.push(path);
        }
        if had_preloaded_success && self.sort_key_uses_transcript() {
            self.apply_sort();
        }
        if processing_targets.is_empty() {
            self.transcript_ai_last_error = None;
            return;
        }
        let worker_count = transcript_parallelism(&transcript_cfg);
        self.debug_log(format!(
            "transcript ai start: files={} process={} skipped={} workers={} mode={:?} model_variant={:?} model_dir={} vad_enabled={} vad_model={} compute={:?} dml_device={} cpu_threads={} force_cpu={}",
            total,
            processing_targets.len(),
            pre_done,
            worker_count,
            transcript_cfg.perf_mode,
            transcript_cfg.model_variant,
            model_dir.display(),
            transcript_cfg.vad_enabled,
            transcript_cfg
                .vad_model_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(auto)".to_string()),
            transcript_cfg.compute_target,
            transcript_cfg.dml_device_id,
            transcript_cfg.cpu_intra_threads,
            env_force_cpu_enabled(),
        ));
        let (tx, rx) = std::sync::mpsc::channel::<super::TranscriptAiRunResult>();
        let cancel_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::clone(&cancel_requested);
        let thread_targets = processing_targets.clone();
        std::thread::spawn(move || {
            let mut parallel = worker_count.max(1);
            if parallel == 1 {
                // Single-worker path keeps runtime/session reuse most stable for accelerator modes.
                let mut config = transcript_cfg.clone();
                config.vad_model_path = resolve_vad_model_path(&config);
                let mut transcriber = match load_transcriber_with_retry(&model_dir, &config) {
                    Ok(v) => v,
                    Err(init_err) => {
                        for path in &thread_targets {
                            let _ = tx.send(super::TranscriptAiRunResult::Item(
                                super::TranscriptAiItemResult {
                                    path: path.clone(),
                                    srt_path: None,
                                    detected_language: None,
                                    error: Some(format!("Transcriber init failed: {init_err}")),
                                },
                            ));
                        }
                        let _ = tx.send(super::TranscriptAiRunResult::Finished);
                        return;
                    }
                };
                let mut queue: VecDeque<PathBuf> = VecDeque::from(thread_targets.clone());
                while let Some(path) = queue.pop_front() {
                    if cancel_flag.load(Ordering::Relaxed) {
                        let _ = tx.send(super::TranscriptAiRunResult::Item(
                            super::TranscriptAiItemResult {
                                path,
                                srt_path: None,
                                detected_language: None,
                                error: None,
                            },
                        ));
                        while let Some(skipped) = queue.pop_front() {
                            let _ = tx.send(super::TranscriptAiRunResult::Item(
                                super::TranscriptAiItemResult {
                                    path: skipped,
                                    srt_path: None,
                                    detected_language: None,
                                    error: None,
                                },
                            ));
                        }
                        break;
                    }
                    let _ = tx.send(super::TranscriptAiRunResult::Started(path.clone()));
                    let first = match catch_unwind(AssertUnwindSafe(|| {
                        transcriber.transcribe_to_srt(&path)
                    })) {
                        Ok(v) => v,
                        Err(payload) => Err(format!(
                            "Transcriber panic: {}",
                            panic_payload_to_string(payload)
                        )),
                    };
                    let item = match first {
                        Ok(result) => super::TranscriptAiItemResult {
                            path,
                            srt_path: Some(result.srt_path),
                            detected_language: result.detected_language,
                            error: None,
                        },
                        Err(first_err) => {
                            if !matches!(config.compute_target, TranscriptComputeTarget::Cpu) {
                                let mut cpu_cfg = config.clone();
                                cpu_cfg.compute_target = TranscriptComputeTarget::Cpu;
                                match load_transcriber_with_retry(&model_dir, &cpu_cfg) {
                                    Ok(mut cpu_transcriber) => {
                                        let cpu_attempt =
                                            match catch_unwind(AssertUnwindSafe(|| {
                                                cpu_transcriber.transcribe_to_srt(&path)
                                            })) {
                                                Ok(v) => v,
                                                Err(payload) => Err(format!(
                                                    "CPU transcriber panic: {}",
                                                    panic_payload_to_string(payload)
                                                )),
                                            };
                                        match cpu_attempt {
                                            Ok(result) => super::TranscriptAiItemResult {
                                                path,
                                                srt_path: Some(result.srt_path),
                                                detected_language: result.detected_language,
                                                error: None,
                                            },
                                            Err(cpu_err) => super::TranscriptAiItemResult {
                                                path,
                                                srt_path: None,
                                                detected_language: None,
                                                error: Some(format!(
                                                    "{first_err}; CPU retry failed: {cpu_err}"
                                                )),
                                            },
                                        }
                                    }
                                    Err(cpu_init_err) => super::TranscriptAiItemResult {
                                        path,
                                        srt_path: None,
                                        detected_language: None,
                                        error: Some(format!(
                                            "{first_err}; CPU fallback init failed: {cpu_init_err}"
                                        )),
                                    },
                                }
                            } else {
                                super::TranscriptAiItemResult {
                                    path,
                                    srt_path: None,
                                    detected_language: None,
                                    error: Some(first_err),
                                }
                            }
                        }
                    };
                    let _ = tx.send(super::TranscriptAiRunResult::Item(item));
                }
                let _ = tx.send(super::TranscriptAiRunResult::Finished);
                return;
            }

            // CPU multi-worker path (Balanced/Boost only): file-level parallelism with backoff.
            let mut queue: VecDeque<PathBuf> = VecDeque::from(thread_targets);
            let (item_tx, item_rx) = std::sync::mpsc::channel::<super::TranscriptAiItemResult>();
            let mut running = 0usize;
            while !queue.is_empty() || running > 0 {
                while running < parallel && !queue.is_empty() {
                    if cancel_flag.load(Ordering::Relaxed) {
                        queue.clear();
                        break;
                    }
                    let path = queue.pop_front().expect("queue not empty");
                    let _ = tx.send(super::TranscriptAiRunResult::Started(path.clone()));
                    let per_file_tx = item_tx.clone();
                    let per_file_model_dir = model_dir.clone();
                    let per_file_cfg = transcript_cfg.clone();
                    let per_file_cancel = Arc::clone(&cancel_flag);
                    std::thread::spawn(move || {
                        let item = if per_file_cancel.load(Ordering::Relaxed) {
                            super::TranscriptAiItemResult {
                                path,
                                srt_path: None,
                                detected_language: None,
                                error: None,
                            }
                        } else {
                            match catch_unwind(AssertUnwindSafe(|| {
                                run_transcribe_path(
                                    path.clone(),
                                    &per_file_model_dir,
                                    &per_file_cfg,
                                )
                            })) {
                                Ok(v) => v,
                                Err(payload) => super::TranscriptAiItemResult {
                                    path,
                                    srt_path: None,
                                    detected_language: None,
                                    error: Some(format!(
                                        "Transcript worker panic: {}",
                                        panic_payload_to_string(payload)
                                    )),
                                },
                            }
                        };
                        let _ = per_file_tx.send(item);
                    });
                    running += 1;
                }

                if running == 0 {
                    break;
                }
                match item_rx.recv_timeout(Duration::from_millis(120)) {
                    Ok(item) => {
                        running = running.saturating_sub(1);
                        if let Some(err) = item.error.as_ref() {
                            if parallel > 1 && is_probably_oom(err) {
                                parallel = 1;
                            }
                        }
                        let _ = tx.send(super::TranscriptAiRunResult::Item(item));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if cancel_flag.load(Ordering::Relaxed) {
                            while let Some(skipped) = queue.pop_front() {
                                let _ = tx.send(super::TranscriptAiRunResult::Item(
                                    super::TranscriptAiItemResult {
                                        path: skipped,
                                        srt_path: None,
                                        detected_language: None,
                                        error: None,
                                    },
                                ));
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
            }
            let _ = tx.send(super::TranscriptAiRunResult::Finished);
        });
        self.transcript_ai_last_error = None;
        self.transcript_ai_state = Some(super::TranscriptAiRunState {
            started_at: std::time::Instant::now(),
            total,
            process_total: processing_targets.len(),
            skipped_total: pre_done.min(total),
            done: pre_done.min(total),
            pending: processing_targets.iter().cloned().collect(),
            cancel_requested,
            rx,
        });
    }

    pub(super) fn drain_transcript_model_download_results(&mut self, ctx: &egui::Context) {
        let Some(_) = &self.transcript_model_download_state else {
            return;
        };
        let mut finished: Option<super::TranscriptModelDownloadResult> = None;
        if let Some(state) = self.transcript_model_download_state.as_mut() {
            while let Ok(event) = state.rx.try_recv() {
                match event {
                    super::TranscriptModelDownloadEvent::Progress { done, total } => {
                        let (next_done, next_total) =
                            fold_download_progress(state.done, state.total, done, total);
                        state.done = next_done;
                        state.total = next_total;
                    }
                    super::TranscriptModelDownloadEvent::Finished(result) => {
                        finished = Some(result);
                    }
                }
            }
        }
        if let Some(result) = finished {
            self.transcript_model_download_state = None;
            if let Some(err) = result.error {
                self.transcript_ai_last_error = Some(err.clone());
                self.debug_log(format!("transcript model download failed: {err}"));
            } else if let Some(dir) = result.model_dir {
                self.debug_log(format!("transcript model ready: {}", dir.display()));
                self.transcript_ai_model_dir = Some(dir);
                self.refresh_transcript_ai_status();
                self.transcript_ai_last_error = None;
            }
        }
        if self.transcript_model_download_state.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint();
        }
    }

    pub(super) fn drain_transcript_ai_results(&mut self, ctx: &egui::Context) {
        let Some(_) = &self.transcript_ai_state else {
            return;
        };
        let mut items = Vec::<super::TranscriptAiItemResult>::new();
        let mut started = Vec::<PathBuf>::new();
        let mut finished = false;
        if let Some(state) = &self.transcript_ai_state {
            while let Ok(msg) = state.rx.try_recv() {
                match msg {
                    super::TranscriptAiRunResult::Started(path) => started.push(path),
                    super::TranscriptAiRunResult::Item(item) => items.push(item),
                    super::TranscriptAiRunResult::Finished => finished = true,
                }
            }
        }

        for path in started {
            if let Some(state) = self.transcript_ai_state.as_mut() {
                state.pending.remove(&path);
            }
            self.transcript_ai_inflight.insert(path);
        }
        let had_items = !items.is_empty();
        let mut had_success = false;
        for item in items {
            let detected_language = item.detected_language.clone();
            self.transcript_ai_inflight.remove(&item.path);
            if let Some(state) = self.transcript_ai_state.as_mut() {
                state.pending.remove(&item.path);
            }
            if let Some(state) = self.transcript_ai_state.as_mut() {
                state.done = state.done.saturating_add(1).min(state.total);
            }
            if let Some(err) = item.error {
                self.transcript_ai_last_error = Some(err.clone());
                self.debug_log(format!(
                    "transcript ai failed: path={} err={err}",
                    item.path.display()
                ));
                continue;
            }
            if let Some(srt_path) = item.srt_path {
                if srt_path.is_file() {
                    if let Some(t) = super::transcript::load_srt(&srt_path) {
                        if self.set_transcript_for_path(&item.path, Some(t)) {
                            self.set_transcript_language_for_path(
                                &item.path,
                                detected_language.clone(),
                            );
                            had_success = true;
                        }
                    } else {
                        self.transcript_ai_last_error = Some(format!(
                            "Transcript file could not be parsed: {}",
                            srt_path.display()
                        ));
                        self.queue_transcript_for_path(&item.path, true);
                        had_success = true;
                    }
                } else {
                    let msg = format!("transcript ai missing output: {}", srt_path.display());
                    self.transcript_ai_last_error = Some(msg.clone());
                    self.debug_log(msg);
                }
            }
        }

        if finished {
            let canceled = self
                .transcript_ai_state
                .as_ref()
                .map(|s| s.cancel_requested.load(Ordering::Relaxed))
                .unwrap_or(false);
            if canceled && self.transcript_ai_last_error.is_none() {
                self.transcript_ai_last_error = Some("Transcription canceled.".to_string());
            }
            self.transcript_ai_inflight.clear();
            self.transcript_ai_state = None;
            if had_success && self.sort_key_uses_transcript() {
                self.apply_sort();
            }
            ctx.request_repaint();
            return;
        }
        if had_items && self.transcript_ai_state.is_some() {
            // Reflect per-file transcript updates immediately while a batch is still running.
            ctx.request_repaint();
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_transcript_languages, fold_download_progress,
        has_required_model_files_for_variant, sanitize_transcript_language_task,
        transcript_catalog_from_generation_config, transcript_catalog_from_tokenizer,
    };
    use crate::app::types::TranscriptModelVariant;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neowaves_transcript_{tag}_{nonce}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn fp16_variant_without_sidecar_is_ready() {
        let dir = temp_dir("fp16_ready");
        std::fs::create_dir_all(dir.join("onnx")).expect("mkdir onnx");
        for rel in [
            "config.json",
            "tokenizer.json",
            "preprocessor_config.json",
            "onnx/encoder_model_fp16.onnx",
            "onnx/decoder_model_fp16.onnx",
            "onnx/decoder_with_past_model_fp16.onnx",
        ] {
            std::fs::write(dir.join(rel), "{}").expect("touch");
        }
        assert!(has_required_model_files_for_variant(
            &dir,
            TranscriptModelVariant::Fp16
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_parses_generation_config() {
        let dir = temp_dir("catalog");
        let text = r#"{
  "lang_to_id": {"<|ja|>": 50266, "<|en|>": 50259},
  "task_to_id": {"transcribe": 50359, "translate": 50358}
}"#;
        std::fs::write(dir.join("generation_config.json"), text).expect("write cfg");
        let (langs, tasks) =
            transcript_catalog_from_generation_config(&dir).expect("catalog parse");
        assert!(langs.iter().any(|v| v == "ja"));
        assert!(langs.iter().any(|v| v == "en"));
        assert!(tasks.iter().any(|v| v == "transcribe"));
        assert!(tasks.iter().any(|v| v == "translate"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_invalid_values() {
        let langs = vec!["en".to_string(), "ja".to_string()];
        let tasks = vec!["transcribe".to_string(), "translate".to_string()];
        let (lang, task) = sanitize_transcript_language_task(
            "focus_language_t_jaoken",
            "bad_task",
            &langs,
            &tasks,
        );
        assert_eq!(lang, "auto");
        assert_eq!(task, "transcribe");
    }

    #[test]
    fn tokenizer_catalog_ignores_non_language_noise_tokens() {
        let dir = temp_dir("tokenizer_noise");
        let text = r#"{
  "model": {
    "vocab": {
      "<|ja|>": 1,
      "<|en|>": 2,
      "<|es-419|>": 3,
      "<|--|>": 4,
      "<|00|>": 5,
      "<|000|>": 6,
      "<|transcribe|>": 7
    }
  }
}"#;
        std::fs::write(dir.join("tokenizer.json"), text).expect("write tokenizer");
        let (langs, tasks) = transcript_catalog_from_tokenizer(&dir);
        assert!(langs.iter().any(|v| v == "ja"));
        assert!(langs.iter().any(|v| v == "en"));
        assert!(langs.iter().any(|v| v == "es-419"));
        assert!(!langs.iter().any(|v| v == "--"));
        assert!(!langs.iter().any(|v| v == "00"));
        assert!(!langs.iter().any(|v| v == "000"));
        assert!(tasks.iter().any(|v| v == "transcribe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonical_language_list_is_whitelist_only() {
        let langs = canonical_transcript_languages();
        assert!(langs.iter().any(|v| v == "ja"));
        assert!(langs.iter().any(|v| v == "en"));
        assert!(langs.iter().any(|v| v == "yue"));
        assert!(!langs.iter().any(|v| v == "aa"));
        assert!(!langs.iter().any(|v| v == "ability"));
        assert_eq!(langs.len(), 100);
    }

    #[test]
    fn download_progress_fold_is_monotonic() {
        let mut done = 0usize;
        let mut total = 1usize;
        for (next_done, next_total) in [(0, 3), (1, 3), (1, 3), (0, 3), (3, 3)] {
            (done, total) = fold_download_progress(done, total, next_done, next_total);
        }
        assert_eq!(total, 3);
        assert_eq!(done, 3);
    }
}
