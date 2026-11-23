//! File-level scanning, planning and safe execution. Never writes raw disk devices.
use serde::Serialize;
use blake3::Hasher;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

const CHUNK: usize = 1024 * 1024;
/// Allow space for normal filesystem activity and metadata.
pub const SAFETY_MARGIN: u64 = 32 * 1024 * 1024;
const APP_RECOVERY: &str = ".comparator-recovery";
const TMP_PREFIX: &str = ".comparator-stage-";

#[derive(Clone, Debug, Serialize)]
pub struct FileRecord {
    pub relative: PathBuf,
    pub size: u64,
    pub digest: String,
    #[serde(skip_serializing)]
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub root: PathBuf,
    pub files: Vec<FileRecord>,
    pub folders: Vec<PathBuf>,
    pub total_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Pair {
    pub source: Inventory,
    pub destination: Inventory,
    pub scan_mode: ScanMode,
}

/// Smart mode uses matching size and modification time as a shortcut for unchanged
/// same-path files. Verified always reads every byte before claiming equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ScanMode { Smart, Verified }


#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Mode {
    Changes,
    Mirror,
    Partial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Action {
    CopyNew,
    Replace,
    Identical,
    FoundElsewhere,
    KeepTarget,
    Quarantine,
    NoSpace,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanEntry {
    pub relative: PathBuf,
    pub size: u64,
    pub action: Action,
    pub existing_match: Option<PathBuf>,
    pub source_digest: Option<String>,
    pub target_digest: Option<String>,
    pub target_size: Option<u64>,
    #[serde(skip_serializing)]
    pub source_modified: Option<SystemTime>,
    #[serde(skip_serializing)]
    pub target_modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub mode: Mode,
    pub entries: Vec<PlanEntry>,
    pub new_count: usize,
    pub replace_count: usize,
    pub same_count: usize,
    /// How many same-path matches were inferred from size and modification time.
    pub metadata_matches: usize,
    pub matching_elsewhere: usize,
    pub target_only: usize,
    pub quarantine_count: usize,
    pub new_folders: usize,
    pub old_folders: usize,
    pub no_space_count: usize,
    pub bytes_to_copy: u64,
    pub bytes_not_fitting: u64,
    pub required_bytes_before_partial: u64,
    pub available_bytes: Option<u64>,
    pub space_blocked: bool,
}

impl Plan {
    pub fn actionable(&self) -> usize {
        self.new_count + self.replace_count + self.quarantine_count + self.new_folders + self.old_folders
    }
    pub fn permitted(&self, pair: &Pair) -> bool {
        !self.space_blocked
            && (self.mode != Mode::Mirror
                || (pair.source.warnings.is_empty() && pair.destination.warnings.is_empty()))
    }
}

#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub side: &'static str,
    pub current: String,
    pub processed_files: usize,
    pub hashed_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct CopyProgress {
    pub current: String,
    pub completed: usize,
    pub total: usize,
    pub written: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunSummary {
    pub created: usize,
    pub replaced: usize,
    pub moved_to_recovery: usize,
    pub copied_bytes: u64,
    pub failed: Vec<String>,
    pub cancelled: bool,
    pub recovery_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileComparison {
    pub left: PathBuf,
    pub right: PathBuf,
    pub left_size: u64,
    pub right_size: u64,
    pub left_hash: String,
    pub right_hash: String,
    pub identical: bool,
    pub detail: String,
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Cancelled by user".into())
    } else {
        Ok(())
    }
}

fn digest_file(path: &Path, cancel: &AtomicBool) -> Result<String, String> {
    digest_file_with_progress(path, cancel, |_| {})
}

fn digest_file_with_progress(
    path: &Path,
    cancel: &AtomicBool,
    mut on_read: impl FnMut(u64),
) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if count == 0 { break; }
        hasher.update(&buffer[..count]);
        on_read(count as u64);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn ignored_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == APP_RECOVERY || name.starts_with(TMP_PREFIX))
}

/// Reject overlapping roots; also resolves source/destination symlink root aliases.
pub fn validate_roots(source: &Path, target: &Path) -> Result<(PathBuf, PathBuf), String> {
    let source = source
        .canonicalize()
        .map_err(|e| format!("Source path: {e}"))?;
    let target = target
        .canonicalize()
        .map_err(|e| format!("Destination path: {e}"))?;
    if !source.is_dir() || !target.is_dir() {
        return Err("Both selected paths must be existing folders or mounted drives".into());
    }
    if source.starts_with(&target) || target.starts_with(&source) {
        return Err("Source and destination must not overlap or contain each other".into());
    }
    Ok((source, target))
}

/// Phase 1: fast metadata walk. No file contents are read here.
/// There can be millions of files, so only emit a UI update at most every
/// ~200 ms and every 1,024 entries; do not queue an event for every file.
fn scan_one(
    root: PathBuf,
    side: &'static str,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ScanProgress),
) -> Result<Inventory, String> {
    let mut result = Inventory {
        root: root.clone(), files: Vec::new(), folders: Vec::new(),
        total_bytes: 0, warnings: Vec::new(),
    };
    let mut last_notice = Instant::now();
    for item in WalkDir::new(&root).follow_links(false).into_iter().filter_entry(|entry| {
        entry.depth() == 0 || !ignored_entry(entry.path())
    }) {
        cancelled(cancel)?;
        let item = match item {
            Ok(entry) => entry,
            Err(error) => {
                result.warnings.push(format!("Unreadable entry: {error}"));
                continue;
            }
        };
        if item.depth() == 0 { continue; }
        let relative = match item.path().strip_prefix(&root) {
            Ok(rel) => rel.to_path_buf(), Err(_) => continue,
        };
        if item.file_type().is_symlink() {
            result.warnings.push(format!("Symbolic link skipped: {}", relative.display()));
            continue;
        }
        if item.file_type().is_dir() {
            result.folders.push(relative);
            continue;
        }
        if !item.file_type().is_file() {
            result.warnings.push(format!("Special file skipped: {}", relative.display()));
            continue;
        }
        let meta = match item.metadata() {
            Ok(m) => m,
            Err(error) => {
                result.warnings.push(format!("{}: {error}", relative.display()));
                continue;
            }
        };
        result.total_bytes = result.total_bytes.saturating_add(meta.len());
        result.files.push(FileRecord {
            relative, size: meta.len(), digest: String::new(), modified: meta.modified().ok(),
        });
        if result.files.len() % 1024 == 0 && last_notice.elapsed() >= Duration::from_millis(200) {
            last_notice = Instant::now();
            progress(ScanProgress {
                side, current: "Listing files".into(),
                processed_files: result.files.len(), hashed_bytes: 0,
            });
        }
    }
    result.files.sort_unstable_by(|a, b| a.relative.cmp(&b.relative));
    result.folders.sort_unstable();
    progress(ScanProgress {
        side, current: "Listing complete".into(),
        processed_files: result.files.len(), hashed_bytes: 0,
    });
    Ok(result)
}

/// Phase 2: hash only ambiguous files in Smart mode. Hashes remain empty for
/// metadata-only matches and files that cannot possibly match by size.
/// Limit reading to a single sequential stream per location; many parallel
/// seeks can make spinning disks and external USB drives much slower.
fn hash_selected(
    inventory: &mut Inventory,
    selected: &[bool],
    side: &'static str,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(ScanProgress),
) -> Result<(), String> {
    let mut hashed_bytes = 0u64;
    if !selected.iter().any(|wanted| *wanted) {
        progress(ScanProgress {
            side, current: "Complete".into(),
            processed_files: inventory.files.len(), hashed_bytes: 0,
        });
        return Ok(());
    }
    let mut last_notice = Instant::now();
    let mut bad = vec![false; inventory.files.len()];
    for (index, (file, &wanted)) in inventory.files.iter_mut().zip(selected).enumerate() {
        if !wanted { continue; }
        cancelled(cancel)?;
        let relative = file.relative.clone();
        let absolute = inventory.root.join(&relative);
        let before = fs::symlink_metadata(&absolute)
            .map_err(|e| format!("{}: {e}", absolute.display()));
        let hash = match before {
            Ok(meta) if meta.file_type().is_file()
                && meta.len() == file.size && meta.modified().ok() == file.modified => {
                    digest_file_with_progress(&absolute, cancel, |bytes| {
                        hashed_bytes = hashed_bytes.saturating_add(bytes);
                        if last_notice.elapsed() >= Duration::from_millis(250) {
                            last_notice = Instant::now();
                            progress(ScanProgress {
                                side, current: relative.display().to_string(),
                                processed_files: index, hashed_bytes,
                            });
                        }
                    })
                }
            Ok(_) => Err(format!("File changed while scanning: {}", absolute.display())),
            Err(error) => Err(error),
        };
        match hash {
            Ok(digest) => {
                let after = fs::symlink_metadata(&absolute);
                let unchanged = after.ok().is_some_and(|meta|
                    meta.file_type().is_file() && meta.len() == file.size
                        && meta.modified().ok() == file.modified);
                if unchanged { file.digest = digest; }
                else {
                    bad[index] = true;
                    inventory.warnings.push(format!("File changed while hashing: {}", absolute.display()));
                }
            }
            Err(error) => {
                cancelled(cancel)?;
                bad[index] = true;
                inventory.warnings.push(error);
            }
        }
    }
    if bad.iter().any(|bad| *bad) {
        inventory.files = inventory.files.drain(..).zip(bad)
            .filter_map(|(file, bad)| (!bad).then_some(file)).collect();
    }
    progress(ScanProgress {
        side, current: "Complete".into(),
        processed_files: inventory.files.len(), hashed_bytes,
    });
    Ok(())
}

/// Metadata-first comparison. For potential name-independent matches, hash
/// *only* file size groups present on both sides. Changed same-path files
/// with equal size also get a strong content comparison. In Verified mode,
/// hash the entire contents of both locations (slower but exact).
pub fn scan_pair_with_mode(
    source: &Path,
    destination: &Path,
    mode: ScanMode,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
) -> Result<Pair, String> {
    let (source, destination) = validate_roots(source, destination)?;
    let mut source = scan_one(source, "source", cancel, &mut progress)?;
    let mut destination = scan_one(destination, "destination", cancel, &mut progress)?;
    let mut hash_src = vec![mode == ScanMode::Verified; source.files.len()];
    let mut hash_dst = vec![mode == ScanMode::Verified; destination.files.len()];
    if mode == ScanMode::Smart {
        let target_paths: HashMap<&Path, usize> = destination.files.iter().enumerate()
            .map(|(i, file)| (file.relative.as_path(), i)).collect();
        // Build size buckets only for sources missing at the same path. This
        // avoids millions of tiny Vec allocations for unrelated target files.
        let wanted_sizes: HashSet<u64> = source.files.iter()
            .filter(|file| !target_paths.contains_key(file.relative.as_path()))
            .map(|file| file.size).collect();
        let target_sizes: HashMap<u64, Vec<usize>> = {
            let mut groups = HashMap::<u64, Vec<usize>>::new();
            for (i, file) in destination.files.iter().enumerate() {
                if wanted_sizes.contains(&file.size) {
                    groups.entry(file.size).or_default().push(i);
                }
            }
            groups
        };
        for (index, file) in source.files.iter().enumerate() {
            if let Some(&other_index) = target_paths.get(file.relative.as_path()) {
                let target = &destination.files[other_index];
                if target.size == file.size
                    && (file.modified.is_none() || target.modified.is_none()
                        || file.modified != target.modified) {
                    hash_src[index] = true;
                    hash_dst[other_index] = true;
                }
            } else if let Some(candidates) = target_sizes.get(&file.size) {
                // A file with a new name may already be on the destination.
                hash_src[index] = true;
                for &other_index in candidates { hash_dst[other_index] = true; }
            }
        }
    }
    hash_selected(&mut source, &hash_src, "source", cancel, &mut progress)?;
    hash_selected(&mut destination, &hash_dst, "destination", cancel, &mut progress)?;
    Ok(Pair { source, destination, scan_mode: mode })
}

pub fn scan_pair(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ScanProgress),
) -> Result<Pair, String> {
    scan_pair_with_mode(source, destination, ScanMode::Smart, cancel, progress)
}

