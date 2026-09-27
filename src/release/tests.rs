//! Tests for `bump release` / `bump finish` -- both flows, the CI gate, and PR construction.
//!
//! Real git in `TempDir`s against BARE local remotes (no network). `BUMP_GATES_PROBE`
//! forces the ungated verdict offline; env mutation is serialized behind the shared
//! `crate::ENV_LOCK` (env is process-global, so this must be the SAME lock the `main.rs`
//! gate tests use). Install and CI are exercised through injected doubles -- no real
//! `cargo install` or `gh api` ever runs, and every `Ci` double returns a zero poll
//! interval and appear window so no test sleeps.

use super::*;
use crate::config::Config;
use crate::git;
use crate::github::{self, CheckRuns, StatusState};
use crate::version::BumpType;
use eyre::{Result, bail};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

// ---- env-probe seam (shared lock, per module docs) --------------------------------

fn set_probe(val: &str) -> Option<String> {
    let prev = std::env::var("BUMP_GATES_PROBE").ok();
    unsafe { std::env::set_var("BUMP_GATES_PROBE", val) };
    prev
}

fn restore_probe(prev: Option<String>) {
    match prev {
        Some(v) => unsafe { std::env::set_var("BUMP_GATES_PROBE", v) },
        None => unsafe { std::env::remove_var("BUMP_GATES_PROBE") },
    }
}

// ---- test doubles -----------------------------------------------------------------

/// Records push ORDER. When `fail_branch`, `push_branch` records then fails WITHOUT
/// pushing (the rejected-push case). Otherwise both do a REAL push so the strengthened
/// confirm step (`HEAD == origin/<default>`) is genuinely exercised.
struct RecordingPusher {
    calls: RefCell<Vec<String>>,
    fail_branch: bool,
}

impl RecordingPusher {
    fn new(fail_branch: bool) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail_branch,
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Pusher for RecordingPusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("branch:{branch}"));
        if self.fail_branch {
            bail!("simulated branch push rejection");
        }
        git::push_branch(dir, branch)
    }
    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("tag:{tag}"));
        git::push_tag(dir, tag)
    }
    fn push_feature_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("feature:{branch}"));
        if self.fail_branch {
            bail!("simulated branch push rejection");
        }
        git::push_feature_branch(dir, branch)
    }
}

/// Records the OPEN-PR probe + create calls, and models `gh`'s own behavior: once
/// `create_pr` runs, an open PR EXISTS, so a subsequent `open_pr_exists` returns true.
/// This is exactly what makes "create exactly once across two runs" assertable with the
/// SAME instance across both runs -- no real `gh`.
struct RecordingPr {
    exists: RefCell<bool>,
    list_calls: RefCell<u32>,
    create_calls: RefCell<u32>,
    /// `(branch, base, title, body)` of every create call.
    created: RefCell<Vec<(String, String, String, String)>>,
}

impl RecordingPr {
    fn new() -> Self {
        Self {
            exists: RefCell::new(false),
            list_calls: RefCell::new(0),
            create_calls: RefCell::new(0),
            created: RefCell::new(Vec::new()),
        }
    }
    fn created(&self) -> Vec<(String, String, String, String)> {
        self.created.borrow().clone()
    }
    fn create_calls(&self) -> u32 {
        *self.create_calls.borrow()
    }
    fn list_calls(&self) -> u32 {
        *self.list_calls.borrow()
    }
}

impl Pr for RecordingPr {
    fn open_pr_exists(&self, _dir: &Path, _branch: &str) -> Result<bool> {
        *self.list_calls.borrow_mut() += 1;
        Ok(*self.exists.borrow())
    }
    fn create_pr(&self, _dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        *self.create_calls.borrow_mut() += 1;
        self.created
            .borrow_mut()
            .push((branch.into(), base.into(), title.into(), body.into()));
        // An open PR now exists (models gh): the next probe returns true.
        *self.exists.borrow_mut() = true;
        Ok(format!("https://github.com/o/r/pull/{}", self.create_calls.borrow()))
    }
}

/// A fresh no-op `Pr` for the UNGATED tests, whose paths never touch the PR seam. Bundled
/// as a helper so each ungated `release(...)` call can pass `&no_pr()` inline.
fn no_pr() -> RecordingPr {
    RecordingPr::new()
}

/// Records the RESOLVED install command WITHOUT executing it (no real `cargo install`).
struct RecordingInstaller {
    calls: RefCell<Vec<String>>,
}

