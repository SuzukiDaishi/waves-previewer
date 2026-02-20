#[cfg(windows)]
pub(super) fn lower_current_thread_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        THREAD_PRIORITY_BELOW_NORMAL,
    };

    // Keep UI responsive under heavy AI workloads.
    unsafe {
        let handle = GetCurrentThread();
        let ok = SetThreadPriority(handle, THREAD_MODE_BACKGROUND_BEGIN as i32);
        if ok == 0 {
            let _ = SetThreadPriority(handle, THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
}

#[cfg(not(windows))]
pub(super) fn lower_current_thread_priority() {}
