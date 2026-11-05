use std::{collections::HashSet, fs::{self, File}, io::{Read, Write}, path::{Component, Path, PathBuf}};
use serde::{Deserialize, Serialize};
use crate::{error::{Result, ReactorError, io_error}, operations, paths::ReactorPaths, permissions, store::{Store, SUPPORTED_SCHEMA_VERSION}};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry { path: String, bytes: u64, blake3: String }
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest { format: u32, schema: u32, created_at: String, files: Vec<Entry> }
#[derive(Serialize, Deserialize)]
struct RestoreJournal { source: PathBuf, manifest_hash: String, stage: PathBuf, archive: PathBuf, files: Vec<Entry> }

fn invalid(message: &str) -> ReactorError { ReactorError::InvalidInput(message.to_owned()) }

fn safe_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && value.len() <= 255 && !value.ends_with('.')
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
}

fn allowed(relative: &str) -> bool {
    if matches!(relative, "reactor.db" | "config.json") { return true; }
    let parts = relative.split('/').collect::<Vec<_>>();
    parts.len() == 3 && parts[0] == "logs" && safe_component(parts[1]) && safe_component(parts[2])
}

fn checked(root: &Path, relative: &str) -> Result<PathBuf> {
    if !allowed(relative) { return Err(invalid("backup contains an unsupported path")); }
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else { return Err(invalid("backup path escapes its directory")); };
        path.push(component);
        if fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?.file_type().is_symlink() {
            return Err(invalid("backup paths must not be symlinks"));
        }
    }
    Ok(path)
}

fn fingerprint(path: &Path, relative: String) -> Result<Entry> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(invalid("backup requires regular files")); }
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    let mut hash = blake3::Hasher::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| io_error(path, error))?;
        if read == 0 { break; }
        count = count.saturating_add(read as u64); hash.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(|error| io_error(path, error))?;
    if count != metadata.len() || after.len() != metadata.len() || after.modified().ok() != metadata.modified().ok() {
        return Err(invalid("backup file changed while reading"));
    }
    Ok(Entry { path: relative, bytes: count, blake3: hash.finalize().to_hex().to_string() })
}

fn copy(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io_error(source, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(invalid("copy requires a regular source file")); }
    let parent = target.parent().ok_or_else(|| invalid("copy target has no parent"))?;
    permissions::create_private_dir(parent).map_err(|error| io_error(parent, error))?;
    let mut source_file = File::open(source).map_err(|error| io_error(source, error))?;
    let mut target_file = permissions::create_private_new_file(target).map_err(|error| io_error(target, error))?;
    let count = std::io::copy(&mut source_file, &mut target_file).map_err(|error| io_error(target, error))?;
    target_file.flush().and_then(|()| target_file.sync_all()).map_err(|error| io_error(target, error))?;
    let after = source_file.metadata().map_err(|error| io_error(source, error))?;
    if count != metadata.len() || after.len() != metadata.len() || after.modified().ok() != metadata.modified().ok() {
        return Err(invalid("source changed during backup"));
    }
    Ok(())
}

fn copy_logs(paths: &ReactorPaths, target: &Path, files: &mut Vec<Entry>) -> Result<()> {
    let root = paths.data_dir.join("runs");
    if !root.exists() { return Ok(()); }
    if fs::symlink_metadata(&root).map_err(|error| io_error(&root, error))?.file_type().is_symlink() { return Err(invalid("Run log root must not be a symlink")); }
    for run in fs::read_dir(&root).map_err(|error| io_error(&root, error))? {
        let run = run.map_err(|error| io_error(&root, error))?;
        if !run.file_type().map_err(|error| io_error(run.path(), error))?.is_dir() { continue; }
        let id = run.file_name().to_string_lossy().into_owned();
        if !safe_component(&id) { return Err(invalid("invalid log directory name")); }
        for file in fs::read_dir(run.path()).map_err(|error| io_error(run.path(), error))? {
            let file = file.map_err(|error| io_error(run.path(), error))?;
            let name = file.file_name().to_string_lossy().into_owned();
            if !safe_component(&name) { return Err(invalid("invalid log filename")); }
            if files.len() >= 100_000 { return Err(invalid("backup exceeds 100,000 files")); }
            let relative = format!("logs/{id}/{name}");
            let destination = target.join(&relative);
            copy(&file.path(), &destination)?;
            files.push(fingerprint(&destination, relative)?);
        }
    }
    Ok(())
}

