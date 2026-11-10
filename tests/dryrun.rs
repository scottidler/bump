//! Plain `bump -n` says what the real run does: it refuses a tagged HEAD, names the
//! latest tag as "from" in a generic repo, lists each manifest with its lockfile, and
//! says "Tagged" when no commit would happen. Real compiled binary, fixture repos.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init(work: &Path) {
    fs::create_dir_all(work).unwrap();
    git(work, &["init", "-q", "-b", "main"]);
    git(work, &["config", "user.email", "t@t"]);
    git(work, &["config", "user.name", "t"]);
}

/// The doc's `t4`: Cargo 0.1.2, HEAD tagged v0.1.2, pushed to a bare origin.
fn released_head(tmp: &Path) -> PathBuf {
    let origin = tmp.join("origin.git");
    let work = tmp.join("t4");
    git(tmp, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    init(&work);
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"shake\"\nversion = \"0.1.2\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "init"]);
    git(&work, &["tag", "-a", "v0.1.2", "-m", "v0.1.2"]);
    git(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&work, &["push", "-q", "origin", "main", "v0.1.2"]);
    work
}

/// The doc's `G`: no manifest, v0.6.0 tagged, one commit past it.
fn generic_repo(tmp: &Path) -> PathBuf {
    let work = tmp.join("G");
    init(&work);
    fs::write(work.join("f"), "a\n").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-qm", "a"]);
    git(&work, &["tag", "-a", "v0.6.0", "-m", "v0.6.0"]);
    fs::write(work.join("f"), "a\nb\n").unwrap();
    git(&work, &["commit", "-qam", "b"]);
    work
}

fn bump(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(args)
        .current_dir(cwd)
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn dry_run_refuses_a_tagged_head_like_the_real_run() {
    let tmp = TempDir::new().unwrap();
    let work = released_head(tmp.path());
    let dry = bump(&work, &["-n", "--no-verify"]);
    assert_eq!(dry.status.code(), Some(1), "dry-run must refuse: {}", out(&dry));
    assert!(err(&dry).contains("HEAD already has a tag"), "{}", err(&dry));
    assert!(!out(&dry).contains("Would"), "must not promise a commit: {}", out(&dry));

    let real = bump(&work, &["--no-verify"]);
    assert_eq!(real.status.code(), Some(1));
    assert!(err(&real).contains("HEAD already has a tag"), "{}", err(&real));
}

#[test]
fn dry_run_with_force_plans_past_a_tagged_head() {
    let tmp = TempDir::new().unwrap();
    let work = released_head(tmp.path());
    let dry = bump(&work, &["-n", "--no-verify", "--force"]);
    assert_eq!(dry.status.code(), Some(0), "{}{}", out(&dry), err(&dry));
    // HEAD is pushed, so --force plans a new commit, not an amend.
    assert!(
        out(&dry).contains("[dry-run] Would commit and tag: v0.1.3"),
        "{}",
        out(&dry)
    );
    assert!(!out(&dry).contains("amend"), "{}", out(&dry));
}

#[test]
fn dry_run_generic_repo_bumps_from_the_latest_tag_and_says_tag() {
    let tmp = TempDir::new().unwrap();
    let work = generic_repo(tmp.path());
    let dry = bump(&work, &["-n", "--no-verify"]);
    assert_eq!(dry.status.code(), Some(0), "{}{}", out(&dry), err(&dry));
    let stdout = out(&dry);
    assert!(stdout.contains("bump: v0.6.0 -> 0.6.1"), "{stdout}");
    assert!(!stdout.contains("unknown"), "{stdout}");
    assert!(stdout.contains("[dry-run] Would tag: v0.6.1"), "{stdout}");
    assert!(
        !stdout.contains("Would update"),
        "a generic repo has no file to update: {stdout}"
    );
}

#[test]
fn real_run_generic_repo_says_tagged_not_committed() {
    let tmp = TempDir::new().unwrap();
    let work = generic_repo(tmp.path());
    let real = bump(&work, &["--no-verify"]);
    assert_eq!(real.status.code(), Some(0), "{}{}", out(&real), err(&real));
    let stdout = out(&real);
    assert!(stdout.contains("bump: v0.6.0 -> 0.6.1"), "{stdout}");
    assert!(stdout.contains("Tagged v0.6.1"), "{stdout}");
    assert!(!stdout.contains("Committed and tagged"), "{stdout}");
}

#[test]
fn dry_run_lists_the_manifest_with_its_lockfile() {
    let tmp = TempDir::new().unwrap();
    let work = released_head(tmp.path());
    fs::write(work.join("Cargo.lock"), "version = 3\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "lock"]);
    let dry = bump(&work, &["-n", "--no-verify"]);
    assert_eq!(dry.status.code(), Some(0), "{}{}", out(&dry), err(&dry));
    assert!(
        out(&dry).contains("[dry-run] Would update: Cargo.toml, Cargo.lock"),
        "{}",
        out(&dry)
    );
    assert!(
        out(&dry).contains("[dry-run] Would amend previous commit and tag: v0.1.3"),
        "{}",
        out(&dry)
    );
}
