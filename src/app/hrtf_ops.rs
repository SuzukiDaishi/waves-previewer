//! Headphone monitoring through an HRTF: the settings (prefs), the SOFA
//! profiles and their loading, the virtual speaker room, and the per-frame
//! sync that hands `binaural::BinauralFilters` to the audio callback. See
//! `docs/MULTICHANNEL_SPEC.md`, section 6.
//!
//! Where a channel is heard from is decided in two steps. The channel layout
//! (`channel_layout_ops`) names its speaker; the room (here) says where that
//! speaker stands -- `SpeakerPos::direction` (ITU-R BS.775 / BS.2051 and
//! Dolby angles), moved by a room preset, moved again by the user's own
//! placement. A channel the layout names no speaker for (a 13-channel file,
//! a channel set to none) has a placement of its own per channel count.
//!
//! All of this is per user and per machine, like the volume: it lives in
//! prefs and never in a session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::audio_channels::{standard_layout_vec, Layout, SpeakerPos};
use crate::binaural::{BinauralFilters, ChannelRoute, Direction};

use super::loading_ops::{poll_job, JobPoll};
use super::WavesPreviewer;

/// The bundled HRTF: SADIE II subject D1, the KU100 dummy head, at 48 kHz.
pub(crate) const BUILTIN_SOFA_FILE: &str = "D1_48K_24bit_256tap_FIR_SOFA.sofa";
pub(crate) const BUILTIN_PROFILE_NAME: &str = "SADIE II D1 (KU100)";
/// The folder beside the executable the installer puts the bundled HRTF in.
const BUILTIN_DIR: &str = "hrtf";
/// Building the filters takes well under a millisecond (24 HRIR lookups and
/// FFTs); past this it is worth a line in the debug log.
const SLOW_FILTER_BUILD_MS: u128 = 2;
/// The LFE gain's range, in dB: off is its own mode, and +10 dB is the
/// in-band gain cinema calibration gives the LFE channel.
pub(crate) const LFE_GAIN_RANGE_DB: std::ops::RangeInclusive<f32> = -30.0..=10.0;
/// The output trim's range, in dB.
pub(crate) const TRIM_RANGE_DB: std::ops::RangeInclusive<f32> = -24.0..=12.0;
/// A speaker's own gain's range, in dB.
pub(crate) const SPEAKER_GAIN_RANGE_DB: std::ops::RangeInclusive<f32> = -30.0..=12.0;

/// Which HRTF is in use.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HrtfProfile {
    Builtin,
    File(PathBuf),
}

impl HrtfProfile {
    pub fn label(&self) -> String {
        match self {
            HrtfProfile::Builtin => BUILTIN_PROFILE_NAME.to_string(),
            HrtfProfile::File(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        }
    }

    fn pref_value(&self) -> String {
        match self {
            HrtfProfile::Builtin => "builtin".to_string(),
            HrtfProfile::File(path) => format!("file:{}", path.display()),
        }
    }

    fn from_pref(value: &str) -> Option<Self> {
        match value.trim() {
            "builtin" => Some(HrtfProfile::Builtin),
            other => other
                .strip_prefix("file:")
                .filter(|path| !path.is_empty())
                .map(|path| HrtfProfile::File(PathBuf::from(path))),
        }
    }
}

/// A set of speaker positions laid over the standard directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoomPreset {
    /// ITU-R BS.775 / BS.2051 and Dolby: `SpeakerPos::direction` as is.
    Standard,
    /// A square: front pair at +-45, rear pair at +-135 degrees.
    Quad,
    /// The square, with the upper layer straight above it at 35 degrees.
    Cube,
    /// Auro-3D: the height layer 30 degrees above the main speakers, the
    /// "voice of god" overhead.
    Auro3d,
}

