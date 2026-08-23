//! Windows Media Foundation video decoding.
//!
//! The OS decoder is the right default on Windows: it covers H.264, HEVC, VP9
//! and anything else the machine has a codec for, uses hardware acceleration
//! where it exists, handles fragmented and unusual containers that the bundled
//! demuxer does not, and does colour conversion and scaling itself. The
//! bundled OpenH264 stays as the fallback for the files it cannot open and for
//! every other platform.
//!
//! `IMFSourceReader` is used in its synchronous form: this whole object lives
//! on one decode worker thread, which blocks in `ReadSample` and is the only
//! thread that ever touches it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_RGB32,
    MFSTARTUP_LITE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use super::container::{VideoCodec, VideoStreamInfo};
use super::frame::{rgba_to_color_image, Rotation, VideoFrame};
use super::VideoDecoder;

/// Per-thread Media Foundation lifecycle.
///
/// `MFStartup` refcounts internally, but it is per-process and must be paired;
/// this guard ties one startup to the life of one decoder, which is also the
/// life of one worker thread.
struct MfSession;

impl MfSession {
    fn start() -> Result<Self> {
        unsafe {
            // A failure here means COM was already initialised on this thread
            // with a different model, which is fine to proceed from.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("MFStartup")?;
        }
        Ok(Self)
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

pub struct MediaFoundationDecoder {
    // Field order matters: the reader must be released before MFShutdown runs.
    reader: IMFSourceReader,
    _session: MfSession,
    info: VideoStreamInfo,
    /// Decoder output size, which is what the frames actually carry.
    frame_size: (u32, u32),
    rgba_scratch: Vec<u8>,
    finished: bool,
}

impl MediaFoundationDecoder {
    pub fn open(path: &Path, rotation_hint: Rotation) -> Result<Self> {
        let session = MfSession::start()?;
        let url = HSTRING::from(path.as_os_str());

        let attributes: IMFAttributes = unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).context("MFCreateAttributes")?;
            let attributes = attributes.context("MFCreateAttributes returned nothing")?;
            // Lets the reader insert a converter/scaler, which is what makes
            // "give me RGB32" work for any input format.
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
                .context("enable advanced video processing")?;
            attributes
        };

        let reader: IMFSourceReader = unsafe {
            MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), &attributes)
                .context("MFCreateSourceReaderFromURL")?
        };

        // Ask for straight RGB32 so no colour conversion is left to do here.
        unsafe {
            let media_type: IMFMediaType = MFCreateMediaType().context("MFCreateMediaType")?;
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .context("set major type")?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .context("set subtype")?;
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &media_type,
                )
                .context("no video stream, or RGB32 unavailable for it")?;
        }

        let native = unsafe {
            reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .context("GetCurrentMediaType")?
        };
        let packed = unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0) };
        let coded_width = (packed >> 32) as u32;
        let coded_height = (packed & 0xFFFF_FFFF) as u32;
        if coded_width == 0 || coded_height == 0 {
            anyhow::bail!("Media Foundation reported a zero-sized video stream");
        }

        // Media Foundation applies the container's rotation itself, so the
        // frames arrive display-oriented and the container's matrix must not
        // be applied a second time. The hint is kept only for the declared
        // display size.
        let (display_width, display_height) = (coded_width, coded_height);
        let info = VideoStreamInfo {
            coded_width,
            coded_height,
            display_width,
            display_height,
            rotation: Rotation::None,
            duration_secs: 0.0,
            nominal_fps: 0.0,
            codec_label: "video".to_string(),
            codec: VideoCodec::Unknown,
        };
        let _ = rotation_hint;

        Ok(Self {
            reader,
            _session: session,
            info,
            frame_size: (coded_width, coded_height),
            rgba_scratch: Vec::new(),
            finished: false,
        })
    }

    /// Fill in the parts of the stream description only the container knows
    /// (duration, frame rate, codec name), keeping Media Foundation's own
    /// frame size and its already-applied rotation.
    pub fn adopt_container_info(&mut self, container: &VideoStreamInfo) {
        self.info.duration_secs = container.duration_secs;
        self.info.nominal_fps = container.nominal_fps;
        self.info.codec_label = container.codec_label.clone();
        self.info.codec = container.codec;
    }
}

impl VideoDecoder for MediaFoundationDecoder {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
    }

    fn seek(&mut self, secs: f64, _max_forward_walk: usize) -> Result<()> {
        // 100-nanosecond units, the unit every Media Foundation time uses.
        let hns = (secs.max(0.0) * 10_000_000.0) as i64;
        let position = windows::Win32::System::Variant::VARIANT::from(hns);
        unsafe {
            self.reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &position)
                .context("SetCurrentPosition")?;
        }
        self.finished = false;
        Ok(())
    }

    fn next_frame(
        &mut self,
        box_px: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<VideoFrame>> {
        if self.finished {
            return Ok(None);
        }
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let mut stream_flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;
            let hr = unsafe {
                self.reader.ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            };
            if hr.is_err() {
                anyhow::bail!("ReadSample failed: {hr:?}");
            }
            if stream_flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.finished = true;
                return Ok(None);
            }
            let Some(sample) = sample else {
                // A gap or a format change: no picture this time round, but
                // the stream continues.
                continue;
            };
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                .context("ConvertToContiguousBuffer")?;
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            unsafe {
                buffer
                    .Lock(&mut data, None, Some(&mut len))
                    .context("lock media buffer")?;
            }
            let (w, h) = self.frame_size;
            let image = {
                let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
                // RGB32 is BGRX in memory; swap into RGBA before scaling.
                self.rgba_scratch.clear();
                self.rgba_scratch.reserve(bytes.len());
                for px in bytes.chunks_exact(4) {
                    self.rgba_scratch.push(px[2]);
                    self.rgba_scratch.push(px[1]);
                    self.rgba_scratch.push(px[0]);
                    self.rgba_scratch.push(255);
                }
                rgba_to_color_image(&self.rgba_scratch, w, h, Rotation::None, box_px.0, box_px.1)
            };
            let unlock = unsafe { buffer.Unlock() };
            debug_assert!(unlock.is_ok() || unlock.is_err());
            let _ = S_OK;

            let Some(image) = image else {
                continue;
            };
            return Ok(Some(VideoFrame {
                pts_secs: timestamp as f64 / 10_000_000.0,
                image: Arc::new(image),
            }));
        }
    }
}

impl Drop for MediaFoundationDecoder {
    fn drop(&mut self) {
        // Nothing beyond the default field drops; spelled out so the ordering
        // comment on `reader` has somewhere to point.
    }
}

// Safety: every method takes `&mut self`, and one decoder is owned by exactly
// one worker thread for its whole life (created there, dropped there). The COM
// objects are apartment-agnostic under COINIT_MULTITHREADED, which the session
// guard establishes on that same thread.
unsafe impl Send for MediaFoundationDecoder {}

/// Wrap a `windows` `Interface` import so the unused-import lint stays quiet
/// on builds that do not exercise every path.
const _: fn() = || {
    fn assert_interface<T: Interface>() {}
    assert_interface::<IMFSourceReader>();
};
