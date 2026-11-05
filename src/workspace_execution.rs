use crate::{
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
    permissions,
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

pub struct WorkspaceLocks(Vec<File>);

impl WorkspaceLocks {
    pub fn acquire(roots: &[PathBuf]) -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or(ReactorError::UserDirectoriesUnavailable)?;
        let directory = base
            .data_local_dir()
            .join("reactor")
            .join("execution-locks");
        permissions::create_private_dir(&directory).map_err(|error| io_error(&directory, error))?;
        let mut requested = BTreeMap::<String, bool>::new();
        for root in roots {
            for (index, ancestor) in root.ancestors().enumerate() {
                let text = ancestor.to_string_lossy().into_owned();
                #[cfg(windows)]
                let text = text.to_lowercase();
                let key = blake3::hash(text.as_bytes()).to_hex().to_string();
                let exclusive = requested.entry(key).or_default();
                *exclusive |= index == 0;
            }
        }
        let mut locks = Self(Vec::new());
        for (key, exclusive) in requested {
            let path = directory.join(key);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|error| io_error(&path, error))?;
            permissions::restrict_file(&path).map_err(|error| io_error(&path, error))?;
            if !crate::file_lock::try_lock_mode(&file, exclusive)
                .map_err(|error| io_error(&path, error))?
            {
                return Err(ReactorError::InvalidInput(
                    "workspace write scope is occupied".to_owned(),
                ));
            }
            locks.0.push(file);
        }
        Ok(locks)
    }
}

impl Drop for WorkspaceLocks {
    fn drop(&mut self) {
        for file in &self.0 {
            let _ = crate::file_lock::unlock(file);
        }
    }
}

pub fn validate_policy_files(original: &Path, execution: &Path, files: &[&str]) -> Result<()> {
    use std::io::Read;
    fn read(path: &Path) -> Result<Option<Vec<u8>>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(path, error)),
        };
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error(path, error))?;
        if bytes.len() > 1024 * 1024 {
            return Err(ReactorError::InvalidInput(
                "policy file exceeds 1 MiB".to_owned(),
            ));
        }
        Ok(Some(bytes))
    }
    for relative in files {
        if read(&original.join(relative))? != read(&execution.join(relative))? {
            return Err(ReactorError::InvalidInput(
                "execution policy files differ from prepared workspace".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Create a worktree for one Plan occurrence.
pub fn prepare_worktree(paths: &ReactorPaths, run_id: &str, original: &Path) -> Result<PathBuf> {
    use crate::process_supervisor::BoundedOutput;
    if !crate::ids::is_safe_run_id(run_id) {
        return Err(ReactorError::InvalidInput("invalid Run ID".to_owned()));
    }
    let parent = paths.data_dir.join("worktrees");
    permissions::create_private_dir(&parent).map_err(|error| io_error(&parent, error))?;
    let parent = dunce::canonicalize(&parent).map_err(|error| io_error(&parent, error))?;
    let target = parent.join(run_id);
    if target.exists() {
        return Err(ReactorError::InvalidInput(
            "execution worktree already exists; explicit recovery required".to_owned(),
        ));
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(original)
        .args(["worktree", "add", "-b"])
        .arg(format!("reactor/{run_id}"))
        .arg(&target)
        .arg("HEAD")
        .bounded_output()
        .map_err(|error| io_error(original, error))?;
    if !output.status.success() {
        return Err(ReactorError::InvalidInput(
            "could not create execution worktree".to_owned(),
        ));
    }
    Ok(target)
}