pub fn available_space(path: &Path) -> Option<u64> {
    fs4::available_space(path).ok()
}

/// Build an entirely read-only preview. Partial mode packs small files first so
/// it transfers the largest number of complete files within the free-space budget.
pub fn plan(pair: &Pair, mode: Mode, skip_matching_content: bool) -> Plan {
    plan_with_space(pair, mode, skip_matching_content, available_space(&pair.destination.root))
}

fn plan_with_space(pair: &Pair, mode: Mode, skip_matching_content: bool, available: Option<u64>) -> Plan {
    let destination: HashMap<&Path, &FileRecord> = pair.destination.files.iter()
        .map(|f| (f.relative.as_path(), f)).collect();
    let source_paths: HashSet<&Path> = pair.source.files.iter().map(|f| f.relative.as_path()).collect();
    let mut by_hash: HashMap<&str, &Path> = HashMap::new();
    for file in &pair.destination.files {
        if !file.digest.is_empty() {
            by_hash.entry(&file.digest).or_insert(file.relative.as_path());
        }
    }
    let mut entries = Vec::with_capacity(pair.source.files.len() + pair.destination.files.len());
    for source in &pair.source.files {
        let existing = destination.get(source.relative.as_path()).copied();
        let match_elsewhere = (!source.digest.is_empty()).then(||
            by_hash.get(source.digest.as_str()).copied()).flatten()
            .filter(|path| *path != source.relative.as_path());
        let metadata_match = existing.is_some_and(|target|
            source.size == target.size && source.modified.is_some()
                && source.modified == target.modified
                && (source.digest.is_empty() || target.digest.is_empty()));
        let action = if let Some(target) = existing {
            if metadata_match || (!source.digest.is_empty() && source.digest == target.digest) {
                Action::Identical
            } else { Action::Replace }
        } else if mode != Mode::Mirror && skip_matching_content && match_elsewhere.is_some() {
            Action::FoundElsewhere
        } else {
            Action::CopyNew
        };
        entries.push(PlanEntry {
            relative: source.relative.clone(),
            size: source.size,
            action,
            existing_match: match_elsewhere.map(Path::to_path_buf),
            source_digest: (!source.digest.is_empty()).then(|| source.digest.clone()),
            target_digest: existing.and_then(|f|
                (!f.digest.is_empty()).then(|| f.digest.clone())),
            target_size: existing.map(|f| f.size),
            source_modified: source.modified,
            target_modified: existing.and_then(|f| f.modified),
        });
    }
    for target in &pair.destination.files {
        if !source_paths.contains(target.relative.as_path()) {
            entries.push(PlanEntry {
                relative: target.relative.clone(),
                size: target.size,
                action: if mode == Mode::Mirror { Action::Quarantine } else { Action::KeepTarget },
                existing_match: None,
                source_digest: None,
                target_digest: (!target.digest.is_empty()).then(|| target.digest.clone()),
                target_size: Some(target.size),
                source_modified: None,
                target_modified: target.modified,
            });
        }
    }
    let source_dirs: HashSet<&Path> = pair.source.folders.iter().map(PathBuf::as_path).collect();
    let target_dirs: HashSet<&Path> = pair.destination.folders.iter().map(PathBuf::as_path).collect();
    let new_folders = source_dirs.difference(&target_dirs).count();
    let old_folders = if mode == Mode::Mirror {
        target_dirs.difference(&source_dirs).count()
    } else { 0 };
    let capacity = available.map(|bytes| bytes.saturating_sub(SAFETY_MARGIN));
    let required = entries.iter().filter(|e| matches!(e.action, Action::CopyNew | Action::Replace))
        .fold(0u64, |acc, entry| acc.saturating_add(entry.size));
    let mut bytes_not_fitting = 0u64;
    if mode == Mode::Partial {
        if let Some(mut room) = capacity {
            let mut candidates: Vec<usize> = (0..entries.len())
                .filter(|&index| matches!(entries[index].action, Action::CopyNew | Action::Replace))
                .collect();
            candidates.sort_by_key(|&index| (entries[index].size, entries[index].relative.clone()));
            for index in candidates {
                if entries[index].size > room {
                    entries[index].action = Action::NoSpace;
                    bytes_not_fitting = bytes_not_fitting.saturating_add(entries[index].size);
                } else {
                    room -= entries[index].size;
                }
            }
        }
    }
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    let count = |action: Action| entries.iter().filter(|e| e.action == action).count();
    let metadata_matches = entries.iter().filter(|e| e.action == Action::Identical
        && (e.source_digest.is_none() || e.target_digest.is_none())).count();
    let bytes_to_copy = entries.iter()
        .filter(|e| matches!(e.action, Action::CopyNew | Action::Replace))
        .fold(0u64, |acc, entry| acc.saturating_add(entry.size));
    Plan {
        mode,
        new_count: count(Action::CopyNew),
        replace_count: count(Action::Replace),
        same_count: count(Action::Identical),
        metadata_matches,
        matching_elsewhere: count(Action::FoundElsewhere),
        target_only: count(Action::KeepTarget) + count(Action::Quarantine),
        quarantine_count: count(Action::Quarantine),
        new_folders,
        old_folders,
        no_space_count: count(Action::NoSpace),
        entries,
        bytes_to_copy,
        bytes_not_fitting,
        required_bytes_before_partial: required,
        available_bytes: available,
        space_blocked: mode != Mode::Partial && capacity.is_some_and(|room| required > room),
    }
}

