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
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    commit_and_push(origin_dir, work);
}

/// A web service: `[package]`, `src/main.rs` and `axum`, no `clap` (valet's shape).
fn setup_ungated_service_repo(origin_dir: &Path, work: &Path) {
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"svc\"\nversion = \"0.1.0\"\n\n[dependencies]\naxum = \"0.8\"\n",
    )
    .unwrap();
    fs::create_dir_all(work.join("src")).unwrap();
    fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    commit_and_push(origin_dir, work);
}

/// Commit everything in `work` on `main` and push it to a fresh bare `origin_dir`.
fn commit_and_push(origin_dir: &Path, work: &Path) {
    Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main", origin_dir.to_str().unwrap()])
        .output()
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
/// flags present at least once (presence, not a line count -- a line count breaks the
/// moment the after-help spells a flag out in prose, which it must: an agent reading
/// `--help` needs to see `--standalone "<words>"` etc. literally, not paraphrased).
/// Mirrors the design doc's amended Acceptance Criterion: `bump release --help | grep -oE
/// -- '--(standalone|no-ci-gate|ci-timeout)' | sort -u | wc -l` prints `3`.
#[test]
fn release_help_names_all_three_flags_at_least_once() {
    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found: Vec<&str> = ["--standalone", "--no-ci-gate", "--ci-timeout"]
        .into_iter()
        .filter(|flag| stdout.contains(flag))
        .collect();
    found.sort_unstable();
    found.dedup();
    assert_eq!(found.len(), 3, "bump release --help:\n{stdout}");
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

/// A service fixture's dry run prints the clap skip reason as its install line, and no
/// `verify:` line follows: nothing is installed, so nothing is run with `--version`.
#[test]
fn release_dry_run_on_a_service_prints_the_clap_skip_and_no_verify() {
    let origin = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    setup_ungated_service_repo(origin.path(), work.path());

    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release", "-n", "--no-ci-gate"])
        .current_dir(work.path())
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "bump release -n must succeed; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let install_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("[dry-run] install:") || line.starts_with("[dry-run] verify:"))
        .collect();
    assert_eq!(
        install_lines,
        vec!["[dry-run] install: skipped (no clap dependency: not a CLI; pass --install or set install: in bump.yml)"],
        "stdout: {stdout}"
    );
}

/// A fake `gh` on PATH that appends its argv to `log` and answers the open-PR probe
/// (`gh pr list ...`) with `answer`. Every other call (a `gh pr create` included) exits 0
/// printing a URL, so a regression that spawns it would succeed silently: only the argv
/// log can catch it.
fn fake_gh(bin: &Path, log: &Path, answer: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = \"pr list\" ]; then echo '{answer}'; exit 0; fi\necho https://github.com/o/r/pull/1\n",
        log.display()
    );
    let gh = bin.join("gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A released `v0.1.0` on `main` (annotated tag on origin) and a feature branch
/// `add-thing` carrying one work commit: the gated feature-branch release row.
fn setup_gated_feature_branch(origin_dir: &Path, work: &Path) {
    setup_ungated_repo(origin_dir, work);
    git(work, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
    git(work, &["push", "-q", "origin", "v0.1.0"]);
    git(work, &["checkout", "-q", "-b", "add-thing"]);
    fs::write(work.join("thing.txt"), "thing").unwrap();
    git(work, &["add", "-A"]);
    git(work, &["commit", "-qm", "feat: add thing"]);
}

/// The real binary, gated, on a feature branch with no open PR: it pushes the branch, runs
/// ONLY the open-PR probe through `gh`, never `gh pr create`, and prints the `/pr:create`
/// hand-off with the release-intent line on a line of its own. A PR opened inside bump is
/// invisible to the PreToolUse guards on `/pr:create`'s body contract
/// (tatari-tv/drata-cli#18), so this is pinned at the process boundary, not just the port.
#[test]
fn gated_release_never_spawns_gh_pr_create() {
    let origin = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();
    setup_gated_feature_branch(origin.path(), work.path());
    let log = bin.path().join("gh.log");
    fake_gh(bin.path(), &log, "[]");

    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap());
    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release"])
        .current_dir(work.path())
        .env("PATH", path)
        .env("BUMP_GATES_PROBE", "gated:pull_request")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "gated bump release must pause with exit 0; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.lines().any(|c| c.starts_with("pr create")),
        "bump must never run gh pr create; gh calls:\n{calls}"
    );
    assert!(
        calls.lines().any(|c| c.starts_with("pr list --head add-thing")),
        "the open-PR probe ran; gh calls:\n{calls}"
    );
    assert!(stdout.contains("next: open the PR with /pr:create"), "stdout: {stdout}");
    assert!(
        stdout.lines().any(|l| l == "Release: rides this PR (v0.1.1)"),
        "the release-intent line is printed bare, ready to copy; stdout: {stdout}"
    );
    let pushed = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "refs/heads/add-thing"])
        .current_dir(origin.path())
        .output()
        .unwrap();
    assert!(pushed.status.success(), "the feature branch was pushed to origin");
}

fn release_dry_run(work: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["release", "-n"])
        .current_dir(work)
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "bump release -n must succeed; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// A committed `ci: none` makes the real gate skip, so the preview must say so instead
/// of promising a CI wait the run never does.
#[test]
fn release_dry_run_under_committed_ci_none_previews_the_skip() {
    let origin = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    fs::write(work.path().join("bump.yml"), "ci: none\n").unwrap();
    setup_ungated_repo(origin.path(), work.path());

    let stdout = release_dry_run(work.path());
    assert!(
        stdout.contains("[dry-run] CI gate: skipped, bump.yml at") && stdout.contains("declares `ci: none`"),
        "the preview must name the ci: none skip; stdout: {stdout}"
    );
    assert!(
        !stdout.contains("wait for green CI"),
        "ci: none never waits, so the preview must not either; stdout: {stdout}"
    );
}

/// Without the declaration the preview still promises the wait.
#[test]
fn release_dry_run_without_ci_none_previews_the_wait() {
    let origin = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    setup_ungated_repo(origin.path(), work.path());

    let stdout = release_dry_run(work.path());
    assert!(stdout.contains("[dry-run] wait for green CI"), "stdout: {stdout}");
}
