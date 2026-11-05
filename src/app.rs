use std::path::PathBuf;

use crate::{
    config::Config,
    error::{Result, io_error},
    paths::ReactorPaths,
    permissions,
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
    pub config: Config,
    pub store: Store,
    pub instance_id: String,
    _access: crate::operations::AccessGuard,
}

impl App {
    pub fn initialize(
        paths: ReactorPaths,
        config_override: Option<Config>,
    ) -> Result<Initialization> {
        permissions::create_private_dir(&paths.config_dir)
            .map_err(|source| io_error(&paths.config_dir, source))?;
        permissions::create_private_dir(&paths.data_dir)
            .map_err(|source| io_error(&paths.data_dir, source))?;
        let _access = crate::operations::AccessGuard::acquire(&paths, false)?;
        if paths.data_dir.join("restore.json").exists() {
            return Err(crate::error::ReactorError::InvalidInput("restore is incomplete; resume backup restore before initialization".to_owned()));
        }

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
        permissions::restrict_file(&database_file)
            .map_err(|source| io_error(&database_file, source))?;
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

    pub fn open(paths: &ReactorPaths) -> Result<Self> {
        let access = crate::operations::AccessGuard::acquire(paths, false)?;
        if paths.data_dir.join("restore.json").exists() {
            return Err(crate::error::ReactorError::InvalidInput("restore is incomplete; rerun the same backup restore command".to_owned()));
        }
        permissions::restrict_dir(&paths.config_dir)
            .map_err(|source| io_error(&paths.config_dir, source))?;
        permissions::restrict_dir(&paths.data_dir)
            .map_err(|source| io_error(&paths.data_dir, source))?;
        let config_file = paths.config_file();
        let database_file = paths.database_file();
        if config_file.exists() {
            permissions::restrict_file(&config_file)
                .map_err(|source| io_error(&config_file, source))?;
        }
        if database_file.exists() {
            permissions::restrict_file(&database_file)
                .map_err(|source| io_error(&database_file, source))?;
        }
        let config = Config::load(&config_file)?;
        let store = Store::open_initialized(&database_file)?;
        let instance_id = store.instance_id()?;
        Ok(Self {
            _access: access,
            config,
            store,
            instance_id,
        })
    }
}
