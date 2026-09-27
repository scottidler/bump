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
        ci_gate: true,
        ci_timeout: DEFAULT_CI_TIMEOUT,
    }
}

// ===================================================================================
// Ungated e2e: branch push THEN tag push, IN ORDER; install resolved (not executed)
// ===================================================================================

#[test]
fn ungated_release_pushes_branch_then_tag_in_order() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let report = report.expect("ungated release must succeed");
    assert_eq!(report.tag, "v0.1.6");
    assert!(!report.resumed);
    // Branch push STRICTLY before tag push (the strengthened ordering).
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    // The tag is on origin (created at HEAD locally, then pushed). `remote_tag_sha` on an
    // exact refspec returns the tag-object SHA, so assert PRESENCE.
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag must be pushed to origin"
    );
    // The version file was bumped and the install command resolved (not executed).
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(report.install_command.as_deref(), Some("cargo install --path ."));
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    drop(origin);
}

/// The production `GitPusher` + `ShellInstaller` end-to-end: branch and tag both land on
/// origin, and the resolved install command actually runs (marker file appears).
#[test]
fn ungated_release_with_real_pusher_and_installer() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();

    let opts = ReleaseOpts {
        install: InstallChoice::Command("touch install-marker".to_string()),
        ..auto_opts(None, false)
    };
    let prev = set_probe("ungated");
    let report = release(dir, &opts, &GitPusher, &ShellInstaller, &GhPr, &GhCi);
    restore_probe(prev);

    let report = report.expect("real-pusher release must succeed");
    assert_eq!(report.tag, "v0.1.6");
    // Branch on origin at HEAD.
    let head = git::head_sha(dir).unwrap();
    let remote_main = git_ok(dir, &["rev-parse", "origin/main"]);
    assert_eq!(remote_main, head, "origin/main must equal HEAD");
    // Tag on origin (presence; exact refspec returns the tag-object SHA).
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag must be on origin"
    );
    // ShellInstaller actually ran the command.
    assert!(
        dir.join("install-marker").exists(),
        "install command must have executed"
    );
    drop(origin);
}

// ===================================================================================
// Rejected branch push leaves ZERO tags (local or remote) -- strengthened ordering
// ===================================================================================

#[test]
fn rejected_branch_push_leaves_no_tag() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(true); // branch push is rejected
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let result = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    assert!(result.is_err(), "a rejected branch push must fail the release");
    // The whole point: NO tag anywhere, and the tag push was never attempted.
    assert!(
        !git::tag_exists(dir, "v0.1.6").unwrap(),
        "no LOCAL tag on a rejected push"
    );
    assert_eq!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap(),
        None,
        "no REMOTE tag on a rejected push"
    );
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string()],
        "tag push never attempted"
    );
    assert!(installer.calls().is_empty(), "install never runs on a failed release");
    drop(origin);
}

// ===================================================================================
// RESUME: both sub-states (local tag ABSENT -> create+push; PRESENT -> push only)
// ===================================================================================

#[test]
fn resume_local_tag_absent_creates_and_pushes() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_partial_release("0.1.5", "0.1.6");
    let dir = work.path();
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "precondition: no local tag");

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let report = report.expect("resume must complete");
    assert!(report.resumed, "must be reported as a resume");
    assert_eq!(report.tag, "v0.1.6");
    // Created the missing tag, pushed it -- NO branch push, NO re-bump.
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    assert!(git::tag_exists(dir, "v0.1.6").unwrap(), "tag created locally");
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag pushed to origin"
    );
    assert_eq!(read_cargo_version(dir), "0.1.6", "version unchanged -- no re-bump");
    drop(origin);
}

#[test]
fn resume_local_tag_present_pushes_only() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_partial_release("0.1.5", "0.1.6");
    let dir = work.path();
    // Prior run created the local tag but died before pushing it.
    git_ok(dir, &["tag", "-a", "v0.1.6", "-m", "v0.1.6"]);
    let tag_sha_before = git::tag_sha(dir, "v0.1.6").unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let report = report.expect("resume must complete");
    assert!(report.resumed);
    assert_eq!(
        pusher.calls(),
        vec!["tag:v0.1.6".to_string()],
        "push only, no re-create"
    );
    // Local tag object untouched (not recreated), and now on origin.
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), tag_sha_before);
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag now on origin"
    );
    drop(origin);
}

/// A completed resume, re-run, is a clean refusal (already tagged) -- never a re-bump and
/// never a false "already released" claim mid-flight (that claim never appears here).
#[test]
fn resume_completes_then_second_run_refuses_without_rebump() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_partial_release("0.1.5", "0.1.6");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let first = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    // Second run: the remote now carries the tag, so there is nothing left to do.
    let second = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    assert!(first.expect("first resume completes").resumed);
    let err = second
        .expect_err("second run must refuse -- already tagged")
        .to_string();
    assert!(err.contains("already tagged"), "got: {err}");
    assert!(
        !err.contains("already released"),
        "must NOT claim 'already released': {err}"
    );
    assert_eq!(read_cargo_version(dir), "0.1.6", "no re-bump on the second run");
    drop(origin);
}

