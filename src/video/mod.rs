//! Video preview: reading the picture out of a video file.
//!
//! NeoWaves plays a video's *audio* through the ordinary decode path — a video
//! row in the list is an audio row that happens to live in an mp4. This module
//! exists only so the editor can show which picture the sound belongs to, and
//! so the list can show a thumbnail. Nothing here ever writes.
//!
//! Three backends sit behind one trait:
//!
//! * **ProRes**, in pure Rust (`oxideav-prores`), everywhere. No stock OS
//!   decoder reads it, so a ProRes track goes here first and never reaches
//!   the other two.
//! * **Media Foundation** on Windows, which decodes whatever the machine has a
//!   codec for (H.264, HEVC, VP9, ...), in hardware where possible.
//! * **OpenH264** everywhere, built from bundled C++ sources, which decodes
//!   H.264 and nothing else.
//!
//! Everything but ProRes tries the last two in that order. Failing both is not
//! an error the user needs to act on: the audio still plays and the panel says
//! the picture is not available, which is the honest outcome for an AV1 file.

pub mod annexb;
pub mod container;
pub mod frame;

#[cfg(feature = "video")]
mod decoder_openh264;

#[cfg(windows)]
mod decoder_mf;

mod decoder_prores;

// The fixture synthesises its H.264 with OpenH264's *encoder* and its audio
// with FDK, so it only exists when those are compiled in.
#[cfg(any())]
pub mod test_fixture;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

pub use container::{VideoCodec, VideoStreamInfo};
pub use frame::{Rotation, VideoFrame};

/// A positioned reader over a video track's pictures.
///
/// Implementations are `Send` but not `Sync`: one decoder belongs to one
/// worker thread from construction to drop, because both backends hold native
/// decoder state that is not safe to share.
pub trait VideoDecoder: Send {
    fn info(&self) -> &VideoStreamInfo;

    /// Prepare the decoder to produce pictures inside `box_px`.
    ///
    /// Software decoders may leave this as a no-op and scale in
    /// [`VideoDecoder::next_frame`]. Native backends can override it so their
    /// video processor performs the resize before pixels reach Rust. The
    /// returned size is the actual uncompressed frame carried by the backend.
    fn prepare_output(&mut self, box_px: (u32, u32)) -> Result<(u32, u32)> {
        Ok(box_px)
    }

    /// Position so the next [`VideoDecoder::next_frame`] returns the picture
    /// showing at `secs`.
    ///
    /// `max_forward_walk` is how many frames ahead the decoder may reach by
    /// decoding forward rather than restarting from a keyframe. Walking
    /// forward is what keeps ordinary playback smooth; restarting is what
    /// makes a scrub land quickly. The caller sizes it from the machine tier.
    fn seek(&mut self, secs: f64, max_forward_walk: usize) -> Result<()>;

    /// The next picture in presentation order, already rotated and scaled to
    /// fit `box_px`. `Ok(None)` means the track ended or the request was
    /// cancelled.
    fn next_frame(&mut self, box_px: (u32, u32), cancel: &AtomicBool)
        -> Result<Option<VideoFrame>>;
}

/// Why a file has no video preview.
#[derive(Clone, Debug)]
pub enum VideoOpenError {
    /// The container has no video track (an audio-only `.mp4`, which is just
    /// an `.m4a` by another name).
    NoVideoTrack,
    /// There is a video track, but nothing in this build decodes it.
    UnsupportedCodec(String),
    /// The container or the decoder refused the file.
    Failed(String),
}

impl std::fmt::Display for VideoOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoOpenError::NoVideoTrack => write!(f, "no video track"),
            VideoOpenError::UnsupportedCodec(codec) => write!(f, "{codec} not supported"),
            VideoOpenError::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

/// What a file's video track looks like, without opening a decoder.
///
/// Used to decide whether the editor should reserve a video panel at all, and
/// what to say in it when no decoder can be had.
pub fn probe_video_stream(path: &Path) -> Result<VideoStreamInfo, VideoOpenError> {
    if crate::mpegts::is_mpegts_path(path) {
        return ts_stream_info(path).map(|(info, _)| info);
    }
    match container::VideoContainer::open(path) {
        Ok(container) => Ok(container.info),
        Err(err) => Err(classify_open_error(&err)),
    }
}

fn classify_open_error(err: &anyhow::Error) -> VideoOpenError {
    let text = format!("{err:#}");
    if text.contains("no video track") {
        VideoOpenError::NoVideoTrack
    } else {
        VideoOpenError::Failed(text)
    }
}

