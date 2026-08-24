//! Builds `assets/licenses/third_party.json`, the snapshot the Licenses window
//! embeds with `include_str!`.
//!
//! Two inputs are merged. `cargo about generate --format json` supplies every
//! crate in the dependency graph and the licence text found for it. That misses
//! everything that is not a crate -- C/C++ sources a `-sys` crate compiles in,
//! DLLs the Windows installer copies, fonts and data files under `assets/`, the
//! models fetched at runtime -- so `assets/licenses/extra.json` supplies those
//! by hand, along with the per-crate flags and notes that turn the raw graph
//! into something a reader can act on.
//!
//! Licence texts are pooled and referenced by key: 640 crates share 318
//! distinct texts, because a permissive licence carries a different copyright
//! line for each holder. Keeping every distinct text is the point -- MIT and
//! BSD both require the specific copyright notice to travel with the binary --
//! but storing each one once keeps the snapshot near 600 KB instead of 4 MB.
//!
//! Run it through `commands/generate_licenses.ps1`, which invokes cargo-about
//! first.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Sort order for the `kind` groups, so the window opens on the things a reader
/// is most likely to be looking for rather than on 600 crates.
const KIND_ORDER: &[&str] = &[
    // NeoWaves's own statement about the distribution, not a third-party
    // component — the window gives it its own block above everything else.
    "distribution",
    "bundled-native",
    "runtime-dll",
    "spec",
    "font",
    "data",
    "model",
    "crate",
];

fn kind_rank(kind: &str) -> usize {
    KIND_ORDER
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(KIND_ORDER.len())
}

#[derive(Debug, Serialize)]
struct Manifest {
    generated_at: String,
    generator: String,
    licenses: Vec<PooledLicense>,
    components: Vec<Component>,
}

#[derive(Debug, Serialize)]
struct PooledLicense {
    key: String,
    id: String,
    name: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct Component {
    name: String,
    version: String,
    kind: String,
    /// The SPDX expression as written, e.g. `MIT OR Apache-2.0`. Display only.
    license_expr: String,
    /// Individual SPDX ids, for grouping and filtering.
    license_ids: Vec<String>,
    /// Keys into `licenses`, for showing the full text.
    license_keys: Vec<String>,
    authors: String,
    repository: String,
    flag: Option<String>,
    note: Option<String>,
    /// Entries sharing a topic collapse into one row in the window's summary.
    topic: Option<String>,
    /// The cargo feature that gates this component, if any. The app resolves it
    /// with `cfg!` so the window reports the binary in front of the reader
    /// rather than every component the project could be built with.
    feature: Option<String>,
}

/// The hand-maintained side: `assets/licenses/extra.json`.
#[derive(Debug, Deserialize)]
struct Extra {
    #[serde(default)]
    licenses: Vec<ExtraLicense>,
    #[serde(default)]
    components: Vec<ExtraComponent>,
    #[serde(default)]
    crate_overrides: BTreeMap<String, Override>,
}

#[derive(Debug, Deserialize)]
struct ExtraLicense {
    id: String,
    name: String,
    text_file: String,
}

#[derive(Debug, Deserialize)]
struct ExtraComponent {
    name: String,
    version: String,
    kind: String,
    #[serde(default)]
    license_ids: Vec<String>,
    #[serde(default)]
    authors: String,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    flag: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    feature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Override {
    #[serde(default)]
    flag: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    feature: Option<String>,
}

/// Looks up a crate's override, honouring a trailing `*` as a prefix match.
///
/// Families like `symphonia-*` are a dozen crates that all carry the same terms
/// and want the same note; spelling each one out would go stale the moment a
/// codec is added or dropped. An exact key always wins over a wildcard.
fn lookup_override<'a>(
    overrides: &'a BTreeMap<String, Override>,
    name: &str,
) -> Option<&'a Override> {
    if let Some(exact) = overrides.get(name) {
        return Some(exact);
    }
    overrides
        .iter()
        .filter_map(|(key, value)| key.strip_suffix('*').map(|prefix| (prefix, value)))
        .filter(|(prefix, _)| name.starts_with(prefix))
        // Longest prefix wins, so `symphonia-codec-*` could refine `symphonia*`.
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, value)| value)
}

