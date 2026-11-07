//! The PR hand-off: bump never opens a PR, it probes and hands `/pr:create` the release-intent line; the slug precondition; the inherited-pending bump.

use super::*;

/// A gated release with no open PR touches the PR seam ONLY through the open-PR probe and
/// reports the exact lines the `/pr:create` body must carry. A PR opened inside bump is
/// invisible to the PreToolUse guards on `/pr:create` (tatari-tv/drata-cli#18), so this
/// pins that bump never opens one. Break-the-code proof (design doc, Testing Strategy):
/// remove the `never_amend` short-circuit and the `HEAD~1` assertion fails, because the
/// bump amends the feature commit.
#[test]
fn gated_release_never_opens_a_pr_and_hands_off_the_release_line() {
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
    assert_eq!(
        pr.calls(),
        vec!["list:add-thing".to_string()],
        "only the open-PR probe; bump never opens a PR"
    );
    assert_eq!(
        report.pr_body_lines,
        vec!["Release: rides this PR (v0.1.6)".to_string()]
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
    assert_eq!(pr.calls(), vec!["list:more-work".to_string()]);
    assert_eq!(
        report.pr_body_lines,
        vec!["Release: rides this PR (v0.1.7)".to_string()]
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
    assert!(pr.calls().is_empty(), "no PR touched");
    drop(origin);
}

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
fn pr_body_lines_end_with_the_release_line() {
    assert_eq!(
        pr_body_lines("v0.1.6", None),
        vec!["Release: rides this PR (v0.1.6)".to_string()]
    );
    assert_eq!(
        pr_body_lines("v0.1.6", Some("ship it")),
        vec![
            "Standalone release ordered by Scott: \"ship it\"".to_string(),
            "Release: rides this PR (v0.1.6)".to_string(),
        ]
    );
}

#[test]
fn pr_create_handoff_names_pr_create_and_prints_each_line_bare() {
    let one = pr_create_handoff("add-thing", &pr_body_lines("v0.1.6", None));
    assert_eq!(
        one,
        "no open PR for add-thing; bump does not open PRs.\n\
         next: open the PR with /pr:create; its body must carry this line verbatim:\n\
         Release: rides this PR (v0.1.6)"
    );
    let two = pr_create_handoff("bump-v0-1-6", &pr_body_lines("v0.1.6", Some("ship it")));
    assert!(two.contains("carry these lines verbatim:"), "{two}");
    let last_two: Vec<&str> = two.lines().rev().take(2).collect();
    assert_eq!(
        last_two,
        vec![
            "Release: rides this PR (v0.1.6)",
            "Standalone release ordered by Scott: \"ship it\""
        ]
    );
}

/// Audit round 1, cheap-win 5: a gated run carrying Scott's standalone order on a branch
/// whose PR is already open never writes a body, so the order goes onto that PR as a
/// comment, verbatim, and no hand-off is printed. Without an order, nothing is commented.
#[test]
fn standalone_order_is_commented_on_an_already_open_pr() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_gated_feature_branch("0.1.5");
    let dir = work.path();
    let pr = RecordingPr::already_open();
    let bare_pr = RecordingPr::already_open();
    let prev = set_probe("gated:pull_request");
    let ordered = release(
        dir,
        &standalone_opts(None, "ship it with the fix"),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &pr,
        &NoCi,
    );
    let bare = release(
        dir,
        &auto_opts(None, false),
        &RecordingPusher::new(false),
        &RecordingInstaller::new(),
        &bare_pr,
        &NoCi,
    );
    restore_probe(prev);

    let ordered = ordered.expect("the ordered run pauses");
    assert!(ordered.paused);
    assert!(
        ordered.pr_body_lines.is_empty(),
        "an open PR needs no /pr:create hand-off"
    );
    assert_eq!(
        pr.calls(),
        vec!["list:feature".to_string(), "comment:feature".to_string()]
    );
    assert_eq!(
        pr.comments(),
        vec![(
            "feature".to_string(),
            "Standalone release ordered by Scott: \"ship it with the fix\"".to_string()
        )]
    );
    assert!(bare.expect("the bare re-run pauses").paused);
    assert!(bare_pr.comments().is_empty(), "no order, no comment");
    drop(origin);
}
