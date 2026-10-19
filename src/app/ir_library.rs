//! The Reverb's IR library as rules: where it may be on this machine, and
//! what is in it. `scan` blocks (it walks folders that may be on a share):
//! workers only.
//!
//! The library is whichever of these exist, in this order -- an IR named
//! by its path inside the library is taken from the first that has it:
//! the folder the user chose (the whole library, downloaded), the selection
//! the installer put in `ir\` beside NeoWaves, and in a development build
//! the git submodule at `vendor/IR-Library`.

use std::path::{Path, PathBuf};

/// The folder beside the executable the installer puts its selection of
/// IRs in (`assets/ir/bundled.txt`).
pub const BUNDLED_DIR: &str = "ir";

/// File types read as IRs.
const IR_EXTENSIONS: [&str; 5] = ["wav", "wave", "aif", "aiff", "flac"];

/// The library's roots, most preferred first. Not checked: `scan` and
/// `crate::impulse::resolve` skip the ones that do not exist.
pub fn library_roots(user_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = user_dir {
        roots.push(dir.to_path_buf());
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        roots.push(dir.join(BUNDLED_DIR));
    }
    // A development build runs from target/, where the installer never put
    // anything: take the library from the repository.
    #[cfg(debug_assertions)]
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/IR-Library"));
    roots
}

/// One IR in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrEntry {
    /// Its path inside the library, `/` between the parts.
    pub rel: String,
    /// Its file name without the extension.
    pub name: String,
}

impl IrEntry {
    /// The library's top folder it is in ("Rooms"), or "" at the top.
    pub fn category(&self) -> &str {
        self.rel.split_once('/').map_or("", |(top, _)| top)
    }
}

/// What a scan found.
#[derive(Clone, Debug, Default)]
pub struct IrCatalog {
    /// The roots that exist, in order.
    pub roots: Vec<PathBuf>,
    /// Every IR, once, sorted by its path.
    pub entries: Vec<IrEntry>,
}

impl IrCatalog {
    /// The top folders, in order.
    pub fn categories(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for e in &self.entries {
            let c = e.category();
            if !c.is_empty() && out.last() != Some(&c) && !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// The entries in `category` (all of them for `None`) whose path holds
    /// every word of `query`, in any case.
    pub fn filter(&self, category: Option<&str>, query: &str) -> Vec<&IrEntry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .filter(|e| {
                category.map_or(true, |c| e.category() == c) && {
                    let rel = e.rel.to_lowercase();
                    words.iter().all(|w| rel.contains(w.as_str()))
                }
            })
            .collect()
    }
}

fn is_ir_file(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'));
    !hidden
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| IR_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Walk `roots` for IRs. An IR in two roots is listed once (the first root
/// is the one `resolve` will read). Blocking.
pub fn scan(roots: &[PathBuf]) -> IrCatalog {
    let mut catalog = IrCatalog::default();
    let mut seen = std::collections::HashSet::new();
    for root in roots.iter().filter(|r| r.is_dir()) {
        catalog.roots.push(root.clone());
        let walk = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0 || !e.file_name().to_str().is_some_and(|n| n.starts_with('.'))
            });
        for entry in walk.filter_map(Result::ok) {
            if !entry.file_type().is_file() || !is_ir_file(entry.path()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let rel = crate::impulse::normalize_rel(&rel.to_string_lossy());
            if seen.insert(rel.clone()) {
                let name = entry
                    .path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| rel.clone());
                catalog.entries.push(IrEntry { rel, name });
            }
        }
    }
    catalog
        .entries
        .sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    catalog
}

#[cfg(test)]
/// The installer's selection: library paths, one per line. A line that
/// starts with `#` is a comment -- only at the start, since names in the
/// library contain `#` ("F# Minor 11 Wide Saw").
pub fn bundled_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(crate::impulse::normalize_rel)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neowaves_irlib_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_scan_lists_each_ir_once_from_the_first_root() {
        let a = temp("a");
        let b = temp("b");
        for (root, rel) in [
            (&a, "Rooms/Hall/Big Hall.wav"),
            (&a, "Plates/Plate 1.WAV"),
            (&b, "Rooms/Hall/Big Hall.wav"),
            (&b, "Springs/Spring.aiff"),
            (&b, "Springs/notes.txt"),
            (&b, "Springs/._Spring.aiff"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        let missing = a.join("not-there");
        let catalog = scan(&[a.clone(), missing, b.clone()]);
        assert_eq!(catalog.roots, vec![a.clone(), b.clone()]);
        let rels: Vec<&str> = catalog.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(
            rels,
            [
                "Plates/Plate 1.WAV",
                "Rooms/Hall/Big Hall.wav",
                "Springs/Spring.aiff"
            ]
        );
        assert_eq!(catalog.categories(), ["Plates", "Rooms", "Springs"]);
        assert_eq!(catalog.entries[1].name, "Big Hall");
        let hits: Vec<&str> = catalog
            .filter(None, "big HALL")
            .iter()
            .map(|e| e.rel.as_str())
            .collect();
        assert_eq!(hits, ["Rooms/Hall/Big Hall.wav"]);
        assert_eq!(catalog.filter(Some("Springs"), "").len(), 1);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn the_bundled_selection_is_in_the_library() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let list = std::fs::read_to_string(root.join("assets/ir/bundled.txt"))
            .expect("assets/ir/bundled.txt");
        let entries = bundled_list(&list);
        assert!(entries.len() >= 20, "{} IRs", entries.len());
        assert!(
            entries.iter().any(|e| e == crate::impulse::DEFAULT_IR),
            "the default IR ships"
        );
        let library = root.join("vendor/IR-Library");
        if !library.join("License.md").is_file() {
            eprintln!("vendor/IR-Library is not checked out: the files were not checked");
            return;
        }
        let mut bytes = 0u64;
        for rel in &entries {
            let path = library.join(rel);
            let meta =
                std::fs::metadata(&path).unwrap_or_else(|_| panic!("{rel} is not in the library"));
            bytes += meta.len();
        }
        // The installer carries these: keep the selection a selection.
        assert!(bytes < 160_000_000, "{} MB", bytes / 1_000_000);
    }
}