impl RecordingInstaller {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Installer for RecordingInstaller {
    fn install(&self, _dir: &Path, command: &str) -> Result<()> {
        self.calls.borrow_mut().push(command.to_string());
        Ok(())
    }
}

/// Every `Ci` double polls with zero interval and a zero appear window, so no test sleeps
/// and a zero-runs read is decided on the first poll.
macro_rules! zero_timing {
    () => {
        fn poll_interval(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
        fn appear_window(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    };
}

fn green_runs() -> CheckRuns {
    CheckRuns {
        total: 1,
        ..CheckRuns::default()
    }
}

/// No GitHub remote: the gate has nothing to wait on and proceeds.
struct NoCi;

impl Ci for NoCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(None)
    }
    zero_timing!();
}

/// One completed check run with a failing conclusion.
struct RedCi;

impl Ci for RedCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(Some(CheckRuns {
            total: 1,
            failed: vec![("test".to_string(), "https://ci.example/run/1".to_string())],
            ..CheckRuns::default()
        }))
    }
    zero_timing!();
}

/// All green; records every sha it was asked about.
struct GreenCi {
    asked: RefCell<Vec<String>>,
}

impl GreenCi {
    fn new() -> Self {
        Self {
            asked: RefCell::new(Vec::new()),
        }
    }
    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl Ci for GreenCi {
    fn check_runs(&self, _dir: &Path, sha: &str) -> Result<Option<CheckRuns>> {
        self.asked.borrow_mut().push(sha.to_string());
        Ok(Some(green_runs()))
    }
    zero_timing!();
}

/// Zero check runs and zero legacy statuses: CI never registered.
struct SilentCi;

impl Ci for SilentCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(Some(CheckRuns::default()))
    }
    zero_timing!();
}

/// A check-runs payload whose `total_count` exceeds the runs returned, fed through the
/// production parser, so the port surfaces the truncation error exactly as `GhCi` would.
struct TruncatedCi;

impl Ci for TruncatedCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        let runs: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"name":"c{i}","status":"completed","conclusion":"success","html_url":""}}"#))
            .collect();
        let payload = format!(r#"{{"total_count":101,"check_runs":[{}]}}"#, runs.join(","));
        github::check_runs_from_json(&payload).map(Some)
    }
    zero_timing!();
}

/// An API/auth failure.
struct ErrCi;

impl Ci for ErrCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        bail!("gh api: HTTP 401 Bad credentials")
    }
    zero_timing!();
}

/// Runs that never complete, or a legacy status stuck at pending.
struct PendingCi {
    statuses_only: bool,
}

impl Ci for PendingCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(Some(if self.statuses_only {
            CheckRuns {
                statuses: StatusState::Pending,
                ..CheckRuns::default()
            }
        } else {
            CheckRuns {
                total: 2,
                incomplete: 1,
                ..CheckRuns::default()
            }
        }))
    }
    zero_timing!();
}

/// A combined legacy status of failure/error with no check runs.
struct StatusFailureCi;

impl Ci for StatusFailureCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(Some(CheckRuns {
            statuses: StatusState::Failure,
            ..CheckRuns::default()
        }))
    }
    zero_timing!();
}

/// Green, but on its FIRST read lands one more commit on origin/main from a side clone,
/// modelling a merge that races the CI wait. `manifest` is the version that commit's
/// Cargo.toml carries. Records every sha asked.
struct LandingCi {
    origin: std::path::PathBuf,
    manifest: String,
    landed: RefCell<Option<String>>,
    asked: RefCell<Vec<String>>,
}

impl LandingCi {
    fn new(origin: &Path, manifest: &str) -> Self {
        Self {
            origin: origin.to_path_buf(),
            manifest: manifest.to_string(),
            landed: RefCell::new(None),
            asked: RefCell::new(Vec::new()),
        }
    }
    fn landed(&self) -> Option<String> {
        self.landed.borrow().clone()
    }
    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl Ci for LandingCi {
    fn check_runs(&self, _dir: &Path, sha: &str) -> Result<Option<CheckRuns>> {
        self.asked.borrow_mut().push(sha.to_string());
        if self.landed.borrow().is_none() {
            let side = TempDir::new().unwrap();
            let s = side.path();
            git_ok(s, &["clone", "-q", self.origin.to_str().unwrap(), "."]);
            git_ok(s, &["config", "user.email", "test@test.com"]);
            git_ok(s, &["config", "user.name", "Test"]);
            write_cargo(s, &self.manifest);
            fs::write(s.join("raced.txt"), "merged during the CI wait").unwrap();
            git_ok(s, &["add", "-A"]);
            git_ok(s, &["commit", "-m", "raced merge"]);
            git_ok(s, &["push", "-q", "origin", "main"]);
            *self.landed.borrow_mut() = Some(git_ok(s, &["rev-parse", "HEAD"]));
        }
        Ok(Some(green_runs()))
    }
    zero_timing!();
}

// ---- git harness ------------------------------------------------------------------

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write_cargo(dir: &Path, version: &str) {
    fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"test-pkg\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
}

fn read_cargo_version(dir: &Path) -> String {
    let content = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("version = ") {
            return rest.trim_matches('"').to_string();
        }
    }
    panic!("no version in Cargo.toml");
}

