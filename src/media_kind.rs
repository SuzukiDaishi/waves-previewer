//! What kind of media a path holds, and what the app is allowed to do with it.
//!
//! Video files ride the same list, decode and playback machinery as audio files
//! — only their audio track is ever played. What separates them is that NeoWaves
//! has no video encoder, so it cannot write one back out: a destructive edit or
//! an export would either lose the picture or produce a file the user did not
//! ask for. Rather than spread `ext == "mp4"` tests across the ~20 places that
//! gate editing and export, every such decision reads one of the capability
//! predicates below. Lifting the restriction later (extracting the audio to a
//! virtual item, say, and letting that be edited) is then a change to this file
//! rather than an archaeology exercise.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
}

impl MediaKind {
    pub fn is_video(self) -> bool {
        matches!(self, MediaKind::Video)
    }
}

/// True for the extensions listed in [`crate::audio_io::SUPPORTED_VIDEO_EXTS`].
pub fn is_video_extension(ext: &str) -> bool {
    crate::audio_io::SUPPORTED_VIDEO_EXTS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(is_video_extension)
        .unwrap_or(false)
}

pub fn media_kind_for_path(path: &Path) -> MediaKind {
    if is_video_path(path) {
        MediaKind::Video
    } else {
        MediaKind::Audio
    }
}

/// Whether the editor may change this source's samples.
///
/// Video is read-only for now. When that changes, this is the predicate to
/// relax — the editor's tool panel, its destructive shortcuts, the post-frame
/// command dispatch and the list's per-file gain all consult it.
pub fn source_allows_destructive_edit(path: &Path) -> bool {
    !is_video_path(path)
}

/// Whether this source can be written back out as an audio file.
///
/// Blocked for video for the same reason as editing: the export pipeline picks
/// its output format from the source extension when nothing else says
/// otherwise, and there is no encoder that could produce an `.mp4` back.
pub fn source_allows_export(path: &Path) -> bool {
    !is_video_path(path)
}

/// Whether marker / loop metadata may be written *into* this file.
///
/// False for video: markers and loop regions on a video still work, they just
/// land in the `<stem>.markers.json` / `<stem>.loop.json` sidecars the app
/// already uses for formats with no native representation. The video file
/// itself is never rewritten.
pub fn source_allows_metadata_write(path: &Path) -> bool {
    !is_video_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn video_extensions_are_recognized_case_insensitively() {
        for name in ["clip.mp4", "clip.MOV", "clip.M4v", "clip.3gp", "clip.3g2"] {
            let path = PathBuf::from(name);
            assert!(is_video_path(&path), "{name} should be video");
            assert_eq!(media_kind_for_path(&path), MediaKind::Video);
        }
    }

    #[test]
    fn audio_extensions_are_not_video() {
        for name in ["a.wav", "a.mp3", "a.m4a", "a.flac", "a.ogg", "a.aiff", "a"] {
            let path = PathBuf::from(name);
            assert!(!is_video_path(&path), "{name} should not be video");
            assert_eq!(media_kind_for_path(&path), MediaKind::Audio);
        }
    }

    #[test]
    fn video_sources_are_read_only_and_audio_sources_are_not() {
        let video = PathBuf::from("clip.mp4");
        let audio = PathBuf::from("clip.m4a");
        assert!(!source_allows_destructive_edit(&video));
        assert!(!source_allows_export(&video));
        assert!(!source_allows_metadata_write(&video));
        assert!(source_allows_destructive_edit(&audio));
        assert!(source_allows_export(&audio));
        assert!(source_allows_metadata_write(&audio));
    }
}
