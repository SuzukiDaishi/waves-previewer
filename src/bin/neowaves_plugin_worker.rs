use std::io::Read;

#[cfg(windows)]
struct ComInitGuard {
    uninit: bool,
}

#[cfg(windows)]
impl ComInitGuard {
    fn init() -> Self {
        use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        use windows_sys::Win32::System::Com::{
            CoInitializeEx, COINIT_MULTITHREADED,
        };
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED as u32) };
        let ok = hr == S_OK || hr == S_FALSE;
        let changed_mode = hr == RPC_E_CHANGED_MODE;
        Self {
            uninit: ok && !changed_mode,
        }
    }
}

#[cfg(windows)]
impl Drop for ComInitGuard {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn main() {
    #[cfg(windows)]
    let _com = ComInitGuard::init();

    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("read stdin failed: {e}");
        std::process::exit(1);
    }
    let req: neowaves::plugin::WorkerRequest = match serde_json::from_slice(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("invalid request: {e}");
            std::process::exit(2);
        }
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    let output = match serde_json::to_vec(&resp) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("encode response failed: {e}");
            std::process::exit(3);
        }
    };
    if let Err(e) = std::io::Write::write_all(&mut std::io::stdout(), &output) {
        eprintln!("write stdout failed: {e}");
        std::process::exit(4);
    }
}
