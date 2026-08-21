use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    // The hint is part of the message because this is otherwise indistinguishable
    // from a typo: a profile added to the file but not yet loaded by the running
    // daemon fails exactly like a misspelt one.
    #[error(
        "profile not found: {0} (config is read at daemon start — POST /api/reload if you just added it)"
    )]
    ProfileNotFound(String),

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("llama-server binary not found: {0}")]
    BinaryNotFound(PathBuf),

    #[error("profile '{profile}' references unknown model '{model}'")]
    InvalidModelRef { profile: String, model: String },

    #[error("state persistence error: {0}")]
    StatePersist(String),

    #[error("config validation error: {0}")]
    ConfigValidation(String),

    #[error("config serialize error: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// An operation was aborted because the daemon is shutting down.
    #[error("daemon is shutting down")]
    Shutdown,
}

pub type Result<T> = std::result::Result<T, Error>;