/// A bare `origin` on `main` and a clone whose Cargo.toml is at `version`, tagged
/// `v<version>` with `main` AND the tag pushed (origin/HEAD set): a released version.
/// HEAD == origin/main, ahead == 0. (A local-only tag would make `version` a pending
/// version with a local tag at HEAD: the local-tag resume row, not a release.)
fn setup_released(version: &str) -> (TempDir, TempDir) {
    let origin = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .current_dir(origin.path())
        .output()
        .unwrap();

    let work = TempDir::new().unwrap();
    let w = work.path();
    git_ok(w, &["init", "-b", "main"]);
    git_ok(w, &["config", "user.email", "test@test.com"]);
    git_ok(w, &["config", "user.name", "Test"]);
    write_cargo(w, version);
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "init"]);
    git_ok(w, &["tag", "-a", &format!("v{version}"), "-m", &format!("v{version}")]);
    git_ok(w, &["remote", "add", "origin", origin.path().to_str().unwrap()]);
    git_ok(w, &["push", "-u", "origin", "main"]);
    git_ok(w, &["push", "origin", &format!("v{version}")]);
    git_ok(w, &["remote", "set-head", "origin", "main"]);
    (origin, work)
}

/// setup_released + a committed (unpushed) code change, so HEAD is ahead of origin.
fn setup_with_pending_commit(version: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(version);
    let w = work.path();
    fs::write(w.join("feature.txt"), "work").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "feature"]);
    (origin, work)
}

/// setup_released + a version bump that was committed AND pushed but never tagged on the
/// remote (a run killed between branch push and tag push). HEAD == origin/main.
fn setup_partial_release(from: &str, to: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(from);
    let w = work.path();
    write_cargo(w, to);
    git_ok(w, &["commit", "-am", &format!("Bump version to {to}")]);
    git_ok(w, &["push", "origin", "main"]);
    (origin, work)
}

fn auto_opts(bump_type: Option<BumpType>, dry_run: bool) -> ReleaseOpts {
    ReleaseOpts {
        bump_type,
        dry_run,
        install: InstallChoice::Auto,
        standalone: None,
        ci_gate: true,
        ci_timeout: DEFAULT_CI_TIMEOUT,
    }
}

/// `auto_opts` carrying Scott's standalone order.
fn standalone_opts(bump_type: Option<BumpType>, words: &str) -> ReleaseOpts {
    ReleaseOpts {
        standalone: Some(words.to_string()),
        ..auto_opts(bump_type, false)
    }
}

// ===================================================================================
// GATED flow (Phase 6): feature-branch fresh + idempotent re-run, level mismatch,
// stranded commits, gated generic. Real git in TempDirs + fake `Pr`; probe forced gated.
// ===================================================================================

/// setup_released + a feature branch carrying an unpushed code commit (the caller's
/// contract: the code change is already committed; the verb owns everything mechanical).
fn setup_gated_feature_branch(version: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(version);
    let w = work.path();
    git_ok(w, &["checkout", "-b", "feature"]);
    fs::write(w.join("feature.txt"), "work").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "feature work"]);
    (origin, work)
}

/// setup_released at `base_tag` + a feature branch carrying a work commit whose manifest is
/// ALREADY bumped to `bumped` (a prior gated run's `--no-tag` bump rode the branch). The
/// work commit keeps it from being a bump-only branch.
fn setup_gated_already_bumped(base_tag: &str, bumped: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(base_tag);
    let w = work.path();
    git_ok(w, &["checkout", "-b", "feature"]);
    fs::write(w.join("feature.txt"), "work").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "feature work"]);
    write_cargo(w, bumped);
    git_ok(w, &["commit", "-am", &format!("Bump version to {bumped}")]);
    (origin, work)
}

/// A generic (no-manifest) repo on a feature branch, pushed default + origin/HEAD set.
fn setup_generic_gated_feature() -> (TempDir, TempDir) {
    let origin = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .current_dir(origin.path())
        .output()
        .unwrap();
    let work = TempDir::new().unwrap();
    let w = work.path();
    git_ok(w, &["init", "-b", "main"]);
    git_ok(w, &["config", "user.email", "test@test.com"]);
    git_ok(w, &["config", "user.name", "Test"]);
    fs::write(w.join("README.md"), "# generic").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "init"]);
    git_ok(w, &["remote", "add", "origin", origin.path().to_str().unwrap()]);
    git_ok(w, &["push", "-u", "origin", "main"]);
    git_ok(w, &["remote", "set-head", "origin", "main"]);
    git_ok(w, &["checkout", "-b", "feature"]);
    fs::write(w.join("feature.txt"), "x").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "feature"]);
    (origin, work)
}

