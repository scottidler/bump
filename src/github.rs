use eyre::{Context, Result};
use log::{debug, warn};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Maximum number of retry attempts for network operations
const MAX_RETRIES: u32 = 3;
/// Base delay between retries in milliseconds
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// Ruleset rule types that do NOT block a normal push, so they never make a
/// branch "gated" for tagging purposes.
const HARMLESS_RULE_TYPES: [&str; 2] = ["deletion", "non_fast_forward"];

/// Gate classification for a repo's default branch.
///
/// `detect` is infallible: a probe that cannot reach a verdict collapses to
/// `Unknown(reason)` so the caller can warn-and-proceed (tag creation is local
/// and recoverable; the dangerous step is the push, which `bump` never does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Both protection layers clear: a direct push to the default branch is allowed.
    Ungated,
    /// Gated: lists the blocking rule types (e.g. `["classic_protection", "pull_request"]`).
    Gated(Vec<String>),
    /// Probe failed: carries the reason (no remote, non-GitHub, gh error, offline).
    Unknown(String),
}

/// Outcome of the classic branch-protection probe.
enum ClassicResult {
    /// Protection object exists (HTTP 200) -> contributes a blocking rule.
    Gated,
    /// No protection (HTTP 404).
    Clear,
    /// Probe failed for some other reason.
    Error(String),
}

/// Detect whether `path`'s remote default branch is gated.
///
/// Honors the `BUMP_GATES_PROBE` env override (test/scripted seam) before any
/// network access. Otherwise: resolve the GitHub slug and default branch, then
/// probe the classic branch-protection layer and the rulesets layer.
pub fn detect(path: &Path) -> Gate {
    debug!("detect: path={}", path.display());

    if let Ok(probe) = env::var("BUMP_GATES_PROBE") {
        debug!("detect: BUMP_GATES_PROBE override active: {probe}");
        return parse_probe_override(&probe);
    }

    let slug = match remote_slug(path) {
        Some(slug) => slug,
        None => {
            debug!("detect: no GitHub remote for {}", path.display());
            return Gate::Unknown("not a GitHub remote".to_string());
        }
    };

    let branch = match default_branch(path, &slug) {
        Ok(branch) => branch,
        Err(e) => {
            warn!("detect: could not resolve default branch for {slug}: {e}");
            return Gate::Unknown(format!("could not resolve default branch: {e}"));
        }
    };

    let org = org_of(&slug);
    let mut blocking: Vec<String> = Vec::new();

    match probe_classic(org, &slug, &branch) {
        ClassicResult::Gated => blocking.push("classic_protection".to_string()),
        ClassicResult::Clear => {}
        ClassicResult::Error(e) => {
            warn!("detect: classic-protection probe failed for {slug}: {e}");
            return Gate::Unknown(format!("classic-protection probe failed: {e}"));
        }
    }

    match probe_rulesets(org, &slug, &branch) {
        Ok(mut types) => blocking.append(&mut types),
        Err(e) => {
            warn!("detect: ruleset probe failed for {slug}: {e}");
            return Gate::Unknown(format!("ruleset probe failed: {e}"));
        }
    }

    if blocking.is_empty() {
        debug!("detect: {slug} ({branch}) is ungated");
        Gate::Ungated
    } else {
        debug!("detect: {slug} ({branch}) is gated by {blocking:?}");
        Gate::Gated(blocking)
    }
}

/// Parse the `BUMP_GATES_PROBE` env override into a `Gate`.
///
/// Forms: `ungated` | `gated` | `gated:type1,type2` | `unknown:reason`.
fn parse_probe_override(probe: &str) -> Gate {
    let probe = probe.trim();
    if probe == "ungated" {
        Gate::Ungated
    } else if probe == "gated" {
        Gate::Gated(vec!["pull_request".to_string()])
    } else if let Some(types) = probe.strip_prefix("gated:") {
        let types: Vec<String> = types
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Gate::Gated(types)
    } else if let Some(reason) = probe.strip_prefix("unknown:") {
        Gate::Unknown(reason.trim().to_string())
    } else {
        Gate::Unknown(format!("invalid BUMP_GATES_PROBE: {probe}"))
    }
}

/// The org/owner portion of a repo slug (`org/repo` -> `org`).
fn org_of(slug: &str) -> &str {
    slug.split('/').next().unwrap_or(slug)
}

/// Best-effort "'branch' on owner/repo" label for user-facing messages. Uses
/// only local git (no network), falling back to generic wording when a piece is
/// unavailable, so it is safe to call on the rare refusal/warning path.
pub fn repo_label(path: &Path) -> String {
    let slug = remote_slug(path).unwrap_or_else(|| "this repo".to_string());
    let branch = local_default_branch(path).unwrap_or_else(|| "the default branch".to_string());
    format!("'{branch}' on {slug}")
}

