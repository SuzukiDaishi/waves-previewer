//! Keeping the picture windows just in front of the app.
//!
//! A video window -- the editor's detached viewer, or a Multi Edits track's
//! picture -- is an OS window of its own, and left alone it drops behind the
//! main window the moment the main window is clicked. On Windows each one is
//! made an *owned* window of the main window (`GWLP_HWNDPARENT`). Windows then
//! keeps it above its owner and hides it with the owner when that is
//! minimized. Other programs can still cover both, as they would not with an
//! always-on-top window.
//!
//! egui's `ViewportBuilder` has no owner, and eframe does not pass winit's
//! through, so the owner is set once the window exists. An immediate viewport
//! is created on the UI thread inside `show_viewport_immediate`, so this
//! thread's windows are searched for the one carrying the viewport's title.
//!
//! Elsewhere this does nothing.

use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct WindowOwner {
    /// The main window's HWND, once `update` has been handed it.
    main: Option<isize>,
    /// Picture windows already given the main window as owner.
    owned: HashSet<egui::ViewportId>,
    /// Picture windows shown this frame.
    shown: HashSet<egui::ViewportId>,
}

impl WindowOwner {
    /// Note the main window from the frame `update` is handed. Under kittest
    /// there is no native window, and nothing is noted.
    pub fn note_main(&mut self, frame: &eframe::Frame) {
        if self.main.is_some() {
            return;
        }
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                    self.main = Some(win32.hwnd.get());
                }
            }
        }
        #[cfg(not(windows))]
        let _ = frame;
    }

    /// Keep the picture window of `viewport`, titled `title`, in front of the
    /// main window. Call right after showing it, on every frame it is shown;
    /// a window not found yet is looked for again next frame.
    pub fn keep_in_front(&mut self, viewport: egui::ViewportId, title: &str) {
        self.shown.insert(viewport);
        if self.owned.contains(&viewport) {
            return;
        }
        let Some(main) = self.main else {
            return;
        };
        if own_window_titled(main, title) {
            self.owned.insert(viewport);
        }
    }

    /// Forget the windows not shown this frame: one opened again is a new OS
    /// window, and needs its owner set again.
    pub fn end_frame(&mut self) {
        let shown = std::mem::take(&mut self.shown);
        self.owned.retain(|id| shown.contains(id));
    }
}

/// Give every window of this thread titled `title` (other than `main`) the
/// window `main` as owner, and bring it up in front of it. Returns whether
/// any was found.
#[cfg(all(windows, target_pointer_width = "64"))]
fn own_window_titled(main: isize, title: &str) -> bool {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetWindowTextLengthW, GetWindowTextW, SetWindowLongPtrW, SetWindowPos,
        GWLP_HWNDPARENT, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    struct Search {
        main: HWND,
        wanted: Vec<u16>,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` below, alive for the whole
        // enumeration, which runs on this thread before it returns.
        let search = unsafe { &mut *(lparam as *mut Search) };
        if hwnd != search.main {
            // SAFETY: `hwnd` is a live window handed over by the enumeration.
            let len = unsafe { GetWindowTextLengthW(hwnd) };
            if len as usize == search.wanted.len() && len > 0 {
                let mut text = vec![0u16; len as usize + 1];
                // SAFETY: the buffer holds `len` characters and the NUL.
                let got = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
                if got as usize == search.wanted.len() && text[..got as usize] == search.wanted[..] {
                    search.found.push(hwnd);
                }
            }
        }
        1
    }

    let mut search = Search {
        main: main as HWND,
        wanted: title.encode_utf16().collect(),
        found: Vec::new(),
    };
    // SAFETY: the callback only reads the windows it is given and writes to
    // `search`, which outlives the call.
    unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(visit),
            &mut search as *mut Search as LPARAM,
        );
    }
    for &hwnd in &search.found {
        // SAFETY: both handles are live top-level windows of this thread.
        // GWLP_HWNDPARENT on a top-level window sets its owner.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, main);
            SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
    !search.found.is_empty()
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
fn own_window_titled(_main: isize, _title: &str) -> bool {
    false
}

#[cfg(all(test, windows, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetWindow, GW_OWNER, WS_OVERLAPPEDWINDOW,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// A hidden top-level window of this thread, titled `title`.
    fn window(title: &str) -> HWND {
        let class = wide("STATIC");
        let title = wide(title);
        // SAFETY: a system class needs no module handle; the strings are
        // NUL-terminated and live through the call.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "create a test window");
        hwnd
    }

    fn owner_of(hwnd: HWND) -> HWND {
        // SAFETY: a live window of this test.
        unsafe { GetWindow(hwnd, GW_OWNER) }
    }

    #[test]
    fn a_picture_window_is_owned_by_the_main_window() {
        let main = window("owner test: main");
        let title = "NeoWaves Video \u{2014} owner test / Video 01";
        let video = window(title);
        let other = window("owner test: another window");

        let mut owner = WindowOwner {
            main: Some(main as isize),
            ..WindowOwner::default()
        };
        let viewport = egui::ViewportId::from_hash_of("owner test");
        owner.keep_in_front(viewport, title);
        assert_eq!(owner_of(video), main, "the picture window is owned");
        assert!(owner_of(other).is_null(), "only the window with that title");
        assert!(owner_of(main).is_null());
        assert!(owner.owned.contains(&viewport));

        // Shown this frame: kept. Not shown next frame: forgotten, so a
        // window opened again is owned again.
        owner.end_frame();
        assert!(owner.owned.contains(&viewport));
        owner.end_frame();
        assert!(!owner.owned.contains(&viewport));

        assert!(!own_window_titled(main as isize, "owner test: no such window"));
        // SAFETY: the windows this test made.
        unsafe {
            DestroyWindow(video);
            DestroyWindow(other);
            DestroyWindow(main);
        }
    }

    #[test]
    fn without_a_main_window_nothing_is_owned() {
        let title = "NeoWaves Video \u{2014} owner test / no main";
        let video = window(title);
        let mut owner = WindowOwner::default();
        owner.keep_in_front(egui::ViewportId::from_hash_of("no main"), title);
        assert!(owner_of(video).is_null());
        assert!(owner.owned.is_empty());
        // SAFETY: the window this test made.
        unsafe {
            DestroyWindow(video);
        }
    }
}
