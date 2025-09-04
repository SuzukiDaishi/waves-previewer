use std::path::PathBuf;

use super::types::FileMeta;

pub fn spawn_meta_worker(paths: Vec<PathBuf>) -> std::sync::mpsc::Receiver<(PathBuf, FileMeta)> {
    use std::sync::mpsc; let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for p in paths {
            // Stage 1: quick header-only metadata
            if let Ok(reader) = hound::WavReader::open(&p) {
                let spec = reader.spec();
                let _ = tx.send((p.clone(), FileMeta{
                    channels: spec.channels,
                    sample_rate: spec.sample_rate,
                    bits_per_sample: spec.bits_per_sample,
                    duration_secs: None,
                    rms_db: None,
                    thumb: Vec::new(),
                }));
            }
            // Stage 2: decode for RMS and thumbnail
            if let Ok((mono, _sr)) = crate::wave::decode_wav_mono(&p) {
                let mut sum_sq = 0.0f64;
                for &v in &mono { sum_sq += (v as f64)*(v as f64); }
                let n = mono.len().max(1) as f64;
                let rms = (sum_sq/n).sqrt() as f32;
                let rms_db = if rms>0.0 { 20.0*rms.log10() } else { -120.0 };
                let mut thumb = Vec::new();
                crate::wave::build_minmax(&mut thumb, &mono, 128);
                // attempt to reuse spec (optional)
                let (ch, sr, bits) = if let Ok(reader2) = hound::WavReader::open(&p) { let s = reader2.spec(); (s.channels, s.sample_rate, s.bits_per_sample) } else { (0,0,0) };
                let length_secs = if sr > 0 { mono.len() as f32 / sr as f32 } else { f32::NAN };
                let _ = tx.send((p, FileMeta{ channels: ch, sample_rate: sr, bits_per_sample: bits, duration_secs: Some(length_secs), rms_db: Some(rms_db), thumb }));
            }
        }
    });
    rx
}

