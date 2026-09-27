//! PR construction for the gated flow: the `Pr` port, its production `GhPr`, and the pure
//! title / body / branch-slug functions that make the PR pass the title guard and Gate D by
//! construction.

use crate::github;
use eyre::Result;
use std::path::Path;

/// The PR seam for the gated flow. A port (preferred over the doc's optional
/// `BUMP_PR_PROBE` env seam for consistency with `Pusher`/`Installer`) so tests inject a
/// fake `gh` without a real GitHub round-trip.
///
/// `open_pr_exists` is the Phase-0 open-PR probe (`gh pr list --head <branch> --state
/// open --json number`, NOT `gh pr view`); `create_pr` is `gh pr create` with an explicit
/// head, base, title and body, only ever called when `open_pr_exists` returns false, and
/// returns the new PR's URL. `comment_pr` (`gh pr comment <branch> --body <body>`) records
/// Scott's standalone order on a PR that was already open, whose body this run never built.
pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String>;
    fn comment_pr(&self, dir: &Path, branch: &str, body: &str) -> Result<()>;
}

/// Production `Pr`: the real `gh` PR operations (list-probe + explicit create).
pub struct GhPr;

impl Pr for GhPr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool> {
        github::open_pr_exists(dir, branch)
    }

    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        github::create_pr(dir, branch, base, title, body)
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

/// The PR title: `<type>(<scope>): <branch words>`, type/scope from the first commit
/// subject on the branch (`chore` when it has no conventional prefix). By construction it
/// slugifies back to the branch, which `branch-pr-title-guard.sh` enforces.
pub fn pr_title(branch: &str, subjects: &[String]) -> String {
    let words = branch.replace('-', " ");
    let (kind, scope) = subjects
        .first()
        .and_then(|s| parse_conventional_prefix(s))
        .unwrap_or_else(|| ("chore".to_string(), None));
    match scope {
        Some(scope) => format!("{kind}({scope}): {words}"),
        None => format!("{kind}: {words}"),
    }
}

/// `feat(scope)!: subject` -> `("feat", Some("scope"))`. `None` when the subject has no
/// conventional prefix.
fn parse_conventional_prefix(subject: &str) -> Option<(String, Option<String>)> {
    let (head, _) = subject.split_once(':')?;
    let head = head.trim_end_matches('!');
    let (kind, scope) = match head.split_once('(') {
        Some((k, rest)) => (k, Some(rest.strip_suffix(')')?)),
        None => (head, None),
    };
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    if let Some(s) = scope
        && (s.is_empty()
            || !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
    {
        return None;
    }
    Some((kind.to_string(), scope.map(str::to_string)))
}

/// The PR body: one `- <subject>` per commit on the branch, Scott's standalone order quoted
/// verbatim when there is one, then the release-intent line Gate D looks for as the LAST
/// line.
pub fn pr_body(subjects: &[String], tag: &str, standalone: Option<&str>) -> String {
    let mut body = String::new();
    for s in subjects {
        body.push_str(&format!("- {s}\n"));
    }
    if subjects.is_empty() {
        body.push_str("- version bump\n");
    }
    if let Some(words) = standalone {
        body.push_str(&format!("\n{}\n", standalone_order_line(words)));
    }
    body.push_str(&format!("\nRelease: rides this PR ({tag})"));
    body
}

/// Scott's standalone order as it is quoted on the PR: in the body of a PR this run opens,
/// or as a comment on one that was already open.
pub(super) fn standalone_order_line(words: &str) -> String {
    format!("Standalone release ordered by Scott: \"{words}\"")
}
