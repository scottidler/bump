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

/// Phase 5 criterion, row 3: the tag is on the remote at the merged tip -> "already
/// released", and the install step STILL runs, so a re-run after "tag pushed, install
/// failed" installs. Two runs install twice and never push; `--no-install` skips it.
#[test]
fn finish_already_released_still_installs() {
    let (origin, work) = setup_finish_fully_released("0.1.5", "0.1.6");
    let dir = work.path();

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let first = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("first finish reports released");
    let second = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("second finish too");

    for report in [first, second] {
        assert_eq!(report.tag, "v0.1.6");
        assert!(!report.resumed, "already-released is NOT a resume");
        assert_eq!(report.install_command.as_deref(), Some("cargo install --path ."));
    }
    assert!(pusher.calls().is_empty(), "already released never pushes a tag");
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string(); 2]);

    let skip = FinishOpts {
        install: InstallChoice::Skip,
        ..finish_opts(false)
    };
    let skipped = finish(dir, &skip, &pusher, &installer, &NoCi).expect("--no-install run");
    assert!(skipped.install_command.is_none(), "--no-install skips the install");
    assert_eq!(installer.calls().len(), 2, "no third install under --no-install");
    assert!(pusher.calls().is_empty());
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

// ---- Phase 5: finish from any worktree -------------------------------------------

/// A main worktree plus a feature worktree beside it (`git worktree add`), the herdr
/// layout. origin/main carries the untagged merged bump to `to`; the main worktree's `main`
/// was never pulled since the merge (BEHIND); the feature worktree sits on `feature` at the
/// merged bump with an untracked scratch file.
struct Worktrees {
    origin: TempDir,
    main: TempDir,
    feature: TempDir,
}

fn setup_finish_worktrees(from: &str, to: &str) -> Worktrees {
    let (origin, main) = setup_released(from);
    let m = main.path();
    let base = git_ok(m, &["rev-parse", "HEAD"]);
    write_cargo(m, to);
    git_ok(m, &["commit", "-am", &format!("Bump version to {to}")]);
    git_ok(m, &["push", "origin", "main"]);
    git_ok(m, &["branch", "feature"]);
    git_ok(m, &["reset", "--hard", &base]);
    let feature = add_feature_worktree(m);
    Worktrees { origin, main, feature }
}

/// `git worktree add <tmp> feature` (the branch must exist) plus an untracked scratch file.
fn add_feature_worktree(main: &Path) -> TempDir {
    let feature = TempDir::new().unwrap();
    fs::remove_dir(feature.path()).unwrap(); // `git worktree add` creates it fresh
    git_ok(main, &["worktree", "add", feature.path().to_str().unwrap(), "feature"]);
    fs::write(feature.path().join("scratch.txt"), "wip").unwrap();
    feature
}

/// Everything about a worktree finish must not touch: branch, HEAD, full status (untracked
/// included), and the manifest on disk.
fn snapshot(dir: &Path) -> (String, String, String, String) {
    (
        git::current_branch(dir).unwrap(),
        git::head_sha(dir).unwrap(),
        git_ok(dir, &["status", "--porcelain"]),
        fs::read_to_string(dir.join("Cargo.toml")).unwrap(),
    )
}

fn same_path(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a).unwrap() == fs::canonicalize(b).unwrap()
}

