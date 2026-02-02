impl super::WavesPreviewer {
    pub(super) fn drain_heavy_preview_results(&mut self) {
        if let Some(rx) = &self.heavy_preview_rx {
            if let Ok(mono) = rx.try_recv() {
                if let Some(idx) = self.active_tab {
                    if let Some(tool) = self.heavy_preview_tool {
                        // Preview results are mono overlays tied to the current tool.
                        self.set_preview_mono(idx, tool, mono);
                    }
                }
                self.heavy_preview_rx = None;
                self.heavy_preview_tool = None;
            }
        }
    }

    pub(super) fn drain_heavy_overlay_results(&mut self) {
        if let Some(rx) = &self.heavy_overlay_rx {
            if let Ok((p, overlay, timeline_len, gen)) = rx.try_recv() {
                let expected_tool = self.overlay_expected_tool.take();
                // Generation guard avoids applying stale overlays after rapid tool changes.
                if gen == self.overlay_expected_gen {
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                        if let Some(tab) = self.tabs.get_mut(idx) {
                            if let Some(tool) = expected_tool {
                                // If a tool was requested, only apply when it still matches.
                                if tab.preview_audio_tool == Some(tool) || tab.active_tool == tool {
                                    tab.preview_overlay =
                                        Some(Self::preview_overlay_from_channels(
                                            overlay,
                                            tool,
                                            timeline_len,
                                        ));
                                }
                            } else {
                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                    overlay,
                                    tab.active_tool,
                                    timeline_len,
                                ));
                            }
                        }
                    }
                }
                self.heavy_overlay_rx = None;
            }
        }
    }
}