/// Pools licence texts and hands out a stable key per distinct text.
#[derive(Default)]
struct TextPool {
    /// text -> key
    by_text: BTreeMap<String, String>,
    /// how many keys already issued for a given SPDX id, so keys stay unique
    counters: BTreeMap<String, usize>,
    entries: Vec<PooledLicense>,
}

impl TextPool {
    /// Returns the key for `text`, adding it to the pool on first sight.
    ///
    /// Keys are `MIT`, `MIT-2`, `MIT-3`... rather than a content hash so a
    /// regenerated snapshot diffs readably when one crate's copyright line
    /// changes.
    fn intern(&mut self, id: &str, name: &str, text: &str) -> String {
        if let Some(key) = self.by_text.get(text) {
            return key.clone();
        }
        let counter = self.counters.entry(id.to_string()).or_insert(0);
        *counter += 1;
        let key = if *counter == 1 {
            id.to_string()
        } else {
            format!("{id}-{counter}")
        };
        self.by_text.insert(text.to_string(), key.clone());
        self.entries.push(PooledLicense {
            key: key.clone(),
            id: id.to_string(),
            name: name.to_string(),
            text: text.to_string(),
        });
        key
    }
}

/// cargo-about's licence array order is not stable across otherwise equivalent
/// machines. Sort by the fields that define a pooled entry before assigning the
/// human-readable `MIT`, `MIT-2`, ... keys, so CI and developer machines emit
/// byte-identical snapshots.
fn ordered_license_records(licenses: &[Value]) -> Vec<&Value> {
    let mut ordered: Vec<&Value> = licenses.iter().collect();
    ordered.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["id"].as_str().unwrap_or_default())
            .then_with(|| {
                a["text"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["text"].as_str().unwrap_or_default())
            })
            .then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["name"].as_str().unwrap_or_default())
            })
    });
    ordered
}

