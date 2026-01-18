use std::path::PathBuf;

use super::WavesPreviewer;

#[cfg(feature = "kittest")]
use std::collections::VecDeque;

#[cfg(feature = "kittest")]
#[derive(Default)]
pub struct TestDialogQueue {
    folder: VecDeque<Option<PathBuf>>,
    files: VecDeque<Option<Vec<PathBuf>>>,
}

#[cfg(feature = "kittest")]
impl TestDialogQueue {
    fn next_folder(&mut self) -> Option<PathBuf> {
        self.folder.pop_front().unwrap_or(None)
    }

    fn next_files(&mut self) -> Option<Vec<PathBuf>> {
        self.files.pop_front().unwrap_or(None)
    }

    fn push_folder(&mut self, path: Option<PathBuf>) {
        self.folder.push_back(path);
    }

    fn push_files(&mut self, files: Option<Vec<PathBuf>>) {
        self.files.push_back(files);
    }
}

impl WavesPreviewer {
    pub(super) fn pick_folder_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return self.test_dialogs.next_folder();
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new().pick_folder()
        }
    }

    pub(super) fn pick_files_dialog(&mut self) -> Option<Vec<PathBuf>> {
        #[cfg(feature = "kittest")]
        {
            return self.test_dialogs.next_files();
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("Audio", crate::audio_io::SUPPORTED_EXTS)
                .pick_files()
        }
    }

    pub(super) fn pick_project_open_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return None;
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("NeoWaves Project", &["nwproj"])
                .pick_file()
        }
    }

    pub(super) fn pick_project_save_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return None;
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("NeoWaves Project", &["nwproj"])
                .save_file()
        }
    }

    pub(super) fn pick_external_file_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return None;
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("CSV", &["csv"])
                .pick_file()
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_queue_folder_dialog(&mut self, path: Option<PathBuf>) {
        self.test_dialogs.push_folder(path);
    }

    #[cfg(feature = "kittest")]
    pub fn test_queue_files_dialog(&mut self, files: Option<Vec<PathBuf>>) {
        self.test_dialogs.push_files(files);
    }

    #[cfg(feature = "kittest")]
    pub fn test_simulate_drop_paths(&mut self, paths: &[PathBuf]) -> usize {
        let added = self.add_files_merge(paths);
        if added > 0 {
            self.after_add_refresh();
        }
        added
    }
}
