use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result, io_error};

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    Sandbox,
    FullAccess,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub access: AccessMode,
}

impl Default for WorkspacePolicy {
    fn default() -> Self {
        Self {
            access: AccessMode::Sandbox,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkspaceRecord {
    pub id: String,
    pub canonical_path: String,
    pub status: String,
    pub policy: WorkspacePolicy,
    pub policy_fingerprint: String,
    pub agents: Vec<WorkspaceAgentRecord>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkspaceAgentRecord {
    pub agent_kind: String,
    pub status: String,
    pub capabilities: serde_json::Value,
    pub policy_fingerprint: Option<String>,
    pub checked_at: Option<String>,
}

pub struct CanonicalWorkspace {
    pub path: PathBuf,
    pub display: String,
    pub key: String,
}

pub fn canonicalize(path: &Path) -> Result<CanonicalWorkspace> {
    let metadata = fs::metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.is_dir() {
        return Err(ReactorError::InvalidWorkspacePath(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    let path = dunce::canonicalize(path).map_err(|source| io_error(path, source))?;
    let display = path
        .to_str()
        .ok_or_else(|| ReactorError::InvalidWorkspacePath("path is not valid UTF-8".to_owned()))?
        .to_owned();
    let key = path_key(&display);
    Ok(CanonicalWorkspace { path, display, key })
}

pub fn validate_id(id: &str) -> Result<()> {
    let mut characters = id.chars();
    let valid_first = characters
        .next()
        .is_some_and(|value| value.is_ascii_alphanumeric());
    let valid_rest = characters.all(|value| value.is_ascii_alphanumeric() || ".-_".contains(value));
    if !valid_first || !valid_rest || id.len() > 64 {
        return Err(ReactorError::InvalidWorkspaceId(id.to_owned()));
    }
    Ok(())
}

pub fn fingerprint(workspace: &CanonicalWorkspace, policy: &WorkspacePolicy) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(workspace.key.as_bytes());
    hash.update(
        &serde_json::to_vec(policy)
            .map_err(|error| ReactorError::InvalidInput(error.to_string()))?,
    );
    for relative in ["AGENTS.md", ".codex/config.toml"] {
        let path = workspace.path.join(relative);
        match fs::read(&path) {
            Ok(bytes) => {
                hash.update(relative.as_bytes());
                hash.update(&bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(path, source)),
        }
    }
    Ok(hash.finalize().to_hex().to_string())
}

pub fn paths_overlap(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(windows)]
fn path_key(path: &str) -> String {
    path.to_lowercase()
}

#[cfg(not(windows))]
fn path_key(path: &str) -> String {
    path.to_owned()
}