impl RoomPreset {
    pub const ALL: [RoomPreset; 4] = [
        RoomPreset::Standard,
        RoomPreset::Quad,
        RoomPreset::Cube,
        RoomPreset::Auro3d,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RoomPreset::Standard => "Standard (ITU / Dolby)",
            RoomPreset::Quad => "Quad (square)",
            RoomPreset::Cube => "Cube",
            RoomPreset::Auro3d => "Auro-3D (30\u{b0} heights)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            RoomPreset::Standard => "standard",
            RoomPreset::Quad => "quad",
            RoomPreset::Cube => "cube",
            RoomPreset::Auro3d => "auro3d",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|preset| preset.key() == key.trim())
    }

    /// Where this room puts `pos`, if it differs from the standard.
    fn direction(self, pos: SpeakerPos) -> Option<(f32, f32)> {
        use SpeakerPos::*;
        match (self, pos) {
            (RoomPreset::Quad | RoomPreset::Cube, Fl) => Some((-45.0, 0.0)),
            (RoomPreset::Quad | RoomPreset::Cube, Fr) => Some((45.0, 0.0)),
            (RoomPreset::Quad | RoomPreset::Cube, Bl) => Some((-135.0, 0.0)),
            (RoomPreset::Quad | RoomPreset::Cube, Br) => Some((135.0, 0.0)),
            (RoomPreset::Cube, Tfl) => Some((-45.0, 35.0)),
            (RoomPreset::Cube, Tfr) => Some((45.0, 35.0)),
            (RoomPreset::Cube, Tbl) => Some((-135.0, 35.0)),
            (RoomPreset::Cube, Tbr) => Some((135.0, 35.0)),
            (RoomPreset::Auro3d, Tfl) => Some((-30.0, 30.0)),
            (RoomPreset::Auro3d, Tfr) => Some((30.0, 30.0)),
            (RoomPreset::Auro3d, Tfc) => Some((0.0, 30.0)),
            (RoomPreset::Auro3d, Tsl) => Some((-90.0, 30.0)),
            (RoomPreset::Auro3d, Tsr) => Some((90.0, 30.0)),
            (RoomPreset::Auro3d, Tbl) => Some((-110.0, 30.0)),
            (RoomPreset::Auro3d, Tbr) => Some((110.0, 30.0)),
            (RoomPreset::Auro3d, Tbc) => Some((180.0, 30.0)),
            _ => None,
        }
    }
}

/// Where a virtual speaker stands, and how loud it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpeakerPlacement {
    /// Degrees, negative to the left.
    pub azimuth_deg: f32,
    /// Degrees, positive up; negative for a lower layer.
    pub elevation_deg: f32,
    pub gain_db: f32,
}

impl SpeakerPlacement {
    pub fn at(azimuth_deg: f32, elevation_deg: f32) -> Self {
        Self {
            azimuth_deg,
            elevation_deg,
            gain_db: 0.0,
        }
    }

    fn to_pref(self) -> String {
        format!(
            "{}:{}:{}",
            self.azimuth_deg, self.elevation_deg, self.gain_db
        )
    }

    fn from_pref(text: &str) -> Option<Self> {
        let mut parts = text.split(':').map(|v| v.trim().parse::<f32>().ok());
        let (Some(Some(azimuth_deg)), Some(Some(elevation_deg)), Some(Some(gain_db))) =
            (parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        Some(Self {
            azimuth_deg: wrap_azimuth(azimuth_deg),
            elevation_deg: elevation_deg.clamp(-90.0, 90.0),
            gain_db,
        })
    }
}

/// An azimuth in (-180, 180].
pub(crate) fn wrap_azimuth(deg: f32) -> f32 {
    let wrapped = (deg + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 {
        180.0
    } else {
        wrapped
    }
}

/// One virtual speaker: a named position, or a channel the layout names no
/// speaker for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpeakerItem {
    Named(SpeakerPos),
    Unlabeled { channels: usize, ch: usize },
}

/// What the LFE channel does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LfeMode {
    /// Added to both ears alike: it has no direction.
    BothEars,
    Off,
}

/// The user's headphone settings (prefs).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HrtfSettings {
    pub enabled: bool,
    pub profile: HrtfProfile,
    /// SOFA files the user added, in the order they were added.
    pub user_files: Vec<PathBuf>,
    pub room: RoomPreset,
    /// The user's own placement per position, indexed like `SpeakerPos::ALL`.
    pub overrides: [Option<SpeakerPlacement>; SpeakerPos::ALL.len()],
    /// Placements of channels with no speaker, per channel count.
    pub unlabeled: BTreeMap<usize, Vec<SpeakerPlacement>>,
    pub lfe: LfeMode,
    pub lfe_gain_db: f32,
    pub lfe_lowpass: bool,
    pub trim_db: f32,
    /// Also put stereo on the room's L and R virtual speakers (off: stereo
    /// plays straight to the ears, and only 3 or more channels go through
    /// the HRTF).
    pub stereo_too: bool,
}

