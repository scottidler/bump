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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
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
    };
    let prev = set_probe("ungated");
    let report = release(dir, &opts, &GitPusher, &ShellInstaller);
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
    let result = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
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
    let report = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
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
    let first = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
    // Second run: the remote now carries the tag, so there is nothing left to do.
    let second = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer);
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
    let report = release(dir, &auto_opts(BumpType::Patch, true), &pusher, &installer);
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
    let err = release(tmp.path(), &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
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
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
        .expect_err("detached HEAD must refuse")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("detached"), "got: {err}");
    drop(origin);
}

/// A gated verdict refuses (the gated flow is a later phase).
#[test]
fn refuses_when_gated_this_phase() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("gated:pull_request");
    let err = release(dir, &auto_opts(BumpType::Patch, false), &pusher, &installer)
        .expect_err("gated must refuse this phase")
        .to_string();
    restore_probe(prev);
    assert!(err.contains("GATED"), "got: {err}");
    assert!(pusher.calls().is_empty());
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
