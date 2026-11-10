//! State-aware `bump finish -n` (S1): it fetches `origin/<default>`, classifies the merged
//! tip without checking it out, and prints only the steps the real run would take from
//! that state. Real compiled binary on the design doc's `t4` (released, HEAD tagged) and
//! `t5` (merged version pushed, untagged) fixtures, each with a local bare `origin`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn cli_manifest(version: &str) -> String {
    format!(
        "[package]\nname = \"shake\"\nversion = \"{version}\"\nedition = \"2021\"\n\n[dependencies]\nclap = \"4\"\n"
    )
}

/// The doc's `mk`: a clap CLI `shake` at 0.1.1 tagged v0.1.1, one commit past the tag,
/// both pushed to a bare `<name>.git` with `origin/HEAD` set. Returns the work tree.
fn mk(tmp: &Path, name: &str) -> PathBuf {
    let origin = tmp.join(format!("{name}.git"));
    let work = tmp.join(name);
    fs::create_dir_all(work.join("src")).unwrap();
    git(tmp, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    fs::write(work.join("Cargo.toml"), cli_manifest("0.1.1")).unwrap();
    fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "init"]);
    git(&work, &["tag", "-a", "v0.1.1", "-m", "v0.1.1"]);
    fs::write(work.join("src/main.rs"), "fn main() {}\n// more\n").unwrap();
    git(&work, &["commit", "-qam", "more"]);
    git(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&work, &["push", "-q", "origin", "main", "v0.1.1"]);
    git(&work, &["remote", "set-head", "origin", "main"]);
    work
}

/// The doc's `v2`: commit Cargo.toml at 0.1.2.
fn v2(work: &Path) {
    fs::write(work.join("Cargo.toml"), cli_manifest("0.1.2")).unwrap();
    git(work, &["commit", "-qam", "v0.1.2"]);
}

/// `mk t4; v2; tag v0.1.2; push`: released, HEAD tagged.
fn t4(tmp: &Path) -> PathBuf {
    let work = mk(tmp, "t4");
    v2(&work);
    git(&work, &["tag", "-a", "v0.1.2", "-m", "v0.1.2"]);
    git(&work, &["push", "-q", "origin", "main", "v0.1.2"]);
    work
}

/// `mk t5; v2; push`: the merged version 0.1.2 is on origin, untagged (pending).
fn t5(tmp: &Path) -> PathBuf {
    let work = mk(tmp, "t5");
    v2(&work);
    git(&work, &["push", "-q", "origin", "main"]);
    work
}

