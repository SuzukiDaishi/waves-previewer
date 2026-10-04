//! Which speaker each channel of a file feeds: its *channel layout*, shared
//! by the mini meter (labels, the SURROUND view) and playback (the mix
//! matrix's Auto mode). See `docs/MULTICHANNEL_SPEC.md`.
//!
//! A file's layout is, first match wins:
//! 1. the user's choice for that file (stored in the session),
//! 2. the channel mask in its WAVE header,
//! 3. the user's default for its channel count (stored in prefs),
//! 4. the standard WAVE order for its channel count.

use std::path::{Path, PathBuf};

use crate::audio_channels::{layout_from_mask, standard_layout_vec, Layout, SpeakerPos, PRESETS};

use super::WavesPreviewer;

/// Where a file's layout came from, for the layout editor to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutOrigin {
    /// The user chose it for this file (session).
    File,
    /// The file's WAVE channel mask.
    Mask,
    /// The user's default for this channel count (prefs).
    Default,
    /// The standard WAVE order for this channel count.
    Standard,
}

impl LayoutOrigin {
    pub fn describe(self) -> &'static str {
        match self {
            LayoutOrigin::File => "chosen for this file",
            LayoutOrigin::Mask => "from the file's channel mask",
            LayoutOrigin::Default => "your default for this channel count",
            LayoutOrigin::Standard => "standard WAV order",
        }
    }
}

/// Which speakers an output device feeds: a preset's speakers on
/// consecutive outputs from `first` (counting from 1); the rest feed none.
/// Chosen per device, for an interface whose outputs are wired to the room
/// in an order of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputSpeakers {
    pub preset: String,
    pub first: usize,
}

/// The layout editor window's state: the file and the layout being edited.
#[derive(Clone, Debug)]
pub(crate) struct ChannelLayoutEditor {
    pub path: PathBuf,
    pub draft: Layout,
}

/// A layout as stored in prefs and sessions: speaker keys, comma-separated,
/// `-` for a channel that feeds no speaker.
pub(crate) fn layout_to_string(layout: &[Option<SpeakerPos>]) -> String {
    layout
        .iter()
        .map(|pos| pos.map_or("-", SpeakerPos::key))
        .collect::<Vec<_>>()
        .join(",")
}

/// What to call a layout: the preset it is, else its channel count.
pub(crate) fn layout_name(layout: &[Option<SpeakerPos>]) -> String {
    crate::audio_channels::PRESETS
        .iter()
        .find(|preset| {
            preset.speakers.len() == layout.len()
                && preset.speakers.iter().zip(layout).all(|(a, b)| Some(*a) == *b)
        })
        .map(|preset| preset.name.to_string())
        .unwrap_or_else(|| format!("{} ch (custom)", layout.len()))
}

/// Inverse of [`layout_to_string`]. `None` for an unknown speaker name.
pub(crate) fn layout_from_string(text: &str) -> Option<Layout> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.split(',')
        .map(|key| match key.trim() {
            "-" | "" => Some(None),
            key => SpeakerPos::from_key(key).map(Some),
        })
        .collect()
}

/// The speakers of a device with `channels` outputs: `choice` when it fits
/// the device, else the standard order for the count, else 7.1.4 on the
/// first twelve outputs of a device wider than that. See
/// `WavesPreviewer::output_layout_for`.
pub(crate) fn output_layout(choice: Option<&OutputSpeakers>, channels: usize) -> Option<Layout> {
    if let Some(choice) = choice {
        if let Some(preset) = PRESETS.iter().find(|preset| preset.name == choice.preset) {
            let first = choice.first.max(1) - 1;
            if first + preset.speakers.len() <= channels {
                let mut layout = vec![None; channels];
                for (i, pos) in preset.speakers.iter().enumerate() {
                    layout[first + i] = Some(*pos);
                }
                return Some(layout);
            }
        }
    }
    standard_layout_vec(channels).or_else(|| {
        let bed = standard_layout_vec(12)?;
        (channels > bed.len()).then(|| {
            let mut layout = bed;
            layout.resize(channels, None);
            layout
        })
    })
}

