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
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }

        let config = Self::default();
        config.validate()?;
        write_new(path, &config)?;
        Ok(config)
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

    fn validate(&self) -> Result<()> {
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

fn write_new(path: &Path, config: &Config) -> Result<()> {
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
    fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;

    Ok(())
}
