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
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::mf::MfSession;

use super::container::{VideoCodec, VideoStreamInfo};
use super::frame::{bgra_stride_to_color_image, Rotation, VideoFrame};
use super::VideoDecoder;

/// How far before a seek target a transport stream is asked to land. Media
/// Foundation's MPEG-2 source seeks by estimate and can come down past the
/// target; half a second is an AVCHD GOP, and the forward walk decodes the
/// rest of the way.
const TS_SEEK_PREROLL_SECS: f64 = 0.5;
/// The first step back when a transport stream seek still came down too late.
/// Each further retry doubles it, so even a file with one keyframe is found
/// in a handful of tries rather than a walk back a second at a time.
const TS_SEEK_RETRY_SECS: f64 = 2.0;

/// The width a picture is shown at: its stored width stretched by the pixel
/// aspect ratio, which is what turns AVCHD's 1440x1080 into 16:9.
fn display_width_for(coded_width: u32, pixel_aspect: (u32, u32)) -> u32 {
    let (num, den) = pixel_aspect;
    if num == 0 || den == 0 {
        return coded_width;
    }
    let width = (u64::from(coded_width) * u64::from(num) + u64::from(den) / 2) / u64::from(den);
    u32::try_from(width).unwrap_or(coded_width).max(1)
}

/// The pixel aspect ratio to apply to frames of `output` size, or 1:1 when
/// Media Foundation has already applied it.
///
/// Which it does depends on the source: for AVCHD's 1440x1080 the MPEG-2
/// source hands over 1920x1080, for an anamorphic mp4 the stored size. So
/// the ratio is applied only when the output is still the stored size; a
/// rotated output (also not the stored size) is left alone the same way.
fn pending_pixel_aspect(reader: &IMFSourceReader, output: (u32, u32)) -> (u32, u32) {
    let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    let Ok(native) = (unsafe { reader.GetNativeMediaType(stream, 0) }) else {
        return (1, 1);
    };
    let unpack = |packed: u64| ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32);
    let stored = unsafe { native.GetUINT64(&MF_MT_FRAME_SIZE) }.map(unpack);
    let ratio = unsafe { native.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO) }.map(unpack);
    match (stored, ratio) {
        (Ok(stored), Ok(ratio)) if stored == output && ratio.0 > 0 && ratio.1 > 0 => ratio,
        _ => (1, 1),
    }
}

/// A source reader on `url` that hands over RGB32 frames at the stream's own
/// size.
fn create_reader(url: &HSTRING) -> Result<IMFSourceReader> {
    let attributes: IMFAttributes = unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2).context("MFCreateAttributes")?;
        let attributes = attributes.context("MFCreateAttributes returned nothing")?;
        // Lets the reader insert a converter/scaler, which is what makes
        // "give me RGB32" work for any input format.
        attributes
            .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
            .context("enable advanced video processing")?;
        // This is opportunistic: systems without a matching hardware MFT
        // continue through Media Foundation's software transforms.
        attributes
            .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
            .context("enable hardware transforms")?;
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
    Ok(reader)
}

pub struct MediaFoundationDecoder {
    // Field order matters: the reader must be released before MFShutdown runs.
    reader: IMFSourceReader,
    _session: MfSession,
    info: VideoStreamInfo,
    /// Native RGB32 size selected when the OS cannot resize this stream.
    native_frame_size: (u32, u32),
    /// Decoder output size, which is what the frames actually carry.
    frame_size: (u32, u32),
    /// Bounding box most recently prepared by the worker.
    prepared_box_px: Option<(u32, u32)>,
    /// False after an output-size media type was rejected. The decoder stays
    /// usable and falls back to the existing Rust scaler.
    native_resize_available: bool,
    /// A seek starts decoding at the preceding keyframe. Keep walking until
    /// this exact presentation time is covered before returning a picture.
    pending_seek_secs: Option<f64>,
    /// First frame after a seek target, retained for the next sequential call.
    queued_frame: Option<((u32, u32), VideoFrame)>,
    /// Last picture returned to the worker. Nearby forward requests can keep
    /// decoding from here instead of restarting at the preceding keyframe.
    last_returned_secs: Option<f64>,
    finished: bool,
    /// Added to Media Foundation's sample times (100 ns units) to put them on
    /// the row's timeline. Zero for ISO-BMFF; a transport stream's source
    /// picks a zero of its own, which [`Self::align_to_timeline`] measures.
    /// Kept in the source's own integer unit so a frame lands on the same
    /// exact time it would without an offset: added in seconds, a frame at
    /// 0.3 s came out at 0.30000000000000004 and missed a playhead at 0.3.
    timeline_offset_hns: i64,
    /// How far before a seek target to ask the source to land.
    seek_preroll_secs: f64,
    /// The file, for opening a fresh reader.
    url: HSTRING,
    /// A transport stream: its seeks are estimates that can come down past
    /// the target, or past the last keyframe, so they are checked and
    /// retried further back.
    transport_stream: bool,
    /// Where (in the source's own seconds) the last transport stream seek
    /// aimed, until a picture at or before its target shows it landed early
    /// enough.
    seek_aim_secs: Option<f64>,
    /// Retries spent on the current seek.
    seek_retries: u32,
    /// Nothing has been read since the reader was created.
    reader_at_start: bool,
}