/// A transport stream's picture as its timestamps describe it, and where its
/// first frame sits on the row's timeline (negative when the picture starts
/// before the audio).
///
/// The size is a 16:9 placeholder: a transport stream says nothing about it
/// short of the codec's own headers, and the decoder reports the real one
/// when it opens.
fn ts_stream_info(path: &Path) -> Result<(VideoStreamInfo, f64), VideoOpenError> {
    use crate::mpegts::{StreamKind, TsProbe, PTS_CLOCK_HZ};
    let probe = TsProbe::open(path).map_err(|err| VideoOpenError::Failed(format!("{err:#}")))?;
    let video = probe.first_video().ok_or(VideoOpenError::NoVideoTrack)?;
    let start_secs = match (video.first_pts, crate::audio_mpegts::timeline_zero_pts(&probe)) {
        (Some(first), Some(zero)) => TsProbe::secs_between(first, zero),
        _ => 0.0,
    };
    let codec = match video.kind {
        StreamKind::H264 => VideoCodec::H264,
        StreamKind::Hevc => VideoCodec::H265,
        _ => VideoCodec::Unknown,
    };
    let nominal_fps = video
        .pts_step
        .filter(|step| *step > 0)
        .map(|step| (PTS_CLOCK_HZ / step as f64) as f32)
        .unwrap_or(0.0);
    let duration_secs = video
        .span_secs()
        .map(|span| (start_secs + span).max(0.0))
        .unwrap_or(0.0);
    let info = VideoStreamInfo {
        coded_width: 16,
        coded_height: 9,
        display_width: 16,
        display_height: 9,
        rotation: Rotation::None,
        duration_secs,
        nominal_fps,
        codec_label: video.kind.label().to_string(),
        codec,
    };
    Ok((info, start_secs))
}

/// A transport stream's decoder: Media Foundation reads `.mts` / `.m2ts`
/// itself, so only the timing comes from [`crate::mpegts`].
fn open_ts_decoder(path: &Path) -> Result<Box<dyn VideoDecoder>, VideoOpenError> {
    let (info, picture_start_secs) = ts_stream_info(path)?;
    #[cfg(windows)]
    {
        let failed = |err: anyhow::Error| VideoOpenError::Failed(format!("{err:#}"));
        let mut decoder =
            decoder_mf::MediaFoundationDecoder::open(path, Rotation::None).map_err(failed)?;
        decoder.adopt_container_info(&info);
        decoder.align_to_timeline(picture_start_secs).map_err(failed)?;
        Ok(Box::new(decoder))
    }
    #[cfg(not(windows))]
    {
        // The OpenH264 path needs a sample index, which this demuxer does
        // not build; the audio still plays.
        let _ = (info, picture_start_secs);
        Err(VideoOpenError::UnsupportedCodec(
            "MPEG-TS video preview is Windows-only".to_string(),
        ))
    }
}

/// Open a decoder for `path`, preferring the OS decoder where there is one.
pub fn open_video_decoder(path: &Path) -> Result<Box<dyn VideoDecoder>, VideoOpenError> {
    if crate::mpegts::is_mpegts_path(path) {
        return open_ts_decoder(path);
    }
    // The container is parsed first either way: it is the only source of the
    // duration, frame rate and codec name the UI shows, and on the OpenH264
    // path it is also the demuxer.
    let container = match container::VideoContainer::open(path) {
        Ok(container) => container,
        Err(err) => return Err(classify_open_error(&err)),
    };

    // Media Foundation has no ProRes decoder on a stock install, so trying it
    // first would only log a misleading failure after building a source
    // reader over what is often a multi-gigabyte file.
    if container.info.codec == VideoCodec::ProRes {
        return decoder_prores::ProResDecoder::open(container)
            .map(|decoder| Box::new(decoder) as Box<dyn VideoDecoder>)
            .map_err(|err| VideoOpenError::Failed(format!("{err:#}")));
    }

    #[cfg(windows)]
    {
        match decoder_mf::MediaFoundationDecoder::open(path, container.info.rotation) {
            Ok(mut decoder) => {
                decoder.adopt_container_info(&container.info);
                return Ok(Box::new(decoder));
            }
            Err(err) => {
                // Not fatal: fall through to the bundled decoder, which may
                // still handle an H.264 file the OS refused.
                eprintln!(
                    "video: Media Foundation could not open {}: {err:#}",
                    path.display()
                );
            }
        }
    }

    #[cfg(feature = "video")]
    {
        let codec = container.info.codec;
        let label = container.info.codec_label.clone();
        match decoder_openh264::OpenH264Decoder::open(container) {
            Ok(decoder) => return Ok(Box::new(decoder)),
            Err(err) => {
                if codec == VideoCodec::H264 {
                    return Err(VideoOpenError::Failed(format!("{err:#}")));
                }
                return Err(VideoOpenError::UnsupportedCodec(label));
            }
        }
    }

    #[cfg(not(feature = "video"))]
    {
        let _ = container;
        Err(VideoOpenError::UnsupportedCodec(
            "video preview disabled in this build".to_string(),
        ))
    }
}

