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
            match rx.try_recv() {
                Ok((p, overlay, timeline_len, gen)) => {
                    let expected_tool = self.overlay_expected_tool;
                    let expected_path = self.overlay_expected_path.clone();
                    // Generation guard avoids applying stale overlays after rapid tool changes.
                    if gen == self.overlay_expected_gen
                        && expected_path.as_deref() == Some(p.as_path())
                    {
                        if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                            if let Some(tab) = self.tabs.get_mut(idx) {
                                if let Some(tool) = expected_tool {
                                    // If a tool was requested, only apply when it still matches.
                                    if tab.preview_audio_tool == Some(tool)
                                        || tab.active_tool == tool
                                    {
                                        tab.preview_overlay =
                                            Some(Self::preview_overlay_from_channels(
                                                overlay,
                                                tool,
                                                timeline_len,
                                            ));
                                    }
                                } else {
                                    tab.preview_overlay = Some(
                                        Self::preview_overlay_from_channels(
                                            overlay,
                                            tab.active_tool,
                                            timeline_len,
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    self.clear_heavy_overlay_state();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.clear_heavy_overlay_state();
                }
            }
        }
    }
}