impl Default for HrtfSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: HrtfProfile::Builtin,
            user_files: Vec::new(),
            room: RoomPreset::Standard,
            overrides: [None; SpeakerPos::ALL.len()],
            unlabeled: BTreeMap::new(),
            lfe: LfeMode::BothEars,
            lfe_gain_db: 0.0,
            lfe_lowpass: false,
            trim_db: 0.0,
            stereo_too: true,
        }
    }
}

fn speaker_index(pos: SpeakerPos) -> usize {
    SpeakerPos::ALL
        .iter()
        .position(|p| *p == pos)
        .expect("ALL names every position")
}

impl HrtfSettings {
    /// Where `pos` stands in this room when the source's layout is `layout`.
    pub fn placement(&self, pos: SpeakerPos, layout: &[Option<SpeakerPos>]) -> SpeakerPlacement {
        if let Some(own) = self.overrides[speaker_index(pos)] {
            return own;
        }
        let (azimuth_deg, elevation_deg) = self
            .room
            .direction(pos)
            .unwrap_or_else(|| pos.direction(layout));
        SpeakerPlacement::at(azimuth_deg, elevation_deg)
    }

    pub fn has_override(&self, pos: SpeakerPos) -> bool {
        self.overrides[speaker_index(pos)].is_some()
    }

    pub fn set_override(&mut self, pos: SpeakerPos, placement: Option<SpeakerPlacement>) {
        self.overrides[speaker_index(pos)] = placement;
    }

    /// Where channel `ch` of a `channels`-channel source with no speaker
    /// stands: the user's placement, or evenly round the ear-level circle
    /// from straight ahead.
    pub fn unlabeled_placement(&self, channels: usize, ch: usize) -> SpeakerPlacement {
        self.unlabeled
            .get(&channels)
            .and_then(|list| list.get(ch).copied())
            .unwrap_or_else(|| default_unlabeled_placement(channels, ch))
    }

    /// Where `item` stands when the source's layout is `layout`.
    pub fn item_placement(
        &self,
        item: SpeakerItem,
        layout: &[Option<SpeakerPos>],
    ) -> SpeakerPlacement {
        match item {
            SpeakerItem::Named(pos) => self.placement(pos, layout),
            SpeakerItem::Unlabeled { channels, ch } => self.unlabeled_placement(channels, ch),
        }
    }

    /// Put `item` at `placement` (the user's own, from now on).
    pub fn set_item_placement(&mut self, item: SpeakerItem, placement: SpeakerPlacement) {
        let placement = SpeakerPlacement {
            azimuth_deg: wrap_azimuth(placement.azimuth_deg),
            elevation_deg: placement.elevation_deg.clamp(-90.0, 90.0),
            gain_db: placement
                .gain_db
                .clamp(*SPEAKER_GAIN_RANGE_DB.start(), *SPEAKER_GAIN_RANGE_DB.end()),
        };
        match item {
            SpeakerItem::Named(pos) => self.set_override(pos, Some(placement)),
            SpeakerItem::Unlabeled { channels, ch } => {
                self.set_unlabeled_placement(channels, ch, placement)
            }
        }
    }

    /// Whether `item` has been placed by the user.
    pub fn item_edited(&self, item: SpeakerItem) -> bool {
        match item {
            SpeakerItem::Named(pos) => self.has_override(pos),
            SpeakerItem::Unlabeled { channels, ch } => self
                .unlabeled
                .get(&channels)
                .and_then(|list| list.get(ch))
                .is_some_and(|p| *p != default_unlabeled_placement(channels, ch)),
        }
    }

    /// Forget the user's placement of `item`.
    pub fn reset_item(&mut self, item: SpeakerItem) {
        match item {
            SpeakerItem::Named(pos) => self.set_override(pos, None),
            SpeakerItem::Unlabeled { channels, ch } => self.set_unlabeled_placement(
                channels,
                ch,
                default_unlabeled_placement(channels, ch),
            ),
        }
    }

    pub fn set_unlabeled_placement(
        &mut self,
        channels: usize,
        ch: usize,
        placement: SpeakerPlacement,
    ) {
        let list = self.unlabeled.entry(channels).or_insert_with(|| {
            (0..channels)
                .map(|i| default_unlabeled_placement(channels, i))
                .collect()
        });
        if let Some(slot) = list.get_mut(ch) {
            *slot = placement;
        }
    }

