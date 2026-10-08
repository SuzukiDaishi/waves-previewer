//! The committed ADM fixtures, read into scenes. What each file holds is in
//! `test_samples/formats/README.md`; this pins what the app makes of it.

use std::path::{Path, PathBuf};

use neowaves::audio_channels::SpeakerPos;
use neowaves::spatial::scene::{Coords, ElementKind, ObjectScene};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples/formats")
        .join(name)
}

fn load(name: &str) -> ObjectScene {
    neowaves::adm::load_scene(&fixture(name), None, &mut |_| {})
        .unwrap_or_else(|err| panic!("{name}: {err:#}"))
}

#[test]
fn the_list_row_is_read_from_chna_alone() {
    let summary = neowaves::adm::probe_summary(&fixture("adm_bw64_objects.wav"))
        .unwrap()
        .expect("an ADM file");
    assert_eq!(&*summary.label, "ADM \u{b7} 2 bed + 2 obj");
    assert_eq!(summary.tracks, 4);
    let hoa = neowaves::adm::probe_summary(&fixture("adm_hoa_pack.wav"))
        .unwrap()
        .unwrap();
    assert_eq!(&*hoa.label, "ADM \u{b7} 1 obj + 1 other");
    // A WAVE without ADM chunks is not object audio.
    assert!(neowaves::adm::probe_summary(&fixture("ch_12.wav"))
        .unwrap()
        .is_none());
}

#[test]
fn a_master_becomes_two_bed_channels_and_two_objects() {
    let scene = load("adm_bw64_objects.wav");
    assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
    assert_eq!(scene.programme.as_deref(), Some("Fixture"));
    assert_eq!(scene.shape.tracks, 4);
    assert_eq!(scene.elements.len(), 4);

    // The bed comes from common definitions the file never writes out.
    let speakers: Vec<Option<SpeakerPos>> = scene
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            ElementKind::Bed { speaker, .. } => Some(*speaker),
            ElementKind::Object => None,
        })
        .collect();
    assert_eq!(speakers, vec![Some(SpeakerPos::Fl), Some(SpeakerPos::Fr)]);

    let sweep = scene.element("adm:AO_1002:AC_00031001").expect("the sweep");
    assert_eq!(sweep.track, 2);
    assert_eq!(sweep.coords, Coords::Cartesian);
    assert_eq!(&*sweep.group, "Main");
    assert_eq!(sweep.active, Some((0.0, 0.5)));
    // The repeated last block is gone; the width is kept, verbatim.
    assert_eq!(sweep.keyframes.len(), 2);
    assert_eq!(
        sweep.keyframes[0].extras.as_deref(),
        Some("<width>0.1</width>")
    );
    let at = |secs| sweep.cart_at(secs).unwrap();
    assert_eq!(at(0.0), [-1.0, 1.0, 0.0]);
    assert!(
        (at(0.25)[0]).abs() < 1e-5,
        "centre of the front wall at 0.25 s"
    );
    assert_eq!(at(0.46), [1.0, 1.0, 0.0]);

    let jump = scene.element("adm:AO_1003:AC_00031002").expect("the jump");
    assert_eq!(&*jump.name, "Jump & glide", "an entity in the name");
    assert_eq!(jump.track, 3);
    assert_eq!(jump.coords, Coords::Polar);
    assert_eq!(jump.keyframes.len(), 2);
    let second = &jump.keyframes[1];
    assert!(
        (second.secs - 0.35).abs() < 1e-9,
        "arrives after its 0.1 s glide"
    );
    assert!((second.ramp_secs - 0.1).abs() < 1e-9);
    assert!((second.gain - 0.5).abs() < 1e-4, "-6 dB");
    assert_eq!(jump.sample_at(0.2).unwrap().0, [30.0, 0.0, 1.0], "held");
}

#[test]
fn with_no_programme_every_top_level_object_plays() {
    let scene = load("adm_riff_small.wav");
    assert_eq!(scene.programme, None);
    assert_eq!(scene.elements.len(), 4);
    assert_eq!(scene.object_count(), 2);
}

#[test]
fn a_truncated_document_keeps_what_was_complete() {
    let scene = load("adm_axml_truncated.wav");
    assert!(
        scene.diagnostics.iter().any(|d| d.contains("ends inside")),
        "{:?}",
        scene.diagnostics
    );
    let jump = scene
        .element("adm:AO_1003:AC_00031002")
        .expect("the polar object's first block survived");
    assert_eq!(jump.keyframes.len(), 1);
    assert!(scene.element("adm:AO_1002:AC_00031001").is_some());
}

