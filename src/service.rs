use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{fs, io::Write};

use serde::{Serialize, Deserialize};

use crate::{
    daemon,
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
};

#[cfg(unix)]
use crate::permissions;

const STATE_WAIT: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize)]
pub struct Registration { pub instance_id: String, pub system: bool }

pub fn registration(paths: &ReactorPaths) -> Result<Registration> {
    let path = paths.config_dir.join("service-registration.json");
    let bytes = std::fs::read(&path).map_err(|error| io_error(&path, error))?;
    let registration: Registration = serde_json::from_slice(&bytes).map_err(|error| service_error("registration", &error.to_string()))?;
    validate_instance_id(&registration.instance_id)?;
    Ok(registration)
}

fn save_registration(paths: &ReactorPaths, instance_id: &str, system: bool) -> Result<()> {
    let bytes = serde_json::to_vec(&Registration { instance_id: instance_id.to_owned(), system })
        .map_err(|error| service_error("registration", &error.to_string()))?;
    let path = paths.config_dir.join("service-registration.json");
    crate::fs_support::atomic_write(&path, &bytes)?;
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).map_err(|error| io_error(&path, error))?; }
    Ok(())
}

fn system_mode(paths: &ReactorPaths) -> bool { registration(paths).is_ok_and(|registration| registration.system) }

fn system_control(operation: &'static str, instance_id: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let name = format!("reactor-{instance_id}.service");
        run_checked(operation, Command::new("systemctl").args([operation, &name]))
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = instance_id; Err(service_error(operation, "system services require Linux")) }
}

pub fn install_system(paths: &ReactorPaths, instance_id: &str, user: &str) -> Result<ServiceStatus> {
    #[cfg(target_os = "linux")]
    {
        validate_instance_id(instance_id)?;
        if user.is_empty() || user.len() > 64 || !user.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)) {
            return Err(service_error("install", "invalid service account"));
        }
        let account = std::ffi::CString::new(user).map_err(|_| service_error("install", "invalid service account"))?;
        let record = unsafe { libc::getpwnam(account.as_ptr()) };
        if record.is_null() || unsafe { (*record).pw_uid } == 0 {
            return Err(service_error("install", "an existing non-root service account is required"));
        }
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { (*record).pw_uid };
        for path in [&paths.config_dir, &paths.data_dir] {
            if std::fs::metadata(path).map_err(|error| io_error(path, error))?.uid() != uid {
                return Err(service_error("install", "config and data directories must already belong to the service account"));
            }
        }
        fn quote(path: &Path) -> Result<String> {
            let value = path.to_str().ok_or_else(|| service_error("install", "invalid service path"))?;
            if value.chars().any(char::is_control) { return Err(service_error("install", "invalid service path")); }
            Ok(format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%")))
        }
        let executable = dunce::canonicalize(std::env::current_exe().map_err(|error| io_error("executable", error))?).map_err(|error| io_error("executable", error))?;
        let unit = format!("[Unit]\nDescription=Reactor agent scheduler\nAfter=network-online.target\n\n[Service]\nType=simple\nUser={user}\nExecStart={} daemon run --config-dir {} --data-dir {}\nRestart=on-failure\nRestartSec=5\nKillMode=mixed\nTimeoutStopSec=35min\nNoNewPrivileges=true\n\n[Install]\nWantedBy=multi-user.target\n",
            quote(&executable)?, quote(&canonical_directory(&paths.config_dir)?)?, quote(&canonical_directory(&paths.data_dir)?)?);
        let path = PathBuf::from("/etc/systemd/system").join(format!("reactor-{instance_id}.service"));
        crate::fs_support::atomic_write(&path, unit.as_bytes())?;
        run_checked("install", Command::new("systemctl").arg("daemon-reload"))?;
        system_control("enable", instance_id)?;
        save_registration(paths, instance_id, true)?;
        status(paths, instance_id)
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (paths, instance_id, user); Err(service_error("install", "system services require Linux")) }
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub platform: &'static str,
    pub installed: bool,
    pub running: bool,
}

pub fn install(paths: &ReactorPaths, instance_id: &str) -> Result<ServiceStatus> {
    validate_instance_id(instance_id)?;
    let executable = dunce::canonicalize(
        std::env::current_exe().map_err(|source| io_error("current executable", source))?,
    )
    .map_err(|source| io_error("current executable", source))?;
    let config_dir = canonical_directory(&paths.config_dir)?;
    let data_dir = canonical_directory(&paths.data_dir)?;
    platform::install(instance_id, &executable, &config_dir, &data_dir)?;
    save_registration(paths, instance_id, false)?;
    status(paths, instance_id)
}