impl MediaFoundationDecoder {
    pub fn open(path: &Path, rotation_hint: Rotation) -> Result<Self> {
        let session = MfSession::start()?;
        let url = HSTRING::from(path.as_os_str());

        let reader = create_reader(&url)?;

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
        // display size. Where the source leaves the pixel aspect ratio to us,
        // frames come out at the stored size and the panel stretches them to
        // this one.
        let display_width = display_width_for(
            coded_width,
            pending_pixel_aspect(&reader, (coded_width, coded_height)),
        );
        let display_height = coded_height;
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
            native_frame_size: (coded_width, coded_height),
            frame_size: (coded_width, coded_height),
            prepared_box_px: None,
            native_resize_available: true,
            pending_seek_secs: None,
            queued_frame: None,
            last_returned_secs: None,
            finished: false,
            timeline_offset_hns: 0,
            seek_preroll_secs: 0.0,
            url,
            transport_stream: false,
            seek_aim_secs: None,
            seek_retries: 0,
            reader_at_start: true,
        })
    }

    /// A fresh reader on the same file, at its start, keeping the output
    /// size already negotiated. This is how a transport stream goes back to
    /// the start: seeking Media Foundation's MPEG-2 source to zero can come
    /// down just past the first keyframe and, in a file with no other, read
    /// on to the end without a picture.
    fn reopen(&mut self) -> Result<()> {
        self.reader = create_reader(&self.url)?;
        if self.frame_size != self.native_frame_size {
            let media_type = Self::rgb32_type(Some(self.frame_size))?;
            let resized = unsafe {
                self.reader.SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &media_type,
                )
            };
            if resized.is_err() {
                self.frame_size = self.native_frame_size;
                self.native_resize_available = false;
            }
        }
        self.reader_at_start = true;
        self.finished = false;
        Ok(())
    }

    /// Point the reader at `source_secs` of the source's own time.
    fn position_source(&mut self, source_secs: f64) -> Result<()> {
        if self.transport_stream {
            if source_secs <= 0.0 {
                // From the start, whatever comes first is right.
                self.seek_aim_secs = None;
                if !self.reader_at_start {
                    self.reopen()?;
                }
                return Ok(());
            }
            self.seek_aim_secs = Some(source_secs);
        }
        // 100-nanosecond units, the unit every Media Foundation time uses.
        // An all-zero time format GUID means "the default", which for a media
        // source is exactly those units.
        let position = PROPVARIANT::from((source_secs * 10_000_000.0) as i64);
        let moved = unsafe {
            self.reader
                .SetCurrentPosition(&windows::core::GUID::zeroed(), &position)
        };
        match moved {
            Ok(()) => {}
            Err(err) if !self.transport_stream => {
                return Err(anyhow::Error::from(err).context("SetCurrentPosition"));
            }
            // The MPEG-2 source refuses some positions outright (near the
            // end of a short file, 0xC00D36E5); the start of a fresh reader it
            // never refuses, and the walk forward reaches the target from
            // there.
            Err(_) => {
                self.seek_aim_secs = None;
                return self.reopen();
            }
        }
        self.reader_at_start = false;
        Ok(())
    }

    /// A transport stream seek that came down past its target, or past the
    /// last keyframe: aim further back, twice as far each time, and from the
    /// start of the file once that is nearer.
    fn retry_seek_earlier(&mut self, aim_secs: f64) -> Result<()> {
        self.seek_retries = self.seek_retries.saturating_add(1);
        let back = TS_SEEK_RETRY_SECS * f64::from(1u32 << (self.seek_retries - 1).min(16));
        self.queued_frame = None;
        self.finished = false;
        self.position_source((aim_secs - back).max(0.0))
    }

    /// Put a transport stream's pictures on the row's timeline.
    ///
    /// `picture_start_secs` is where the first picture belongs on that
    /// timeline, from the stream's own timestamps (see `audio_mpegts`).
    /// Media Foundation's MPEG-2 source renumbers time from a zero it does
    /// not report, so the first picture's time is read once and the
    /// difference kept; its seeks are estimates, so they are aimed early.
    pub fn align_to_timeline(&mut self, picture_start_secs: f64) -> Result<()> {
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let first_hns = loop {
            let mut stream_flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;
            unsafe {
                self.reader
                    .ReadSample(
                        stream,
                        0,
                        None,
                        Some(&mut stream_flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .context("ReadSample for the first picture")?;
            }
            if stream_flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                anyhow::bail!("the video stream ended before its first picture");
            }
            if sample.is_some() {
                break timestamp;
            }
        };
        self.reopen().context("rewind after the first picture")?;
        self.timeline_offset_hns = (picture_start_secs * 10_000_000.0).round() as i64 - first_hns;
        self.seek_preroll_secs = TS_SEEK_PREROLL_SECS;
        self.transport_stream = true;
        self.pending_seek_secs = None;
        self.queued_frame = None;
        self.last_returned_secs = None;
        self.finished = false;
        Ok(())
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

    fn rgb32_type(frame_size: Option<(u32, u32)>) -> Result<IMFMediaType> {
        let media_type: IMFMediaType = unsafe { MFCreateMediaType().context("MFCreateMediaType")? };
        unsafe {
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .context("set major type")?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .context("set subtype")?;
            if let Some((width, height)) = frame_size {
                let packed = (u64::from(width) << 32) | u64::from(height);
                media_type
                    .SetUINT64(&MF_MT_FRAME_SIZE, packed)
                    .context("set output frame size")?;
            }
        }
        Ok(media_type)
    }

    fn select_rgb32_output(&mut self, frame_size: Option<(u32, u32)>) -> Result<(u32, u32)> {
        let media_type = Self::rgb32_type(frame_size)?;
        unsafe {
            self.reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &media_type,
                )
                .context("select RGB32 video output")?;
        }
        let selected = unsafe {
            self.reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .context("GetCurrentMediaType after resize")?
        };
        let packed = unsafe { selected.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0) };
        let actual = ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32);
        if actual.0 == 0 || actual.1 == 0 {
            anyhow::bail!("Media Foundation selected a zero-sized RGB32 output");
        }
        self.frame_size = actual;
        self.pending_seek_secs = None;
        self.queued_frame = None;
        self.last_returned_secs = None;
        self.finished = false;
        Ok(actual)
    }

    fn read_next_frame(
        &mut self,
        box_px: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<VideoFrame>> {
        if self.finished {
            return Ok(None);
        }
        self.reader_at_start = false;
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
                pts_secs: (timestamp + self.timeline_offset_hns) as f64 / 10_000_000.0,
                image: Arc::new(image),
            }));
        }
    }
}