// ===================================================================================
// -n dry-run executes NOTHING
// ===================================================================================

#[test]
fn dry_run_executes_nothing() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let head_before = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let report = release(dir, &auto_opts(None, true), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let report = report.expect("dry-run must succeed");
    assert!(report.dry_run);
    assert_eq!(report.tag, "v0.1.6", "dry-run still reports the target tag");
    assert_eq!(report.install_command.as_deref(), Some("cargo install --path ."));
    // No side effects whatsoever.
    assert_eq!(git::head_sha(dir).unwrap(), head_before, "no commit/amend");
    assert_eq!(read_cargo_version(dir), "0.1.5", "no version write");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no tag");
    assert_eq!(git::remote_tag_sha(dir, "v0.1.6").unwrap(), None, "no remote tag");
    assert!(pusher.calls().is_empty(), "no push");
    assert!(installer.calls().is_empty(), "no install");
    drop(origin);
}

// ===================================================================================
// Each UNGATED bash-driver `die` condition reproduced as a distinct refusal
// ===================================================================================

/// bash: `die "not inside a git repo"`.
#[test]
fn refuses_when_not_a_git_repo() {
    let tmp = TempDir::new().unwrap();
    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = release(
        tmp.path(),
        &auto_opts(None, false),
        &pusher,
        &installer,
        &no_pr(),
        &NoCi,
    )
    .expect_err("must refuse outside a git repo")
    .to_string();
    assert!(err.contains("not a git repository"), "got: {err}");
}

/// bash: `die "tree is dirty..."`.
#[test]
fn refuses_dirty_tree() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    fs::write(dir.join("dirty.txt"), "x").unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("dirty tree must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("dirty"), "got: {err}");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// bash: `die "ungated release runs from the default branch..."`.
#[test]
fn refuses_when_not_on_default() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "feature"]);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("off-default must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("git checkout main"), "must print the exact fix: {err}");
    drop(origin);
}

/// bash: `die "$DEFAULT is $BEHIND commit(s) behind..."`.
#[test]
fn refuses_when_behind_origin() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let c1 = git::head_sha(dir).unwrap();
    git_ok(dir, &["commit", "--allow-empty", "-m", "c2"]);
    git_ok(dir, &["push", "origin", "main"]);
    git_ok(dir, &["reset", "--hard", &c1]); // local now behind origin/main

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("behind must refuse")
        .to_string();
    restore_probe(prev);
    assert!(
        err.contains("git pull --ff-only origin main"),
        "must print the exact fix: {err}"
    );
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// bash: `die "nothing to release: HEAD == origin/$DEFAULT..."` -- here the version is
/// already tagged on the remote.
#[test]
fn refuses_when_nothing_to_release() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["push", "origin", "v0.1.5"]); // version already tagged on origin

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("nothing-to-release must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("nothing ahead"), "got: {err}");
    assert!(err.contains("already tagged"), "got: {err}");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// bash: `die "gate status is UNKNOWN..."` -- but `release` FAILS CLOSED (it pushes).
#[test]
fn refuses_when_gate_unknown() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("unknown:offline");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("unknown gate must fail closed")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("UNKNOWN"), "got: {err}");
    assert!(err.contains("offline"), "must carry the probe reason: {err}");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// Detached HEAD refuses with the one exact fix.
#[test]
fn refuses_on_detached_head() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "--detach", "HEAD"]);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi)
        .expect_err("detached HEAD must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("detached"), "got: {err}");
    drop(origin);
}

/// Gated, on the default branch, clean, HEAD == origin: refuse -- bump rides a feature PR,
/// never the default branch. (Phase 6 replaces Phase 5's "not this phase" gated refusal.)
#[test]
fn gated_on_default_clean_refuses_bump_rides_a_pr() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &pr, &NoCi)
        .expect_err("gated on default clean must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("bump rides a feature PR"), "got: {err}");
    assert!(pusher.calls().is_empty());
    assert_eq!(pr.create_calls(), 0, "no PR touched on a refusal");
    // NO tag created on this gated path either.
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap());
    drop(origin);
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