/// Phase 5 criterion: run finish from a feature worktree. The main worktree fast-forwards
/// to `0.1.6`, the tag lands on origin/main's tip and is pushed, install runs in the main
/// worktree, and the feature worktree is untouched.
#[test]
fn finish_from_feature_worktree_finishes_in_the_default_worktree() {
    let wt = setup_finish_worktrees("0.1.5", "0.1.6");
    let (main, feature) = (wt.main.path(), wt.feature.path());
    let feature_before = snapshot(feature);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(feature, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect("finish from a feature worktree must release in the main one");

    assert_eq!(report.tag, "v0.1.6");
    assert!(!report.resumed);
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    assert_eq!(read_cargo_version(main), "0.1.6", "main worktree fast-forwarded");
    assert_eq!(git::current_branch(main).unwrap(), "main");
    let tip = git_ok(main, &["rev-parse", "origin/main"]);
    assert_eq!(git::head_sha(main).unwrap(), tip, "main worktree at origin/main");
    assert_eq!(git::tag_sha(main, "v0.1.6").unwrap(), tip, "tag on origin/main's tip");
    assert_eq!(
        git::remote_tag_commit(main, "v0.1.6").unwrap().as_deref(),
        Some(tip.as_str()),
        "tag pushed, pointing at origin/main's tip"
    );
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    assert!(
        same_path(&installer.dirs()[0], main),
        "install ran in the main worktree"
    );
    assert_eq!(snapshot(feature), feature_before, "feature worktree untouched");
    drop(wt.origin);
}

/// Phase 5 criterion: red CI on the merged sha leaves NO tag, local or remote, pushes
/// nothing and installs nothing; the refusal says the re-run reuses the version.
#[test]
fn finish_red_ci_leaves_no_tag() {
    let wt = setup_finish_worktrees("0.1.5", "0.1.6");
    let (main, feature) = (wt.main.path(), wt.feature.path());
    let feature_before = snapshot(feature);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(feature, &finish_opts(false), &pusher, &installer, &RedCi)
        .expect_err("red CI must refuse")
        .to_string();

    assert!(err.contains("CI is RED"), "got: {err}");
    assert!(err.contains("re-run bump finish"), "got: {err}");
    assert!(err.contains("reuses v0.1.6"), "got: {err}");
    assert_no_tag_anywhere(main, "v0.1.6");
    assert!(pusher.calls().is_empty(), "no tag push on red CI");
    assert!(installer.calls().is_empty(), "no install on red CI");
    assert_eq!(snapshot(feature), feature_before, "feature worktree untouched");
    drop(wt.origin);
}

/// Phase 5 criterion: the main worktree's `main` has a local commit origin lacks AND origin
/// has moved. finish refuses naming `git pull --rebase` before any pull (no git
/// fast-forward error in the message), and both worktrees are untouched.
#[test]
fn finish_diverged_default_refuses_before_pull() {
    let wt = setup_finish_worktrees("0.1.5", "0.1.6");
    let (main, feature) = (wt.main.path(), wt.feature.path());
    git_ok(main, &["commit", "--allow-empty", "-m", "local only"]);
    let main_before = snapshot(main);
    let feature_before = snapshot(feature);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(feature, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("a diverged default must refuse")
        .to_string();

    assert!(err.contains("has diverged from origin/main"), "got: {err}");
    assert!(err.contains("git pull --rebase origin main"), "got: {err}");
    assert!(
        err.contains(&format!("cd {}", main.display())),
        "names the worktree: {err}"
    );
    assert!(
        !err.contains("Not possible to fast-forward"),
        "refused before the pull: {err}"
    );
    assert_eq!(snapshot(main), main_before, "main worktree untouched");
    assert_eq!(snapshot(feature), feature_before, "feature worktree untouched");
    assert_no_tag_anywhere(main, "v0.1.6");
    assert!(pusher.calls().is_empty());
    assert!(installer.calls().is_empty());
    drop(wt.origin);
}

/// The main worktree's `main` carries a commit origin lacks, and origin has not moved:
/// ahead refuses with the literal rescue (branch it, reset to origin) before any pull.
#[test]
fn finish_ahead_default_refuses_before_pull() {
    let (origin, main_wt) = setup_released("0.1.5");
    let main = main_wt.path();
    git_ok(main, &["branch", "feature"]);
    git_ok(main, &["commit", "--allow-empty", "-m", "never landed"]);
    let feature_wt = add_feature_worktree(main);
    let feature = feature_wt.path();
    let main_before = snapshot(main);
    let feature_before = snapshot(feature);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(feature, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("an ahead default must refuse")
        .to_string();

    assert!(err.contains("NOT on origin/main"), "got: {err}");
    assert!(err.contains("git branch stranded-"), "got: {err}");
    assert!(err.contains("git reset --hard origin/main"), "got: {err}");
    assert!(
        err.contains(&format!("cd {}", main.display())),
        "names the worktree: {err}"
    );
    assert_eq!(snapshot(main), main_before, "main worktree untouched");
    assert_eq!(snapshot(feature), feature_before, "feature worktree untouched");
    assert!(pusher.calls().is_empty());
    assert!(installer.calls().is_empty());
    drop(origin);
}

/// Tracked changes in the RESOLVED worktree (not the current one) refuse before anything
/// moves, naming that worktree; both worktrees untouched.
#[test]
fn finish_refuses_tracked_change_in_the_resolved_worktree() {
    let wt = setup_finish_worktrees("0.1.5", "0.1.6");
    let (main, feature) = (wt.main.path(), wt.feature.path());
    let cargo_toml = main.join("Cargo.toml");
    let contents = fs::read_to_string(&cargo_toml).unwrap();
    fs::write(&cargo_toml, format!("{contents}\n# tracked edit\n")).unwrap();
    let main_before = snapshot(main);
    let feature_before = snapshot(feature);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let err = finish(feature, &finish_opts(false), &pusher, &installer, &NoCi)
        .expect_err("a dirty main worktree must refuse")
        .to_string();

    assert!(err.contains("uncommitted tracked changes"), "got: {err}");
    assert!(err.contains(&main.display().to_string()), "names the worktree: {err}");
    assert_eq!(snapshot(main), main_before, "main worktree untouched");
    assert_eq!(snapshot(feature), feature_before, "feature worktree untouched");
    assert!(pusher.calls().is_empty());
    drop(wt.origin);
}

/// No local default branch anywhere (a clone that only ever had the feature branch): the
/// checkout creates `main` from origin, and finish releases the merged tip.
#[test]
fn finish_creates_a_missing_default_branch_from_origin() {
    let (origin, work) = setup_finish_untagged_merged("0.1.5", "0.1.6");
    let dir = work.path();
    git_ok(dir, &["branch", "-D", "main"]);

    let pusher = RecordingPusher::new(false);
    let installer = RecordingInstaller::new();
    let report = finish(dir, &finish_opts(false), &pusher, &installer, &NoCi).expect("finish must create main");

    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(git::current_branch(dir).unwrap(), "main");
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    drop(origin);
}