/// Ensure every component of a destination path is a real directory, never a
/// symlink/junction. Existing paths are inspected again immediately before use.
fn make_safe_parent(root: &Path, relative_parent: &Path) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for part in relative_parent.components() {
        let Component::Normal(name) = part else {
            return Err(format!("Unsafe relative path: {}", relative_parent.display()));
        };
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!("Destination component is not a normal folder: {}", path.display()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(path)
}

fn safe_file_path(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    let Some(parent) = rel.parent() else { return Err("Missing file parent".into()); };
    let Some(file_name) = rel.file_name() else { return Err("Missing filename".into()); };
    let parent = make_safe_parent(root, parent)?;
    Ok(parent.join(file_name))
}

/// Check the source path's existing ancestry instead of following a substituted
/// symlink. Unlike destination parents, source paths are never created.
fn safe_source_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let mut candidate = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(component) = part else {
            return Err(format!("Unsafe source relative path: {}", relative.display()));
        };
        candidate.push(component);
        let meta = fs::symlink_metadata(&candidate)
            .map_err(|e| format!("{}: {e}", candidate.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("Symlink appeared in source path: {}", candidate.display()));
        }
    }
    Ok(candidate)
}

/// Protect an existing destination file from changes between planning and
/// execution. If scanned with Smart mode, check its size/time snapshot; if a
/// digest was calculated, confirm that too. This is not filesystem snapshotting.
fn verify_target(path: &Path, entry: &PlanEntry, cancel: &AtomicBool) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_file() || Some(meta.len()) != entry.target_size
        || meta.modified().ok() != entry.target_modified {
        return Err(format!("Destination changed since scan: {}", path.display()));
    }
    if let Some(expected_digest) = entry.target_digest.as_deref() {
        verify_file(path, expected_digest, cancel)?;
    }
    Ok(())
}

