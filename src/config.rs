use config::{Environment, File, FileFormat};
use serde::Deserialize;
use std::{collections::HashSet, env::args, path::PathBuf, sync::OnceLock};

#[derive(Clone, Debug, Deserialize)]
pub struct Server {
    pub address: String,
    pub port: String,
}

impl Server {
    #[must_use]
    pub fn addr(&self) -> String {
        format!("{}:{}", &self.address, &self.port)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Database {
    pub path: PathBuf,
    pub max_connections: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Indexing {
    pub batch_size: usize,
    pub max_concurrent_batches: usize,
    pub max_concurrent_indexers: usize,
    pub ignore_extensions: Vec<String>,
    pub max_retries: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct App {
    pub sources: Vec<PathBuf>,
    pub log_level: String,
    pub tesseract_bin: String,
    pub pdftoppm_bin: String,
}

impl App {
    #[must_use]
    pub fn canonicalized_sources(&self) -> HashSet<PathBuf> {
        self.sources
            .iter()
            .filter_map(|p| {
                p.canonicalize()
                    .map_err(|err| {
                        tracing::warn!(
                            "failed to canonicalize path for source {} ({})",
                            p.display(),
                            err
                        );
                    })
                    .ok()
            })
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub server: Server,
    pub database: Database,
    pub app: App,
    pub indexing: Indexing,
}

enum DefaultConfigFile {
    Arg(String),
    Static,
}

const DEFAULT_CONFIG_FILES: &[&str] = &["config.yaml", "data/config.yaml"];

impl Config {
    fn from_file(f: DefaultConfigFile) -> Result<Self, config::ConfigError> {
        let default = String::from_utf8_lossy(include_bytes!("default_config.yaml"));
        let mut config =
            config::Config::builder().add_source(File::from_str(&default, FileFormat::Yaml));

        match f {
            DefaultConfigFile::Arg(file) => {
                config = config.add_source(File::with_name(&file).required(true));
            }
            DefaultConfigFile::Static => {
                for file in DEFAULT_CONFIG_FILES {
                    config = config.add_source(File::with_name(file).required(false));
                }
            }
        }

        config = config.add_source(Environment::with_prefix("FAYLS"));

        config.build()?.try_deserialize::<Self>()
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
pub fn load() -> Result<(), config::ConfigError> {
    let config_file = args()
        .nth(1)
        .map_or_else(|| DefaultConfigFile::Static, DefaultConfigFile::Arg);
    CONFIG
        .set(Config::from_file(config_file)?)
        .expect("Config already set");

    Ok(())
}

#[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
pub fn get() -> &'static Config {
    CONFIG.get().expect("Confing not initialized")
}
