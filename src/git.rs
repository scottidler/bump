use eyre::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Check if the given path is inside a git repository
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(path)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Get the latest semver tag (tags starting with 'v')
pub fn get_latest_tag(path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["tag", "-l", "v*", "--sort=-v:refname"])
        .current_dir(path)
        .output()
        .context("Failed to run git tag")?;

    if !output.status.success() {
        bail!("git tag failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let tags = String::from_utf8_lossy(&output.stdout);
    Ok(tags.lines().next().map(|s| s.to_string()))
}

/// Check if a specific tag exists
pub fn tag_exists(path: &Path, tag: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["tag", "-l", tag])
        .current_dir(path)
        .output()
        .context("Failed to run git tag")?;

    if !output.status.success() {
        bail!("git tag failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let result = String::from_utf8_lossy(&output.stdout);
    Ok(!result.trim().is_empty())
}

/// Stage all changes (git add -A)
pub fn stage_all(path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .output()
        .context("Failed to run git add")?;

    if !output.status.success() {
        bail!("git add failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Get list of staged files
pub fn get_staged_files(path: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(path)
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        bail!("git diff failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let files = String::from_utf8_lossy(&output.stdout);
    Ok(files.lines().map(|s| s.to_string()).collect())
}

/// Create a commit with the given message
pub fn commit(path: &Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .output()
        .context("Failed to run git commit")?;

    if !output.status.success() {
        bail!("git commit failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Create an annotated tag with the given message
pub fn create_tag(path: &Path, tag: &str, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["tag", "-a", tag, "-m", message])
        .current_dir(path)
        .output()
        .context("Failed to run git tag")?;

    if !output.status.success() {
        bail!("git tag failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Check if HEAD has an annotated tag pointing directly at it
pub fn head_has_tag(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["describe", "--exact-match", "HEAD"])
        .current_dir(path)
        .output()
        .context("Failed to run git describe")?;

    // If the command succeeds, HEAD has a tag
    Ok(output.status.success())
}

/// Check if HEAD has been pushed to the remote tracking branch
/// Returns false if there's no upstream or if HEAD is ahead of upstream
pub fn is_head_pushed(path: &Path) -> Result<bool> {
    // First check if we have an upstream
    let upstream_check = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
        .current_dir(path)
        .output()
        .context("Failed to check upstream")?;

    if !upstream_check.status.success() {
        // No upstream configured - not pushed
        return Ok(false);
    }

    // Check if HEAD is an ancestor of (or equal to) the upstream
    // If HEAD is ahead of upstream, this will fail
    let merge_base = Command::new("git")
        .args(["merge-base", "--is-ancestor", "HEAD", "@{u}"])
        .current_dir(path)
        .output()
        .context("Failed to check merge base")?;

    Ok(merge_base.status.success())
}

/// Amend the previous commit without changing the message
pub fn amend_commit_no_edit(path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "--amend", "--no-edit"])
        .current_dir(path)
        .output()
        .context("Failed to run git commit --amend")?;

    if !output.status.success() {
        bail!("git commit --amend failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Check if there are any uncommitted changes (staged or unstaged)
pub fn has_uncommitted_changes(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        bail!("git status failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let status = String::from_utf8_lossy(&output.stdout);
    Ok(!status.trim().is_empty())
}

/// Relation of local HEAD to the remote tracking branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadRemote {
    /// HEAD points at the same commit as the remote branch.
    Equal,
    /// HEAD has commits the remote does not (the bump commit isn't merged/pushed yet).
    Ahead,
    /// The remote has commits HEAD does not (local is stale).
    Behind,
    /// Histories have diverged.
    Diverged,
}

/// Run `git rev-parse <rev>` and return the resolved SHA.
fn rev_parse(path: &Path, rev: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(path)
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        bail!(
            "git rev-parse {} failed: {}",
            rev,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Is `ancestor` an ancestor of (or equal to) `descendant`?
fn is_ancestor(path: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(path)
        .output()
        .context("Failed to run git merge-base")?;
    Ok(output.status.success())
}

/// The SHA at local HEAD.
pub fn head_sha(path: &Path) -> Result<String> {
    rev_parse(path, "HEAD")
}

/// The current branch name (`git rev-parse --abbrev-ref HEAD`).
pub fn current_branch(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .context("Failed to run git rev-parse --abbrev-ref")?;

    if !output.status.success() {
        bail!(
            "git rev-parse --abbrev-ref HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve the remote default branch from `refs/remotes/origin/HEAD`.
pub fn remote_default_branch(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(path)
        .output()
        .context("Failed to run git symbolic-ref")?;

    if !output.status.success() {
        bail!(
            "could not determine the remote default branch (is origin/HEAD set?). \
             Try: git remote set-head origin -a"
        );
    }

    String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("refs/remotes/origin/")
        .map(str::to_string)
        .ok_or_else(|| eyre::eyre!("unexpected symbolic-ref output for origin/HEAD"))
}

/// Fetch a single branch from origin (updates the remote-tracking ref).
pub fn fetch_branch(path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["fetch", "origin", branch])
        .current_dir(path)
        .output()
        .context("Failed to run git fetch")?;

    if !output.status.success() {
        bail!(
            "git fetch origin {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Compare local HEAD to `origin/<branch>` (call `fetch_branch` first).
pub fn compare_head_to_remote(path: &Path, branch: &str) -> Result<HeadRemote> {
    let head = head_sha(path)?;
    let remote_ref = format!("origin/{branch}");
    let remote = rev_parse(path, &remote_ref)?;

    if head == remote {
        return Ok(HeadRemote::Equal);
    }

    let head_is_ancestor = is_ancestor(path, "HEAD", &remote_ref)?;
    let remote_is_ancestor = is_ancestor(path, &remote_ref, "HEAD")?;

    Ok(match (head_is_ancestor, remote_is_ancestor) {
        (true, false) => HeadRemote::Behind,
        (false, true) => HeadRemote::Ahead,
        _ => HeadRemote::Diverged,
    })
}

/// The commit SHA a local tag points to (annotated tags are dereferenced).
pub fn tag_sha(path: &Path, tag: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-list", "-n", "1", tag])
        .current_dir(path)
        .output()
        .context("Failed to run git rev-list")?;

    if !output.status.success() {
        bail!(
            "git rev-list {} failed: {}",
            tag,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The commit SHA a tag points to ON THE REMOTE, or `None` if the remote has no
/// such tag. Annotated tags are dereferenced via the `^{}` peeled line.
pub fn remote_tag_sha(path: &Path, tag: &str) -> Result<Option<String>> {
    let refspec = format!("refs/tags/{tag}");
    let output = Command::new("git")
        .args(["ls-remote", "origin", &refspec])
        .current_dir(path)
        .output()
        .context("Failed to run git ls-remote")?;

    if !output.status.success() {
        bail!(
            "git ls-remote origin {} failed: {}",
            refspec,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let peeled = format!("{refspec}^{{}}");
    let mut plain_sha = None;
    for line in stdout.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        if name == peeled {
            // Peeled commit of an annotated tag: this is what the tag points to.
            return Ok(Some(sha.trim().to_string()));
        }
        if name == refspec {
            plain_sha = Some(sha.trim().to_string());
        }
    }
    Ok(plain_sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_is_git_repo_current_dir() {
        // The bump project itself should be a git repo
        let cwd = env::current_dir().unwrap();
        assert!(is_git_repo(&cwd));
    }

    #[test]
    fn test_is_git_repo_not_repo() {
        // /tmp is unlikely to be a git repo
        assert!(!is_git_repo(Path::new("/tmp")));
    }

    #[test]
    fn test_get_latest_tag() {
        // Just verify it doesn't error on the current repo
        let cwd = env::current_dir().unwrap();
        let result = get_latest_tag(&cwd);
        assert!(result.is_ok());
    }

    #[test]
    fn test_tag_exists_nonexistent() {
        let cwd = env::current_dir().unwrap();
        let result = tag_exists(&cwd, "v999.999.999");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_head_has_tag() {
        // Just verify it doesn't error on the current repo
        let cwd = env::current_dir().unwrap();
        let result = head_has_tag(&cwd);
        assert!(result.is_ok());
        // The actual value depends on whether HEAD has a tag
    }

    #[test]
    fn test_is_head_pushed() {
        // Just verify it doesn't error on the current repo
        let cwd = env::current_dir().unwrap();
        let result = is_head_pushed(&cwd);
        assert!(result.is_ok());
        // The actual value depends on remote state
    }

    #[test]
    fn test_has_uncommitted_changes() {
        let cwd = env::current_dir().unwrap();
        let result = has_uncommitted_changes(&cwd);
        assert!(result.is_ok());
        // The actual value depends on working tree state
    }
}
