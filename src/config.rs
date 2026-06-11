use argon2::{
    Argon2, PasswordHasher,
    password_hash::{SaltString, rand_core::OsRng},
};
use config::{Environment, File, FileFormat};
use glob_match::glob_match;
use rand::RngExt;
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
    pub max_retries: usize,
    pub max_file_size: u64,
    pub tesseract_bin: String,
    pub extractor_bin: String,
    index_contents_whitelist: Vec<String>,
}

impl Indexing {
    pub(crate) fn whitelisted(&self, path: &str) -> bool {
        for glob in &self.index_contents_whitelist {
            if glob_match(glob, path) {
                return true;
            }
        }

        false
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    Cpu,
    Vaapi,
    Nvenc,
    V4l,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Preview {
    pub max_unknown_file_size: u64,
    pub ffmpeg_bin: String,
    pub ffprobe_bin: String,
    pub encoder: Encoder,
}

#[derive(Clone, Debug, Deserialize)]
pub struct App {
    sources: Vec<PathBuf>,
    pub log_level: String,
    pub theme: String,
    pub cache_dir: PathBuf,
    pub share_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Auth {
    pub user: String,
    pub pass: String,
}

static CANONICALIZED_SOURCES: OnceLock<HashSet<PathBuf>> = OnceLock::new();

impl App {
    #[must_use]
    pub fn canonicalized_sources(&self) -> &HashSet<PathBuf> {
        CANONICALIZED_SOURCES.get_or_init(|| {
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
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub server: Server,
    pub database: Database,
    pub preview: Preview,
    pub app: App,
    pub indexing: Indexing,
    pub auth: Auth,
}

enum ConfigFile {
    Arg(String),
    Static,
}

const DEFAULT_CONFIG_FILES: &[&str] = &["config.yaml", "data/config.yaml"];

impl Config {
    fn from_file(f: ConfigFile) -> Result<Self, config::ConfigError> {
        let default = String::from_utf8_lossy(include_bytes!("default_config.yaml"));
        let mut config =
            config::Config::builder().add_source(File::from_str(&default, FileFormat::Yaml));

        match f {
            ConfigFile::Arg(file) => {
                config = config.add_source(File::with_name(&file).required(true));
            }
            ConfigFile::Static => {
                for file in DEFAULT_CONFIG_FILES {
                    config = config.add_source(File::with_name(file).required(false));
                }
            }
        }

        config = config.add_source(Environment::with_prefix("FAYLS").separator("_"));

        config.build()?.try_deserialize::<Self>()
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static SECRET: OnceLock<Vec<u8>> = OnceLock::new();
static ADMIN_AUTH: OnceLock<String> = OnceLock::new();

#[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
pub fn load() -> Result<(), config::ConfigError> {
    let config_file = args()
        .nth(1)
        .map_or_else(|| ConfigFile::Static, ConfigFile::Arg);
    CONFIG
        .set(Config::from_file(config_file)?)
        .expect("Config already set");

    _ = std::fs::create_dir_all(&get().app.cache_dir);

    Ok(())
}

#[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
pub fn get() -> &'static Config {
    CONFIG.get().expect("Confing not initialized")
}

pub(crate) fn secret() -> &'static [u8] {
    SECRET.get_or_init(|| {
        let sf = get().app.cache_dir.join("secret");

        if let Ok(bytes) = std::fs::read(&sf) {
            return bytes;
        }

        let mut bytes = vec![0u8; 128];
        rand::rng().fill(&mut bytes[..]);
        _ = std::fs::write(&sf, &bytes);
        bytes
    })
}

pub(crate) fn admin_auth() -> &'static str {
    ADMIN_AUTH.get_or_init(|| {
        let file = get().app.cache_dir.join("auth");

        if let Ok(existing) = std::fs::read_to_string(&file) {
            return existing;
        }

        let auth = format!("{}:{}", get().auth.user, get().auth.pass);

        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hashed_auth = argon2
            .hash_password(auth.as_bytes(), &salt)
            .expect("can't hash admin creds")
            .to_string();

        _ = std::fs::write(&file, &hashed_auth);
        hashed_auth
    })
}
