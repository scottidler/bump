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

/// The env var carrying the token for `org`, in the persona vocabulary the dotfiles
/// `gh()` shell function uses (`GITHUB_PAT_WORK` for the work org, `GITHUB_PAT_HOME`
/// for everything else). bump runs `gh` as a subprocess, where that shell function does
/// not exist, so the same selection has to happen here or a work-org call goes out as
/// the home account and 404s like "no access".
fn persona_token_var(org: &str) -> &'static str {
    match org {
        "tatari-tv" => "GITHUB_PAT_WORK",
        _ => "GITHUB_PAT_HOME",
    }
}

/// Resolve the token for `org`: the org's token file, then `GITHUB_PAT_<ORG>` (`-` as
/// `_`, uppercased), then the persona env var (`persona_token_var`), else `None`
/// (ambient `gh auth` applies). Tokens are only ever read into the subprocess env,
/// never printed or logged -- debug logs name the variable/source, not the value.
fn token_for_org(org: &str) -> Option<String> {
    if let Some(token) = token_path(org).and_then(|p| fs::read_to_string(p).ok()) {
        let token = token.trim();
        if !token.is_empty() {
            debug!("token_for_org: {org} -> token file");
            return Some(token.to_string());
        }
    }
    let org_var = format!("GITHUB_PAT_{}", org.to_uppercase().replace('-', "_"));
    for var in [org_var.as_str(), persona_token_var(org)] {
        if let Ok(token) = env::var(var)
            && !token.trim().is_empty()
        {
            debug!("token_for_org: {org} -> ${var}");
            return Some(token.trim().to_string());
        }
    }
    debug!("token_for_org: {org} -> no token found; using ambient gh auth");
    None
}

