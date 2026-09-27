//! Gated flow: pause + PR once, level mismatch, stranded commits, gated generic.

use super::*;

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
