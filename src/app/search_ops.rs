use std::time::{Duration, Instant};

use super::types::SortDir;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn schedule_search_refresh(&mut self) {
        self.search_dirty = true;
        self.search_deadline = Some(Instant::now() + Duration::from_millis(300));
    }

    /// Compiled highlight regex for the current search query. Rebuilds only
    /// when the query or regex mode changes; `regex::Regex` clones are cheap
    /// (`Arc` internally), so callers can grab one per frame.
    pub(super) fn cached_highlight_regex(&mut self) -> Option<regex::Regex> {
        let query = self.search_query.trim();
        if query.is_empty() {
            return None;
        }
        let stale = match &self.search_highlight_cache {
            Some((cached_query, cached_mode, _)) => {
                cached_query != query || *cached_mode != self.search_use_regex
            }
            None => true,
        };
        if stale {
            let re = crate::app::helpers::build_highlight_regex(query, self.search_use_regex);
            self.search_highlight_cache = Some((query.to_string(), self.search_use_regex, re));
        }
        self.search_highlight_cache
            .as_ref()
            .and_then(|(_, _, re)| re.clone())
    }

    pub(super) fn apply_search_if_due(&mut self) {
        let Some(deadline) = self.search_deadline else {
            return;
        };
        if !self.search_dirty {
            return;
        }
        if Instant::now() >= deadline {
            self.apply_filter_from_search();
            if self.sort_dir != SortDir::None {
                self.apply_sort();
            }
            self.search_dirty = false;
            self.search_deadline = None;
        }
    }
}
