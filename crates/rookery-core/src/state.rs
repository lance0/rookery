use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::BackendType;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
#[derive(Default)]
pub enum ServerState {
    #[default]
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
        #[serde(default)]
        backend_type: BackendType,
        #[serde(default)]
        container_id: Option<String>,
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

/// Persists state to disk so the daemon can reconcile on restart.
pub struct StatePersistence {
    pub path: PathBuf,
}

impl Default for StatePersistence {
    fn default() -> Self {
        Self::new()
    }
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

// --- Agent state persistence ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub agents: HashMap<String, AgentEntry>,
}

pub struct AgentPersistence {
    /// Path to the agents.json file. Public for test construction with tempdir.
    pub path: PathBuf,
}

impl Default for AgentPersistence {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentPersistence {
    pub fn new() -> Self {
        let path = if let Some(state_dir) = dirs::state_dir() {
            state_dir.join("rookery").join("agents.json")
        } else if let Some(home) = dirs::home_dir() {
            home.join(".local")
                .join("state")
                .join("rookery")
                .join("agents.json")
        } else {
            PathBuf::from("/tmp/rookery-agents.json")
        };
        Self { path }
    }

    pub fn load(&self) -> Result<AgentState> {
        if !self.path.exists() {
            return Ok(AgentState::default());
        }
        let content = std::fs::read_to_string(&self.path)?;
        let state: AgentState = serde_json::from_str(&content)?;
        Ok(state)
    }

    pub fn save(&self, state: &AgentState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(state)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Remove dead agents from persisted state.
    pub fn reconcile(&self, mut state: AgentState) -> AgentState {
        let dead: Vec<String> = state
            .agents
            .iter()
            .filter(|(_name, entry)| !is_process_alive(entry.pid, None))
            .map(|(name, _)| name.clone())
            .collect();

        for name in &dead {
            tracing::warn!(agent = %name, "persisted agent no longer running, removing");
            state.agents.remove(name);
        }

        state
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
            backend_type: BackendType::LlamaServer,
            container_id: None,
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
            backend_type: BackendType::LlamaServer,
            container_id: None,
        };
        let reconciled = persistence.reconcile(state);
        assert!(matches!(reconciled, ServerState::Stopped));
    }

