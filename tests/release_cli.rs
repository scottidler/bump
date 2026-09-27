//! End-to-end terminal-visibility tests for `bump release`'s Phase 6 CLI surface: the
//! design doc's acceptance criterion (`bump release --help | grep -cE
//! 'standalone|no-ci-gate|ci-timeout'` == 3) and the `--no-ci-gate` flag reaching
//! execution (proved by the printed `CI gate: SKIPPED` line, not just the parsed flag).
//! Same pattern as `tests/skip_member.rs`: real compiled binary, real stdout.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A bare `origin` (a SEPARATE `TempDir`, so it never shows up as an untracked path in
/// `work`'s own git status -- `bump release` refuses on any dirty tree) and a `work`
/// clone on `main`, one commit, pushed, `origin/HEAD` set -- enough for
/// `remote_default_branch` and `compare_head_to_remote` to resolve locally, no network.
/// `BUMP_GATES_PROBE=ungated` stands in for `gh`'s branch-protection probe.
fn setup_ungated_repo(origin_dir: &Path, work: &Path) {
    Command::new("git")
        .args(["init", "--bare", "-q", origin_dir.to_str().unwrap()])
        .output()
        .unwrap();

    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    git(work, &["init", "-q"]);
    git(work, &["config", "user.email", "test@test.com"]);
    git(work, &["config", "user.name", "Test"]);
    git(work, &["add", "-A"]);
    git(work, &["commit", "-qm", "init"]);
    git(work, &["branch", "-m", "main"]);
    git(work, &["remote", "add", "origin", origin_dir.to_str().unwrap()]);
    git(work, &["push", "-u", "-q", "origin", "main"]);
    git(work, &["remote", "set-head", "origin", "main"]);
}

/// Acceptance criterion, verbatim: `bump release --help` names exactly the three Phase 6
/// flags once each (clap's own Options block); the after-help prose must not repeat them.
#[test]
fn release_help_names_the_three_flags_exactly_three_times() {
    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matching = stdout
        .lines()
        .filter(|l| l.contains("standalone") || l.contains("no-ci-gate") || l.contains("ci-timeout"))
        .count();
    assert_eq!(matching, 3, "bump release --help:\n{stdout}");
}

/// `bump release -n --no-ci-gate` on a fixture prints the `CI gate: SKIPPED` line,
/// proving the flag reaches past parsing into `wait_for_green`'s dry-run echo, not just
/// into a parsed-and-ignored field.
#[test]
fn release_dry_run_no_ci_gate_prints_ci_gate_skipped() {
    let origin = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    setup_ungated_repo(origin.path(), work.path());

    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release", "-n", "--no-ci-gate"])
        .current_dir(work.path())
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "bump release -n --no-ci-gate must succeed; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("CI gate: SKIPPED"),
        "--no-ci-gate must reach wait_for_green's dry-run echo; stdout: {stdout}"
    );
}