    /// What every channel of a source with `layout` does in the ears.
    pub fn routes(&self, layout: &[Option<SpeakerPos>]) -> Vec<ChannelRoute> {
        let channels = layout.len();
        layout
            .iter()
            .enumerate()
            .map(|(ch, pos)| match pos {
                Some(SpeakerPos::Lfe) => match self.lfe {
                    LfeMode::BothEars => ChannelRoute::BothEars {
                        gain: crate::levels::db_to_amplitude(self.lfe_gain_db),
                        lowpass: self.lfe_lowpass,
                    },
                    LfeMode::Off => ChannelRoute::Silent,
                },
                Some(pos) => speaker_route(self.placement(*pos, layout)),
                None => speaker_route(self.unlabeled_placement(channels, ch)),
            })
            .collect()
    }

    /// Prefs lines for every setting.
    pub fn prefs_lines(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("hrtf_enabled={}\n", u8::from(self.enabled)));
        out.push_str(&format!("hrtf_profile={}\n", self.profile.pref_value()));
        for path in &self.user_files {
            out.push_str(&format!("hrtf_file={}\n", path.display()));
        }
        out.push_str(&format!("hrtf_room={}\n", self.room.key()));
        for (pos, placement) in SpeakerPos::ALL.iter().zip(&self.overrides) {
            if let Some(placement) = placement {
                out.push_str(&format!(
                    "hrtf_speaker={}|{}\n",
                    pos.key(),
                    placement.to_pref()
                ));
            }
        }
        for (channels, list) in &self.unlabeled {
            let joined: Vec<String> = list.iter().map(|p| p.to_pref()).collect();
            out.push_str(&format!("hrtf_unlabeled={channels}|{}\n", joined.join(",")));
        }
        out.push_str(&format!(
            "hrtf_lfe={}\n",
            match self.lfe {
                LfeMode::BothEars => "both",
                LfeMode::Off => "off",
            }
        ));
        out.push_str(&format!("hrtf_lfe_gain_db={}\n", self.lfe_gain_db));
        out.push_str(&format!(
            "hrtf_lfe_lowpass={}\n",
            u8::from(self.lfe_lowpass)
        ));
        out.push_str(&format!("hrtf_trim_db={}\n", self.trim_db));
        out.push_str(&format!(
            "hrtf_stereo_speakers={}\n",
            u8::from(self.stereo_too)
        ));
        out
    }

    /// Read one `hrtf_*=` prefs line. Returns whether it was one.
    pub fn load_prefs_line(&mut self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix("hrtf_") else {
            return false;
        };
        let Some((key, value)) = rest.split_once('=') else {
            return true;
        };
        let flag = || value.trim() == "1";
        let number = || value.trim().parse::<f32>().ok().filter(|v| v.is_finite());
        match key {
            "enabled" => self.enabled = flag(),
            "profile" => {
                if let Some(profile) = HrtfProfile::from_pref(value) {
                    self.profile = profile;
                }
            }
            "file" => {
                let path = PathBuf::from(value.trim());
                if !value.trim().is_empty() && !self.user_files.contains(&path) {
                    self.user_files.push(path);
                }
            }
            "room" => {
                if let Some(room) = RoomPreset::from_key(value) {
                    self.room = room;
                }
            }
            "speaker" => {
                if let Some((pos, placement)) = value.split_once('|') {
                    if let (Some(pos), Some(placement)) = (
                        SpeakerPos::from_key(pos.trim()),
                        SpeakerPlacement::from_pref(placement),
                    ) {
                        self.set_override(pos, Some(placement));
                    }
                }
            }
            "unlabeled" => {
                if let Some((channels, list)) = value.split_once('|') {
                    let placements: Option<Vec<SpeakerPlacement>> =
                        list.split(',').map(SpeakerPlacement::from_pref).collect();
                    if let (Ok(channels), Some(placements)) =
                        (channels.trim().parse::<usize>(), placements)
                    {
                        if placements.len() == channels {
                            self.unlabeled.insert(channels, placements);
                        }
                    }
                }
            }
            "lfe" => {
                self.lfe = if value.trim() == "off" {
                    LfeMode::Off
                } else {
                    LfeMode::BothEars
                }
            }
            "lfe_gain_db" => {
                if let Some(v) = number() {
                    self.lfe_gain_db =
                        v.clamp(*LFE_GAIN_RANGE_DB.start(), *LFE_GAIN_RANGE_DB.end());
                }
            }
            "lfe_lowpass" => self.lfe_lowpass = flag(),
            "trim_db" => {
                if let Some(v) = number() {
                    self.trim_db = v.clamp(*TRIM_RANGE_DB.start(), *TRIM_RANGE_DB.end());
                }
            }
            "stereo_speakers" => self.stereo_too = flag(),
            // Written while stereo was off unless asked for: every prefs save
            // stored that default, so it is not anybody's choice to keep.
            "stereo" => {}
            _ => {}
        }
        true
    }
}

