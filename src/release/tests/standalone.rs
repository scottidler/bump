//! Scott's standalone order and the bump-only branch refusal it is the exception to.

use super::*;

const ORDER: &str = "release them to prod with a .1 release";

/// Records the branch's `@{u}` at the moment the feature push starts, then pushes for real.
struct UpstreamPusher {
    upstream_at_push: RefCell<Vec<String>>,
    calls: RefCell<Vec<String>>,
}

impl UpstreamPusher {
    fn new() -> Self {
        Self {
            upstream_at_push: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Pusher for UpstreamPusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("branch:{branch}"));
        git::push_branch(dir, branch)
    }
    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("tag:{tag}"));
        git::push_tag(dir, tag)
    }
    fn push_feature_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        self.calls.borrow_mut().push(format!("feature:{branch}"));
        self.upstream_at_push.borrow_mut().push(upstream(dir));
        git::push_feature_branch(dir, branch)
    }
}

fn upstream(dir: &Path) -> String {
    git_ok(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
}

fn gated_release<P: Pusher>(dir: &Path, opts: &ReleaseOpts, pusher: &P, pr: &RecordingPr) -> Result<ReleaseReport> {
    let prev = set_probe("gated:pull_request");
    let result = release(dir, opts, pusher, &RecordingInstaller::new(), pr, &NoCi);
    restore_probe(prev);
    result
}

fn ungated_release<P: Pusher>(dir: &Path, opts: &ReleaseOpts, pusher: &P) -> Result<ReleaseReport> {
    let prev = set_probe("ungated");
    let result = release(dir, opts, pusher, &RecordingInstaller::new(), &no_pr(), &NoCi);
    restore_probe(prev);
    result
}

#[test]
fn gated_standalone_cuts_tracking_branch_bumps_and_quotes_scott() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5"); // the tip carries v0.1.5
    let dir = work.path();
    let tagged_tip = git::head_sha(dir).unwrap();

    let pusher = UpstreamPusher::new();
    let pr = RecordingPr::new();
    let report = gated_release(dir, &standalone_opts(None, ORDER), &pusher, &pr).expect("standalone pauses on its PR");

    assert!(report.paused);
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(git::current_branch(dir).unwrap(), "bump-v0-1-6");
    assert_eq!(
        *pusher.upstream_at_push.borrow(),
        vec!["origin/main".to_string()],
        "cut with --track origin/main"
    );
    assert_eq!(upstream(dir), "origin/bump-v0-1-6", "the push set the upstream");
    assert_eq!(
        git_ok(dir, &["rev-parse", "HEAD~1"]),
        git_ok(dir, &["rev-parse", "origin/main"])
    );
    assert_eq!(git_ok(dir, &["rev-parse", "origin/main"]), tagged_tip, "main untouched");
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(
        git_ok(dir, &["rev-parse", "origin/bump-v0-1-6"]),
        git::head_sha(dir).unwrap(),
        "the bump commit is on origin's branch"
    );
    let created = pr.created();
    assert_eq!(created.len(), 1);
    let (branch, base, title, body) = &created[0];
    assert_eq!(branch, "bump-v0-1-6");
    assert_eq!(base, "main");
    assert_eq!(title, "chore: bump v0 1 6");
    assert!(
        body.contains(&format!("Standalone release ordered by Scott: \"{ORDER}\"")),
        "body: {body}"
    );
    assert_eq!(body.lines().last(), Some("Release: rides this PR (v0.1.6)"));
    assert_eq!(*pusher.calls.borrow(), vec!["feature:bump-v0-1-6".to_string()]);
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);
}

