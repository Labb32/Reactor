use std::{ffi::OsString, process::Command};

pub(super) struct Launcher {
    pub program: OsString,
    pub prefix: Vec<OsString>,
}

impl Launcher {
    pub fn command<I, S>(&self, arguments: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix).args(arguments);
        command
    }

    #[cfg(not(windows))]
    pub fn find(name: &str) -> Option<Self> {
        Some(Self {
            program: find_on_path(name)?,
            prefix: Vec::new(),
        })
    }

    #[cfg(windows)]
    pub fn find(name: &str) -> Option<Self> {
        if let Some(program) = find_on_path(&format!("{name}.exe")) {
            return Some(Self {
                program,
                prefix: Vec::new(),
            });
        }
        let script = find_on_path(&format!("{name}.ps1"))?;
        Some(Self {
            program: find_on_path("powershell.exe")?,
            prefix: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-File".into(),
                script,
            ],
        })
    }
}

fn find_on_path(name: &str) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| dunce::canonicalize(candidate).ok())
        .map(|candidate| candidate.into_os_string())
}
