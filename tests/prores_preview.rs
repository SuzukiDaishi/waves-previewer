//! ProRes preview: the picture of a ProRes `.mov` reaches the panel.
//!
//! Media Foundation ships no ProRes decoder, so before `oxideav-prores` a
//! ProRes file -- the usual export for a lyric or graphics overlay with
//! transparency -- had no picture on any platform. These run the committed
//! fixtures (see `test_samples/video/README.md`) through the public video API.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use neowaves::video::{
    decode_poster_frame, open_video_decoder, probe_video_stream, VideoCodec, VideoDecoder,
};

const FPS: f64 = 24.0;
const FRAMES: usize = 4;
/// The left half of frames 0-3.
const FRAME_COLORS: [[u8; 3]; FRAMES] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join(name)
}

const ALPHA_4444: &str = "prores_4444_alpha_64x48.mov";
const OPAQUE_422: &str = "prores_422hq_64x48.mov";

fn pixel(image: &egui::ColorImage, x: usize, y: usize) -> [u8; 3] {
    let p = image.pixels[y * image.size[0] + x];
    [p.r(), p.g(), p.b()]
}

/// Within what a 10-bit 4:4:4 / 4:2:2 round trip through Y'CbCr costs.
fn assert_close(got: [u8; 3], want: [u8; 3], what: &str) {
    let off = got
        .iter()
        .zip(want)
        .map(|(g, w)| (i32::from(*g) - i32::from(w)).abs())
        .max()
        .unwrap_or(0);
    assert!(off <= 12, "{what}: got {got:?}, expected about {want:?}");
}

/// Decode the frame showing at `frame_index`, at the fixture's own size so no
/// scaling blurs the colours.
fn frame_at(decoder: &mut dyn VideoDecoder, frame_index: usize) -> neowaves::video::VideoFrame {
    let cancel = AtomicBool::new(false);
    // A quarter of a frame in, so the lookup is not at the mercy of rounding
    // on an exact frame boundary.
    let secs = (frame_index as f64 + 0.25) / FPS;
    decoder.seek(secs, 2).expect("seek");
    decoder
        .next_frame((64, 48), &cancel)
        .expect("decode")
        .unwrap_or_else(|| panic!("no picture for frame {frame_index}"))
}

#[test]
fn prores_tracks_are_recognised_and_named_by_their_chroma() {
    for (name, label) in [(ALPHA_4444, "ProRes 4444"), (OPAQUE_422, "ProRes 422")] {
        let info = probe_video_stream(&fixture(name)).expect("probe");
        assert_eq!(info.codec, VideoCodec::ProRes, "{name}");
        assert_eq!(info.codec_label, label, "{name}");
        assert_eq!((info.coded_width, info.coded_height), (64, 48), "{name}");
        assert!(
            (info.duration_secs - FRAMES as f64 / FPS).abs() < 0.01,
            "{name}: duration {}",
            info.duration_secs
        );
    }
}

#[test]
fn a_prores_file_opens_on_the_prores_decoder() {
    // Not Media Foundation, which has no ProRes decoder and would fail.
    let decoder = open_video_decoder(&fixture(ALPHA_4444)).expect("open");
    assert_eq!(decoder.info().codec, VideoCodec::ProRes);
    assert_eq!(decoder.info().codec_label, "ProRes 4444");
}

#[test]
fn seeking_shows_the_frame_at_that_time_in_any_order() {
    for name in [ALPHA_4444, OPAQUE_422] {
        let mut decoder = open_video_decoder(&fixture(name)).expect("open");
        // Backwards as well as forwards: every ProRes frame stands alone.
        for index in [2usize, 0, 3, 1] {
            let frame = frame_at(decoder.as_mut(), index);
            assert!(
                (frame.pts_secs - index as f64 / FPS).abs() < 0.002,
                "{name}: asked for frame {index}, got pts {}",
                frame.pts_secs
            );
            assert_eq!(frame.image.size, [64, 48], "{name}");
            assert_close(
                pixel(&frame.image, 16, 24),
                FRAME_COLORS[index],
                &format!("{name} frame {index}"),
            );
        }
    }
}

