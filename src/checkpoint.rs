use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::{
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
    permissions,
    run::{FileChangeSummary, RunResult},
};

const TEXT_LIMIT: u64 = 1024 * 1024;
const MANIFEST_TEXT_LIMIT: u64 = 64 * 1024 * 1024;
const MANIFEST_FILE_LIMIT: usize = 250_000;
const ARTIFACT_LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    run_id: String,
    workspace_id: String,
    captured_at: String,
    roots: Vec<ManifestRoot>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ManifestRoot {
    path: String,
    files: BTreeMap<String, FileEntry>,
}

impl Manifest {
    fn remove_content(&mut self) {
        for root in &mut self.roots {
            for file in root.files.values_mut() {
                file.content = None;
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FileEntry {
    kind: String,
    size: u64,
    hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DiffReport {
    pub run_id: String,
    pub workspace_id: String,
    pub state: String,
    pub completed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<u64>,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FileChange {
    #[serde(flatten)]
    pub summary: FileChangeSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
}

pub fn begin(
    paths: &ReactorPaths,
    run_id: &str,
    workspace_id: &str,
    roots: &[PathBuf],
) -> Result<()> {
    if !crate::ids::is_safe_run_id(run_id) {
        return Err(ReactorError::InvalidStoredData(
            "checkpoint run ID is invalid".to_owned(),
        ));
    }
    crate::workspace::validate_id(workspace_id)?;
    let manifest = capture(run_id, workspace_id, roots)?;
    write_json(&pending_path(paths, run_id), &manifest)
}

pub fn finish(paths: &ReactorPaths, run_id: &str, state: &str) -> Result<RunResult> {
    validate_run_id(run_id)?;
    let pending = pending_path(paths, run_id);
    let before: Manifest = read_json(&pending)?;
    if before.run_id != run_id {
        return Err(ReactorError::InvalidStoredData(
            "checkpoint run ID does not match".to_owned(),
        ));
    }
    crate::workspace::validate_id(&before.workspace_id)?;
    let roots = before
        .roots
        .iter()
        .map(|root| PathBuf::from(&root.path))
        .collect::<Vec<_>>();
    let mut after = capture(run_id, &before.workspace_id, &roots)?;
    let completed_at = crate::time::now_utc();
    let mut report = compare(&before, &after, state, &completed_at);
    for change in &mut report.changes {
        if let Some(diff) = &mut change.unified_diff { *diff = crate::redact::text(diff); }
    }
    after.remove_content();

    let manifest_path = paths
        .data_dir
        .join("manifests")
        .join(format!("{run_id}.json"));
    let diff_path = paths
        .data_dir
        .join("diffs")
        .join(format!("{run_id}.json"));
    write_json(&diff_path, &report)?;
    write_json(&manifest_path, &after)?;

    Ok(RunResult {
        state: state.to_owned(),
        completed_at,
        exit_code: None,
        termination_reason: None,
        log_path: None,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        diff_path: diff_path.to_string_lossy().into_owned(),
        changed_files: report
            .changes
            .into_iter()
            .map(|change| change.summary)
            .collect(),
    })
}

pub fn complete(paths: &ReactorPaths, run_id: &str) -> Result<()> {
    validate_run_id(run_id)?;
    let pending = pending_path(paths, run_id);
    match fs::remove_file(&pending) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(pending, source)),
    }
}

pub fn read_diff(paths: &ReactorPaths, result: &RunResult) -> Result<DiffReport> {
    let path = Path::new(&result.diff_path);
    if path.parent() != Some(paths.data_dir.join("diffs").as_path()) {
        return Err(ReactorError::InvalidStoredData(
            "run diff path is outside the artifact directory".to_owned(),
        ));
    }
    read_json(path)
}

pub fn pending_exists(paths: &ReactorPaths, run_id: &str) -> bool {
    crate::ids::is_safe_run_id(run_id) && pending_path(paths, run_id).is_file()
}

pub fn remove_run_artifacts(paths: &ReactorPaths, run_id: &str) -> Result<()> {
    validate_run_id(run_id)?;
    for path in [pending_path(paths, run_id), paths.data_dir.join("manifests").join(format!("{run_id}.json")), paths.data_dir.join("diffs").join(format!("{run_id}.json"))] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => fs::remove_file(&path).map_err(|error| io_error(&path, error))?,
            Ok(_) => return Err(ReactorError::InvalidStoredData("artifact deletion requires a regular file".to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(io_error(&path, error)),
        }
    }
    Ok(())
}

pub fn pending_run_ids(paths: &ReactorPaths) -> Result<Vec<String>> {
    let directory = paths.data_dir.join("manifests").join("pending");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(directory, source)),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&directory, source))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if crate::ids::is_safe_run_id(id) {
            ids.push(id.to_owned());
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

fn validate_run_id(run_id: &str) -> Result<()> {
    if crate::ids::is_safe_run_id(run_id) {
        Ok(())
    } else {
        Err(ReactorError::InvalidStoredData("checkpoint run ID is invalid".to_owned()))
    }
}

fn capture(run_id: &str, workspace_id: &str, roots: &[PathBuf]) -> Result<Manifest> {
    let mut captured = Vec::with_capacity(roots.len());
    let mut remaining_text = MANIFEST_TEXT_LIMIT;
    let mut file_count = 0_usize;
    for root in roots {
        crate::execution_control::check().map_err(|source| io_error(root, source))?;
        let canonical = dunce::canonicalize(root).map_err(|source| io_error(root, source))?;
        if canonical != *root {
            return Err(ReactorError::InvalidStoredData(format!(
                "checkpoint root changed: {}",
                root.display()
            )));
        }
        let root = canonical;
        let display = root
            .to_str()
            .ok_or_else(|| ReactorError::InvalidStoredData("root path is not UTF-8".to_owned()))?
            .to_owned();
        captured.push(ManifestRoot {
            path: display,
            files: capture_root(&root, &mut remaining_text, &mut file_count)?,
        });
    }
    Ok(Manifest {
        run_id: run_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        captured_at: crate::time::now_utc(),
        roots: captured,
    })
}

fn capture_root(
    root: &Path,
    remaining_text: &mut u64,
    file_count: &mut usize,
) -> Result<BTreeMap<String, FileEntry>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .parents(true)
        .add_custom_ignore_filename(".reactorignore");
    let filter_root = root.to_owned();
    builder.filter_entry(move |entry| !excluded(&filter_root, entry.path()));
    let mut files = BTreeMap::new();
    for entry in builder.build() {
        crate::execution_control::check().map_err(|source| io_error(root, source))?;
        let entry = entry.map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        if entry.path() == root {
            continue;
        }
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|source| io_error(entry.path(), source))?;
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            continue;
        }
        *file_count += 1;
        if *file_count > MANIFEST_FILE_LIMIT {
            return Err(ReactorError::InvalidInput(format!(
                "checkpoint exceeds {MANIFEST_FILE_LIMIT} files"
            )));
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        let key = relative.to_string_lossy().replace('\\', "/");
        if files
            .insert(
                key.clone(),
                file_entry(entry.path(), &metadata, remaining_text)?,
            )
            .is_some()
        {
            return Err(ReactorError::InvalidStoredData(format!(
                "duplicate checkpoint path: {key}"
            )));
        }
    }
    Ok(files)
}

fn excluded(root: &Path, path: &Path) -> bool {
    if path == root {
        return false;
    }
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(|component| {
            let value = component.as_os_str();
            value == ".git" || value == ".reactor"
        })
    })
}