/// setup_released at `base_tag` + a feature branch whose manifest is ALREADY bumped to
/// `bumped` (a prior gated run's `--no-tag` bump rode the branch).
fn setup_gated_already_bumped(base_tag: &str, bumped: &str) -> (TempDir, TempDir) {
    let (origin, work) = setup_released(base_tag);
    let w = work.path();
    git_ok(w, &["checkout", "-b", "feature"]);
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

/// Gated e2e: two runs, PAUSED both times, branch pushed, PR-create invoked EXACTLY ONCE
/// (first run creates; second sees the open PR via the list-probe fake and skips), and NO
/// tag anywhere across the fresh + resume paths.
#[test]
fn gated_release_pauses_and_creates_pr_once_across_two_runs() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_gated_feature_branch("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let pr = RecordingPr::new(); // ONE instance across both runs
    let prev = set_probe("gated:pull_request");
    let first = release(dir, &auto_opts(None, false), &pusher, &installer, &pr, &NoCi);
    let second = release(dir, &auto_opts(None, false), &pusher, &installer, &pr, &NoCi);
    restore_probe(prev);

    let first = first.expect("first gated run pauses");
    assert!(first.paused, "gated run must PAUSE (exit-0 semantics)");
    assert!(!first.dry_run);
    assert_eq!(first.tag, "v0.1.6");
    assert!(first.install_command.is_none(), "no install on a paused gated run");
    // Fresh run bumped the version onto the branch.
    assert_eq!(read_cargo_version(dir), "0.1.6", "version bump rode the branch");

    let second = second.expect("second gated run also pauses (idempotent)");
    assert!(second.paused);

    // PR created EXACTLY ONCE across two runs; the probe ran on each.
    assert_eq!(pr.create_calls(), 1, "PR create must run exactly once");
    assert!(pr.list_calls() >= 2, "the open-PR probe runs on every run");

    // Only feature-branch pushes, NEVER a tag push.
    assert!(
        !pusher.calls().is_empty() && pusher.calls().iter().all(|c| c.starts_with("feature:")),
        "only feature-branch pushes: {:?}",
        pusher.calls()
    );
    assert!(
        !pusher.calls().iter().any(|c| c.starts_with("tag:")),
        "no tag push in the gated flow"
    );

    // NO tag anywhere -- gated release never tags (that is bump finish's job).
    assert!(
        !git::tag_exists(dir, "v0.1.6").unwrap(),
        "no local tag in the gated flow"
    );
    assert_eq!(git::remote_tag_sha(dir, "v0.1.6").unwrap(), None, "no remote tag");
    // Install never runs on a paused gated release.
    assert!(installer.calls().is_empty(), "no install on a paused gated release");
    drop(origin);
}

/// A version already bumped to vX on the branch, re-run with a level implying vY != vX,
/// refuses NAMING BOTH versions -- never silently keeps either.
#[test]
fn gated_level_mismatch_refuses_naming_both_versions() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    // minor bump (0.2.0) already rode the branch off tag 0.1.0.
    let (origin, work) = setup_gated_already_bumped("0.1.0", "0.2.0");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    // Re-run with -M (major) -> implies v1.0.0, but v0.2.0 is riding.
    let err = release(
        dir,
        &auto_opts(Some(BumpType::Major), false),
        &pusher,
        &installer,
        &pr,
        &NoCi,
    )
    .expect_err("level mismatch must refuse")
    .to_string();
    restore_probe(prev);

    assert!(err.contains("v0.2.0"), "must name the riding version: {err}");
    assert!(err.contains("v1.0.0"), "must name the implied version: {err}");
    // Nothing touched: no push, no PR, no re-bump.
    assert!(pusher.calls().is_empty(), "nothing pushed on a mismatch refusal");
    assert_eq!(pr.create_calls(), 0);
    assert_eq!(
        read_cargo_version(dir),
        "0.2.0",
        "version left as-is -- neither kept nor changed"
    );
    drop(origin);
}

/// On the gated default branch with local commits NOT on origin: refuse printing the
/// LITERAL rescue commands, and NEVER create a branch or reset history.
#[test]
fn gated_stranded_commits_refuse_with_literal_rescue_commands() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5"); // on main, HEAD ahead of origin
    let dir = work.path();
    let head_before = git::head_sha(dir).unwrap();
    let branches_before = git_ok(dir, &["branch", "--format=%(refname:short)"]);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &pr, &NoCi)
        .expect_err("stranded commits on the gated default must refuse")
        .to_string();
    restore_probe(prev);

    // LITERAL runnable commands, never a prose description.
    assert!(err.contains("git branch stranded-"), "literal `git branch` cmd: {err}");
    assert!(err.contains("git reset --hard origin/main"), "literal reset cmd: {err}");
    assert!(err.contains("bump release"), "the re-run instruction: {err}");
    // The verb NEVER created a branch or reset history itself.
    assert_eq!(git::head_sha(dir).unwrap(), head_before, "HEAD untouched (no reset)");
    assert_eq!(
        git_ok(dir, &["branch", "--format=%(refname:short)"]),
        branches_before,
        "no branch created by the verb"
    );
    assert!(pusher.calls().is_empty(), "nothing pushed");
    drop(origin);
}

/// A gated repo with no version-bearing manifest is unsupported (bump finish cannot derive
/// a version); both verbs refuse.
#[test]
fn gated_generic_repo_is_unsupported() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_generic_gated_feature();
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let err = release(dir, &auto_opts(None, false), &pusher, &installer, &pr, &NoCi)
        .expect_err("gated generic must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("generic"), "got: {err}");
    assert!(err.contains("unsupported"), "got: {err}");
    assert!(pusher.calls().is_empty());
    assert_eq!(pr.create_calls(), 0);
    drop(origin);
}

// ===================================================================================
// Install resolution (pure): precedence override > config > default-if-Cargo > skip
// ===================================================================================