fn speaker_route(placement: SpeakerPlacement) -> ChannelRoute {
    ChannelRoute::Speaker {
        direction: Direction::new(placement.azimuth_deg, placement.elevation_deg),
        gain: crate::levels::db_to_amplitude(placement.gain_db),
    }
}

/// Channels with no speaker, until placed: evenly round the ear-level circle,
/// the first straight ahead, going right.
pub(crate) fn default_unlabeled_placement(channels: usize, ch: usize) -> SpeakerPlacement {
    let step = 360.0 / channels.max(1) as f32;
    SpeakerPlacement::at(wrap_azimuth(ch as f32 * step), 0.0)
}

/// A loaded HRTF, resampled to one output rate.
pub(crate) struct LoadedHrtf {
    pub sofa: sofar::reader::Sofar,
    pub path: PathBuf,
    pub sample_rate: u32,
}

impl LoadedHrtf {
    pub fn describe(&self) -> String {
        format!(
            "{} directions \u{b7} {} taps \u{b7} {} Hz",
            self.sofa.num_measurements(),
            self.sofa.filter_len(),
            self.sample_rate
        )
    }
}

type LoadResult = Result<Arc<LoadedHrtf>, String>;

/// An HRTF at a rate: what is loaded, being loaded, or failed to load.
pub(crate) type HrtfKey = (HrtfProfile, u32);

pub(crate) enum HrtfLoad {
    Idle,
    Loading {
        key: HrtfKey,
        rx: Receiver<LoadResult>,
    },
    Ready {
        key: HrtfKey,
        hrtf: Arc<LoadedHrtf>,
    },
    Failed {
        key: HrtfKey,
        message: String,
    },
}

/// What the HRTF is doing right now, for the top bar and the window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HrtfStatus {
    Off,
    Loading,
    Failed(String),
    /// On, but what is playing is not for it.
    Bypassed(&'static str),
    Active {
        channels: usize,
    },
}

/// What the callback was last given, so an unchanged frame does nothing.
#[derive(Clone, Debug, PartialEq)]
struct AppliedKey {
    engine: usize,
    sample_rate: u32,
    layout: Layout,
    rev: u64,
    hrtf: usize,
}

/// The headphone feature's runtime state (not saved).
pub(crate) struct HrtfRuntime {
    pub load: HrtfLoad,
    applied: Option<AppliedKey>,
    pub status: HrtfStatus,
    /// Bumped by every settings change, so the next sync rebuilds.
    pub rev: u64,
    pub window_open: bool,
    /// The speaker the window has picked, for the side view.
    pub selected: Option<SpeakerItem>,
    /// The speaker being dragged in one of the window's views.
    pub dragging: Option<SpeakerItem>,
    /// A change made mid-drag, to save once the pointer is let go.
    pub pending_save: bool,
    pub show_all_positions: bool,
    /// The layout of the source last applied, for the window to draw.
    pub source_layout: Option<Layout>,
}

impl Default for HrtfRuntime {
    fn default() -> Self {
        Self {
            load: HrtfLoad::Idle,
            applied: None,
            status: HrtfStatus::Off,
            rev: 0,
            window_open: false,
            selected: None,
            dragging: None,
            pending_save: false,
            show_all_positions: false,
            source_layout: None,
        }
    }
}

/// Where the bundled HRTF may be. Asked on the loading worker, never on the
/// UI thread.
fn builtin_sofa_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        out.push(dir.join(BUILTIN_DIR).join(BUILTIN_SOFA_FILE));
    }
    // A development build runs from target/, where the installer never put
    // anything: take it from the repository.
    #[cfg(debug_assertions)]
    out.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/sofas/D1_HRIR_SOFA")
            .join(BUILTIN_SOFA_FILE),
    );
    out
}

/// Open `profile` resampled to `sample_rate`. Blocks: a worker's job.
fn load_hrtf(profile: &HrtfProfile, sample_rate: u32) -> LoadResult {
    let path = match profile {
        HrtfProfile::File(path) => path.clone(),
        HrtfProfile::Builtin => builtin_sofa_candidates()
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                format!("{BUILTIN_PROFILE_NAME} is not installed (looked for {BUILTIN_DIR}\\{BUILTIN_SOFA_FILE} beside NeoWaves)")
            })?,
    };
    let sofa = sofar::reader::OpenOptions::new()
        .sample_rate(sample_rate as f32)
        .open(&path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    if sofa.filter_len() == 0 {
        return Err(format!("{}: no impulse responses", path.display()));
    }
    Ok(Arc::new(LoadedHrtf {
        sofa,
        path,
        sample_rate,
    }))
}

