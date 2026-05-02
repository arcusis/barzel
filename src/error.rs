use thiserror::Error;

#[derive(Error, Debug)]
pub enum BarzelError {
    #[allow(dead_code)]
    #[error("project detection failed: {0}")]
    Detection(String),

    #[allow(dead_code)]
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("TOML error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("TOML parse error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[allow(dead_code)]
    #[error("unknown command: {0}")]
    UnknownCommand(String),
}

pub type Result<T> = std::result::Result<T, BarzelError>;
