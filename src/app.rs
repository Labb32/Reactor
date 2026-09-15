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
}

pub struct App {
    #[allow(dead_code)]
    pub config: Config,
    #[allow(dead_code)]
    pub store: Store,
}

impl App {
    pub fn initialize(paths: ReactorPaths) -> Result<Initialization> {
        fs::create_dir_all(&paths.config_dir)
            .map_err(|source| io_error(&paths.config_dir, source))?;
        fs::create_dir_all(&paths.data_dir).map_err(|source| io_error(&paths.data_dir, source))?;

        let config_file = paths.config_file();
        let database_file = paths.database_file();
        let _config = Config::load_or_create(&config_file)?;
        let mut store = Store::open(&database_file)?;
        let schema_version = store.migrate()?;

        Ok(Initialization {
            config_file,
            database_file,
            schema_version,
        })
    }

    #[allow(dead_code)]
    pub fn open(paths: &ReactorPaths) -> Result<Self> {
        let config = Config::load(&paths.config_file())?;
        let store = Store::open_initialized(&paths.database_file())?;
        Ok(Self { config, store })
    }
}
