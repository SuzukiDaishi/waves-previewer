#![cfg(feature = "mp3_lame")]

use std::path::PathBuf;
use std::process::Command;

fn make_temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "neowaves_cli_export_formats_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn direct_wav_to_mp3_export_transcodes_instead_of_copying_riff_bytes() {
    let dir = make_temp_dir();
    let input = dir.join("input.wav");
    let output_without_extension = dir.join("output");
    let expected_output = dir.join("output.mp3");
    let sample_rate = 48_000;
    let tone = (0..sample_rate)
        .map(|i| ((i as f32 / sample_rate as f32) * 440.0 * std::f32::consts::TAU).sin() * 0.25)
        .collect::<Vec<_>>();
    neowaves::wave::export_channels_audio(&[tone], sample_rate, &input).expect("write input wav");

    let result = Command::new(env!("CARGO_BIN_EXE_neowaves"))
        .args([
            "--cli",
            "export",
            "file",
            "--input",
            input.to_str().unwrap(),
            "--output",
            output_without_extension.to_str().unwrap(),
            "--format",
            "mp3",
        ])
        .output()
        .expect("run CLI export");
    assert!(
        result.status.success(),
        "CLI export failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&result.stdout).expect("CLI stdout JSON");
    assert_eq!(
        envelope["result"]["destination"].as_str(),
        Some(expected_output.to_str().unwrap())
    );

    let bytes = std::fs::read(&expected_output).expect("read exported mp3");
    assert!(bytes.len() > 4);
    assert_ne!(
        &bytes[..4],
        b"RIFF",
        "a WAV file was copied to an .mp3 path"
    );
    let info = neowaves::audio_io::read_audio_info(&expected_output).expect("probe exported mp3");
    assert_eq!(info.sample_rate, sample_rate);
    assert_eq!(info.channels, 1);
    let (decoded, decoded_rate) =
        neowaves::audio_io::decode_audio_multi(&expected_output).expect("decode exported mp3");
    assert_eq!(decoded_rate, sample_rate);
    assert_eq!(decoded.len(), 1);
    assert!(!decoded[0].is_empty());

    let _ = std::fs::remove_dir_all(dir);
}
