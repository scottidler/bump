//! The CI gate: the `Ci` port, its production `GhCi`, and `wait_for_green`, which decides
//! from check runs + legacy statuses whether a sha may be tagged (design doc, API Design
//! "CI gate").

use super::tag::TagTarget;
use crate::config::{self, CiDeclaration};
use crate::git;
use crate::github::{self, CheckRuns, StatusState};
use eyre::{Context, Result, bail};
use log::debug;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// How often the CI gate re-reads check runs and legacy statuses.
pub const CI_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long the CI gate waits for ANY check run or status to register on a sha before
/// deciding from the sha's `.github/workflows` tree.
pub const CI_APPEAR_WINDOW: Duration = Duration::from_secs(120);

/// The default `--ci-timeout`: how long the gate waits on incomplete runs before refusing.
pub const DEFAULT_CI_TIMEOUT: Duration = Duration::from_secs(1800);

/// The CI seam for the gate. `Ok(None)` = the repo has no GitHub remote (nothing to wait
/// on); `Err` = an API/auth failure or a truncated read, which the gate fails closed on.
///
/// The poll interval and appear window live on the port because they describe the remote
/// CI system (how often it is sane to poll GitHub, how long GitHub takes to register a
/// run); test doubles return zero for both so no test ever sleeps.
pub trait Ci {
    fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>>;
    fn poll_interval(&self) -> Duration {
        CI_POLL_INTERVAL
    }
    fn appear_window(&self) -> Duration {
        CI_APPEAR_WINDOW
    }
}

/// Production `Ci`: `gh api` check-runs + legacy status for the sha.
pub struct GhCi;

impl Ci for GhCi {
    fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>> {
        github::check_runs(dir, sha)
    }
}

/// The CI gate's settings for one run, taken from the verb's opts.
#[derive(Debug, Clone, Copy)]
pub(super) struct CiGate {
    pub(super) enabled: bool,
    pub(super) timeout: Duration,
}

pub(super) fn ci_gate(enabled: bool, timeout: Duration) -> CiGate {
    CiGate { enabled, timeout }
}

/// The CI gate (design doc, API Design "CI gate"): poll check runs + legacy statuses for
/// `sha` until every run completed green (proceed) or anything is red, truncated, errored,
/// or timed out (refuse, no tag). Zero runs and zero statuses after the appear window are
/// decided by the sha's `.github/workflows` tree. A committed `ci: none` in `bump.yml` at
/// the sha, a repo with no GitHub remote, or `--no-ci-gate` skip the gate with a notice.
pub(super) fn wait_for_green<C: Ci>(dir: &Path, sha: &str, gate: CiGate, ci: &C, target: &TagTarget) -> Result<()> {
    debug!(
        "wait_for_green: dir={} sha={} enabled={} timeout={:?}",
        dir.display(),
        sha,
        gate.enabled,
        gate.timeout
    );
    let TagTarget { tag, rerun, .. } = *target;
    if !gate.enabled {
        println!("CI gate: SKIPPED (--no-ci-gate); {tag} is tagged without waiting for CI on {sha}");
        return Ok(());
    }
    if config::load_at(dir, sha)?.ci == Some(CiDeclaration::None) {
        println!("CI gate: skipped, bump.yml at {sha} declares `ci: none` (this repo's workflows never run on push)");
        return Ok(());
    }

    let start = Instant::now();
    let mut announced = false;
    loop {
        let runs = ci
            .check_runs(dir, sha)
            .wrap_err_with(|| format!("CI gate: could not read CI for {sha}; NO tag was created (fails closed)"))?;
        let Some(runs) = runs else {
            println!("CI gate: no GitHub remote, nothing to wait on");
            return Ok(());
        };
        debug!("wait_for_green: sha={sha} runs={runs:?}");

        if !runs.failed.is_empty() || runs.statuses == StatusState::Failure {
            let mut lines = String::new();
            for (name, url) in &runs.failed {
                lines.push_str(&format!("  FAILED  {name}  {url}\n"));
            }
            if runs.statuses == StatusState::Failure {
                lines.push_str("  FAILED  combined commit status (failure/error)\n");
            }
            bail!(
                "CI is RED on {sha}:\n{lines}NO tag was created. Fix it, commit, and re-run {rerun}: \
                 the re-run reuses {tag}, it never bumps past it."
            );
        }

        let no_ci_reported = runs.total == 0 && runs.statuses == StatusState::None;
        if no_ci_reported {
            if start.elapsed() >= ci.appear_window() {
                let window = ci.appear_window().as_secs();
                if git::has_workflows_at(dir, sha)? {
                    bail!(
                        "CI never registered on {sha} after {window}s; re-run when it has. If this repo's workflows \
                         never run on push, declare `ci: none` in bump.yml (committed, reviewed) and re-run."
                    );
                }
                println!(
                    "CI gate: no check runs or statuses on {sha} after {window}s, and the repo has no workflows at that sha; proceeding"
                );
                return Ok(());
            }
        } else if runs.incomplete == 0 && runs.statuses != StatusState::Pending {
            println!("CI gate: green ({} check run(s)) on {sha}", runs.total);
            return Ok(());
        } else if start.elapsed() >= gate.timeout {
            bail!(
                "CI gate: timed out after {}s with {} check run(s) incomplete{} on {sha}. NO tag was created.\n\
                 Re-run {rerun} when they finish (it reuses {tag}).",
                gate.timeout.as_secs(),
                runs.incomplete,
                if runs.statuses == StatusState::Pending {
                    " and the commit status pending"
                } else {
                    ""
                }
            );
        }

        if !announced {
            println!("CI gate: waiting on CI for {sha} (no tag exists yet)");
            announced = true;
        }
        thread::sleep(ci.poll_interval());
    }
}