#[test]
fn resolve_install_explicit_override_wins() {
    let tmp = TempDir::new().unwrap();
    let config = Config {
        install: Some("from-config".to_string()),
        ..Config::default()
    };
    let choice = InstallChoice::Command("explicit".to_string());
    assert_eq!(
        resolve_install(tmp.path(), &choice, &config).as_deref(),
        Some("explicit")
    );
}

#[test]
fn resolve_install_skip_is_none() {
    let tmp = TempDir::new().unwrap();
    let config = Config {
        install: Some("from-config".to_string()),
        ..Config::default()
    };
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Skip, &config), None);
}

#[test]
fn resolve_install_auto_prefers_config() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0"); // Cargo present, but config wins
    let config = Config {
        install: Some("make install".to_string()),
        ..Config::default()
    };
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("make install")
    );
}

#[test]
fn resolve_install_auto_defaults_to_cargo_when_cargo_present() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    let config = Config::default();
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("cargo install --path .")
    );
}

#[test]
fn resolve_install_auto_skips_when_no_manifest_and_no_config() {
    let tmp = TempDir::new().unwrap();
    let config = Config::default();
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Auto, &config), None);
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

/// Row 1 e2e: origin/main carries the untagged merged bump. finish checks out main,
/// fast-forwards to the merged tip, creates an ANNOTATED tag on that commit, pushes it BY
/// NAME, and installs.
#[test]
fn finish_tags_merged_tip_and_pushes_by_name() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("finish must tag the merged tip");

    assert_eq!(report.tag, "v0.1.6");
    assert!(!report.resumed);
    assert!(!report.paused);
    // Only a tag push (finish never pushes a branch).
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    // The tag is ANNOTATED and points at the merged tip (== origin/main).
    assert_eq!(
        git_ok(dir, &["cat-file", "-t", "v0.1.6"]),
        "tag",
        "must be an ANNOTATED tag"
    );
    let merged = git_ok(dir, &["rev-parse", "origin/main"]);
    assert_eq!(
        git::tag_sha(dir, "v0.1.6").unwrap(),
        merged,
        "tag points at the merged commit"
    );
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag pushed to origin"
    );
    // finish reached the default branch and fast-forwarded to the merged version.
    assert_eq!(
        git::current_branch(dir).unwrap(),
        "main",
        "checked out the default branch"
    );
    assert_eq!(read_cargo_version(dir), "0.1.6", "fast-forwarded to the merged bump");
    // Install resolved AND run through the double.
    assert_eq!(report.install_command.as_deref(), Some("cargo install --path ."));
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    drop(origin);
}

/// Row 2: a commit merged to origin/main WITHOUT a version bump (version == last tag).
/// finish refuses with the branch instruction; nothing tagged or installed.
#[test]
fn finish_missed_bump_refuses_with_branch_instruction() {
    let (origin, work) = setup_finish_missed_bump("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("missed bump must refuse")
        .to_string();
    assert!(err.contains("no untagged version"), "got: {err}");
    assert!(
        err.contains("run bump release on a branch"),
        "must point at the branch flow: {err}"
    );
    assert!(pusher.calls().is_empty(), "no tag push on a refusal");
    assert!(installer.calls().is_empty(), "no install on a refusal");
    drop(origin);
}

/// Row 4: a local-only tag at the merged tip RESUMES (pushes the tag), NEVER no-ops, and is
/// NOT reported as already-released. Distinct from the remote-tag no-op below.
#[test]
fn finish_local_only_tag_resumes_and_pushes() {
    let (origin, work) = setup_finish_local_only_tag("0.1.5", "0.1.6");
    let dir = work.path();
    assert!(
        git::tag_exists(dir, "v0.1.6").unwrap(),
        "precondition: local tag present"
    );
    assert_eq!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap(),
        None,
        "precondition: NOT on the remote"
    );
    let tag_before = git::tag_sha(dir, "v0.1.6").unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("finish must resume");

    assert!(report.resumed, "a local-only tag is a RESUME, not already-released");
    assert_eq!(report.tag, "v0.1.6");
    // Pushed only (never re-created); the tag object is unchanged and now on origin.
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), tag_before, "tag NOT recreated");
    assert!(
        git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(),
        "tag now on origin"
    );
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    drop(origin);
}

/// Row 3: a fully-released version (tag on origin at the merged tip) is a clean NO-OP, and a
/// SECOND full finish run is still a clean no-op -- never a resume, never a push, never an
/// install.
#[test]
fn finish_remote_tag_is_clean_noop_across_two_runs() {
    let (origin, work) = setup_finish_fully_released("0.1.5", "0.1.6");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let first = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("first finish no-ops");
    let second = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("second finish also no-ops");

    for report in [first, second] {
        assert_eq!(report.tag, "v0.1.6");
        assert!(!report.resumed, "already-released is NOT a resume");
        assert!(report.install_command.is_none(), "no install on a no-op");
    }
    assert!(pusher.calls().is_empty(), "no-op never pushes a tag");
    assert!(installer.calls().is_empty(), "no-op never installs");
    drop(origin);
}

