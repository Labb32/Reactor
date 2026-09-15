use std::{env, path::PathBuf};

use directories::BaseDirs;

use crate::error::{ReactorError, Result};

const CONFIG_DIR_ENV: &str = "REACTOR_CONFIG_DIR";
const DATA_DIR_ENV: &str = "REACTOR_DATA_DIR";

#[derive(Debug, Clone)]
pub struct ReactorPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl ReactorPaths {
    pub fn discover() -> Result<Self> {
        let base = BaseDirs::new().ok_or(ReactorError::UserDirectoriesUnavailable)?;
        let config_dir =
            path_override(CONFIG_DIR_ENV)?.unwrap_or_else(|| base.config_dir().join("reactor"));
        let data_dir =
            path_override(DATA_DIR_ENV)?.unwrap_or_else(|| base.data_local_dir().join("reactor"));

        Ok(Self {
            config_dir,
            data_dir,
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("reactor.db")
    }
}

fn path_override(name: &'static str) -> Result<Option<PathBuf>> {
    let Some(value) = env::var_os(name) else {
        return Ok(None);
    };

    if value.is_empty() {
        return Err(ReactorError::EmptyPathOverride { name });
    }

    Ok(Some(PathBuf::from(value)))
}
