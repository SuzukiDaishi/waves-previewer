//! Shared-file primitives for session documents.
//!
//! A `.nwsess` on a file server has more than one writer: two GUI instances,
//! or a GUI and a `--cli` batch. Nothing here coordinates them up front --
//! there is deliberately no lock file (see `docs/NWPROJ_PLAN.md`) -- so the
//! protection is optimistic: remember exactly what was read, and refuse to
//! commit over a document that changed underneath.
//!
//! The comparison key is a hash of the file's bytes rather than its mtime.
//! On a share the mtime comes from the *server's* clock, at the server's
//! resolution, filtered through the client's attribute cache; two machines
//! cannot agree on it. The bytes they can agree on.
//!
//! Everything in this module blocks on the filesystem, so all of it belongs
//! on a worker thread -- never call it from the UI thread.

use std::io;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// How long to wait before each retry of a share operation that failed for a
/// reason that usually clears on its own.
const RETRY_DELAYS_MS: [u64; 3] = [100, 300, 900];

/// True when an I/O error is the kind a network share produces transiently:
/// another client (or the virus scanner) holding the file open for the
/// moment, or an SMB session that dropped and is about to reconnect. These
/// are worth retrying; a missing file or a full disk is not.
pub(crate) fn is_transient_share_error(err: &io::Error) -> bool {
    if matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
    ) {
        return true;
    }
    match err.raw_os_error() {
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION: someone else has
        // the file open. ERROR_UNEXP_NET_ERR / ERROR_NETNAME_DELETED: the
        // SMB session dropped and the redirector is reconnecting.
        #[cfg(windows)]
        Some(32) | Some(33) | Some(59) | Some(64) => true,
        _ => false,
    }
}

pub(crate) fn retry_shared_io_with<T>(
    delays: &[u64],
    mut op: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut attempt = 0usize;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= delays.len() || !is_transient_share_error(&err) {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(delays[attempt]));
                attempt += 1;
            }
        }
    }
}

/// Run a filesystem operation against a shared path, retrying the transient
/// failures a share produces. Without this a single sharing violation --
/// which on a busy share is a routine event, not a fault -- loses a save.
pub(crate) fn retry_shared_io<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_shared_io_with(&RETRY_DELAYS_MS, op)
}

/// Replace `destination` with `source` in one step, so a reader on another
/// machine sees either the whole old document or the whole new one and never
/// a half-written file.
#[cfg(windows)]
pub(crate) fn atomic_replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    retry_shared_io(|| {
        let ok = unsafe {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })
}

#[cfg(not(windows))]
pub(crate) fn atomic_replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    retry_shared_io(|| std::fs::rename(source, destination))
}

/// Identifies the exact bytes a session was read from. Two readers that saw
/// the same document produce the same fingerprint on any machine.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionFingerprint {
    pub len: u64,
    pub sha256: [u8; 32],
}

impl std::fmt::Debug for SessionFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SessionFingerprint({}, {} bytes)", self.short_hex(), self.len)
    }
}

impl SessionFingerprint {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&digest);
        Self {
            len: bytes.len() as u64,
            sha256,
        }
    }

    /// Short form for debug logs. Never used for comparison.
    pub fn short_hex(&self) -> String {
        self.sha256[..6].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The identity/version header of a session document, parsed on its own.
///
/// Deserialized from a struct that carries nothing but these fields, so it
/// succeeds on any valid session TOML -- including one written by a newer
/// build with fields this one has never seen, and one whose *body* this
/// build would reject. That matters: the whole point is to name who saved
/// the document we are about to refuse to overwrite.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct SessionStamp {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
    #[serde(default)]
    pub saved_at: Option<String>,
    #[serde(default)]
    pub saved_by: Option<String>,
}

impl SessionStamp {
    /// Best effort: an unparseable document still yields an empty stamp
    /// rather than an error, because the caller only wants it to describe a
    /// conflict it has already detected by hash.
    pub fn parse(text: &str) -> Self {
        toml::from_str(text).unwrap_or_default()
    }

    pub fn parse_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => Self::parse(text),
            Err(_) => Self::default(),
        }
    }

    /// "revision 43, saved by tanaka, 2026-08-31 14:32" -- as much of that as
    /// the document actually carries.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        match self.revision {
            Some(rev) => parts.push(format!("revision {rev}")),
            None => parts.push("unknown revision".to_string()),
        }
        if let Some(by) = self.saved_by.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(format!("saved by {by}"));
        }
        if let Some(at) = self.saved_at.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(at.to_string());
        }
        parts.join(", ")
    }
}