/// Pulls the bare SPDX ids out of an expression like
/// `(MIT OR Apache-2.0) AND OFL-1.1`.
fn spdx_ids(expr: &str) -> Vec<String> {
    let mut ids: Vec<String> = expr
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+'))
        .filter(|token| !token.is_empty())
        .filter(|token| !matches!(*token, "OR" | "AND" | "WITH"))
        .map(|token| token.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn arg_value(args: &[String], name: &str, default: &str) -> PathBuf {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let raw_path = arg_value(&args, "--raw", "target/about-raw.json");
    let extra_path = arg_value(&args, "--extra", "assets/licenses/extra.json");
    let texts_dir = arg_value(&args, "--texts", "assets/licenses/texts");
    let out_path = arg_value(&args, "--out", "assets/licenses/third_party.json");
    let notice_out_path = arg_value(
        &args,
        "--notice-out",
        "assets/licenses/THIRD_PARTY_NOTICES.txt",
    );

    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(&raw_path)
            .with_context(|| format!("reading cargo-about output {}", raw_path.display()))?,
    )
    .context("parsing cargo-about output")?;

    let extra: Extra = serde_json::from_str(
        &std::fs::read_to_string(&extra_path)
            .with_context(|| format!("reading {}", extra_path.display()))?,
    )
    .context("parsing extra.json")?;

    let mut pool = TextPool::default();

    // cargo-about reports one licence record per (text, crate set). Walk them
    // first so the crate -> text-key map is ready before the crate list.
    let mut keys_for_crate: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut ids_for_crate: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let licenses = raw["licenses"]
        .as_array()
        .context("cargo-about output has no `licenses` array")?;
    for license in ordered_license_records(licenses) {
        let id = license["id"].as_str().unwrap_or_default();
        let name = license["name"].as_str().unwrap_or(id);
        let text = license["text"].as_str().unwrap_or_default();
        if id.is_empty() || text.is_empty() {
            continue;
        }
        let key = pool.intern(id, name, text);
        let Some(used_by) = license["used_by"].as_array() else {
            continue;
        };
        for user in used_by {
            let package = &user["crate"];
            let (Some(name), Some(version)) =
                (package["name"].as_str(), package["version"].as_str())
            else {
                continue;
            };
            let ident = (name.to_string(), version.to_string());
            keys_for_crate
                .entry(ident.clone())
                .or_default()
                .insert(key.clone());
            ids_for_crate
                .entry(ident)
                .or_default()
                .insert(id.to_string());
        }
    }

    let mut components = Vec::new();

    let crates = raw["crates"]
        .as_array()
        .context("cargo-about output has no `crates` array")?;
    for entry in crates {
        let package = &entry["package"];
        let name = package["name"].as_str().unwrap_or_default().to_string();
        let version = package["version"].as_str().unwrap_or_default().to_string();
        if name.is_empty() {
            continue;
        }
        let ident = (name.clone(), version.clone());
        let expr = entry["license"]
            .as_str()
            .or_else(|| package["license"].as_str())
            .unwrap_or_default()
            .to_string();

        // Prefer the ids cargo-about actually resolved a text for; fall back to
        // the declared expression when it resolved none.
        let license_ids: Vec<String> = ids_for_crate
            .get(&ident)
            .map(|ids| ids.iter().cloned().collect())
            .filter(|ids: &Vec<String>| !ids.is_empty())
            .unwrap_or_else(|| spdx_ids(&expr));

        let license_keys: Vec<String> = keys_for_crate
            .get(&ident)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default();

        let authors = package["authors"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let over = lookup_override(&extra.crate_overrides, &name);
        components.push(Component {
            name,
            version,
            kind: "crate".to_string(),
            license_expr: expr,
            license_ids,
            license_keys,
            authors,
            repository: package["repository"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            flag: over.and_then(|o| o.flag.clone()),
            note: over.and_then(|o| o.note.clone()),
            topic: over.and_then(|o| o.topic.clone()),
            feature: over.and_then(|o| o.feature.clone()),
        });
    }

    // The hand-maintained texts join the same pool, keyed by their own id so
    // names such as `NeoWaves-Compliance` stay readable.
    let mut extra_keys: BTreeMap<String, String> = BTreeMap::new();
    for license in &extra.licenses {
        let path = texts_dir.join(&license.text_file);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading licence text {}", path.display()))?;
        let key = pool.intern(&license.id, &license.name, text.trim_end());
        extra_keys.insert(license.id.clone(), key);
    }

    for component in extra.components {
        let mut license_keys = Vec::new();
        for id in &component.license_ids {
            // An extra may reference either its own text or one already pooled
            // from the crate graph (`MIT`, `Apache-2.0`).
            let key = extra_keys.get(id).cloned().or_else(|| {
                pool.entries
                    .iter()
                    .find(|e| &e.id == id)
                    .map(|e| e.key.clone())
            });
            match key {
                Some(key) => license_keys.push(key),
                None => bail!(
                    "extra.json component `{}` references licence id `{}`, but no text for it \
                     exists in assets/licenses/texts/ or in the crate graph",
                    component.name,
                    id
                ),
            }
        }
        let license_expr = component.license_ids.join(" AND ");
        components.push(Component {
            name: component.name,
            version: component.version,
            kind: component.kind,
            license_expr,
            license_ids: component.license_ids,
            license_keys,
            authors: component.authors,
            repository: component.repository,
            flag: component.flag,
            note: component.note,
            topic: component.topic,
            feature: component.feature,
        });
    }

    components.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.version.cmp(&b.version))
    });

    let mut licenses = pool.entries;
    licenses.sort_by(|a, b| a.key.cmp(&b.key));

    let mut manifest = Manifest {
        generated_at: build_date(),
        generator: format!("cargo-about + {}", env!("CARGO_BIN_NAME")),
        licenses,
        components,
    };
    preserve_generated_at_if_unchanged(&mut manifest, &out_path);

    if let Some(parent) = notice_out_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let notices = render_notices(&manifest);
    std::fs::write(&notice_out_path, &notices)
        .with_context(|| format!("writing {}", notice_out_path.display()))?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut json = serde_json::to_string_pretty(&manifest)?;
    json.push('\n');
    std::fs::write(&out_path, &json).with_context(|| format!("writing {}", out_path.display()))?;

    eprintln!(
        "{}: {} components, {} pooled licence texts, {} KiB; notices: {} KiB",
        out_path.display(),
        manifest.components.len(),
        manifest.licenses.len(),
        json.len() / 1024,
        notices.len() / 1024,
    );
    Ok(())
}

