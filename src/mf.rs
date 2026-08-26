//! Windows Media Foundation process/thread lifecycle.
//!
//! `MFStartup` refcounts internally, but it is per-process and must be paired
//! with an `MFShutdown`; this guard ties one startup to the life of one object,
//! which is also the life of one worker thread. The video decoder and the AAC
//! audio decoder both need it, so it lives here rather than in either of them.

use anyhow::{Context, Result};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_LITE, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

struct ComApartment {
    uninitialize: bool,
}

impl ComApartment {
    fn start() -> Result<Self> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            // Both S_OK and S_FALSE increment COM's per-thread reference count
            // and therefore require a matching CoUninitialize.
            return Ok(Self { uninitialize: true });
        }
        if result == RPC_E_CHANGED_MODE {
            // The worker was already initialized with another apartment model.
            // COM remains usable, but this call changed no refcount.
            return Ok(Self {
                uninitialize: false,
            });
        }
        Err(
            anyhow::Error::from(windows::core::Error::from_hresult(result))
                .context("CoInitializeEx"),
        )
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) struct MfSession {
    // Dropped after `MfSession::drop`, so Media Foundation shuts down before
    // this guard releases the matching COM initialization.
    _com: ComApartment,
}

impl MfSession {
    pub(crate) fn start() -> Result<Self> {
        let com = ComApartment::start()?;
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).context("MFStartup")?;
        }
        Ok(Self { _com: com })
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}