/// One frame to stand in for the whole file: the first picture of the movie.
///
/// Used for the list thumbnail when a video carries no embedded cover art.
/// Opens a decoder, takes one frame and closes it again — nothing is kept
/// alive, because this runs across a folder's worth of files.
pub fn decode_poster_frame(path: &Path, max_dim: u32) -> Option<VideoFrame> {
    let mut decoder = open_video_decoder(path).ok()?;
    let cancel = AtomicBool::new(false);
    decoder.seek(0.0, usize::MAX).ok()?;
    decoder
        .next_frame((max_dim.max(1), max_dim.max(1)), &cancel)
        .ok()?
}

#[cfg(any())]
mod tests {
    use super::*;
    use crate::video::test_fixture::{frame_color, FixtureFile, FIXTURE_FPS};

    /// Which frame a decoded pixel came from.
    ///
    /// The fixture goes RGB -> YUV 4:2:0 -> H.264 -> back and is then box
    /// filtered down to the panel size, so the colour never returns exactly.
    /// Matching the *nearest* frame of the ramp is both robust to that and a
    /// stricter assertion than a tolerance would be — it fails if the wrong
    /// frame comes back, however close its colour happens to land.
    fn nearest_frame_index(pixel: egui::Color32, frames: usize) -> Option<usize> {
        (0..frames).min_by_key(|i| {
            let want = frame_color(*i, frames);
            (pixel.r() as i32 - want[0] as i32).abs() + (pixel.b() as i32 - want[2] as i32).abs()
        })
    }

    fn center_pixel(image: &egui::ColorImage) -> egui::Color32 {
        let [w, h] = image.size;
        image.pixels[(h / 2) * w + w / 2]
    }

    #[test]
    fn a_video_track_is_described_before_anything_is_decoded() {
        let fixture = FixtureFile::build("probe", 8, 0.0).expect("fixture");
        let info = probe_video_stream(&fixture.path).expect("probe");
        assert_eq!(info.coded_width, 64);
        assert_eq!(info.coded_height, 64);
        assert_eq!(info.codec, VideoCodec::H264);
        assert_eq!(info.rotation, Rotation::None);
        assert!((info.aspect() - 1.0).abs() < 0.01);
        assert!(
            (info.duration_secs - 8.0 / FIXTURE_FPS as f64).abs() < 0.2,
            "duration {} for 8 frames at {FIXTURE_FPS} fps",
            info.duration_secs
        );
    }

    #[test]
    fn the_poster_frame_is_the_first_picture_of_the_movie() {
        let frames = 8;
        let fixture = FixtureFile::build("poster", frames, 0.0).expect("fixture");
        let frame = decode_poster_frame(&fixture.path, 40).expect("poster frame");
        assert!(frame.image.size[0] > 0 && frame.image.size[1] > 0);
        assert!(frame.image.size[0] <= 40 && frame.image.size[1] <= 40);
        assert_eq!(
            nearest_frame_index(center_pixel(&frame.image), frames),
            Some(0),
            "poster frame should be frame 0"
        );
    }

    #[test]
    fn frames_come_back_in_presentation_order_with_rising_timestamps() {
        let frames = 8;
        let fixture = FixtureFile::build("order", frames, 0.0).expect("fixture");
        let mut decoder = open_video_decoder(&fixture.path).expect("decoder");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        decoder.seek(0.0, usize::MAX).expect("seek to start");

        let mut seen = Vec::new();
        while let Some(frame) = decoder.next_frame((64, 64), &cancel).expect("decode") {
            seen.push(frame.pts_secs);
        }
        assert_eq!(seen.len(), frames, "every frame should be delivered once");
        assert!(
            seen.windows(2).all(|w| w[1] > w[0]),
            "timestamps must rise: {seen:?}"
        );
        let expected_last = (frames - 1) as f64 / FIXTURE_FPS as f64;
        assert!((seen[frames - 1] - expected_last).abs() < 0.01);
    }