impl WavesPreviewer {
    /// The layout of `path` with `channels` channels, and where it came
    /// from. `None` for a channel count with no layout to offer (9, 11, 13+
    /// with nothing chosen), which plays by channel index.
    pub(super) fn channel_layout_resolved(
        &self,
        path: &Path,
        channels: usize,
    ) -> Option<(Layout, LayoutOrigin)> {
        if channels == 0 {
            return None;
        }
        if let Some(layout) = self
            .channel_layout_overrides
            .get(path)
            .filter(|layout| layout.len() == channels)
        {
            return Some((layout.clone(), LayoutOrigin::File));
        }
        if let Some(layout) = self
            .channel_mask_for(path)
            .and_then(|mask| layout_from_mask(mask, channels))
        {
            return Some((layout.into_iter().map(Some).collect(), LayoutOrigin::Mask));
        }
        if let Some(layout) = self.channel_layout_defaults.get(&channels) {
            return Some((layout.clone(), LayoutOrigin::Default));
        }
        standard_layout_vec(channels).map(|layout| (layout, LayoutOrigin::Standard))
    }

    pub(super) fn channel_layout_for(&self, path: &Path, channels: usize) -> Option<Layout> {
        self.channel_layout_resolved(path, channels)
            .map(|(layout, _)| layout)
    }

    fn channel_mask_for(&self, path: &Path) -> Option<u32> {
        self.item_for_path(path)?.meta.as_ref()?.channel_mask
    }

    /// Choose `path`'s layout (`None` returns it to its mask, default or
    /// the standard). Stored in the session on its next save.
    pub(super) fn set_channel_layout_override(&mut self, path: &Path, layout: Option<Layout>) {
        match layout {
            Some(layout) => {
                self.channel_layout_overrides
                    .insert(path.to_path_buf(), layout);
            }
            None => {
                self.channel_layout_overrides.remove(path);
            }
        }
        self.channel_layout_rev = self.channel_layout_rev.wrapping_add(1);
    }

    /// Make `layout` the default for its channel count, and save prefs.
    pub(super) fn set_channel_layout_default(&mut self, layout: Layout) {
        self.channel_layout_defaults.insert(layout.len(), layout);
        self.channel_layout_rev = self.channel_layout_rev.wrapping_add(1);
        self.save_prefs();
    }

    /// Tell the audio callback which speaker each channel of what is
    /// playing feeds. Cheap when nothing moved: it only resolves again when
    /// the source, its channel count, its channel mask (metadata can land
    /// after playback starts) or a layout setting changes.
    pub(super) fn sync_playback_channel_layout(&mut self) {
        // A replaced engine starts with no layouts: its shared state is new.
        let engine = std::sync::Arc::as_ptr(&self.audio.shared) as usize;
        let device = self.audio.output_device_name().map(str::to_string);
        let out_channels = self.audio.output_channels();
        let out_unchanged = self.output_layout_applied.as_ref().is_some_and(|applied| {
            applied.0 == engine
                && applied.1 == device
                && applied.2 == out_channels
                && applied.3 == self.channel_layout_rev
        });
        if !out_unchanged {
            let layout = self.output_layout_for(device.as_deref(), out_channels);
            self.audio.set_output_layout(layout);
            self.output_layout_applied =
                Some((engine, device, out_channels, self.channel_layout_rev));
        }
        let path = match &self.playback_session.source {
            super::PlaybackSourceKind::ListPreview(path)
            | super::PlaybackSourceKind::EditorTab(path) => Some(path),
            _ => None,
        };
        // A timeline's mix is in its output format, which can change while
        // it plays without the channel count changing (5.1 to Film 5.1).
        let timeline = match &self.playback_session.source {
            super::PlaybackSourceKind::MultiEdit(id) => {
                self.multi_edit.doc(id).map(|doc| doc.layout.as_str())
            }
            _ => None,
        };
        let channels = self.audio.source_channels().unwrap_or(0);
        let mask = path.and_then(|path| self.channel_mask_for(path));
        let unchanged = out_unchanged
            && self.channel_layout_applied.as_ref().is_some_and(|applied| {
                applied.0.as_ref() == path
                    && applied.1.as_deref() == timeline
                    && applied.2 == channels
                    && applied.3 == mask
                    && applied.4 == self.channel_layout_rev
            });
        if unchanged {
            return;
        }
        let (path, timeline) = (path.cloned(), timeline.map(str::to_string));
        let layout = (channels > 0)
            .then(|| self.playing_source_layout(channels))
            .flatten();
        self.audio.set_source_layout(layout);
        self.channel_layout_applied =
            Some((path, timeline, channels, mask, self.channel_layout_rev));
    }

    /// Which speaker each channel of what is playing feeds, when something
    /// says: a file's layout, or a timeline's output format.
    pub(super) fn playing_source_layout(&self, channels: usize) -> Option<Layout> {
        match &self.playback_session.source {
            super::PlaybackSourceKind::ListPreview(path)
            | super::PlaybackSourceKind::EditorTab(path) => self.channel_layout_for(path, channels),
            super::PlaybackSourceKind::MultiEdit(id) => self
                .multi_edit
                .doc(id)
                .map(|doc| doc.output_layout())
                .filter(|layout| layout.len() == channels),
            _ => None,
        }
    }

