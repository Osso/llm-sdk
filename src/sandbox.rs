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

/// Resolve symlinks so bwrap gets real paths (it can't mkdir through symlinks).
fn canonicalize_or_keep(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Build bwrap args for a developer agent.
/// Worktree is mounted writable at `/repo`.
/// The main repo's `.git` dir is also mounted writable so the worktree can write objects/refs.
///
/// Note: --proc /proc is omitted because Bun (Claude CLI runtime) hangs
/// when bwrap mounts a synthetic procfs.
pub fn developer_prefix(worktree_path: &Path, git_dir: Option<&Path>) -> Vec<String> {
    let worktree = canonicalize_or_keep(worktree_path);
    let claude_dir = claude_config_dir();
    let mcp_state = mcp_state_dir();
    let mut args: Vec<String> = [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--bind", "/tmp", "/tmp",
        "--bind", &worktree, REPO_MOUNT,
        "--bind", &claude_dir, &claude_dir,
        "--bind", &mcp_state, &mcp_state,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if let Some(gd) = git_dir {
        let gd_str = canonicalize_or_keep(gd);
        args.extend(["--bind".to_string(), gd_str.clone(), gd_str]);
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
    let project = canonicalize_or_keep(project_path);
    let claude_dir = claude_config_dir();
    let mcp_state = mcp_state_dir();
    [
        "bwrap",
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--bind", "/tmp", "/tmp",
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
        // Find the --bind that maps the worktree to /tmp/repo
        let repo_bind = prefix.windows(3)
            .position(|w| w[0] == "--bind" && w[2] == REPO_MOUNT)
            .expect("should have --bind <worktree> /tmp/repo");
        assert_eq!(prefix[repo_bind + 1], "/home/user/.worktrees/dev-0");
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
    fn developer_prefix_mounts_git_dir_writable() {
        let worktree = PathBuf::from("/home/user/.worktrees/dev-0");
        let git_dir = PathBuf::from("/home/user/project/.git");
        let prefix = developer_prefix(&worktree, Some(&git_dir));

        // Find all --bind (RW) mounts
        let rw_binds: Vec<usize> = prefix
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_str() == "--bind")
            .map(|(i, _)| i)
            .collect();

        // .git dir must appear as a --bind (not --ro-bind)
        let git_bound = rw_binds.iter().any(|&i| {
            prefix.get(i + 1).is_some_and(|s| s == "/home/user/project/.git")
        });
        assert!(git_bound, "git dir must be mounted RW via --bind, got: {:?}", prefix);
    }

    #[test]
    fn developer_prefix_git_dir_is_writable_in_sandbox() {
        if !is_available() {
            return;
        }
        // Create a real temp worktree and .git dir to test actual bwrap
        let tmp = std::env::temp_dir().join("sandbox-git-test");
        let worktree = tmp.join("worktree");
        let git_dir = tmp.join("dotgit");
        let _ = std::fs::create_dir_all(&worktree);
        let _ = std::fs::create_dir_all(&git_dir);

        let prefix = developer_prefix(&worktree, Some(&git_dir));
        let test_file = git_dir.join("write-test");
        let status = std::process::Command::new(&prefix[0])
            .args(&prefix[1..])
            .arg("touch")
            .arg(test_file.to_string_lossy().as_ref())
            .status()
            .expect("failed to run bwrap");

        assert!(status.success(), "writing to .git dir inside sandbox must succeed");
        assert!(test_file.exists(), ".git write must be visible on host");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn developer_prefix_resolves_symlinks() {
        let tmp = std::env::temp_dir().join("sandbox-symlink-test");
        let real_dir = tmp.join("real");
        let link = tmp.join("link");
        let _ = std::fs::create_dir_all(&real_dir);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let prefix = developer_prefix(&link, Some(&link));
        let real_str = real_dir.to_string_lossy().to_string();

        // Worktree bind should use resolved path (find the one mapping to /tmp/repo)
        let bind_idx = prefix.windows(3)
            .position(|w| w[0] == "--bind" && w[2] == REPO_MOUNT)
            .expect("should have --bind <worktree> /tmp/repo");
        assert_eq!(prefix[bind_idx + 1], real_str, "worktree must be canonicalized");

        // Git dir bind should also use resolved path
        let link_str = link.to_string_lossy().to_string();
        assert!(
            !prefix.contains(&link_str),
            "symlink path must not appear in bwrap args: {:?}", prefix
        );

        let _ = std::fs::remove_dir_all(&tmp);
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
