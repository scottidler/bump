use eyre::{Context, Result, bail};
use log::debug;
use std::path::{Path, PathBuf};
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
    debug!("is_head_pushed: path={}", path.display());

    // A branch cut with `--no-track` (the standalone release path, and any feature
    // branch someone forgot to set an upstream on) has no `@{u}` at all, but its tip can
    // still be sitting on origin -- e.g. cut straight from origin/<default>. Checking any
    // `origin/*` ref containment FIRST catches that case; the `@{u}` check below only
    // ever sees branches that DO have an upstream configured.
    let contained = Command::new("git")
        .args(["branch", "-r", "--contains", "HEAD"])
        .current_dir(path)
        .output()
        .context("Failed to run git branch -r --contains")?;
    if contained.status.success() && !String::from_utf8_lossy(&contained.stdout).trim().is_empty() {
        debug!("is_head_pushed: HEAD is contained in a remote-tracking ref");
        return Ok(true);
    }

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

/// Amend the previous commit, REPLACING its message. For an explicit `--message` or
/// `--automatic` on the amend path, where the plain `--no-edit` amend above would
/// otherwise silently keep the prior commit's message and ignore what was asked for.
pub fn amend_commit_with_message(path: &Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "--amend", "-m", message])
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

/// Check if there are any TRACKED uncommitted changes (staged or modified), ignoring
/// untracked files. For paths that never `git add`: an untracked file can't ride into
/// a commit that's never made, so it isn't a reason to refuse. Contrast with
/// `has_uncommitted_changes`, which callers that DO `git add -A` must keep using.
pub fn has_tracked_changes(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(path)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        bail!("git status failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let status = String::from_utf8_lossy(&output.stdout);
    Ok(!status.trim().is_empty())
}

/// List the paths git considers dirty (staged, modified, or untracked) via
/// `git status --porcelain`. Captured BEFORE bump mutates anything so the lockfile
/// guard can tell a bump-synced lockfile from one the user had already changed.
pub fn dirty_files(path: &Path) -> Result<Vec<String>> {
    debug!("dirty_files: path={}", path.display());
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output()
        .context("Failed to run git status")?;

    if !output.status.success() {
        bail!("git status failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let status = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();
    for line in status.lines() {
        // Porcelain v1 lines are "XY <path>": two ASCII status columns + a space, so
        // the path starts at byte 3 (always a valid char boundary). `get` avoids the
        // string-slice panic footgun on any unexpected short line.
        let Some(rest) = line.get(3..) else { continue };
        // A rename/copy is "orig -> new"; the post-change name is what ends up staged.
        let name = rest.rsplit(" -> ").next().unwrap_or(rest);
        files.push(name.to_string());
    }
    debug!("dirty_files: {} dirty path(s)", files.len());
    Ok(files)
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

/// The commit SHA a tag points to ON THE REMOTE, or `None` if the remote has no such tag
/// (annotated tags dereferenced). Same query as `remote_tag_commit`; kept as a separate
/// name because callers ask two different questions ("does this tag exist" vs. "what
/// commit is this tag really at"), but they must never diverge in behavior, so this just
/// delegates.
pub fn remote_tag_sha(path: &Path, tag: &str) -> Result<Option<String>> {
    remote_tag_commit(path, tag)
}

/// The COMMIT a tag points to ON THE REMOTE (annotated tags dereferenced), or `None` if the
/// remote has no such tag. `git ls-remote` only emits the peeled `^{}` line when the peeled
/// refspec is ALSO requested (an exact, single refspec query never gets it, so a naive
/// exact-refspec query would return the annotated TAG-OBJECT sha instead of the commit) --
/// this passes both refspecs in one call to get the real commit. `bump finish` needs it to
/// tell an at-HEAD remote tag (already released) from an at-other one (missed bump). Wired
/// to `release::finish`'s remote-tag arm in production.
pub fn remote_tag_commit(path: &Path, tag: &str) -> Result<Option<String>> {
    debug!("remote_tag_commit: path={} tag={}", path.display(), tag);
    let refspec = format!("refs/tags/{tag}");
    let peeled = format!("{refspec}^{{}}");
    let output = Command::new("git")
        .args(["ls-remote", "origin", &refspec, &peeled])
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
    let mut plain = None;
    for line in stdout.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        if name == peeled {
            // Peeled commit of an annotated tag: the commit the tag ultimately points to.
            return Ok(Some(sha.trim().to_string()));
        }
        if name == refspec {
            // A lightweight tag has no `^{}` line; its ref sha IS the commit.
            plain = Some(sha.trim().to_string());
        }
    }
    Ok(plain)
}

/// Push a single branch to origin BY NAME. Never `--tags`, never `--follow-tags`,
/// never `--force`. `bump release`'s strengthened ordering pushes the branch first and
/// only tags after confirming it landed, so a rejected branch push can never strand a tag.
///
/// Wired to `release::GitPusher::push_branch` in production.
pub fn push_branch(path: &Path, branch: &str) -> Result<()> {
    debug!("push_branch: path={} branch={}", path.display(), branch);
    let output = Command::new("git")
        .args(["push", "origin", branch])
        .current_dir(path)
        .output()
        .context("Failed to run git push")?;

    if !output.status.success() {
        bail!(
            "git push origin {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Push a single tag to origin BY EXPLICIT NAME. Never `git push --tags` / `--follow-tags`
/// (those can land a tag even when the branch push was rejected -- the okta-auth-rs
/// orphan). Never `--force`, never tag deletion. Wired to `release::GitPusher::push_tag`
/// and `release::finish`'s tag-push arms in production.
pub fn push_tag(path: &Path, tag: &str) -> Result<()> {
    debug!("push_tag: path={} tag={}", path.display(), tag);
    let output = Command::new("git")
        .args(["push", "origin", tag])
        .current_dir(path)
        .output()
        .context("Failed to run git push")?;

    if !output.status.success() {
        bail!(
            "git push origin {} failed: {}",
            tag,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Push a FEATURE branch to origin and set its upstream: `git push --no-follow-tags -u
/// origin <branch>`. Used by the GATED `bump release` flow. `--no-follow-tags` is the
/// invariant that matters here -- a stray local tag (e.g. an old `v*` on the branch tip)
/// must NEVER ride the branch push in the gated flow, where tagging is `bump finish`'s
/// job on the merged commit, never `release`'s. Never `--tags`, never `--force`.
///
/// Wired to `release::GitPusher::push_feature_branch` in production.
pub fn push_feature_branch(path: &Path, branch: &str) -> Result<()> {
    debug!("push_feature_branch: path={} branch={}", path.display(), branch);
    let output = Command::new("git")
        .args(["push", "--no-follow-tags", "-u", "origin", branch])
        .current_dir(path)
        .output()
        .context("Failed to run git push")?;

    if !output.status.success() {
        bail!(
            "git push --no-follow-tags -u origin {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Checkout an existing local branch. `bump finish` uses this to reach the default branch
/// before fast-forwarding to the merged tip. Wired to `release::finish` in production.
pub fn checkout(path: &Path, branch: &str) -> Result<()> {
    debug!("checkout: path={} branch={}", path.display(), branch);
    let output = Command::new("git")
        .args(["checkout", branch])
        .current_dir(path)
        .output()
        .context("Failed to run git checkout")?;

    if !output.status.success() {
        bail!(
            "git checkout {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Fast-forward-ONLY pull of `origin/<branch>` into the current branch: `git pull --ff-only
/// origin <branch>`. Never a merge commit, never a rebase -- if the local branch can't
/// fast-forward, it fails loudly, so `bump finish` never tags a diverged tree. Wired to
/// `release::finish` in production.
pub fn pull_ff_only(path: &Path, branch: &str) -> Result<()> {
    debug!("pull_ff_only: path={} branch={}", path.display(), branch);
    let output = Command::new("git")
        .args(["pull", "--ff-only", "origin", branch])
        .current_dir(path)
        .output()
        .context("Failed to run git pull --ff-only")?;

    if !output.status.success() {
        bail!(
            "git pull --ff-only origin {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Commit subjects on `base..HEAD`, oldest first (`git log --reverse --format=%s`). Used
/// to derive a gated PR's title (the first subject) and body (one `- <subject>` line
/// each).
// Phase 1 lands this helper on its own; `pr_title`/`pr_body` call it starting in Phase 3.
#[allow(dead_code)]
pub fn commit_subjects(path: &Path, base: &str) -> Result<Vec<String>> {
    debug!("commit_subjects: path={} base={}", path.display(), base);
    let range = format!("{base}..HEAD");
    let output = Command::new("git")
        .args(["log", "--reverse", "--format=%s", &range])
        .current_dir(path)
        .output()
        .context("Failed to run git log")?;
    if !output.status.success() {
        bail!("git log {} failed: {}", range, String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Paths changed on HEAD relative to `base` (`git diff --name-only base...HEAD`, the
/// merge-base form). Used to classify a bump-only branch and to tell a branch's own
/// version bump from an inherited pending version.
// Wired into `is_bump_only_branch` / the version-line-in-diff check starting in Phase 3/4.
#[allow(dead_code)]
pub fn changed_files(path: &Path, base: &str) -> Result<Vec<String>> {
    debug!("changed_files: path={} base={}", path.display(), base);
    let range = format!("{base}...HEAD");
    let output = Command::new("git")
        .args(["diff", "--name-only", &range])
        .current_dir(path)
        .output()
        .context("Failed to run git diff --name-only")?;
    if !output.status.success() {
        bail!(
            "git diff --name-only {} failed: {}",
            range,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// Create `branch` at `upstream` and check it out, with `upstream` as its tracking ref:
/// `git checkout -b <branch> --track <upstream>`. The standalone release path uses this so
/// the new branch has an upstream from its first second (see `is_head_pushed`).
// Wired into `execute_gated_standalone` starting in Phase 4.
#[allow(dead_code)]
pub fn checkout_new_tracking(path: &Path, branch: &str, upstream: &str) -> Result<()> {
    debug!(
        "checkout_new_tracking: path={} branch={} upstream={}",
        path.display(),
        branch,
        upstream
    );
    let output = Command::new("git")
        .args(["checkout", "-b", branch, "--track", upstream])
        .current_dir(path)
        .output()
        .context("Failed to run git checkout -b")?;
    if !output.status.success() {
        bail!(
            "git checkout -b {} --track {} failed: {}",
            branch,
            upstream,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// The worktree (of the repository containing `path`) that has `branch` checked out, or
/// `None`. `bump finish` runs from wherever the agent is, which under a worktree layout is
/// usually a feature-branch worktree while the default branch lives in the main checkout;
/// `git checkout <default>` fails there ("already checked out"), so finish goes to the
/// worktree that holds it instead.
// Wired into `finish_dir` starting in Phase 5.
#[allow(dead_code)]
pub fn worktree_for_branch(path: &Path, branch: &str) -> Result<Option<PathBuf>> {
    debug!("worktree_for_branch: path={} branch={}", path.display(), branch);
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(path)
        .output()
        .context("Failed to run git worktree list")?;
    if !output.status.success() {
        bail!("git worktree list failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let wanted = format!("refs/heads/{branch}");
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(p));
        } else if let Some(r) = line.strip_prefix("branch ") {
            if r == wanted {
                return Ok(current);
            }
        } else if line.is_empty() {
            current = None;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use tempfile::TempDir;

    /// A bare `origin` plus a working clone on `main` with one commit, HEAD == origin/main.
    fn bare_remote_and_clone() -> (TempDir, TempDir) {
        let origin = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin.path())
            .output()
            .unwrap();

        let work = TempDir::new().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(work.path())
                .output()
                .unwrap();
        }
        std::fs::write(work.path().join("README.md"), "# test").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["remote", "add", "origin", origin.path().to_str().unwrap()])
            .current_dir(work.path())
            .output()
            .unwrap();
        (origin, work)
    }

    #[test]
    fn push_branch_and_tag_reach_bare_remote() {
        let (origin, work) = bare_remote_and_clone();
        // Branch push lands the commit on origin.
        push_branch(work.path(), "main").unwrap();
        let ls = Command::new("git")
            .args(["ls-remote", "origin", "refs/heads/main"])
            .current_dir(work.path())
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&ls.stdout).trim().is_empty(),
            "branch must be on origin after push_branch"
        );
        // Tag push lands the annotated tag on origin BY NAME.
        create_tag(work.path(), "v0.1.0", "v0.1.0").unwrap();
        push_tag(work.path(), "v0.1.0").unwrap();
        // `remote_tag_sha` peels the annotated tag: it must return the underlying COMMIT,
        // not the tag object's own SHA, and that commit must equal HEAD.
        let head = head_sha(work.path()).unwrap();
        assert_eq!(
            remote_tag_sha(work.path(), "v0.1.0").unwrap(),
            Some(head),
            "remote_tag_sha must resolve the annotated tag to its target commit"
        );
        drop(origin);
    }

    #[test]
    fn push_branch_errors_on_missing_remote() {
        // A repo with no `origin` remote fails loudly rather than silently.
        let work = TempDir::new().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(work.path())
                .output()
                .unwrap();
        }
        std::fs::write(work.path().join("README.md"), "# test").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(work.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(work.path())
            .output()
            .unwrap();

        assert!(
            push_branch(work.path(), "main").is_err(),
            "push_branch must error when origin is missing"
        );
        create_tag(work.path(), "v0.1.0", "v0.1.0").unwrap();
        assert!(
            push_tag(work.path(), "v0.1.0").is_err(),
            "push_tag must error when origin is missing"
        );
    }

    /// Run a git command in `dir`, returning trimmed stdout (panics on failure).
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Phase 1 success criterion: a branch cut from `origin/main` with NO upstream is
    /// reported pushed by remote-containment alone; one local commit ahead of that tip
    /// flips it back to unpushed.
    #[test]
    fn is_head_pushed_true_by_containment_then_false_after_local_commit() {
        let (_origin, work) = bare_remote_and_clone();
        push_branch(work.path(), "main").unwrap();

        // Cut a branch at origin/main's tip with `--no-track`: no `@{u}` at all.
        git_in(work.path(), &["fetch", "origin"]);
        git_in(work.path(), &["checkout", "-b", "feature", "--no-track", "origin/main"]);
        assert!(
            is_head_pushed(work.path()).unwrap(),
            "a fresh branch cut from origin/main's tip must read as pushed via containment, even with no upstream"
        );

        // One local commit ahead of origin: no remote ref contains HEAD, and there is
        // still no upstream, so this must now read as NOT pushed.
        std::fs::write(work.path().join("feature.txt"), "work").unwrap();
        git_in(work.path(), &["add", "-A"]);
        git_in(work.path(), &["commit", "-m", "feature work"]);
        assert!(
            !is_head_pushed(work.path()).unwrap(),
            "a local commit ahead of origin, with no upstream, must read as not pushed"
        );
    }

    #[test]
    fn commit_subjects_lists_oldest_first() {
        let (_origin, work) = bare_remote_and_clone();
        let base = head_sha(work.path()).unwrap();

        std::fs::write(work.path().join("a.txt"), "a").unwrap();
        git_in(work.path(), &["add", "-A"]);
        git_in(work.path(), &["commit", "-m", "first commit"]);
        std::fs::write(work.path().join("b.txt"), "b").unwrap();
        git_in(work.path(), &["add", "-A"]);
        git_in(work.path(), &["commit", "-m", "second commit"]);

        let subjects = commit_subjects(work.path(), &base).unwrap();
        assert_eq!(subjects, vec!["first commit", "second commit"]);
    }

    #[test]
    fn commit_subjects_errors_on_unknown_base() {
        let (_origin, work) = bare_remote_and_clone();
        assert!(
            commit_subjects(work.path(), "not-a-real-ref").is_err(),
            "an unresolvable base must error, not return an empty list"
        );
    }

    #[test]
    fn changed_files_lists_diff_paths() {
        let (_origin, work) = bare_remote_and_clone();
        let base = head_sha(work.path()).unwrap();

        std::fs::write(work.path().join("a.txt"), "a").unwrap();
        std::fs::write(work.path().join("b.txt"), "b").unwrap();
        git_in(work.path(), &["add", "-A"]);
        git_in(work.path(), &["commit", "-m", "add two files"]);

        let mut files = changed_files(work.path(), &base).unwrap();
        files.sort();
        assert_eq!(files, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn changed_files_errors_on_unknown_base() {
        let (_origin, work) = bare_remote_and_clone();
        assert!(
            changed_files(work.path(), "not-a-real-ref").is_err(),
            "an unresolvable base must error, not return an empty list"
        );
    }

    #[test]
    fn checkout_new_tracking_creates_branch_with_upstream() {
        let (_origin, work) = bare_remote_and_clone();
        push_branch(work.path(), "main").unwrap();
        git_in(work.path(), &["fetch", "origin"]);

        checkout_new_tracking(work.path(), "bump-v0-1-0", "origin/main").unwrap();

        assert_eq!(current_branch(work.path()).unwrap(), "bump-v0-1-0");
        let upstream = git_in(
            work.path(),
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        );
        assert_eq!(upstream, "origin/main");
    }

    #[test]
    fn checkout_new_tracking_errors_when_branch_already_exists() {
        let (_origin, work) = bare_remote_and_clone();
        push_branch(work.path(), "main").unwrap();
        git_in(work.path(), &["fetch", "origin"]);
        git_in(work.path(), &["branch", "dup"]);

        assert!(
            checkout_new_tracking(work.path(), "dup", "origin/main").is_err(),
            "checking out an already-existing branch name must error"
        );
    }

    #[test]
    fn worktree_for_branch_finds_sibling_worktree_and_none_otherwise() {
        let (_origin, work) = bare_remote_and_clone();
        assert_eq!(
            worktree_for_branch(work.path(), "main").unwrap(),
            Some(work.path().to_path_buf()),
            "the branch checked out in the primary worktree must resolve to it"
        );
        assert_eq!(
            worktree_for_branch(work.path(), "no-such-branch").unwrap(),
            None,
            "a branch checked out nowhere must resolve to None"
        );

        let sibling = TempDir::new().unwrap();
        // Detach the sibling dir name so `git worktree add` can create it fresh.
        std::fs::remove_dir(sibling.path()).unwrap();
        git_in(
            work.path(),
            &["worktree", "add", "-b", "feature", sibling.path().to_str().unwrap()],
        );
        assert_eq!(
            worktree_for_branch(work.path(), "feature").unwrap(),
            Some(sibling.path().to_path_buf()),
            "a branch checked out in a sibling worktree must resolve to that worktree's path"
        );
    }

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
