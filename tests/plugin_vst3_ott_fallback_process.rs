use std::path::Path;
use std::path::PathBuf;

use neowaves::plugin::{WorkerRequest, WorkerResponse};

fn unique_temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("neowaves_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_test_wav(path: &Path) {
    let sr = 48_000u32;
    let len = 48_000usize;
    let mut ch = vec![0.0f32; len];
    for (i, v) in ch.iter_mut().enumerate() {
        let t = i as f32 / sr as f32;
        *v = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.2;
    }
    neowaves::wave::export_channels_audio(&[ch], sr, path).expect("write test wav");
}

#[test]
fn missing_ott_is_not_reported_as_generic_success() {
    let dir = unique_temp_dir("plugin_ott_fallback");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    write_test_wav(&input);

    let req = WorkerRequest::ProcessFx {
        plugin_path: "C:\\nope\\OTT.vst3".to_string(),
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 1024,
        enabled: true,
        bypass: false,
        state_blob_b64: None,
        params: Vec::new(),
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    assert!(matches!(
        resp,
        WorkerResponse::Error { ref message }
            if message.contains("native VST3 process failed")
    ));
    assert!(!output.is_file());

    let _ = std::fs::remove_dir_all(dir);
}
