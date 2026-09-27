//! PR construction: title and body built from the branch and its commits, the slug precondition, the inherited-pending bump.

use super::*;

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
    let body = pr_body(
        &["feat: a".to_string(), "Bump version to v0.1.6".to_string()],
        "v0.1.6",
        None,
    );
    assert_eq!(
        body,
        "- feat: a\n- Bump version to v0.1.6\n\nRelease: rides this PR (v0.1.6)"
    );
    assert!(pr_body(&[], "v1.0.0", None).ends_with("Release: rides this PR (v1.0.0)"));
}

#[test]
fn pr_body_quotes_the_standalone_order_before_the_release_line() {
    let body = pr_body(&["Bump version to v0.1.6".to_string()], "v0.1.6", Some("ship it"));
    assert_eq!(
        body,
        "- Bump version to v0.1.6\n\nStandalone release ordered by Scott: \"ship it\"\n\nRelease: rides this PR (v0.1.6)"
    );
}
