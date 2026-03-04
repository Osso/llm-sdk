//! Bubblewrap (bwrap) sandbox for agent isolation.
//!
//! Developers get their worktree bind-mounted at the project path so Claude
//! sees the real project location but writes go to the isolated worktree.
//! Non-developer agents get a read-only sandbox.

use std::path::Path;

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
/// Worktree is mounted at the project path so writes land in the worktree.
///
/// Note: --proc /proc is omitted because Bun (Claude CLI runtime) hangs
/// when bwrap mounts a synthetic procfs.
pub fn developer_prefix(worktree_path: &Path, project_path: &Path) -> Vec<String> {
    let worktree = worktree_path.to_string_lossy();
    let project = project_path.to_string_lossy();
    let claude_dir = claude_config_dir();
    let claude_json = claude_json_file();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
        "--bind", &worktree, &project,
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
pub fn readonly_prefix() -> Vec<String> {
    let claude_dir = claude_config_dir();
    let claude_json = claude_json_file();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--tmpfs", "/tmp",
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
    fn developer_prefix_mounts_worktree_at_project() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let project = PathBuf::from("/home/user/projects/myapp");
        let prefix = developer_prefix(&worktree, &project);

        assert_eq!(prefix[0], "bwrap");
        let bind_idx = prefix.iter().position(|s| s == "--bind").unwrap();
        assert_eq!(prefix[bind_idx + 1], "/home/user/.worktrees/dev-0");
        assert_eq!(prefix[bind_idx + 2], "/home/user/projects/myapp");
        assert!(!prefix.contains(&"--proc".to_string()));
        assert_eq!(prefix.last().unwrap(), "--");
    }

    #[test]
    fn readonly_prefix_has_no_writable_project_bind() {
        let prefix = readonly_prefix();
        assert_eq!(prefix[0], "bwrap");
        assert!(prefix.contains(&"--ro-bind".to_string()));
        // Only --bind should be for ~/.claude
        let bind_positions: Vec<_> = prefix
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--bind")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(bind_positions.len(), 2, "~/.claude and ~/.claude.json should be writable");
        assert_eq!(prefix.last().unwrap(), "--");
    }

    #[test]
    fn developer_prefix_binds_claude_json_writable() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let project = PathBuf::from("/home/user/projects/myapp");
        let prefix = developer_prefix(&worktree, &project);

        // ~/.claude.json must be writable for MCP server config
        let home = dirs::home_dir().unwrap();
        let claude_json = home.join(".claude.json").to_string_lossy().into_owned();
        assert!(
            prefix.contains(&claude_json),
            "developer sandbox must bind ~/.claude.json writable"
        );
    }

    #[test]
    fn readonly_prefix_binds_claude_json_writable() {
        let prefix = readonly_prefix();

        let home = dirs::home_dir().unwrap();
        let claude_json = home.join(".claude.json").to_string_lossy().into_owned();
        assert!(
            prefix.contains(&claude_json),
            "readonly sandbox must bind ~/.claude.json writable"
        );
    }
}
