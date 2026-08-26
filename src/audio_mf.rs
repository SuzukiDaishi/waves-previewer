//! AAC decoding through the decoder Windows already ships.
//!
//! NeoWaves does not carry an AAC decoder of its own: shipping one (FDK,
//! Symphonia's, anything else) puts the AAC patent obligation on whoever
//! distributes the binary, which is why `AAC UNSUPPORTED` exists at all. The
//! decoder inside Windows is licensed by Microsoft as part of the OS, and
//! calling it through the documented Media Foundation API adds no codec to
//! this program — exactly the arrangement the video preview already relies on
//! for H.264/HEVC (see `src/video/decoder_mf.rs`).
//!
//! Only the *first audio track* is read, and only forwards from the start:
//! everything above this module wants whole (or prefix) buffers, and seeking
//! is done on the decoded samples. `IMFSourceReader` is used in its
//! synchronous form, one reader per decode call, on the worker thread that
//! asked for it.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFSourceReader, MFAudioFormat_AAC, MFAudioFormat_Float,
    MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
    MFTEnumEx, MFT_CATEGORY_AUDIO_DECODER, MFT_ENUM_FLAG_ALL, MFT_REGISTER_TYPE_INFO,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_FIRST_AUDIO_STREAM,
};
use windows::Win32::System::Com::CoTaskMemFree;

use crate::mf::MfSession;

/// The one stream index every call here uses.
const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

/// Whether this machine can decode AAC.
///
/// Every desktop Windows edition ships the AAC decoder, but the N/KN editions
/// leave it out until the Media Feature Pack is installed, and this is the
/// difference between "plays" and "`AAC UNSUPPORTED`" for the whole UI. So it
/// is answered by asking Media Foundation which audio decoders are registered
/// for AAC rather than by assuming Windows implies one. Probed once.
pub fn aac_decoder_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| probe_aac_decoder().unwrap_or(false))
}

fn probe_aac_decoder() -> Result<bool> {
    let _session = MfSession::start()?;
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Audio,
        guidSubtype: MFAudioFormat_AAC,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_AUDIO_DECODER,
            MFT_ENUM_FLAG_ALL,
            Some(&input),
            None,
            &mut activates,
            &mut count,
        )
        .context("MFTEnumEx for an AAC audio decoder")?;
    }
    if !activates.is_null() {
        // The enumerator handed out one reference per entry; read each one out
        // so it is released, then free the array itself.
        for i in 0..count as usize {
            drop(unsafe { std::ptr::read(activates.add(i)) });
        }
        unsafe { CoTaskMemFree(Some(activates as *const core::ffi::c_void)) };
    }
    Ok(count > 0)
}

/// What the reader was told to hand back. Both are interleaved.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleLayout {
    Float32,
    Pcm16,
}

impl SampleLayout {
    fn bytes_per_sample(self) -> usize {
        match self {
            SampleLayout::Float32 => 4,
            SampleLayout::Pcm16 => 2,
        }
    }
}

/// A source reader positioned on one file's first audio track.
pub struct MfAudioDecoder {
    // Field order matters: the reader must be released before MFShutdown runs.
    reader: IMFSourceReader,
    _session: MfSession,
    sample_rate: u32,
    channels: usize,
    layout: SampleLayout,
    /// Set once a block has been handed out. After that, a reader error is
    /// treated as the end of a damaged stream rather than losing the audio
    /// already decoded — the same shape as symphonia hitting an unexpected EOF.
    produced_any: bool,
    finished: bool,
}

