use std::{fs, io::Write, path::Path, thread, time::{Duration, Instant}};
use serde::{Deserialize, Serialize};
use crate::{app::App, error::{Result, ReactorError, io_error}, paths::ReactorPaths};

pub struct AccessGuard(std::fs::File);
impl AccessGuard {
    pub fn acquire(paths: &ReactorPaths, exclusive: bool) -> Result<Self> {
        let path = paths.data_dir.join("offline.lock");
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(&path).map_err(|error| io_error(&path, error))?;
        crate::permissions::restrict_file(&path).map_err(|error| io_error(&path, error))?;
        if !crate::daemon::platform::try_lock_mode(&file, exclusive).map_err(|error| io_error(&path, error))? {
            return Err(ReactorError::InvalidInput("offline maintenance or another Reactor command is active".to_owned()));
        }
        Ok(Self(file))
    }
}
impl Drop for AccessGuard { fn drop(&mut self) { let _ = crate::daemon::platform::unlock(&self.0); } }

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| ReactorError::InvalidInput("file has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let temporary = parent.join(format!(".{}.tmp", crate::ids::new_id()));
    let mut file = crate::permissions::create_private_new_file(&temporary).map_err(|error| io_error(&temporary, error))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|error| io_error(&temporary, error))?;
    drop(file);
    replace_file(&temporary, path)
}

pub fn replace_file(temporary: &Path, path: &Path) -> Result<()> {
    #[cfg(not(windows))]
    fs::rename(&temporary, path).map_err(|error| io_error(path, error))?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let from = temporary.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        let to = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(io_error(path, std::io::Error::last_os_error()));
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent).and_then(|file| file.sync_all()).map_err(|error| io_error(parent, error))?;
    }
    Ok(())
}

pub fn maintenance(paths: &ReactorPaths) -> bool { paths.data_dir.join("maintenance.json").exists() }

pub fn set_maintenance(paths: &ReactorPaths, enabled: bool) -> Result<()> {
    let path = paths.data_dir.join("maintenance.json");
    if enabled {
        atomic_write(&path, serde_json::json!({"entered_at": crate::time::now_utc()}).to_string().as_bytes())
    } else {
        let app = App::open(paths)?;
        if app.store.has_active_execution()? { return Err(ReactorError::InvalidInput("cannot leave maintenance while execution or recovery is active".to_owned())); }
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(&path, error)),
        }
    }
}

pub fn drain(paths: &ReactorPaths, timeout: Duration) -> Result<()> {
    set_maintenance(paths, true)?;
    let started = Instant::now();
    loop {
        let mut app = App::open(paths)?;
        if !app.store.has_active_execution()? { return Ok(()); }
        if started.elapsed() >= timeout {
            for run in app.store.list_runs()? {
                if matches!(run.state.as_str(), "PREPARING" | "RUNNING") { app.store.cancel_run(&run.id, None)?; }
            }
            return Err(ReactorError::InvalidInput("drain timed out; cancellation requested, maintenance remains enabled".to_owned()));
        }
        if crate::interrupt::requested() { return Err(ReactorError::InvalidInput("drain interrupted; maintenance remains enabled".to_owned())); }
        drop(app);
        thread::sleep(Duration::from_millis(250));
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReadyState { pub process_id: u32, pub generation: String, pub updated_at: String, pub applied_config: serde_json::Value }

pub struct Readiness { paths: ReactorPaths, generation: String, config: serde_json::Value }
impl Readiness {
    pub fn starting(paths: &ReactorPaths, config: &crate::config::Config) -> Result<Self> {
        let path = paths.data_dir.join("ready.json");
        match fs::remove_file(&path) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(io_error(path, error)) }
        Ok(Self { paths: paths.clone(), generation: crate::ids::new_id(), config: serde_json::to_value(config).map_err(|error| ReactorError::InvalidInput(error.to_string()))? })
    }
    pub fn ready(&self) -> Result<()> {
        let state = ReadyState { process_id: std::process::id(), generation: self.generation.clone(), updated_at: crate::time::now_utc(), applied_config: self.config.clone() };
        atomic_write(&self.paths.data_dir.join("ready.json"), &serde_json::to_vec(&state).map_err(|error| ReactorError::InvalidInput(error.to_string()))?)
    }
}
impl Drop for Readiness { fn drop(&mut self) { let _ = fs::remove_file(self.paths.data_dir.join("ready.json")); } }

pub fn status(paths: &ReactorPaths) -> Result<serde_json::Value> {
    let running = crate::daemon::is_running(paths)?;
    let ready = fs::read(paths.data_dir.join("ready.json")).ok().and_then(|bytes| serde_json::from_slice::<ReadyState>(&bytes).ok());
    let ready_now = running && ready.as_ref().is_some_and(|state| {
        let owner = fs::read_to_string(paths.data_dir.join("daemon.lock")).ok();
        let fresh = state.updated_at.parse::<jiff::Timestamp>().ok().is_some_and(|time| {
            let now = jiff::Timestamp::now().as_second(); now >= time.as_second() && now - time.as_second() <= 15
        });
        owner.as_deref().is_some_and(|owner| owner.trim() == state.process_id.to_string()) && fresh
    });
    let app = App::open(paths);
    let database_available = app.is_ok();
    let configured = app.as_ref().ok().and_then(|app| serde_json::to_value(&app.config).ok());
    let divergence = ready.as_ref().is_some_and(|state| configured.as_ref().is_some_and(|configured| configured != &state.applied_config));
    let disks = [paths.config_dir.clone(), paths.data_dir.clone()].iter().map(|path| crate::storage::disk_space(path).map(|space| serde_json::to_value(space).unwrap_or_default()).unwrap_or_else(|error| serde_json::json!({"path": path, "error": error.to_string()}))).collect::<Vec<_>>();
    let log_bytes = crate::storage::log_bytes(&paths.data_dir.join("runs")).ok();
    Ok(serde_json::json!({"running": running, "ready": ready_now && database_available,
        "database_available": database_available, "maintenance": maintenance(paths), "configuration_restart_required": divergence,
        "applied": ready, "disks": disks, "log_bytes": log_bytes, "log_warning": log_bytes.is_some_and(|bytes| bytes >= 5 * 1024 * 1024 * 1024)}))
}
