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
                config = config.add_source(File::with_name(&file).required(false));
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

/// # Errors
/// Configuration errors
pub fn load_config() -> Result<Config, config::ConfigError> {
    let config_file = args()
        .nth(1)
        .map_or_else(|| DefaultConfigFile::Static, DefaultConfigFile::Arg);
    Config::from_file(config_file)
}
