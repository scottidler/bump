//! Tests for `bump release` -- the UNGATED flow state machine (Phase 5).
//!
//! Real git in `TempDir`s against BARE local remotes (no network). `BUMP_GATES_PROBE`
//! forces the ungated verdict offline; env mutation is serialized behind the shared
//! `crate::ENV_LOCK` (env is process-global, so this must be the SAME lock the `main.rs`
//! gate tests use). Install is exercised through injected doubles -- no real
//! `cargo install` ever runs.

use super::*;
use crate::config::Config;
use crate::git;
use crate::github::CheckRuns;
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
    /// `(base, title, body)` of every PR created.
    created: RefCell<Vec<(String, String, String)>>,
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
    fn created(&self) -> Vec<(String, String, String)> {
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
    fn create_pr(&self, _dir: &Path, _branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        *self.create_calls.borrow_mut() += 1;
        self.created
            .borrow_mut()
            .push((base.to_string(), title.to_string(), body.to_string()));
        // An open PR now exists (models gh): the next probe returns true.
        *self.exists.borrow_mut() = true;
        Ok("https://example.test/pr/1".to_string())
    }
}

/// A `Ci` with no GitHub remote: `check_runs` answers `None`, so the CI gate has nothing
/// to wait on and passes immediately. The default for every test that is not about CI.
struct NoCi;

impl Ci for NoCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(None)
    }
}

/// A `Ci` whose one check completed red. The gate must refuse and leave NO tag.
struct RedCi;

impl Ci for RedCi {
    fn check_runs(&self, _dir: &Path, _sha: &str) -> Result<Option<CheckRuns>> {
        Ok(Some(CheckRuns {
            total: 1,
            incomplete: 0,
            failed: vec![("ci".to_string(), "https://example.test/run/1".to_string())],
        }))
    }
}

/// A `Ci` whose checks all completed green, recording every sha it was asked about.
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
        Ok(Some(CheckRuns {
            total: 2,
            incomplete: 0,
            failed: Vec::new(),
        }))
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

/// A bare `origin` on `main` and a clone whose Cargo.toml is at `version`, with a LOCAL
/// tag `v<version>` and `main` pushed (origin/HEAD set). HEAD == origin/main, ahead == 0.
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

fn auto_opts(bump_type: BumpType, dry_run: bool) -> ReleaseOpts {
    ReleaseOpts {
        bump_type,
        dry_run,
        install: InstallChoice::Auto,
        standalone: None,
        ci_gate: true,
        ci_timeout: std::time::Duration::from_secs(5),
    }
}

fn standalone_opts(bump_type: BumpType, words: &str) -> ReleaseOpts {
    ReleaseOpts {
        standalone: Some(words.to_string()),
        ..auto_opts(bump_type, false)
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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
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
        bump_type: BumpType::Patch,
        dry_run: false,
        install: InstallChoice::Command("touch install-marker".to_string()),
        standalone: None,
        ci_gate: true,
        ci_timeout: std::time::Duration::from_secs(5),
    };
    let prev = set_probe("ungated");
    let report = release(dir, &opts, &GitPusher, &ShellInstaller, &GhPr, &NoCi);
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
    let result = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
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
    let first = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
    // Second run: the remote now carries the tag, so there is nothing left to do.
    let second = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi);
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
    let report = release(dir, &auto_opts(BumpType::Patch, true), &pusher, &installer, &no_pr(), &NoCi);
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
        &auto_opts(BumpType::Patch, false),
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &pr, &NoCi)
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
    let first = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &pr, &NoCi);
    let second = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &pr, &NoCi);
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
    let err = release(dir, &auto_opts(BumpType::Major, false), &pusher, &installer, &pr, &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &pr, &NoCi)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &pr, &NoCi)
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
        ci_timeout: std::time::Duration::from_secs(5),
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
// 2026-09-26: one release command. CI gate before every tag, PR by construction,
// --standalone in place of the env-var door, finish from any worktree, no amend of a
// commit that is already on origin.
// ===================================================================================

