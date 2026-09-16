use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result, io_error};

const MAX_POLICY_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAccess {
    Deny,
    #[default]
    Allow,
}

impl PolicyAccess {
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub writable_roots: Vec<String>,
    pub network: PolicyAccess,
    pub mcp: PolicyAccess,
    pub hooks: PolicyAccess,
}

impl WorkspacePolicy {
    pub fn write_paths(&self, workspace: &Path) -> Vec<PathBuf> {
        std::iter::once(workspace.to_owned())
            .chain(self.writable_roots.iter().map(PathBuf::from))
            .collect()
    }

    pub fn write_keys(&self, workspace_key: &str) -> Vec<String> {
        std::iter::once(workspace_key.to_owned())
            .chain(self.writable_roots.iter().map(|path| path_key(path)))
            .collect()
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

impl WorkspaceRecord {
    #[allow(dead_code)]
    pub fn write_paths(&self) -> Vec<PathBuf> {
        self.policy.write_paths(Path::new(&self.canonical_path))
    }
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

pub fn normalize_policy(
    workspace: &CanonicalWorkspace,
    policy: &WorkspacePolicy,
) -> Result<WorkspacePolicy> {
    let mut roots: Vec<CanonicalWorkspace> = Vec::new();
    for value in &policy.writable_roots {
        if value.trim().is_empty() {
            return Err(ReactorError::InvalidSandboxPolicy(
                "writable root must not be empty".to_owned(),
            ));
        }
        let input = Path::new(value);
        let input = if input.is_absolute() {
            input.to_owned()
        } else {
            workspace.path.join(input)
        };
        let root = canonicalize(&input)?;
        if root.path.parent().is_none() {
            return Err(ReactorError::InvalidSandboxPolicy(format!(
                "filesystem root is not allowed: {}",
                root.display
            )));
        }
        if root.key == workspace.key || root.path.starts_with(&workspace.path) {
            continue;
        }
        if workspace.path.starts_with(&root.path) {
            return Err(ReactorError::InvalidSandboxPolicy(format!(
                "writable root contains the workspace: {}",
                root.display
            )));
        }
        if roots
            .iter()
            .any(|registered| paths_overlap(&registered.key, &root.key))
        {
            return Err(ReactorError::InvalidSandboxPolicy(format!(
                "writable roots overlap: {}",
                root.display
            )));
        }
        roots.push(root);
    }
    roots.sort_by(|left, right| left.key.cmp(&right.key));

    Ok(WorkspacePolicy {
        writable_roots: roots.into_iter().map(|root| root.display).collect(),
        network: policy.network,
        mcp: policy.mcp,
        hooks: policy.hooks,
    })
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

pub fn fingerprint(
    workspace: &CanonicalWorkspace,
    policy: &WorkspacePolicy,
    policy_files: &[&str],
) -> Result<String> {
    let mut hash = blake3::Hasher::new();
    hash.update(workspace.key.as_bytes());
    hash.update(b"\0");
    hash.update(
        &serde_json::to_vec(policy)
            .map_err(|error| ReactorError::InvalidInput(error.to_string()))?,
    );
    hash.update(b"\0");
    for relative in policy_files {
        let path = workspace.path.join(relative);
        match File::open(&path) {
            Ok(mut file) => {
                let size = file
                    .metadata()
                    .map_err(|source| io_error(&path, source))?
                    .len();
                if size > MAX_POLICY_FILE_BYTES {
                    return Err(ReactorError::InvalidInput(format!(
                        "policy file exceeds 1 MiB: {relative}"
                    )));
                }
                hash.update(relative.as_bytes());
                hash.update(b"\0");
                let mut buffer = [0_u8; 8192];
                let mut total = 0_u64;
                loop {
                    let count = file
                        .read(&mut buffer)
                        .map_err(|source| io_error(&path, source))?;
                    if count == 0 {
                        break;
                    }
                    total += count as u64;
                    if total > MAX_POLICY_FILE_BYTES {
                        return Err(ReactorError::InvalidInput(format!(
                            "policy file exceeds 1 MiB: {relative}"
                        )));
                    }
                    hash.update(&buffer[..count]);
                }
                hash.update(b"\0");
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
