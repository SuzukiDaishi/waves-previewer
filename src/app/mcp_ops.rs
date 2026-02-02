use crate::mcp;

use super::types::{MediaId, MediaSource};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn process_mcp_commands(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.mcp_cmd_rx else {
            return;
        };
        let Some(tx) = self.mcp_resp_tx.clone() else {
            return;
        };
        let mut cmds = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            cmds.push(cmd);
        }
        for cmd in cmds {
            let res = self.handle_mcp_command(cmd, ctx);
            let _ = tx.send(res);
        }
    }

    pub(super) fn mcp_list_files(
        &self,
        args: mcp::types::ListFilesArgs,
    ) -> std::result::Result<mcp::types::ListFilesResult, String> {
        use regex::RegexBuilder;
        let query = args.query.unwrap_or_default();
        let query = query.trim().to_string();
        let use_regex = args.regex.unwrap_or(false);
        let mut ids: Vec<MediaId> = self.files.clone();
        ids.retain(|id| {
            self.item_for_id(*id)
                .map(|item| item.source == MediaSource::File)
                .unwrap_or(false)
        });
        if !query.is_empty() {
            let re = if use_regex {
                RegexBuilder::new(&query)
                    .case_insensitive(true)
                    .build()
                    .ok()
            } else {
                RegexBuilder::new(&regex::escape(&query))
                    .case_insensitive(true)
                    .build()
                    .ok()
            };
            ids.retain(|id| {
                let Some(item) = self.item_for_id(*id) else {
                    return false;
                };
                let name = item.display_name.as_str();
                let parent = item.display_folder.as_str();
                let transcript = item
                    .transcript
                    .as_ref()
                    .map(|t| t.full_text.as_str())
                    .unwrap_or("");
                let external_hit = item.external.values().any(|v| {
                    if let Some(re) = re.as_ref() {
                        re.is_match(v)
                    } else {
                        false
                    }
                });
                if let Some(re) = re.as_ref() {
                    re.is_match(name)
                        || re.is_match(parent)
                        || re.is_match(transcript)
                        || external_hit
                } else {
                    false
                }
            });
        }
        let total = ids.len() as u32;
        let offset = args.offset.unwrap_or(0) as usize;
        let limit = args.limit.unwrap_or(u32::MAX) as usize;
        let include_meta = args.include_meta.unwrap_or(true);
        let mut items = Vec::new();
        for id in ids.into_iter().skip(offset).take(limit) {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let path = item.path.display().to_string();
            let name = item.display_name.clone();
            let folder = item.display_folder.clone();
            let meta = if include_meta {
                item.meta.as_ref()
            } else {
                None
            };
            let status = if !item.path.exists() {
                Some("missing".to_string())
            } else if let Some(m) = item.meta.as_ref() {
                if m.decode_error.is_some() {
                    Some("decode_failed".to_string())
                } else {
                    Some("ok".to_string())
                }
            } else {
                None
            };
            items.push(mcp::types::FileItem {
                path,
                name,
                folder,
                length_secs: meta.and_then(|m| m.duration_secs),
                sample_rate: meta.map(|m| m.sample_rate),
                channels: meta.map(|m| m.channels),
                bits: meta.map(|m| m.bits_per_sample),
                peak_db: meta.and_then(|m| m.peak_db),
                lufs_i: meta.and_then(|m| m.lufs_i),
                gain_db: Some(item.pending_gain_db),
                status,
            });
        }
        Ok(mcp::types::ListFilesResult { total, items })
    }
}