fn verify_file(path: &Path, expected_digest: &str, cancel: &AtomicBool) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err(format!("Not an ordinary file: {}", path.display()));
    }
    if digest_file(path, cancel)? != expected_digest {
        return Err(format!("File changed since scan: {}", path.display()));
    }
    Ok(())
}

fn move_to_recovery(
    root: &Path,
    source: &Path,
    relative: &Path,
    recovery_run: &Path,
    category: &str,
) -> Result<PathBuf, String> {
    let relative_backup = Path::new(APP_RECOVERY)
        .join(recovery_run.file_name().ok_or("Invalid recovery run")?)
        .join(category)
        .join(relative);
    let backup = safe_file_path(root, &relative_backup)?;
    if backup.exists() {
        return Err(format!("Recovery name collision: {}", backup.display()));
    }
    fs::rename(source, &backup).map_err(|e| format!("Unable to preserve original {}: {e}", source.display()))?;
    Ok(backup)
}

/// Publish a verified staged file without ever replacing a newly appeared
/// destination path. A hard link is an atomic no-clobber create on filesystems
/// supporting it. The create_new fallback still refuses overwrites, though
/// it is not crash-atomic on filesystems without hard links.
fn install_no_clobber(staged: &Path, destination: &Path) -> Result<(), String> {
    match fs::hard_link(staged, destination) {
        Ok(()) => {
            let _ = fs::remove_file(staged);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(format!("Destination changed during commit: {}", destination.display()))
        }
        Err(_) => {
            let mut input = File::open(staged).map_err(|e| e.to_string())?;
            let mut output = OpenOptions::new().create_new(true).write(true).open(destination)
                .map_err(|e| format!("Destination unavailable {}: {e}", destination.display()))?;
            let installed = std::io::copy(&mut input, &mut output)
                .and_then(|_| output.sync_all())
                .map_err(|e| format!("Failed to install {}: {e}", destination.display()));
            if installed.is_err() {
                drop(output);
                let _ = fs::remove_file(destination);
            } else {
                let _ = fs::remove_file(staged);
            }
            installed.map(|_| ())
        }
    }
}