impl VideoDecoder for MediaFoundationDecoder {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
    }

    fn prepare_output(&mut self, box_px: (u32, u32)) -> Result<(u32, u32)> {
        let box_px = (box_px.0.max(1), box_px.1.max(1));
        if self.prepared_box_px == Some(box_px) {
            return Ok(self.frame_size);
        }
        self.prepared_box_px = Some(box_px);

        if !self.native_resize_available {
            // The Rust scaler can use a new bounding box without flushing and
            // re-selecting the same native RGB32 media type on every resize.
            return Ok(self.frame_size);
        }
        let target = super::frame::fit_within(
            self.native_frame_size.0,
            self.native_frame_size.1,
            box_px.0,
            box_px.1,
        );
        match self.select_rgb32_output(Some(target)) {
            Ok(actual) => return Ok(actual),
            Err(err) => {
                eprintln!(
                    "video: Media Foundation output resize to {}x{} was rejected: {err:#}",
                    target.0, target.1
                );
                self.native_resize_available = false;
            }
        }

        // A codec or third-party transform can reject sized output types.
        // Re-select plain RGB32 and retain the existing CPU scaling fallback.
        // Even if that defensive re-selection is also rejected, the reader's
        // last valid RGB32 type remains usable; output-size negotiation must
        // never turn an otherwise playable stream into a hard failure.
        match self.select_rgb32_output(None) {
            Ok(actual) => Ok(actual),
            Err(fallback_err) => {
                eprintln!(
                    "video: Media Foundation could not re-select native RGB32; keeping {}x{}: {fallback_err:#}",
                    self.frame_size.0, self.frame_size.1
                );
                Ok(self.frame_size)
            }
        }
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
        let offset_secs = self.timeline_offset_hns as f64 / 10_000_000.0;
        let source_secs = (secs - offset_secs - self.seek_preroll_secs).max(0.0);
        self.seek_retries = 0;
        self.position_source(source_secs)?;
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
                if at_or_before.is_none() && !cancel.load(Ordering::Relaxed) {
                    if let Some(aim) = self.seek_aim_secs {
                        // The stream ended without a picture: the seek came
                        // down past the last keyframe.
                        self.retry_seek_earlier(aim)?;
                        continue;
                    }
                }
                if let Some(frame) = &at_or_before {
                    self.last_returned_secs = Some(frame.pts_secs);
                }
                return Ok(at_or_before);
            };
            if frame.pts_secs <= target + 1.0e-7 {
                // Landed early enough; the walk forward does the rest.
                self.seek_aim_secs = None;
                at_or_before = Some(frame);
                continue;
            }
            if let Some(frame_at_target) = at_or_before {
                self.queued_frame = Some((box_px, frame));
                self.last_returned_secs = Some(frame_at_target.pts_secs);
                return Ok(Some(frame_at_target));
            }
            if let Some(aim) = self.seek_aim_secs {
                // The seek came down after the target, so the picture that
                // is showing then lies further back.
                self.retry_seek_earlier(aim)?;
                continue;
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
