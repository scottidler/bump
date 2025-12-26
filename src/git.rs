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
}
