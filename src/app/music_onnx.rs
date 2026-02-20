use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use ndarray::{
    s, Array1, Array2, Array3, Array4, Array5, ArrayD, Axis, Ix1, Ix2, Ix3, Ix4, Ix5, Zip,
};
use ort::{
    ep,
    session::Session,
    value::{DynValue, Value},
};
use rand::Rng;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::types::{MusicAnalysisResult, MusicStemSet};

const MUSIC_MODEL_ID: &str = "zukky/allinone-DLL-ONNX";
const MUSIC_MODEL_REVISION: &str = "main";
const DEMUCS_SUPPORTED_SAMPLE_RATE: u32 = 44_100;
const HARMONIX_LABELS: [&str; 10] = [
    "start", "end", "intro", "outro", "break", "bridge", "inst", "solo", "verse", "chorus",
];
const MUSIC_ANALYZE_CANCELED: &str = "music analyze canceled";

pub(super) fn is_cancel_error(err: &str) -> bool {
    let msg = err.trim().to_ascii_lowercase();
    msg.contains(MUSIC_ANALYZE_CANCELED)
}

fn cancel_err<T>() -> Result<T, String> {
    Err(MUSIC_ANALYZE_CANCELED.to_string())
}

#[inline]
fn check_canceled(cancel_requested: &Arc<AtomicBool>) -> Result<(), String> {
    if cancel_requested.load(Ordering::Relaxed) {
        return cancel_err();
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InferenceExecMode {
    DmlPreferred,
    CpuOnly,
}

impl InferenceExecMode {
    fn progress_label(self) -> &'static str {
        match self {
            InferenceExecMode::DmlPreferred => "DML preferred (CPU fallback)",
            InferenceExecMode::CpuOnly => "CPU",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StemResolveResult {
    pub bass: PathBuf,
    pub drums: PathBuf,
    pub other: PathBuf,
    pub vocals: PathBuf,
    pub missing: Vec<String>,
}

impl StemResolveResult {
    pub(super) fn is_ready(&self) -> bool {
        self.missing.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct FoldConfig {
    sample_rate: usize,
    hop_size: usize,
    fps: usize,
    window_size: usize,
    num_bands: usize,
    fmin: f32,
    fmax: f32,
    min_hops_per_beat: usize,
    export_frames: Option<usize>,
    threshold_downbeat: Option<f32>,
    best_threshold_downbeat: Option<f32>,
}

#[derive(Clone, Debug, Deserialize)]
struct EnsembleManifest {
    models: Vec<String>,
    configs: Vec<String>,
    best_threshold_downbeat: Option<f32>,
}

#[derive(Clone, Debug)]
struct ModelSpec {
    model_paths: Vec<PathBuf>,
    config: FoldConfig,
    manifest_best_threshold_downbeat: Option<f32>,
}

#[derive(Clone, Debug)]
struct AnalysisSegment {
    start_sec: f32,
    label: String,
}

#[derive(Clone, Debug, Default)]
struct AnalysisResultSec {
    beats_sec: Vec<f32>,
    downbeats_sec: Vec<f32>,
    segments: Vec<AnalysisSegment>,
}

pub(super) struct MusicAnalyzeOutput {
    pub result: MusicAnalysisResult,
    pub source_len_samples: usize,
}

pub(super) fn music_model_snapshots_root() -> PathBuf {
    hf_cache_root()
        .join("models--zukky--allinone-DLL-ONNX")
        .join("snapshots")
}

pub(super) fn music_model_repo_root() -> PathBuf {
    hf_cache_root().join("models--zukky--allinone-DLL-ONNX")
}

pub(super) fn resolve_music_model_dir() -> Option<PathBuf> {
    let snapshots = music_model_snapshots_root();
    if !snapshots.is_dir() {
        return None;
    }
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(&snapshots).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !has_required_music_model_files(&path) {
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

pub(super) fn has_required_music_model_files(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if dir.join("onnx/ensemble_manifest.json").is_file() {
        return true;
    }
    dir.join("onnx/harmonix-all-20480.onnx").is_file()
        || dir.join("onnx/harmonix-all.onnx").is_file()
        || dir.join("onnx/harmonix-fold0.onnx").is_file()
        || dir.join("onnx/folds/harmonix-fold0.onnx").is_file()
}

pub(super) fn resolve_demucs_model_path(model_dir: &Path) -> Option<PathBuf> {
    let candidates = ["htdemucs.onnx", "onnx/htdemucs.onnx"];
    for rel in candidates {
        let path = model_dir.join(rel);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub(super) fn download_music_model_snapshot() -> Result<PathBuf, String> {
    use hf_hub::api::sync::Api;
    use hf_hub::{Repo, RepoType};

    let api = Api::new().map_err(|e| format!("hf-hub init failed: {e}"))?;
    let repo = api.repo(Repo::with_revision(
        MUSIC_MODEL_ID.to_string(),
        RepoType::Model,
        MUSIC_MODEL_REVISION.to_string(),
    ));

    // Seed files first.
    let seed_files = [
        "onnx/ensemble_manifest.json",
        "onnx/harmonix-all-20480.onnx",
        "onnx/harmonix-all-20480.json",
        "onnx/harmonix-all.onnx",
        "onnx/harmonix-all.json",
        "onnx/harmonix-fold0.onnx",
        "onnx/harmonix-fold0.json",
        "onnx/folds/harmonix-fold0.onnx",
        "onnx/folds/harmonix-fold0.json",
        "htdemucs.onnx",
        "onnx/htdemucs.onnx",
    ];
    for rel in seed_files {
        let _ = repo.get(rel);
    }

    // If manifest exists, download every listed model/config.
    if let Ok(manifest_path) = repo.get("onnx/ensemble_manifest.json") {
        if let Ok(text) = std::fs::read_to_string(&manifest_path) {
            if let Ok(manifest) = serde_json::from_str::<EnsembleManifest>(&text) {
                for rel in manifest.models.iter().chain(manifest.configs.iter()) {
                    let _ = repo.get(rel);
                }
            }
        }
    }

    resolve_music_model_dir().ok_or_else(|| {
        "Music analyze model download finished but snapshot was not found.".to_string()
    })
}

pub(super) fn resolve_stem_paths(
    audio_path: &Path,
    stems_dir_override: Option<&Path>,
) -> StemResolveResult {
    let stem_name = audio_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");

    let root_dir = match stems_dir_override {
        Some(dir) => dir.to_path_buf(),
        None => audio_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("stems")
            .join(stem_name),
    };

    let bass = root_dir.join("bass.wav");
    let drums = root_dir.join("drums.wav");
    let other = root_dir.join("other.wav");
    let vocals = root_dir.join("vocals.wav");

    let mut missing = Vec::new();
    for (name, path) in [
        ("bass.wav", &bass),
        ("drums.wav", &drums),
        ("other.wav", &other),
        ("vocals.wav", &vocals),
    ] {
        if !path.is_file() {
            missing.push(name.to_string());
        }
    }

    StemResolveResult {
        bass,
        drums,
        other,
        vocals,
        missing,
    }
}

pub(super) fn load_stems_for_preview(
    stems: &StemResolveResult,
    target_sr: u32,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<MusicStemSet, String> {
    check_canceled(cancel_requested)?;
    if !stems.is_ready() {
        return Err(format!("stems missing: {}", stems.missing.join(", ")));
    }
    let load_one = |path: &Path| -> Result<Vec<Vec<f32>>, String> {
        let (channels, sr) = crate::audio_io::decode_audio_multi(path)
            .map_err(|e| format!("decode failed ({}): {e}", path.display()))?;
        if channels.is_empty() {
            return Err(format!("decoded zero channels: {}", path.display()));
        }
        if sr == target_sr {
            Ok(channels)
        } else {
            Ok(channels
                .into_iter()
                .map(|ch| resample_linear(&ch, sr, target_sr))
                .collect())
        }
    };

    let bass = load_one(stems.bass.as_path())?;
    check_canceled(cancel_requested)?;
    let drums = load_one(stems.drums.as_path())?;
    check_canceled(cancel_requested)?;
    let other = load_one(stems.other.as_path())?;
    check_canceled(cancel_requested)?;
    let vocals = load_one(stems.vocals.as_path())?;
    check_canceled(cancel_requested)?;

    Ok(MusicStemSet {
        sample_rate: target_sr,
        bass,
        drums,
        other,
        vocals,
    })
}

pub(super) fn load_or_demix_stems_for_preview(
    audio_path: &Path,
    stems: &StemResolveResult,
    model_dir: &Path,
    target_sr: u32,
    cancel_requested: &Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(String),
) -> Result<MusicStemSet, String> {
    check_canceled(cancel_requested)?;
    if stems.is_ready() {
        on_progress("Loading stems...".to_string());
        return load_stems_for_preview(stems, target_sr, cancel_requested);
    }
    let demucs_model = resolve_demucs_model_path(model_dir).ok_or_else(|| {
        format!(
            "Missing stems ({}) and Demucs model not found in {}",
            stems.missing.join(", "),
            model_dir.display()
        )
    })?;
    demix_input_audio_to_stems(
        audio_path,
        demucs_model.as_path(),
        target_sr,
        cancel_requested,
        on_progress,
    )
}

fn demix_input_audio_to_stems(
    audio_path: &Path,
    demucs_model_path: &Path,
    target_sr: u32,
    cancel_requested: &Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(String),
) -> Result<MusicStemSet, String> {
    check_canceled(cancel_requested)?;
    let exec_mode = preferred_exec_mode();
    on_progress(format!("Demucs: EP: {}", exec_mode.progress_label()));
    on_progress("Demucs: decoding source audio...".to_string());
    let (channels, src_sr) = crate::audio_io::decode_audio_multi(audio_path)
        .map_err(|e| format!("decode failed ({}): {e}", audio_path.display()))?;
    if channels.is_empty() {
        return Err(format!("decoded zero channels: {}", audio_path.display()));
    }
    let stereo_44k = normalize_to_stereo_sr(&channels, src_sr, DEMUCS_SUPPORTED_SAMPLE_RATE);
    let len = stereo_44k
        .first()
        .map(|ch| ch.len())
        .unwrap_or(0)
        .min(stereo_44k.get(1).map(|ch| ch.len()).unwrap_or(0));
    if len == 0 {
        return Err("demucs input has zero samples".to_string());
    }

    let mut stereo = Array2::<f32>::zeros((2, len));
    for i in 0..len {
        stereo[[0, i]] = stereo_44k[0][i];
        stereo[[1, i]] = stereo_44k[1][i];
    }

    check_canceled(cancel_requested)?;
    on_progress("Demucs: loading ONNX model...".to_string());
    let mut demucs = DemucsOnnx::new(demucs_model_path, exec_mode)
        .map_err(|e| format!("Demucs load failed ({}): {e}", demucs_model_path.display()))?;
    on_progress("Demucs: separating stems...".to_string());
    let mut last_pct = -1i32;
    let mut last_emit = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(250))
        .unwrap_or_else(std::time::Instant::now);
    let stems = demucs
        .separate(&stereo, None, cancel_requested, |chunk, total| {
            let pct = if total > 0 {
                (chunk as f32 * 100.0) / total as f32
            } else {
                0.0
            };
            let pct_i = pct.round() as i32;
            let now = std::time::Instant::now();
            if pct_i == last_pct
                && now.duration_since(last_emit) < std::time::Duration::from_millis(120)
            {
                return;
            }
            last_pct = pct_i;
            last_emit = now;
            on_progress(format!(
                "Demucs: separating stems {chunk}/{total} ({pct:.0}%)"
            ));
        })
        .map_err(|e| format!("Demucs separate failed: {e}"))?;
    check_canceled(cancel_requested)?;
    let source_names = demucs.sources().to_vec();

    let mut by_name = std::collections::HashMap::<String, Vec<Vec<f32>>>::new();
    for (source_idx, source_name) in source_names.iter().enumerate() {
        if source_idx >= stems.len_of(Axis(0)) {
            continue;
        }
        let stem = stems.slice(s![source_idx, .., ..]).to_owned();
        let mut chs = Vec::<Vec<f32>>::new();
        for ch in 0..stem.len_of(Axis(0)) {
            let v = stem.slice(s![ch, ..]).to_vec();
            chs.push(if target_sr == DEMUCS_SUPPORTED_SAMPLE_RATE {
                v
            } else {
                resample_linear(&v, DEMUCS_SUPPORTED_SAMPLE_RATE, target_sr)
            });
        }
        by_name.insert(source_name.to_ascii_lowercase(), chs);
    }

    let pick = |name: &str, fallback_idx: usize| -> Option<Vec<Vec<f32>>> {
        if let Some(v) = by_name.get(name) {
            return Some(v.clone());
        }
        if fallback_idx < stems.len_of(Axis(0)) {
            let stem = stems.slice(s![fallback_idx, .., ..]).to_owned();
            let mut chs = Vec::<Vec<f32>>::new();
            for ch in 0..stem.len_of(Axis(0)) {
                let v = stem.slice(s![ch, ..]).to_vec();
                chs.push(if target_sr == DEMUCS_SUPPORTED_SAMPLE_RATE {
                    v
                } else {
                    resample_linear(&v, DEMUCS_SUPPORTED_SAMPLE_RATE, target_sr)
                });
            }
            return Some(chs);
        }
        None
    };

    let bass = pick("bass", 1).ok_or_else(|| "Demucs output missing bass stem".to_string())?;
    let drums = pick("drums", 0).ok_or_else(|| "Demucs output missing drums stem".to_string())?;
    let other = pick("other", 2).ok_or_else(|| "Demucs output missing other stem".to_string())?;
    let vocals =
        pick("vocals", 3).ok_or_else(|| "Demucs output missing vocals stem".to_string())?;

    Ok(MusicStemSet {
        sample_rate: target_sr,
        bass,
        drums,
        other,
        vocals,
    })
}

fn normalize_to_stereo_sr(input: &[Vec<f32>], src_sr: u32, dst_sr: u32) -> Vec<Vec<f32>> {
    let mut channels: Vec<Vec<f32>> = if input.len() >= 2 {
        vec![input[0].clone(), input[1].clone()]
    } else if input.len() == 1 {
        vec![input[0].clone(), input[0].clone()]
    } else {
        vec![Vec::new(), Vec::new()]
    };
    if src_sr != dst_sr {
        channels = channels
            .into_iter()
            .map(|ch| resample_linear(&ch, src_sr, dst_sr))
            .collect();
    }
    let len = channels.iter().map(|ch| ch.len()).min().unwrap_or(0);
    for ch in channels.iter_mut() {
        ch.truncate(len);
    }
    channels
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn preferred_exec_mode() -> InferenceExecMode {
    if env_truthy("NEOWAVES_MUSIC_FORCE_CPU") {
        return InferenceExecMode::CpuOnly;
    }
    #[cfg(windows)]
    {
        InferenceExecMode::DmlPreferred
    }
    #[cfg(not(windows))]
    {
        InferenceExecMode::CpuOnly
    }
}

fn preferred_intra_threads(max_cap: usize, exec_mode: InferenceExecMode) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);
    let ui_reserved = 1usize;
    let usable = cores.saturating_sub(ui_reserved).max(1);
    let target = match exec_mode {
        // DML path is usually bottlenecked by device dispatch; keep CPU side threads low.
        InferenceExecMode::DmlPreferred => usable.min(2),
        // CPU path: keep some parallelism but avoid monopolizing all cores.
        InferenceExecMode::CpuOnly => usable.min(4),
    };
    target.clamp(1, max_cap.max(1))
}

fn cpu_fold_parallelism(requested_folds: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);
    let usable = cores.saturating_sub(1).max(1);
    requested_folds.min(2).min(usable).max(1)
}

pub(super) fn analyze_music(
    model_dir: &Path,
    stems: &MusicStemSet,
    cancel_requested: &Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(String),
) -> Result<MusicAnalyzeOutput, String> {
    check_canceled(cancel_requested)?;
    let exec_mode = preferred_exec_mode();
    on_progress(format!("Analyze: EP: {}", exec_mode.progress_label()));
    on_progress("Analyze: resolving model files...".to_string());
    let model_spec = resolve_model_spec(model_dir)?;
    let mut cfg = model_spec.config.clone();
    on_progress("Analyze: building spectrogram...".to_string());
    let mut spec = extract_spectrogram_from_stems(stems, &cfg, cancel_requested)?;
    let original_frames = spec.len_of(Axis(1));
    if original_frames == 0 {
        return Err("Music analysis spectrogram has zero frames".to_string());
    }
    let export_frames = cfg
        .export_frames
        .unwrap_or(original_frames)
        .max(original_frames);
    if original_frames < export_frames {
        let mut padded =
            Array3::<f32>::zeros((spec.len_of(Axis(0)), export_frames, spec.len_of(Axis(2))));
        padded
            .slice_mut(s![.., 0..original_frames, ..])
            .assign(&spec);
        spec = padded;
    }

    let spec4 = spec.insert_axis(Axis(0));
    check_canceled(cancel_requested)?;
    let mut logits = run_onnx_ensemble(
        &model_spec.model_paths,
        &spec4,
        exec_mode,
        cancel_requested,
        on_progress,
    )?;
    if original_frames < export_frames {
        logits = trim_logits(&logits, original_frames)?;
    }
    if let Some(best) = model_spec.manifest_best_threshold_downbeat {
        cfg.best_threshold_downbeat = Some(best);
    }

    on_progress("Analyze: postprocessing...".to_string());
    check_canceled(cancel_requested)?;
    let analysis = postprocess(&logits, &cfg)?;
    let source_len = stems.len_samples().max(1);
    let to_sample = |sec: f32| -> usize {
        let pos = (sec.max(0.0) * stems.sample_rate as f32).round() as usize;
        pos.min(source_len.saturating_sub(1))
    };
    let beats: Vec<usize> = analysis.beats_sec.into_iter().map(to_sample).collect();
    let downbeats: Vec<usize> = analysis.downbeats_sec.into_iter().map(to_sample).collect();
    let sections: Vec<(usize, String)> = analysis
        .segments
        .into_iter()
        .map(|seg| (to_sample(seg.start_sec), seg.label))
        .collect();
    let estimated_bpm = estimate_bpm_from_beats_samples(&beats, stems.sample_rate);

    Ok(MusicAnalyzeOutput {
        result: MusicAnalysisResult {
            beats,
            downbeats,
            sections,
            estimated_bpm,
        },
        source_len_samples: source_len,
    })
}

fn resolve_model_spec(model_dir: &Path) -> Result<ModelSpec, String> {
    let preferred = [
        (
            "onnx/harmonix-all-20480.onnx",
            "onnx/harmonix-all-20480.json",
        ),
        ("onnx/harmonix-all.onnx", "onnx/harmonix-all.json"),
        ("onnx/harmonix-fold0.onnx", "onnx/harmonix-fold0.json"),
        (
            "onnx/folds/harmonix-fold0.onnx",
            "onnx/folds/harmonix-fold0.json",
        ),
    ];
    for (model_rel, cfg_rel) in preferred {
        let model_path = model_dir.join(model_rel);
        let config_path = model_dir.join(cfg_rel);
        if model_path.is_file() && config_path.is_file() {
            let cfg = read_fold_config(&config_path)?;
            return Ok(ModelSpec {
                model_paths: vec![model_path],
                config: cfg,
                manifest_best_threshold_downbeat: None,
            });
        }
    }

    let manifest_path = model_dir.join("onnx/ensemble_manifest.json");
    if manifest_path.is_file() {
        let manifest_text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("manifest read failed: {e}"))?;
        let manifest: EnsembleManifest = serde_json::from_str(&manifest_text)
            .map_err(|e| format!("manifest parse failed: {e}"))?;
        let mut model_paths = Vec::<PathBuf>::new();
        let mut config: Option<FoldConfig> = None;
        for (model_rel, cfg_rel) in manifest.models.iter().zip(manifest.configs.iter()) {
            let model_path = model_dir.join(model_rel);
            let config_path = model_dir.join(cfg_rel);
            if !model_path.is_file() || !config_path.is_file() {
                continue;
            }
            if config.is_none() {
                config = Some(read_fold_config(&config_path)?);
            }
            model_paths.push(model_path);
        }
        if !model_paths.is_empty() {
            return Ok(ModelSpec {
                model_paths,
                config: config.ok_or_else(|| "manifest config missing".to_string())?,
                manifest_best_threshold_downbeat: manifest.best_threshold_downbeat,
            });
        }
    }

    Err("music analyze model files are missing".to_string())
}

fn read_fold_config(path: &Path) -> Result<FoldConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("config read failed ({}): {e}", path.display()))?;
    serde_json::from_str::<FoldConfig>(&text)
        .map_err(|e| format!("config parse failed ({}): {e}", path.display()))
}

fn run_onnx_ensemble(
    model_paths: &[PathBuf],
    spec: &Array4<f32>,
    exec_mode: InferenceExecMode,
    cancel_requested: &Arc<AtomicBool>,
    on_progress: &mut dyn FnMut(String),
) -> Result<std::collections::HashMap<String, ArrayD<f32>>, String> {
    check_canceled(cancel_requested)?;
    if model_paths.is_empty() {
        return Err("empty model path list".to_string());
    }
    let total = model_paths.len();
    let worker_count = if exec_mode == InferenceExecMode::CpuOnly {
        cpu_fold_parallelism(total)
    } else {
        1
    };
    on_progress(format!(
        "Analyze: ONNX inference 0/{} (workers={worker_count})",
        total
    ));

    let ordered_logits: Vec<std::collections::HashMap<String, ArrayD<f32>>> = if worker_count <= 1 {
        let mut out = Vec::with_capacity(total);
        for (index, model_path) in model_paths.iter().enumerate() {
            check_canceled(cancel_requested)?;
            on_progress(format!("Analyze: ONNX inference {}/{}", index + 1, total));
            out.push(run_onnx_single_model(model_path, spec, exec_mode)?);
        }
        out
    } else {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let paths = Arc::new(model_paths.to_vec());
        let spec_shared = Arc::new(spec.clone());
        let next = Arc::new(AtomicUsize::new(0));
        let cancel_shared = Arc::clone(cancel_requested);
        let (tx, rx) = std::sync::mpsc::channel::<(
            usize,
            Result<std::collections::HashMap<String, ArrayD<f32>>, String>,
        )>();
        for _ in 0..worker_count {
            let paths = Arc::clone(&paths);
            let spec_shared = Arc::clone(&spec_shared);
            let next = Arc::clone(&next);
            let cancel_shared = Arc::clone(&cancel_shared);
            let tx = tx.clone();
            std::thread::spawn(move || {
                super::threading::lower_current_thread_priority();
                loop {
                    if cancel_shared.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= paths.len() {
                        break;
                    }
                    let path = &paths[idx];
                    let result = run_onnx_single_model(path, spec_shared.as_ref(), exec_mode);
                    let _ = tx.send((idx, result));
                }
            });
        }
        drop(tx);

        let mut done = 0usize;
        let mut ordered: Vec<Option<std::collections::HashMap<String, ArrayD<f32>>>> =
            vec![None; total];
        while done < total {
            if cancel_requested.load(Ordering::Relaxed) {
                return cancel_err();
            }
            let recv = rx.recv_timeout(std::time::Duration::from_millis(80));
            let (idx, result) = match recv {
                Ok(v) => v,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("onnx worker channel disconnected".to_string())
                }
            };
            let logits = result?;
            ordered[idx] = Some(logits);
            done += 1;
            on_progress(format!("Analyze: ONNX inference {done}/{total}"));
        }
        ordered
            .into_iter()
            .map(|entry| entry.ok_or_else(|| "onnx worker output missing".to_string()))
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut accum: std::collections::HashMap<String, ArrayD<f32>> =
        std::collections::HashMap::new();
    for logits in ordered_logits {
        if accum.is_empty() {
            accum = logits;
            continue;
        }
        for (name, value) in logits {
            let entry = accum
                .get_mut(&name)
                .ok_or_else(|| format!("missing logit in accumulator: {name}"))?;
            add_inplace(entry, &value)?;
        }
    }
    if model_paths.len() > 1 {
        let scale = 1.0 / model_paths.len() as f32;
        for value in accum.values_mut() {
            value.mapv_inplace(|v| v * scale);
        }
    }
    Ok(accum)
}

fn run_onnx_single_model(
    model_path: &Path,
    spec: &Array4<f32>,
    exec_mode: InferenceExecMode,
) -> Result<std::collections::HashMap<String, ArrayD<f32>>, String> {
    let mut session = commit_session(model_path, exec_mode)?;
    let input = Value::from_array(spec.clone()).map_err(|e| format!("ORT input failed: {e}"))?;
    let outputs = session
        .run([(&input).into()])
        .map_err(|e| format!("ORT run failed ({}): {e}", model_path.display()))?;
    let mut logits = std::collections::HashMap::<String, ArrayD<f32>>::new();
    for (name, value) in outputs.iter() {
        let arr = value
            .try_extract_array::<f32>()
            .map_err(|e| format!("extract output {name} failed: {e}"))?
            .to_owned()
            .into_dyn();
        logits.insert(name.to_string(), arr);
    }
    Ok(logits)
}

fn add_inplace(target: &mut ArrayD<f32>, other: &ArrayD<f32>) -> Result<(), String> {
    if target.shape() != other.shape() {
        return Err(format!(
            "logit shape mismatch: {:?} vs {:?}",
            target.shape(),
            other.shape()
        ));
    }
    Zip::from(target).and(other).for_each(|a, b| *a += *b);
    Ok(())
}

fn trim_logits(
    logits: &std::collections::HashMap<String, ArrayD<f32>>,
    frames: usize,
) -> Result<std::collections::HashMap<String, ArrayD<f32>>, String> {
    let mut trimmed = std::collections::HashMap::new();
    for (name, value) in logits {
        let sliced = match value.ndim() {
            3 => {
                let view = value
                    .view()
                    .into_dimensionality::<Ix3>()
                    .map_err(|e| format!("trim logits {name} dim3 failed: {e}"))?;
                view.slice(s![.., .., 0..frames]).to_owned().into_dyn()
            }
            2 => {
                let view = value
                    .view()
                    .into_dimensionality::<Ix2>()
                    .map_err(|e| format!("trim logits {name} dim2 failed: {e}"))?;
                view.slice(s![.., 0..frames]).to_owned().into_dyn()
            }
            1 => {
                let view = value
                    .view()
                    .into_dimensionality::<Ix1>()
                    .map_err(|e| format!("trim logits {name} dim1 failed: {e}"))?;
                view.slice(s![0..frames]).to_owned().into_dyn()
            }
            _ => value.clone(),
        };
        trimmed.insert(name.clone(), sliced);
    }
    Ok(trimmed)
}

fn squeeze_batch(arr: ArrayD<f32>) -> ArrayD<f32> {
    if arr.ndim() > 1 && arr.shape()[0] == 1 {
        arr.index_axis_move(Axis(0), 0).into_dyn()
    } else {
        arr
    }
}

fn postprocess(
    logits: &std::collections::HashMap<String, ArrayD<f32>>,
    cfg: &FoldConfig,
) -> Result<AnalysisResultSec, String> {
    let logits_beat = squeeze_batch(
        logits
            .get("logits_beat")
            .ok_or_else(|| "missing logits_beat".to_string())?
            .clone(),
    );
    let logits_downbeat = squeeze_batch(
        logits
            .get("logits_downbeat")
            .ok_or_else(|| "missing logits_downbeat".to_string())?
            .clone(),
    );
    let logits_section = squeeze_batch(
        logits
            .get("logits_section")
            .ok_or_else(|| "missing logits_section".to_string())?
            .clone(),
    );
    let logits_function = squeeze_batch(
        logits
            .get("logits_function")
            .ok_or_else(|| "missing logits_function".to_string())?
            .clone(),
    );

    let logits_beat = to_1d(logits_beat)?;
    let logits_downbeat = to_1d(logits_downbeat)?;
    let logits_section = to_1d(logits_section)?;
    let logits_function = to_2d(logits_function)?;

    let raw_prob_beats = sigmoid_array(&logits_beat);
    let raw_prob_downbeats = sigmoid_array(&logits_downbeat);
    let activations_beat = raw_prob_beats.clone();
    let activations_downbeat = raw_prob_downbeats.clone();
    let activations_no = (&Array1::from_elem(activations_beat.len(), 1.0) - &activations_beat
        + &Array1::from_elem(activations_downbeat.len(), 1.0)
        - &activations_downbeat)
        / 2.0;
    let mut activations_xbeat = &activations_beat - &activations_downbeat;
    activations_xbeat.mapv_inplace(|v| v.max(1e-8));
    let frames = activations_beat.len();
    let mut activations_combined = Array2::<f32>::zeros((frames, 3));
    for i in 0..frames {
        activations_combined[[i, 0]] = activations_xbeat[i];
        activations_combined[[i, 1]] = activations_downbeat[i];
        activations_combined[[i, 2]] = activations_no[i];
        let sum = activations_combined.row(i).sum();
        if sum > 0.0 {
            for j in 0..3 {
                activations_combined[[i, j]] /= sum;
            }
        }
    }
    let threshold = cfg
        .best_threshold_downbeat
        .or(cfg.threshold_downbeat)
        .unwrap_or(0.19);
    let postprocessor = dbn::DBNDownBeatTrackingProcessor::new(&[3, 4], threshold, cfg.fps)
        .map_err(|e| format!("DBN init failed: {e}"))?;
    let pred_downbeat_times =
        postprocessor.process(&activations_combined.slice(s![.., 0..2]).to_owned());

    let mut beats = Vec::new();
    let mut downbeats = Vec::new();
    for (time, beat_pos) in pred_downbeat_times {
        beats.push(time);
        if (beat_pos - 1.0).abs() < f32::EPSILON {
            downbeats.push(time);
        }
    }
    let raw_prob_sections = sigmoid_array(&logits_section);
    let raw_prob_functions = softmax_axis0(&logits_function);
    let filter_size = 4 * cfg.min_hops_per_beat + 1;
    let prob_sections = local_maxima(&raw_prob_sections, filter_size)?;
    let boundary_candidates = peak_picking(&prob_sections, 12 * cfg.fps, 12 * cfg.fps)?;
    let boundary: Vec<bool> = boundary_candidates.iter().map(|&v| v > 0.0).collect();
    let duration = prob_sections.len() as f32 * cfg.hop_size as f32 / cfg.sample_rate as f32;
    let mut pred_boundary_times = event_frames_to_time(&boundary, cfg.sample_rate, cfg.hop_size);
    if pred_boundary_times.is_empty() || pred_boundary_times[0] != 0.0 {
        pred_boundary_times.insert(0, 0.0);
    }
    if pred_boundary_times.last().copied().unwrap_or(0.0) != duration {
        pred_boundary_times.push(duration);
    }
    let mut pred_boundaries = Vec::new();
    for idx in 0..pred_boundary_times.len().saturating_sub(1) {
        pred_boundaries.push((pred_boundary_times[idx], pred_boundary_times[idx + 1]));
    }
    let mut boundary_indices = Vec::new();
    for (idx, &flag) in boundary.iter().enumerate() {
        if flag && idx > 0 {
            boundary_indices.push(idx);
        }
    }
    let mut labels = Vec::new();
    let mut start = 0usize;
    for &end in &boundary_indices {
        if end > start {
            let segment = raw_prob_functions.slice(s![.., start..end]);
            let mean = segment
                .mean_axis(Axis(1))
                .ok_or_else(|| "section mean failed".to_string())?;
            labels.push(argmax(&mean));
        }
        start = end;
    }
    if start < raw_prob_functions.len_of(Axis(1)) {
        let segment = raw_prob_functions.slice(s![.., start..]);
        let mean = segment
            .mean_axis(Axis(1))
            .ok_or_else(|| "tail section mean failed".to_string())?;
        labels.push(argmax(&mean));
    }
    let segments = pred_boundaries
        .into_iter()
        .zip(labels.into_iter())
        .map(|((start_sec, _end_sec), label_idx)| AnalysisSegment {
            start_sec,
            label: HARMONIX_LABELS
                .get(label_idx)
                .unwrap_or(&"unknown")
                .to_string(),
        })
        .collect::<Vec<_>>();

    Ok(AnalysisResultSec {
        beats_sec: beats,
        downbeats_sec: downbeats,
        segments,
    })
}

fn commit_session(path: &Path, exec_mode: InferenceExecMode) -> Result<Session, String> {
    let mut builder = Session::builder().map_err(|e| format!("ORT builder failed: {e}"))?;
    builder = builder
        .with_parallel_execution(false)
        .map_err(|e| format!("ORT parallel config failed: {e}"))?;
    builder = builder
        .with_inter_threads(1)
        .map_err(|e| format!("ORT inter-thread config failed: {e}"))?;
    builder = builder
        .with_intra_threads(preferred_intra_threads(8, exec_mode))
        .map_err(|e| format!("ORT intra-thread config failed: {e}"))?;

    let cpu = ep::CPU::default().build().fail_silently();
    #[cfg(windows)]
    let dml = ep::DirectML::default().build().fail_silently();

    let providers = {
        let mut out = Vec::new();
        #[cfg(windows)]
        {
            if exec_mode == InferenceExecMode::DmlPreferred {
                out.push(dml);
            }
        }
        out.push(cpu);
        out
    };

    let builder = builder
        .with_execution_providers(providers)
        .map_err(|e| format!("ORT execution provider setup failed: {e}"))?;

    match builder.commit_from_file(path) {
        Ok(session) => Ok(session),
        Err(first_err) => {
            let cpu_only = vec![ep::CPU::default().build().fail_silently()];
            let mut cpu_builder =
                Session::builder().map_err(|e| format!("ORT builder failed: {e}"))?;
            cpu_builder = cpu_builder
                .with_parallel_execution(false)
                .map_err(|e| format!("ORT parallel config failed: {e}"))?
                .with_inter_threads(1)
                .map_err(|e| format!("ORT inter-thread config failed: {e}"))?;
            cpu_builder = cpu_builder
                .with_intra_threads(preferred_intra_threads(8, InferenceExecMode::CpuOnly))
                .map_err(|e| format!("ORT intra-thread config failed: {e}"))?;
            let cpu_builder = cpu_builder
                .with_execution_providers(cpu_only)
                .map_err(|e| format!("ORT execution provider setup failed: {e}"))?;
            cpu_builder
                .commit_from_file(path)
                .map_err(|cpu_err| format!("{first_err}; CPU fallback failed: {cpu_err}"))
        }
    }
}

fn sigmoid_array(arr: &Array1<f32>) -> Array1<f32> {
    arr.mapv(|x| 1.0 / (1.0 + (-x).exp()))
}

fn softmax_axis0(arr: &Array2<f32>) -> Array2<f32> {
    let (rows, cols) = arr.dim();
    let mut out = Array2::<f32>::zeros((rows, cols));
    for col in 0..cols {
        let mut max_val = f32::NEG_INFINITY;
        for row in 0..rows {
            max_val = max_val.max(arr[[row, col]]);
        }
        let mut sum = 0.0;
        for row in 0..rows {
            let val = (arr[[row, col]] - max_val).exp();
            out[[row, col]] = val;
            sum += val;
        }
        if sum > 0.0 {
            for row in 0..rows {
                out[[row, col]] /= sum;
            }
        }
    }
    out
}

fn local_maxima(arr: &Array1<f32>, filter_size: usize) -> Result<Array1<f32>, String> {
    if filter_size % 2 == 0 {
        return Err("filter_size must be odd".to_string());
    }
    let pad = filter_size / 2;
    let n = arr.len();
    let mut padded = vec![f32::NEG_INFINITY; n + 2 * pad];
    for i in 0..n {
        padded[pad + i] = arr[i];
    }
    let mut output = Array1::<f32>::zeros(n);
    for i in 0..n {
        let window = &padded[i..i + filter_size];
        let max_val = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if window[pad] == max_val {
            output[i] = arr[i];
        }
    }
    Ok(output)
}

fn peak_picking(
    boundary: &Array1<f32>,
    window_past: usize,
    window_future: usize,
) -> Result<Array1<f32>, String> {
    let window_size = window_past + window_future;
    if window_size % 2 != 0 {
        return Err("window_past + window_future must be even".to_string());
    }
    let window_size = window_size + 1;
    let n = boundary.len();
    let mut padded = vec![0.0f32; n + window_past + window_future];
    for i in 0..n {
        padded[window_past + i] = boundary[i];
    }
    let mut local = vec![false; n];
    for i in 0..n {
        let window = &padded[i..i + window_size];
        let max_val = window.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if boundary[i] == max_val && boundary[i] > 0.0 {
            local[i] = true;
        }
    }
    let mut prefix = vec![0.0f32; padded.len() + 1];
    for i in 0..padded.len() {
        prefix[i + 1] = prefix[i] + padded[i];
    }
    let mut strength = vec![0.0f32; n];
    for i in 0..n {
        let past_start = i;
        let past_end = i + window_past;
        let future_start = i + window_past + 1;
        let future_end = future_start + window_future;
        let past_mean = if window_past > 0 {
            (prefix[past_end] - prefix[past_start]) / window_past as f32
        } else {
            0.0
        };
        let future_mean = if window_future > 0 {
            (prefix[future_end] - prefix[future_start]) / window_future as f32
        } else {
            0.0
        };
        strength[i] = boundary[i] - ((past_mean + future_mean) / 2.0);
    }
    let mut out = Array1::<f32>::zeros(n);
    for i in 0..n {
        if local[i] {
            out[i] = strength[i];
        }
    }
    Ok(out)
}

fn event_frames_to_time(frames: &[bool], sample_rate: usize, hop_size: usize) -> Vec<f32> {
    let mut out = Vec::new();
    for (idx, &flag) in frames.iter().enumerate() {
        if flag {
            out.push(idx as f32 * hop_size as f32 / sample_rate as f32);
        }
    }
    out
}

fn to_1d(arr: ArrayD<f32>) -> Result<Array1<f32>, String> {
    match arr.ndim() {
        1 => arr
            .into_dimensionality::<Ix1>()
            .map_err(|e| format!("to_1d dim1 failed: {e}")),
        2 => {
            let arr2 = arr
                .into_dimensionality::<Ix2>()
                .map_err(|e| format!("to_1d dim2 failed: {e}"))?;
            if arr2.nrows() == 1 {
                Ok(arr2.index_axis(Axis(0), 0).to_owned())
            } else if arr2.ncols() == 1 {
                Ok(arr2.column(0).to_owned())
            } else {
                Err(format!("expected 1D logits, got 2D {:?}", arr2.dim()))
            }
        }
        _ => Err(format!("expected 1D logits, got {}D", arr.ndim())),
    }
}

fn to_2d(arr: ArrayD<f32>) -> Result<Array2<f32>, String> {
    match arr.ndim() {
        2 => arr
            .into_dimensionality::<Ix2>()
            .map_err(|e| format!("to_2d dim2 failed: {e}")),
        3 => {
            let arr3 = arr
                .into_dimensionality::<Ix3>()
                .map_err(|e| format!("to_2d dim3 failed: {e}"))?;
            if arr3.len_of(Axis(0)) == 1 {
                Ok(arr3.index_axis(Axis(0), 0).to_owned())
            } else {
                Err(format!("expected 2D logits, got 3D {:?}", arr3.dim()))
            }
        }
        _ => Err(format!("expected 2D logits, got {}D", arr.ndim())),
    }
}

fn argmax(arr: &Array1<f32>) -> usize {
    let mut best_idx = 0usize;
    let mut best_val = f32::NEG_INFINITY;
    for (idx, &val) in arr.iter().enumerate() {
        if val > best_val {
            best_val = val;
            best_idx = idx;
        }
    }
    best_idx
}

fn extract_spectrogram_from_stems(
    stems: &MusicStemSet,
    cfg: &FoldConfig,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<Array3<f32>, String> {
    check_canceled(cancel_requested)?;
    let num_bins = cfg.window_size / 2;
    let bin_freqs = fft_frequencies(num_bins, cfg.sample_rate as u32);
    let filterbank =
        logarithmic_filterbank(&bin_freqs, cfg.num_bands, cfg.fmin, cfg.fmax, true, true);
    let window = hann_window(cfg.window_size);
    let stem_mono = [
        mixdown_channels(&stems.bass),
        mixdown_channels(&stems.drums),
        mixdown_channels(&stems.other),
        mixdown_channels(&stems.vocals),
    ];
    let stem_resampled: Vec<Vec<f32>> = stem_mono
        .iter()
        .map(|m| {
            if stems.sample_rate as usize == cfg.sample_rate {
                m.clone()
            } else {
                resample_linear(m, stems.sample_rate, cfg.sample_rate as u32)
            }
        })
        .collect();

    let mut specs = Vec::<Array2<f32>>::new();
    for stem in stem_resampled {
        check_canceled(cancel_requested)?;
        let frames = frame_signal(&stem, cfg.window_size, cfg.hop_size as f32, 0, "normal")
            .map_err(|e| format!("frame_signal failed: {e}"))?;
        let spectrum = stft_magnitude(&frames, cfg.window_size, &window, cancel_requested)
            .map_err(|e| format!("stft_magnitude failed: {e}"))?;
        let filtered = spectrum.dot(&filterbank);
        specs.push(log_spectrogram(&filtered, 1.0, 1.0));
    }

    let frames = specs.iter().map(|s| s.len_of(Axis(0))).min().unwrap_or(0);
    if frames == 0 {
        return Err("empty spectrogram frames".to_string());
    }
    let mut out = Array3::<f32>::zeros((4, frames, filterbank.len_of(Axis(1))));
    for (idx, spec) in specs.into_iter().enumerate() {
        out.slice_mut(s![idx, .., ..])
            .assign(&spec.slice(s![0..frames, ..]));
    }
    Ok(out)
}

fn fft_frequencies(num_fft_bins: usize, sample_rate: u32) -> Vec<f32> {
    let n = (num_fft_bins * 2) as f32;
    (0..num_fft_bins)
        .map(|i| i as f32 * sample_rate as f32 / n)
        .collect()
}

fn log_frequencies(bands_per_octave: usize, fmin: f32, fmax: f32, fref: f32) -> Vec<f32> {
    let left = (fmin / fref).log2() * bands_per_octave as f32;
    let right = (fmax / fref).log2() * bands_per_octave as f32;
    let left = left.floor() as i32;
    let right = right.ceil() as i32;
    let mut freqs = Vec::new();
    for i in left..right {
        let freq = fref * 2.0f32.powf(i as f32 / bands_per_octave as f32);
        freqs.push(freq);
    }
    freqs.retain(|&f| f >= fmin && f <= fmax);
    freqs
}

fn frequencies2bins(frequencies: &[f32], bin_frequencies: &[f32], unique_bins: bool) -> Vec<usize> {
    let mut indices = Vec::with_capacity(frequencies.len());
    for &freq in frequencies {
        let idx = bin_frequencies.partition_point(|&v| v < freq);
        let mut idx = idx.clamp(1, bin_frequencies.len() - 1);
        let left = bin_frequencies[idx - 1];
        let right = bin_frequencies[idx];
        if freq - left < right - freq {
            idx -= 1;
        }
        indices.push(idx);
    }
    if unique_bins {
        indices.sort_unstable();
        indices.dedup();
    }
    indices
}

fn triangular_filterbank(bins: &[usize], num_bins: usize, norm_filters: bool) -> Array2<f32> {
    let mut filters = Array2::<f32>::zeros((num_bins, bins.len().saturating_sub(2)));
    for i in 0..bins.len().saturating_sub(2) {
        let start = bins[i] as isize;
        let mut center = bins[i + 1] as isize;
        let mut stop = bins[i + 2] as isize;
        if stop - start < 2 {
            center = start;
            stop = start + 1;
        }
        let start_usize = start.max(0) as usize;
        let stop_usize = stop.max(0) as usize;
        let mut data = vec![0.0f32; stop_usize.saturating_sub(start_usize)];
        let up_len = (center - start).max(0) as usize;
        let down_len = (stop - center).max(0) as usize;
        if up_len > 0 {
            for idx in 0..up_len {
                data[idx] = idx as f32 / up_len as f32;
            }
        }
        if down_len > 0 {
            for idx in 0..down_len {
                let pos = up_len + idx;
                if pos < data.len() {
                    data[pos] = 1.0 - idx as f32 / down_len as f32;
                }
            }
        }
        if norm_filters {
            let sum: f32 = data.iter().sum();
            if sum > 0.0 {
                for v in &mut data {
                    *v /= sum;
                }
            }
        }
        for (offset, &val) in data.iter().enumerate() {
            let bin = start_usize + offset;
            if bin < num_bins {
                filters[[bin, i]] = val;
            }
        }
    }
    filters
}

fn logarithmic_filterbank(
    bin_frequencies: &[f32],
    num_bands: usize,
    fmin: f32,
    fmax: f32,
    norm_filters: bool,
    unique_filters: bool,
) -> Array2<f32> {
    let freqs = log_frequencies(num_bands, fmin, fmax, 440.0);
    let bins = frequencies2bins(&freqs, bin_frequencies, unique_filters);
    triangular_filterbank(&bins, bin_frequencies.len(), norm_filters)
}

fn hann_window(size: usize) -> Vec<f32> {
    if size == 0 {
        return Vec::new();
    }
    if size == 1 {
        return vec![1.0];
    }
    let mut window = Vec::with_capacity(size);
    let denom = (size - 1) as f32;
    for n in 0..size {
        let value = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * n as f32 / denom).cos());
        window.push(value);
    }
    window
}

fn frame_signal(
    signal: &[f32],
    frame_size: usize,
    hop_size: f32,
    origin: i32,
    end: &str,
) -> anyhow::Result<Array2<f32>> {
    if end != "normal" && end != "extend" {
        bail!("Unsupported end mode: {}", end);
    }
    let hop = hop_size as f64;
    let len = signal.len() as f64;
    let num_frames = if end == "extend" {
        (len / hop).floor() + 1.0
    } else {
        (len / hop).ceil()
    } as usize;
    let hop_int = hop_size.round() as usize;
    let use_fast = origin == 0 && (hop_size - hop_int as f32).abs() < 1e-6;
    let mut frames = Array2::<f32>::zeros((num_frames, frame_size));
    if use_fast {
        let pad_left = frame_size / 2;
        let ref_last = (num_frames.saturating_sub(1)) * hop_int;
        let pad_right = (ref_last + frame_size).saturating_sub(signal.len() + pad_left);
        let mut padded = vec![0.0f32; signal.len() + pad_left + pad_right];
        padded[pad_left..pad_left + signal.len()].copy_from_slice(signal);
        for i in 0..num_frames {
            let start = i * hop_int;
            let slice = &padded[start..start + frame_size];
            frames
                .slice_mut(s![i, ..])
                .assign(&Array1::from(slice.to_vec()));
        }
        return Ok(frames);
    }
    for i in 0..num_frames {
        let frame = signal_frame(signal, i, frame_size, hop_size, origin, 0.0);
        frames.slice_mut(s![i, ..]).assign(&Array1::from(frame));
    }
    Ok(frames)
}

fn signal_frame(
    signal: &[f32],
    index: usize,
    frame_size: usize,
    hop_size: f32,
    origin: i32,
    pad: f32,
) -> Vec<f32> {
    let num_samples = signal.len() as isize;
    let ref_sample = (index as f32 * hop_size) as isize;
    let mut start = ref_sample - frame_size as isize / 2 - origin as isize;
    let mut stop = start + frame_size as isize;
    if start >= 0 && stop <= num_samples {
        return signal[start as usize..stop as usize].to_vec();
    }
    let mut frame = vec![pad; frame_size];
    let mut left = 0isize;
    let mut right = 0isize;
    if start < 0 {
        left = std::cmp::min(stop, 0) - start;
        start = 0;
    }
    if stop > num_samples {
        right = stop - std::cmp::max(start, num_samples);
        stop = num_samples;
    }
    let left_usize = left.max(0) as usize;
    let right_usize = right.max(0) as usize;
    let start_usize = start.max(0) as usize;
    let stop_usize = stop.max(0) as usize;
    if stop_usize > start_usize {
        frame[left_usize..frame_size - right_usize]
            .copy_from_slice(&signal[start_usize..stop_usize]);
    }
    frame
}

fn stft_magnitude(
    frames: &Array2<f32>,
    fft_size: usize,
    window: &[f32],
    cancel_requested: &Arc<AtomicBool>,
) -> anyhow::Result<Array2<f32>> {
    let (num_frames, frame_size) = frames.dim();
    if window.len() != frame_size {
        bail!("window length does not match frame_size");
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let rfft = planner.plan_fft_forward(fft_size);
    let mut input = vec![0.0f32; fft_size];
    let mut output = rfft.make_output_vec();
    let num_bins = fft_size / 2;
    let mut spec = Array2::<f32>::zeros((num_frames, num_bins));
    for frame_idx in 0..num_frames {
        if (frame_idx & 0x0F) == 0 && cancel_requested.load(Ordering::Relaxed) {
            bail!("{MUSIC_ANALYZE_CANCELED}");
        }
        let frame = frames.row(frame_idx);
        for i in 0..frame_size {
            input[i] = frame[i] * window[i];
        }
        for i in frame_size..fft_size {
            input[i] = 0.0;
        }
        rfft.process(&mut input, &mut output)?;
        for bin in 0..num_bins {
            let c = output[bin];
            spec[[frame_idx, bin]] = (c.re * c.re + c.im * c.im).sqrt();
        }
    }
    Ok(spec)
}

fn log_spectrogram(spec: &Array2<f32>, mul: f32, add: f32) -> Array2<f32> {
    spec.mapv(|v| (v * mul + add).log10())
}

fn mixdown_channels(channels: &[Vec<f32>]) -> Vec<f32> {
    if channels.is_empty() {
        return Vec::new();
    }
    if channels.len() == 1 {
        return channels[0].clone();
    }
    let len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut out = vec![0.0f32; len];
    for ch in channels {
        for (i, v) in ch.iter().enumerate() {
            out[i] += *v;
        }
    }
    let inv = 1.0f32 / channels.len() as f32;
    for v in &mut out {
        *v *= inv;
    }
    out
}

fn resample_linear(input: &[f32], src_sr: u32, dst_sr: u32) -> Vec<f32> {
    if input.is_empty() || src_sr == 0 || dst_sr == 0 || src_sr == dst_sr {
        return input.to_vec();
    }
    let ratio = dst_sr as f64 / src_sr as f64;
    let out_len = ((input.len() as f64) * ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = (i as f64) / ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

fn estimate_bpm_from_beats_samples(beats: &[usize], sample_rate: u32) -> Option<f32> {
    if sample_rate == 0 || beats.len() < 2 {
        return None;
    }
    let mut intervals_sec = Vec::<f32>::new();
    for w in beats.windows(2) {
        let a = w[0];
        let b = w[1];
        if b <= a {
            continue;
        }
        let sec = (b - a) as f32 / sample_rate as f32;
        if !sec.is_finite() || sec <= 0.0 {
            continue;
        }
        let bpm = 60.0 / sec;
        if (20.0..=300.0).contains(&bpm) {
            intervals_sec.push(sec);
        }
    }
    if intervals_sec.is_empty() {
        return None;
    }
    intervals_sec.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_sec = if intervals_sec.len() % 2 == 1 {
        intervals_sec[intervals_sec.len() / 2]
    } else {
        let hi = intervals_sec.len() / 2;
        (intervals_sec[hi - 1] + intervals_sec[hi]) * 0.5
    };
    if !median_sec.is_finite() || median_sec <= 0.0 {
        return None;
    }
    let mut bpm = 60.0 / median_sec;
    while bpm < 60.0 {
        bpm *= 2.0;
    }
    while bpm > 200.0 {
        bpm *= 0.5;
    }
    if bpm.is_finite() && bpm > 0.0 {
        Some(bpm)
    } else {
        None
    }
}

const DEMUCS_FFT_WINDOW_SIZE: usize = 4096;
const DEMUCS_FFT_HOP_SIZE: usize = 1024;
const DEMUCS_OVERLAP: f32 = 0.25;
const DEMUCS_MAX_SHIFT_SECS: f32 = 0.5;
const DEMUCS_TRANSITION_POWER: f32 = 1.0;

const DEMUCS_SOURCES_4: [&str; 4] = ["drums", "bass", "other", "vocals"];
const DEMUCS_SOURCES_6: [&str; 6] = ["drums", "bass", "other", "vocals", "guitar", "piano"];

struct DemucsOnnx {
    session: Session,
    input_names: Vec<String>,
    segment_samples: usize,
    nb_sources: usize,
    sources: Vec<String>,
    pad: usize,
    pad_end: usize,
    padded_segment_samples: usize,
    stft_frames: usize,
    nb_bins: usize,
    window: Vec<f32>,
    normalized_window: Vec<f32>,
    rfft: Arc<dyn RealToComplex<f32>>,
    irfft: Arc<dyn ComplexToReal<f32>>,
}

impl DemucsOnnx {
    fn new(model_path: &Path, exec_mode: InferenceExecMode) -> anyhow::Result<Self> {
        if !model_path.is_file() {
            bail!("Demucs ONNX model not found: {}", model_path.display());
        }
        let session = commit_demucs_session(model_path, exec_mode)?;
        let input_names = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect::<Vec<_>>();
        if input_names.len() < 2 {
            bail!("Demucs ONNX model must have 2 inputs");
        }

        if session.outputs().len() < 2 {
            bail!("Demucs ONNX model must have 2 outputs");
        }

        let input_shape = session.inputs()[0]
            .dtype()
            .tensor_shape()
            .context("Demucs input shape missing")?;
        if input_shape.len() < 3 {
            bail!("Unexpected demucs input shape");
        }
        let segment_samples = input_shape[2];
        if segment_samples <= 0 {
            bail!("Demucs segment length is dynamic or invalid");
        }
        let segment_samples = segment_samples as usize;

        let out_shape = session.outputs()[0]
            .dtype()
            .tensor_shape()
            .context("Demucs output shape missing")?;
        if out_shape.len() < 2 {
            bail!("Unexpected demucs output shape");
        }
        let nb_sources = out_shape[1];
        if nb_sources <= 0 {
            bail!("Demucs sources dimension is dynamic or invalid");
        }
        let nb_sources = nb_sources as usize;

        let sources = if nb_sources == 6 {
            DEMUCS_SOURCES_6.iter().map(|s| s.to_string()).collect()
        } else {
            DEMUCS_SOURCES_4.iter().map(|s| s.to_string()).collect()
        };

        let pad = (DEMUCS_FFT_HOP_SIZE / 2) * 3;
        let le = (segment_samples as f32 / DEMUCS_FFT_HOP_SIZE as f32).ceil() as usize;
        let pad_end = pad + le * DEMUCS_FFT_HOP_SIZE - segment_samples;
        let padded_segment_samples = segment_samples + pad + pad_end;
        let stft_frames = padded_segment_samples / DEMUCS_FFT_HOP_SIZE + 1;
        let nb_bins = DEMUCS_FFT_WINDOW_SIZE / 2 + 1;

        let window = demucs_hann_window();
        let normalized_window = demucs_normalized_window(&window, stft_frames);

        let mut planner = RealFftPlanner::<f32>::new();
        let rfft = planner.plan_fft_forward(DEMUCS_FFT_WINDOW_SIZE);
        let irfft = planner.plan_fft_inverse(DEMUCS_FFT_WINDOW_SIZE);

        Ok(Self {
            session,
            input_names,
            segment_samples,
            nb_sources,
            sources,
            pad,
            pad_end,
            padded_segment_samples,
            stft_frames,
            nb_bins,
            window,
            normalized_window,
            rfft,
            irfft,
        })
    }

    fn sources(&self) -> &[String] {
        &self.sources
    }

    fn separate(
        &mut self,
        audio: &Array2<f32>,
        shift_offset: Option<usize>,
        cancel_requested: &Arc<AtomicBool>,
        mut on_chunk_progress: impl FnMut(usize, usize),
    ) -> anyhow::Result<Array3<f32>> {
        if audio.nrows() != 2 {
            bail!("audio must have shape (2, samples)");
        }
        let length = audio.len_of(Axis(1));
        let max_shift = (DEMUCS_MAX_SHIFT_SECS * DEMUCS_SUPPORTED_SAMPLE_RATE as f32) as usize;

        let mut mean_sum = 0.0f32;
        for i in 0..length {
            mean_sum += (audio[[0, i]] + audio[[1, i]]) * 0.5;
        }
        let ref_mean = mean_sum / length.max(1) as f32;
        let mut var_sum = 0.0f32;
        for i in 0..length {
            let m = (audio[[0, i]] + audio[[1, i]]) * 0.5;
            var_sum += (m - ref_mean) * (m - ref_mean);
        }
        let mut ref_std = if length > 1 {
            (var_sum / (length as f32 - 1.0)).sqrt()
        } else {
            1.0
        };
        if ref_std == 0.0 {
            ref_std = 1.0;
        }

        let padded_length = length + 2 * max_shift;
        let mut padded_mix = Array2::<f32>::zeros((2, padded_length));
        for ch in 0..2 {
            for i in 0..length {
                padded_mix[[ch, max_shift + i]] = (audio[[ch, i]] - ref_mean) / ref_std;
            }
        }

        let shift_offset = if let Some(offset) = shift_offset {
            if offset >= max_shift {
                bail!("demucs_shift must be in [0, {}), got {}", max_shift, offset);
            }
            offset
        } else {
            rand::thread_rng().gen_range(0..max_shift.max(1))
        };
        let shifted_length = length + max_shift.saturating_sub(shift_offset);
        let shifted_audio = padded_mix
            .slice(s![.., shift_offset..shift_offset + shifted_length])
            .to_owned();

        let stride_samples = ((1.0 - DEMUCS_OVERLAP) * self.segment_samples as f32) as usize;
        let mut out = Array3::<f32>::zeros((self.nb_sources, 2, shifted_length));
        let mut sum_weight = vec![0.0f32; shifted_length];
        let weight = demucs_weight_window(self.segment_samples);
        let total_chunks = if stride_samples == 0 {
            1
        } else {
            shifted_length.saturating_add(stride_samples.saturating_sub(1)) / stride_samples
        };

        let mut offset = 0usize;
        let mut chunk_index = 0usize;
        while offset < shifted_length {
            if cancel_requested.load(Ordering::Relaxed) {
                bail!("{MUSIC_ANALYZE_CANCELED}");
            }
            chunk_index = chunk_index.saturating_add(1);
            on_chunk_progress(chunk_index, total_chunks.max(1));
            let chunk_length = (shifted_length - offset).min(self.segment_samples);
            let chunk_out = self.chunk_infer(&shifted_audio, offset, chunk_length)?;
            if cancel_requested.load(Ordering::Relaxed) {
                bail!("{MUSIC_ANALYZE_CANCELED}");
            }

            for source in 0..self.nb_sources {
                for ch in 0..2 {
                    for i in 0..chunk_length {
                        out[[source, ch, offset + i]] += weight[i] * chunk_out[[source, ch, i]];
                    }
                }
            }
            for i in 0..chunk_length {
                sum_weight[offset + i] += weight[i];
            }

            if stride_samples == 0 {
                break;
            }
            offset += stride_samples;
        }

        for source in 0..self.nb_sources {
            for ch in 0..2 {
                for i in 0..shifted_length {
                    if sum_weight[i] > 0.0 {
                        out[[source, ch, i]] /= sum_weight[i];
                    }
                    out[[source, ch, i]] = out[[source, ch, i]] * ref_std + ref_mean;
                }
            }
        }

        let trim_start = max_shift.saturating_sub(shift_offset);
        Ok(out
            .slice(s![.., .., trim_start..trim_start + length])
            .to_owned())
    }

    fn chunk_infer(
        &mut self,
        shifted_audio: &Array2<f32>,
        segment_offset: usize,
        chunk_length: usize,
    ) -> anyhow::Result<Array3<f32>> {
        let mut mix_segment = Array2::<f32>::zeros((2, self.segment_samples));
        let delta = self.segment_samples - chunk_length;
        let start = segment_offset as isize - (delta / 2) as isize;
        let end = start + self.segment_samples as isize;
        let correct_start = start.max(0) as usize;
        let correct_end = end.min(shifted_audio.len_of(Axis(1)) as isize) as usize;
        let pad_left = (correct_start as isize - start) as usize;
        if correct_end > correct_start {
            for ch in 0..2 {
                let src = shifted_audio.slice(s![ch, correct_start..correct_end]);
                let mut dst = mix_segment
                    .slice_mut(s![ch, pad_left..pad_left + (correct_end - correct_start)]);
                dst.assign(&src);
            }
        }

        let mut padded_mix = Array2::<f32>::zeros((2, self.padded_segment_samples));
        padded_mix
            .slice_mut(s![.., self.pad..self.pad + self.segment_samples])
            .assign(&mix_segment);
        demucs_reflect_padding(
            &mut padded_mix,
            self.pad,
            self.pad_end,
            self.segment_samples,
        );

        let (x_input, xt_input) = self.prepare_inputs(&padded_mix, &mix_segment)?;
        let (x_out, xt_out) = self.onnx_infer(&x_input, &xt_input)?;

        let freq_bins = self.nb_bins - 1;
        let frames = self.stft_frames - 4;
        let mut sources = Array3::<f32>::zeros((self.nb_sources, 2, self.segment_samples));

        for source in 0..self.nb_sources {
            let mut z_target_0 = Array2::<Complex32>::zeros((self.nb_bins, self.stft_frames));
            let mut z_target_1 = Array2::<Complex32>::zeros((self.nb_bins, self.stft_frames));
            for b in 0..freq_bins {
                for t in 0..frames {
                    let real0 = x_out[[0, source, 0, b, t]];
                    let imag0 = x_out[[0, source, 1, b, t]];
                    let real1 = x_out[[0, source, 2, b, t]];
                    let imag1 = x_out[[0, source, 3, b, t]];
                    z_target_0[[b, t + 2]] = Complex32::new(real0, imag0);
                    z_target_1[[b, t + 2]] = Complex32::new(real1, imag1);
                }
            }

            let padded_wave_0 = self.istft_channel(&z_target_0)?;
            let padded_wave_1 = self.istft_channel(&z_target_1)?;

            for i in 0..self.segment_samples {
                sources[[source, 0, i]] = padded_wave_0[self.pad + i] + xt_out[[0, source, 0, i]];
                sources[[source, 1, i]] = padded_wave_1[self.pad + i] + xt_out[[0, source, 1, i]];
            }
        }

        if chunk_length < self.segment_samples {
            let trim_start = (self.segment_samples - chunk_length) / 2;
            let trim_end = trim_start + chunk_length;
            Ok(sources.slice(s![.., .., trim_start..trim_end]).to_owned())
        } else {
            Ok(sources)
        }
    }

    fn prepare_inputs(
        &self,
        padded_mix: &Array2<f32>,
        mix_segment: &Array2<f32>,
    ) -> anyhow::Result<(Array4<f32>, Array3<f32>)> {
        let z0 = self.stft_channel(
            padded_mix
                .row(0)
                .as_slice()
                .context("Invalid padded mix slice")?,
        )?;
        let z1 = self.stft_channel(
            padded_mix
                .row(1)
                .as_slice()
                .context("Invalid padded mix slice")?,
        )?;

        if z0.len_of(Axis(0)) != self.nb_bins || z0.len_of(Axis(1)) != self.stft_frames {
            bail!("Unexpected FFT bins in Demucs STFT");
        }

        let freq_bins = self.nb_bins - 1;
        let frames = self.stft_frames - 4;
        let mut x_input = Array4::<f32>::zeros((1, 4, freq_bins, frames));
        for b in 0..freq_bins {
            for t in 0..frames {
                let v0 = z0[[b, t + 2]];
                let v1 = z1[[b, t + 2]];
                x_input[[0, 0, b, t]] = v0.re;
                x_input[[0, 1, b, t]] = v0.im;
                x_input[[0, 2, b, t]] = v1.re;
                x_input[[0, 3, b, t]] = v1.im;
            }
        }

        let mut xt_input = Array3::<f32>::zeros((1, 2, self.segment_samples));
        xt_input.slice_mut(s![0, .., ..]).assign(mix_segment);

        Ok((x_input, xt_input))
    }

    fn onnx_infer(
        &mut self,
        x_input: &Array4<f32>,
        xt_input: &Array3<f32>,
    ) -> anyhow::Result<(Array5<f32>, Array4<f32>)> {
        let input_a = Value::from_array((
            vec![1usize, 2usize, self.segment_samples],
            xt_input.iter().copied().collect::<Vec<_>>(),
        ))?;
        let input_b = Value::from_array((
            vec![1usize, 4usize, self.nb_bins - 1, self.stft_frames - 4],
            x_input.iter().copied().collect::<Vec<_>>(),
        ))?;

        let mut run_inputs: Vec<(String, DynValue)> = Vec::with_capacity(2);
        // Model exported from demucs.onnx expects waveform input first in known exports.
        // Keep name-based wiring to remain robust if input order differs.
        let first = self
            .input_names
            .first()
            .cloned()
            .unwrap_or_else(|| "input_0".to_string());
        let second = self
            .input_names
            .get(1)
            .cloned()
            .unwrap_or_else(|| "input_1".to_string());
        run_inputs.push((first, input_a.into()));
        run_inputs.push((second, input_b.into()));

        let outputs = self.session.run(run_inputs)?;
        let x_out = outputs[0].try_extract_array::<f32>()?.to_owned();
        let xt_out = outputs[1].try_extract_array::<f32>()?.to_owned();
        Ok((
            x_out.into_dimensionality::<Ix5>()?,
            xt_out.into_dimensionality::<Ix4>()?,
        ))
    }

    fn stft_channel(&self, signal: &[f32]) -> anyhow::Result<Array2<Complex32>> {
        let pad = DEMUCS_FFT_WINDOW_SIZE / 2;
        let mut padded = vec![0.0f32; signal.len() + 2 * pad];
        padded[pad..pad + signal.len()].copy_from_slice(signal);
        for i in 0..pad {
            padded[pad - 1 - i] = signal[i];
            padded[pad + signal.len() + i] = signal[signal.len() - 1 - i];
        }

        let frame_count = (padded.len() - DEMUCS_FFT_WINDOW_SIZE) / DEMUCS_FFT_HOP_SIZE + 1;
        let mut spec = Array2::<Complex32>::zeros((self.nb_bins, frame_count));
        let mut input = vec![0.0f32; DEMUCS_FFT_WINDOW_SIZE];
        let mut output = self.rfft.make_output_vec();
        let scale = 1.0 / (DEMUCS_FFT_WINDOW_SIZE as f32).sqrt();

        for frame_idx in 0..frame_count {
            let start = frame_idx * DEMUCS_FFT_HOP_SIZE;
            let slice = &padded[start..start + DEMUCS_FFT_WINDOW_SIZE];
            for i in 0..DEMUCS_FFT_WINDOW_SIZE {
                input[i] = slice[i] * self.window[i];
            }
            self.rfft.process(&mut input, &mut output)?;
            for bin in 0..self.nb_bins {
                spec[[bin, frame_idx]] = output[bin] * scale;
            }
        }
        Ok(spec)
    }

    fn istft_channel(&self, spec: &Array2<Complex32>) -> anyhow::Result<Vec<f32>> {
        let frames = spec.len_of(Axis(1));
        let out_len = DEMUCS_FFT_WINDOW_SIZE + DEMUCS_FFT_HOP_SIZE * (frames - 1);
        let mut out = vec![0.0f32; out_len];
        let mut input = self.irfft.make_input_vec();
        let mut output = vec![0.0f32; DEMUCS_FFT_WINDOW_SIZE];
        let scale = (DEMUCS_FFT_WINDOW_SIZE as f32).sqrt();

        for frame_idx in 0..frames {
            for bin in 0..self.nb_bins {
                input[bin] = spec[[bin, frame_idx]] * scale;
            }
            if self.nb_bins > 0 {
                input[0].im = 0.0;
            }
            if self.nb_bins > 1 {
                input[self.nb_bins - 1].im = 0.0;
            }
            self.irfft.process(&mut input, &mut output)?;
            let start = frame_idx * DEMUCS_FFT_HOP_SIZE;
            for i in 0..DEMUCS_FFT_WINDOW_SIZE {
                let norm = self.normalized_window[start + i] + 1e-8;
                out[start + i] += output[i] * self.window[i] / norm;
            }
        }

        let pad = DEMUCS_FFT_WINDOW_SIZE / 2;
        Ok(out[pad..out_len - pad].to_vec())
    }
}

fn commit_demucs_session(path: &Path, exec_mode: InferenceExecMode) -> anyhow::Result<Session> {
    let mut builder = Session::builder()?;
    builder = builder.with_parallel_execution(false)?;
    builder = builder.with_inter_threads(1)?;
    builder = builder.with_intra_threads(preferred_intra_threads(8, exec_mode))?;

    let cpu = ep::CPU::default().build().fail_silently();
    #[cfg(windows)]
    let dml = ep::DirectML::default().build().fail_silently();

    let providers = {
        let mut out = Vec::new();
        #[cfg(windows)]
        {
            if exec_mode == InferenceExecMode::DmlPreferred {
                out.push(dml);
            }
        }
        out.push(cpu);
        out
    };
    builder = builder.with_execution_providers(providers)?;
    match builder.commit_from_file(path) {
        Ok(session) => Ok(session),
        Err(first) => {
            let cpu_only = vec![ep::CPU::default().build().fail_silently()];
            let mut cpu_builder = Session::builder()?;
            cpu_builder = cpu_builder.with_parallel_execution(false)?;
            cpu_builder = cpu_builder.with_inter_threads(1)?;
            cpu_builder = cpu_builder
                .with_intra_threads(preferred_intra_threads(8, InferenceExecMode::CpuOnly))?;
            cpu_builder = cpu_builder.with_execution_providers(cpu_only)?;
            cpu_builder
                .commit_from_file(path)
                .map_err(|e| anyhow::anyhow!("{first}; CPU fallback failed: {e}"))
        }
    }
}

fn demucs_hann_window() -> Vec<f32> {
    let n = DEMUCS_FFT_WINDOW_SIZE as f32;
    (0..DEMUCS_FFT_WINDOW_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / n).cos()))
        .collect()
}

fn demucs_normalized_window(window: &[f32], nb_frames: usize) -> Vec<f32> {
    let length = DEMUCS_FFT_WINDOW_SIZE + DEMUCS_FFT_HOP_SIZE * (nb_frames - 1);
    let mut norm = vec![0.0f32; length];
    for i in 0..nb_frames {
        let start = i * DEMUCS_FFT_HOP_SIZE;
        for j in 0..DEMUCS_FFT_WINDOW_SIZE {
            norm[start + j] += window[j] * window[j];
        }
    }
    norm
}

fn demucs_reflect_padding(
    padded_mix: &mut Array2<f32>,
    pad: usize,
    pad_end: usize,
    segment_samples: usize,
) {
    for i in 0..pad {
        for ch in 0..2 {
            padded_mix[[ch, pad - 1 - i]] = padded_mix[[ch, pad + i]];
        }
    }
    let last_elem = segment_samples + pad - 1;
    for i in 0..pad_end {
        for ch in 0..2 {
            padded_mix[[ch, last_elem + i + 1]] = padded_mix[[ch, last_elem - i]];
        }
    }
}

fn demucs_weight_window(segment_samples: usize) -> Vec<f32> {
    let half = segment_samples / 2;
    let mut weight = demucs_linspace(1.0, half as f32, half);
    weight.extend(demucs_linspace(
        (segment_samples - half) as f32,
        1.0,
        segment_samples - half,
    ));
    let max_val = weight.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for v in &mut weight {
        *v = (*v / max_val).powf(DEMUCS_TRANSITION_POWER);
    }
    weight
}

fn demucs_linspace(start: f32, end: f32, count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![start];
    }
    let step = (end - start) / (count - 1) as f32;
    (0..count).map(|i| start + step * i as f32).collect()
}

mod dbn {
    use super::*;
    use anyhow::{bail, Result};
    use std::collections::HashMap;

    const MIN_BPM: f32 = 55.0;
    const MAX_BPM: f32 = 215.0;
    const NUM_TEMPI: usize = 60;
    const TRANSITION_LAMBDA: f32 = 100.0;
    const OBSERVATION_LAMBDA: usize = 16;
    const TRANSITION_THRESHOLD: f32 = 1e-16;

    pub struct BeatStateSpace {
        pub num_states: usize,
        pub first_states: Vec<usize>,
        pub last_states: Vec<usize>,
        pub state_positions: Vec<f32>,
        pub state_intervals: Vec<usize>,
    }

    impl BeatStateSpace {
        pub fn new(min_interval: f32, max_interval: f32, num_intervals: Option<usize>) -> Self {
            let mut intervals: Vec<usize> =
                (min_interval.round() as usize..=max_interval.round() as usize).collect();
            if let Some(num_intervals) = num_intervals {
                if num_intervals < intervals.len() {
                    let mut num_log_intervals = num_intervals;
                    loop {
                        let min_log = min_interval.log2();
                        let max_log = max_interval.log2();
                        let mut set = std::collections::BTreeSet::new();
                        for i in 0..num_log_intervals {
                            let ratio = if num_log_intervals == 1 {
                                0.0
                            } else {
                                i as f32 / (num_log_intervals - 1) as f32
                            };
                            let val = 2.0f32.powf(min_log + (max_log - min_log) * ratio);
                            set.insert(val.round() as usize);
                        }
                        if set.len() >= num_intervals {
                            intervals = set.into_iter().collect();
                            break;
                        }
                        num_log_intervals += 1;
                    }
                }
            }

            let num_states: usize = intervals.iter().sum();
            let num_intervals = intervals.len();
            let mut first_states = Vec::with_capacity(num_intervals);
            let mut last_states = Vec::with_capacity(num_intervals);
            let mut cumsum = 0usize;
            for &interval in &intervals {
                first_states.push(cumsum);
                cumsum += interval;
                last_states.push(cumsum - 1);
            }

            let mut state_positions = Vec::with_capacity(num_states);
            let mut state_intervals = Vec::with_capacity(num_states);
            for &interval in &intervals {
                for i in 0..interval {
                    state_positions.push(i as f32 / interval as f32);
                    state_intervals.push(interval);
                }
            }

            Self {
                num_states,
                first_states,
                last_states,
                state_positions,
                state_intervals,
            }
        }
    }

    pub struct BarStateSpace {
        pub num_beats: usize,
        pub state_positions: Vec<f32>,
        pub state_intervals: Vec<usize>,
        pub num_states: usize,
        pub first_states: Vec<Vec<usize>>,
        pub last_states: Vec<Vec<usize>>,
    }

    impl BarStateSpace {
        pub fn new(
            num_beats: usize,
            min_interval: f32,
            max_interval: f32,
            num_intervals: Option<usize>,
        ) -> Self {
            let bss = BeatStateSpace::new(min_interval, max_interval, num_intervals);
            let mut state_positions = Vec::new();
            let mut state_intervals = Vec::new();
            let mut first_states = Vec::new();
            let mut last_states = Vec::new();
            let mut num_states = 0usize;

            for beat in 0..num_beats {
                state_positions.extend(bss.state_positions.iter().map(|v| v + beat as f32));
                state_intervals.extend(&bss.state_intervals);
                first_states.push(bss.first_states.iter().map(|v| v + num_states).collect());
                last_states.push(bss.last_states.iter().map(|v| v + num_states).collect());
                num_states += bss.num_states;
            }

            Self {
                num_beats,
                state_positions,
                state_intervals,
                num_states,
                first_states,
                last_states,
            }
        }
    }

    fn exponential_transition(
        from_intervals: &[usize],
        to_intervals: &[usize],
        transition_lambda: Option<f32>,
        threshold: f32,
    ) -> Vec<Vec<f32>> {
        if transition_lambda.is_none() {
            let size = from_intervals.len().min(to_intervals.len());
            let mut matrix = vec![vec![0.0f32; to_intervals.len()]; from_intervals.len()];
            for (i, row) in matrix.iter_mut().enumerate().take(size) {
                row[i] = 1.0;
            }
            return matrix;
        }

        let lambda = transition_lambda.unwrap_or(0.0);
        let mut prob = vec![vec![0.0f32; to_intervals.len()]; from_intervals.len()];
        for (i, &from_int) in from_intervals.iter().enumerate() {
            for (j, &to_int) in to_intervals.iter().enumerate() {
                let ratio = to_int as f32 / from_int as f32;
                let value = (-lambda * (ratio - 1.0).abs()).exp();
                prob[i][j] = if value <= threshold { 0.0 } else { value };
            }
            let sum: f32 = prob[i].iter().sum();
            if sum > 0.0 {
                for v in &mut prob[i] {
                    *v /= sum;
                }
            }
        }
        prob
    }

    pub struct BarTransitionModel {
        pub state_space: BarStateSpace,
        pub pointers: Vec<usize>,
        pub prev_states: Vec<usize>,
        pub probabilities: Vec<f32>,
    }

    impl BarTransitionModel {
        pub fn new(state_space: BarStateSpace, transition_lambda: f32) -> Result<Self> {
            let mut states: Vec<usize> = (0..state_space.num_states).collect();
            let mut first_set = std::collections::HashSet::new();
            for group in &state_space.first_states {
                for &s in group {
                    first_set.insert(s);
                }
            }
            states.retain(|s| !first_set.contains(s));
            let mut prev_states: Vec<usize> = states.iter().map(|s| s - 1).collect();
            let mut probabilities: Vec<f32> = vec![1.0; states.len()];

            for beat in 0..state_space.num_beats {
                let to_states = &state_space.first_states[beat];
                let from_states = &state_space.last_states
                    [(beat + state_space.num_beats - 1) % state_space.num_beats];
                let from_int: Vec<usize> = from_states
                    .iter()
                    .map(|&s| state_space.state_intervals[s])
                    .collect();
                let to_int: Vec<usize> = to_states
                    .iter()
                    .map(|&s| state_space.state_intervals[s])
                    .collect();
                let prob = exponential_transition(
                    &from_int,
                    &to_int,
                    Some(transition_lambda),
                    TRANSITION_THRESHOLD,
                );
                for (from_idx, row) in prob.iter().enumerate() {
                    for (to_idx, &p) in row.iter().enumerate() {
                        if p > 0.0 {
                            states.push(to_states[to_idx]);
                            prev_states.push(from_states[from_idx]);
                            probabilities.push(p);
                        }
                    }
                }
            }

            let (pointers, prev_states, probabilities) = make_sparse(
                state_space.num_states,
                &states,
                &prev_states,
                &probabilities,
            )?;

            Ok(Self {
                state_space,
                pointers,
                prev_states,
                probabilities,
            })
        }
    }

    fn make_sparse(
        num_states: usize,
        states: &[usize],
        prev_states: &[usize],
        probabilities: &[f32],
    ) -> Result<(Vec<usize>, Vec<usize>, Vec<f32>)> {
        let mut map = HashMap::new();
        for ((&s, &p), &prob) in states.iter().zip(prev_states).zip(probabilities) {
            let entry = map.entry((s, p)).or_insert(0.0f32);
            *entry += prob;
        }
        if map.is_empty() {
            bail!("No transitions found.");
        }
        let mut entries: Vec<(usize, usize, f32)> =
            map.into_iter().map(|((s, p), prob)| (s, p, prob)).collect();
        entries.sort_by_key(|(s, p, _)| (*s, *p));

        let mut counts = vec![0usize; num_states];
        for (s, _, _) in &entries {
            counts[*s] += 1;
        }
        let mut pointers = Vec::with_capacity(num_states + 1);
        pointers.push(0);
        for count in counts {
            let last = *pointers.last().unwrap_or(&0);
            pointers.push(last + count);
        }

        let prev_states = entries.iter().map(|(_, p, _)| *p).collect();
        let probabilities = entries.iter().map(|(_, _, prob)| *prob).collect();
        Ok((pointers, prev_states, probabilities))
    }

    pub struct RNNDownBeatTrackingObservationModel {
        pub pointers: Vec<usize>,
        observation_lambda: usize,
    }

    impl RNNDownBeatTrackingObservationModel {
        pub fn new(state_space: &BarStateSpace, observation_lambda: usize) -> Self {
            let border = 1.0 / observation_lambda as f32;
            let mut pointers = vec![0usize; state_space.num_states];
            for (idx, &pos) in state_space.state_positions.iter().enumerate() {
                if (pos % 1.0) < border {
                    pointers[idx] = 1;
                }
                if pos < border {
                    pointers[idx] = 2;
                }
            }
            Self {
                pointers,
                observation_lambda,
            }
        }

        pub fn log_densities(&self, observations: &Array2<f32>) -> Vec<[f32; 3]> {
            let mut log_densities = Vec::with_capacity(observations.nrows());
            let eps = f32::EPSILON;
            for row in observations.outer_iter() {
                let no_beat = 1.0 - (row[0] + row[1]);
                let mut entry = [0.0f32; 3];
                entry[0] = (no_beat / (self.observation_lambda as f32 - 1.0))
                    .max(eps)
                    .ln();
                entry[1] = row[0].max(eps).ln();
                entry[2] = row[1].max(eps).ln();
                log_densities.push(entry);
            }
            log_densities
        }
    }

    pub struct HiddenMarkovModel {
        transition_model: BarTransitionModel,
        observation_model: RNNDownBeatTrackingObservationModel,
        log_initial: Vec<f32>,
        log_trans: Vec<f32>,
    }

    impl HiddenMarkovModel {
        pub fn new(
            transition_model: BarTransitionModel,
            observation_model: RNNDownBeatTrackingObservationModel,
            initial: Option<Vec<f32>>,
        ) -> Self {
            let num_states = transition_model.state_space.num_states;
            let log_initial = if let Some(initial) = initial {
                initial
                    .into_iter()
                    .map(|v| v.max(f32::EPSILON).ln())
                    .collect()
            } else {
                vec![-(num_states as f32).ln(); num_states]
            };
            let log_trans = transition_model
                .probabilities
                .iter()
                .map(|&p| p.max(f32::EPSILON).ln())
                .collect();
            Self {
                transition_model,
                observation_model,
                log_initial,
                log_trans,
            }
        }

        pub fn viterbi(&self, observations: &Array2<f32>) -> (Vec<usize>, f32) {
            let log_obs = self.observation_model.log_densities(observations);
            let obs_ptr = &self.observation_model.pointers;
            let num_states = self.transition_model.state_space.num_states;
            let num_frames = observations.nrows();

            let mut delta_prev = vec![f32::NEG_INFINITY; num_states];
            let mut delta_curr = vec![f32::NEG_INFINITY; num_states];
            let mut psi = vec![-1isize; num_frames * num_states];

            for state in 0..num_states {
                delta_prev[state] = self.log_initial[state] + log_obs[0][obs_ptr[state]];
            }

            for t in 1..num_frames {
                for state in 0..num_states {
                    let start = self.transition_model.pointers[state];
                    let end = self.transition_model.pointers[state + 1];
                    if start == end {
                        continue;
                    }
                    let mut best_prev = self.transition_model.prev_states[start];
                    let mut best_score = delta_prev[best_prev] + self.log_trans[start];
                    for idx in start + 1..end {
                        let prev = self.transition_model.prev_states[idx];
                        let score = delta_prev[prev] + self.log_trans[idx];
                        if score > best_score {
                            best_score = score;
                            best_prev = prev;
                        }
                    }
                    delta_curr[state] = best_score + log_obs[t][obs_ptr[state]];
                    psi[t * num_states + state] = best_prev as isize;
                }
                std::mem::swap(&mut delta_prev, &mut delta_curr);
                for v in &mut delta_curr {
                    *v = f32::NEG_INFINITY;
                }
            }

            let mut last_state = 0usize;
            let mut best_last = delta_prev[0];
            for (state, score) in delta_prev.iter().enumerate().skip(1) {
                if *score > best_last {
                    best_last = *score;
                    last_state = state;
                }
            }

            let mut path = vec![0usize; num_frames];
            path[num_frames - 1] = last_state;
            for t in (0..num_frames - 1).rev() {
                let prev = psi[(t + 1) * num_states + path[t + 1]];
                path[t] = if prev < 0 { 0 } else { prev as usize };
            }

            (path, best_last)
        }
    }

    pub struct DBNDownBeatTrackingProcessor {
        hmms: Vec<HiddenMarkovModel>,
        threshold: f32,
        fps: usize,
    }

    impl DBNDownBeatTrackingProcessor {
        pub fn new(beats_per_bar: &[usize], threshold: f32, fps: usize) -> Result<Self> {
            let mut hmms = Vec::new();
            for &beats in beats_per_bar {
                let min_interval = 60.0 * fps as f32 / MAX_BPM;
                let max_interval = 60.0 * fps as f32 / MIN_BPM;
                let st = BarStateSpace::new(beats, min_interval, max_interval, Some(NUM_TEMPI));
                let tm = BarTransitionModel::new(st, TRANSITION_LAMBDA)?;
                let om =
                    RNNDownBeatTrackingObservationModel::new(&tm.state_space, OBSERVATION_LAMBDA);
                hmms.push(HiddenMarkovModel::new(tm, om, None));
            }
            Ok(Self {
                hmms,
                threshold,
                fps,
            })
        }

        pub fn process(&self, activations: &Array2<f32>) -> Vec<(f32, f32)> {
            let (activations, first) = if self.threshold > 0.0 {
                threshold_activations(activations, self.threshold)
            } else {
                (activations.to_owned(), 0usize)
            };
            if !activations.iter().any(|&v| v > 0.0) {
                return Vec::new();
            }

            let mut best_idx = 0usize;
            let mut best_log_prob = f32::NEG_INFINITY;
            let mut best_path = Vec::new();
            for (idx, hmm) in self.hmms.iter().enumerate() {
                let (path, log_prob) = hmm.viterbi(&activations);
                if log_prob > best_log_prob {
                    best_log_prob = log_prob;
                    best_idx = idx;
                    best_path = path;
                }
            }

            let hmm = &self.hmms[best_idx];
            let st = &hmm.transition_model.state_space;
            let om = &hmm.observation_model;

            let mut beat_numbers = Vec::with_capacity(best_path.len());
            let mut beat_range = Vec::with_capacity(best_path.len());
            for &state in &best_path {
                let pos = st.state_positions[state];
                beat_numbers.push(pos.floor() as i32 + 1);
                beat_range.push(om.pointers[state] >= 1);
            }

            let mut beats = Vec::new();
            if beat_range.iter().any(|&v| v) {
                let mut idx = Vec::new();
                for i in 1..beat_range.len() {
                    if beat_range[i] != beat_range[i - 1] {
                        idx.push(i);
                    }
                }
                if beat_range[0] {
                    idx.insert(0, 0);
                }
                if *beat_range.last().unwrap_or(&false) {
                    idx.push(beat_range.len());
                }
                for pair in idx.chunks(2) {
                    if pair.len() != 2 {
                        continue;
                    }
                    let left = pair[0];
                    let right = pair[1];
                    if left >= right {
                        continue;
                    }
                    let mut best_idx = left;
                    let mut best_val = f32::NEG_INFINITY;
                    for t in left..right {
                        let v0 = activations[[t, 0]];
                        let v1 = activations[[t, 1]];
                        if v0 > best_val {
                            best_val = v0;
                            best_idx = t;
                        }
                        if v1 > best_val {
                            best_val = v1;
                            best_idx = t;
                        }
                    }
                    beats.push(best_idx);
                }
            }

            let mut result = Vec::new();
            for &b in &beats {
                let time = (b + first) as f32 / self.fps as f32;
                let beat_number = beat_numbers[b] as f32;
                result.push((time, beat_number));
            }
            result
        }
    }

    fn threshold_activations(activations: &Array2<f32>, threshold: f32) -> (Array2<f32>, usize) {
        let mut first = 0usize;
        let mut last = 0usize;
        let mut found = false;
        for (row_idx, row) in activations.outer_iter().enumerate() {
            for &val in row {
                if val >= threshold {
                    if !found {
                        first = row_idx;
                        last = row_idx + 1;
                        found = true;
                    } else {
                        last = last.max(row_idx + 1);
                    }
                }
            }
        }
        if !found {
            return (activations.to_owned(), 0);
        }
        let clipped = activations.slice(s![first..last, ..]).to_owned();
        (clipped, first)
    }
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

#[cfg(test)]
mod tests {
    use super::{
        estimate_bpm_from_beats_samples, has_required_music_model_files, normalize_to_stereo_sr,
        resolve_demucs_model_path, resolve_stem_paths,
    };

    #[test]
    fn stem_resolve_reports_missing() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_music_stem_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp");
        let audio = dir.join("a.wav");
        std::fs::write(&audio, b"x").expect("dummy");

        let resolved = resolve_stem_paths(&audio, None);
        assert_eq!(resolved.missing.len(), 4);
    }

    #[test]
    fn stem_resolve_uses_override_dir() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_music_stem_override_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let audio_dir = dir.join("audio");
        let stems_dir = dir.join("custom_stems");
        std::fs::create_dir_all(&audio_dir).expect("audio temp");
        std::fs::create_dir_all(&stems_dir).expect("stems temp");
        let audio = audio_dir.join("song.wav");
        std::fs::write(&audio, b"x").expect("dummy");
        for name in ["bass.wav", "drums.wav", "other.wav", "vocals.wav"] {
            std::fs::write(stems_dir.join(name), b"wav").expect("stem");
        }
        let resolved = resolve_stem_paths(&audio, Some(stems_dir.as_path()));
        assert!(resolved.is_ready());
        assert_eq!(resolved.bass.parent(), Some(stems_dir.as_path()));
        assert_eq!(resolved.drums.parent(), Some(stems_dir.as_path()));
    }

    #[test]
    fn required_model_files_detect_manifest_or_single_model() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_music_model_req_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("onnx")).expect("onnx dir");
        assert!(!has_required_music_model_files(&dir));
        std::fs::write(dir.join("onnx").join("ensemble_manifest.json"), b"{}").expect("manifest");
        assert!(has_required_music_model_files(&dir));
    }

    #[test]
    fn resolve_demucs_model_prefers_repo_root() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_music_demucs_path_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("onnx")).expect("onnx dir");
        std::fs::write(dir.join("onnx").join("htdemucs.onnx"), b"a").expect("onnx demucs");
        std::fs::write(dir.join("htdemucs.onnx"), b"b").expect("root demucs");
        let path = resolve_demucs_model_path(&dir).expect("demucs path");
        assert_eq!(path, dir.join("htdemucs.onnx"));
    }

    #[test]
    fn normalize_to_stereo_duplicates_mono() {
        let input = vec![vec![0.1f32, -0.2, 0.3]];
        let out = normalize_to_stereo_sr(&input, 44_100, 44_100);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn estimated_bpm_from_beats_uses_median_interval() {
        let sr = 44_100u32;
        let beats = vec![
            0usize,
            22_050,           // 120 BPM
            44_100,           // 120 BPM
            66_150,           // 120 BPM
            66_150 + 110_250, // outlier gap
            66_150 + 132_300, // back to 120 BPM
        ];
        let bpm = estimate_bpm_from_beats_samples(&beats, sr).expect("estimated bpm");
        assert!((bpm - 120.0).abs() < 0.5);
    }

    #[test]
    fn estimated_bpm_folds_to_musical_range() {
        let sr = 44_100u32;
        let slow = vec![0usize, 88_200, 176_400]; // 30 BPM -> folded to 60 BPM
        let fast = vec![0usize, 8_820, 17_640]; // 300 BPM -> folded to 150 BPM
        let slow_bpm = estimate_bpm_from_beats_samples(&slow, sr).expect("slow bpm");
        let fast_bpm = estimate_bpm_from_beats_samples(&fast, sr).expect("fast bpm");
        assert!((slow_bpm - 60.0).abs() < 0.5);
        assert!((fast_bpm - 150.0).abs() < 0.5);
    }
}
