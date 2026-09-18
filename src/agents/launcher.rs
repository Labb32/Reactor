use std::{ffi::OsString, process::Command};

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