    #[test]
    fn seeking_lands_on_the_frame_showing_at_that_time() {
        let frames = 8;
        let fixture = FixtureFile::build("seek", frames, 0.0).expect("fixture");
        let mut decoder = open_video_decoder(&fixture.path).expect("decoder");
        let cancel = std::sync::atomic::AtomicBool::new(false);

        // Out of order on purpose: a backwards seek is the case that has to
        // restart the decoder from a keyframe.
        for target in [5usize, 1, 7, 0, 3] {
            let secs = target as f64 / FIXTURE_FPS as f64;
            decoder.seek(secs, 2).expect("seek");
            let frame = decoder
                .next_frame((64, 64), &cancel)
                .expect("decode")
                .unwrap_or_else(|| panic!("no frame at {secs}s"));
            assert!(
                (frame.pts_secs - secs).abs() < 0.01,
                "asked for {secs}s, got {}s",
                frame.pts_secs
            );
            assert_eq!(
                nearest_frame_index(center_pixel(&frame.image), frames),
                Some(target),
                "picture at {secs}s should be frame {target}"
            );
        }
    }

    #[test]
    fn a_seek_between_frames_shows_the_frame_already_on_screen() {
        let frames = 8;
        let fixture = FixtureFile::build("between", frames, 0.0).expect("fixture");
        let mut decoder = open_video_decoder(&fixture.path).expect("decoder");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        // Two thirds of the way through frame 3's slot.
        let secs = (3.0 + 0.66) / FIXTURE_FPS as f64;
        decoder.seek(secs, usize::MAX).expect("seek");
        let frame = decoder
            .next_frame((64, 64), &cancel)
            .expect("decode")
            .expect("frame");
        assert_eq!(
            nearest_frame_index(center_pixel(&frame.image), frames),
            Some(3)
        );
    }

