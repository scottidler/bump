//! `bump finish`: tag the merged tip, missed bump, local-tag resume, no-op, refusals, dry run.

use super::*;

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
/// finish refuses naming the standalone door (and forbidding an invented order); nothing
/// tagged or installed.
#[test]
fn finish_missed_bump_refuses_naming_the_standalone_door() {
    let (origin, work) = setup_finish_missed_bump("0.1.5");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("missed bump must refuse")
        .to_string();
    assert!(err.contains("no untagged version"), "got: {err}");
    assert!(
        err.contains("--standalone \"<his exact words>\""),
        "must name the door: {err}"
    );
    assert!(err.contains("do not invent an order"), "got: {err}");
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

/// `tag_ladder` (shared by `--tag-only` and `finish`) splits behind from diverged: only
/// behind can fast-forward; diverged names the rebase.
#[test]
fn tag_ladder_splits_behind_and_diverged() {
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let c1 = git::head_sha(dir).unwrap();
    git_ok(dir, &["commit", "--allow-empty", "-m", "c2"]);
    git_ok(dir, &["push", "origin", "main"]);
    git_ok(dir, &["reset", "--hard", &c1]);

    let behind = crate::tag_ladder(dir).expect_err("behind refuses").to_string();
    assert!(behind.contains("git pull --ff-only origin main"), "got: {behind}");

    git_ok(dir, &["commit", "--allow-empty", "-m", "local only"]);
    let diverged = crate::tag_ladder(dir).expect_err("diverged refuses").to_string();
    assert!(diverged.contains("git pull --rebase origin main"), "got: {diverged}");
    assert!(!diverged.contains("--ff-only"), "got: {diverged}");
    assert_eq!(git::tag_sha(dir, "v0.1.5").unwrap(), c1, "no tag moved or created");
    assert!(!git::tag_exists(dir, "v0.1.6").unwrap());
    drop(origin);
}
