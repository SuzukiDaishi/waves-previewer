//! Impulse responses for the Reverb, with no UI: what one is called
//! (`IrRef`), a compact handle to that name which a `Copy` settings struct
//! can carry (`IrId`), and one decoded and resampled to a buffer's rate
//! (`ImpulseResponse`).
//!
//! A library IR is named by its path inside the library ("Rooms/.../x.wav"),
//! never by where the library sits on this machine: a session that says
//! `lib:Rooms/...` opens on a colleague's machine with the library somewhere
//! else. `resolve` finds it among the roots this machine has.
//!
//! Loading blocks (it reads and resamples a file): workers only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The IR the Reverb uses until one is chosen: a small church, a room most
/// material sits well in. It is in the installer's selection
/// (`assets/ir/bundled.txt`).
pub const DEFAULT_IR: &str =
    "Rooms/Historical/Religious/Sunnybrook Farm Willowdale Church/Sunnybrook Farm Willowdale Church.wav";

/// The envelope drawn for an IR: one point per this much of it.
const ENVELOPE_HOP_SECS: f64 = 0.005;
/// ...and never more points than this, however long it is.
const ENVELOPE_MAX_POINTS: usize = 4_096;
/// The floor the envelope is measured against, in dBFS.
const ENVELOPE_FLOOR_DB: f32 = -120.0;
/// Silence put either side of an IR before it is resampled, in samples of
/// the file: the resampler's filter needs room, or what sits at the very
/// start -- an IR's direct sound -- comes out as nothing.
const RESAMPLE_PAD_FRAMES: usize = 256;
/// The least input the resampler is given: shorter than one of its blocks
/// (2,048 frames), it returns silence. A short IR is followed by silence up
/// to this length, which is cut off again after.
const RESAMPLE_MIN_INPUT_FRAMES: usize = 16_384;

/// Which IR: one in the library, by its path inside it, or a file anywhere.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IrRef {
    /// A path inside the library, with `/` between its parts.
    Library(String),
    File(PathBuf),
}

impl IrRef {
    /// How a session writes it: `lib:<path in the library>` or `file:<path>`.
    pub fn to_session_string(&self) -> String {
        match self {
            IrRef::Library(rel) => format!("lib:{rel}"),
            IrRef::File(path) => format!("file:{}", path.display()),
        }
    }

    /// The other way round. A bare path inside the library is accepted too.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if let Some(rel) = text.strip_prefix("lib:") {
            return Some(IrRef::Library(normalize_rel(rel)));
        }
        if let Some(path) = text.strip_prefix("file:") {
            return Some(IrRef::File(PathBuf::from(path)));
        }
        Some(IrRef::Library(normalize_rel(text)))
    }

    /// What to call it on screen: its file name without the extension.
    pub fn name(&self) -> String {
        let path = match self {
            IrRef::Library(rel) => Path::new(rel.as_str()),
            IrRef::File(path) => path.as_path(),
        };
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.to_session_string())
    }
}

/// A library path with `/` separators and no leading one.
pub fn normalize_rel(rel: &str) -> String {
    rel.trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

/// An `IrRef` interned for this process: small and `Copy`, so the Reverb's
/// settings stay a plain value that undo, the preview key and sessions all
/// handle like any other setting. A session never stores the number -- it
/// serializes as the `IrRef` it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IrId(u32);

#[derive(Default)]
struct Interner {
    refs: Vec<IrRef>,
    ids: HashMap<IrRef, u32>,
}

fn interner() -> &'static Mutex<Interner> {
    static INTERNER: OnceLock<Mutex<Interner>> = OnceLock::new();
    INTERNER.get_or_init(Default::default)
}

impl IrId {
    pub fn of(r: &IrRef) -> Self {
        let mut table = interner().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(id) = table.ids.get(r) {
            return IrId(*id);
        }
        let id = table.refs.len() as u32;
        table.refs.push(r.clone());
        table.ids.insert(r.clone(), id);
        IrId(id)
    }

    /// The default IR (`DEFAULT_IR`).
    pub fn default_ir() -> Self {
        Self::of(&IrRef::Library(DEFAULT_IR.to_string()))
    }

