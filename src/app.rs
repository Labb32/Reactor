use std::{fs, path::PathBuf};

use crate::{
    config::Config,
    error::{Result, io_error},
    paths::ReactorPaths,
    store::Store,
};

#[derive(Debug)]
pub struct Initialization {
    pub config_file: PathBuf,
    pub database_file: PathBuf,
    pub schema_version: u32,
    pub instance_id: String,
}

pub struct App {
    #[allow(dead_code)]
    pub config: Config,
    #[allow(dead_code)]
    pub store: Store,
    pub instance_id: String,
}

impl App {
    pub fn initialize(
        paths: ReactorPaths,
        config_override: Option<Config>,
    ) -> Result<Initialization> {
        fs::create_dir_all(&paths.config_dir)
            .map_err(|source| io_error(&paths.config_dir, source))?;
        fs::create_dir_all(&paths.data_dir).map_err(|source| io_error(&paths.data_dir, source))?;

        let config_file = paths.config_file();
        let database_file = paths.database_file();
        Config::recover(&config_file)?;
        let should_save_config = config_override.is_some() || !config_file.exists();
        let config = if let Some(config) = config_override {
            config.validate()?;
            config
        } else if config_file.exists() {
            Config::load(&config_file)?
        } else {
            Config::default()
        };
        let mut store = Store::open(&database_file)?;
        let schema_version = store.migrate()?;
        let instance_id = store.instance_id()?;
        if should_save_config {
            config.save(&config_file)?;
        }

        Ok(Initialization {
            config_file,
            database_file,
            schema_version,
            instance_id,
        })
    }

    #[allow(dead_code)]
    pub fn open(paths: &ReactorPaths) -> Result<Self> {
        let config = Config::load(&paths.config_file())?;
        let store = Store::open_initialized(&paths.database_file())?;
        let instance_id = store.instance_id()?;
        Ok(Self {
            config,
            store,
            instance_id,
        })
    }
}
