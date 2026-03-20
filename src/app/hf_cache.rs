use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() {
        return;
    }
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn repo_cache_dir_name(repo_id: &str) -> String {
    format!("models--{}", repo_id.replace('/', "--"))
}

fn hf_cache_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(path) = std::env::var_os("HF_HUB_CACHE") {
        push_unique(&mut roots, PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("HUGGINGFACE_HUB_CACHE") {
        push_unique(&mut roots, PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("HF_HOME") {
        let path = PathBuf::from(path);
        push_unique(&mut roots, path.clone());
        push_unique(&mut roots, path.join("hub"));
    }
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        let path = PathBuf::from(path).join("huggingface");
        push_unique(&mut roots, path.clone());
        push_unique(&mut roots, path.join("hub"));
    }
    for env_name in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(path) = std::env::var_os(env_name) {
            let path = PathBuf::from(path).join("huggingface");
            push_unique(&mut roots, path.clone());
            push_unique(&mut roots, path.join("hub"));
        }
    }
    for env_name in ["USERPROFILE", "HOME"] {
        if let Some(path) = std::env::var_os(env_name) {
            let path = PathBuf::from(path).join(".cache").join("huggingface");
            push_unique(&mut roots, path.clone());
            push_unique(&mut roots, path.join("hub"));
        }
    }
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut home = OsString::from(drive);
        home.push(path);
        let path = PathBuf::from(home).join(".cache").join("huggingface");
        push_unique(&mut roots, path.clone());
        push_unique(&mut roots, path.join("hub"));
    }

    roots
}

fn looks_like_hf_hub_root(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    if path.file_name() == Some(OsStr::new("hub")) {
        return true;
    }
    if path.join(".locks").is_dir() {
        return true;
    }
    std::fs::read_dir(path)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.flatten())
        .map(|entry| entry.file_name())
        .any(|name| name.to_string_lossy().starts_with("models--"))
}