fn finish_n(work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["finish", "-n"])
        .args(args)
        .current_dir(work)
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A successful `finish -n`'s stdout.
fn planned(work: &Path, args: &[&str]) -> String {
    let out = finish_n(work, args);
    assert!(
        out.status.success(),
        "finish -n {args:?} failed; stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    stdout(&out)
}

/// Everything `finish -n` must leave alone: HEAD, every ref, the worktree status and the
/// manifest bytes.
fn snapshot(work: &Path) -> (String, String, String, String) {
    (
        git(work, &["rev-parse", "HEAD"]),
        git(work, &["for-each-ref"]),
        git(work, &["status", "--porcelain", "--untracked-files=all"]),
        fs::read_to_string(work.join("Cargo.toml")).unwrap(),
    )
}

#[test]
fn already_released_plans_only_install_and_verify() {
    let tmp = TempDir::new().unwrap();
    let work = t4(tmp.path());
    let out = planned(&work, &[]);
    assert!(
        out.contains("[dry-run] already released v0.1.2; would run: install, verify\n"),
        "{out}"
    );
    assert!(!out.contains("git tag -a"), "no tag step on a released version: {out}");
    assert!(!out.contains("git push"), "no push step on a released version: {out}");
    assert!(
        !out.contains("wait for green CI"),
        "no CI wait on a released version: {out}"
    );
    assert!(out.contains("[dry-run] install: cargo install --path .\n"), "{out}");
    assert!(out.contains("[dry-run] verify: shake --version == v0.1.2\n"), "{out}");
}

#[test]
fn already_released_with_no_install_plans_nothing() {
    let tmp = TempDir::new().unwrap();
    let work = t4(tmp.path());
    let out = planned(&work, &["--no-install"]);
    assert!(
        out.contains("[dry-run] already released v0.1.2; would run: nothing\n"),
        "{out}"
    );
    assert!(out.contains("[dry-run] install: skipped\n"), "{out}");
    assert!(!out.contains("git tag -a"), "{out}");
}

#[test]
fn pending_merged_version_plans_the_tag_and_push() {
    let tmp = TempDir::new().unwrap();
    let work = t5(tmp.path());
    let out = planned(&work, &[]);
    assert!(out.contains("[dry-run] v0.1.2 is untagged on origin/main\n"), "{out}");
    assert!(
        out.contains("[dry-run] git tag -a v0.1.2 <sha> -m \"Release v0.1.2\"\n"),
        "{out}"
    );
    assert!(
        out.contains("[dry-run] git push --no-follow-tags origin v0.1.2\n"),
        "{out}"
    );
    assert!(!out.contains("already released"), "{out}");
    assert!(
        !out.contains("git pull"),
        "local main equals origin/main: no pull step: {out}"
    );
    assert!(out.contains("[dry-run] verify: shake --version == v0.1.2\n"), "{out}");
}

#[test]
fn a_local_only_tag_at_the_tip_plans_the_push_without_a_new_tag() {
    let tmp = TempDir::new().unwrap();
    let work = t5(tmp.path());
    git(&work, &["tag", "-a", "v0.1.2", "-m", "v0.1.2"]);
    let out = planned(&work, &[]);
    assert!(
        out.contains("[dry-run] local tag v0.1.2 at origin/main was never pushed; resuming\n"),
        "{out}"
    );
    assert!(
        out.contains("[dry-run] (local tag v0.1.2 already present at the sha)\n"),
        "{out}"
    );
    assert!(!out.contains("git tag -a"), "{out}");
    assert!(
        out.contains("[dry-run] git push --no-follow-tags origin v0.1.2\n"),
        "{out}"
    );
}

/// The merged version is pushed from another clone; this checkout (and its stale
/// `origin/main`) still sits at 0.1.1, whose tag v0.1.1 is on origin at an older commit.
/// Read from the checkout that is a missed bump; read from origin's fetched tip it is
/// v0.1.2, untagged, behind by one: pull, then tag.
#[test]
fn a_checkout_behind_origin_is_classified_by_origins_tip() {
    let tmp = TempDir::new().unwrap();
    let upstream = mk(tmp.path(), "t5");
    let behind = tmp.path().join("behind");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            tmp.path().join("t5.git").to_str().unwrap(),
            behind.to_str().unwrap(),
        ],
    );
    git(&behind, &["config", "user.email", "t@t"]);
    git(&behind, &["config", "user.name", "t"]);
    v2(&upstream);
    git(&upstream, &["push", "-q", "origin", "main"]);
    let tip = git(&upstream, &["rev-parse", "HEAD"]);
    assert_ne!(
        git(&behind, &["rev-parse", "origin/main"]),
        tip,
        "precondition: the clone's origin/main is stale"
    );
    let (head, _, status, manifest) = snapshot(&behind);
    let heads = git(&behind, &["for-each-ref", "refs/heads", "refs/tags"]);

    let out = planned(&behind, &[]);
    assert!(
        out.contains(&format!(
            "[dry-run] fetched origin/main: {} carries v0.1.2\n",
            &tip[..8]
        )),
        "{out}"
    );
    assert!(out.contains("[dry-run] git pull --ff-only origin main\n"), "{out}");
    assert!(out.contains("[dry-run] git tag -a v0.1.2 <sha>"), "{out}");
    assert!(!out.contains("v0.1.1"), "the checkout's version never leaks in: {out}");

    // The fetch moved origin/main and nothing else.
    assert_eq!(git(&behind, &["rev-parse", "origin/main"]), tip);
    let (head_after, _, status_after, manifest_after) = snapshot(&behind);
    assert_eq!(head_after, head, "HEAD unchanged");
    assert_eq!(status_after, status, "worktree unchanged");
    assert_eq!(manifest_after, manifest, "checkout still at 0.1.1");
    assert_eq!(
        git(&behind, &["for-each-ref", "refs/heads", "refs/tags"]),
        heads,
        "no branch or tag moved"
    );
}

#[test]
fn finish_n_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    for work in [t4(tmp.path()), t5(tmp.path())] {
        let before = snapshot(&work);
        let out = finish_n(&work, &[]);
        assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
        assert_eq!(
            snapshot(&work),
            before,
            "{}: HEAD, refs or worktree moved",
            work.display()
        );
    }
}