fn require_offline(paths: &ReactorPaths) -> Result<operations::AccessGuard> {
    if !operations::maintenance(paths) || crate::daemon::is_running(paths)? {
        return Err(invalid("enter maintenance and stop the daemon before backup or restore"));
    }
    operations::AccessGuard::acquire(paths, true)
}

pub fn create(paths: &ReactorPaths, directory: &Path) -> Result<Manifest> {
    let _lock = require_offline(paths)?;
    let parent = dunce::canonicalize(directory.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."))).map_err(|error| io_error(directory, error))?;
    let name = directory.file_name().ok_or_else(|| invalid("backup needs a new directory name"))?;
    let directory = parent.join(name);
    for root in [&paths.data_dir, &paths.config_dir] {
        let root = dunce::canonicalize(root).map_err(|error| io_error(root, error))?;
        if directory.starts_with(root) { return Err(invalid("backup destination must be outside Reactor config/data storage")); }
    }
    let store = Store::open_initialized(&paths.database_file())?;
    if store.has_active_execution()? || !store.pending_deletions()?.is_empty() { return Err(invalid("finish execution, deletion and recovery before backup")); }
    crate::config::Config::load(&paths.config_file())?;
    fs::create_dir(&directory).map_err(|error| io_error(&directory, error))?;
    permissions::restrict_dir(&directory).map_err(|error| io_error(&directory, error))?;
    copy(&paths.config_file(), &directory.join("config.json"))?;
    store.backup_database(&directory.join("reactor.db"))?;
    let mut files = vec![fingerprint(&directory.join("config.json"), "config.json".to_owned())?, fingerprint(&directory.join("reactor.db"), "reactor.db".to_owned())?];
    copy_logs(paths, &directory, &mut files)?;
    let manifest = Manifest { format: 1, schema: SUPPORTED_SCHEMA_VERSION, created_at: crate::time::now_utc(), files };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| invalid(&error.to_string()))?;
    if bytes.len() > 16 * 1024 * 1024 { return Err(invalid("backup manifest exceeds 16 MiB")); }
    operations::atomic_write(&directory.join("manifest.json"), &bytes)?;
    Ok(manifest)
}

fn load(directory: &Path) -> Result<(Manifest, String)> {
    let manifest_path = directory.join("manifest.json");
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|error| io_error(&manifest_path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 * 1024 { return Err(invalid("invalid backup manifest")); }
    let bytes = fs::read(&manifest_path).map_err(|error| io_error(&manifest_path, error))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|error| invalid(&error.to_string()))?;
    if manifest.format != 1 || manifest.schema != SUPPORTED_SCHEMA_VERSION || manifest.files.len() > 100_000 {
        return Err(invalid("unsupported backup format or schema"));
    }
    let mut seen = HashSet::new();
    for entry in &manifest.files {
        if !seen.insert(entry.path.clone()) { return Err(invalid("duplicate backup path")); }
        let actual = fingerprint(&checked(directory, &entry.path)?, entry.path.clone())?;
        if actual.bytes != entry.bytes || actual.blake3 != entry.blake3 { return Err(invalid("backup checksum mismatch")); }
    }
    if !seen.contains("config.json") || !seen.contains("reactor.db") { return Err(invalid("backup config/database missing")); }
    Ok((manifest, blake3::hash(&bytes).to_hex().to_string()))
}

fn archive_current(paths: &ReactorPaths, archive: &Path) -> Result<()> {
    permissions::create_private_dir(archive).map_err(|error| io_error(archive, error))?;
    let mut files = Vec::new();
    for (source, name) in [(paths.config_file(), "config.json"), (paths.database_file(), "reactor.db"),
        (paths.data_dir.join("reactor.db-wal"), "reactor.db-wal"), (paths.data_dir.join("reactor.db-shm"), "reactor.db-shm")] {
        if source.exists() { copy(&source, &archive.join(name))?; files.push(fingerprint(&archive.join(name), name.to_owned())?); }
    }
    copy_logs(paths, archive, &mut files)?;
    operations::atomic_write(&archive.join("rollback.json"), &serde_json::to_vec(&files).map_err(|error| invalid(&error.to_string()))?)
}

