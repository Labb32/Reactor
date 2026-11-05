use std::{path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use serde::Serialize;
use crate::error::{Result, ReactorError, io_error};

const MIN_FREE: u64 = 10 * 1024 * 1024 * 1024;
const LOG_WARNING: u64 = 5 * 1024 * 1024 * 1024;
static LAST_DISK_WARNING: AtomicU64 = AtomicU64::new(0);
static LAST_LOG_SCAN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize)]
pub struct DiskSpace { pub path: String, pub available: u64, pub total: u64, pub sufficient: bool }

pub fn disk_space(path: &Path) -> Result<DiskSpace> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(|| ReactorError::InvalidInput("disk path has no existing ancestor".to_owned()))?;
    }
    let (available, total) = platform::space(existing).map_err(|error| io_error(existing, error))?;
    Ok(DiskSpace { path: path.to_string_lossy().into_owned(), available, total,
        sufficient: available >= MIN_FREE && total > 0 && (available as u128 * 100) >= total as u128 * 10 })
}

pub fn sufficient(paths: &[PathBuf]) -> bool {
    for path in paths {
        match disk_space(path) {
            Ok(space) if space.sufficient => {},
            _ => {
                if due(&LAST_DISK_WARNING, 60) { eprintln!("warning: low disk space; new executions blocked: require 10 GiB and 10% on {}", path.display()); }
                return false;
            }
        }
    }
    true
}

fn due(last: &AtomicU64, seconds: u64) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let previous = last.load(Ordering::Relaxed);
    now.saturating_sub(previous) >= seconds && last.compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}

pub fn log_bytes(root: &Path) -> std::io::Result<u64> {
    if !root.exists() { return Ok(0); }
    let mut size = 0_u64;
    for run in std::fs::read_dir(root)? {
        let run = run?;
        if !run.file_type()?.is_dir() || run.file_type()?.is_symlink() { continue; }
        for file in std::fs::read_dir(run.path())? {
            let file = file?;
            if file.file_type()?.is_file() && !file.file_type()?.is_symlink() { size = size.saturating_add(file.metadata()?.len()); }
        }
    }
    Ok(size)
}

pub fn warn_log_size(root: &Path) {
    if due(&LAST_LOG_SCAN, 60) {
        if let Ok(bytes) = log_bytes(root) {
            if bytes >= LOG_WARNING { eprintln!("warning: retained Run logs exceed 5 GiB ({bytes} bytes)"); }
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::{ffi::CString, io, os::unix::ffi::OsStrExt, path::Path};
    pub fn space(path: &Path) -> io::Result<(u64, u64)> {
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::other("invalid disk path"))?;
        let mut information = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), information.as_mut_ptr()) } != 0 { return Err(io::Error::last_os_error()); }
        let information = unsafe { information.assume_init() };
        Ok(((information.f_bavail as u64).saturating_mul(information.f_frsize as u64),
            (information.f_blocks as u64).saturating_mul(information.f_frsize as u64)))
    }
}

#[cfg(windows)]
mod platform {
    use std::{io, os::windows::ffi::OsStrExt, path::Path};
    pub fn space(path: &Path) -> io::Result<(u64, u64)> {
        let path = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let (mut available, mut total, mut free) = (0, 0, 0);
        if unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, &mut free) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((available, total))
    }
}