pub fn start(paths: &ReactorPaths, instance_id: &str) -> Result<ServiceStatus> {
    let current = status(paths, instance_id)?;
    if !current.installed {
        return Err(service_error("start", "service is not installed"));
    }
    if !current.running {
        if system_mode(paths) { system_control("start", instance_id)?; } else { platform::start(instance_id)?; }
        wait_for(paths, true)?;
    }
    status(paths, instance_id)
}

pub fn stop(paths: &ReactorPaths, instance_id: &str) -> Result<ServiceStatus> {
    let _ = crate::operations::set_maintenance(paths, true);
    if crate::app::App::open(paths).is_ok() { crate::operations::drain(paths, Duration::from_secs(1800))?; }
    let current = status(paths, instance_id)?;
    if current.installed && current.running {
        if system_mode(paths) { system_control("stop", instance_id)?; } else { platform::stop(instance_id)?; }
        wait_for(paths, false)?;
    }
    status(paths, instance_id)
}

pub fn uninstall(paths: &ReactorPaths, instance_id: &str) -> Result<ServiceStatus> {
    let current = status(paths, instance_id)?;
    if current.running {
        if system_mode(paths) { system_control("stop", instance_id)?; } else { platform::stop(instance_id)?; }
        wait_for(paths, false)?;
    }
    if current.installed {
        if system_mode(paths) {
            system_control("disable", instance_id)?;
            #[cfg(target_os = "linux")]
            {
                let path = PathBuf::from("/etc/systemd/system").join(format!("reactor-{instance_id}.service"));
                std::fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
                run_checked("uninstall", Command::new("systemctl").arg("daemon-reload"))?;
            }
        } else { platform::uninstall(instance_id)?; }
    }
    status(paths, instance_id)
}

pub fn status(paths: &ReactorPaths, instance_id: &str) -> Result<ServiceStatus> {
    validate_instance_id(instance_id)?;
    Ok(ServiceStatus {
        platform: if system_mode(paths) { "systemd_system" } else { platform::NAME },
        installed: if system_mode(paths) {
            PathBuf::from("/etc/systemd/system").join(format!("reactor-{instance_id}.service")).is_file()
        } else { platform::installed(instance_id)? },
        running: daemon::is_running(paths)?,
    })
}

fn validate_instance_id(instance_id: &str) -> Result<()> {
    uuid::Uuid::parse_str(instance_id)
        .map(|_| ())
        .map_err(|_| ReactorError::InvalidStoredData("instance ID is not a UUID".to_owned()))
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    dunce::canonicalize(path).map_err(|source| io_error(path, source))
}

fn wait_for(paths: &ReactorPaths, running: bool) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < STATE_WAIT {
        if daemon::is_running(paths)? == running
            && (!running || crate::operations::status(paths)?["ready"] == true) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    let expected = if running { "start" } else { "stop" };
    Err(service_error(expected, "daemon state did not change"))
}

#[cfg(unix)]
fn write_service_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| service_error("install", "service file has no parent directory"))?;
    permissions::create_private_dir(parent).map_err(|source| io_error(parent, source))?;
    let temporary = parent.join(format!(".service.{}.tmp", crate::ids::new_id()));
    let mut file = permissions::create_private_new_file(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    if let Err(source) = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, source));
    }
    drop(file);
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(path, source));
    }
    Ok(())
}

fn run_checked(operation: &'static str, command: &mut Command) -> Result<()> {
    use crate::process_supervisor::BoundedOutput;
    let output = command
        .bounded_output()
        .map_err(|source| service_error(operation, &source.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output_error(operation, &output))
    }
}

fn output_error(operation: &'static str, output: &Output) -> ReactorError {
    let bytes = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let message = String::from_utf8_lossy(bytes);
    let message = message.trim();
    service_error(
        operation,
        if message.is_empty() {
            "platform command returned a failure status"
        } else {
            message
        },
    )
}

fn service_error(operation: &'static str, message: &str) -> ReactorError {
    ReactorError::ServiceOperation {
        operation,
        message: message.chars().take(4_096).collect(),
    }
}

#[cfg(windows)]
mod platform {
    use std::{fs, path::Path, process::Command};

    use super::{Result, run_checked, service_error};
    use crate::{ids, permissions};