pub fn restore(paths: &ReactorPaths, directory: &Path, replace: bool) -> Result<serde_json::Value> {
    if !replace { return Err(invalid("restore requires --replace; current data is archived before replacement")); }
    let _lock = require_offline(paths)?;
    let source = dunce::canonicalize(directory).map_err(|error| io_error(directory, error))?;
    let (manifest, manifest_hash) = load(&source)?;
    let journal_path = paths.data_dir.join("restore.json");
    let journal: RestoreJournal = if journal_path.exists() {
        let bytes = fs::read(&journal_path).map_err(|error| io_error(&journal_path, error))?;
        let journal: RestoreJournal = serde_json::from_slice(&bytes).map_err(|error| invalid(&error.to_string()))?;
        if journal.source != source || journal.manifest_hash != manifest_hash { return Err(invalid("resume restore using the original unchanged backup")); }
        let root = dunce::canonicalize(&paths.data_dir).map_err(|error| io_error(&paths.data_dir, error))?;
        if journal.stage.parent() != Some(root.as_path()) || !journal.stage.file_name().is_some_and(|name| name.to_string_lossy().starts_with(".restore-")) {
            return Err(invalid("invalid restore staging path"));
        }
        journal
    } else {
        if let Ok(store) = Store::open_initialized(&paths.database_file()) {
            if store.has_active_execution()? { return Err(invalid("finish active execution and recovery before restore")); }
        }
        let root = dunce::canonicalize(&paths.data_dir).map_err(|error| io_error(&paths.data_dir, error))?;
        let id = crate::ids::new_id();
        let stage = root.join(format!(".restore-{id}"));
        let archive = root.join(format!("pre-restore-{id}"));
        permissions::create_private_dir(&stage).map_err(|error| io_error(&stage, error))?;
        for entry in &manifest.files {
            let target = if entry.path == "reactor.db" { stage.join("input.db") } else { stage.join(&entry.path) };
            copy(&checked(&source, &entry.path)?, &target)?;
        }
        crate::config::Config::load(&stage.join("config.json"))?;
        {
            let mut database = Store::open_initialized(&stage.join("input.db"))?;
            database.prepare_restored_state()?;
            database.backup_database(&stage.join("reactor.db"))?;
        }
        let files = manifest.files.iter().map(|entry| fingerprint(&stage.join(&entry.path), entry.path.clone())).collect::<Result<Vec<_>>>()?;
        archive_current(paths, &archive)?;
        let journal = RestoreJournal { source, manifest_hash, stage, archive, files };
        operations::atomic_write(&journal_path, &serde_json::to_vec(&journal).map_err(|error| invalid(&error.to_string()))?)?;
        journal
    };
    for entry in &journal.files {
        let actual = fingerprint(&checked(&journal.stage, &entry.path)?, entry.path.clone())?;
        if actual.bytes != entry.bytes || actual.blake3 != entry.blake3 { return Err(invalid("restore staging checksum mismatch")); }
    }
    for suffix in ["reactor.db-wal", "reactor.db-shm"] {
        let path = paths.data_dir.join(suffix);
        match fs::remove_file(&path) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(io_error(path, error)) }
    }
    for entry in &journal.files {
        let target = match entry.path.as_str() {
            "config.json" => paths.config_file(),
            "reactor.db" => paths.database_file(),
            relative => {
                let tail = relative.strip_prefix("logs/").ok_or_else(|| invalid("invalid restore path"))?;
                paths.data_dir.join("runs").join(tail)
            }
        };
        let parent = target.parent().ok_or_else(|| invalid("restore target has no parent"))?;
        // Reject existing symlink ancestors before writing any restored files.
        let mut cursor = parent;
        while cursor.starts_with(&paths.data_dir) || cursor.starts_with(&paths.config_dir) {
            match fs::symlink_metadata(cursor) {
                Ok(metadata) if metadata.file_type().is_symlink() => return Err(invalid("restore destination contains a symlink")),
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(io_error(cursor, error)),
            }
            let Some(next) = cursor.parent() else { break; }; cursor = next;
        }
        permissions::create_private_dir(parent).map_err(|error| io_error(parent, error))?;
        let temporary = parent.join(format!(".restore-{}.tmp", crate::ids::new_id()));
        copy(&journal.stage.join(&entry.path), &temporary)?;
        let copied = fingerprint(&temporary, entry.path.clone())?;
        if copied.bytes != entry.bytes || copied.blake3 != entry.blake3 { return Err(invalid("restore copy checksum mismatch")); }
        operations::replace_file(&temporary, &target)?;
    }
    operations::set_maintenance(paths, true)?;
    fs::remove_file(&journal_path).map_err(|error| io_error(&journal_path, error))?;
    Ok(serde_json::json!({"restored": true, "maintenance": true, "rollback_archive": journal.archive,
        "workspace_preparation_required": true, "automatic_start": false}))
}
