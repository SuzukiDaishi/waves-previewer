use std::io::{BufRead, Write};

#[cfg(windows)]
struct ComInitGuard {
    uninit: bool,
}

#[cfg(windows)]
impl ComInitGuard {
    fn init() -> Self {
        use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED as u32) };
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

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let mut stdout = std::io::stdout();
    let mut service = neowaves::plugin::gui_worker::GuiWorkerService::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("read stdin failed: {e}");
                std::process::exit(1);
            }
        };
        if read == 0 {
            break;
        }
        let req: neowaves::plugin::WorkerRequest = match serde_json::from_str(line.trim_end()) {
            Ok(v) => v,
            Err(e) => {
                let resp = neowaves::plugin::WorkerResponse::Error {
                    message: format!("invalid request: {e}"),
                };
                if let Ok(raw) = serde_json::to_vec(&resp) {
                    let _ = stdout.write_all(&raw);
                    let _ = stdout.write_all(b"\n");
                    let _ = stdout.flush();
                }
                continue;
            }
        };
        let resp = service.handle_request(req);
        let raw = match serde_json::to_vec(&resp) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("encode response failed: {e}");
                std::process::exit(3);
            }
        };
        if let Err(e) = stdout.write_all(&raw) {
            eprintln!("write stdout failed: {e}");
            std::process::exit(4);
        }
        if let Err(e) = stdout.write_all(b"\n") {
            eprintln!("write stdout newline failed: {e}");
            std::process::exit(4);
        }
        if let Err(e) = stdout.flush() {
            eprintln!("flush stdout failed: {e}");
            std::process::exit(4);
        }
    }
}
