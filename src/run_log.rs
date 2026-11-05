use std::{
    collections::VecDeque,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use crate::permissions;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LOG_FILES: usize = 5;
const CAPTURE_TAIL_BYTES: usize = 1024 * 1024;
const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub struct RunLog {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    failed: bool,
}

pub type SharedRunLog = Arc<Mutex<RunLog>>;

impl RunLog {
    pub fn open(path: &Path) -> io::Result<SharedRunLog> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("log path has no parent"))?;
        permissions::create_private_dir(parent)?;
        let file = permissions::create_private_new_file(path)?;
        Ok(Arc::new(Mutex::new(Self {
            path: path.to_owned(),
            file: Some(file),
            size: 0,
            failed: false,
        })))
    }

    pub fn flush(log: &SharedRunLog) -> io::Result<()> {
        let mut log = log
            .lock()
            .map_err(|_| io::Error::other("log writer lock was poisoned"))?;
        log.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is unavailable"))?
            .flush()
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let result = self.write_inner(bytes);
        if result.is_err() { self.failed = true; }
        result
    }

    pub fn failed(log: &SharedRunLog) -> bool {
        log.lock().map(|log| log.failed).unwrap_or(true)
    }

    fn write_inner(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.size > 0 && self.size + bytes.len() as u64 > MAX_LOG_BYTES {
            self.rotate()?;
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is unavailable"))?
            .write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }

        let oldest = rotated_path(&self.path, MAX_LOG_FILES - 1);
        remove_file_if_present(&oldest)?;
        for index in (1..MAX_LOG_FILES - 1).rev() {
            let source = rotated_path(&self.path, index);
            if source.exists() {
                fs::rename(&source, rotated_path(&self.path, index + 1))?;
            }
        }
        if self.path.exists() {
            fs::rename(&self.path, rotated_path(&self.path, 1))?;
        }
        self.file = Some(permissions::create_private_new_file(&self.path)?);
        self.size = 0;
        Ok(())
    }
}

pub fn capture(mut reader: impl Read, log: SharedRunLog) -> io::Result<Vec<u8>> {
    capture_inner(&mut reader, log, None)
}

pub fn capture_agent(mut reader: impl Read, log: SharedRunLog, events: Arc<Mutex<crate::agent_events::Events>>) -> io::Result<Vec<u8>> {
    capture_inner(&mut reader, log, Some(events))
}

fn capture_inner(reader: &mut impl Read, log: SharedRunLog, events: Option<Arc<Mutex<crate::agent_events::Events>>>) -> io::Result<Vec<u8>> {
    let mut redactor = crate::redact::Redactor::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut write_error = None;
    let mut tail = VecDeque::new();
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            let remaining = redactor.push(&[], true);
            if write_error.is_none()
                && let Err(error) = log.lock().map_err(|_| io::Error::other("log lock poisoned")).and_then(|mut log| log.write(&remaining)) {
                    write_error = Some(error);
                }
            if let Some(events) = &events {
                events.lock().map_err(|_| io::Error::other("event reader lock poisoned"))?.finish();
            }
            return match write_error {
                Some(error) => Err(error),
                None => Ok(tail.into()),
            };
        }
        if let Some(events) = &events {
            events.lock().map_err(|_| io::Error::other("event reader lock poisoned"))?.feed(&buffer[..count]);
        }
        tail.extend(&buffer[..count]);
        if tail.len() > CAPTURE_TAIL_BYTES {
            tail.drain(..tail.len() - CAPTURE_TAIL_BYTES);
        }
        let masked = redactor.push(&buffer[..count], false);
        if write_error.is_none()
            && let Err(error) = log
                .lock()
                .map_err(|_| io::Error::other("log writer lock was poisoned"))
                .and_then(|mut log| log.write(&masked))
        {
            write_error = Some(error);
        }
    }
}

pub fn prune(root: &Path, protected: &std::collections::HashSet<String>) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let now = SystemTime::now();
    for entry in entries {
        let entry = entry?;
        if protected.contains(&entry.file_name().to_string_lossy().into_owned()) { continue; }
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let directory = entry.path();
        let files = match fs::read_dir(&directory) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for file in files {
            let file = file?;
            let file_type = file.file_type()?;
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let metadata = match file.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if now
                .duration_since(metadata.modified()?)
                .is_ok_and(|age| age > RETENTION)
            {
                remove_file_if_present(&file.path())?;
            }
        }
    }
    Ok(())
}

pub fn remove_run_directory(root: &Path, run_id: &str) -> io::Result<()> {
    let component = Path::new(run_id);
    if run_id.is_empty() || matches!(run_id, "." | "..") || component.components().count() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Run ID is not a safe path component",
        ));
    }
    let directory = root.join(component);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Run log path is not a regular directory",
        ));
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Run log directory contains a non-regular file",
            ));
        }
        files.push(entry.path());
    }
    for file in files {
        remove_file_if_present(&file)?;
    }
    match fs::remove_dir(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