/// A `bump-vX-Y-Z` left by a prior run is checked out and classified by its own diff:
/// empty -> the fresh standalone bump; its own version line -> no second bump; work
/// commits -> a feature branch the bump rides with. Scott's words are quoted each time.
#[test]
fn gated_standalone_reuses_an_existing_branch_by_its_diff() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    for case in ["empty", "bumped", "work"] {
        let (origin, work) = setup_released("0.1.5");
        let dir = work.path();
        git_ok(dir, &["branch", "--no-track", "bump-v0-1-6", "main"]);
        git_ok(dir, &["checkout", "bump-v0-1-6"]);
        match case {
            "bumped" => {
                write_cargo(dir, "0.1.6");
                git_ok(dir, &["commit", "-am", "Bump version to v0.1.6"]);
            }
            "work" => {
                fs::write(dir.join("fix.txt"), "fix").unwrap();
                git_ok(dir, &["add", "-A"]);
                git_ok(dir, &["commit", "-m", "fix: the thing"]);
            }
            _ => {}
        }
        let before = git::head_sha(dir).unwrap();
        git_ok(dir, &["checkout", "main"]);

        let pr = RecordingPr::new();
        let pusher = RecordingPusher::new(false);
        let report = gated_release(dir, &standalone_opts(None, ORDER), &pusher, &pr)
            .unwrap_or_else(|e| panic!("{case}: existing branch must proceed: {e}"));

        assert_eq!(report.tag, "v0.1.6", "{case}");
        assert_eq!(git::current_branch(dir).unwrap(), "bump-v0-1-6", "{case}");
        assert_eq!(read_cargo_version(dir), "0.1.6", "{case}");
        let head = git::head_sha(dir).unwrap();
        if case == "bumped" {
            assert_eq!(head, before, "{case}: the branch's own bump is not bumped again");
        } else {
            assert_eq!(
                git_ok(dir, &["rev-parse", "HEAD~1"]),
                before,
                "{case}: one new version commit"
            );
        }
        assert_eq!(pusher.calls(), vec!["feature:bump-v0-1-6".to_string()], "{case}");
        let body = &pr.created()[0].3;
        assert!(body.contains(ORDER), "{case}: {body}");
        assert_no_tag_anywhere(dir, "v0.1.6");
        drop(origin);
    }
}

#[test]
fn gated_bump_only_branch_refuses_without_standalone() {
    let _guard = crate::ENV_LOCK.lock().unwrap();

    // A fresh-cut branch: an empty diff is bump-only here (the verb has no Gate A).
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "release-it"]);
    let head = git::head_sha(dir).unwrap();
    let pusher = RecordingPusher::new(false);
    let pr = RecordingPr::new();
    let err = gated_release(dir, &auto_opts(None, false), &pusher, &pr)
        .expect_err("an empty branch must refuse")
        .to_string();
    assert!(err.contains("bump rides a feature PR"), "got: {err}");
    assert!(err.contains(STANDALONE_DOOR), "names the door: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), head, "nothing committed");
    assert_eq!(read_cargo_version(dir), "0.1.5", "nothing bumped");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_eq!(pr.list_calls(), 0, "no PR touched");
    drop(origin);

    // A branch whose only change is the version line.
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    git_ok(dir, &["checkout", "-b", "release-it"]);
    write_cargo(dir, "0.1.6");
    git_ok(dir, &["commit", "-am", "Bump version to v0.1.6"]);
    let head = git::head_sha(dir).unwrap();
    let pusher = RecordingPusher::new(false);
    let err = gated_release(dir, &auto_opts(None, false), &pusher, &RecordingPr::new())
        .expect_err("a version-only branch must refuse")
        .to_string();
    assert!(err.contains("--standalone"), "got: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), head);
    assert!(pusher.calls().is_empty());
    drop(origin);
}

#[test]
fn dep_bump_and_lockfile_only_branches_are_not_bump_only() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let prev = set_probe("gated:pull_request");
    let mut states = Vec::new();
    for case in ["dep-bump", "lock-refresh", "version-only"] {
        let (origin, work) = setup_released("0.1.5");
        let dir = work.path();
        fs::write(dir.join("Cargo.lock"), "# lock v1\n").unwrap();
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-m", "lockfile"]);
        git_ok(dir, &["push", "origin", "main"]);
        git_ok(dir, &["checkout", "-b", case]);
        match case {
            "dep-bump" => {
                fs::write(
                    dir.join("Cargo.toml"),
                    "[package]\nname = \"test-pkg\"\nversion = \"0.1.5\"\n\n[dependencies]\nserde = \"1\"\n",
                )
                .unwrap();
                fs::write(dir.join("Cargo.lock"), "# lock v2\n").unwrap();
            }
            "lock-refresh" => fs::write(dir.join("Cargo.lock"), "# lock v2\n").unwrap(),
            _ => write_cargo(dir, "0.1.6"),
        }
        git_ok(dir, &["commit", "-am", case]);
        states.push(format!("{:?}", classify(dir, &auto_opts(None, false)).unwrap()));
        drop(origin);
    }
    restore_probe(prev);

    for state in &states[..2] {
        assert!(
            state.starts_with("GatedFresh") && state.contains("force: false"),
            "fresh work: {state}"
        );
    }
    assert!(states[2].starts_with("GatedBumpOnlyBranch"), "{}", states[2]);
}

