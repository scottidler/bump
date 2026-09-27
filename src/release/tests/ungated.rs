//! Ungated flow: fresh release ordering, rejected push, RESUME, dry run, and each refusal.

use super::*;

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
    assert!(err.contains(STANDALONE_DOOR), "must name the standalone door: {err}");
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
    assert!(err.contains(STANDALONE_DOOR), "must name the standalone door: {err}");
    assert!(pusher.calls().is_empty());
    assert_eq!(pr.create_calls(), 0, "no PR touched on a refusal");
    // NO tag created on this gated path either.
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap());
    drop(origin);
}

/// Diverged is not behind: a fast-forward cannot apply, so the refusal names the rebase.
#[test]
fn refuses_when_diverged_from_origin_naming_rebase() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let c1 = git::head_sha(dir).unwrap();
    git_ok(dir, &["commit", "--allow-empty", "-m", "someone else pushed first"]);
    git_ok(dir, &["push", "origin", "main"]);
    git_ok(dir, &["reset", "--hard", &c1]);
    git_ok(dir, &["commit", "--allow-empty", "-m", "local work"]);
    let head = git::head_sha(dir).unwrap();

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
    .expect_err("diverged must refuse")
    .to_string();
    restore_probe(prev);
    assert!(err.contains("git pull --rebase origin main"), "got: {err}");
    assert!(!err.contains("--ff-only"), "a fast-forward cannot apply: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), head, "no version commit");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);
}

/// main carries an UNPUSHED commit setting 0.1.6 over tag v0.1.5 (a design doc's Phase 1
/// bumped it). `-m` refuses naming both versions and touches nothing; a bare run pushes,
/// tags v0.1.6, and never re-bumps.
#[test]
fn ungated_pending_version_is_pushed_and_tagged_not_rebumped() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    write_cargo(dir, "0.1.6");
    git_ok(dir, &["commit", "-am", "Bump version to v0.1.6"]);
    let head = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let minor = release(
        dir,
        &auto_opts(Some(BumpType::Minor), false),
        &pusher,
        &installer,
        &no_pr(),
        &NoCi,
    );
    let minor_pushes = pusher.calls();
    let bare = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let err = minor.expect_err("-m implies a different version").to_string();
    assert!(err.contains("v0.1.6") && err.contains("v0.2.0"), "names both: {err}");
    assert!(minor_pushes.is_empty(), "the refusal pushed nothing");

    let report = bare.expect("bare run ships the pending version");
    assert_eq!(report.tag, "v0.1.6");
    assert!(!report.resumed, "ahead of origin is not a resume");
    assert_eq!(git::head_sha(dir).unwrap(), head, "no version commit");
    assert_eq!(read_cargo_version(dir), "0.1.6", "never re-bumped");
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    assert_eq!(git::remote_tag_commit(dir, "v0.1.6").unwrap(), Some(head));
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);
}

/// An UNGATED generic (no-manifest) repo: tag `v0.1.5` on origin, plus one unpushed work
/// commit, so HEAD is ahead of origin/main. The version lives in tags alone.
fn setup_generic_ungated_ahead() -> (TempDir, TempDir) {
    let origin = TempDir::new().unwrap();
    git_ok(origin.path(), &["init", "--bare", "-b", "main"]);
    let work = TempDir::new().unwrap();
    let w = work.path();
    git_ok(w, &["init", "-b", "main"]);
    git_ok(w, &["config", "user.email", "test@test.com"]);
    git_ok(w, &["config", "user.name", "Test"]);
    fs::write(w.join("a.txt"), "a").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "init"]);
    git_ok(w, &["tag", "-a", "v0.1.5", "-m", "v0.1.5"]);
    git_ok(w, &["remote", "add", "origin", origin.path().to_str().unwrap()]);
    git_ok(w, &["push", "-u", "origin", "main"]);
    git_ok(w, &["push", "origin", "v0.1.5"]);
    git_ok(w, &["remote", "set-head", "origin", "main"]);
    fs::write(w.join("b.txt"), "b").unwrap();
    git_ok(w, &["add", "-A"]);
    git_ok(w, &["commit", "-m", "fix: work"]);
    (origin, work)
}

/// Audit round 1, must-fix 1: v0.3.3 tagged an ungated generic repo; the CI gate's
/// manifest-version check must not refuse a repo that has no manifest.
#[test]
fn ungated_generic_release_tags_and_pushes() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_generic_ungated_ahead();
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let report = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let report = report.expect("ungated generic release must tag");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    assert_eq!(git::remote_tag_commit(dir, "v0.1.6").unwrap(), Some(head));
    drop(origin);
}

/// Audit round 1, must-fix 1: a red CI after the branch push leaves origin/main untagged;
/// the re-run is the RESUME row (tags the same version on the same sha), never "nothing to
/// release". A third run, now tagged, is "nothing to release".
#[test]
fn ungated_generic_red_ci_rerun_resumes() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_generic_ungated_ahead();
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let prev = set_probe("ungated");
    let red = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &RedCi);
    let green = GreenCi::new();
    let resumed = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &green);
    let again = release(dir, &auto_opts(None, false), &pusher, &installer, &no_pr(), &NoCi);
    restore_probe(prev);

    let err = red.expect_err("red CI must refuse").to_string();
    assert!(err.contains("RED"), "got: {err}");
    let report = resumed.expect("the re-run after red CI must resume");
    assert!(report.resumed, "the re-run is the RESUME row");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(green.asked(), vec![head.clone()]);
    assert_eq!(git::remote_tag_commit(dir, "v0.1.6").unwrap(), Some(head));
    let err = again.expect_err("a tagged tip has nothing to release").to_string();
    assert!(err.contains("nothing to release"), "got: {err}");
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);
}
