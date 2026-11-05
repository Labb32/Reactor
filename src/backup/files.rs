use super::*;

pub(super) fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 255
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
}

pub(super) fn allowed(relative: &str) -> bool {
    if matches!(relative, "reactor.db" | "config.json") {
        return true;
    }
    let parts = relative.split('/').collect::<Vec<_>>();
    parts.len() == 3 && parts[0] == "logs" && safe_component(parts[1]) && safe_component(parts[2])
}

pub(super) fn checked(root: &Path, relative: &str) -> Result<PathBuf> {
    if !allowed(relative) {
        return Err(invalid("backup contains an unsupported path"));
    }
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(invalid("backup path escapes its directory"));
        };
        path.push(component);
        if fs::symlink_metadata(&path)
            .map_err(|error| io_error(&path, error))?
            .file_type()
            .is_symlink()
        {
            return Err(invalid("backup paths must not be symlinks"));
        }
    }
    Ok(path)
}

pub(super) fn fingerprint(path: &Path, relative: String) -> Result<Entry> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("backup requires regular files"));
    }
    let mut file = File::open(path).map_err(|error| io_error(path, error))?;
    let mut hash = blake3::Hasher::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error(path, error))?;
        if read == 0 {
            break;
        }
        count = count.saturating_add(read as u64);
        hash.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(|error| io_error(path, error))?;
    if count != metadata.len()
        || after.len() != metadata.len()
        || after.modified().ok() != metadata.modified().ok()
    {
        return Err(invalid("backup file changed while reading"));
    }
    Ok(Entry {
        path: relative,
        bytes: count,
        blake3: hash.finalize().to_hex().to_string(),
    })
}

pub(super) fn copy(source: &Path, target: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io_error(source, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("copy requires a regular source file"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| invalid("copy target has no parent"))?;
    permissions::create_private_dir(parent).map_err(|error| io_error(parent, error))?;
    let mut source_file = File::open(source).map_err(|error| io_error(source, error))?;
    let mut target_file =
        permissions::create_private_new_file(target).map_err(|error| io_error(target, error))?;
    let count = std::io::copy(&mut source_file, &mut target_file)
        .map_err(|error| io_error(target, error))?;
    target_file
        .flush()
        .and_then(|()| target_file.sync_all())
        .map_err(|error| io_error(target, error))?;
    let after = source_file
        .metadata()
        .map_err(|error| io_error(source, error))?;
    if count != metadata.len()
        || after.len() != metadata.len()
        || after.modified().ok() != metadata.modified().ok()
    {
        return Err(invalid("source changed during backup"));
    }
    Ok(())
}

pub(super) fn copy_logs(paths: &ReactorPaths, target: &Path, files: &mut Vec<Entry>) -> Result<()> {
    let root = paths.data_dir.join("runs");
    if !root.exists() {
        return Ok(());
    }
    if fs::symlink_metadata(&root)
        .map_err(|error| io_error(&root, error))?
        .file_type()
        .is_symlink()
    {
        return Err(invalid("Run log root must not be a symlink"));
    }
    for run in fs::read_dir(&root).map_err(|error| io_error(&root, error))? {
        let run = run.map_err(|error| io_error(&root, error))?;
        if !run
            .file_type()
            .map_err(|error| io_error(run.path(), error))?
            .is_dir()
        {
            continue;
        }
        let id = run.file_name().to_string_lossy().into_owned();
        if !safe_component(&id) {
            return Err(invalid("invalid log directory name"));
        }
        for file in fs::read_dir(run.path()).map_err(|error| io_error(run.path(), error))? {
            let file = file.map_err(|error| io_error(run.path(), error))?;
            let name = file.file_name().to_string_lossy().into_owned();
            if !safe_component(&name) {
                return Err(invalid("invalid log filename"));
            }
            if files.len() >= 100_000 {
                return Err(invalid("backup exceeds 100,000 files"));
            }
            let relative = format!("logs/{id}/{name}");
            let destination = target.join(&relative);
            copy(&file.path(), &destination)?;
            files.push(fingerprint(&destination, relative)?);
        }
    }
    Ok(())
}
