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
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_RGB32, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::mf::MfSession;

use super::container::{VideoCodec, VideoStreamInfo};
use super::frame::{bgra_stride_to_color_image, Rotation, VideoFrame};
use super::VideoDecoder;

pub struct MediaFoundationDecoder {
    // Field order matters: the reader must be released before MFShutdown runs.
    reader: IMFSourceReader,
    _session: MfSession,
    info: VideoStreamInfo,
    /// Decoder output size, which is what the frames actually carry.
    frame_size: (u32, u32),
    /// A seek starts decoding at the preceding keyframe. Keep walking until
    /// this exact presentation time is covered before returning a picture.
    pending_seek_secs: Option<f64>,
    /// First frame after a seek target, retained for the next sequential call.
    queued_frame: Option<((u32, u32), VideoFrame)>,
    /// Last picture returned to the worker. Nearby forward requests can keep
    /// decoding from here instead of restarting at the preceding keyframe.
    last_returned_secs: Option<f64>,
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
            pending_seek_secs: None,
            queued_frame: None,
            last_returned_secs: None,
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

    fn read_next_frame(
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
                let row_bytes = w as usize * 4;
                let stride = if h > 0 {
                    (bytes.len() / h as usize).max(row_bytes)
                } else {
                    row_bytes
                };
                bgra_stride_to_color_image(bytes, w, h, stride, box_px.0, box_px.1)
            };
            let _ = unsafe { buffer.Unlock() };
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

impl VideoDecoder for MediaFoundationDecoder {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
    }

    fn seek(&mut self, secs: f64, max_forward_walk: usize) -> Result<()> {
        let secs = secs.max(0.0);
        let frame_secs = if self.info.nominal_fps > 1.0 {
            1.0 / self.info.nominal_fps as f64
        } else {
            1.0 / 30.0
        };
        let can_walk_forward = self.last_returned_secs.is_some_and(|last| {
            secs > last + frame_secs * 0.25
                && secs - last <= frame_secs * max_forward_walk.max(1) as f64
        });
        if can_walk_forward {
            self.pending_seek_secs = Some(secs);
            self.finished = false;
            return Ok(());
        }
        // 100-nanosecond units, the unit every Media Foundation time uses.
        // An all-zero time format GUID means "the default", which for a media
        // source is exactly those units.
        let hns = (secs * 10_000_000.0) as i64;
        let position = PROPVARIANT::from(hns);
        unsafe {
            self.reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &position)
                .context("SetCurrentPosition")?;
        }
        self.pending_seek_secs = Some(secs);
        self.queued_frame = None;
        self.last_returned_secs = None;
        self.finished = false;
        Ok(())
    }

    fn next_frame(
        &mut self,
        box_px: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<VideoFrame>> {
        let take_next = |this: &mut Self| -> Result<Option<VideoFrame>> {
            if let Some((queued_box, frame)) = this.queued_frame.take() {
                if queued_box == box_px {
                    return Ok(Some(frame));
                }
            }
            this.read_next_frame(box_px, cancel)
        };
        let Some(target) = self.pending_seek_secs.take() else {
            let frame = take_next(self)?;
            if let Some(frame) = &frame {
                self.last_returned_secs = Some(frame.pts_secs);
            }
            return Ok(frame);
        };
        let mut at_or_before: Option<VideoFrame> = None;
        loop {
            let Some(frame) = take_next(self)? else {
                if let Some(frame) = &at_or_before {
                    self.last_returned_secs = Some(frame.pts_secs);
                }
                return Ok(at_or_before);
            };
            if frame.pts_secs <= target + 1.0e-7 {
                at_or_before = Some(frame);
                continue;
            }
            if let Some(frame_at_target) = at_or_before {
                self.queued_frame = Some((box_px, frame));
                self.last_returned_secs = Some(frame_at_target.pts_secs);
                return Ok(Some(frame_at_target));
            }
            // The requested time precedes the first timestamp in the file.
            self.last_returned_secs = Some(frame.pts_secs);
            return Ok(Some(frame));
        }
    }
}

// Safety: every method takes `&mut self`, and one decoder is owned by exactly
// one worker thread for its whole life (created there, dropped there). The COM
// objects are apartment-agnostic under COINIT_MULTITHREADED, which the session
// guard establishes on that same thread.
unsafe impl Send for MediaFoundationDecoder {}
