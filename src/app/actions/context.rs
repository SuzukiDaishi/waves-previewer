use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::types::{MediaId, WorkspaceView};
use super::super::WavesPreviewer;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorRangeSnapshot {
    pub media_id: MediaId,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub revision: u64,
    pub workspace: String,
    pub visible_count: usize,
    pub library_count: usize,
    pub selected_media_ids: Vec<MediaId>,
    pub active_media_id: Option<MediaId>,
    pub editor_range: Option<EditorRangeSnapshot>,
    pub search_query: String,
    pub search_uses_regex: bool,
    pub session_open: bool,
}

impl ContextSnapshot {
    pub(super) fn capture(app: &WavesPreviewer) -> Self {
        let workspace = match app.workspace_view {
            WorkspaceView::List => "list",
            WorkspaceView::Editor => "editor",
            WorkspaceView::EffectGraph => "effect_graph",
            WorkspaceView::Recording => "recording",
        }
        .to_string();

        let active_media_id = app
            .active_tab
            .and_then(|index| app.tabs.get(index))
            .and_then(|tab| app.path_index.get(&tab.path));
        let editor_range = app
            .active_tab
            .and_then(|index| app.tabs.get(index))
            .and_then(|tab| {
                let media_id = app.path_index.get(&tab.path)?;
                let (start, end) = tab.selection?;
                let sample_rate = u64::from(tab.buffer_sample_rate.max(1));
                Some(EditorRangeSnapshot {
                    media_id,
                    start_ms: (start as u64).saturating_mul(1000) / sample_rate,
                    end_ms: (end as u64).saturating_mul(1000) / sample_rate,
                })
            });

        let mut snapshot = Self {
            revision: 0,
            workspace,
            visible_count: app.files.len(),
            library_count: app.items.len(),
            selected_media_ids: app.selected_item_ids(),
            active_media_id,
            editor_range,
            search_query: app.search_query.clone(),
            search_uses_regex: app.search_use_regex,
            session_open: app.project_path.is_some(),
        };
        snapshot.revision = snapshot.calculate_revision();
        snapshot
    }

    fn calculate_revision(&self) -> u64 {
        let mut stable = self.clone();
        stable.revision = 0;
        let bytes = serde_json::to_vec(&stable).unwrap_or_default();
        let digest = Sha256::digest(bytes);
        u64::from_le_bytes(
            digest[..8]
                .try_into()
                .expect("SHA-256 prefix is eight bytes"),
        )
    }
}

impl WavesPreviewer {
    pub fn action_context_snapshot(&self) -> ContextSnapshot {
        ContextSnapshot::capture(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_stable_for_identical_context() {
        let mut snapshot = ContextSnapshot {
            revision: 0,
            workspace: "list".into(),
            visible_count: 10,
            library_count: 20,
            selected_media_ids: vec![4, 9],
            active_media_id: None,
            editor_range: None,
            search_query: "door".into(),
            search_uses_regex: false,
            session_open: true,
        };
        snapshot.revision = snapshot.calculate_revision();
        assert_eq!(snapshot.revision, snapshot.calculate_revision());
        let previous = snapshot.revision;
        snapshot.selected_media_ids.push(12);
        assert_ne!(previous, snapshot.calculate_revision());
    }
}
