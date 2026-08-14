use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

use crate::app::PluginCatalogEntry;
use crate::plugin::PluginFormat;

pub struct PluginCatalogDb {
    connection: Connection,
}

impl PluginCatalogDb {
    pub fn open_default() -> Result<Self> {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("NeoWaves");
        std::fs::create_dir_all(&base)?;
        Self::open(&base.join("plugin_catalog.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("open plugin catalog: {}", path.display()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS search_paths (
               path TEXT PRIMARY KEY,
               enabled INTEGER NOT NULL DEFAULT 1,
               updated_at INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS plugins (
               plugin_key TEXT PRIMARY KEY,
               path TEXT NOT NULL,
               name TEXT NOT NULL,
               format TEXT NOT NULL,
               architecture TEXT NOT NULL DEFAULT '',
               vendor TEXT NOT NULL DEFAULT '',
               category TEXT NOT NULL DEFAULT '',
               main_input_buses TEXT NOT NULL DEFAULT '',
               main_output_buses TEXT NOT NULL DEFAULT '',
               supports_state INTEGER NOT NULL DEFAULT 0,
               supports_gui INTEGER NOT NULL DEFAULT 0,
               supports_latency INTEGER NOT NULL DEFAULT 0,
               last_failure_kind TEXT,
               last_failure_message TEXT,
               failure_count INTEGER NOT NULL DEFAULT 0,
               quarantine INTEGER NOT NULL DEFAULT 0,
               scanned_at INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        Ok(Self { connection })
    }

    pub fn replace_search_paths(&mut self, paths: &[PathBuf]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM search_paths", [])?;
        for path in paths {
            transaction.execute(
                "INSERT INTO search_paths(path, enabled, updated_at) VALUES (?1, 1, unixepoch())",
                params![path.to_string_lossy().to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_entries(&mut self, entries: &[PluginCatalogEntry]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for entry in entries {
            transaction.execute(
                "INSERT INTO plugins(plugin_key, path, name, format, scanned_at)
                 VALUES (?1, ?2, ?3, ?4, unixepoch())
                 ON CONFLICT(plugin_key) DO UPDATE SET
                   path=excluded.path, name=excluded.name, format=excluded.format,
                   scanned_at=excluded.scanned_at",
                params![
                    entry.key,
                    entry.path.to_string_lossy().to_string(),
                    entry.name,
                    match entry.format {
                        PluginFormat::Vst3 => "vst3",
                        PluginFormat::Clap => "clap",
                    },
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_entries(&self) -> Result<Vec<PluginCatalogEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT plugin_key, name, path, format FROM plugins
             WHERE quarantine=0 ORDER BY lower(name), path",
        )?;
        let rows = statement.query_map([], |row| {
            let format: String = row.get(3)?;
            Ok(PluginCatalogEntry {
                key: row.get(0)?,
                name: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                format: if format.eq_ignore_ascii_case("clap") {
                    PluginFormat::Clap
                } else {
                    PluginFormat::Vst3
                },
            })
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn record_failure(&self, plugin_key: &str, kind: &str, message: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE plugins SET last_failure_kind=?2, last_failure_message=?3,
             failure_count=failure_count+1,
             quarantine=CASE WHEN failure_count+1 >= 3 THEN 1 ELSE quarantine END
             WHERE plugin_key=?1",
            params![plugin_key, kind, message],
        )?;
        Ok(())
    }

    pub fn record_success(&self, plugin_key: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE plugins SET last_failure_kind=NULL, last_failure_message=NULL,
             failure_count=0, quarantine=0 WHERE plugin_key=?1",
            params![plugin_key],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_roundtrips_vst3_and_clap_entries() {
        let mut db = PluginCatalogDb::open(Path::new(":memory:")).unwrap();
        db.upsert_entries(&[
            PluginCatalogEntry {
                key: "a".into(),
                name: "A".into(),
                path: "A.vst3".into(),
                format: PluginFormat::Vst3,
            },
            PluginCatalogEntry {
                key: "b".into(),
                name: "B".into(),
                path: "B.clap".into(),
                format: PluginFormat::Clap,
            },
        ])
        .unwrap();
        let entries = db.load_entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].format, PluginFormat::Clap);
    }
}