    pub const NAME: &str = "windows_task_scheduler";
    pub fn install(
        instance_id: &str,
        executable: &Path,
        config_dir: &Path,
        data_dir: &Path,
    ) -> Result<()> {
        let executable = text_path(executable)?;
        let arguments = format!(
            "daemon run --config-dir {} --data-dir {}",
            quote_argument(config_dir)?,
            quote_argument(data_dir)?
        );
        let script_path = data_dir.join(format!("service-install-{}.ps1", ids::new_id()));
        let script = r#"$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $args[1] -Argument $args[2]
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $identity
$principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -StartWhenAvailable
Register-ScheduledTask -TaskName $args[0] -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
"#;
        let mut file = permissions::create_private_new_file(&script_path)
            .map_err(|source| crate::error::io_error(&script_path, source))?;
        use std::io::Write;
        file.write_all(script.as_bytes())
            .map_err(|source| crate::error::io_error(&script_path, source))?;
        file.sync_all()
            .map_err(|source| crate::error::io_error(&script_path, source))?;
        drop(file);
        let result = run_checked(
            "install",
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ])
                .arg(&script_path)
                .arg(task_name(instance_id))
                .arg(executable)
                .arg(arguments),
        );
        let remove_result = fs::remove_file(&script_path)
            .map_err(|source| crate::error::io_error(&script_path, source));
        result.and(remove_result)
    }

    pub fn start(instance_id: &str) -> Result<()> {
        run_checked(
            "start",
            Command::new("schtasks.exe").args(["/Run", "/TN", &task_name(instance_id)]),
        )
    }

    pub fn stop(instance_id: &str) -> Result<()> {
        run_checked(
            "stop",
            Command::new("schtasks.exe").args(["/End", "/TN", &task_name(instance_id)]),
        )
    }

    pub fn uninstall(instance_id: &str) -> Result<()> {
        run_checked(
            "uninstall",
            Command::new("schtasks.exe").args(["/Delete", "/TN", &task_name(instance_id), "/F"]),
        )
    }

    pub fn installed(instance_id: &str) -> Result<bool> {
        Command::new("schtasks.exe")
            .args(["/Query", "/TN", &task_name(instance_id)])
            .output()
            .map(|output| output.status.success())
            .map_err(|source| service_error("status", &source.to_string()))
    }

    fn task_name(instance_id: &str) -> String {
        format!("Reactor-{instance_id}")
    }

    fn quote_argument(path: &Path) -> Result<String> {
        let value = text_path(path)?;
        let mut quoted = String::with_capacity(value.len() + 2);
        quoted.push('"');
        let mut backslashes = 0;
        for character in value.chars() {
            match character {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                _ => {
                    quoted.extend(std::iter::repeat_n('\\', backslashes));
                    quoted.push(character);
                    backslashes = 0;
                }
            }
        }
        quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        quoted.push('"');
        Ok(quoted)
    }

    fn text_path(path: &Path) -> Result<&str> {
        path.as_os_str()
            .to_str()
            .ok_or_else(|| service_error("install", "service paths must contain valid Unicode"))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use std::{fs, path::Path, process::Command};

    use directories::BaseDirs;

    use super::{Result, run_checked, service_error, write_service_file};

    pub const NAME: &str = "systemd_user";
    pub fn install(
        instance_id: &str,
        executable: &Path,
        config_dir: &Path,
        data_dir: &Path,
    ) -> Result<()> {
        let unit = format!(
            "[Unit]\nDescription=Reactor agent scheduler\n\n[Service]\nType=simple\nExecStart={} daemon run --config-dir {} --data-dir {}\nRestart=on-failure\nRestartSec=5\nKillMode=mixed\nTimeoutStopSec=35min\n\n[Install]\nWantedBy=default.target\n",
            quote(executable)?,
            quote(config_dir)?,
            quote(data_dir)?
        );
        let unit_name = unit_name(instance_id);
        write_service_file(&unit_path(instance_id)?, &unit)?;
        run_checked(
            "install",
            Command::new("systemctl").args(["--user", "daemon-reload"]),
        )?;
        run_checked(
            "install",
            Command::new("systemctl").args(["--user", "enable", &unit_name]),
        )
    }

    pub fn start(instance_id: &str) -> Result<()> {
        run_checked(
            "start",
            Command::new("systemctl").args(["--user", "start", &unit_name(instance_id)]),
        )
    }

    pub fn stop(instance_id: &str) -> Result<()> {
        run_checked(
            "stop",
            Command::new("systemctl").args(["--user", "stop", &unit_name(instance_id)]),
        )
    }

    pub fn uninstall(instance_id: &str) -> Result<()> {
        let unit_name = unit_name(instance_id);
        run_checked(
            "uninstall",
            Command::new("systemctl").args(["--user", "disable", &unit_name]),
        )?;
        let path = unit_path(instance_id)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::error::io_error(&path, error)),
        }
        run_checked(
            "uninstall",
            Command::new("systemctl").args(["--user", "daemon-reload"]),
        )
    }

    pub fn installed(instance_id: &str) -> Result<bool> {
        Ok(unit_path(instance_id)?.is_file())
    }

    fn unit_name(instance_id: &str) -> String {
        format!("reactor-{instance_id}.service")
    }

    fn unit_path(instance_id: &str) -> Result<std::path::PathBuf> {
        let base = BaseDirs::new()
            .ok_or_else(|| service_error("status", "user directories are unavailable"))?;
        Ok(base
            .config_dir()
            .join("systemd/user")
            .join(unit_name(instance_id)))
    }

    fn quote(path: &Path) -> Result<String> {
        let value = path
            .to_str()
            .ok_or_else(|| service_error("install", "service paths must contain valid Unicode"))?;
        if value.chars().any(char::is_control) {
            return Err(service_error(
                "install",
                "service paths cannot contain control characters",
            ));
        }
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "$$")
            .replace('%', "%%");
        Ok(format!("\"{escaped}\""))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{fs, path::Path, process::Command};

    use directories::BaseDirs;

    use super::{Result, run_checked, service_error, write_service_file};

    pub const NAME: &str = "launch_agent";
    pub fn install(
        instance_id: &str,
        executable: &Path,
        config_dir: &Path,
        data_dir: &Path,
    ) -> Result<()> {
        let label = label(instance_id);
        let stdout = data_dir.join("daemon.stdout.log");
        let stderr = data_dir.join("daemon.stderr.log");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string><string>daemon</string><string>run</string>\n    <string>--config-dir</string><string>{}</string>\n    <string>--data-dir</string><string>{}</string>\n  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>{}</string>\n  <key>StandardErrorPath</key><string>{}</string>\n</dict>\n</plist>\n",
            xml(executable)?,
            xml(config_dir)?,
            xml(data_dir)?,
            xml(&stdout)?,
            xml(&stderr)?
        );
        write_service_file(&plist_path(instance_id)?, &plist)
    }

    pub fn start(instance_id: &str) -> Result<()> {
        let target = target();
        let label = label(instance_id);
        let loaded = Command::new("launchctl")
            .args(["print", &format!("{target}/{label}")])
            .output()
            .map_err(|source| service_error("start", &source.to_string()))?
            .status
            .success();
        if !loaded {
            run_checked(
                "start",
                Command::new("launchctl")
                    .arg("bootstrap")
                    .arg(&target)
                    .arg(plist_path(instance_id)?),
            )?;
        }
        run_checked(
            "start",
            Command::new("launchctl").args(["kickstart", &format!("{target}/{label}")]),
        )
    }

    pub fn stop(instance_id: &str) -> Result<()> {
        run_checked(
            "stop",
            Command::new("launchctl")
                .args(["bootout", &format!("{}/{}", target(), label(instance_id))]),
        )
    }

    pub fn uninstall(instance_id: &str) -> Result<()> {
        let path = plist_path(instance_id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(crate::error::io_error(&path, error)),
        }
    }

    pub fn installed(instance_id: &str) -> Result<bool> {
        Ok(plist_path(instance_id)?.is_file())
    }

    fn target() -> String {
        // SAFETY: geteuid has no preconditions.
        format!("gui/{}", unsafe { libc::geteuid() })
    }

    fn label(instance_id: &str) -> String {
        format!("dev.reactor.scheduler.{instance_id}")
    }

    fn plist_path(instance_id: &str) -> Result<std::path::PathBuf> {
        let base = BaseDirs::new()
            .ok_or_else(|| service_error("status", "user directories are unavailable"))?;
        Ok(base
            .home_dir()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(instance_id))))
    }

    fn xml(path: &Path) -> Result<String> {
        let value = path
            .to_str()
            .ok_or_else(|| service_error("install", "service paths must contain valid Unicode"))?;
        if value.chars().any(char::is_control) {
            return Err(service_error(
                "install",
                "service paths cannot contain control characters",
            ));
        }
        Ok(value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;"))
    }
}
