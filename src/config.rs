use config::{Environment, File, FileFormat};
use serde::Deserialize;
use std::{env::args, path::PathBuf};

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
pub struct App {
    pub database: PathBuf,
    pub sources: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub server: Server,
    pub app: App,
}

const DEFAULT_CONFIG_FILE: &str = "config.yaml";

impl Config {
    fn from_file(filename: &str) -> Result<Self, config::ConfigError> {
        let default = String::from_utf8_lossy(include_bytes!("config.yaml"));
        let config = config::Config::builder()
            .add_source(File::from_str(&default, FileFormat::Yaml))
            .add_source(File::with_name(filename).required(false))
            .add_source(Environment::with_prefix("FAYLS"))
            .build()?;
        config.try_deserialize::<Self>()
    }
}

/// # Errors
/// Configuration errors
pub fn load_config() -> Result<Config, config::ConfigError> {
    let config_file = args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_CONFIG_FILE.to_string());
    Config::from_file(&config_file)
}
