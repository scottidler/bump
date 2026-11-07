//! The gated flow's PR seam: the `Pr` port, its production `GhPr`, the branch-slug test,
//! and the release-intent lines handed to `/pr:create`. bump never opens a PR: a PR opened
//! inside this binary is invisible to the PreToolUse guards that hold `/pr:create`'s body
//! contract, which is how tatari-tv/drata-cli#18 shipped a non-contract body.

use crate::github;
use eyre::Result;
use std::path::Path;

/// The PR seam for the gated flow. A port (preferred over the doc's optional
/// `BUMP_PR_PROBE` env seam for consistency with `Pusher`/`Installer`) so tests inject a
/// fake `gh` without a real GitHub round-trip.
///
/// `open_pr_exists` is the Phase-0 open-PR probe (`gh pr list --head <branch> --state
/// open --json number`, NOT `gh pr view`). `comment_pr` (`gh pr comment <branch> --body
/// <body>`) records Scott's standalone order on a PR that was already open, whose body this
/// run never wrote. There is deliberately no create: `/pr:create` opens every PR.
pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    fn comment_pr(&self, dir: &Path, branch: &str, body: &str) -> Result<()>;
}

/// Production `Pr`: the real `gh` open-PR probe and comment.
pub struct GhPr;

impl Pr for GhPr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool> {
        github::open_pr_exists(dir, branch)
    }

    fn comment_pr(&self, dir: &Path, branch: &str, body: &str) -> Result<()> {
        github::comment_pr(dir, branch, body)
    }
}

/// `title_slug` from `branch-pr-title-guard.sh`: lowercase, collapse every run of
/// non-`[a-z0-9]` to `-`, trim dashes. A branch is a legal release branch only when it is
/// its own slug.
pub(super) fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut in_run = false;
    for c in branch.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            in_run = false;
        } else if !in_run {
            slug.push('-');
            in_run = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// The release-intent line Gate D and `/pr:create`'s contract look for in the PR body.
pub(super) fn release_intent_line(tag: &str) -> String {
    format!("Release: rides this PR ({tag})")
}

/// The lines the PR body must carry, in order, for the PR `/pr:create` opens: Scott's
/// standalone order quoted verbatim when there is one, then the release-intent line.
pub(super) fn pr_body_lines(tag: &str, standalone: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(words) = standalone {
        lines.push(standalone_order_line(words));
    }
    lines.push(release_intent_line(tag));
    lines
}

/// The hand-off printed when the branch has no open PR: bump never opens one, so the next
/// step is `/pr:create`, with the exact lines its body must carry, one per line to copy.
pub(super) fn pr_create_handoff(branch: &str, lines: &[String]) -> String {
    let which = if lines.len() == 1 { "this line" } else { "these lines" };
    let mut out = format!(
        "no open PR for {branch}; bump does not open PRs.\n\
         next: open the PR with /pr:create; its body must carry {which} verbatim:"
    );
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// Scott's standalone order as it is quoted on the PR: a line the `/pr:create` body must
/// carry, or a comment on a PR that was already open.
pub(super) fn standalone_order_line(words: &str) -> String {
    format!("Standalone release ordered by Scott: \"{words}\"")
}