/// What is at a session path right now.
pub(crate) enum SessionDiskState {
    /// Nothing there. Writing is safe; the caller decides whether a document
    /// that was expected to exist going missing is worth reporting.
    Missing,
    Present {
        fingerprint: SessionFingerprint,
        /// Kept so a deliberate overwrite can back up exactly what it
        /// replaces without a second read -- which on a share could pick up
        /// a *third* writer's document instead of the one we checked.
        bytes: Vec<u8>,
    },
}

impl SessionDiskState {
    pub fn fingerprint(&self) -> Option<SessionFingerprint> {
        match self {
            Self::Missing => None,
            Self::Present { fingerprint, .. } => Some(*fingerprint),
        }
    }

    /// Parsed on demand. Deserializing it walks the whole TOML, which on a
    /// large session is real work, and the common case -- a save nobody
    /// disturbed -- never needs to know who saved the previous version.
    pub fn stamp(&self) -> SessionStamp {
        match self {
            Self::Missing => SessionStamp::default(),
            Self::Present { bytes, .. } => SessionStamp::parse_bytes(bytes),
        }
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present { bytes, .. } => Some(bytes.as_slice()),
        }
    }
}

/// Read a session's bytes, retrying the transient failures a share produces.
/// `Ok(None)` means the file is not there, which is a normal answer here and
/// not an error.
pub(crate) fn read_session_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match retry_shared_io(|| std::fs::read(path)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Fingerprint and stamp whatever is at `path` now.
pub(crate) fn read_session_state(path: &Path) -> io::Result<SessionDiskState> {
    let Some(bytes) = read_session_bytes(path)? else {
        return Ok(SessionDiskState::Missing);
    };
    let fingerprint = SessionFingerprint::of_bytes(&bytes);
    Ok(SessionDiskState::Present { fingerprint, bytes })
}

/// The name written into `saved_by`, so a colleague reading the conflict
/// message knows whose save they are about to replace.
///
/// This lands in a file the team already shares with each other and goes
/// nowhere else.
pub(crate) fn local_display_name() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match (user, host) {
        (Some(user), Some(host)) => format!("{user}@{host}"),
        (Some(user), None) => user,
        (None, Some(host)) => host,
        (None, None) => "unknown".to_string(),
    }
}

/// Name a sidecar by what is in it, not by its position in the list.
///
/// The old scheme was `data/tab_0000.wav` -- an index. Two people editing the
/// same shared session both wrote *different audio* to that one name, so the
/// second save destroyed the first person's take even when the document-level
/// conflict check later refused their document. Content addressing removes
/// the collision at the source, and identical audio deduplicates for free.
pub(crate) fn hash_audio_content(channels: &[Vec<f32>], sample_rate: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sample_rate.to_le_bytes());
    hasher.update((channels.len() as u64).to_le_bytes());
    for channel in channels {
        hasher.update((channel.len() as u64).to_le_bytes());
        // `f32` has no stable `to_le_bytes` over a slice, and transmuting the
        // buffer would make the name endian-dependent -- a session written on
        // one machine has to resolve on another.
        for sample in channel {
            hasher.update(sample.to_le_bytes());
        }
    }
    digest_name(hasher)
}

/// Same, for a sidecar sourced from a file already on disk.
pub(crate) fn hash_file_content(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut file = retry_shared_io(|| std::fs::File::open(path))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(digest_name(hasher))
}