/// The missed-bump shape (`mk` alone: origin/main one commit past v0.1.1, version
/// unchanged) refuses in the dry-run exactly as the real run refuses.
#[test]
fn missed_bump_refuses_like_the_real_run() {
    let tmp = TempDir::new().unwrap();
    let work = mk(tmp.path(), "sub");
    let out = finish_n(&work, &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(stderr(&out).contains("no untagged version on main"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "no steps printed: {}", stdout(&out));
}

/// Phase 7's empty-install refusal survives the state-aware dry-run, on both states.
#[test]
fn empty_install_is_still_refused() {
    let tmp = TempDir::new().unwrap();
    for work in [t4(tmp.path()), t5(tmp.path())] {
        let out = finish_n(&work, &["--install", ""]);
        assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
        assert!(
            stderr(&out).contains("install command is empty; pass --no-install to skip the install"),
            "{}",
            stderr(&out)
        );
    }
}

/// Phases 8/9's verify verdicts still print on both states.
#[test]
fn verify_lines_survive_on_both_states() {
    let tmp = TempDir::new().unwrap();
    for work in [t4(tmp.path()), t5(tmp.path())] {
        let out = planned(&work, &["--install", "cargo install --path . && echo done"]);
        assert!(
            out.contains(
                "[dry-run] verify: skipped (install command uses shell syntax; verify reads one plain cargo install)\n"
            ),
            "{out}"
        );
        let out = planned(&work, &["--install", "RUSTFLAGS=-g cargo install --path ."]);
        assert!(out.contains("[dry-run] verify: shake --version == v0.1.2\n"), "{out}");
    }
}

/// `remote.origin.tagOpt=--tags` makes a plain `git fetch origin main` import every tag
/// on origin; the dry-run's fetch is `--no-tags`, so a tag absent locally stays absent.
#[test]
fn finish_n_imports_no_tag_under_tagopt_tags() {
    let tmp = TempDir::new().unwrap();
    let work = t5(tmp.path());
    let extra = tmp.path().join("extra");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            tmp.path().join("t5.git").to_str().unwrap(),
            extra.to_str().unwrap(),
        ],
    );
    git(&extra, &["config", "user.email", "t@t"]);
    git(&extra, &["config", "user.name", "t"]);
    git(&extra, &["tag", "-a", "vEXTRA", "-m", "vEXTRA", "HEAD~1"]);
    git(&extra, &["push", "-q", "origin", "vEXTRA"]);
    git(&work, &["config", "remote.origin.tagOpt", "--tags"]);

    let out = finish_n(&work, &["--no-install", "--no-ci-gate"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert_eq!(git(&work, &["tag", "-l"]), "v0.1.1", "no tag imported");
}

/// A clone whose local main is behind origin, where the merge changed `install:` in a
/// tracked `bump.yml`: the dry-run shows the merged tip's install command, the one the
/// real run reads after its fast-forward.
#[test]
fn finish_n_behind_reads_the_merged_tips_bump_yml() {
    let tmp = TempDir::new().unwrap();
    let upstream = mk(tmp.path(), "t5");
    fs::write(upstream.join("bump.yml"), "install: cargo install --path .\n").unwrap();
    git(&upstream, &["add", "bump.yml"]);
    git(&upstream, &["commit", "-qm", "bump.yml"]);
    git(&upstream, &["push", "-q", "origin", "main"]);
    let behind = tmp.path().join("behind");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            tmp.path().join("t5.git").to_str().unwrap(),
            behind.to_str().unwrap(),
        ],
    );
    fs::write(upstream.join("bump.yml"), "install: cargo install --locked --path .\n").unwrap();
    git(&upstream, &["add", "bump.yml"]);
    v2(&upstream);
    git(&upstream, &["push", "-q", "origin", "main"]);

    let out = planned(&behind, &[]);
    assert!(out.contains("[dry-run] git pull --ff-only origin main\n"), "{out}");
    assert!(
        out.contains("[dry-run] install: cargo install --locked --path .\n"),
        "the merged tip's install command: {out}"
    );
    assert_eq!(
        fs::read_to_string(behind.join("bump.yml")).unwrap(),
        "install: cargo install --path .\n",
        "the worktree file is untouched"
    );
}

/// No `bump.yml` at the merged tip and an untracked one in the worktree: the move leaves
/// the untracked file alone, so the dry-run reads it, as the real run does.
#[test]
fn finish_n_behind_falls_back_to_an_untracked_bump_yml() {
    let tmp = TempDir::new().unwrap();
    let upstream = mk(tmp.path(), "t5");
    let behind = tmp.path().join("behind");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            tmp.path().join("t5.git").to_str().unwrap(),
            behind.to_str().unwrap(),
        ],
    );
    fs::write(behind.join("bump.yml"), "install: cargo install --offline --path .\n").unwrap();
    v2(&upstream);
    git(&upstream, &["push", "-q", "origin", "main"]);

    let out = planned(&behind, &[]);
    assert!(out.contains("[dry-run] git pull --ff-only origin main\n"), "{out}");
    assert!(
        out.contains("[dry-run] install: cargo install --offline --path .\n"),
        "the untracked worktree file: {out}"
    );
}

/// A `ci: none` committed at the merged tip: the real run skips the CI wait
/// (`wait_for_green`), so the dry-run prints the skip and no wait line.
#[test]
fn finish_n_prints_no_ci_wait_under_ci_none() {
    let tmp = TempDir::new().unwrap();
    let work = mk(tmp.path(), "ci");
    fs::write(work.join("bump.yml"), "ci: none\n").unwrap();
    git(&work, &["add", "bump.yml"]);
    v2(&work);
    git(&work, &["push", "-q", "origin", "main"]);

    let out = planned(&work, &["--no-install"]);
    assert!(!out.contains("wait for green CI"), "{out}");
    assert!(out.contains("[dry-run] CI gate: skipped, bump.yml at "), "{out}");
    assert!(out.contains(" declares `ci: none`\n"), "{out}");
    assert!(out.contains("[dry-run] git tag -a v0.1.2 <sha>"), "{out}");

    let untagged = t5(tmp.path());
    let out = planned(&untagged, &["--no-install"]);
    assert!(
        out.contains("[dry-run] wait for green CI"),
        "no ci: none, so it waits: {out}"
    );
}