#[test]
fn transparent_pixels_show_the_checkerboard_and_opaque_files_do_not() {
    let mut alpha = open_video_decoder(&fixture(ALPHA_4444)).expect("open 4444");
    let frame = frame_at(alpha.as_mut(), 0);
    // Two neighbouring checkerboard cells (8 px at this 1:1 size), both on the
    // transparent right half: a dark neutral grey, and not the same one.
    let a = pixel(&frame.image, 36, 4);
    let b = pixel(&frame.image, 44, 4);
    for p in [a, b] {
        assert!(p[0] == p[1] && p[1] == p[2], "checkerboard is grey: {p:?}");
        assert!(p[0] > 20 && p[0] < 90, "checkerboard is dark: {p:?}");
    }
    assert_ne!(a, b, "adjacent cells alternate");

    // The 422 file has no alpha; its right half is simply black.
    let mut opaque = open_video_decoder(&fixture(OPAQUE_422)).expect("open 422");
    let frame = frame_at(opaque.as_mut(), 0);
    assert_close(pixel(&frame.image, 36, 4), [0, 0, 0], "422 right half");
    assert_close(pixel(&frame.image, 44, 4), [0, 0, 0], "422 right half");
}

#[test]
fn frames_are_shrunk_into_the_panel_box() {
    let mut decoder = open_video_decoder(&fixture(ALPHA_4444)).expect("open");
    let cancel = AtomicBool::new(false);
    decoder.seek(0.0, 2).expect("seek");
    let frame = decoder
        .next_frame((32, 32), &cancel)
        .expect("decode")
        .expect("frame");
    assert_eq!(frame.image.size, [32, 24], "aspect kept, fit inside the box");
    assert_close(pixel(&frame.image, 8, 12), FRAME_COLORS[0], "shrunk frame 0");
}

#[test]
fn playback_walks_every_frame_then_ends() {
    let mut decoder = open_video_decoder(&fixture(OPAQUE_422)).expect("open");
    let cancel = AtomicBool::new(false);
    decoder.seek(0.0, 2).expect("seek");
    let mut seen = Vec::new();
    while let Some(frame) = decoder.next_frame((64, 48), &cancel).expect("decode") {
        seen.push(frame.pts_secs);
    }
    assert_eq!(seen.len(), FRAMES, "every frame once: {seen:?}");
    assert!(seen.windows(2).all(|w| w[1] > w[0]), "rising: {seen:?}");
}

#[test]
fn the_list_thumbnail_is_the_first_frame() {
    let frame = decode_poster_frame(&fixture(ALPHA_4444), 40).expect("poster frame");
    assert_eq!(frame.image.size, [40, 30]);
    assert_close(pixel(&frame.image, 10, 15), FRAME_COLORS[0], "poster");
}

#[test]
fn a_damaged_frame_is_an_error_not_a_crash() {
    let bytes = std::fs::read(fixture(ALPHA_4444)).expect("read fixture");
    let icpf = bytes
        .windows(4)
        .position(|w| w == b"icpf")
        .expect("a ProRes frame");
    let mut damaged = bytes.clone();
    // Past the frame header, into the coded slices of the first frame.
    for byte in damaged.iter_mut().skip(icpf + 200).take(400) {
        *byte = 0xA5;
    }
    let dir = std::env::temp_dir().join("neowaves_prores_fixtures");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("damaged_{}.mov", std::process::id()));
    std::fs::write(&path, &damaged).expect("write damaged copy");

    // Opening, seeking and decoding may each refuse; none may panic.
    if let Ok(mut decoder) = open_video_decoder(&path) {
        let cancel = AtomicBool::new(false);
        let _ = decoder.seek(0.0, 2);
        let _ = decoder.next_frame((64, 48), &cancel);
    }
    let _ = std::fs::remove_file(&path);
}