/// Row 5: a generic (no-manifest) repo -- finish cannot derive a version, so it refuses with
/// the gated-generic-unsupported message before any checkout.
#[test]
fn finish_gated_generic_repo_refuses() {
    let (origin, work) = setup_generic_gated_feature();
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("gated generic must refuse")
        .to_string();
    assert!(err.contains("generic"), "got: {err}");
    assert!(err.contains("unsupported"), "got: {err}");
    assert!(err.contains("manifest"), "must explain the missing manifest: {err}");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// Row 6: an UNTRACKED file does not refuse -- finish never stages or commits anything
/// (it only checks out, fast-forwards, and tags), so a stray file can't ride onto the
/// release. finish proceeds and completes normally.
#[test]
fn finish_allows_untracked_file() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    fs::write(dir.join("stray.txt"), "x").unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).unwrap();

    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    drop(origin);
}

/// Row 6: a TRACKED, uncommitted modification refuses BEFORE any checkout (which would
/// clobber it).
#[test]
fn finish_refuses_tracked_change() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    let cargo_toml = dir.join("Cargo.toml");
    let contents = fs::read_to_string(&cargo_toml).unwrap();
    fs::write(&cargo_toml, format!("{contents}\n# tracked edit\n")).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("tracked change must refuse")
        .to_string();
    assert!(err.contains("uncommitted tracked changes"), "got: {err}");
    assert!(pusher.calls().is_empty());
    // No checkout happened -- HEAD is still on the feature branch.
    assert_eq!(
        git::current_branch(dir).unwrap(),
        "feature",
        "no checkout on a dirty refusal"
    );
    drop(origin);
}

/// `-n` dry run echoes the plan and mutates NOTHING -- no checkout, no tag, no push, no
/// install.
#[test]
fn finish_dry_run_executes_nothing() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    let branch_before = git::current_branch(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(dir, &finish_opts(true), &pusher, &installer, &NoCi).expect("dry-run must succeed");

    assert!(report.dry_run);
    assert_eq!(
        report.tag, "v0.1.6",
        "dry-run reports the current manifest version's tag"
    );
    assert_eq!(report.install_command.as_deref(), Some("cargo install --path ."));
    // No side effects whatsoever.
    assert_eq!(
        git::current_branch(dir).unwrap(),
        branch_before,
        "no checkout in dry-run"
    );
    assert!(pusher.calls().is_empty(), "no push in dry-run");
    assert!(installer.calls().is_empty(), "no install in dry-run");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no tag in dry-run");
    drop(origin);
}

// ===================================================================================
// Phase 3 (2026-09-26 one-release-command): the CI gate, the pending version, tag
// placement on the verified sha, and the PR built by construction.
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

/// Break-the-code proof (design doc, Testing Strategy): delete the `wait_for_green` call
/// in `gate_tag_and_push` and the first assertion block fails, because the red run tags.
#[test]
fn red_ci_leaves_no_tag_and_green_rerun_resumes_same_version() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let prev = set_probe("ungated");

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let red = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &RedCi);
    let err = red.expect_err("red CI must refuse").to_string();
    assert!(err.contains("RED"), "got: {err}");
    assert!(err.contains("test"), "names the failed run: {err}");
    assert!(
        err.contains("reuses v0.1.6"),
        "says the re-run reuses the version: {err}"
    );
    // The version commit is on origin, untagged, and no tag exists anywhere.
    let head = git::head_sha(dir).unwrap();
    assert_eq!(
        git_ok(dir, &["rev-parse", "origin/main"]),
        head,
        "version commit on origin"
    );
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert!(installer.calls().is_empty(), "no install on red");

    let green = GreenCi::new();
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &green)
        .expect("green re-run must release");
    restore_probe(prev);

    assert!(report.resumed, "the re-run is the RESUME row");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(green.asked(), vec![head.clone()], "GreenCi asked exactly HEAD's sha");
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), head, "tag on the verified sha");
    assert!(git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(), "tag pushed");
    assert_eq!(read_cargo_version(dir), "0.1.6", "never re-bumped");
    drop(origin);
}

#[test]
fn red_ci_then_fix_commit_tags_pending_version() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let prev = set_probe("ungated");

    let installer = RecordingInstaller::new();
    let red_pusher = RecordingPusher::new(false);
    let red = release(dir, &auto_opts(None, false), &red_pusher, &installer, &no_pr(), &RedCi)
        .expect_err("red CI must refuse")
        .to_string();
    assert!(red.contains("RED"), "got: {red}");
    assert_no_tag_anywhere(dir, "v0.1.6");

    // The fix lands on main, unpushed.
    fs::write(dir.join("fix.txt"), "fix").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "fix the red test"]);
    let fix = git::head_sha(dir).unwrap();

    // `-m` on this tree names both versions and touches nothing.
    let level_pusher = RecordingPusher::new(false);
    let err = release(
        dir,
        &auto_opts(Some(BumpType::Minor), false),
        &level_pusher,
        &installer,
        &no_pr(),
        &GreenCi::new(),
    )
    .expect_err("-m on a pending version must refuse")
    .to_string();
    assert!(err.contains("v0.1.6"), "names the pending version: {err}");
    assert!(err.contains("v0.2.0"), "names the implied version: {err}");
    assert!(level_pusher.calls().is_empty(), "nothing pushed on the refusal");

    // A bare re-run pushes the fix and tags the PENDING version on it.
    let pusher = RecordingPusher::new(false);
    let green = GreenCi::new();
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &green)
        .expect("bare re-run must release the pending version");
    restore_probe(prev);

    assert_eq!(report.tag, "v0.1.6", "never v0.1.7");
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    assert_eq!(git_ok(dir, &["rev-parse", "origin/main"]), fix, "fix pushed");
    assert_eq!(green.asked(), vec![fix.clone()]);
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), fix);
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);
}