    /// Which speaker each output of a device with `channels` outputs feeds:
    /// the user's choice for that device; else the standard order for its
    /// channel count; else, for a device wider than any standard layout
    /// (a 16- or 64-channel interface), 7.1.4 on its first twelve outputs
    /// -- spreading the clip over all of them by index is what it used to do.
    pub(super) fn output_layout_for(
        &self,
        device: Option<&str>,
        channels: usize,
    ) -> Option<Layout> {
        output_layout(
            device.and_then(|device| self.output_speakers.get(device)),
            channels,
        )
    }

    /// Choose the speakers of the open output device (`None`: back to the
    /// standard for its channel count). Saved in prefs.
    pub(super) fn set_output_speakers(&mut self, choice: Option<OutputSpeakers>) {
        let Some(device) = self.audio.output_device_name().map(str::to_string) else {
            return;
        };
        match choice {
            Some(choice) => {
                self.output_speakers.insert(device, choice);
            }
            None => {
                self.output_speakers.remove(&device);
            }
        }
        self.channel_layout_rev = self.channel_layout_rev.wrapping_add(1);
        self.save_prefs();
    }

    /// Prefs lines for the per-channel-count defaults and the output
    /// speakers.
    pub(super) fn channel_layout_prefs_lines(&self) -> String {
        let mut out: String = self
            .channel_layout_defaults
            .iter()
            .map(|(count, layout)| format!("channel_layout_{count}={}\n", layout_to_string(layout)))
            .collect();
        for (device, choice) in &self.output_speakers {
            out.push_str(&format!(
                "output_speakers={}|{}|{}\n",
                choice.preset,
                choice.first,
                device.replace('\n', " ")
            ));
        }
        out
    }

    /// Read one `channel_layout_<count>=...` prefs line. Returns whether it
    /// was one.
    pub(super) fn load_channel_layout_prefs_line(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("output_speakers=") {
            let mut parts = rest.splitn(3, '|');
            if let (Some(preset), Some(first), Some(device)) =
                (parts.next(), parts.next(), parts.next())
            {
                if let Ok(first) = first.trim().parse::<usize>() {
                    self.output_speakers.insert(
                        device.to_string(),
                        OutputSpeakers {
                            preset: preset.to_string(),
                            first,
                        },
                    );
                }
            }
            return true;
        }
        let Some(rest) = line.strip_prefix("channel_layout_") else {
            return false;
        };
        let Some((count, value)) = rest.split_once('=') else {
            return true;
        };
        if let (Ok(count), Some(layout)) =
            (count.trim().parse::<usize>(), layout_from_string(value))
        {
            if count == layout.len() {
                self.channel_layout_defaults.insert(count, layout);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_feeds_the_standard_speakers_unless_told_otherwise() {
        assert_eq!(output_layout(None, 2), standard_layout_vec(2));
        assert_eq!(output_layout(None, 6), standard_layout_vec(6));
        assert_eq!(output_layout(None, 9), None, "no layout for nine outputs");
        // A 64-channel card: 7.1.4 on the first twelve, the rest silent.
        let wide = output_layout(None, 64).expect("a layout");
        assert_eq!(&wide[..12], &standard_layout_vec(12).expect("7.1.4")[..]);
        assert!(wide[12..].iter().all(Option::is_none));
        // 5.1 Film order from output 5.
        let choice = OutputSpeakers {
            preset: "5.1 Film / Pro Tools".to_string(),
            first: 5,
        };
        let film = output_layout(Some(&choice), 16).expect("a layout");
        assert!(film[..4].iter().all(Option::is_none));
        assert_eq!(film[4], Some(SpeakerPos::Fl));
        assert_eq!(film[5], Some(SpeakerPos::Fc));
        assert_eq!(film[9], Some(SpeakerPos::Lfe));
        // A choice the device is too small for is not used.
        assert_eq!(output_layout(Some(&choice), 8), standard_layout_vec(8));
    }

    #[test]
    fn a_layout_round_trips_through_its_stored_form() {
        let layout: Layout = vec![Some(SpeakerPos::Fl), None, Some(SpeakerPos::Tbr)];
        let text = layout_to_string(&layout);
        assert_eq!(text, "FL,-,TBR");
        assert_eq!(layout_from_string(&text), Some(layout));
        assert_eq!(layout_from_string("FL,XX"), None, "an unknown speaker");
        assert_eq!(layout_from_string(""), None);
    }
}