/// Read the remote default branch from the local `refs/remotes/origin/HEAD`
/// symref only (no API fallback). `None` if the symref is absent.
pub fn local_default_branch(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("refs/remotes/origin/")
        .map(str::to_string)
}

/// Resolve the GitHub `owner/repo` slug from the `origin` remote, or `None` if
/// there is no `origin` or it is not a github.com remote.
pub fn remote_slug(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout);
    parse_slug(url.trim())
}

/// Parse a git remote URL into a `owner/repo` slug. Recognizes SSH (SCP-like and
/// `ssh://`) and HTTPS forms; only github.com hosts are accepted.
fn parse_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let url = url.strip_suffix(".git").unwrap_or(url);

    // SCP-like SSH: git@github.com:owner/repo
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        if host != "github.com" {
            return None;
        }
        return normalize_slug(path);
    }

    // URL forms: https://github.com/owner/repo, ssh://git@github.com/owner/repo
    for prefix in ["https://", "http://", "ssh://"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            // Drop any userinfo (git@) ahead of the host.
            let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
            let (host, path) = rest.split_once('/')?;
            if host != "github.com" {
                return None;
            }
            return normalize_slug(path);
        }
    }

    None
}

/// Reduce a remote URL's path component to a clean `owner/repo`.
fn normalize_slug(path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Resolve the remote default branch name. Prefers the local
/// `refs/remotes/origin/HEAD` symref; falls back to the GitHub API.
fn default_branch(path: &Path, slug: &str) -> Result<String> {
    if let Some(branch) = local_default_branch(path) {
        debug!("default_branch: {slug} -> {branch} (symref)");
        return Ok(branch);
    }

    // Fallback: ask GitHub directly.
    let output = run_gh(
        org_of(slug),
        &["api", &format!("repos/{slug}"), "--jq", ".default_branch"],
    )
    .with_context(|| format!("gh api repos/{slug} failed"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("{}", err.trim());
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        eyre::bail!("empty default branch from API");
    }
    debug!("default_branch: {slug} -> {branch} (api)");
    Ok(branch)
}

/// Probe the classic branch-protection layer.
fn probe_classic(org: &str, slug: &str, branch: &str) -> ClassicResult {
    debug!("probe_classic: slug={slug} branch={branch}");
    let endpoint = format!("repos/{slug}/branches/{branch}/protection");
    match run_gh(org, &["api", &endpoint, "--silent"]) {
        Ok(output) if output.status.success() => ClassicResult::Gated,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("HTTP 404") || stderr.contains("Not Found") {
                ClassicResult::Clear
            } else {
                ClassicResult::Error(stderr.trim().to_string())
            }
        }
        Err(e) => ClassicResult::Error(e.to_string()),
    }
}

/// Probe the rulesets layer, returning the blocking rule types (with the
/// harmless `deletion`/`non_fast_forward` types filtered out, deduplicated).
fn probe_rulesets(org: &str, slug: &str, branch: &str) -> Result<Vec<String>, String> {
    debug!("probe_rulesets: slug={slug} branch={branch}");
    let endpoint = format!("repos/{slug}/rules/branches/{branch}");
    match run_gh(org, &["api", &endpoint, "--jq", ".[].type"]) {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            Ok(filter_rule_types(&raw))
        }
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Filter a newline-separated list of rule types: drop the harmless ones and
/// deduplicate while preserving first-seen order.
fn filter_rule_types(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() || HARMLESS_RULE_TYPES.contains(&t) {
            continue;
        }
        if !out.iter().any(|existing| existing == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Path to the per-org GitHub token file: `$XDG_CONFIG_HOME/github/tokens/{org}`
/// (falling back to `$HOME/.config/...`). This mirrors `gx`'s default template.
fn token_path(org: &str) -> Option<PathBuf> {
    let base = match env::var("XDG_CONFIG_HOME") {
        Ok(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("github").join("tokens").join(org))
}

/// Build a `gh` command with per-org auth: set `GH_TOKEN` from the org's token
/// file when one exists, else fall back to ambient `gh auth` (recorded at debug).
fn gh_command(org: &str) -> Command {
    let mut cmd = Command::new("gh");
    match token_path(org).and_then(|p| fs::read_to_string(p).ok()) {
        Some(token) if !token.trim().is_empty() => {
            cmd.env("GH_TOKEN", token.trim());
        }
        _ => {
            debug!("gh_command: no token file for {org}; using ambient gh auth");
        }
    }
    cmd
}

/// Execute a `gh` command (token-authed for `org`) with retry + exponential
/// backoff on retryable network errors. A non-retryable failure (e.g. HTTP 404)
/// is returned as a non-success `Output`, not an `Err`.
fn run_gh(org: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut last_error = None;

    for attempt in 0..MAX_RETRIES {
        let output = gh_command(org).args(args).output().context("Failed to execute gh")?;

        if output.status.success() {
            return Ok(output);
        }

        let error = String::from_utf8_lossy(&output.stderr);
        if is_retryable_error(&error) && attempt < MAX_RETRIES - 1 {
            let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt);
            warn!(
                "gh attempt {} failed, retrying in {}ms: {}",
                attempt + 1,
                delay,
                error.trim()
            );
            thread::sleep(Duration::from_millis(delay));
            last_error = Some(error.to_string());
        } else {
            return Ok(output);
        }
    }

    Err(eyre::eyre!(
        "gh failed after {} attempts: {}",
        MAX_RETRIES,
        last_error.unwrap_or_default()
    ))
}

/// Check if an error message indicates a retryable condition.
fn is_retryable_error(error: &str) -> bool {
    let retryable_patterns = [
        "timeout",
        "timed out",
        "connection refused",
        "connection reset",
        "network",
        "rate limit",
        "too many requests",
        "503",
        "502",
        "504",
        "ETIMEDOUT",
        "ECONNRESET",
        "ENOTFOUND",
    ];

    let error_lower = error.to_lowercase();
    retryable_patterns.iter().any(|pattern| error_lower.contains(pattern))
}

/// The `gh` argv for the OPEN-PR existence probe on `branch`.
///
/// Phase 0 finding (supersedes the API Design table's `gh pr view`): `gh pr view` returns
/// exit 0 for a MERGED/closed PR, so it CANNOT distinguish an open PR from a stale merged
/// one on a reused branch name. `gh pr list --head <branch> --state open --json number`
/// exits 0 in every case and returns a JSON array whose emptiness IS the verdict.
///
/// Gated `#[cfg(test)]` this phase: only the (also `#[cfg(test)]`) `release::GhPr` calls
/// the PR seam until Phase 8 wires the subcommand.
#[cfg(test)]
fn pr_list_args(branch: &str) -> Vec<String> {
    ["pr", "list", "--head", branch, "--state", "open", "--json", "number"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Interpret the `gh pr list --json number` stdout: a NON-empty JSON array means an open
/// PR exists (skip create); an empty array means none (create). Empty stdout is treated as
/// "no PR". Any non-array / non-JSON payload is a loud error, never a silent false.
#[cfg(test)]
fn open_pr_exists_from_json(stdout: &str) -> Result<bool> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).with_context(|| format!("gh pr list returned non-JSON: {trimmed}"))?;
    match value.as_array() {
        Some(arr) => Ok(!arr.is_empty()),
        None => eyre::bail!("gh pr list JSON was not an array: {trimmed}"),
    }
}

/// Does an OPEN pull request exist for `branch`? Runs the `pr_list_args` probe in the
/// repo at `path` (gh infers the repo from its remote), per-org token-authed. Gated
/// `#[cfg(test)]` this phase for the same reason as the git push helpers.
#[cfg(test)]
pub fn open_pr_exists(path: &Path, branch: &str) -> Result<bool> {
    debug!("open_pr_exists: path={} branch={}", path.display(), branch);
    let org = remote_slug(path).map(|s| org_of(&s).to_string()).unwrap_or_default();
    let args = pr_list_args(branch);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = gh_command(&org)
        .args(&arg_refs)
        .current_dir(path)
        .output()
        .context("Failed to run gh pr list")?;
    if !output.status.success() {
        eyre::bail!("gh pr list failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let exists = open_pr_exists_from_json(&String::from_utf8_lossy(&output.stdout))?;
    debug!("open_pr_exists: branch={branch} exists={exists}");
    Ok(exists)
}

/// Open a PR for the current branch with `gh pr create --fill`. Only ever called behind
/// `open_pr_exists` returning false -- `gh pr create --fill` ERRORS on an existing open PR
/// (known gh behavior, Phase 0 addendum), so this is a race backstop, not the primary
/// guard. Gated `#[cfg(test)]` this phase.
#[cfg(test)]
pub fn create_pr(path: &Path, branch: &str) -> Result<()> {
    debug!("create_pr: path={} branch={}", path.display(), branch);
    let org = remote_slug(path).map(|s| org_of(&s).to_string()).unwrap_or_default();
    let output = gh_command(&org)
        .args(["pr", "create", "--fill"])
        .current_dir(path)
        .output()
        .context("Failed to run gh pr create")?;
    if !output.status.success() {
        eyre::bail!(
            "gh pr create --fill failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_list_args_is_the_open_pr_probe() {
        // The load-bearing Phase 0 decision: list (not view), scoped to --state open.
        assert_eq!(
            pr_list_args("my-feature"),
            vec![
                "pr",
                "list",
                "--head",
                "my-feature",
                "--state",
                "open",
                "--json",
                "number"
            ]
        );
    }

    #[test]
    fn open_pr_from_json_empty_array_is_false() {
        assert!(!open_pr_exists_from_json("[]").unwrap());
        assert!(!open_pr_exists_from_json("  []  \n").unwrap());
        // Empty stdout (no output) is treated as "no open PR", not an error.
        assert!(!open_pr_exists_from_json("").unwrap());
    }

    #[test]
    fn open_pr_from_json_nonempty_array_is_true() {
        assert!(open_pr_exists_from_json("[{\"number\":7}]").unwrap());
        assert!(open_pr_exists_from_json("[{\"number\":7},{\"number\":8}]").unwrap());
    }

    #[test]
    fn open_pr_from_json_non_array_is_loud_error() {
        // A non-array payload must fail loudly, never be read as a silent false.
        assert!(open_pr_exists_from_json("{\"number\":7}").is_err());
        assert!(open_pr_exists_from_json("not json").is_err());
    }

    #[test]
    fn parse_slug_scp_ssh() {
        assert_eq!(
            parse_slug("git@github.com:scottidler/bump.git").as_deref(),
            Some("scottidler/bump")
        );
        assert_eq!(
            parse_slug("git@github.com:scottidler/bump").as_deref(),
            Some("scottidler/bump")
        );
    }

    #[test]
    fn parse_slug_https() {
        assert_eq!(
            parse_slug("https://github.com/tatari-tv/philo.git").as_deref(),
            Some("tatari-tv/philo")
        );
        assert_eq!(
            parse_slug("https://github.com/tatari-tv/philo").as_deref(),
            Some("tatari-tv/philo")
        );
    }

    #[test]
    fn parse_slug_ssh_url_with_userinfo() {
        assert_eq!(
            parse_slug("ssh://git@github.com/scottidler/bump.git").as_deref(),
            Some("scottidler/bump")
        );
    }

    #[test]
    fn parse_slug_non_github_host_is_none() {
        assert_eq!(parse_slug("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_slug("https://bitbucket.org/owner/repo.git"), None);
    }

    #[test]
    fn parse_slug_garbage_is_none() {
        assert_eq!(parse_slug("not a url"), None);
        assert_eq!(parse_slug("https://github.com/onlyowner"), None);
        assert_eq!(parse_slug(""), None);
    }

    #[test]
    fn filter_rule_types_drops_harmless() {
        let raw = "deletion\nnon_fast_forward\n";
        assert!(filter_rule_types(raw).is_empty());
    }

    #[test]
    fn filter_rule_types_keeps_blocking() {
        let raw = "pull_request\nworkflows\nnon_fast_forward\nrequired_status_checks\n";
        assert_eq!(
            filter_rule_types(raw),
            vec!["pull_request", "workflows", "required_status_checks"]
        );
    }

    #[test]
    fn filter_rule_types_dedupes_preserving_order() {
        let raw = "pull_request\nworkflows\npull_request\n";
        assert_eq!(filter_rule_types(raw), vec!["pull_request", "workflows"]);
    }

    #[test]
    fn filter_rule_types_empty_input() {
        assert!(filter_rule_types("").is_empty());
        assert!(filter_rule_types("\n\n").is_empty());
    }

    #[test]
    fn org_of_splits_slug() {
        assert_eq!(org_of("scottidler/bump"), "scottidler");
        assert_eq!(org_of("noslash"), "noslash");
    }

    #[test]
    fn probe_override_ungated() {
        assert_eq!(parse_probe_override("ungated"), Gate::Ungated);
    }

    #[test]
    fn probe_override_gated_bare() {
        assert_eq!(
            parse_probe_override("gated"),
            Gate::Gated(vec!["pull_request".to_string()])
        );
    }

    #[test]
    fn probe_override_gated_with_types() {
        assert_eq!(
            parse_probe_override("gated:pull_request,workflows"),
            Gate::Gated(vec!["pull_request".to_string(), "workflows".to_string()])
        );
    }

    #[test]
    fn probe_override_unknown() {
        assert_eq!(
            parse_probe_override("unknown:offline"),
            Gate::Unknown("offline".to_string())
        );
    }

    #[test]
    fn probe_override_invalid() {
        match parse_probe_override("bogus") {
            Gate::Unknown(reason) => assert!(reason.contains("invalid")),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn is_retryable_error_matches_network() {
        assert!(is_retryable_error("connection reset by peer"));
        assert!(is_retryable_error("HTTP 503 Service Unavailable"));
        assert!(!is_retryable_error("Not Found (HTTP 404)"));
    }
}