/// A CI read that fails (truncated, or an API/auth error) refuses with no tag and no push.
fn assert_bad_ci_read_leaves_no_tag<C: Ci>(ci: &C, expect: &str) {
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("ungated");
    let result = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        ci,
    );
    restore_probe(prev);

    let err = format!("{:#}", result.expect_err("a bad CI read must fail closed"));
    assert!(err.contains(expect), "got: {err}");
    assert!(err.contains("NO tag was created"), "got: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert!(!pusher.calls().iter().any(|c| c.starts_with("tag:")), "no tag push");
    drop(origin);
}

#[test]
fn truncated_or_erroring_ci_leaves_no_tag() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    assert_bad_ci_read_leaves_no_tag(&TruncatedCi, "truncated");
    assert_bad_ci_read_leaves_no_tag(&ErrCi, "401");
}

#[test]
fn silent_ci_refuses_when_workflows_exist_and_proceeds_when_none() {
    let _guard = crate::ENV_LOCK.lock().unwrap();

    // With a tracked workflow at the sha: CI should have registered, so refuse.
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    fs::write(dir.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "add ci"]);
    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(None, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &SilentCi,
    )
    .expect_err("zero runs with workflows present must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("CI never registered"), "got: {err}");
    assert!(err.contains("ci: none"), "names the bump.yml declaration: {err}");
    assert!(!err.contains("--no-ci-gate"), "never hands the agent the flag: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);

    // No workflows at the sha: nothing will ever register, so proceed.
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let prev = set_probe("ungated");
    let report = release(
        dir,
        &auto_opts(None, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &SilentCi,
    )
    .expect("zero runs and no workflows must proceed");
    restore_probe(prev);
    assert_eq!(report.tag, "v0.1.6");
    assert!(git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some());
    drop(origin);
}

/// Incomplete check runs, and a legacy status stuck at `pending` with zero runs, both
/// wait and are subject to `--ci-timeout` (zero here, so the first poll times out).
#[test]
fn incomplete_ci_times_out_without_tag() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    for statuses_only in [false, true] {
        let (origin, work) = setup_with_pending_commit("0.1.5");
        let dir = work.path();
        let opts = ReleaseOpts {
            ci_timeout: std::time::Duration::ZERO,
            ..auto_opts(None, false)
        };
        let prev = set_probe("ungated");
        let err = release(
            dir,
            &opts,
            &RecordingPusher::new(false),
            &RecordingInstaller::new(),
            &no_pr(),
            &PendingCi { statuses_only },
        )
        .expect_err("incomplete CI past the timeout must refuse")
        .to_string();
        restore_probe(prev);
        assert!(err.contains("timed out"), "statuses_only={statuses_only}: {err}");
        assert_no_tag_anywhere(dir, "v0.1.6");
        drop(origin);
    }
}

#[test]
fn legacy_status_failure_is_red() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(None, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &StatusFailureCi,
    )
    .expect_err("a failed commit status must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("commit status"), "got: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);
}

/// `ci_gate: false` (the future `--no-ci-gate`) skips the gate entirely: even red CI tags.
#[test]
fn disabled_ci_gate_tags_without_reading_ci() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let opts = ReleaseOpts {
        ci_gate: false,
        ..auto_opts(None, false)
    };
    let prev = set_probe("ungated");
    let report = release(
        dir,
        &opts,
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &RedCi,
    )
    .expect("a disabled gate never reads CI");
    restore_probe(prev);
    assert_eq!(report.tag, "v0.1.6");
    drop(origin);
}

/// A COMMITTED `ci: none` at the sha skips the gate; the same line in an untracked
/// `bump.yml` does not.
#[test]
fn wait_for_green_honors_committed_ci_none_only() {
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    fs::write(dir.join(".github/workflows/ci.yml"), "on: workflow_dispatch\n").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "manual-only ci"]);
    let version = version::parse_version("0.1.6").unwrap();
    let target = TagTarget {
        tag: "v0.1.6",
        version: &version,
        default: "main",
        rerun: "bump release",
    };
    let gate = ci_gate(true, DEFAULT_CI_TIMEOUT);

    fs::write(dir.join("bump.yml"), "ci: none\n").unwrap();
    let head = git::head_sha(dir).unwrap();
    assert!(
        wait_for_green(dir, &head, gate, &SilentCi, &target).is_err(),
        "an UNTRACKED ci: none must not switch the gate off"
    );

    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "declare ci: none"]);
    let head = git::head_sha(dir).unwrap();
    wait_for_green(dir, &head, gate, &SilentCi, &target).expect("a committed ci: none skips the gate");
    drop(origin);
}

