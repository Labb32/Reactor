use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{
    app::App,
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
    permissions, runner,
};

const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub fn run(paths: ReactorPaths) -> Result<()> {
    let _lock = DaemonLock::acquire(&paths)?;
    App::initialize(paths.clone(), None)?;
    let mut app = App::open(&paths)?;
    runner::prepare_execution(&paths, &mut app)?;
    let worker_count = usize::from(app.config.concurrency.global);
    drop(app);

    let (sender, receiver) = mpsc::channel();
    for _ in 0..worker_count {
        let sender = sender.clone();
        let paths = paths.clone();
        thread::Builder::new()
            .name("reactor-worker".to_owned())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| worker(&paths)));
                let error = match outcome {
                    Ok(Err(error)) => error,
                    Ok(Ok(())) => ReactorError::InvalidStoredData(
                        "daemon worker stopped unexpectedly".to_owned(),
                    ),
                    Err(_) => ReactorError::InvalidStoredData("daemon worker panicked".to_owned()),
                };
                let _ = sender.send(error);
            })
            .map_err(|source| io_error("daemon worker", source))?;
    }
    drop(sender);
    match receiver.recv() {
        Ok(error) => Err(error),
        Err(_) => Err(ReactorError::InvalidStoredData(
            "all daemon workers stopped unexpectedly".to_owned(),
        )),
    }
}

pub fn is_running(paths: &ReactorPaths) -> Result<bool> {
    Ok(DaemonLock::try_acquire(paths, false)?.is_none())
}

fn worker(paths: &ReactorPaths) -> Result<()> {
    let mut app = App::open(paths)?;
    loop {
        if runner::execute_available(paths, &mut app)?.is_none() {
            thread::sleep(IDLE_POLL_INTERVAL);
        }
    }
}

struct DaemonLock {
    file: File,
}

impl DaemonLock {
    fn acquire(paths: &ReactorPaths) -> Result<Self> {
        Self::try_acquire(paths, true)?.ok_or(ReactorError::DaemonAlreadyRunning)
    }

    fn try_acquire(paths: &ReactorPaths, write_owner: bool) -> Result<Option<Self>> {
        permissions::create_private_dir(&paths.data_dir)
            .map_err(|source| io_error(&paths.data_dir, source))?;
        let path = lock_path(paths);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        permissions::restrict_file(&path).map_err(|source| io_error(&path, source))?;
        if !platform::try_lock(&file).map_err(|source| io_error(&path, source))? {
            return Ok(None);
        }
        if write_owner {
            file.set_len(0).map_err(|source| io_error(&path, source))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|source| io_error(&path, source))?;
            writeln!(file, "{}", std::process::id()).map_err(|source| io_error(&path, source))?;
            file.sync_all().map_err(|source| io_error(&path, source))?;
        }
        Ok(Some(Self { file }))
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = platform::unlock(&self.file);
    }
}

fn lock_path(paths: &ReactorPaths) -> PathBuf {
    paths.data_dir.join("daemon.lock")
}

#[cfg(unix)]
mod platform {
    use std::{fs::File, io, os::fd::AsRawFd};

    pub fn try_lock(file: &File) -> io::Result<bool> {
        // SAFETY: flock only reads the valid descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub fn unlock(file: &File) -> io::Result<()> {
        // SAFETY: flock only reads the valid descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{fs::File, io, mem::zeroed, os::windows::io::AsRawHandle};

    use windows_sys::Win32::{
        Foundation::{ERROR_LOCK_VIOLATION, GetLastError, HANDLE},
        Storage::FileSystem::{
            LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
        },
        System::IO::OVERLAPPED,
    };

    pub fn try_lock(file: &File) -> io::Result<bool> {
        // SAFETY: OVERLAPPED is initialized and the file handle remains valid.
        let mut overlapped = unsafe { zeroed::<OVERLAPPED>() };
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if result != 0 {
            return Ok(true);
        }
        // SAFETY: GetLastError has no preconditions.
        if unsafe { GetLastError() } == ERROR_LOCK_VIOLATION {
            Ok(false)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn unlock(file: &File) -> io::Result<()> {
        // SAFETY: OVERLAPPED matches the range locked above and the handle is valid.
        let mut overlapped = unsafe { zeroed::<OVERLAPPED>() };
        if unsafe { UnlockFileEx(file.as_raw_handle() as HANDLE, 0, 1, 0, &mut overlapped) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
