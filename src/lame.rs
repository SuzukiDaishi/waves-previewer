//! Minimal MIT-licensed bridge to the replaceable libmp3lame shared library.
//!
//! Keeping this bridge in NeoWaves avoids statically incorporating an LGPL
//! Rust wrapper. The C implementation is built as `libmp3lame.dll`/a shared
//! object by `build.rs` and can be replaced independently by the user.

#![cfg(feature = "mp3_lame")]

use anyhow::{bail, Context, Result};
use std::ffi::{c_float, c_int, c_uchar, c_void};
use std::ptr::NonNull;

#[link(name = "mp3lame", kind = "dylib")]
extern "C" {
    fn lame_init() -> *mut c_void;
    fn lame_close(gfp: *mut c_void) -> c_int;
    fn lame_set_num_channels(gfp: *mut c_void, channels: c_int) -> c_int;
    fn lame_set_in_samplerate(gfp: *mut c_void, sample_rate: c_int) -> c_int;
    fn lame_set_brate(gfp: *mut c_void, bitrate_kbps: c_int) -> c_int;
    fn lame_set_quality(gfp: *mut c_void, quality: c_int) -> c_int;
    fn lame_init_params(gfp: *mut c_void) -> c_int;
    fn lame_encode_buffer_ieee_float(
        gfp: *mut c_void,
        left: *const c_float,
        right: *const c_float,
        samples: c_int,
        output: *mut c_uchar,
        output_len: c_int,
    ) -> c_int;
    fn lame_encode_flush_nogap(gfp: *mut c_void, output: *mut c_uchar, output_len: c_int) -> c_int;
}

struct Encoder(NonNull<c_void>);

impl Encoder {
    fn new() -> Result<Self> {
        let ptr = unsafe { lame_init() };
        NonNull::new(ptr)
            .map(Self)
            .context("LAME returned a null encoder")
    }

    fn ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = lame_close(self.ptr());
        }
    }
}

fn check_config(code: c_int, operation: &str) -> Result<()> {
    if code < 0 {
        bail!("LAME {operation} failed with error {code}");
    }
    Ok(())
}

fn append_encoded(
    out: &mut Vec<u8>,
    scratch: &[u8],
    encoded: c_int,
    operation: &str,
) -> Result<()> {
    if encoded < 0 {
        let reason = match encoded {
            -1 => "output buffer too small",
            -2 => "allocation failure",
            -3 => "encoder parameters are not initialized",
            -4 => "psychoacoustic model failure",
            _ => "unknown encoder error",
        };
        bail!("LAME {operation} failed: {reason} ({encoded})");
    }
    out.extend_from_slice(&scratch[..encoded as usize]);
    Ok(())
}

/// Encodes one or two planar float channels using the same CBR/quality settings
/// NeoWaves used through `mp3lame-encoder` before the dynamic-link migration.
pub fn encode_planar_f32(
    channels: &[Vec<f32>],
    sample_rate: u32,
    bitrate_kbps: u32,
) -> Result<Vec<u8>> {
    if !(1..=2).contains(&channels.len()) {
        bail!("LAME expects one or two channels, got {}", channels.len());
    }
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    if frames > c_int::MAX as usize {
        bail!("audio is too long for one LAME encode call");
    }
    let sample_rate = c_int::try_from(sample_rate).context("MP3 sample rate is too large")?;
    let bitrate = c_int::try_from(bitrate_kbps).context("MP3 bitrate is too large")?;

    let encoder = Encoder::new().context("initialize dynamic LAME encoder")?;
    unsafe {
        check_config(
            lame_set_num_channels(encoder.ptr(), channels.len() as c_int),
            "channel setup",
        )?;
        check_config(
            lame_set_in_samplerate(encoder.ptr(), sample_rate),
            "sample-rate setup",
        )?;
        check_config(lame_set_brate(encoder.ptr(), bitrate), "bitrate setup")?;
        // LAME quality 0 is the wrapper's former `Quality::Best` value.
        check_config(lame_set_quality(encoder.ptr(), 0), "quality setup")?;
        check_config(lame_init_params(encoder.ptr()), "parameter initialization")?;
    }

    // LAME documents 1.25 * samples + 7200 as the required upper bound.
    let buffer_len = frames
        .saturating_add((frames + 3) / 4)
        .saturating_add(7200)
        .max(7200);
    let output_len = c_int::try_from(buffer_len).context("MP3 output buffer is too large")?;
    let mut scratch = vec![0u8; buffer_len];
    let right = if channels.len() == 1 {
        std::ptr::null()
    } else {
        channels[1].as_ptr()
    };
    let encoded = unsafe {
        lame_encode_buffer_ieee_float(
            encoder.ptr(),
            channels[0].as_ptr(),
            right,
            frames as c_int,
            scratch.as_mut_ptr(),
            output_len,
        )
    };
    let mut out = Vec::with_capacity(buffer_len);
    append_encoded(&mut out, &scratch, encoded, "encode")?;

    let flushed =
        unsafe { lame_encode_flush_nogap(encoder.ptr(), scratch.as_mut_ptr(), output_len) };
    append_encoded(&mut out, &scratch, flushed, "flush")?;
    Ok(out)
}
