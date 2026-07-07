//! Git branch lookup for the sidebar, from a pane's known cwd.
//!
//! Reads `.git/HEAD` directly instead of shelling out to `git`: HEAD's
//! `ref: refs/heads/<branch>` line names the branch regardless of whether
//! that ref is packed, so no subprocess or `packed-refs` parsing is
//! needed. Correct for worktrees, where `.git` is a file pointing at a
//! private `gitdir` with its own `HEAD`.
//!
//! Cwd here is always a local path: today's only frontends are the local
//! TUI and an attach client on the same machine (see
//! `session/remote.rs`). If a networked SSH remote is added later, this
//! lookup needs to move to wherever the PTY actually runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(2);

/// Walks up from `start` to find the enclosing repo's git dir, resolving
/// worktree/submodule `.git` files to their real `gitdir`.
fn find_git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let contents = std::fs::read_to_string(&candidate).ok()?;
            let rest = contents.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = PathBuf::from(rest);
            return Some(if gitdir.is_absolute() { gitdir } else { dir.join(gitdir) });
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Branch name from a git dir's `HEAD`, or a short sha when detached.
fn read_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if head.len() >= 7 && head.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        Some(format!("@{}", &head[..7]))
    } else {
        None
    }
}

fn branch_for_path(cwd: &str) -> Option<String> {
    let git_dir = find_git_dir(Path::new(cwd))?;
    read_branch(&git_dir)
}

/// Per-cwd branch cache so the sidebar's per-frame redraw doesn't restat
/// `.git/HEAD` more than a couple of times a second.
#[derive(Default)]
pub struct GitInfoCache {
    entries: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl GitInfoCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn branch_for(&self, cwd: &str) -> Option<String> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        if let Some((checked_at, branch)) = entries.get(cwd) {
            if now.duration_since(*checked_at) < TTL {
                return branch.clone();
            }
        }
        let branch = branch_for_path(cwd);
        entries.insert(cwd.to_string(), (now, branch.clone()));
        branch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_branch_from_head() {
        let dir = std::env::temp_dir().join(format!("git-info-test-{}", std::process::id()));
        let git_dir = dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/thing\n").unwrap();

        assert_eq!(branch_for_path(dir.to_str().unwrap()), Some("feature/thing".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detached_head_shows_short_sha() {
        let dir = std::env::temp_dir().join(format!("git-info-test-detached-{}", std::process::id()));
        let git_dir = dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "a78fe53efaaea56b80d47569d85e0d7b76512aa7\n").unwrap();

        assert_eq!(branch_for_path(dir.to_str().unwrap()), Some("@a78fe53".to_string()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn worktree_gitdir_pointer_resolves() {
        // Mirrors real git layout: the worktree's `.git` file points at a
        // private admin dir (normally `<main>/.git/worktrees/<name>`) that
        // holds its own HEAD, separate from the main repo's.
        let base = std::env::temp_dir().join(format!("git-info-test-wt-{}", std::process::id()));
        let worktree_admin_dir = base.join("main-repo-git").join("worktrees").join("feature");
        let worktree_dir = base.join("worktree");
        std::fs::create_dir_all(&worktree_admin_dir).unwrap();
        std::fs::create_dir_all(&worktree_dir).unwrap();
        std::fs::write(worktree_admin_dir.join("HEAD"), "ref: refs/heads/worktree-branch\n")
            .unwrap();
        std::fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}\n", worktree_admin_dir.display()),
        )
        .unwrap();

        assert_eq!(
            branch_for_path(worktree_dir.to_str().unwrap()),
            Some("worktree-branch".to_string())
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn non_git_directory_returns_none() {
        assert_eq!(branch_for_path(std::env::temp_dir().to_str().unwrap()), None);
    }
}