// ===================================================================================
// `bump finish` (Phase 7): the gated post-merge tag step. Real git in TempDirs against
// bare remotes; the tag push goes through RecordingPusher, install through
// RecordingInstaller (never a real cargo install). finish tags the merged tip via the
// shared --tag-only ladder and is gate-probe-independent, so these need no
// BUMP_GATES_PROBE and touch no process-global env (safe to run in parallel).
// ===================================================================================

fn finish_opts(dry_run: bool) -> FinishOpts {
    FinishOpts {
        dry_run,
        install: InstallChoice::Auto,
        ci_gate: true,
        ci_timeout: DEFAULT_CI_TIMEOUT,
    }
}

/// origin/main carries an UNTAGGED version bump (the merged PR); local main is rewound
/// BEHIND origin and HEAD sits on the merged feature branch -- the real post-merge state,
/// so finish must checkout main AND fast-forward to the merged tip before tagging.
fn setup_finish_untagged_merged(from: &str, to: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(from);
    let w = work.path();
    let base = git_ok(w, &["rev-parse", "HEAD"]);
    write_cargo(w, to);
    git_ok(w, &["commit", "-am", &format!("Bump version to {to}")]);
    git_ok(w, &["push", "origin", "main"]); // merged bump lands on origin/main
    git_ok(w, &["branch", "feature"]); // feature -> the merged bump commit
    git_ok(w, &["reset", "--hard", &base]); // local main rewinds BEHIND origin/main
    git_ok(w, &["checkout", "feature"]); // HEAD off the default branch
    (origin, work)
}

/// A non-bump commit merged to origin/main: the manifest version still equals the last
/// released tag (which is pushed to origin). The missed-bump state.
fn setup_finish_missed_bump(version: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(version);
    let w = work.path();
    git_ok(w, &["push", "origin", &format!("v{version}")]); // last release tag on origin
    let base = git_ok(w, &["rev-parse", "HEAD"]);
    fs::write(w.join("feature.txt"), "work").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "feature work (no version bump)"]);
    git_ok(w, &["push", "origin", "main"]); // a new commit merged, version UNCHANGED
    git_ok(w, &["branch", "feature"]);
    git_ok(w, &["reset", "--hard", &base]);
    git_ok(w, &["checkout", "feature"]);
    (origin, work)
}

/// origin/main carries the untagged merged bump; a prior finish created the LOCAL tag at
/// the merged tip but died before pushing it. HEAD == origin/main.
fn setup_finish_local_only_tag(from: &str, to: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(from);
    let w = work.path();
    write_cargo(w, to);
    git_ok(w, &["commit", "-am", &format!("Bump version to {to}")]);
    git_ok(w, &["push", "origin", "main"]);
    git_ok(w, &["tag", "-a", &format!("v{to}"), "-m", &format!("v{to}")]); // local tag, UNPUSHED
    (origin, work)
}

/// A fully released version: setup_finish_local_only_tag plus the tag pushed to origin.
fn setup_finish_fully_released(from: &str, to: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_finish_local_only_tag(from, to);
    let w = work.path();
    git_ok(w, &["push", "origin", &format!("v{to}")]); // tag now on origin at HEAD
    (origin, work)
}

// ===================================================================================
// Phase 3 (2026-09-26 one-release-command) helpers, shared by `gate` and `pr`.
// ===================================================================================

fn subject_of(dir: &Path, rev: &str) -> String {
    git_ok(dir, &["log", "-1", "--format=%s", rev])
}

fn assert_no_tag_anywhere(dir: &Path, tag: &str) {
    assert!(!git::tag_exists(dir, tag).unwrap(), "no LOCAL {tag}");
    assert_eq!(git::remote_tag_sha(dir, tag).unwrap(), None, "no REMOTE {tag}");
}

/// setup_released + main carrying a PUSHED, untagged version bump to `pending`.
fn setup_pending_on_origin(released: &str, pending: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(released);
    let w = work.path();
    write_cargo(w, pending);
    git_ok(w, &["commit", "-am", &format!("Bump version to {pending}")]);
    git_ok(w, &["push", "origin", "main"]);
    (origin, work)
}

mod finish;
mod gate;
mod gated;
mod install;
mod pr;
mod standalone;
mod ungated;
