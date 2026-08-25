//! Windows Media Foundation process/thread lifecycle.
//!
//! `MFStartup` refcounts internally, but it is per-process and must be paired
//! with an `MFShutdown`; this guard ties one startup to the life of one object,
//! which is also the life of one worker thread. The video decoder and the AAC
//! audio decoder both need it, so it lives here rather than in either of them.

use anyhow::{Context, Result};
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_LITE, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

pub(crate) struct MfSession;

impl MfSession {
    pub(crate) fn start() -> Result<Self> {
        unsafe {
            // A failure here means COM was already initialised on this thread
            // with a different model, which is fine to proceed from.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("MFStartup")?;
        }
        Ok(Self)
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}