/// A release check should not fail just because it runs on another date. Keep
/// the previous date when every substantive field is byte-for-byte equivalent.
fn preserve_generated_at_if_unchanged(manifest: &mut Manifest, out_path: &std::path::Path) {
    let Ok(previous_text) = std::fs::read_to_string(out_path) else {
        return;
    };
    let Ok(mut previous) = serde_json::from_str::<Value>(&previous_text) else {
        return;
    };
    let previous_date = previous
        .get("generated_at")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Ok(mut current) = serde_json::to_value(&*manifest) else {
        return;
    };
    previous
        .as_object_mut()
        .map(|map| map.remove("generated_at"));
    current
        .as_object_mut()
        .map(|map| map.remove("generated_at"));
    if previous == current {
        if let Some(previous_date) = previous_date {
            manifest.generated_at = previous_date;
        }
    }
}

fn render_notices(manifest: &Manifest) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    writeln!(out, "NeoWaves THIRD-PARTY NOTICES").ok();
    writeln!(
        out,
        "Generated from Cargo.lock and assets/licenses/extra.json."
    )
    .ok();
    writeln!(
        out,
        "NeoWaves's own MIT licence is distributed separately as LICENSE.\n"
    )
    .ok();
    writeln!(out, "COMPONENTS\n==========\n").ok();
    for component in &manifest.components {
        writeln!(out, "{} {}", component.name, component.version).ok();
        writeln!(out, "  Kind: {}", component.kind).ok();
        writeln!(out, "  Licence: {}", component.license_expr).ok();
        if !component.authors.is_empty() {
            writeln!(out, "  Authors: {}", component.authors).ok();
        }
        if !component.repository.is_empty() {
            writeln!(out, "  Source: {}", component.repository).ok();
        }
        if let Some(feature) = &component.feature {
            writeln!(out, "  Optional feature: {feature}").ok();
        }
        if let Some(note) = &component.note {
            writeln!(out, "  Note: {note}").ok();
        }
        out.push('\n');
    }

    writeln!(out, "FULL LICENCE TEXTS\n==================\n").ok();
    for license in &manifest.licenses {
        writeln!(out, "{} ({})\n{}", license.name, license.id, "-".repeat(72)).ok();
        out.push_str(license.text.trim_end());
        out.push_str("\n\n");
    }
    out
}

/// `YYYY-MM-DD` for the snapshot header.
///
/// Deliberately date-only: a timestamp would churn the committed file on every
/// regeneration even when nothing about the dependency graph changed.
fn build_date() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pooled_keys(records: &[Value]) -> Vec<(String, String)> {
        let mut pool = TextPool::default();
        for license in ordered_license_records(records) {
            pool.intern(
                license["id"].as_str().unwrap(),
                license["name"].as_str().unwrap(),
                license["text"].as_str().unwrap(),
            );
        }
        pool.entries
            .into_iter()
            .map(|entry| (entry.key, entry.text))
            .collect()
    }

    #[test]
    fn pooled_keys_do_not_depend_on_cargo_about_record_order() {
        let forward = vec![
            json!({"id": "MIT", "name": "MIT", "text": "z copyright"}),
            json!({"id": "Apache-2.0", "name": "Apache", "text": "apache"}),
            json!({"id": "MIT", "name": "MIT", "text": "a copyright"}),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();

        assert_eq!(pooled_keys(&forward), pooled_keys(&reversed));
        assert_eq!(
            pooled_keys(&forward),
            vec![
                ("Apache-2.0".to_string(), "apache".to_string()),
                ("MIT".to_string(), "a copyright".to_string()),
                ("MIT-2".to_string(), "z copyright".to_string()),
            ]
        );
    }
}
