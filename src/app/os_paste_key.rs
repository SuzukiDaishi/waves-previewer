//! Ctrl+V as Windows saw it, for the paste egui never reports.
//!
//! egui-winit turns Ctrl+V into `Event::Paste(text)` -- but only when the
//! clipboard holds text. Copy files in Explorer and the clipboard holds a file
//! list (CF_HDROP) and nothing else, so egui-winit swallows the key: no
//! `Event::Paste`, no `Key::V`, nothing the app can see. That is the whole of
//! "Ctrl+V does nothing after copying in Explorer" (and of the investigation
//! in docs/CLIPBOARD_HOTKEY_ISSUE_20260201.md).
//!
//! So on Windows a keyboard hook scoped to the UI thread notes the keystroke
//! itself. It sees only this thread's messages -- keys pressed while one of
//! our windows has focus -- passes every key on untouched, and does nothing
//! but raise a flag the clipboard handler takes once per frame.

use std::cell::Cell;

thread_local! {
    /// Per thread, not per process: a thread hook runs on the thread that
    /// installed it -- the UI thread, which is also the one that takes the
    /// flag -- and parallel tests, each on its own thread, stay apart.
    static PASTE_PRESSED: Cell<bool> = const { Cell::new(false) };
}

/// Virtual-key codes the paste chords use. A letter's code is its uppercase
/// ASCII value.
const VK_V: u16 = b'V' as u16;
const VK_INSERT: u16 = 0x2D;

/// Whether a keystroke is a paste chord being pressed: Ctrl+V or Shift+Insert
/// (the same chords egui-winit treats as paste), not auto-repeat from a held
/// key, not with Alt (AltGr on many layouts), not the key coming back up.
///
/// `flags` is the keystroke-message lParam: bit 30 is "was already down"
/// (auto-repeat), bit 31 is "being released".
pub(crate) fn is_paste_press(vk: u16, flags: u32, ctrl: bool, shift: bool, alt: bool) -> bool {
    let released = flags & (1 << 31) != 0;
    let repeat = flags & (1 << 30) != 0;
    if released || repeat || alt {
        return false;
    }
    (vk == VK_V && ctrl && !shift) || (vk == VK_INSERT && shift && !ctrl)
}

/// Whether Ctrl+V (or Shift+Insert) was pressed since the last call. Taken
/// every frame whatever the workspace, so a press the list did not own
/// cannot fire later.
pub(crate) fn take() -> bool {
    PASTE_PRESSED.with(|flag| flag.replace(false))
}

/// Stands in for the hook in tests, which have no message loop.
#[cfg(feature = "kittest")]
pub(crate) fn note_for_test() {
    PASTE_PRESSED.with(|flag| flag.set(true));
}

/// Installs the hook on the calling thread, once per process. Call it from
/// the thread that runs the event loop.
#[cfg(windows)]
pub(crate) fn install() {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_KEYBOARD};

    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // SAFETY: a thread hook needs no module handle; the procedure is a
        // plain function that lives for the whole process.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD,
                Some(hook_proc),
                std::ptr::null_mut(),
                GetCurrentThreadId(),
            )
        };
        if hook.is_null() {
            eprintln!("os_paste_key: SetWindowsHookExW failed; Explorer file paste disabled");
        }
    });
}

#[cfg(not(windows))]
pub(crate) fn install() {}

#[cfg(windows)]
unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HC_ACTION};

    // HC_ACTION: the message is being taken off the queue. HC_NOREMOVE is
    // a peek at the same message, which would count the press twice.
    if code == HC_ACTION as i32 {
        // A key is held when the high bit of its state is set.
        let held = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
        if is_paste_press(
            wparam as u16,
            lparam as u32,
            held(VK_CONTROL),
            held(VK_SHIFT),
            held(VK_MENU),
        ) {
            PASTE_PRESSED.with(|flag| flag.set(true));
            crate::ui_wake::wake_ui();
        }
    }
    // SAFETY: passing the message on unchanged, as every hook must.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWN: u32 = 1;
    const REPEAT: u32 = 1 | (1 << 30);
    const UP: u32 = 1 | (1 << 30) | (1 << 31);

    #[test]
    fn ctrl_v_press_is_a_paste() {
        assert!(is_paste_press(VK_V, DOWN, true, false, false));
    }

    #[test]
    fn repeat_release_and_bare_v_are_not() {
        assert!(!is_paste_press(VK_V, REPEAT, true, false, false), "held key");
        assert!(!is_paste_press(VK_V, UP, true, false, false), "key up");
        assert!(!is_paste_press(VK_V, DOWN, false, false, false), "no Ctrl");
    }

    #[test]
    fn other_modifiers_make_it_another_chord() {
        assert!(!is_paste_press(VK_V, DOWN, true, false, true), "Ctrl+Alt+V / AltGr");
        assert!(!is_paste_press(VK_V, DOWN, true, true, false), "Ctrl+Shift+V");
    }

    #[test]
    fn shift_insert_is_a_paste_too() {
        assert!(is_paste_press(VK_INSERT, DOWN, false, true, false));
        assert!(!is_paste_press(VK_INSERT, DOWN, true, true, false), "Ctrl+Shift+Insert");
        assert!(!is_paste_press(VK_INSERT, DOWN, false, false, false), "bare Insert");
    }
}