impl WavesPreviewer {
    /// Note a settings change: the next frame rebuilds the filters, and
    /// `save` writes prefs (not while a drag is still moving).
    pub(super) fn hrtf_settings_changed(&mut self, save: bool) {
        self.hrtf_runtime.rev = self.hrtf_runtime.rev.wrapping_add(1);
        if save {
            self.save_prefs();
        }
    }

    pub(super) fn set_hrtf_enabled(&mut self, enabled: bool) {
        if self.hrtf.enabled != enabled {
            self.hrtf.enabled = enabled;
            self.hrtf_settings_changed(true);
        }
    }

    pub(super) fn set_hrtf_profile(&mut self, profile: HrtfProfile) {
        if self.hrtf.profile != profile {
            self.hrtf.profile = profile;
            self.hrtf_settings_changed(true);
        }
    }

    /// Add a SOFA file to the profiles and switch to it.
    pub(super) fn add_hrtf_file(&mut self, path: PathBuf) {
        if !self.hrtf.user_files.contains(&path) {
            self.hrtf.user_files.push(path.clone());
        }
        self.hrtf.profile = HrtfProfile::File(path);
        self.hrtf_settings_changed(true);
    }

    /// Take a SOFA file off the list (the file stays where it is).
    pub(super) fn remove_hrtf_file(&mut self, path: &Path) {
        self.hrtf.user_files.retain(|p| p != path);
        if self.hrtf.profile == HrtfProfile::File(path.to_path_buf()) {
            self.hrtf.profile = HrtfProfile::Builtin;
        }
        self.hrtf_settings_changed(true);
    }

    /// Ask for a SOFA file and use it.
    pub(super) fn pick_and_add_hrtf_file(&mut self) {
        if let Some(path) = self.pick_sofa_dialog() {
            self.add_hrtf_file(path);
        }
    }

    pub(super) fn open_hrtf_window(&mut self) {
        self.hrtf_runtime.window_open = true;
    }

    /// The loaded HRTF, if the one in use is loaded at the output's rate.
    pub(super) fn hrtf_loaded(&self) -> Option<&Arc<LoadedHrtf>> {
        match &self.hrtf_runtime.load {
            HrtfLoad::Ready { hrtf, .. } => Some(hrtf),
            _ => None,
        }
    }

