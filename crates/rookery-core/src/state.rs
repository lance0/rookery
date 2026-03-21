use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum ServerState {
    Stopped,
    Starting {
        profile: String,
        since: DateTime<Utc>,
    },
    Running {
        profile: String,
        pid: u32,
        port: u16,
        since: DateTime<Utc>,
        #[serde(default)]
        command_line: Vec<String>,
        #[serde(default)]
        exe_path: Option<PathBuf>,
    },
    Stopping {
        since: DateTime<Utc>,
    },
    Failed {
        last_error: String,
        profile: String,
        since: DateTime<Utc>,
    },
}

impl ServerState {
    pub fn is_running(&self) -> bool {
        matches!(self, ServerState::Running { .. })
    }

    pub fn profile_name(&self) -> Option<&str> {
        match self {
            ServerState::Starting { profile, .. }
            | ServerState::Running { profile, .. }
            | ServerState::Failed { profile, .. } => Some(profile),
            _ => None,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            ServerState::Running { pid, .. } => Some(*pid),
            _ => None,
        }
    }
}

impl Default for ServerState {
    fn default() -> Self {
        ServerState::Stopped
    }
}

/// Persists state to disk so the daemon can reconcile on restart.
pub struct StatePersistence {
    path: PathBuf,
}

impl StatePersistence {
    pub fn new() -> Self {
        let path = Self::state_path();
        Self { path }
    }

    pub fn state_path() -> PathBuf {
        if let Some(state_dir) = dirs::state_dir() {
            state_dir.join("rookery").join("state.json")
        } else if let Some(home) = dirs::home_dir() {
            home.join(".local")
                .join("state")
                .join("rookery")
                .join("state.json")
        } else {
            PathBuf::from("/tmp/rookery-state.json")
        }
    }

    pub fn load(&self) -> Result<ServerState> {
        if !self.path.exists() {
            return Ok(ServerState::Stopped);
        }
        let content = std::fs::read_to_string(&self.path)?;
        let state: ServerState = serde_json::from_str(&content)?;
        Ok(state)
    }

    pub fn save(&self, state: &ServerState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(state)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Check if a previously-running process is still alive and matches expectations.
    pub fn reconcile(&self, state: ServerState) -> ServerState {
        match &state {
            ServerState::Running { pid, exe_path, .. } => {
                if is_process_alive(*pid, exe_path.as_deref()) {
                    state
                } else {
                    tracing::warn!(pid, "previous process no longer running, resetting state");
                    ServerState::Stopped
                }
            }
            ServerState::Starting { .. } | ServerState::Stopping { .. } => {
                // Transient states on daemon restart mean something went wrong
                ServerState::Stopped
            }
            _ => state,
        }
    }
}

fn is_process_alive(pid: u32, expected_exe: Option<&Path>) -> bool {
    let proc_path = PathBuf::from(format!("/proc/{pid}"));
    if !proc_path.exists() {
        return false;
    }

    if let Some(expected) = expected_exe {
        let exe_link = proc_path.join("exe");
        if let Ok(actual) = std::fs::read_link(&exe_link) {
            return actual
                .to_string_lossy()
                .contains(&expected.to_string_lossy().to_string());
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_serialize_roundtrip() {
        let state = ServerState::Running {
            profile: "fast".into(),
            pid: 12345,
            port: 8081,
            since: Utc::now(),
            command_line: vec!["llama-server".into(), "-ngl".into(), "99".into()],
            exe_path: Some(PathBuf::from("/usr/bin/llama-server")),
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: ServerState = serde_json::from_str(&json).unwrap();
        assert!(restored.is_running());
        assert_eq!(restored.pid(), Some(12345));
    }

    #[test]
    fn test_reconcile_dead_process() {
        let persistence = StatePersistence::new();
        let state = ServerState::Running {
            profile: "fast".into(),
            pid: 999999999, // almost certainly not running
            port: 8081,
            since: Utc::now(),
            command_line: vec![],
            exe_path: None,
        };
        let reconciled = persistence.reconcile(state);
        assert!(matches!(reconciled, ServerState::Stopped));
    }
}