/// The CI gate runs on the pushed sha BEFORE the tag exists: a red run refuses, the
/// version commit stays on origin untagged, and the re-run (green now) tags THAT version
/// instead of bumping past it. One intended release, one version number.
#[test]
fn red_ci_leaves_no_tag_and_green_rerun_resumes_same_version() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();

    let prev = set_probe("ungated");
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &RedCi)
        .expect_err("red CI must refuse")
        .to_string();
    assert!(err.contains("CI is RED"), "got: {err}");
    assert!(err.contains("NO TAG WAS CREATED"), "got: {err}");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no local tag after red CI");
    assert_eq!(git::remote_tag_sha(dir, "v0.1.6").unwrap(), None, "no remote tag after red CI");
    assert_eq!(read_cargo_version(dir), "0.1.6", "the version commit landed");
    assert_eq!(
        git_ok(dir, &["rev-parse", "origin/main"]),
        git::head_sha(dir).unwrap(),
        "the version commit is on origin, untagged"
    );
    assert_eq!(pusher.calls(), vec!["branch:main".to_string()], "no tag push attempted");

    let green = GreenCi::new();
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer, &no_pr(), &green)
        .expect("green re-run resumes");
    restore_probe(prev);
    assert!(report.resumed, "the re-run is a RESUME, not a fresh bump");
    assert_eq!(report.tag, "v0.1.6", "same version, never bumped past");
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(green.asked(), vec![git::head_sha(dir).unwrap()], "CI asked about the exact sha");
    assert!(git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some(), "tag pushed on the green run");
    drop(origin);
}

/// `--no-ci-gate` skips the wait: a red CI still tags (the caller re-armed the double-tap
/// on purpose, for repos with no CI).
#[test]
fn no_ci_gate_tags_without_waiting() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let opts = ReleaseOpts {
        ci_gate: false,
        ..auto_opts(BumpType::Patch, false)
    };
    let prev = set_probe("ungated");
    let report = release(dir, &opts, &RecordingPusher::new(false), &RecordingInstaller::new(), &no_pr(), &RedCi)
        .expect("no-ci-gate release must not consult CI");
    restore_probe(prev);
    assert_eq!(report.tag, "v0.1.6");
    assert!(git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some());
    drop(origin);
}

/// The gated PR is opened BY CONSTRUCTION: the title is the branch's words behind the first
/// commit's conventional type and scope (so it slugifies back to the branch), the base is
/// the default branch, and the body ends in the release-intent line Gate D looks for.
#[test]
fn gated_pr_title_and_body_are_built_from_branch_and_commits() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "add-thing"]);
    fs::write(dir.join("thing.txt"), "x").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "feat(core): add thing"]);

    let pr = RecordingPr::new();
    let prev = set_probe("gated:pull_request");
    let report = release(
        dir,
        &auto_opts(BumpType::Patch, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    )
    .expect("gated release pauses");
    restore_probe(prev);

    assert!(report.paused);
    assert_eq!(report.pr_url.as_deref(), Some("https://example.test/pr/1"));
    let created = pr.created();
    assert_eq!(created.len(), 1);
    let (base, title, body) = &created[0];
    assert_eq!(base, "main");
    assert_eq!(title, "feat(core): add thing");
    assert!(body.contains("- feat(core): add thing"), "body lists the commits: {body}");
    assert!(body.contains("Release: rides this PR (v0.1.6)"), "body carries the release line: {body}");
    assert!(!body.contains("Standalone"), "no standalone quote without an order: {body}");
    // The version commit is a NEW commit on top of the feature commit, never an amend.
    assert_eq!(git_ok(dir, &["log", "--format=%s", "-1", "HEAD~1"]), "feat(core): add thing");
    drop(origin);
}

/// A branch cut from origin/<default> with nothing on it is a bump-only branch in the
/// making: `bump release` refuses BEFORE any mutation, and the refusal names the door.
#[test]
fn gated_bump_only_branch_refuses_without_standalone() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "fresh-cut"]);

    let pr = RecordingPr::new();
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("gated:pull_request");
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &RecordingInstaller::new(), &pr, &NoCi)
        .expect_err("bump-only branch must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("bump-only release branch"), "got: {err}");
    assert!(err.contains("--standalone"), "the refusal names the door: {err}");
    assert_eq!(read_cargo_version(dir), "0.1.5", "nothing bumped");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_eq!(pr.create_calls(), 0, "no PR");
    drop(origin);
}