/// Build a `gh` command with per-org auth: `GH_TOKEN` from `token_for_org`, else
/// ambient `gh auth` (recorded at debug).
fn gh_command(org: &str) -> Command {
    let mut cmd = Command::new("gh");
    match token_for_org(org) {
        Some(token) => {
            cmd.env("GH_TOKEN", token);
        }
        None => {
            debug!("gh_command: no token for {org}; using ambient gh auth");
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
/// Wired to `release::GhPr::open_pr_exists` in production.
fn pr_list_args(branch: &str) -> Vec<String> {
    ["pr", "list", "--head", branch, "--state", "open", "--json", "number"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Interpret the `gh pr list --json number` stdout: a NON-empty JSON array means an open
/// PR exists (skip create); an empty array means none (create). Empty stdout is treated as
/// "no PR". Any non-array / non-JSON payload is a loud error, never a silent false.
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
/// repo at `path` (gh infers the repo from its remote), per-org token-authed. Wired to
/// `release::GhPr::open_pr_exists` in production.
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

/// The `gh` argv for opening the release PR: head, base, title and body all explicit,
/// never `--fill` (the title and body are built by `release::pr_title` / `pr_body` so the
/// title-slug and release-intent rules hold by construction).
fn pr_create_args(branch: &str, base: &str, title: &str, body: &str) -> Vec<String> {
    [
        "pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Open a PR from `branch` into `base` with the given title and body; returns the PR URL
/// `gh` prints. Only ever called behind `open_pr_exists` returning false -- `gh pr
/// create` ERRORS on an existing open PR (known gh behavior, Phase 0 addendum), so this
/// is a race backstop, not the primary guard. Wired to `release::GhPr::create_pr` in
/// production.
pub fn create_pr(path: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
    debug!(
        "create_pr: path={} branch={} base={} title={}",
        path.display(),
        branch,
        base,
        title
    );
    let org = remote_slug(path).map(|s| org_of(&s).to_string()).unwrap_or_default();
    let args = pr_create_args(branch, base, title, body);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = gh_command(&org)
        .args(&arg_refs)
        .current_dir(path)
        .output()
        .context("Failed to run gh pr create")?;
    if !output.status.success() {
        eyre::bail!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    debug!("create_pr: url={url}");
    Ok(url)
}

/// The `gh` argv for commenting on the open PR for `branch` (gh resolves a branch name to
/// its PR).
fn pr_comment_args(branch: &str, body: &str) -> Vec<String> {
    ["pr", "comment", branch, "--body", body]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Comment `body` on the open PR for `branch`, per-org token-authed. Wired to
/// `release::GhPr::comment_pr` in production.
pub fn comment_pr(path: &Path, branch: &str, body: &str) -> Result<()> {
    debug!("comment_pr: path={} branch={}", path.display(), branch);
    let org = remote_slug(path).map(|s| org_of(&s).to_string()).unwrap_or_default();
    let args = pr_comment_args(branch, body);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = gh_command(&org)
        .args(&arg_refs)
        .current_dir(path)
        .output()
        .context("Failed to run gh pr comment")?;
    if !output.status.success() {
        eyre::bail!(
            "gh pr comment failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The legacy commit-status API's combined verdict for a commit (`GET
/// .../commits/{sha}/status`), read off `total_count` first -- see `status_from_json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusState {
    Success,
    Pending,
    Failure,
    /// Zero legacy statuses registered on this commit at all (the common case: most
    /// repos never used this API). Distinct from `Pending` on purpose -- see
    /// `status_from_json`.
    #[default]
    None,
}

/// Summary of what GitHub currently reports for one commit, across BOTH the check-runs
/// API and the legacy commit-status API. `release::wait_for_green` is the only consumer
/// that decides pass/fail from this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckRuns {
    /// Check runs GitHub actually returned (never the API's own `total_count` --
    /// see `check_runs_from_json`'s truncation check).
    pub total: usize,
    pub incomplete: usize,
    /// `(name, html_url)` of every run whose conclusion is not success/skipped/neutral.
    pub failed: Vec<(String, String)>,
    /// The legacy commit-status API's combined verdict for the same sha.
    pub statuses: StatusState,
}

/// Parse the `check-runs` API payload into a `CheckRuns` summary (the `statuses` field
/// is left at its default; `check_runs` fills it in from the separate legacy-status
/// read). A missing `check_runs` array is a loud error, never a silent empty summary.
/// Truncation -- the API's own `total_count` claiming more runs than were actually
/// returned (i.e. `per_page=100` wasn't enough) -- is ALSO a loud error: a truncated
/// read cannot tell red from green, so it must never be read as "all green so far". A
/// missing or non-integer `total_count` fails closed the same way: without it the
/// truncation check cannot run.
pub fn check_runs_from_json(text: &str) -> Result<CheckRuns> {
    let value: serde_json::Value = serde_json::from_str(text).context("check-runs payload is not JSON")?;
    let runs = value
        .get("check_runs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("check-runs payload has no check_runs array"))?;

    let total_count = value
        .get("total_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| eyre::eyre!("check-runs payload has no integer total_count; cannot rule out truncation"))?
        as usize;
    if total_count > runs.len() {
        eyre::bail!(
            "check-runs payload truncated: total_count={} but only {} returned",
            total_count,
            runs.len()
        );
    }

    let mut summary = CheckRuns {
        total: runs.len(),
        ..CheckRuns::default()
    };
    for run in runs {
        let status = run.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "completed" {
            summary.incomplete += 1;
            continue;
        }
        let conclusion = run.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(conclusion, "success" | "skipped" | "neutral") {
            let name = run.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let url = run.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            summary.failed.push((name, url));
        }
    }
    Ok(summary)
}

/// Parse the legacy commit-status API payload (`GET .../commits/{sha}/status`) into a
/// `StatusState`. Read off `total_count`, NEVER off `state` alone: a commit with ZERO
/// statuses registered reports `state: "pending"` (observed 2026-09-26 on
/// `scottidler/bump`), so keying on `state` would poll every such repo to the
/// `--ci-timeout` ceiling waiting for a status that will never arrive. `total_count ==
/// 0` -> `None` regardless of `state`; otherwise `state` decides, and any value other
/// than `success`/`pending` (e.g. `failure`, `error`) is `Failure` -- fail closed on an
/// unrecognized state rather than silently proceeding.
pub fn status_from_json(text: &str) -> Result<StatusState> {
    let value: serde_json::Value = serde_json::from_str(text).context("status payload is not JSON")?;
    let total_count = value
        .get("total_count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| eyre::eyre!("status payload has no total_count"))?;
    if total_count == 0 {
        return Ok(StatusState::None);
    }
    let state = value
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| eyre::eyre!("status payload has no state"))?;
    Ok(match state {
        "success" => StatusState::Success,
        "pending" => StatusState::Pending,
        _ => StatusState::Failure,
    })
}

/// The check runs AND legacy statuses GitHub currently reports for `sha` in the repo at
/// `path`, merged into one `CheckRuns`. `Ok(None)` when the repo has no GitHub remote
/// (nothing to wait on). Any non-success `gh api` result on EITHER read, or truncation
/// on the check-runs read, is `Err` -- the CI gate fails closed on both.
/// Wired to `release::GhCi` in production.
pub fn check_runs(path: &Path, sha: &str) -> Result<Option<CheckRuns>> {
    debug!("check_runs: path={} sha={}", path.display(), sha);
    let Some(slug) = remote_slug(path) else {
        return Ok(None);
    };
    let org = org_of(&slug);

    let runs_endpoint = format!("repos/{slug}/commits/{sha}/check-runs?per_page=100");
    let runs_output = run_gh(org, &["api", &runs_endpoint]).context("gh api check-runs failed")?;
    if !runs_output.status.success() {
        eyre::bail!(
            "gh api {} failed: {}",
            runs_endpoint,
            String::from_utf8_lossy(&runs_output.stderr).trim()
        );
    }
    let mut summary = check_runs_from_json(&String::from_utf8_lossy(&runs_output.stdout))?;

    let status_endpoint = format!("repos/{slug}/commits/{sha}/status");
    let status_output = run_gh(org, &["api", &status_endpoint]).context("gh api status failed")?;
    if !status_output.status.success() {
        eyre::bail!(
            "gh api {} failed: {}",
            status_endpoint,
            String::from_utf8_lossy(&status_output.stderr).trim()
        );
    }
    summary.statuses = status_from_json(&String::from_utf8_lossy(&status_output.stdout))?;

    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_create_args_are_explicit_never_fill() {
        let args = pr_create_args("add-thing", "main", "feat(core): add thing", "- x\n\nRelease: rides");
        assert_eq!(
            args,
            vec![
                "pr",
                "create",
                "--head",
                "add-thing",
                "--base",
                "main",
                "--title",
                "feat(core): add thing",
                "--body",
                "- x\n\nRelease: rides"
            ]
        );
        assert!(!args.iter().any(|a| a == "--fill"));
    }

    #[test]
    fn pr_comment_args_name_the_branch_and_body() {
        assert_eq!(
            pr_comment_args("add-thing", "ordered"),
            vec!["pr", "comment", "add-thing", "--body", "ordered"]
        );
    }

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

    #[test]
    fn check_runs_from_json_counts_incomplete_and_failed() {
        let payload = r#"{"total_count":4,"check_runs":[
            {"name":"ci","status":"completed","conclusion":"success","html_url":"u1"},
            {"name":"lint","status":"in_progress","conclusion":null,"html_url":"u2"},
            {"name":"test","status":"completed","conclusion":"failure","html_url":"u3"},
            {"name":"opt","status":"completed","conclusion":"skipped","html_url":"u4"}
        ]}"#;
        let summary = check_runs_from_json(payload).unwrap();
        assert_eq!(summary.total, 4);
        assert_eq!(summary.incomplete, 1);
        assert_eq!(summary.failed, vec![("test".to_string(), "u3".to_string())]);
        assert_eq!(
            summary.statuses,
            StatusState::None,
            "unset until check_runs merges it in"
        );
    }

    #[test]
    fn check_runs_from_json_empty_array_is_zero() {
        let summary = check_runs_from_json(r#"{"total_count":0,"check_runs":[]}"#).unwrap();
        assert_eq!(summary, CheckRuns::default());
    }

    #[test]
    fn check_runs_from_json_missing_check_runs_key_is_a_loud_error() {
        assert!(
            check_runs_from_json("[]").is_err(),
            "no check_runs key must be loud, never a silent empty summary"
        );
        assert!(check_runs_from_json(r#"{"total_count":0}"#).is_err());
    }

    #[test]
    fn check_runs_from_json_truncation_is_a_loud_error() {
        let runs: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"name":"r{i}","status":"completed","conclusion":"success","html_url":""}}"#))
            .collect();
        let payload = format!(r#"{{"total_count":101,"check_runs":[{}]}}"#, runs.join(","));
        let err = check_runs_from_json(&payload).unwrap_err().to_string();
        assert!(err.contains("truncated"), "error must name the truncation: {err}");
    }

    /// Audit round 1, cheap-win 4: without an integer `total_count` the truncation guard
    /// cannot run, so the read fails closed instead of skipping it.
    #[test]
    fn check_runs_from_json_missing_or_non_integer_total_count_is_a_loud_error() {
        let run = r#"{"name":"t","status":"completed","conclusion":"success","html_url":""}"#;
        for payload in [
            format!(r#"{{"check_runs":[{run}]}}"#),
            format!(r#"{{"total_count":"1","check_runs":[{run}]}}"#),
            format!(r#"{{"total_count":-1,"check_runs":[{run}]}}"#),
        ] {
            let err = check_runs_from_json(&payload).unwrap_err().to_string();
            assert!(err.contains("total_count"), "names the field for {payload}: {err}");
        }
    }

    #[test]
    fn status_from_json_zero_total_is_none_even_when_state_says_pending() {
        // The observed real-world payload (scottidler/bump): zero statuses, but `state`
        // itself reads "pending" -- must be read as None, not Pending, or the CI gate
        // would poll every zero-status repo to the timeout.
        let state = status_from_json(r#"{"total_count":0,"state":"pending"}"#).unwrap();
        assert_eq!(state, StatusState::None);
    }

    #[test]
    fn status_from_json_maps_success_and_pending() {
        assert_eq!(
            status_from_json(r#"{"total_count":1,"state":"success"}"#).unwrap(),
            StatusState::Success
        );
        assert_eq!(
            status_from_json(r#"{"total_count":1,"state":"pending"}"#).unwrap(),
            StatusState::Pending
        );
    }

    #[test]
    fn status_from_json_unrecognized_state_fails_closed_as_failure() {
        assert_eq!(
            status_from_json(r#"{"total_count":1,"state":"failure"}"#).unwrap(),
            StatusState::Failure
        );
        assert_eq!(
            status_from_json(r#"{"total_count":1,"state":"error"}"#).unwrap(),
            StatusState::Failure
        );
    }

    #[test]
    fn status_from_json_missing_total_count_is_a_loud_error() {
        assert!(status_from_json(r#"{"state":"success"}"#).is_err());
    }

    #[test]
    fn persona_token_var_maps_work_org_and_defaults_home() {
        assert_eq!(persona_token_var("tatari-tv"), "GITHUB_PAT_WORK");
        assert_eq!(persona_token_var("scottidler"), "GITHUB_PAT_HOME");
        assert_eq!(persona_token_var("some-other-org"), "GITHUB_PAT_HOME");
    }
}