fn file_entry(path: &Path, metadata: &fs::Metadata, remaining_text: &mut u64) -> Result<FileEntry> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| io_error(path, source))?;
        let value = os_bytes(target.as_os_str());
        return Ok(FileEntry {
            kind: "symlink".to_owned(),
            size: value.len() as u64,
            hash: blake3::hash(&value).to_hex().to_string(),
            content: None,
        });
    }
    let mut file = open_regular(path)?;
    let opened_metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !opened_metadata.is_file() || opened_metadata.file_type().is_symlink() {
        return Err(ReactorError::InvalidInput(format!(
            "checkpoint file changed type: {}",
            path.display()
        )));
    }
    if opened_metadata.len() > TEXT_LIMIT {
        return hash_file(file, path, "large");
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    (&mut file)
        .take(TEXT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > TEXT_LIMIT {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| io_error(path, source))?;
        return hash_file(file, path, "large");
    }
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let size = bytes.len() as u64;
    match String::from_utf8(bytes) {
        Ok(content) if !content.contains('\0') => {
            let stored = if size <= *remaining_text {
                *remaining_text -= size;
                Some(content)
            } else {
                None
            };
            Ok(FileEntry {
                kind: "text".to_owned(),
                size,
                hash,
                content: stored,
            })
        }
        _ => Ok(FileEntry {
            kind: "binary".to_owned(),
            size,
            hash,
            content: None,
        }),
    }
}