    #[test]
    fn a_file_that_is_not_a_video_reports_why_rather_than_failing_loudly() {
        let dir = std::env::temp_dir().join("neowaves_video_fixtures");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("not_a_video_{}.mp4", std::process::id()));
        std::fs::write(&path, b"this is not an mp4").expect("write stub");
        assert!(matches!(
            probe_video_stream(&path),
            Err(VideoOpenError::Failed(_))
        ));
        assert!(decode_poster_frame(&path, 40).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_video_does_not_panic() {
        let fixture = FixtureFile::build("truncated", 8, 0.0).expect("fixture");
        let bytes = std::fs::read(&fixture.path).expect("read fixture");
        let cut = bytes.len() * 6 / 10;
        let truncated = fixture.path.with_extension("cut.mp4");
        std::fs::write(&truncated, &bytes[..cut]).expect("write truncated");
        // Either it opens and yields nothing useful, or it refuses. Both are
        // fine; crashing is not.
        match open_video_decoder(&truncated) {
            Ok(mut decoder) => {
                let cancel = std::sync::atomic::AtomicBool::new(false);
                let _ = decoder.seek(0.0, usize::MAX);
                let _ = decoder.next_frame((32, 32), &cancel);
            }
            Err(_) => {}
        }
        let _ = std::fs::remove_file(&truncated);
    }
}

/// Transport streams through Media Foundation, against the fixtures in
/// `test_samples/video`.
#[cfg(all(test, windows))]
mod mpegts_tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test_samples")
            .join("video")
            .join(name)
    }

    /// Whether the sync fixture's yellow "second N" box is lit in `image`.
    fn second_box_lit(image: &egui::ColorImage, second: usize) -> bool {
        let [w, h] = image.size;
        let x = ((288 * second + 120) as f32 / 1920.0 * w as f32) as usize;
        let y = (891.0 / 1080.0 * h as f32) as usize;
        let p = image.pixels[y * w + x];
        p.r() > 150 && p.g() > 150 && p.b() < 110
    }

    #[test]
    fn an_mts_picture_follows_its_timestamps_onto_the_audio_timeline() {
        let mut decoder = open_video_decoder(&fixture("mts_sync_ac3_6s.mts")).expect("decoder");
        assert_eq!(decoder.info().codec, VideoCodec::H264);
        assert!((decoder.info().nominal_fps - 30.0).abs() < 0.5);
        let cancel = AtomicBool::new(false);
        // The picture starts 0.295 s before the audio, so timeline t shows
        // movie time t + 0.295. Aim at the middle of each movie second, going
        // backwards as well as forwards.
        for movie_second in [0usize, 3, 1, 4, 2] {
            let timeline = movie_second as f64 + 0.5 - 0.295;
            decoder.seek(timeline, 2).expect("seek");
            let frame = decoder
                .next_frame((640, 360), &cancel)
                .expect("decode")
                .expect("frame");
            assert!(
                (frame.pts_secs - timeline).abs() < 0.05,
                "asked for {timeline}, got {}",
                frame.pts_secs
            );
            for second in 0..6 {
                assert_eq!(
                    second_box_lit(&frame.image, second),
                    second == movie_second,
                    "at {timeline}: box {second}"
                );
            }
        }
    }

    #[test]
    fn a_frame_on_a_whole_time_comes_back_on_exactly_that_time() {
        // No audio this build decodes, so the picture's first frame is the
        // timeline's zero and frame 9 of 30 fps is exactly 0.3 s. Adding the
        // source offset in seconds made it 0.30000000000000004, a frame from
        // the future for a playhead at 0.3.
        let mut decoder =
            open_video_decoder(&fixture("m2ts_eac3_unsupported.m2ts")).expect("decoder");
        let cancel = AtomicBool::new(false);
        decoder.seek(0.3, usize::MAX).expect("seek");
        let frame = decoder
            .next_frame((40, 40), &cancel)
            .expect("decode")
            .expect("frame");
        assert!(frame.pts_secs <= 0.3, "{}", frame.pts_secs);
        assert!(0.3 - frame.pts_secs < 1.0e-6, "{}", frame.pts_secs);
    }

    #[test]
    fn an_avchd_1440_wide_picture_is_shown_at_16_9() {
        // Stored 1440 wide with 4:3 pixels. Media Foundation may hand over
        // the stored frames or stretch them itself; either way the picture
        // is shown at 16:9, never stretched twice.
        let decoder = open_video_decoder(&fixture("mts_1440x1080i.mts")).expect("decoder");
        assert!(matches!(decoder.info().coded_width, 1440 | 1920));
        let aspect = decoder.info().aspect();
        assert!((aspect - 16.0 / 9.0).abs() < 0.01, "{aspect}");
    }

    #[test]
    fn an_mts_is_described_and_gives_a_poster_frame() {
        let path = fixture("mts_no_audio.mts");
        let info = probe_video_stream(&path).expect("probe");
        assert_eq!(info.codec_label, "H.264");
        assert!((info.duration_secs - 2.0).abs() < 0.05, "{}", info.duration_secs);
        // Media Foundation's MPEG-2 source, sought back to zero, sometimes
        // came down just past the only keyframe and read to the end without
        // a picture: a poster frame that was there one time in two.
        for attempt in 0..10 {
            let frame = decode_poster_frame(&path, 40)
                .unwrap_or_else(|| panic!("no poster frame on attempt {attempt}"));
            assert!(frame.image.size[0] > 0 && frame.image.size[0] <= 40);
        }
        // .m2ts reaches Media Foundation's MPEG-2 source as readily as .mts,
        // whether or not its audio is one this build decodes.
        for name in ["m2ts_ac3_51.m2ts", "m2ts_eac3_unsupported.m2ts"] {
            assert!(decode_poster_frame(&fixture(name), 40).is_some(), "{name}");
        }
    }

    #[test]
    fn a_seek_into_a_stream_with_one_keyframe_still_finds_its_picture() {
        // The fixture is two seconds with a keyframe only at the start, so
        // any seek past it must be retried from further back.
        let mut decoder = open_video_decoder(&fixture("mts_no_audio.mts")).expect("decoder");
        let cancel = AtomicBool::new(false);
        for target in [1.0, 0.4, 1.7, 1.0] {
            decoder.seek(target, 0).expect("seek");
            let frame = decoder
                .next_frame((40, 40), &cancel)
                .expect("decode")
                .unwrap_or_else(|| panic!("no picture at {target}"));
            assert!(
                frame.pts_secs <= target + 1.0e-6 && target - frame.pts_secs < 0.04,
                "asked for {target}, got {}",
                frame.pts_secs
            );
        }
    }
}
