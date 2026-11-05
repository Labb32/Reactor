use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    app::App,
    error::{ReactorError, Result, io_error},
    interrupt,
    paths::ReactorPaths,
    permissions, runner,
};

const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

pub fn run(paths: ReactorPaths) -> Result<()> {
    let _lock = DaemonLock::acquire(&paths)?;
    App::initialize(paths.clone(), None)?;
    let shutdown = interrupt::install("daemon shutdown handler")?;
    let mut app = App::open(&paths)?;
    let readiness = crate::operations::Readiness::starting(&paths, &app.config)?;
    runner::prepare_execution(&paths, &mut app)?;
    let worker_count = usize::from(app.config.concurrency.global);

    let (sender, receiver) = mpsc::channel();
    let (startup_sender, startup_receiver) = mpsc::channel();
    let mut spawned = 0_usize;
    for _worker_index in 0..worker_count {
        let sender = sender.clone();
        let paths = paths.clone();
        let worker_shutdown = shutdown.clone();
        let startup = startup_sender.clone();
        let result = thread::Builder::new()
            .name("reactor-worker".to_owned())
            .spawn(move || {
                let outcome = match catch_unwind(AssertUnwindSafe(|| {
                    worker(&paths, &worker_shutdown, startup)
                })) {
                    Ok(result) => result,
                    Err(_) => Err(ReactorError::InvalidStoredData(
                        "daemon worker panicked".to_owned(),
                    )),
                };
                let _ = sender.send(outcome);
            });
        match result {
            Ok(_) => spawned += 1,
            Err(source) => {
                shutdown.store(true, Ordering::Release);
                for _ in 0..spawned {
                    let _ = receiver.recv();
                }
                return Err(io_error("daemon worker", source));
            }
        }
    }
    drop(sender);
    drop(startup_sender);
    for _ in 0..worker_count {
        if startup_receiver.recv() != Ok(true) { shutdown.store(true, Ordering::Release); }
    }
    let mut failure = None;
    if !shutdown.load(Ordering::Acquire) {
        if let Err(error) = readiness.ready() {
            shutdown.store(true, Ordering::Release); failure = Some(error);
        }
    }
    let mut remaining = spawned;
    while remaining > 0 {
        match receiver.recv_timeout(RECOVERY_INTERVAL) {
            Ok(Ok(())) => { remaining -= 1; }
            Ok(Err(error)) => {
                remaining -= 1;
                shutdown.store(true, Ordering::Release);
                failure.get_or_insert(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !shutdown.load(Ordering::Acquire) {
                    if let Err(error) = runner::recover(&paths, &mut app).and_then(|_| readiness.ready()) {
                        shutdown.store(true, Ordering::Release); failure.get_or_insert(error);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                shutdown.store(true, Ordering::Release);
                failure.get_or_insert_with(|| {
                    ReactorError::InvalidStoredData(
                        "daemon worker result channel closed".to_owned(),
                    )
                });
                break;
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

pub fn is_running(paths: &ReactorPaths) -> Result<bool> {
    Ok(DaemonLock::try_acquire(paths, false)?.is_none())
}

fn worker(paths: &ReactorPaths, shutdown: &AtomicBool, startup: mpsc::Sender<bool>) -> Result<()> {
    let mut app = match App::open(paths) {
        Ok(app) => app,
        Err(error) => { let _ = startup.send(false); return Err(error); }
    };
    let _ = startup.send(true);
    drop(startup);
    while !shutdown.load(Ordering::Acquire) {
        if runner::execute_available(paths, &mut app, Some(shutdown))?.is_none() {
            thread::sleep(IDLE_POLL_INTERVAL);
        }
    }
    Ok(())
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
        if !crate::file_lock::try_lock(&file).map_err(|source| io_error(&path, source))? {
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
        let _ = crate::file_lock::unlock(&self.file);
    }
}

fn lock_path(paths: &ReactorPaths) -> PathBuf {
    paths.data_dir.join("daemon.lock")
}

