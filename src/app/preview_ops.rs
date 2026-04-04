impl super::WavesPreviewer {
    pub(super) fn drain_heavy_preview_results(&mut self) {
        if let Some(rx) = &self.heavy_preview_rx {
            match rx.try_recv() {
                Ok((path, tool, mono, gen)) => {
                    let expected_path = self.heavy_preview_expected_path.clone();
                    let expected_tool = self.heavy_preview_expected_tool;
                    let expected_gen = self.heavy_preview_expected_gen;
                    if gen == expected_gen
                        && expected_tool == Some(tool)
                        && expected_path.as_deref() == Some(path.as_path())
                    {
                        if let Some(idx) = self.active_tab {
                            if self
                                .tabs
                                .get(idx)
                                .map(|tab| tab.path == path && tab.active_tool == tool)
                                .unwrap_or(false)
                            {
                                self.set_preview_mono(idx, tool, mono);
                            }
                        }
                    }
                    self.clear_heavy_preview_state();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.clear_heavy_preview_state();
                }
            }
        }
    }

    pub(super) fn drain_heavy_overlay_results(&mut self) {
        if let Some(rx) = &self.heavy_overlay_rx {
            loop {
                match rx.try_recv() {
                    Ok((p, tool, overlay, gen, is_final)) => {
                        let expected_tool = self.overlay_expected_tool;
                        let expected_path = self.overlay_expected_path.clone();
                        if gen == self.overlay_expected_gen
                            && expected_path.as_deref() == Some(p.as_path())
                        {
                            if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                                if let Some(tab) = self.tabs.get_mut(idx) {
                                    if let Some(expected_tool) = expected_tool {
                                        if tab.preview_audio_tool == Some(expected_tool)
                                            || tab.active_tool == expected_tool
                                            || overlay.is_overview_only()
                                        {
                                            tab.preview_overlay = Some(overlay);
                                        }
                                    } else {
                                        let _ = tool;
                                        tab.preview_overlay = Some(overlay);
                                    }
                                }
                            }
                        }
                        if is_final {
                            self.clear_heavy_overlay_state();
                            break;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.clear_heavy_overlay_state();
                        break;
                    }
                }
            }
        }
    }
}