    pub fn get(self) -> IrRef {
        let table = interner().lock().unwrap_or_else(|e| e.into_inner());
        table.refs[self.0 as usize].clone()
    }
}

impl Serialize for IrId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.get().to_session_string())
    }
}

impl<'de> Deserialize<'de> for IrId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        IrRef::parse(&text)
            .map(|r| IrId::of(&r))
            .ok_or_else(|| serde::de::Error::custom("an empty impulse response name"))
    }
}

/// An IR ready to convolve with: its channels at one buffer's rate.
#[derive(Debug)]
pub struct ImpulseResponse {
    pub id: IrId,
    pub name: String,
    /// The file it was read from.
    pub path: PathBuf,
    /// At `buffer_sr`.
    pub channels: Vec<Vec<f32>>,
    /// Frames of `channels` before the IR's time zero: the ringing a
    /// band-limited direct sound has before it, which resampling leaves
    /// (`resample_ir`). The reverb reads its output this much early, so the
    /// direct sound keeps both its level and its place.
    pub lead: usize,
    pub buffer_sr: u32,
    /// What the file was.
    pub file_sr: u32,
    pub file_frames: usize,
    /// The loudest sample of each `envelope_hop` frames, across channels, in
    /// dBFS: the decay the Inspector draws.
    pub envelope_db: Vec<f32>,
    pub envelope_hop: usize,
}

impl ImpulseResponse {
    /// From channels already at `buffer_sr` whose time zero is `lead` frames
    /// in (a test, or a file just read).
    #[allow(clippy::too_many_arguments)]
    pub fn from_channels(
        id: IrId,
        name: String,
        path: PathBuf,
        channels: Vec<Vec<f32>>,
        lead: usize,
        buffer_sr: u32,
        file_sr: u32,
        file_frames: usize,
    ) -> Self {
        let lead = lead.min(channels.first().map_or(0, Vec::len));
        // The envelope starts at time zero, as the plot does.
        let len = channels.first().map_or(0, Vec::len) - lead;
        let hop = crate::sample_rate::frames_for_secs(ENVELOPE_HOP_SECS, buffer_sr.max(1))
            .max(len.div_ceil(ENVELOPE_MAX_POINTS))
            .max(1);
        let envelope_db = (0..len.div_ceil(hop))
            .map(|i| {
                let (a, b) = (lead + i * hop, lead + ((i + 1) * hop).min(len));
                let peak = channels
                    .iter()
                    .flat_map(|c| c[a..b].iter())
                    .fold(0.0f32, |m, v| m.max(v.abs()));
                if peak > 0.0 {
                    (20.0 * peak.log10()).max(ENVELOPE_FLOOR_DB)
                } else {
                    ENVELOPE_FLOOR_DB
                }
            })
            .collect();
        Self {
            id,
            name,
            path,
            channels,
            lead,
            buffer_sr,
            file_sr,
            file_frames,
            envelope_db,
            envelope_hop: hop,
        }
    }

    /// Frames from time zero to the end.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len) - self.lead
    }

    /// "2 ch · 48 kHz · 2.20 s" -- what the file was.
    pub fn describe(&self) -> String {
        let secs = self.file_frames as f64 / f64::from(self.file_sr.max(1));
        let kind = match self.channels.len() {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            4 => "true stereo".to_string(),
            n => format!("{n} ch"),
        };
        format!(
            "{kind} \u{b7} {:.1} kHz \u{b7} {secs:.2} s",
            f64::from(self.file_sr) / 1000.0
        )
    }
}

/// Where `r` is on this machine: a file as it is, a library IR in the first
/// of `roots` that has it. Blocking.
pub fn resolve(r: &IrRef, roots: &[PathBuf]) -> Option<PathBuf> {
    match r {
        IrRef::File(path) => path.is_file().then(|| path.clone()),
        IrRef::Library(rel) => roots
            .iter()
            .map(|root| root.join(rel))
            .find(|path| path.is_file()),
    }
}

