//! The CI gate, the pending version, and tag placement on the verified sha.

use super::*;
use crate::release::ci::wait_for_green;

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

/// End to end through `release`: tracked workflows + zero runs refuses naming `bump.yml`;
/// an untracked (ignored, so the tree stays clean) `bump.yml` with `ci: none` still
/// refuses; the same line COMMITTED proceeds and the tag is pushed.
#[test]
fn zero_check_runs_refuses_with_workflows_and_proceeds_with_ci_none() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    fs::write(dir.join(".github/workflows/ci.yml"), "on: workflow_dispatch\n").unwrap();
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "manual-only ci"]);
    let pusher = RecordingPusher::new(false);
    let run = |dir: &Path| {
        release(
            dir,
            &auto_opts(None, false),
            &pusher,
            &RecordingInstaller::new(),
            &no_pr(),
            &SilentCi,
        )
    };
    let prev = set_probe("ungated");

    let err = run(dir).expect_err("workflows + zero runs must refuse").to_string();
    assert!(err.contains("bump.yml"), "names bump.yml: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert_eq!(
        read_cargo_version(dir),
        "0.1.6",
        "the version commit rode to origin untagged"
    );

    fs::write(dir.join("bump.yml"), "ci: none\n").unwrap();
    fs::write(dir.join(".git/info/exclude"), "bump.yml\n").unwrap();
    let err = run(dir)
        .expect_err("an untracked ci: none must not switch the gate off")
        .to_string();
    assert!(err.contains("bump.yml"), "the same CI refusal, not a dirty tree: {err}");
    assert_no_tag_anywhere(dir, "v0.1.6");

    git_ok(dir, &["add", "-f", "bump.yml"]);
    git_ok(dir, &["commit", "-m", "declare ci: none"]);
    let report = run(dir).expect("a committed ci: none proceeds");
    restore_probe(prev);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(
        git::remote_tag_commit(dir, "v0.1.6").unwrap(),
        Some(git::head_sha(dir).unwrap()),
        "tagged at the commit that declares ci: none"
    );
    drop(origin);
}
