use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
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
    let manifest = capture(run_id, workspace_id, roots)?;
    write_json(&pending_path(paths, run_id), &manifest)
}

pub fn finish(paths: &ReactorPaths, run_id: &str, state: &str) -> Result<RunResult> {
    let pending = pending_path(paths, run_id);
    let before: Manifest = read_json(&pending)?;
    let roots = before
        .roots
        .iter()
        .map(|root| PathBuf::from(&root.path))
        .collect::<Vec<_>>();
    let mut after = capture(run_id, &before.workspace_id, &roots)?;
    let completed_at = crate::time::now_utc();
    let report = compare(&before, &after, state, &completed_at);
    after.remove_content();

    let manifest_path = paths
        .data_dir
        .join("manifests")
        .join(format!("{}.json", before.workspace_id));
    let diff_path = paths
        .data_dir
        .join("diffs")
        .join(format!("{}.json", before.workspace_id));
    write_json(&diff_path, &report)?;
    write_json(&manifest_path, &after)?;
    fs::remove_file(&pending).map_err(|source| io_error(&pending, source))?;

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

pub fn read_diff(result: &RunResult) -> Result<DiffReport> {
    read_json(Path::new(&result.diff_path))
}

pub fn pending_exists(paths: &ReactorPaths, run_id: &str) -> bool {
    pending_path(paths, run_id).is_file()
}

fn capture(run_id: &str, workspace_id: &str, roots: &[PathBuf]) -> Result<Manifest> {
    let mut captured = Vec::with_capacity(roots.len());
    for root in roots {
        let root = dunce::canonicalize(root).map_err(|source| io_error(root, source))?;
        let display = root
            .to_str()
            .ok_or_else(|| ReactorError::InvalidStoredData("root path is not UTF-8".to_owned()))?
            .to_owned();
        captured.push(ManifestRoot {
            path: display,
            files: capture_root(&root)?,
        });
    }
    Ok(Manifest {
        run_id: run_id.to_owned(),
        workspace_id: workspace_id.to_owned(),
        captured_at: crate::time::now_utc(),
        roots: captured,
    })
}

fn capture_root(root: &Path) -> Result<BTreeMap<String, FileEntry>> {
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
        let entry = entry.map_err(|error| ReactorError::InvalidInput(error.to_string()))?;
        if entry.path() == root {
            continue;
        }
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|source| io_error(entry.path(), source))?;
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
        let key = relative.to_string_lossy().replace('\\', "/");
        if files
            .insert(key.clone(), file_entry(entry.path(), &metadata)?)
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

fn file_entry(path: &Path, metadata: &fs::Metadata) -> Result<FileEntry> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| io_error(path, source))?;
        let value = target.to_string_lossy();
        return Ok(FileEntry {
            kind: "symlink".to_owned(),
            size: value.len() as u64,
            hash: blake3::hash(value.as_bytes()).to_hex().to_string(),
            content: None,
        });
    }
    if metadata.len() > TEXT_LIMIT {
        return hash_file(path, "large");
    }
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let size = bytes.len() as u64;
    match String::from_utf8(bytes) {
        Ok(content) if !content.contains('\0') => Ok(FileEntry {
            kind: "text".to_owned(),
            size,
            hash,
            content: Some(content),
        }),
        _ => Ok(FileEntry {
            kind: "binary".to_owned(),
            size,
            hash,
            content: None,
        }),
    }
}

fn hash_file(path: &Path, kind: &str) -> Result<FileEntry> {
    let mut file = fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
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
        changes,
    }
}

fn text_diff(path: &str, old: Option<&FileEntry>, new: Option<&FileEntry>) -> Option<String> {
    if old.is_some_and(|entry| entry.kind != "text")
        || new.is_some_and(|entry| entry.kind != "text")
    {
        return None;
    }
    let old = old.and_then(|entry| entry.content.as_deref());
    let new = new.and_then(|entry| entry.content.as_deref());
    if old.is_none() && new.is_none() {
        return None;
    }
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
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
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
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    drop(file);
    replace(&temporary, path)
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