#[test]
fn hoa_is_counted_but_not_rendered() {
    let scene = load("adm_hoa_pack.wav");
    assert_eq!(scene.elements.len(), 1);
    assert_eq!(&*scene.elements[0].name, "Voice");
    assert!(
        scene.diagnostics.iter().any(|d| d.contains("not rendered")),
        "{:?}",
        scene.diagnostics
    );
}

fn temp_wav(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "neowaves_adm_export_{}_{label}.wav",
        std::process::id()
    ))
}

fn export(
    name: &str,
    scene: &ObjectScene,
    edits: Option<&neowaves::spatial::scene::SceneEdits>,
    label: &str,
) -> PathBuf {
    let dest = temp_wav(label);
    neowaves::adm::export::export_adm(
        &fixture(name),
        &dest,
        scene,
        edits,
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap_or_else(|err| panic!("{name}: {err:#}"));
    dest
}

#[test]
fn an_export_without_edits_is_the_source_byte_for_byte() {
    for name in ["adm_bw64_objects.wav", "adm_riff_small.wav"] {
        let scene = load(name);
        let dest = export(name, &scene, None, &format!("copy_{name}"));
        let same = std::fs::read(&dest).unwrap() == std::fs::read(fixture(name)).unwrap();
        let _ = std::fs::remove_file(&dest);
        assert!(same, "{name}: the copy differs");
    }
}

#[test]
fn an_exported_edit_reads_back_as_the_edit() {
    use neowaves::spatial::scene::{Keyframe, SceneEdits};
    use std::sync::Arc;
    let scene = load("adm_bw64_objects.wav");
    let key = "adm:AO_1002:AC_00031001";
    let width = Some(Arc::<str>::from("<width>0.1</width>"));
    let edited = vec![
        Keyframe {
            secs: 0.0,
            ramp_secs: 0.0,
            pos: [0.0, 1.0, 0.0],
            gain: 1.0,
            extras: width.clone(),
        },
        Keyframe {
            secs: 0.3,
            ramp_secs: 0.2,
            pos: [0.5, -0.5, 0.75],
            gain: 0.5,
            extras: width.clone(),
        },
    ];
    let mut edits = SceneEdits {
        shape: Some(scene.shape),
        elements: Default::default(),
    };
    edits
        .elements
        .insert(Arc::from(key), Arc::from(edited.clone()));
    let dest = export("adm_bw64_objects.wav", &scene, Some(&edits), "edited");
    let back = neowaves::adm::load_scene(&dest, None, &mut |_| {}).unwrap();
    let pcm_same = {
        let a = neowaves::wav_stream::read_wave_pcm_info(&dest)
            .unwrap()
            .unwrap();
        let b = neowaves::wav_stream::read_wave_pcm_info(&fixture("adm_bw64_objects.wav"))
            .unwrap()
            .unwrap();
        let read = |path: &Path, offset: u64, len: u64| {
            let bytes = std::fs::read(path).unwrap();
            bytes[offset as usize..(offset + len) as usize].to_vec()
        };
        a.frame_count == b.frame_count
            && read(&dest, a.data_offset, a.data_len)
                == read(&fixture("adm_bw64_objects.wav"), b.data_offset, b.data_len)
    };
    let _ = std::fs::remove_file(&dest);
    assert!(back.diagnostics.is_empty(), "{:?}", back.diagnostics);
    assert!(pcm_same, "the audio is copied unchanged");

    let element = back
        .element(key)
        .expect("the edited element is still there");
    assert_eq!(element.keyframes.len(), 2);
    for (got, want) in element.keyframes.iter().zip(&edited) {
        assert!((got.secs - want.secs).abs() < 1e-6, "{got:?} vs {want:?}");
        assert!(
            (got.ramp_secs - want.ramp_secs).abs() < 1e-6,
            "{got:?} vs {want:?}"
        );
        assert_eq!(got.pos, want.pos);
        assert!((got.gain - want.gain).abs() < 1e-6);
        assert_eq!(got.extras, want.extras, "the width is written back");
    }
    // The element nobody touched is as it was.
    assert_eq!(
        back.element("adm:AO_1003:AC_00031002")
            .map(|e| e.keyframes.clone()),
        scene
            .element("adm:AO_1003:AC_00031002")
            .map(|e| e.keyframes.clone())
    );
}
