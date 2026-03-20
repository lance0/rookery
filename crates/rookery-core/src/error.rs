use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("profile not found: {0}")]
    ProfileNotFound(String),

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("llama-server binary not found: {0}")]
    BinaryNotFound(PathBuf),

    #[error("profile '{profile}' references unknown model '{model}'")]
    InvalidModelRef { profile: String, model: String },

    #[error("state persistence error: {0}")]
    StatePersist(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