/// 128 bits of the digest as hex: short enough for a readable filename, wide
/// enough that a collision -- which would silently serve one person's audio
/// in place of another's -- is not a thing that happens.
fn digest_name(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh document lineage id. 128 random bits as hex -- enough that two
/// people creating a session at the same moment on different machines never
/// collide, with no new dependency (there is no `uuid` in the graph).
pub(crate) fn new_session_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// RFC3339 in UTC, so two machines in different time zones stamp comparably.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_bytes_fingerprint_the_same_on_any_machine() {
        let a = SessionFingerprint::of_bytes(b"version = 2\n");
        let b = SessionFingerprint::of_bytes(b"version = 2\n");
        assert_eq!(a, b);
        assert_eq!(a.len, 12);
    }

    #[test]
    fn one_changed_byte_changes_the_fingerprint() {
        let a = SessionFingerprint::of_bytes(b"revision = 41\n");
        let b = SessionFingerprint::of_bytes(b"revision = 42\n");
        assert_ne!(a, b);
    }

    #[test]
    fn a_stamp_parses_out_of_a_document_this_build_cannot_otherwise_read() {
        // A newer build's document: unknown top-level keys, an unknown table,
        // and a `version` this build's own reader would reject. The stamp
        // still has to come out, because it names who we are conflicting with.
        let text = r#"
version = 99
session_id = "ab12"
revision = 43
saved_at = "2026-08-31T14:32:00Z"
saved_by = "tanaka"
unknown_scalar = 7

[future_table]
whatever = true
"#;
        let stamp = SessionStamp::parse(text);
        assert_eq!(stamp.revision, Some(43));
        assert_eq!(stamp.saved_by.as_deref(), Some("tanaka"));
        assert_eq!(stamp.session_id.as_deref(), Some("ab12"));
        assert!(stamp.describe().contains("revision 43"));
        assert!(stamp.describe().contains("saved by tanaka"));
    }

    #[test]
    fn an_unparseable_document_still_yields_an_empty_stamp() {
        let stamp = SessionStamp::parse("this is not toml {{{");
        assert_eq!(stamp, SessionStamp::default());
        assert_eq!(stamp.describe(), "unknown revision");
    }

    #[test]
    fn a_sharing_violation_is_retried_before_it_becomes_an_error() {
        let mut attempts = 0;
        let result = retry_shared_io_with(&[0, 0, 0], || {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "busy"))
            } else {
                Ok(attempts)
            }
        });
        assert_eq!(result.expect("succeeds after retries"), 3);
    }

    #[test]
    fn a_permanent_failure_is_not_retried() {
        let mut attempts = 0;
        let result = retry_shared_io_with(&[0, 0, 0], || {
            attempts += 1;
            Err::<(), _>(io::Error::new(io::ErrorKind::NotFound, "gone"))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 1, "a missing file must not be retried");
    }

    #[test]
    fn retries_are_bounded() {
        let mut attempts = 0;
        let result = retry_shared_io_with(&[0, 0, 0], || {
            attempts += 1;
            Err::<(), _>(io::Error::new(io::ErrorKind::PermissionDenied, "busy"))
        });
        assert!(result.is_err());
        assert_eq!(attempts, 4, "one attempt plus the three configured retries");
    }

    #[test]
    fn a_missing_file_reads_as_missing_rather_than_an_error() {
        let path = std::env::temp_dir().join(format!(
            "neowaves_sync_absent_{}_{}.nwsess",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let state = read_session_state(&path).expect("missing is not an error");
        assert!(matches!(state, SessionDiskState::Missing));
        assert!(state.fingerprint().is_none());
    }

    #[test]
    fn a_display_name_is_never_empty() {
        assert!(!local_display_name().is_empty());
    }

    #[test]
    fn different_audio_never_shares_a_sidecar_name() {
        let a = hash_audio_content(&[vec![0.0, 0.5, 1.0]], 48_000);
        let b = hash_audio_content(&[vec![0.0, 0.5, -1.0]], 48_000);
        assert_ne!(a, b, "two different takes must not collide");
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn the_same_audio_reuses_one_sidecar_name() {
        let a = hash_audio_content(&[vec![0.25, -0.25]], 44_100);
        let b = hash_audio_content(&[vec![0.25, -0.25]], 44_100);
        assert_eq!(a, b);
    }

    #[test]
    fn sample_rate_and_channel_layout_are_part_of_the_name() {
        let mono = hash_audio_content(&[vec![0.1, 0.2]], 48_000);
        let stereo = hash_audio_content(&[vec![0.1], vec![0.2]], 48_000);
        let resampled = hash_audio_content(&[vec![0.1, 0.2]], 44_100);
        assert_ne!(mono, stereo);
        assert_ne!(mono, resampled);
    }

    #[test]
    fn session_ids_are_unique_and_hex() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_timestamp_is_utc_rfc3339() {
        let stamp = now_rfc3339();
        assert!(stamp.ends_with('Z'), "expected UTC, got {stamp}");
        assert!(chrono::DateTime::parse_from_rfc3339(&stamp).is_ok());
    }
}