fn hash_file(mut file: fs::File, path: &Path, kind: &str) -> Result<FileEntry> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        crate::execution_control::check().map_err(|source| io_error(path, source))?;
        let count = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hasher.update(&buffer[..count]);
    }
    Ok(FileEntry {
        kind: kind.to_owned(),
        size,
        hash: hasher.finalize().to_hex().to_string(),
        content: None,
    })
}

fn open_regular(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path).map_err(|source| io_error(path, source))
}

#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

fn compare(before: &Manifest, after: &Manifest, state: &str, completed_at: &str) -> DiffReport {
    let mut changes = Vec::new();
    for (before_root, after_root) in before.roots.iter().zip(&after.roots) {
        let paths = before_root
            .files
            .keys()
            .chain(after_root.files.keys())
            .collect::<BTreeSet<_>>();
        for path in paths {
            let old = before_root.files.get(path);
            let new = after_root.files.get(path);
            if old.map(|entry| (&entry.kind, &entry.hash, entry.size))
                == new.map(|entry| (&entry.kind, &entry.hash, entry.size))
            {
                continue;
            }
            let change = match (old, new) {
                (None, Some(_)) => "added",
                (Some(_), None) => "deleted",
                _ => "modified",
            };
            let kind = new
                .or(old)
                .map(|entry| entry.kind.clone())
                .unwrap_or_default();
            let unified_diff = text_diff(path, old, new);
            changes.push(FileChange {
                summary: FileChangeSummary {
                    root: before_root.path.clone(),
                    path: path.clone(),
                    change: change.to_owned(),
                    kind,
                    before_size: old.map(|entry| entry.size),
                    after_size: new.map(|entry| entry.size),
                    before_hash: old.map(|entry| entry.hash.clone()),
                    after_hash: new.map(|entry| entry.hash.clone()),
                },
                unified_diff,
            });
        }
    }
    DiffReport {
        run_id: before.run_id.clone(),
        workspace_id: before.workspace_id.clone(),
        state: state.to_owned(),
        completed_at: completed_at.to_owned(),
        token_usage: None,
        changes,
    }
}

fn text_diff(path: &str, old: Option<&FileEntry>, new: Option<&FileEntry>) -> Option<String> {
    if old.is_some_and(|entry| entry.kind != "text")
        || new.is_some_and(|entry| entry.kind != "text")
        || old.is_some_and(|entry| entry.content.is_none())
        || new.is_some_and(|entry| entry.content.is_none())
    {
        return None;
    }
    let old = old.and_then(|entry| entry.content.as_deref());
    let new = new.and_then(|entry| entry.content.as_deref());
    if old.is_none() && new.is_none() {
        return None;
    }
    let path = crate::output::escape_control(path);
    Some(
        TextDiff::from_lines(old.unwrap_or_default(), new.unwrap_or_default())
            .unified_diff()
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string(),
    )
}

fn pending_path(paths: &ReactorPaths, run_id: &str) -> PathBuf {
    paths
        .data_dir
        .join("manifests")
        .join("pending")
        .join(format!("{run_id}.json"))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(ARTIFACT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > ARTIFACT_LIMIT {
        return Err(ReactorError::InvalidStoredData(format!(
            "artifact exceeds {} MiB",
            ARTIFACT_LIMIT / 1024 / 1024
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ReactorError::OutputSerialization(error.to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| ReactorError::InvalidStoredData("artifact path has no parent".to_owned()))?;
    permissions::create_private_dir(parent).map_err(|source| io_error(parent, source))?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        crate::ids::new_id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut file = permissions::create_private_new_file(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    if let Err(source) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, source));
    }
    drop(file);
    if let Err(error) = replace(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(temporary: &Path, target: &Path) -> Result<()> {
    fs::rename(temporary, target).map_err(|source| io_error(target, source))
}

#[cfg(windows)]
fn replace(temporary: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: Both null-terminated buffers remain valid for the call.
    let moved = unsafe {
        MoveFileExW(
            temporary_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io_error(target, std::io::Error::last_os_error()));
    }
    Ok(())
}