/// Break-the-code proof (design doc, Testing Strategy): remove the `never_amend`
/// short-circuit and the `HEAD~1` assertion fails, because the bump amends the feature
/// commit.
#[test]
fn gated_pr_title_and_body_are_built_from_branch_and_commits() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "add-thing"]);
    fs::write(dir.join("thing.txt"), "thing").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "feat(core): add thing"]);

    let pr = RecordingPr::new();
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("gated:pull_request");
    let report = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    )
    .expect("gated release pauses");
    restore_probe(prev);

    assert!(report.paused);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(report.pr_url.as_deref(), Some("https://github.com/o/r/pull/1"));
    let created = pr.created();
    assert_eq!(created.len(), 1);
    let (branch, base, title, body) = &created[0];
    assert_eq!(branch, "add-thing");
    assert_eq!(base, "main");
    assert_eq!(title, "feat(core): add thing");
    assert!(
        body.contains("- feat(core): add thing"),
        "body lists the subjects: {body}"
    );
    assert_eq!(
        body.lines().last(),
        Some("Release: rides this PR (v0.1.6)"),
        "body: {body}"
    );
    assert_eq!(
        subject_of(dir, "HEAD~1"),
        "feat(core): add thing",
        "the bump is a NEW commit"
    );
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);
}

#[test]
fn inherited_pending_version_bumps_again() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "more-work"]);
    fs::write(dir.join("work.txt"), "work").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "fix: more work"]);

    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let report = release(
        dir,
        &auto_opts(None, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    )
    .expect("inherited pending version bumps again and pauses");
    restore_probe(prev);

    assert_eq!(report.tag, "v0.1.7");
    assert_eq!(read_cargo_version(dir), "0.1.7", "bumped FROM the manifest version");
    assert_eq!(subject_of(dir, "HEAD~1"), "fix: more work", "a new version commit");
    let (_, _, title, body) = &pr.created()[0];
    assert_eq!(title, "fix: more work");
    assert_eq!(
        body.lines().last(),
        Some("Release: rides this PR (v0.1.7)"),
        "body: {body}"
    );
    let notice = report.notice.expect("the pause names the inherited version");
    assert!(notice.contains("untagged v0.1.6"), "notice: {notice}");
    assert!(notice.contains("bump finish"), "notice: {notice}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);
}

#[test]
fn gated_bad_branch_name_refuses_before_mutation() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "Add_thing"]);
    fs::write(dir.join("thing.txt"), "thing").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "feat: add thing"]);
    let head = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let err = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    )
    .expect_err("a branch that is not its own slug must refuse")
    .to_string();
    restore_probe(prev);

    assert!(err.contains("git branch -m add-thing"), "got: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), head, "no version commit");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_eq!(pr.list_calls(), 0, "no PR touched");
    drop(origin);
}

#[test]
fn manifest_below_latest_tag_refuses_by_name() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    write_cargo(dir, "0.1.4");
    git_ok(dir, &["commit", "-am", "oops"]);

    let pusher = RecordingPusher::new(false);
    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &NoCi,
    )
    .expect_err("a manifest below the latest tag must refuse")
    .to_string();
    restore_probe(prev);
    assert!(
        err.contains("manifest 0.1.4 is below the latest tag v0.1.5"),
        "got: {err}"
    );
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// The three tag-placement cases from the design doc: a same-version commit landing
/// during the wait restarts the gate and moves the tag to the new tip; a different-version
/// commit refuses with no local tag; a force-move between tag and push refuses, leaves the
/// local tag, and the re-run takes the local-tag resume row.
#[test]
fn tag_binds_to_verified_sha() {
    let _guard = crate::ENV_LOCK.lock().unwrap();

    // 1. Same version lands on origin/main during the wait -> restart on the new tip.
    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();
    let ci = LandingCi::new(origin.path(), "0.1.6");
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("ungated");
    let report = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &ci,
    )
    .expect("a same-version tip restarts the gate and tags the tip");
    restore_probe(prev);
    let landed = ci.landed().expect("the double landed a commit");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(
        ci.asked(),
        vec![head.clone(), landed.clone()],
        "gate re-ran on the new tip"
    );
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), landed, "tag on the NEW tip");
    assert_eq!(
        git::remote_tag_commit(dir, "v0.1.6").unwrap(),
        Some(landed),
        "and pushed"
    );
    drop(origin);

    // 2. A different version lands during the wait -> refuse, no local tag.
    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    let ci = LandingCi::new(origin.path(), "0.1.7");
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &ci,
    )
    .expect_err("a changed version at the tip must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("0.1.7"), "names what the tip carries: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert!(pusher.calls().is_empty(), "no tag push");
    drop(origin);

    // 3. origin/main is force-moved between the tag and the push. A one-shot
    //    `reference-transaction` hook rewinds origin's main the moment the local tag ref
    //    is committed, so the real code path runs with no seam added for the test.
    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();
    let rewind = git_ok(dir, &["rev-parse", "HEAD~1"]);
    let hooks = dir.join(".git/hooks");
    let marker = dir.join(".git/rewound");
    let hook = hooks.join("reference-transaction");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n\
             [ \"$1\" = committed ] || exit 0\n\
             grep -q ' refs/tags/v0.1.6$' || exit 0\n\
             [ -e '{marker}' ] && exit 0\n\
             touch '{marker}'\n\
             unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX\n\
             git -C '{origin}' update-ref refs/heads/main {rewind}\n",
            marker = marker.display(),
            origin = origin.path().display(),
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&hook).status().unwrap();
    git_ok(dir, &["config", "core.hooksPath", hooks.to_str().unwrap()]);

    let pusher = RecordingPusher::new(false);
    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &GreenCi::new(),
    )
    .expect_err("a tip that moves between tag and push must refuse")
    .to_string();
    assert!(marker.exists(), "the hook fired");
    assert!(err.contains("NOT pushed"), "got: {err}");
    assert_eq!(
        git::tag_sha(dir, "v0.1.6").unwrap(),
        head,
        "the local tag stays on the verified sha"
    );
    assert_eq!(git::remote_tag_sha(dir, "v0.1.6").unwrap(), None, "never pushed");
    assert!(pusher.calls().is_empty(), "no tag push");

    // origin comes back to the tagged sha: the re-run is the local-tag resume row.
    git_ok(origin.path(), &["update-ref", "refs/heads/main", &head]);
    let green = GreenCi::new();
    let pusher = RecordingPusher::new(false);
    let report = release(
        dir,
        &auto_opts(None, false),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &green,
    )
    .expect("the re-run pushes the local tag");
    restore_probe(prev);
    assert!(report.resumed);
    assert_eq!(
        green.asked(),
        vec![head.clone()],
        "the resume row re-runs the gate on the tip"
    );
    assert_eq!(
        pusher.calls(),
        vec!["tag:v0.1.6".to_string()],
        "push only, never re-created"
    );
    assert_eq!(git::remote_tag_commit(dir, "v0.1.6").unwrap(), Some(head));
    drop(origin);
}