    // === VAL-TRAIT-007: ServerState::Running includes backend_type and container_id ===
    #[test]
    fn test_running_state_has_backend_type_and_container_id() {
        let state = ServerState::Running {
            profile: "vllm_prod".into(),
            pid: 0,
            port: 8081,
            since: Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: BackendType::Vllm,
            container_id: Some("abc123def456".into()),
        };

        let json = serde_json::to_string(&state).unwrap();

        // Verify both new fields appear in the JSON
        assert!(
            json.contains("\"backend_type\""),
            "JSON should contain backend_type"
        );
        assert!(json.contains("\"vllm\""), "JSON should contain vllm value");
        assert!(
            json.contains("\"container_id\""),
            "JSON should contain container_id"
        );
        assert!(
            json.contains("abc123def456"),
            "JSON should contain container_id value"
        );

        // Roundtrip
        let restored: ServerState = serde_json::from_str(&json).unwrap();
        match restored {
            ServerState::Running {
                backend_type,
                container_id,
                ..
            } => {
                assert_eq!(backend_type, BackendType::Vllm);
                assert_eq!(container_id.as_deref(), Some("abc123def456"));
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    // === VAL-TRAIT-008: ServerState backward-compatible deserialization ===
    #[test]
    fn test_state_backward_compat_no_backend_type() {
        // Simulate old state.json without backend_type or container_id fields
        let old_json = r#"{
            "state": "Running",
            "profile": "fast",
            "pid": 12345,
            "port": 8081,
            "since": "2025-01-01T00:00:00Z",
            "command_line": ["llama-server", "-ngl", "99"],
            "exe_path": "/usr/bin/llama-server"
        }"#;

        let state: ServerState = serde_json::from_str(old_json).unwrap();
        match state {
            ServerState::Running {
                profile,
                pid,
                port,
                backend_type,
                container_id,
                command_line,
                exe_path,
                ..
            } => {
                assert_eq!(profile, "fast");
                assert_eq!(pid, 12345);
                assert_eq!(port, 8081);
                // New fields default correctly
                assert_eq!(backend_type, BackendType::LlamaServer);
                assert_eq!(container_id, None);
                // Old fields preserved
                assert_eq!(command_line.len(), 3);
                assert_eq!(exe_path, Some(PathBuf::from("/usr/bin/llama-server")));
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    // === VAL-TRAIT-009: StatePersistence save/load roundtrip with backend metadata ===
    #[test]
    fn test_state_persistence_roundtrip_with_backend_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let persistence = StatePersistence { path };

        // Save a Running state with Vllm type and container_id
        let state = ServerState::Running {
            profile: "vllm_prod".into(),
            pid: 0,
            port: 8081,
            since: Utc::now(),
            command_line: vec!["--model".into(), "test/model".into()],
            exe_path: None,
            backend_type: BackendType::Vllm,
            container_id: Some("container-abc-123".into()),
        };

        persistence.save(&state).unwrap();
        let loaded = persistence.load().unwrap();

        match loaded {
            ServerState::Running {
                profile,
                backend_type,
                container_id,
                command_line,
                ..
            } => {
                assert_eq!(profile, "vllm_prod");
                assert_eq!(backend_type, BackendType::Vllm);
                assert_eq!(container_id.as_deref(), Some("container-abc-123"));
                assert_eq!(command_line, vec!["--model", "test/model"]);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    // === VAL-TRAIT-009 (continued): Roundtrip with LlamaServer defaults ===
    #[test]
    fn test_state_persistence_roundtrip_llama_server_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let persistence = StatePersistence { path };

        let state = ServerState::Running {
            profile: "fast".into(),
            pid: 12345,
            port: 8081,
            since: Utc::now(),
            command_line: vec!["llama-server".into()],
            exe_path: Some(PathBuf::from("/usr/bin/llama-server")),
            backend_type: BackendType::LlamaServer,
            container_id: None,
        };

        persistence.save(&state).unwrap();
        let loaded = persistence.load().unwrap();

        match loaded {
            ServerState::Running {
                pid,
                backend_type,
                container_id,
                ..
            } => {
                assert_eq!(pid, 12345);
                assert_eq!(backend_type, BackendType::LlamaServer);
                assert_eq!(container_id, None);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    // === VAL-TRAIT-009: Save/load roundtrip with BackendType::Vllm and container_id='abc123' ===
    //
    // Explicit test with the exact values from the validation contract:
    // backend_type=Vllm and container_id=Some("abc123").
    #[test]
    fn test_state_persistence_vllm_with_container_id_abc123() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let persistence = StatePersistence { path };

        let since = Utc::now();
        let state = ServerState::Running {
            profile: "vllm_test".into(),
            pid: 0,
            port: 8081,
            since,
            command_line: vec!["--model".into(), "kaitchup/Qwen3.5-27B-NVFP4".into()],
            exe_path: None,
            backend_type: BackendType::Vllm,
            container_id: Some("abc123".into()),
        };

        // Save
        persistence.save(&state).unwrap();

        // Load and assert both fields are preserved
        let loaded = persistence.load().unwrap();
        match loaded {
            ServerState::Running {
                profile,
                backend_type,
                container_id,
                port,
                ..
            } => {
                assert_eq!(profile, "vllm_test");
                assert_eq!(backend_type, BackendType::Vllm);
                assert_eq!(
                    container_id.as_deref(),
                    Some("abc123"),
                    "container_id must survive save/load roundtrip"
                );
                assert_eq!(port, 8081);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    // === VAL-TRAIT-009: reconcile() with dead PID returns Stopped ===
    //
    // When the daemon restarts and the previously-running process has died,
    // reconcile() should return Stopped regardless of backend_type.
    #[test]
    fn test_reconcile_dead_pid_returns_stopped_with_vllm() {
        let persistence = StatePersistence::new();
        let state = ServerState::Running {
            profile: "vllm_prod".into(),
            pid: 999_999_999, // non-existent PID
            port: 8081,
            since: Utc::now(),
            command_line: vec![],
            exe_path: None,
            backend_type: BackendType::Vllm,
            container_id: Some("abc123".into()),
        };

        let reconciled = persistence.reconcile(state);
        assert!(
            matches!(reconciled, ServerState::Stopped),
            "reconcile with dead PID should return Stopped, got {reconciled:?}"
        );
    }
}