/// Read `id` (from `roots` when it is a library IR) and bring it to
/// `buffer_sr`. Blocking.
pub fn load(id: IrId, roots: &[PathBuf], buffer_sr: u32) -> Result<ImpulseResponse, String> {
    let r = id.get();
    let path = resolve(&r, roots).ok_or_else(|| match &r {
        IrRef::Library(rel) => format!("{rel} is not in the IR library"),
        IrRef::File(path) => format!("{} does not exist", path.display()),
    })?;
    let (channels, file_sr) = crate::audio_io::decode_audio_multi(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let file_frames = channels.first().map_or(0, Vec::len);
    if channels.is_empty() || file_frames == 0 {
        return Err(format!("{} holds no audio", path.display()));
    }
    let (channels, lead) = resample_ir(&channels, file_sr, buffer_sr);
    Ok(ImpulseResponse::from_channels(
        id,
        r.name(),
        path,
        channels,
        lead,
        buffer_sr,
        file_sr,
        file_frames,
    ))
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `channels` from `file_sr` to `buffer_sr` as a filter, not as a sound:
/// in place in time and with its gain kept. Returns the channels and how
/// many frames of them come before time zero.
///
/// The resampler loses what sits at the very start of its input (an IR's
/// direct sound, a unit impulse entirely), so the IR is padded with silence
/// either side first -- by a length the two rates divide exactly, so time
/// zero lands on a whole frame. The leading padding is kept: a band-limited
/// direct sound rings before it as well as after, and cutting that half off
/// would take 2.5 dB off a direct sound resampled to twice the rate. And a
/// waveform resampled to twice the rate has twice the samples, so as a
/// filter it would be twice as loud: the result is scaled by
/// `file_sr / buffer_sr`.
pub fn resample_ir(channels: &[Vec<f32>], file_sr: u32, buffer_sr: u32) -> (Vec<Vec<f32>>, usize) {
    if file_sr == buffer_sr || file_sr == 0 || buffer_sr == 0 || channels.is_empty() {
        return (channels.to_vec(), 0);
    }
    let (fin, fout) = (u64::from(file_sr), u64::from(buffer_sr));
    let g = gcd(fin, fout);
    let (unit_in, unit_out) = ((fin / g) as usize, (fout / g) as usize);
    let units = RESAMPLE_PAD_FRAMES.div_ceil(unit_in);
    let (pad_in, pad_out) = (unit_in * units, unit_out * units);
    let len = channels.iter().map(Vec::len).max().unwrap_or(0);
    let padded: Vec<Vec<f32>> = channels
        .iter()
        .map(|c| {
            let mut v = vec![0.0f32; pad_in];
            v.extend_from_slice(c);
            v.resize((pad_in + len + pad_in).max(RESAMPLE_MIN_INPUT_FRAMES), 0.0);
            v
        })
        .collect();
    let out = crate::wave::resample_channels_quality(
        &padded,
        file_sr,
        buffer_sr,
        crate::wave::ResampleQuality::Good,
    );
    // The ringing before time zero, the IR's length at the new rate, and
    // the ringing after it (the filter's own, which a short IR is mostly
    // made of).
    let keep = pad_out
        + ((len as u128 * u128::from(fout)).div_ceil(u128::from(fin)) as usize).max(1)
        + pad_out;
    let scale = (fin as f64 / fout as f64) as f32;
    let channels = out
        .into_iter()
        .map(|c| {
            let mut v: Vec<f32> = c.iter().take(keep).map(|x| x * scale).collect();
            v.resize(keep, 0.0);
            v
        })
        .collect();
    (channels, pad_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_string_names_the_same_ir() {
        let lib = IrRef::Library("Rooms/A Room/A Room.wav".into());
        assert_eq!(lib.to_session_string(), "lib:Rooms/A Room/A Room.wav");
        assert_eq!(IrRef::parse(&lib.to_session_string()), Some(lib.clone()));
        assert_eq!(IrRef::parse("Rooms\\A Room\\A Room.wav"), Some(lib.clone()));
        let file = IrRef::File(PathBuf::from("/x/y.wav"));
        assert_eq!(IrRef::parse(&file.to_session_string()), Some(file));
        assert_eq!(IrRef::parse("  "), None);
        assert_eq!(lib.name(), "A Room");
    }

    #[test]
    fn an_id_is_the_same_for_the_same_ref_and_serializes_as_it() {
        let a = IrId::of(&IrRef::Library("Plates/P.wav".into()));
        let b = IrId::of(&IrRef::Library("Plates/P.wav".into()));
        let c = IrId::of(&IrRef::Library("Plates/Q.wav".into()));
        assert_eq!(a, b);
        assert_ne!(a, c);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"lib:Plates/P.wav\"");
        let back: IrId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn a_library_ir_is_found_in_the_first_root_that_has_it() {
        let base = std::env::temp_dir().join(format!("neowaves_ir_resolve_{}", std::process::id()));
        let (first, second) = (base.join("first"), base.join("second"));
        for dir in [&first, &second] {
            std::fs::create_dir_all(dir.join("Rooms")).unwrap();
        }
        std::fs::write(second.join("Rooms/x.wav"), b"").unwrap();
        let r = IrRef::Library("Rooms/x.wav".into());
        let roots = vec![first.clone(), second.clone()];
        assert_eq!(resolve(&r, &roots), Some(second.join("Rooms/x.wav")));
        std::fs::write(first.join("Rooms/x.wav"), b"").unwrap();
        assert_eq!(resolve(&r, &roots), Some(first.join("Rooms/x.wav")));
        assert_eq!(
            resolve(&IrRef::Library("Rooms/none.wav".into()), &roots),
            None
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resampled_ir_keeps_its_timing_and_its_gain() {
        // The direct sound at the very start, a reflection 1,000 samples in.
        for (file_sr, buffer_sr) in [(44_100u32, 48_000u32), (192_000, 48_000), (48_000, 96_000)] {
            let len = file_sr as usize / 10;
            let mut h = vec![0.0f32; len];
            h[0] = 1.0;
            h[1_000] = 0.5;
            let (out, lead) = resample_ir(&[h], file_sr, buffer_sr);
            let ratio = f64::from(buffer_sr) / f64::from(file_sr);
            assert!(out[0].len() >= lead + (len as f64 * ratio).ceil() as usize);
            // The direct sound whole: what rings before time zero too.
            let early: f32 = out[0][..lead + 8].iter().sum();
            assert!(
                (early - 1.0).abs() < 0.1,
                "{file_sr}->{buffer_sr}: the direct sound, {early}"
            );
            let at = lead + (1_000.0 * ratio).round() as usize;
            let late: f32 = out[0][at - 8..at + 8].iter().sum();
            assert!(
                (late - 0.5).abs() < 0.05,
                "{file_sr}->{buffer_sr}: the reflection, {late}"
            );
            let peak = (at - 8..at + 8)
                .max_by(|a, b| out[0][*a].abs().total_cmp(&out[0][*b].abs()))
                .unwrap();
            assert!(peak.abs_diff(at) <= 1, "in place: {peak} vs {at}");
        }
        // A unit impulse of two samples (the library has one) survives,
        // with its gain.
        let (out, _) = resample_ir(&[vec![1.0, 0.0]], 44_100, 48_000);
        let sum: f32 = out[0].iter().sum();
        assert!((sum - 1.0).abs() < 0.05, "{sum}");
    }

    #[test]
    fn the_envelope_follows_the_decay() {
        let sr = 48_000;
        let decay: Vec<f32> = (0..sr as usize)
            .map(|i| (-(i as f32) / 4_800.0).exp())
            .collect();
        let ir = ImpulseResponse::from_channels(
            IrId::default_ir(),
            "t".into(),
            PathBuf::new(),
            vec![decay],
            0,
            sr,
            sr,
            sr as usize,
        );
        assert!(ir.envelope_db[0] > -1.0);
        assert!(
            ir.envelope_db.windows(2).all(|w| w[1] <= w[0] + 1e-3),
            "falls"
        );
        // 1/e per 0.1 s: about -87 dB at the end of the second.
        assert!(*ir.envelope_db.last().unwrap() < -80.0);
    }
}