#[test]
fn ungated_standalone_releases_from_tagged_default() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let tagged_tip = git::head_sha(dir).unwrap();

    let pusher = RecordingPusher::new(false);
    let err = ungated_release(dir, &auto_opts(None, false), &pusher)
        .expect_err("no order: nothing to release")
        .to_string();
    assert!(err.contains("nothing to release"), "got: {err}");
    assert!(err.contains("--standalone"), "names the flag: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), tagged_tip, "no version commit");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_no_tag_anywhere(dir, "v0.1.6");

    let report = ungated_release(dir, &standalone_opts(None, ORDER), &pusher).expect("the order ships v0.1.6");
    assert_eq!(report.tag, "v0.1.6");
    assert!(!report.resumed);
    assert_eq!(read_cargo_version(dir), "0.1.6");
    assert_eq!(
        git_ok(dir, &["rev-parse", "HEAD~1"]),
        tagged_tip,
        "a new version commit on the tip"
    );
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    assert_eq!(
        git::remote_tag_commit(dir, "v0.1.6").unwrap(),
        Some(git::head_sha(dir).unwrap()),
        "v0.1.6 on origin at the version commit"
    );
    drop(origin);
}

#[test]
fn standalone_with_empty_words_refuses() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();
    let pusher = RecordingPusher::new(false);
    let err = ungated_release(dir, &standalone_opts(None, "  "), &pusher)
        .expect_err("empty words are no order")
        .to_string();
    assert!(err.contains("--standalone needs Scott's words"), "got: {err}");
    assert_eq!(git::head_sha(dir).unwrap(), head);
    assert!(pusher.calls().is_empty());
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);
}

/// On a branch that already carries work the order changes nothing but the audit trail:
/// gated quotes it in the PR body, ungated releases the commits ahead as usual.
#[test]
fn standalone_on_work_carrying_branches_only_quotes_the_order() {
    let _guard = crate::ENV_LOCK.lock().unwrap();

    let (origin, work) = setup_gated_feature_branch("0.1.5");
    let dir = work.path();
    let feature = git::head_sha(dir).unwrap();
    let pr = RecordingPr::new();
    let pusher = RecordingPusher::new(false);
    let report = gated_release(dir, &standalone_opts(None, ORDER), &pusher, &pr).expect("gated work branch pauses");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(git::current_branch(dir).unwrap(), "feature", "no standalone branch cut");
    assert_eq!(git_ok(dir, &["rev-parse", "HEAD~1"]), feature);
    assert_eq!(pusher.calls(), vec!["feature:feature".to_string()]);
    assert!(pr.created()[0].3.contains(ORDER));
    assert_no_tag_anywhere(dir, "v0.1.6");
    drop(origin);

    let (origin, work) = setup_with_pending_commit("0.1.5");
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let report = ungated_release(dir, &standalone_opts(None, ORDER), &pusher).expect("ungated ahead releases");
    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(subject_of(dir, "HEAD~1"), "feature");
    assert_eq!(
        pusher.calls(),
        vec!["branch:main".to_string(), "tag:v0.1.6".to_string()]
    );
    drop(origin);
}

