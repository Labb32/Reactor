use std::{fs, io::Write, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result, io_error};

const MAX_CONCURRENCY: u16 = 64;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub concurrency: ConcurrencyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConcurrencyConfig {
    pub global: u16,
    pub per_agent: u16,
    pub per_provider: u16,
    pub per_local_endpoint: u16,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            global: 2,
            per_agent: 2,
            per_provider: 2,
            per_local_endpoint: 1,
        }
    }
}

impl Config {
    pub fn recover(path: &Path) -> Result<()> {
        let backup = path.with_extension("json.bak");
        if !path.exists() && backup.exists() {
            fs::rename(&backup, path).map_err(|source| io_error(path, source))?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents = crate::input::read_file(path)?;
        let config: Self =
            serde_json::from_slice(&contents).map_err(|source| ReactorError::InvalidConfig {
                path: path.to_path_buf(),
                source,
            })?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        write_config(path, self)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let values = [
            ("concurrency.global", self.concurrency.global),
            ("concurrency.per_agent", self.concurrency.per_agent),
            ("concurrency.per_provider", self.concurrency.per_provider),
            (
                "concurrency.per_local_endpoint",
                self.concurrency.per_local_endpoint,
            ),
        ];

        for (field, value) in values {
            if !(1..=MAX_CONCURRENCY).contains(&value) {
                return Err(ReactorError::InvalidConcurrency { field });
            }
        }

        Ok(())
    }
}

fn write_config(path: &Path, config: &Config) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io_error(path, std::io::Error::other("missing parent directory")))?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;

    let temporary = parent.join(format!(".config.{}.tmp", crate::ids::new_id()));
    let mut contents =
        serde_json::to_vec_pretty(config).map_err(|source| ReactorError::InvalidConfig {
            path: path.to_owned(),
            source,
        })?;
    contents.push(b'\n');
    let mut file = crate::permissions::create_private_new_file(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    if let Err(source) = file.write_all(&contents).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, source));
    }
    drop(file);
    let result = replace_config(path, &temporary);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn replace_config(path: &Path, temporary: &Path) -> Result<()> {
    platform::replace(temporary, path).map_err(|source| io_error(path, source))
}

#[cfg(not(windows))]
mod platform {
    use std::{fs, io, path::Path};

    pub fn replace(temporary: &Path, target: &Path) -> io::Result<()> {
        fs::rename(temporary, target)
    }
}

#[cfg(windows)]
mod platform {
    use std::{io, os::windows::ffi::OsStrExt, path::Path};

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    pub fn replace(temporary: &Path, target: &Path) -> io::Result<()> {
        let temporary = wide(temporary);
        let target = wide(target);
        // SAFETY: Both null-terminated buffers remain valid for the call.
        if unsafe {
            MoveFileExW(
                temporary.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
}
