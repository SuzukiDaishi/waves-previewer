use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RecordingDeviceInfo {
    pub id: String,
    pub display_name: String,
    pub channels: u16,
    pub default_sample_rate: u32,
}

pub struct CaptureStream {
    pub _stream: cpal::Stream,
    pub channels: u16,
    pub sample_rate: u32,
}

fn device_name(device: &cpal::Device) -> Option<String> {
    let description = device.description().ok()?;
    let trimmed = description.name().trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn device_info(device: &cpal::Device) -> Option<RecordingDeviceInfo> {
    let name = device_name(device)?;
    let (channels, sr) = if let Ok(cfg) = device.default_input_config() {
        (cfg.channels(), cfg.sample_rate())
    } else {
        (1, 44100)
    };
    Some(RecordingDeviceInfo {
        id: name.clone(),
        display_name: name,
        channels,
        default_sample_rate: sr,
    })
}

pub fn list_input_devices() -> Vec<RecordingDeviceInfo> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for device in devices {
        if let Some(info) = device_info(&device) {
            out.push(info);
        }
    }
    out
}

pub(crate) fn default_input_device_info() -> Option<RecordingDeviceInfo> {
    let host = cpal::default_host();
    let device = host.default_input_device()?;
    device_info(&device)
}

pub fn start_microphone_capture(
    device_id: Option<&str>,
    tx: std::sync::mpsc::SyncSender<Vec<f32>>,
    err_tx: std::sync::mpsc::Sender<String>,
    overruns: Arc<AtomicUsize>,
) -> Result<CaptureStream> {
    let host = cpal::default_host();

    let device = if let Some(id) = device_id.filter(|s| !s.is_empty()) {
        let devices = host.input_devices().context("enumerate input devices")?;
        let mut found = None;
        for d in devices {
            if device_name(&d).as_deref() == Some(id) {
                found = Some(d);
                break;
            }
        }
        found.with_context(|| format!("input device not found: {id}"))?
    } else {
        host.default_input_device()
            .context("No default input device")?
    };

    let cfg = device
        .default_input_config()
        .context("get default input config")?;
    let channels = cfg.channels();
    let sample_rate = cfg.sample_rate();
    let fmt = cfg.sample_format();
    let stream_cfg: cpal::StreamConfig = cfg.into();

    let stream = build_input_stream(&device, &stream_cfg, fmt, tx, err_tx, overruns)
        .context("build input stream")?;
    stream.play().context("start capture stream")?;

    Ok(CaptureStream {
        _stream: stream,
        channels,
        sample_rate,
    })
}

fn build_input_stream(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    fmt: cpal::SampleFormat,
    tx: std::sync::mpsc::SyncSender<Vec<f32>>,
    err_tx: std::sync::mpsc::Sender<String>,
    overruns: Arc<AtomicUsize>,
) -> Result<cpal::Stream> {
    // Surface stream errors (device unplugged etc.) to the recording worker
    // instead of just stderr, so the UI can show them.
    let err_fn = move |err: cpal::StreamError| {
        let _ = err_tx.send(format!("capture stream error: {err}"));
    };
    // A full channel means the worker stalled; count the dropped buffer so the
    // UI can warn about lost audio instead of failing silently.
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let t = tx.clone();
            device.build_input_stream(
                cfg,
                move |data: &[f32], _| {
                    if t.try_send(data.to_vec()).is_err() {
                        overruns.fetch_add(1, Ordering::Relaxed);
                    }
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let t = tx.clone();
            device.build_input_stream(
                cfg,
                move |data: &[i16], _| {
                    let floats: Vec<f32> =
                        data.iter().map(|&s| s as f32 / 32768.0).collect();
                    if t.try_send(floats).is_err() {
                        overruns.fetch_add(1, Ordering::Relaxed);
                    }
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let t = tx.clone();
            device.build_input_stream(
                cfg,
                move |data: &[u16], _| {
                    let floats: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    if t.try_send(floats).is_err() {
                        overruns.fetch_add(1, Ordering::Relaxed);
                    }
                },
                err_fn,
                None,
            )
        }
        _ => {
            anyhow::bail!("Unsupported input sample format: {:?}", fmt);
        }
    }
    .context("build_input_stream")?;
    Ok(stream)
}

/// Windows WASAPI loopback capture (system audio)
#[cfg(target_os = "windows")]
pub fn start_wasapi_loopback_capture(
    tx: std::sync::mpsc::SyncSender<Vec<f32>>,
    err_tx: std::sync::mpsc::Sender<String>,
    overruns: Arc<AtomicUsize>,
) -> Result<CaptureStream> {
    let host = cpal::host_from_id(cpal::HostId::Wasapi).context("WASAPI host not available")?;
    let device = host
        .default_output_device()
        .context("No default output device for loopback")?;
    let cfg = device
        .default_output_config()
        .context("get default output config for loopback")?;
    let channels = cfg.channels();
    let sample_rate = cfg.sample_rate();
    let fmt = cfg.sample_format();
    let stream_cfg: cpal::StreamConfig = cfg.into();
    let stream = build_input_stream(&device, &stream_cfg, fmt, tx, err_tx, overruns)
        .context("build WASAPI loopback stream")?;
    stream.play().context("start WASAPI loopback stream")?;
    Ok(CaptureStream {
        _stream: stream,
        channels,
        sample_rate,
    })
}