/// A gated default carrying an untagged version is `bump finish`'s state: `--standalone`
/// refuses naming the pending version and `bump finish`, and nothing is cut, committed,
/// pushed or tagged. The ungated twin takes the pending row (the order is only printed).
#[test]
fn standalone_on_a_pending_version_refuses_naming_bump_finish() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();
    let branches = git_ok(dir, &["branch", "--format=%(refname:short)"]);

    let pusher = RecordingPusher::new(false);
    let pr = RecordingPr::new();
    let err = gated_release(dir, &standalone_opts(None, ORDER), &pusher, &pr)
        .expect_err("a pending version on the gated default must refuse")
        .to_string();
    assert!(err.contains("v0.1.6"), "names the pending version: {err}");
    assert!(err.contains("Run: bump finish"), "names the next command: {err}");
    assert_eq!(git::current_branch(dir).unwrap(), "main", "nothing checked out");
    assert_eq!(
        git_ok(dir, &["branch", "--format=%(refname:short)"]),
        branches,
        "no bump branch cut"
    );
    assert!(!git::local_branch_exists(dir, "bump-v0-1-7").unwrap());
    assert_eq!(git::head_sha(dir).unwrap(), head, "nothing committed");
    assert_eq!(read_cargo_version(dir), "0.1.6", "nothing bumped");
    assert!(pusher.calls().is_empty(), "nothing pushed");
    assert_eq!(pr.list_calls(), 0, "no PR touched");
    assert_no_tag_anywhere(dir, "v0.1.6");
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);

    let (origin, work) = setup_pending_on_origin("0.1.5", "0.1.6");
    let dir = work.path();
    let head = git::head_sha(dir).unwrap();
    let pusher = RecordingPusher::new(false);
    let report = ungated_release(dir, &standalone_opts(None, ORDER), &pusher).expect("ungated pending is the release");
    assert_eq!(report.tag, "v0.1.6");
    assert!(report.resumed, "the RESUME row, never a standalone bump");
    assert_eq!(git::head_sha(dir).unwrap(), head, "no version commit");
    assert_eq!(pusher.calls(), vec!["tag:v0.1.6".to_string()]);
    assert_no_tag_anywhere(dir, "v0.1.7");
    drop(origin);
}

/// setup_released at 0.1.5 whose Cargo.toml also carries a table-form dependency
/// (`[dependencies.itoa] version = "1.0.14"`), pushed; then a feature branch that moves that
/// dependency to 1.0.15 (plus a work file when `with_work`). The package version is untouched.
fn setup_dependency_table_bump(with_work: bool) -> (TempDir, TempDir) {
    let (origin, work) = setup_released("0.1.5");
    let dir = work.path();
    let manifest = |dep: &str| {
        format!("[package]\nname = \"test-pkg\"\nversion = \"0.1.5\"\n\n[dependencies.itoa]\nversion = \"{dep}\"\n")
    };
    fs::write(dir.join("Cargo.toml"), manifest("1.0.14")).unwrap();
    git_ok(dir, &["commit", "-am", "add itoa"]);
    git_ok(dir, &["push", "origin", "main"]);
    git_ok(dir, &["checkout", "-b", "bump-itoa"]);
    fs::write(dir.join("Cargo.toml"), manifest("1.0.15")).unwrap();
    if with_work {
        fs::write(dir.join("feature.txt"), "work").unwrap();
    }
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-m", "bump itoa"]);
    (origin, work)
}

/// Audit round 1, must-fix 2: a `version =` line under `[dependencies.<name>]` is not the
/// branch's own bump. Work plus a table-form dependency bump is fresh work: the version
/// commit happens and the PR body names the NEW version, not the released one.
#[test]
fn dependency_table_version_change_with_work_bumps_fresh() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_dependency_table_bump(true);
    let dir = work.path();
    let pusher = RecordingPusher::new(false);
    let pr = RecordingPr::new();
    let report = gated_release(dir, &auto_opts(None, false), &pusher, &pr).expect("fresh work pauses on a PR");

    assert_eq!(report.tag, "v0.1.6");
    assert_eq!(read_cargo_version(dir), "0.1.6", "the version commit happened");
    let body = &pr.created()[0].3;
    assert!(body.contains("Release: rides this PR (v0.1.6)"), "got: {body}");
    assert!(!body.contains("(v0.1.5)"), "never names the released version: {body}");
    drop(origin);
}

/// Audit round 1, must-fix 2: a dependency-table-only change is a dependency bump, which
/// the design doc (Bump-only branch) says is NOT bump-only.
#[test]
fn dependency_table_only_change_is_not_bump_only() {
    let _guard = crate::ENV_LOCK.lock().unwrap();
    let (origin, work) = setup_dependency_table_bump(false);
    let dir = work.path();
    let prev = set_probe("gated:pull_request");
    let state = format!("{:?}", classify(dir, &auto_opts(None, false)).unwrap());
    restore_probe(prev);
    assert!(
        state.starts_with("GatedFresh") && state.contains("force: false"),
        "a dependency bump is fresh work: {state}"
    );
    drop(origin);
}
