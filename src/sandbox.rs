//! Bubblewrap (bwrap) sandbox for agent isolation.
//!
//! All agents see the project at `/repo` inside the sandbox.
//! Developers get their worktree bind-mounted writable at `/repo`.
//! Non-developer agents get the real project read-only at `/repo`.

use std::path::Path;

/// Standard mount point for the project inside the sandbox.
pub const REPO_MOUNT: &str = "/tmp/repo";

/// Resolve the Claude config directory (~/.claude) that must be writable.
fn claude_config_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join(".claude"))
        .unwrap_or_else(|| "/tmp/.claude".into())
        .to_string_lossy()
        .into_owned()
}

/// Resolve ~/.claude.json (MCP server config) that must be writable.
fn claude_json_file() -> String {
    dirs::home_dir()
        .map(|h| h.join(".claude.json"))
        .unwrap_or_else(|| "/tmp/.claude.json".into())
        .to_string_lossy()
        .into_owned()
}

/// Build bwrap args for a developer agent.
/// Worktree is mounted writable at `/repo`.
///
/// Note: --proc /proc is omitted because Bun (Claude CLI runtime) hangs
/// when bwrap mounts a synthetic procfs.
pub fn developer_prefix(worktree_path: &Path) -> Vec<String> {
    let worktree = worktree_path.to_string_lossy();
    let claude_dir = claude_config_dir();
    let claude_json = claude_json_file();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--bind", &worktree, REPO_MOUNT,
        "--bind", &claude_dir, &claude_dir,
        "--bind", &claude_json, &claude_json,
        "--die-with-parent",
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Build bwrap args for a read-only sandbox (non-developer agents).
/// Project is mounted read-only at `/repo`.
pub fn readonly_prefix(project_path: &Path) -> Vec<String> {
    let project = project_path.to_string_lossy();
    let claude_dir = claude_config_dir();
    let claude_json = claude_json_file();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--ro-bind", &project, REPO_MOUNT,
        "--bind", &claude_dir, &claude_dir,
        "--bind", &claude_json, &claude_json,
        "--die-with-parent",
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Check whether `bwrap` is available in PATH.
pub fn is_available() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn developer_prefix_mounts_worktree_at_repo() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let prefix = developer_prefix(&worktree);

        assert_eq!(prefix[0], "bwrap");
        let bind_idx = prefix.iter().position(|s| s == "--bind").unwrap();
        assert_eq!(prefix[bind_idx + 1], "/home/user/.worktrees/dev-0");
        assert_eq!(prefix[bind_idx + 2], REPO_MOUNT);
        assert!(!prefix.contains(&"--proc".to_string()));
        assert_eq!(prefix.last().unwrap(), "--");
    }

    #[test]
    fn readonly_prefix_mounts_project_readonly_at_repo() {
        let project = PathBuf::from("/tmp/test-project");
        let prefix = readonly_prefix(&project);

        assert_eq!(prefix[0], "bwrap");
        // Project should be --ro-bind at /repo
        let ro_binds: Vec<_> = prefix
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--ro-bind")
            .map(|(i, _)| i)
            .collect();
        // Two ro-binds: / -> / and project -> /repo
        assert_eq!(ro_binds.len(), 2);
        let proj_bind = ro_binds[1];
        assert_eq!(prefix[proj_bind + 1], "/tmp/test-project");
        assert_eq!(prefix[proj_bind + 2], REPO_MOUNT);
        assert_eq!(prefix.last().unwrap(), "--");
    }

    #[test]
    fn developer_prefix_binds_claude_json_writable() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let prefix = developer_prefix(&worktree);

        let home = dirs::home_dir().unwrap();
        let claude_json = home.join(".claude.json").to_string_lossy().into_owned();
        assert!(
            prefix.contains(&claude_json),
            "developer sandbox must bind ~/.claude.json writable"
        );
    }

    #[test]
    fn readonly_prefix_binds_claude_json_writable() {
        let project = PathBuf::from("/tmp/test-project");
        let prefix = readonly_prefix(&project);

        let home = dirs::home_dir().unwrap();
        let claude_json = home.join(".claude.json").to_string_lossy().into_owned();
        assert!(
            prefix.contains(&claude_json),
            "readonly sandbox must bind ~/.claude.json writable"
        );
    }
}
