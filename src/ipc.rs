use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

pub const IPC_ADDR: &str = "127.0.0.1:7465";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub project: Option<std::path::PathBuf>,
    pub files: Vec<std::path::PathBuf>,
}

impl IpcRequest {
    pub fn empty() -> Self {
        Self {
            project: None,
            files: Vec::new(),
        }
    }

    pub fn has_payload(&self) -> bool {
        self.project.is_some() || !self.files.is_empty()
    }
}

pub fn try_send_request(req: &IpcRequest) -> bool {
    let Ok(mut stream) = TcpStream::connect(IPC_ADDR) else {
        return false;
    };
    let Ok(payload) = serde_json::to_vec(req) else {
        return false;
    };
    if stream.write_all(&payload).is_err() {
        return false;
    }
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    true
}

pub fn start_listener() -> std::io::Result<mpsc::Receiver<IpcRequest>> {
    let listener = TcpListener::bind(IPC_ADDR)?;
    let (tx, rx) = mpsc::channel::<IpcRequest>();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut buf = Vec::new();
            if stream.read_to_end(&mut buf).is_err() {
                continue;
            }
            let Ok(req) = serde_json::from_slice::<IpcRequest>(&buf) else {
                continue;
            };
            let _ = tx.send(req);
            let _ = stream.write_all(b"ok");
            let _ = stream.flush();
        }
    });
    Ok(rx)
}
