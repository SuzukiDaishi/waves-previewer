//! The Reverb in the app: the IR library's scan, the IRs loaded for it (on a
//! worker, cached by IR and buffer rate), handing the loaded IR to the tab
//! whose settings name it, and the library folder the user chose (prefs).
//! The rules are `ir_library` and `crate::impulse`; the DSP `crate::reverb`.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use super::ir_library::{self, IrCatalog};
use super::loading_ops::{poll_job, JobPoll};
use super::types::ToolKind;
use super::WavesPreviewer;
use crate::impulse::{ImpulseResponse, IrId};

/// IRs kept loaded, so going back and forth between a few is instant.
const IR_CACHE_LEN: usize = 8;

/// An IR at a buffer rate.
type IrKey = (IrId, u32);
type LoadResult = Result<ImpulseResponse, String>;

#[derive(Default)]
pub(crate) struct ReverbRuntime {
    /// The folder the user chose for the whole library (prefs).
    pub(crate) user_dir: Option<PathBuf>,
    pub(crate) catalog: Option<Arc<IrCatalog>>,
    scan: Option<Receiver<IrCatalog>>,
    pub(crate) scan_failed: Option<String>,
    /// Most recently loaded last.
    cache: Vec<Arc<ImpulseResponse>>,
    load: Option<(IrKey, Receiver<LoadResult>)>,
    failed: Option<(IrKey, String)>,
    /// The browser's category and search (for the session of the app).
    pub(crate) category: Option<String>,
    pub(crate) query: String,
}

/// Where the IR a tab's Reverb names stands.
pub(crate) enum IrStatus {
    Ready(Arc<ImpulseResponse>),
    Loading,
    Failed(String),
}

impl ReverbRuntime {
    pub(crate) fn load_prefs_line(&mut self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix("ir_library_dir=") else {
            return false;
        };
        let value = rest.trim();
        self.user_dir = (!value.is_empty()).then(|| PathBuf::from(value));
        true
    }

    pub(crate) fn prefs_lines(&self) -> String {
        self.user_dir
            .as_ref()
            .map(|dir| format!("ir_library_dir={}\n", dir.display()))
            .unwrap_or_default()
    }

    fn cached(&self, key: IrKey) -> Option<Arc<ImpulseResponse>> {
        self.cache
            .iter()
            .find(|ir| (ir.id, ir.buffer_sr) == key)
            .cloned()
    }
}

impl WavesPreviewer {
    /// The IR the tab's Reverb settings name, at the tab's rate.
    fn reverb_key(&self, tab_idx: usize) -> Option<IrKey> {
        let tab = self.tabs.get(tab_idx)?;
        Some((
            tab.tool_state.reverb.ir_or_default(),
            tab.buffer_sample_rate.max(1),
        ))
    }

    /// Every frame: take what the scan and the load finished, and give the
    /// active tab the IR its Reverb names -- from the cache, or by starting
    /// to load it. Nothing to do unless the Reverb is open.
    pub(super) fn tick_reverb(&mut self) {
        self.poll_ir_scan();
        self.poll_ir_load();
        let Some(tab_idx) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.active_tool != ToolKind::Reverb || tab.loading {
            return;
        }
        if self.reverb.catalog.is_none()
            && self.reverb.scan.is_none()
            && self.reverb.scan_failed.is_none()
        {
            self.start_ir_scan();
        }
        let Some(key) = self.reverb_key(tab_idx) else {
            return;
        };
        let tab = &self.tabs[tab_idx];
        if tab
            .reverb_ir
            .as_ref()
            .is_some_and(|ir| (ir.id, ir.buffer_sr) == key)
        {
            return;
        }
        if let Some(ir) = self.reverb.cached(key) {
            self.tabs[tab_idx].reverb_ir = Some(ir);
            return;
        }
        let loading = self.reverb.load.as_ref().is_some_and(|(k, _)| *k == key);
        let failed = self.reverb.failed.as_ref().is_some_and(|(k, _)| *k == key);
        if !loading && !failed {
            self.start_ir_load(key);
        }
    }

    pub(crate) fn reverb_ir_status(&self, tab_idx: usize) -> IrStatus {
        let Some(key) = self.reverb_key(tab_idx) else {
            return IrStatus::Loading;
        };
        if let Some(ir) = self.tabs[tab_idx]
            .reverb_ir
            .as_ref()
            .filter(|ir| (ir.id, ir.buffer_sr) == key)
        {
            return IrStatus::Ready(ir.clone());
        }
        match &self.reverb.failed {
            Some((k, message)) if *k == key => IrStatus::Failed(message.clone()),
            _ => IrStatus::Loading,
        }
    }

    fn start_ir_scan(&mut self) {
        let roots = ir_library::library_roots(self.reverb.user_dir.as_deref());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("ir-scan".to_string())
            .spawn(move || {
                let _ = tx.send(ir_library::scan(&roots));
                crate::ui_wake::wake_ui();
            })
            .ok();
        self.reverb.scan = Some(rx);
    }

    fn poll_ir_scan(&mut self) {
        let Some(rx) = &self.reverb.scan else {
            return;
        };
        match poll_job(rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(catalog) => {
                self.debug_log(format!(
                    "reverb: {} IRs in {} folder(s)",
                    catalog.entries.len(),
                    catalog.roots.len()
                ));
                self.reverb.catalog = Some(Arc::new(catalog));
            }
            JobPoll::Gone => {
                self.reverb.scan_failed =
                    Some("the IR library scan ended without an answer".to_string());
            }
        }
        self.reverb.scan = None;
    }

    fn start_ir_load(&mut self, key: IrKey) {
        let roots = ir_library::library_roots(self.reverb.user_dir.as_deref());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("ir-load".to_string())
            .spawn(move || {
                let _ = tx.send(crate::impulse::load(key.0, &roots, key.1));
                crate::ui_wake::wake_ui();
            })
            .ok();
        self.reverb.load = Some((key, rx));
    }

    fn poll_ir_load(&mut self) {
        let Some((key, rx)) = &self.reverb.load else {
            return;
        };
        let key = *key;
        match poll_job(rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(Ok(ir)) => {
                self.debug_log(format!(
                    "reverb: loaded {} ({})",
                    ir.path.display(),
                    ir.describe()
                ));
                let cache = &mut self.reverb.cache;
                cache.retain(|c| (c.id, c.buffer_sr) != key);
                cache.push(Arc::new(ir));
                if cache.len() > IR_CACHE_LEN {
                    cache.remove(0);
                }
                if self.reverb.failed.as_ref().is_some_and(|(k, _)| *k == key) {
                    self.reverb.failed = None;
                }
            }
            JobPoll::Ready(Err(message)) => {
                self.debug_log(format!("reverb: {message}"));
                self.reverb.failed = Some((key, message));
            }
            JobPoll::Gone => {
                self.reverb.failed = Some((
                    key,
                    "the loading thread ended without an answer".to_string(),
                ));
            }
        }
        self.reverb.load = None;
    }

    /// Use the library at `dir` (`None`: only the installed selection), and
    /// look through it again.
    pub(crate) fn set_ir_library_dir(&mut self, dir: Option<PathBuf>) {
        self.reverb.user_dir = dir;
        self.save_prefs();
        self.rescan_ir_library();
    }

    /// Forget what the library held and what failed to load, and scan again.
    pub(crate) fn rescan_ir_library(&mut self) {
        self.reverb.catalog = None;
        self.reverb.scan_failed = None;
        self.reverb.failed = None;
        self.start_ir_scan();
    }
}
