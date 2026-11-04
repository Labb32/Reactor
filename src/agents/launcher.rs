use std::{ffi::OsString, process::Command};

/// Bound non-interactive capability/authentication probes and their descendants.
pub(super) trait BoundedOutput {
    fn bounded_output(&mut self) -> std::io::Result<std::process::Output>;
}

impl BoundedOutput for Command {
    fn bounded_output(&mut self) -> std::io::Result<std::process::Output> {
        use std::{io::Read, process::Stdio, sync::mpsc, thread, time::Duration};
        crate::execution_control::check()?;
        // Commands cannot be moved out of &mut self; reproduce the explicit invocation.
        let mut command = Command::new(self.get_program());
        command.args(self.get_args());
        if let Some(directory) = self.get_current_dir() { command.current_dir(directory); }
        for (key, value) in self.get_envs() {
            match value { Some(value) => { command.env(key, value); }, None => { command.env_remove(key); } }
        }
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut process = crate::process_supervisor::ProcessSupervisor::spawn(command)?;
        let stdout = process.take_stdout().ok_or_else(|| std::io::Error::other("probe stdout unavailable"))?;
        let stderr = process.take_stderr().ok_or_else(|| std::io::Error::other("probe stderr unavailable"))?;
        let (sender, receiver) = mpsc::channel();
        let output_sender = sender.clone();
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map(|_| bytes);
            let _ = output_sender.send((true, result));
        });
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stderr.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send((false, result));
        });
        let started = std::time::Instant::now();
        let status = loop {
            if let Err(error) = crate::execution_control::check() {
                process.stop(Duration::from_secs(3))?;
                return Err(error);
            }
            if let Some(status) = process.wait_for(Duration::from_millis(250))? { break status; }
            if started.elapsed() >= Duration::from_secs(30) {
                process.stop(Duration::from_secs(3))?;
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "agent probe timed out"));
            }
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for _ in 0..2 {
            let (is_stdout, result) = receiver.recv_timeout(Duration::from_secs(2))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "agent probe output timed out"))?;
            let bytes = result?;
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(std::io::Error::other("agent probe output exceeds 8 MiB"));
            }
            if is_stdout { stdout = bytes; } else { stderr = bytes; }
        }
        Ok(std::process::Output { status, stdout, stderr })
    }
}

pub(super) struct Launcher {
    pub program: OsString,
    #[cfg(windows)]
    script: Option<OsString>,
}

impl Launcher {
    pub fn command<I, S>(&self, arguments: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(&self.program);
        command.args(self.arguments(arguments));
        command
    }

    pub fn arguments<I, S>(&self, arguments: I) -> Vec<OsString>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        platform_arguments(self, arguments)
    }

    #[cfg(not(windows))]
    pub fn find(name: &str) -> Option<Self> {
        Some(Self {
            program: find_on_path(name)?,
        })
    }

    #[cfg(windows)]
    pub fn find(name: &str) -> Option<Self> {
        if let Some(program) = find_on_path(&format!("{name}.exe")) {
            return Some(Self {
                program,
                script: None,
            });
        }
        let script = find_on_path(&format!("{name}.ps1"))?;
        Some(Self {
            program: find_on_path("powershell.exe")?,
            script: Some(script),
        })
    }
}

#[cfg(not(windows))]
fn platform_arguments<I, S>(_launcher: &Launcher, arguments: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect()
}

#[cfg(windows)]
fn platform_arguments<I, S>(launcher: &Launcher, arguments: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    let Some(script) = &launcher.script else {
        return arguments;
    };
    let command = std::iter::once(script.as_os_str())
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(powershell_literal)
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        format!("& {command}").into(),
    ]
}

#[cfg(windows)]
fn powershell_literal(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "''"))
}

fn find_on_path(name: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| dunce::canonicalize(candidate).ok())
        .map(|candidate| candidate.into_os_string())
}