fn copy_checked(
    source_root: &Path,
    target_root: &Path,
    entry: &PlanEntry,
    index: usize,
    run_name: &str,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), String> {
    let src = safe_source_path(source_root, &entry.relative)?;
    let expected_src = entry.source_digest.as_deref();
    // Do not follow symlinks introduced after the scan.
    let src_meta = fs::symlink_metadata(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    if !src_meta.file_type().is_file() || src_meta.len() != entry.size
        || src_meta.modified().ok() != entry.source_modified {
        return Err(format!("Source file changed since scan: {}", src.display()));
    }
    let dst = safe_file_path(target_root, &entry.relative)?;
    if entry.action == Action::CopyNew {
        if fs::symlink_metadata(&dst).is_ok() {
            return Err(format!("Destination appeared after scan: {}", dst.display()));
        }
    } else if entry.action == Action::Replace {
        verify_target(&dst, entry, cancel)?;
    }
    cancelled(cancel)?;
    let stage_name = format!("{TMP_PREFIX}{run_name}-{index}");
    let staged = dst.with_file_name(stage_name);
    let transfer = (|| -> Result<(), String> {
        let mut input = File::open(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        let mut output = OpenOptions::new().create_new(true).write(true).open(&staged)
            .map_err(|e| format!("{}: {e}", staged.display()))?;
        let mut buffer = vec![0u8; CHUNK];
        let mut hasher = Hasher::new();
        let mut copied = 0u64;
        loop {
            cancelled(cancel)?;
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 { break; }
            output.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
            hasher.update(&buffer[..count]);
            copied += count as u64;
            on_bytes(count as u64);
        }
        let digest = hasher.finalize().to_hex().to_string();
        if copied != entry.size || expected_src.is_some_and(|expected| expected != digest.as_str()) {
            return Err(format!("Source content changed during copying: {}", src.display()));
        }
        // Hashing as we copy is only one read pass. With a metadata-only scan,
        // confirm the source size and timestamp stayed stable during that pass.
        let after = fs::symlink_metadata(&src).map_err(|e| e.to_string())?;
        if !after.file_type().is_file() || after.len() != entry.size
            || after.modified().ok() != entry.source_modified {
            return Err(format!("Source changed while copying: {}", src.display()));
        }
        output.sync_all().map_err(|e| format!("Unable to flush {}: {e}", staged.display()))?;
        // Revalidate destination just before replacement.
        if entry.action == Action::Replace {
            verify_target(&dst, entry, cancel)?;
            let old = move_to_recovery(target_root, &dst, &entry.relative,
                &Path::new(run_name), "replaced")?;
            if let Err(error) = install_no_clobber(&staged, &dst) {
                let rollback = fs::rename(&old, &dst);
                return Err(format!("Unable to finish replacement: {error}; rollback: {rollback:?}"));
            }
        } else {
            install_no_clobber(&staged, &dst)?;
        }
        Ok(())
    })();
    if transfer.is_err() {
        let _ = fs::remove_file(&staged);
    }
    transfer
}

/// Executes the preview as-is and refuses to delete permanent files.
/// Mirroring moves target-only files into .comparator-recovery/<run>/removed.
pub fn execute(
    pair: &Pair,
    plan: &Plan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(CopyProgress),
) -> RunSummary {
    let mut result = RunSummary::default();
    if !plan.permitted(pair) {
        result.failed.push("Plan blocked: free space is insufficient, or mirror scan is incomplete".into());
        return result;
    }
    match validate_roots(&pair.source.root, &pair.destination.root) {
        Ok((source, target)) if source == pair.source.root && target == pair.destination.root => {}
        _ => {
            result.failed.push("The selected roots are no longer safe".into());
            return result;
        }
    }
    let run_name = format!("run-{}-{}", SystemTime::now().duration_since(UNIX_EPOCH)
        .unwrap_or_default().as_nanos(), std::process::id());
    let recovery = pair.destination.root.join(APP_RECOVERY).join(&run_name);
    let total = plan.new_count + plan.replace_count + plan.quarantine_count;
    let mut completed = 0;
    let mut copied_by_progress = 0u64;
    let mut last_transfer_notice = 0u64;
    for dir in &pair.source.folders {
        if cancel.load(Ordering::Relaxed) { result.cancelled = true; break; }
        if let Err(e) = make_safe_parent(&pair.destination.root, dir) {
            result.failed.push(format!("Unable to prepare folder {}: {e}", dir.display()));
        }
    }
    if !result.cancelled {
        for (index, entry) in plan.entries.iter().enumerate() {
            if !matches!(entry.action, Action::CopyNew | Action::Replace) { continue; }
            if cancel.load(Ordering::Relaxed) { result.cancelled = true; break; }
            progress(CopyProgress {
                current: entry.relative.display().to_string(),
                completed,
                total,
                written: copied_by_progress,
                total_bytes: plan.bytes_to_copy,
            });
            let transfer = copy_checked(&pair.source.root, &pair.destination.root, entry, index,
                &run_name, cancel, |bytes| {
                    copied_by_progress = copied_by_progress.saturating_add(bytes);
                    if copied_by_progress.saturating_sub(last_transfer_notice) >= 16 * 1024 * 1024 {
                        last_transfer_notice = copied_by_progress;
                        progress(CopyProgress {
                            current: entry.relative.display().to_string(), completed, total,
                            written: copied_by_progress, total_bytes: plan.bytes_to_copy,
                        });
                    }
                });
            match transfer {
                Ok(()) => {
                    result.copied_bytes = result.copied_bytes.saturating_add(entry.size);
                    match entry.action {
                        Action::CopyNew => result.created += 1,
                        Action::Replace => result.replaced += 1,
                        _ => {}
                    }
                }
                Err(e) => {
                    if cancel.load(Ordering::Relaxed) { result.cancelled = true; break; }
                    result.failed.push(e);
                }
            }
            completed += 1;
        }
    }
    // Extra files are only quarantined after a completely successful copy phase.
    if plan.mode == Mode::Mirror && result.failed.is_empty() && !result.cancelled {
        for entry in &plan.entries {
            if entry.action != Action::Quarantine { continue; }
            if cancel.load(Ordering::Relaxed) { result.cancelled = true; break; }
            let path = match safe_file_path(&pair.destination.root, &entry.relative) {
                Ok(p) => p,
                Err(e) => { result.failed.push(e); continue; }
            };
            let moved = verify_target(&path, entry, cancel).and_then(|_| {
                move_to_recovery(&pair.destination.root, &path, &entry.relative,
                    &Path::new(&run_name), "removed").map(|_| ())
            });
            match moved {
                Ok(()) => result.moved_to_recovery += 1,
                Err(e) => result.failed.push(e),
            }
            completed += 1;
            progress(CopyProgress {
                current: entry.relative.display().to_string(),
                completed,
                total,
                written: copied_by_progress,
                total_bytes: plan.bytes_to_copy,
            });
        }
        // Remove only empty directories that do not exist in the source; never
        // touch any file or anything inside the recovery directory.
        if result.failed.is_empty() && !result.cancelled {
            let sources: HashSet<&Path> = pair.source.folders.iter().map(PathBuf::as_path).collect();
            let mut target_dirs = pair.destination.folders.clone();
            target_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            for dir in target_dirs {
                if !sources.contains(dir.as_path()) {
                    if let Ok(path) = safe_file_path(&pair.destination.root, &dir.join(".probe")) {
                        if let Some(parent) = path.parent() { let _ = fs::remove_dir(parent); }
                    }
                }
            }
        }
    }
    progress(CopyProgress { current: "Complete".into(), completed, total,
        written: copied_by_progress, total_bytes: plan.bytes_to_copy });
    if recovery.exists() {
        result.recovery_path = Some(recovery);
    }
    result
}

pub fn compare_files(left: &Path, right: &Path, cancel: &AtomicBool) -> Result<FileComparison, String> {
    let lm = fs::metadata(left).map_err(|e| format!("{}: {e}", left.display()))?;
    let rm = fs::metadata(right).map_err(|e| format!("{}: {e}", right.display()))?;
    if !lm.is_file() || !rm.is_file() { return Err("Select two normal files".into()); }
    let left_hash = digest_file(left, cancel)?;
    let right_hash = digest_file(right, cancel)?;
    let identical = left_hash == right_hash;
    let detail = if identical {
        "Exact content match (BLAKE3). Filenames do not matter.".into()
    } else if lm.len() <= 256 * 1024 && rm.len() <= 256 * 1024 {
        let a = fs::read(left).map_err(|e| e.to_string())?;
        let b = fs::read(right).map_err(|e| e.to_string())?;
        match (String::from_utf8(a), String::from_utf8(b)) {
            (Ok(a), Ok(b)) => {
                let diff = similar::TextDiff::from_lines(&a, &b)
                    .unified_diff().context_radius(3).header("source", "destination").to_string();
                diff.chars().take(40_000).collect()
            }
            _ => "Binary files differ. Compare the checksums and lengths below.".into(),
        }
    } else {
        "Files differ. Text diff is limited to files of 256 KiB or less; BLAKE3 checks all bytes.".into()
    };
    Ok(FileComparison { left: left.to_path_buf(), right: right.to_path_buf(),
        left_size: lm.len(), right_size: rm.len(), left_hash, right_hash, identical, detail })
}

pub fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = bytes as f64;
    let mut index = 0;
    while size >= 1024.0 && index + 1 < units.len() {
        size /= 1024.0;
        index += 1;
    }
    if index == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", units[index]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, name: &str, content: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fn setup() -> (tempfile::TempDir, tempfile::TempDir, AtomicBool) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), AtomicBool::new(false))
    }
    #[test]
    fn blocks_nested_roots() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        assert!(validate_roots(root.path(), &nested).is_err());
    }
    #[test]
    fn fast_scan_does_not_read_unchanged_same_path_files() {
        let (left, right, cancel) = setup();
        write(left.path(), "photo.bin", "large representative bytes");
        write(right.path(), "photo.bin", "large representative bytes");
        let stamp = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for file in [left.path().join("photo.bin"), right.path().join("photo.bin")] {
            OpenOptions::new().write(true).open(file).unwrap()
                .set_modified(stamp).unwrap();
        }
        let quick = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        assert!(quick.source.files[0].digest.is_empty());
        assert!(quick.destination.files[0].digest.is_empty());
        let preview = plan(&quick, Mode::Changes, true);
        assert_eq!(preview.same_count, 1);
        assert_eq!(preview.metadata_matches, 1);
        let exact = scan_pair_with_mode(left.path(), right.path(), ScanMode::Verified,
            &cancel, |_| {}).unwrap();
        assert!(!exact.source.files[0].digest.is_empty());
        assert_eq!(plan(&exact, Mode::Changes, true).metadata_matches, 0);
    }
    #[test]
    fn full_verification_finds_changes_with_same_size_and_date() {
        let (left, right, cancel) = setup();
        write(left.path(), "important.txt", "AAAAA");
        write(right.path(), "important.txt", "ZZZZZ");
        let stamp = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for file in [left.path().join("important.txt"), right.path().join("important.txt")] {
            OpenOptions::new().write(true).open(file).unwrap()
                .set_modified(stamp).unwrap();
        }
        let fast = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        assert_eq!(plan(&fast, Mode::Changes, false).metadata_matches, 1);
        let exact = scan_pair_with_mode(left.path(), right.path(), ScanMode::Verified,
            &cancel, |_| {}).unwrap();
        assert_eq!(plan(&exact, Mode::Changes, false).replace_count, 1);
    }
    #[test]
    fn files_with_unique_sizes_are_not_hashed_before_copy() {
        let (left, right, cancel) = setup();
        write(left.path(), "new.bin", "completely unique length");
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        assert!(pair.source.files[0].digest.is_empty());
        let preview = plan(&pair, Mode::Changes, true);
        assert_eq!(preview.new_count, 1);
        let result = execute(&pair, &preview, &cancel, |_| {});
        assert!(result.failed.is_empty(), "{:?}", result.failed);
        assert_eq!(fs::read(right.path().join("new.bin")).unwrap(), b"completely unique length");
    }
    #[test]
    fn renamed_content_is_detected_without_copying() {
        let (left, right, cancel) = setup();
        write(left.path(), "different/name.txt", "same bytes");
        write(right.path(), "another/path.txt", "same bytes");
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        let p = plan(&pair, Mode::Changes, true);
        assert_eq!(p.matching_elsewhere, 1);
        assert_eq!(p.new_count, 0);
        let mirror = plan(&pair, Mode::Mirror, true);
        assert_eq!(mirror.new_count, 1);
        assert_eq!(mirror.quarantine_count, 1);
    }
    #[test]
    fn copies_and_preserves_replacements_in_recovery() {
        let (left, right, cancel) = setup();
        write(left.path(), "keep.txt", "same");
        write(right.path(), "keep.txt", "same");
        write(left.path(), "folder/changed.txt", "new content");
        write(right.path(), "folder/changed.txt", "old content");
        write(left.path(), "folder/new.txt", "brand new");
        // Exact mode makes the test independent of filesystem timestamp resolution.
        let pair = scan_pair_with_mode(left.path(), right.path(), ScanMode::Verified,
            &cancel, |_| {}).unwrap();
        let p = plan(&pair, Mode::Changes, true);
        assert_eq!(p.same_count, 1);
        assert_eq!(p.new_count, 1);
        assert_eq!(p.replace_count, 1);
        let summary = execute(&pair, &p, &cancel, |_| {});
        assert!(summary.failed.is_empty(), "{:?}", summary.failed);
        assert_eq!(summary.created, 1);
        assert_eq!(summary.replaced, 1);
        assert_eq!(fs::read_to_string(right.path().join("folder/changed.txt")).unwrap(), "new content");
        let recovery = summary.recovery_path.unwrap();
        assert_eq!(fs::read_to_string(recovery.join("replaced/folder/changed.txt")).unwrap(), "old content");
    }
    #[test]
    fn mirror_quarantines_target_only() {
        let (left, right, cancel) = setup();
        write(left.path(), "source.txt", "yes");
        write(right.path(), "extra.txt", "no");
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        let p = plan(&pair, Mode::Mirror, false);
        let summary = execute(&pair, &p, &cancel, |_| {});
        assert!(summary.failed.is_empty(), "{:?}", summary.failed);
        assert!(!right.path().join("extra.txt").exists());
        assert_eq!(fs::read_to_string(summary.recovery_path.unwrap().join("removed/extra.txt")).unwrap(), "no");
    }
    #[test]
    fn partial_mode_skips_files_that_do_not_fit() {
        let (left, right, cancel) = setup();
        write(left.path(), "small_a.txt", "abc");
        write(left.path(), "small_b.txt", "xyz");
        write(left.path(), "big.txt", "1234567890");
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        let p = plan_with_space(&pair, Mode::Partial, false, Some(SAFETY_MARGIN + 6));
        assert_eq!(p.new_count, 2);
        assert_eq!(p.no_space_count, 1);
        assert_eq!(p.bytes_to_copy, 6);
        assert_eq!(p.bytes_not_fitting, 10);
        assert!(!p.space_blocked);
        let full = plan_with_space(&pair, Mode::Changes, false, Some(SAFETY_MARGIN + 6));
        assert!(full.space_blocked);
    }
    #[test]
    fn empty_folders_make_a_plan_actionable() {
        let (left, right, cancel) = setup();
        fs::create_dir_all(left.path().join("only-empty-folder")).unwrap();
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        let p = plan(&pair, Mode::Changes, false);
        assert_eq!(p.new_folders, 1);
        assert_eq!(p.actionable(), 1);
        let summary = execute(&pair, &p, &cancel, |_| {});
        assert!(summary.failed.is_empty());
        assert!(right.path().join("only-empty-folder").is_dir());
    }
    #[test]
    fn refuses_overwrite_when_target_changes_after_scan() {
        let (left, right, cancel) = setup();
        // Different sizes make this a replacement even when both mtimes match.
        write(left.path(), "x.txt", "new contents");
        write(right.path(), "x.txt", "old");
        let pair = scan_pair(left.path(), right.path(), &cancel, |_| {}).unwrap();
        let p = plan(&pair, Mode::Changes, false);
        assert_eq!(p.replace_count, 1);
        write(right.path(), "x.txt", "changed after scan");
        let summary = execute(&pair, &p, &cancel, |_| {});
        assert!(!summary.failed.is_empty());
        assert_eq!(fs::read_to_string(right.path().join("x.txt")).unwrap(), "changed after scan");
    }
}