impl MfAudioDecoder {
    pub fn open(path: &Path) -> Result<Self> {
        let session = MfSession::start()?;
        let url = HSTRING::from(path.as_os_str());
        let reader: IMFSourceReader = unsafe {
            MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), None)
                .context("MFCreateSourceReaderFromURL")?
        };
        // A video file's picture is decoded by the video worker, from its own
        // reader. Leaving the video stream selected here would decode every
        // frame a second time just to reach the sound.
        unsafe {
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .context("deselect streams")?;
            reader
                .SetStreamSelection(AUDIO_STREAM, true)
                .context("no audio stream to select")?;
        }
        // Float is what everything downstream works in; 16-bit PCM is the
        // fallback for a decoder/resampler pair that will not produce it. Each
        // is asked for twice: once naming the bit depth, then leaving it to the
        // reader, because either half of that is what some machines accept.
        let requested = [
            (MFAudioFormat_Float, Some(32)),
            (MFAudioFormat_Float, None),
            (MFAudioFormat_PCM, Some(16)),
            (MFAudioFormat_PCM, None),
        ]
        .into_iter()
        .any(|(subtype, bits)| request_output(&reader, subtype, bits).is_ok());
        if !requested {
            anyhow::bail!("Media Foundation offered neither float nor 16-bit PCM output");
        }
        let (sample_rate, channels, layout) = read_output_layout(&reader)?;
        Ok(Self {
            reader,
            _session: session,
            sample_rate,
            channels,
            layout,
            produced_any: false,
            finished: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The next decoded block as planar samples, or `None` at end of stream.
    ///
    /// Block length is whatever the decoder produced; callers accumulate.
    pub fn next_block(&mut self) -> Result<Option<Vec<Vec<f32>>>> {
        loop {
            if self.finished {
                return Ok(None);
            }
            let mut stream_flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample: Option<IMFSample> = None;
            let read = unsafe {
                self.reader.ReadSample(
                    AUDIO_STREAM,
                    0,
                    None,
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            };
            if let Err(err) = read {
                self.finished = true;
                if self.produced_any {
                    return Ok(None);
                }
                return Err(anyhow::Error::from(err).context("ReadSample"));
            }
            if stream_flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.finished = true;
                return Ok(None);
            }
            if stream_flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                let (sample_rate, channels, layout) = read_output_layout(&self.reader)?;
                // Everything above this module holds one sample rate and one
                // channel count for the whole buffer, so a stream that changes
                // either mid-file has to be reported, not silently spliced.
                if sample_rate != self.sample_rate {
                    anyhow::bail!(
                        "media foundation: sample rate changed mid-stream: expected={} observed={}",
                        self.sample_rate,
                        sample_rate
                    );
                }
                if channels != self.channels {
                    anyhow::bail!(
                        "media foundation: channel count changed mid-stream: expected={} observed={}",
                        self.channels,
                        channels
                    );
                }
                self.layout = layout;
            }
            // No sample with no end-of-stream is a gap or a stream tick.
            let Some(sample) = sample else {
                continue;
            };
            let block = self.sample_to_planar(&sample)?;
            if block.first().map(|c| c.is_empty()).unwrap_or(true) {
                continue;
            }
            self.produced_any = true;
            return Ok(Some(block));
        }
    }

    fn sample_to_planar(&self, sample: &IMFSample) -> Result<Vec<Vec<f32>>> {
        let buffer =
            unsafe { sample.ConvertToContiguousBuffer() }.context("ConvertToContiguousBuffer")?;
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        unsafe {
            buffer
                .Lock(&mut data, None, Some(&mut len))
                .context("lock audio buffer")?;
        }
        let block = {
            let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
            self.bytes_to_planar(bytes)
        };
        let _ = unsafe { buffer.Unlock() };
        Ok(block)
    }

    fn bytes_to_planar(&self, bytes: &[u8]) -> Vec<Vec<f32>> {
        let channels = self.channels.max(1);
        let stride = self.layout.bytes_per_sample();
        let frames = bytes.len() / (stride * channels);
        let mut planar: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
        for frame in 0..frames {
            for (channel, out) in planar.iter_mut().enumerate() {
                let at = (frame * channels + channel) * stride;
                let value = match self.layout {
                    SampleLayout::Float32 => {
                        f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                    }
                    SampleLayout::Pcm16 => {
                        i16::from_le_bytes([bytes[at], bytes[at + 1]]) as f32 / 32768.0
                    }
                };
                out.push(value);
            }
        }
        planar
    }
}

fn request_output(reader: &IMFSourceReader, subtype: GUID, bits: Option<u32>) -> Result<()> {
    unsafe {
        let media_type: IMFMediaType = MFCreateMediaType().context("MFCreateMediaType")?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .context("set major type")?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &subtype)
            .context("set subtype")?;
        if let Some(bits) = bits {
            media_type
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, bits)
                .context("set bit depth")?;
        }
        // A partial type: the reader fills in the source's own sample rate and
        // channel count, so nothing here resamples or downmixes.
        reader
            .SetCurrentMediaType(AUDIO_STREAM, None, &media_type)
            .context("set audio output type")?;
    }
    Ok(())
}

