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

/// Writable state directory for MCP inside the sandbox.
/// ~/.claude.json should be a symlink pointing here.
fn mcp_state_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join(".local/state/agent-orchestrator-mcp"))
        .unwrap_or_else(|| "/tmp/agent-orchestrator-mcp".into())
        .to_string_lossy()
        .into_owned()
}

/// Ensure the MCP state directory exists.
pub fn ensure_state_dirs() {
    let _ = std::fs::create_dir_all(mcp_state_dir());
}

/// Build bwrap args for a developer agent.
/// Worktree is mounted writable at `/repo`.
/// The main repo's `.git` dir is also mounted writable so the worktree can write objects/refs.
///
/// Note: --proc /proc is omitted because Bun (Claude CLI runtime) hangs
/// when bwrap mounts a synthetic procfs.
pub fn developer_prefix(worktree_path: &Path, git_dir: Option<&Path>) -> Vec<String> {
    let worktree = worktree_path.to_string_lossy();
    let claude_dir = claude_config_dir();
    let mcp_state = mcp_state_dir();
    let mut args: Vec<String> = [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--bind", &worktree, REPO_MOUNT,
        "--bind", &claude_dir, &claude_dir,
        "--bind", &mcp_state, &mcp_state,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if let Some(gd) = git_dir {
        let gd_str = gd.to_string_lossy();
        args.extend(["--bind".to_string(), gd_str.to_string(), gd_str.to_string()]);
    }

    args.extend([
        "--chdir".to_string(), REPO_MOUNT.to_string(),
        "--die-with-parent".to_string(),
        "--".to_string(),
    ]);
    args
}

/// Build bwrap args for a read-only sandbox (non-developer agents).
/// Project is mounted read-only at `/repo`.
pub fn readonly_prefix(project_path: &Path) -> Vec<String> {
    let project = project_path.to_string_lossy();
    let claude_dir = claude_config_dir();
    let mcp_state = mcp_state_dir();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--ro-bind", &project, REPO_MOUNT,
        "--bind", &claude_dir, &claude_dir,
        "--bind", &mcp_state, &mcp_state,
        "--chdir", REPO_MOUNT,
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
        let prefix = developer_prefix(&worktree, None);

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
    fn developer_prefix_binds_mcp_state_writable() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let prefix = developer_prefix(&worktree, None);

        let home = dirs::home_dir().unwrap();
        let mcp_state = home
            .join(".local/state/agent-orchestrator-mcp")
            .to_string_lossy()
            .into_owned();
        assert!(
            prefix.contains(&mcp_state),
            "developer sandbox must bind mcp state dir writable"
        );
    }

    #[test]
    fn readonly_prefix_binds_mcp_state_writable() {
        let project = PathBuf::from("/tmp/test-project");
        let prefix = readonly_prefix(&project);

        let home = dirs::home_dir().unwrap();
        let mcp_state = home
            .join(".local/state/agent-orchestrator-mcp")
            .to_string_lossy()
            .into_owned();
        assert!(
            prefix.contains(&mcp_state),
            "readonly sandbox must bind mcp state dir writable"
        );
    }

    #[test]
    fn no_claude_json_bind_mount() {
        let dev_prefix = developer_prefix(Path::new("/tmp/w"), None);
        let ro_prefix = readonly_prefix(Path::new("/tmp/p"));
        for prefix in [&dev_prefix, &ro_prefix] {
            assert!(
                !prefix.iter().any(|s| s.ends_with(".claude.json")),
                "sandbox must not bind-mount ~/.claude.json directly; use state dir symlink instead"
            );
        }
    }

    #[test]
    fn mcp_state_dir_is_writable_in_sandbox() {
        if !is_available() {
            return; // skip if bwrap not installed
        }
        ensure_state_dirs();
        let project = PathBuf::from("/tmp");
        let prefix = readonly_prefix(&project);
        let state = mcp_state_dir();
        let test_file = format!("{}/write-test", state);
        let status = std::process::Command::new(&prefix[0])
            .args(&prefix[1..])
            .arg("touch")
            .arg(&test_file)
            .status()
            .expect("failed to run bwrap");
        assert!(status.success(), "touch inside sandbox must succeed");
        assert!(
            std::path::Path::new(&test_file).exists(),
            "file written inside sandbox must be visible on host"
        );
        let _ = std::fs::remove_file(&test_file);
    }

    #[test]
    fn developer_prefix_chdirs_to_repo_mount() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let prefix = developer_prefix(&worktree, None);

        let chdir_idx = prefix.iter().position(|s| s == "--chdir");
        assert!(chdir_idx.is_some(), "developer prefix must include --chdir");
        assert_eq!(prefix[chdir_idx.unwrap() + 1], REPO_MOUNT);
    }

    #[test]
    fn readonly_prefix_chdirs_to_repo_mount() {
        let project = PathBuf::from("/tmp/test-project");
        let prefix = readonly_prefix(&project);

        let chdir_idx = prefix.iter().position(|s| s == "--chdir");
        assert!(chdir_idx.is_some(), "readonly prefix must include --chdir");
        assert_eq!(prefix[chdir_idx.unwrap() + 1], REPO_MOUNT);
    }
}