/// A hand-made bump-only branch (version lines only vs the default branch) is the same
/// refusal, regardless of its name.
#[test]
fn gated_version_only_diff_is_bump_only_regardless_of_name() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "innocent-name"]);
    write_cargo(dir, "0.1.6");
    git_ok(dir, &["commit", "-am", "Bump version to 0.1.6"]);
    let prev = set_probe("gated:pull_request");
    let err = release(
        dir,
        &auto_opts(BumpType::Patch, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &RecordingPr::new(),
        &NoCi,
    )
    .expect_err("version-only branch must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("bump-only release branch"), "got: {err}");
    drop(origin);
}

/// Scott's explicit order, on a GATED repo from a clean default branch: cut
/// `bump-vX-Y-Z` tracking origin/<default>, put the version bump on it as a NEW commit
/// (HEAD~1 is still origin/main, never an amend), push, open the PR quoting him, pause.
#[test]
fn gated_standalone_cuts_tracking_branch_bumps_and_quotes_scott() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let origin_main = git_ok(dir, &["rev-parse", "origin/main"]);

    let pr = RecordingPr::new();
    let pusher = RecordingPusher::new(false);
    let prev = set_probe("gated:pull_request");
    let report = release(
        dir,
        &standalone_opts(BumpType::Patch, "release them to prod with a .1 release"),
        &pusher,
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    )
    .expect("standalone gated release pauses");
    restore_probe(prev);

    assert!(report.paused);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(git::current_branch(dir).unwrap(), "bump-v0-1-6");
    assert_eq!(
        git_ok(dir, &["rev-parse", "--abbrev-ref", "@{u}"]),
        "origin/main",
        "the standalone branch tracks origin/main from birth"
    );
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(
        git_ok(dir, &["rev-parse", "HEAD~1"]),
        origin_main,
        "the bump is a NEW commit on top of origin/main, not an amend of it"
    );
    assert_eq!(pusher.calls(), vec!["feature:bump-v0-1-6".to_string()]);
    let (_, title, body) = &pr.created()[0];
    assert_eq!(title, "chore: bump v0 1 6");
    assert!(
        body.contains("Standalone release ordered by Scott: \"release them to prod with a .1 release\""),
        "Scott's words are quoted: {body}"
    );
    assert!(body.contains("Release: rides this PR (v0.1.6)"), "got: {body}");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no tag in the gated flow");
    drop(origin);
}

/// Scott's explicit order on an UNGATED repo whose default branch is fully tagged: the
/// version-only commit IS the release (bump, push, CI, tag, push tag).
#[test]
fn ungated_standalone_releases_from_tagged_default() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["push", "origin", "v0.1.5"]); // fully released, nothing ahead

    let prev = set_probe("ungated");
    let err = release(
        dir,
        &auto_opts(BumpType::Patch, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &NoCi,
    )
    .expect_err("without an order there is nothing to release")
    .to_string();
    assert!(err.contains("nothing ahead"), "got: {err}");
    assert!(err.contains("--standalone"), "the refusal names the door: {err}");

    let pusher = RecordingPusher::new(false);
    let report = release(
        dir,
        &standalone_opts(BumpType::Patch, "bump finish it"),
        &pusher,
        &RecordingInstaller::new(),
        &no_pr(),
        &NoCi,
    )
    .expect("standalone ungated release ships");
    restore_probe(prev);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(pusher.calls(), vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]);
    assert!(git::remote_tag_sha(dir, "v0.1.6").unwrap().is_some());
    drop(origin);
}

/// An empty `--standalone ""` is not an order.
#[test]
fn standalone_requires_words() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let err = release(
        work.path(),
        &standalone_opts(BumpType::Patch, "   "),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &no_pr(),
        &NoCi,
    )
    .expect_err("empty order must refuse")
    .to_string();
    assert!(err.contains("--standalone needs Scott's ordering words"), "got: {err}");
    drop(origin);
}

/// A branch whose name carries `/` or `.` can never match a conventional PR title, so the
/// gated flow refuses before bumping or pushing anything and prints the rename.
#[test]
fn gated_bad_branch_name_refuses_before_mutation() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "feat/add.thing"]);
    fs::write(dir.join("thing.txt"), "x").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "feat: add thing"]);

    let pusher = RecordingPusher::new(false);
    let prev = set_probe("gated:pull_request");
    let err = release(
        dir,
        &auto_opts(BumpType::Patch, false),
        &pusher,
        &RecordingInstaller::new(),
        &RecordingPr::new(),
        &NoCi,
    )
    .expect_err("bad branch name must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("git branch -m feat-add-thing"), "got: {err}");
    assert_eq!(read_cargo_version(dir), "0.1.5");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

/// `is_head_pushed` on a branch just cut from origin/main with NO upstream: HEAD is on
/// origin, so it is pushed, and `bump --no-tag` must create a commit rather than amend one
/// that origin already has (the 2026-09-26 marquee divergence).
#[test]
fn head_cut_from_origin_without_upstream_counts_as_pushed() {
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "no-upstream", "--no-track"]);
    assert!(
        git::is_head_pushed(dir).unwrap(),
        "HEAD == origin/main is pushed even with no upstream configured"
    );
    fs::write(dir.join("f.txt"), "x").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "local only"]);
    assert!(!git::is_head_pushed(dir).unwrap(), "a commit on no remote ref is unpushed");
    drop(origin);
}