fn read_output_layout(reader: &IMFSourceReader) -> Result<(u32, usize, SampleLayout)> {
    let media_type = unsafe { reader.GetCurrentMediaType(AUDIO_STREAM) }
        .context("GetCurrentMediaType for the audio stream")?;
    let subtype = unsafe { media_type.GetGUID(&MF_MT_SUBTYPE) }.context("audio output subtype")?;
    let bits = unsafe { media_type.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE) }.unwrap_or(0);
    let layout = if subtype == MFAudioFormat_Float && (bits == 32 || bits == 0) {
        SampleLayout::Float32
    } else if subtype == MFAudioFormat_PCM && bits == 16 {
        SampleLayout::Pcm16
    } else {
        anyhow::bail!("unexpected Media Foundation audio output: {subtype:?} at {bits} bits");
    };
    let sample_rate = unsafe { media_type.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }
        .context("audio output sample rate")?;
    let channels = unsafe { media_type.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }
        .context("audio output channels")?;
    if sample_rate == 0 || channels == 0 {
        anyhow::bail!("media foundation reported a {channels}-channel stream at {sample_rate} Hz");
    }
    Ok((sample_rate, channels as usize, layout))
}

/// Decode a whole file, or its first `max_secs`.
///
/// The third value is whether the end of the stream was reached, matching the
/// prefix decoders in [`crate::audio_io`].
pub fn decode(path: &Path, max_secs: Option<f32>) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    let mut decoder = MfAudioDecoder::open(path)?;
    let sample_rate = decoder.sample_rate();
    let max_frames = max_secs
        .filter(|secs| *secs > 0.0)
        .map(|secs| ((sample_rate as f32) * secs).ceil() as usize)
        .filter(|frames| *frames > 0);
    let mut channels: Vec<Vec<f32>> = (0..decoder.channels().max(1)).map(|_| Vec::new()).collect();
    let mut reached_eof = true;
    while let Some(block) = decoder.next_block()? {
        for (out, decoded) in channels.iter_mut().zip(block) {
            out.extend_from_slice(&decoded);
        }
        if let Some(limit) = max_frames {
            if channels.first().map(|c| c.len()).unwrap_or(0) >= limit {
                reached_eof = false;
                for out in &mut channels {
                    out.truncate(limit);
                }
                break;
            }
        }
    }
    Ok((channels, sample_rate, reached_eof))
}

/// Decode a whole file, handing back roughly `emit_every_secs` at a time.
///
/// Chunks go out through the same emitter the Symphonia path uses, so a
/// progressively loaded AAC track behaves like every other progressive decode.
pub fn decode_progressive_chunks<C, F>(
    path: &Path,
    emit_every_secs: f32,
    mut should_cancel: C,
    mut on_chunk: F,
) -> Result<()>
where
    C: FnMut() -> bool,
    F: FnMut(Vec<Vec<f32>>, u32, usize, bool) -> bool,
{
    let mut decoder = MfAudioDecoder::open(path)?;
    let sample_rate = decoder.sample_rate();
    let emit_frames = (((sample_rate as f32) * emit_every_secs.max(0.05)).ceil() as usize).max(1);
    let mut pending: Vec<Vec<f32>> = (0..decoder.channels().max(1)).map(|_| Vec::new()).collect();
    let mut decoded_frames = 0usize;
    loop {
        if should_cancel() {
            return Ok(());
        }
        let Some(block) = decoder.next_block()? else {
            break;
        };
        let frames = block.first().map(|c| c.len()).unwrap_or(0);
        for (out, decoded) in pending.iter_mut().zip(block) {
            out.extend_from_slice(&decoded);
        }
        decoded_frames = decoded_frames.saturating_add(frames);
        if pending.first().map(|c| c.len()).unwrap_or(0) >= emit_frames
            && !crate::audio_io::emit_ready_chunk(
                path,
                "decode_multi_progressive_aac_chunk",
                sample_rate,
                &mut pending,
                decoded_frames,
                false,
                &mut on_chunk,
            )
        {
            return Ok(());
        }
    }
    if should_cancel() {
        return Ok(());
    }
    let _ = crate::audio_io::emit_ready_chunk(
        path,
        "decode_multi_progressive_aac_final",
        sample_rate,
        &mut pending,
        decoded_frames,
        true,
        &mut on_chunk,
    );
    Ok(())
}