    fn start_hrtf_load(&mut self, key: HrtfKey) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (profile, sample_rate) = key.clone();
        self.debug_log(format!(
            "hrtf: loading {} at {sample_rate} Hz",
            profile.label()
        ));
        std::thread::Builder::new()
            .name("hrtf-load".to_string())
            .spawn(move || {
                let _ = tx.send(load_hrtf(&profile, sample_rate));
                crate::ui_wake::wake_ui();
            })
            .ok();
        self.hrtf_runtime.load = HrtfLoad::Loading { key, rx };
    }

    fn poll_hrtf_load(&mut self) {
        let HrtfLoad::Loading { key, rx } = &self.hrtf_runtime.load else {
            return;
        };
        let next = match poll_job(rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(Ok(hrtf)) => HrtfLoad::Ready {
                key: key.clone(),
                hrtf,
            },
            JobPoll::Ready(Err(message)) => HrtfLoad::Failed {
                key: key.clone(),
                message,
            },
            JobPoll::Gone => HrtfLoad::Failed {
                key: key.clone(),
                message: "the loading thread ended without an answer".to_string(),
            },
        };
        match &next {
            HrtfLoad::Ready { hrtf, .. } => {
                let line = format!("hrtf: loaded {} ({})", hrtf.path.display(), hrtf.describe());
                self.debug_log(line);
            }
            HrtfLoad::Failed { message, .. } => {
                let line = format!("hrtf: failed: {message}");
                self.debug_log(line);
            }
            _ => {}
        }
        self.hrtf_runtime.load = next;
    }

    /// The source the window describes: what is playing, else the active
    /// editor tab. Its path (when it is a file) and its layout.
    pub(super) fn hrtf_window_source(&self) -> Option<(Option<PathBuf>, Layout)> {
        let playing_path = match &self.playback_session.source {
            super::PlaybackSourceKind::ListPreview(path)
            | super::PlaybackSourceKind::EditorTab(path) => Some(path.clone()),
            _ => None,
        };
        let playing_channels = self.audio.source_channels().unwrap_or(0);
        if playing_channels > 0 && self.audio.has_audio_source() {
            return Some((playing_path, self.hrtf_source_layout(playing_channels)));
        }
        let tab = self.active_tab.and_then(|idx| self.tabs.get(idx))?;
        let channels = tab.ch_samples.len();
        if channels == 0 {
            return None;
        }
        let layout = self
            .channel_layout_for(&tab.path, channels)
            .or_else(|| standard_layout_vec(channels))
            .unwrap_or_else(|| vec![None; channels]);
        Some((Some(tab.path.clone()), layout))
    }

    /// The layout of what is playing, as the binaural path should see it.
    fn hrtf_source_layout(&self, channels: usize) -> Layout {
        self.playing_source_layout(channels)
            .or_else(|| standard_layout_vec(channels))
            .unwrap_or_else(|| vec![None; channels])
    }

    fn uninstall_binaural(&mut self, status: HrtfStatus) {
        if self.hrtf_runtime.applied.take().is_some() || self.audio.binaural_filters().is_some() {
            self.audio.set_binaural(None);
        }
        self.hrtf_runtime.status = status;
    }

    /// Give the audio callback the headphone filters for what is playing.
    /// Cheap when nothing moved: it rebuilds only when the engine, the
    /// output rate, the source's layout, the HRTF or a setting changed.
    pub(super) fn sync_binaural(&mut self) {
        self.poll_hrtf_load();
        if !self.hrtf.enabled {
            self.uninstall_binaural(HrtfStatus::Off);
            return;
        }
        let sample_rate = self.audio.shared.out_sample_rate.max(1);
        let key: HrtfKey = (self.hrtf.profile.clone(), sample_rate);
        let hrtf = match &self.hrtf_runtime.load {
            HrtfLoad::Ready { key: loaded, hrtf } if *loaded == key => Arc::clone(hrtf),
            HrtfLoad::Loading { key: loading, .. } if *loading == key => {
                self.uninstall_binaural(HrtfStatus::Loading);
                return;
            }
            HrtfLoad::Failed {
                key: failed,
                message,
            } if *failed == key => {
                let status = HrtfStatus::Failed(message.clone());
                self.uninstall_binaural(status);
                return;
            }
            _ => {
                self.start_hrtf_load(key);
                self.uninstall_binaural(HrtfStatus::Loading);
                return;
            }
        };
        let channels = self.audio.source_channels().unwrap_or(0);
        if channels == 0 {
            self.uninstall_binaural(HrtfStatus::Bypassed("nothing is playing"));
            return;
        }
        if channels < 2 {
            self.uninstall_binaural(HrtfStatus::Bypassed("mono source"));
            return;
        }
        if channels == 2 && !self.hrtf.stereo_too {
            self.uninstall_binaural(HrtfStatus::Bypassed("stereo plays directly"));
            return;
        }
        let layout = self.hrtf_source_layout(channels);
        let applied = AppliedKey {
            engine: Arc::as_ptr(&self.audio.shared) as usize,
            sample_rate,
            layout: layout.clone(),
            rev: self.hrtf_runtime.rev,
            hrtf: Arc::as_ptr(&hrtf) as usize,
        };
        if self.hrtf_runtime.applied.as_ref() == Some(&applied) {
            return;
        }
        let routes = self.hrtf.routes(&layout);
        let started = std::time::Instant::now();
        let filters = BinauralFilters::build(
            &hrtf.sofa,
            &routes,
            sample_rate,
            crate::levels::db_to_amplitude(self.hrtf.trim_db),
        );
        self.audio.set_binaural(Some(Arc::new(filters)));
        let took = started.elapsed().as_millis();
        if took > SLOW_FILTER_BUILD_MS {
            self.debug_log(format!(
                "hrtf: built filters for {channels} channels in {took} ms"
            ));
        }
        self.hrtf_runtime.applied = Some(applied);
        self.hrtf_runtime.source_layout = Some(layout);
        self.hrtf_runtime.status = HrtfStatus::Active { channels };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SpeakerPos::*;

    fn layout_714() -> Layout {
        standard_layout_vec(12).expect("7.1.4")
    }

    #[test]
    fn a_speaker_stands_where_the_room_puts_it_unless_placed() {
        let mut settings = HrtfSettings::default();
        let layout = layout_714();
        assert_eq!(
            settings.placement(Bl, &layout),
            SpeakerPlacement::at(-135.0, 0.0)
        );
        settings.room = RoomPreset::Quad;
        assert_eq!(
            settings.placement(Fl, &layout),
            SpeakerPlacement::at(-45.0, 0.0)
        );
        settings.room = RoomPreset::Auro3d;
        assert_eq!(
            settings.placement(Tbl, &layout),
            SpeakerPlacement::at(-110.0, 30.0)
        );
        let own = SpeakerPlacement {
            azimuth_deg: -20.0,
            elevation_deg: -15.0,
            gain_db: -3.0,
        };
        settings.set_override(Tbl, Some(own));
        assert_eq!(
            settings.placement(Tbl, &layout),
            own,
            "a lower layer is allowed"
        );
    }

    #[test]
    fn the_lfe_goes_to_both_ears_and_unlabeled_channels_spread_round() {
        let mut settings = HrtfSettings::default();
        let routes = settings.routes(&standard_layout_vec(6).unwrap());
        assert_eq!(
            routes[3],
            ChannelRoute::BothEars {
                gain: 1.0,
                lowpass: false
            }
        );
        settings.lfe = LfeMode::Off;
        assert_eq!(
            settings.routes(&standard_layout_vec(6).unwrap())[3],
            ChannelRoute::Silent
        );
        // Four channels with no speaker: ahead, right, behind, left.
        let azimuths: Vec<f32> = (0..4)
            .map(|ch| settings.unlabeled_placement(4, ch).azimuth_deg)
            .collect();
        assert_eq!(azimuths, vec![0.0, 90.0, 180.0, -90.0]);
        settings.set_unlabeled_placement(4, 2, SpeakerPlacement::at(170.0, -30.0));
        assert_eq!(
            settings.unlabeled_placement(4, 2),
            SpeakerPlacement::at(170.0, -30.0)
        );
        assert_eq!(settings.unlabeled_placement(4, 1).azimuth_deg, 90.0);
    }

    #[test]
    fn stereo_plays_from_the_rooms_l_and_r_speakers_by_default() {
        let settings = HrtfSettings::default();
        assert!(settings.stereo_too);
        let stereo = [Some(Fl), Some(Fr)];
        assert_eq!(settings.placement(Fl, &stereo), SpeakerPlacement::at(-30.0, 0.0));
        assert_eq!(settings.placement(Fr, &stereo), SpeakerPlacement::at(30.0, 0.0));
        // 2.1: the pair, and the LFE in both ears.
        let routes = settings.routes(&[Some(Fl), Some(Fr), Some(Lfe)]);
        assert!(matches!(routes[2], ChannelRoute::BothEars { .. }));
        // The key that stored the old default is not read back as a choice.
        let mut read = HrtfSettings::default();
        assert!(read.load_prefs_line("hrtf_stereo=0"));
        assert!(read.stereo_too);
        assert!(read.load_prefs_line("hrtf_stereo_speakers=0"));
        assert!(!read.stereo_too);
    }

    #[test]
    fn settings_round_trip_through_prefs() {
        let mut settings = HrtfSettings {
            enabled: true,
            profile: HrtfProfile::File(PathBuf::from(r"C:\hrtf\my head.sofa")),
            user_files: vec![PathBuf::from(r"C:\hrtf\my head.sofa")],
            room: RoomPreset::Cube,
            lfe: LfeMode::Off,
            lfe_gain_db: 6.0,
            lfe_lowpass: true,
            trim_db: -4.5,
            stereo_too: false,
            ..HrtfSettings::default()
        };
        settings.set_override(Tc, Some(SpeakerPlacement::at(10.0, 80.0)));
        settings.set_unlabeled_placement(3, 1, SpeakerPlacement::at(-60.0, -20.0));
        let text = settings.prefs_lines();
        let mut read = HrtfSettings::default();
        for line in text.lines() {
            assert!(read.load_prefs_line(line), "{line}");
        }
        assert_eq!(read, settings);
        assert!(!read.load_prefs_line("theme=dark"));
    }

    #[test]
    fn azimuths_wrap_into_one_turn() {
        assert_eq!(wrap_azimuth(190.0), -170.0);
        assert_eq!(wrap_azimuth(-180.0), 180.0);
        assert_eq!(wrap_azimuth(540.0), 180.0);
        assert_eq!(wrap_azimuth(-30.0), -30.0);
    }
}
