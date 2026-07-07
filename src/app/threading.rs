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

#[cfg(target_os = "linux")]
pub(super) fn lower_current_thread_priority() {
    // On Linux, setpriority(PRIO_PROCESS, tid) adjusts the niceness of a
    // single thread (a Linux extension - "process" here means kernel task).
    // Nice 10 keeps decode workers well below the UI thread without starving
    // them entirely.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

#[cfg(target_os = "macos")]
pub(super) fn lower_current_thread_priority() {
    // Utility QoS: explicitly background work the user is not waiting on
    // synchronously. The scheduler keeps the (user-interactive) main thread
    // ahead of these workers.
    unsafe {
        let _ = libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(super) fn lower_current_thread_priority() {}
