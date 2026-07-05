//! End-to-end terminal-visibility test for `--skip-member`.
//!
//! The design's whole rationale is that the skip announcement must reach the operator's
//! terminal via `println!`, NOT the file-routed `info!` log. A unit test on the message
//! helper can't catch a `println!` -> `info!` regression; only asserting the compiled
//! binary's real stdout can. This runs `bump` against a temp workspace whose
//! `claude-pricing` member pins its own version and confirms the skip line is printed.

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

/// Workspace whose `pricing` member is package `claude-pricing`, pinned to 2.0.0.
fn setup_workspace(dir: &Path) {
    fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"pricing\"]\nresolver = \"2\"\n\n\
         [workspace.package]\nversion = \"0.5.0\"\n",
    )
    .unwrap();

    fs::create_dir_all(dir.join("app")).unwrap();
    fs::write(
        dir.join("app").join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion.workspace = true\n",
    )
    .unwrap();

    fs::create_dir_all(dir.join("pricing")).unwrap();
    fs::write(
        dir.join("pricing").join("Cargo.toml"),
        "[package]\nname = \"claude-pricing\"\nversion = \"2.0.0\"\n",
    )
    .unwrap();

    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@test.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

/// `--no-tag --dry-run` runs the guard (which prints the skip) then returns before any
/// mutation, and `--no-tag` skips the gate probe, so this needs no network.
#[test]
fn skip_member_prints_skip_line_to_stdout() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    setup_workspace(dir);

    // Run from inside the workspace with no trailing positional: `--skip-member`
    // uses num_args=1.. (space-separated per the house CLI rule), so a trailing
    // directory arg would be greedily swallowed into the skip list. The realistic
    // invocation runs bump in the repo.
    let output = Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["--no-tag", "--dry-run", "--skip-member", "claude-pricing"])
        .current_dir(dir)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "bump must succeed with --skip-member; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("skipping claude-pricing (independent version 2.0.0)"),
        "skip line must reach stdout (a println!->info! regression would drop it); stdout: {stdout}"
    );
}
