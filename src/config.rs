use std::{fs, io::Write, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result, io_error};

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
        let contents = fs::read(path).map_err(|source| io_error(path, source))?;
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
            if value == 0 {
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

    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary).map_err(|source| io_error(&temporary, source))?;
    serde_json::to_writer_pretty(&mut file, config).map_err(|source| {
        ReactorError::InvalidConfig {
            path: temporary.clone(),
            source,
        }
    })?;
    file.write_all(b"\n")
        .map_err(|source| io_error(&temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error(&temporary, source))?;
    if !path.exists() {
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        return Ok(());
    }

    // Preserve the previous file because Windows rename does not replace it.
    let backup = path.with_extension("json.bak");
    if backup.exists() {
        fs::remove_file(&backup).map_err(|source| io_error(&backup, source))?;
    }
    fs::rename(path, &backup).map_err(|source| io_error(path, source))?;
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::rename(&backup, path);
        return Err(io_error(path, source));
    }
    fs::remove_file(&backup).map_err(|source| io_error(&backup, source))?;

    Ok(())
}