/// Both `bump finish` tag arms (no tag yet / local tag at the merged tip) wait for green
/// on the merged sha; red leaves no tag and pushes nothing.
#[test]
fn finish_gates_both_tag_arms_on_ci() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(dir, &finish_opts(false), &pusher, &installer, &RedCi)
        .expect_err("red CI on the merged sha must refuse")
        .to_string();
    assert!(err.contains("bump finish"), "names the re-run: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert!(pusher.calls().is_empty() && installer.calls().is_empty());

    let green = GreenCi::new();
    let report = finish(dir, &finish_opts(false), &pusher, &installer, &green).expect("green finish tags");
    let merged = git_ok(dir, &["rev-parse", "origin/main"]);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(green.asked(), vec![merged.clone()], "gate ran on the merged sha");
    assert_eq!(git::tag_sha(dir, "v0.1.6").unwrap(), merged);
    drop(origin);

    let (origin, work) = setup_finish_local_only_tag("0.1.5", "0.1.6");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let err = finish(dir, &finish_opts(false), &pusher, &RecordingInstaller::new(), &RedCi)
        .expect_err("red CI must refuse the local-tag resume too")
        .to_string();
    assert!(err.contains("RED"), "got: {err}");
    assert!(pusher.calls().is_empty(), "local tag never pushed on red");
    assert_eq!(git::remote_tag_sha(dir, "v0.1.6").unwrap(), None);
    drop(origin);
}

// ---- pure PR construction ---------------------------------------------------------

#[test]
fn branch_slug_matches_the_title_guard() {
    assert_eq!(branch_slug("add-thing"), "add-thing");
    assert_eq!(branch_slug("Add-thing"), "add-thing");
    assert_eq!(branch_slug("add_thing"), "add-thing");
    assert_eq!(branch_slug("add--thing"), "add-thing");
    assert_eq!(branch_slug("feat/x"), "feat-x");
    assert_eq!(branch_slug("v1.2"), "v1-2");
    assert_eq!(branch_slug("-edge-"), "edge");
}

#[test]
fn pr_title_takes_type_and_scope_from_first_subject_else_chore() {
    let subjects = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        pr_title("add-thing", &subjects(&["feat(core): add thing", "fix: later"])),
        "feat(core): add thing"
    );
    assert_eq!(pr_title("add-thing", &subjects(&["fix!: breaking"])), "fix: add thing");
    assert_eq!(pr_title("add-thing", &subjects(&["Add the thing"])), "chore: add thing");
    assert_eq!(pr_title("add-thing", &[]), "chore: add thing");
}

#[test]
fn pr_body_lists_subjects_and_ends_with_release_line() {
    let body = pr_body(&["feat: a".to_string(), "Bump version to v0.1.6".to_string()], "v0.1.6");
    assert_eq!(
        body,
        "- feat: a\n- Bump version to v0.1.6\n\nRelease: rides this PR (v0.1.6)"
    );
    assert!(pr_body(&[], "v1.0.0").ends_with("Release: rides this PR (v1.0.0)"));
}