/// `bump finish` run from a FEATURE-BRANCH WORKTREE: the default branch is checked out in
/// a sibling worktree, so finish fast-forwards and tags THERE instead of failing on
/// `git checkout main` ("already checked out").
#[test]
fn finish_from_feature_worktree_finishes_in_the_default_worktree() {
    let (origin, work) = setup_released("0.1.5");
    let main_wt = work.path();
    let base = git_ok(main_wt, &["rev-parse", "HEAD"]);
    // The merged bump lands on origin/main; local main is rewound behind it.
    write_cargo(main_wt, "0.1.6");
    git_ok(main_wt, &["commit", "-am", "Bump version to 0.1.6"]);
    git_ok(main_wt, &["push", "origin", "main"]);
    git_ok(main_wt, &["branch", "feature"]);
    git_ok(main_wt, &["reset", "--hard", &base]);
    // A sibling worktree holds the feature branch; main stays checked out here.
    let feature_wt = TempDir::new().unwrap();
    let feature_path = feature_wt.path().join("feature");
    git_ok(
        main_wt,
        &["worktree", "add", feature_path.to_str().unwrap(), "feature"],
    );

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(&feature_path, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect("finish from the feature worktree must succeed");

    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    assert_eq!(read_cargo_version(main_wt), "0.1.6", "main worktree fast-forwarded");
    assert_eq!(git::current_branch(&feature_path).unwrap(), "feature", "feature worktree untouched");
    assert_eq!(
        git::tag_sha(main_wt, "v0.1.6").unwrap(),
        git_ok(main_wt, &["rev-parse", "origin/main"]),
        "tag on the merged tip"
    );
    assert!(git::remote_tag_sha(main_wt, "v0.1.6").unwrap().is_some());
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    git_ok(main_wt, &["worktree", "remove", "--force", feature_path.to_str().unwrap()]);
    drop(origin);
}

/// finish waits for CI on the merged commit too: red refuses with no tag.
#[test]
fn finish_red_ci_leaves_no_tag() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let err = finish(dir, &finish_opts(false), &pusher, &RecordingInstaller::new(), &RedCi)
        .expect_err("red CI must refuse")
        .to_string();
    assert!(err.contains("CI is RED"), "got: {err}");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no tag after red CI");
    assert!(pusher.calls().is_empty());
    drop(origin);
}

#[test]
fn pr_title_derives_type_and_scope_from_first_subject() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(pr_title("add-thing", &s(&["feat(core): add thing"])), "feat(core): add thing");
    assert_eq!(pr_title("add-thing", &s(&["fix: add thing", "chore: x"])), "fix: add thing");
    assert_eq!(pr_title("add-thing", &s(&["refactor!: drop it"])), "refactor: add thing");
    assert_eq!(pr_title("add-thing", &s(&["Add the thing"])), "chore: add thing");
    assert_eq!(pr_title("bump-v1-2-3", &[]), "chore: bump v1 2 3");
    assert_eq!(pr_title("x", &s(&["Feat(A): shouty"])), "chore: x");
}

#[test]
fn pr_body_lists_subjects_quotes_order_and_ends_with_release_line() {
    let subjects = vec!["feat: a".to_string(), "fix: b".to_string()];
    let body = pr_body(&subjects, "v1.2.3", None);
    assert_eq!(body, "- feat: a\n- fix: b\n\nRelease: rides this PR (v1.2.3)\n");
    let body = pr_body(&[], "v1.2.3", Some("  bump finish it "));
    assert_eq!(
        body,
        "- version bump\n\nStandalone release ordered by Scott: \"bump finish it\"\n\nRelease: rides this PR (v1.2.3)\n"
    );
}

#[test]
fn standalone_branch_name_has_no_dots() {
    assert_eq!(standalone_branch_name("v1.21.0"), "bump-v1-21-0");
}
