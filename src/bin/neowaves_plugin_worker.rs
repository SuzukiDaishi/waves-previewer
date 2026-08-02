use std::io::{BufRead, Write};

#[cfg(windows)]
struct ComInitGuard {
    uninit: bool,
}

#[cfg(windows)]
impl ComInitGuard {
    fn init() -> Self {
        use windows_sys::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
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

    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = std::io::BufWriter::new(stdout.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("read stdin failed: {error}");
                std::process::exit(1);
            }
        }
        let request: neowaves::plugin::WorkerRequest = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(error) => {
                let response = neowaves::plugin::WorkerResponse::Error {
                    message: format!("invalid request: {error}"),
                };
                let _ = serde_json::to_writer(&mut writer, &response);
                let _ = writer.write_all(b"\n");
                let _ = writer.flush();
                continue;
            }
        };
        let response = neowaves::plugin::worker::handle_request(request);
        if serde_json::to_writer(&mut writer, &response).is_err()
            || writer.write_all(b"\n").is_err()
            || writer.flush().is_err()
        {
            std::process::exit(4);
        }
    }
}