fn default_hf_cache_root() -> PathBuf {
    let roots = hf_cache_roots();
    if let Some(path) = roots.iter().find(|path| looks_like_hf_hub_root(path)) {
        return path.clone();
    }
    if let Some(path) = roots.iter().find(|path| path.is_dir()) {
        return path.clone();
    }
    roots
        .into_iter()
        .find(|path| path.file_name() == Some(OsStr::new("hub")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn read_ref_snapshot_path(
    repo_root: &Path,
    snapshot_root: &Path,
    ref_name: &str,
) -> Option<PathBuf> {
    let ref_path = repo_root.join("refs").join(ref_name);
    let rev = std::fs::read_to_string(ref_path).ok()?;
    let rev = rev.lines().next()?.trim();
    if rev.is_empty() {
        return None;
    }
    let snapshot = snapshot_root.join(rev);
    snapshot.is_dir().then_some(snapshot)
}

fn find_latest_matching_snapshot<F>(root: &Path, predicate: &mut F) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !predicate(&path) {
            continue;
        }
        let ts = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((ts, path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn resolve_model_dir_from_start<F>(
    start: &Path,
    revision: &str,
    predicate: &mut F,
) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    if !start.exists() {
        return None;
    }
    if predicate(start) {
        return Some(start.to_path_buf());
    }
    if !start.is_dir() {
        return None;
    }

    let mut repo_root = None::<PathBuf>;
    let mut snapshot_root = None::<PathBuf>;
    if start.file_name() == Some(OsStr::new("snapshots")) {
        snapshot_root = Some(start.to_path_buf());
        repo_root = start.parent().map(Path::to_path_buf);
    } else if start.parent().and_then(|path| path.file_name()) == Some(OsStr::new("snapshots")) {
        snapshot_root = start.parent().map(Path::to_path_buf);
        repo_root = start
            .parent()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);
    } else if start.join("snapshots").is_dir() {
        repo_root = Some(start.to_path_buf());
        snapshot_root = Some(start.join("snapshots"));
    }

    if let (Some(repo_root), Some(snapshot_root)) = (repo_root.as_deref(), snapshot_root.as_deref())
    {
        let mut ref_names = Vec::<&str>::new();
        for name in [revision, "main", "master"] {
            if !ref_names.contains(&name) {
                ref_names.push(name);
            }
        }
        for ref_name in ref_names {
            if let Some(snapshot) = read_ref_snapshot_path(repo_root, snapshot_root, ref_name) {
                if predicate(&snapshot) {
                    return Some(snapshot);
                }
            }
        }
        if let Some(snapshot) = find_latest_matching_snapshot(snapshot_root, predicate) {
            return Some(snapshot);
        }
    }

    None
}

fn push_search_variants(paths: &mut Vec<PathBuf>, anchor: &Path, repo_dir_name: &str) {
    push_unique(paths, anchor.to_path_buf());
    push_unique(paths, anchor.join(repo_dir_name));
    push_unique(paths, anchor.join("hub").join(repo_dir_name));
    push_unique(
        paths,
        anchor.join("huggingface").join("hub").join(repo_dir_name),
    );
    if anchor.file_name() == Some(OsStr::new("snapshots")) {
        if let Some(parent) = anchor.parent() {
            push_unique(paths, parent.to_path_buf());
        }
    }
    if anchor.parent().and_then(|path| path.file_name()) == Some(OsStr::new("snapshots")) {
        if let Some(snapshot_root) = anchor.parent() {
            push_unique(paths, snapshot_root.to_path_buf());
            if let Some(parent) = snapshot_root.parent() {
                push_unique(paths, parent.to_path_buf());
            }
        }
    }
}

fn candidate_search_starts(repo_id: &str, hint: Option<&Path>) -> Vec<PathBuf> {
    let repo_dir_name = repo_cache_dir_name(repo_id);
    let mut starts = Vec::<PathBuf>::new();
    if let Some(hint) = hint {
        push_search_variants(&mut starts, hint, &repo_dir_name);
    }
    for root in hf_cache_roots() {
        push_search_variants(&mut starts, root.as_path(), &repo_dir_name);
    }
    starts
}

fn scan_score(path: &Path) -> i32 {
    let is_snapshot_dir =
        path.parent().and_then(|parent| parent.file_name()) == Some(OsStr::new("snapshots"));
    let is_repo_root = path.join("snapshots").is_dir() && path.join("refs").is_dir();
    let depth = path.components().count() as i32;
    let mut score = 0i32;
    if is_snapshot_dir {
        score += 200;
    }
    if is_repo_root {
        score += 80;
    }
    score - depth
}

fn should_scan_entry(path: &Path) -> bool {
    match path.file_name().and_then(|name| name.to_str()) {
        Some("blobs") | Some(".locks") | Some(".git") | Some("__pycache__") | Some("xet") => false,
        _ => true,
    }
}

fn scan_for_model_dir<F>(root: &Path, predicate: &mut F) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    if !root.is_dir() {
        return None;
    }
    let mut candidates = Vec::<(i32, std::time::SystemTime, PathBuf)>::new();
    let iter = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(5)
        .into_iter()
        .filter_entry(|entry| should_scan_entry(entry.path()));
    for entry in iter.flatten() {
        if !entry.file_type().is_dir() {
            continue;
        }
        let path = entry.path();
        if !predicate(path) {
            continue;
        }
        let ts = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((scan_score(path), ts, path.to_path_buf()));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    candidates.into_iter().next().map(|(_, _, path)| path)
}

pub(super) fn resolve_model_dir<F>(
    repo_id: &str,
    revision: &str,
    hint: Option<&Path>,
    mut predicate: F,
) -> Option<PathBuf>
where
    F: FnMut(&Path) -> bool,
{
    let starts = candidate_search_starts(repo_id, hint);
    for start in &starts {
        if let Some(dir) = resolve_model_dir_from_start(start, revision, &mut predicate) {
            return Some(dir);
        }
    }
    for start in starts {
        if let Some(dir) = scan_for_model_dir(&start, &mut predicate) {
            return Some(dir);
        }
    }
    None
}

pub(super) fn preferred_repo_root(repo_id: &str) -> PathBuf {
    let repo_dir_name = repo_cache_dir_name(repo_id);
    for start in candidate_search_starts(repo_id, None) {
        if !start.is_dir() {
            continue;
        }
        if start.join("snapshots").is_dir() || start.join("refs").is_dir() {
            return start;
        }
        let direct = start.join(&repo_dir_name);
        if direct.join("snapshots").is_dir() || direct.join("refs").is_dir() {
            return direct;
        }
    }
    default_hf_cache_root().join(repo_dir_name)
}

#[cfg(test)]
mod tests {
    use super::{repo_cache_dir_name, resolve_model_dir};
    use std::path::{Path, PathBuf};

    fn temp_dir(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("neowaves_hf_cache_{tag}_{nonce}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_snapshot(repo_root: &Path, rev: &str) -> PathBuf {
        let snapshot = repo_root.join("snapshots").join(rev);
        std::fs::create_dir_all(&snapshot).expect("create snapshot dir");
        snapshot
    }

    fn write_ref(repo_root: &Path, name: &str, rev: &str) {
        std::fs::create_dir_all(repo_root.join("refs")).expect("create refs dir");
        std::fs::write(repo_root.join("refs").join(name), rev).expect("write ref");
    }

    fn mark_ready(dir: &Path) {
        std::fs::write(dir.join("ready.txt"), "ok").expect("write ready marker");
    }

    fn is_ready(dir: &Path) -> bool {
        dir.join("ready.txt").is_file()
    }

    #[test]
    fn resolve_model_dir_accepts_hub_root_hint() {
        let dir = temp_dir("hub_hint");
        let repo_root = dir.join("hub").join(repo_cache_dir_name("org/model"));
        let snapshot = write_snapshot(&repo_root, "abc123");
        mark_ready(&snapshot);
        write_ref(&repo_root, "main", "abc123");

        let resolved = resolve_model_dir(
            "org/model",
            "main",
            Some(dir.join("hub").as_path()),
            is_ready,
        )
        .expect("resolved snapshot");
        assert_eq!(resolved, snapshot);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_model_dir_prefers_ref_snapshot_before_newer_snapshot() {
        let dir = temp_dir("ref_first");
        let repo_root = dir.join(repo_cache_dir_name("org/model"));
        let older = write_snapshot(&repo_root, "oldrev");
        let newer = write_snapshot(&repo_root, "newrev");
        mark_ready(&older);
        mark_ready(&newer);
        write_ref(&repo_root, "main", "oldrev");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = std::fs::write(newer.join("touch.txt"), "newer");

        let resolved = resolve_model_dir("org/model", "main", Some(repo_root.as_path()), is_ready)
            .expect("resolved snapshot");
        assert_eq!(resolved, older);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_model_dir_scans_nested_hint_layout() {
        let dir = temp_dir("nested_scan");
        let repo_root = dir
            .join("custom")
            .join("nested")
            .join(repo_cache_dir_name("org/model"));
        let snapshot = write_snapshot(&repo_root, "scanrev");
        mark_ready(&snapshot);
        write_ref(&repo_root, "main", "scanrev");

        let resolved = resolve_model_dir("org/model", "main", Some(dir.as_path()), is_ready)
            .expect("resolved nested snapshot");
        assert_eq!(resolved, snapshot);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
