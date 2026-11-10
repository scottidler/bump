//! `bump --gates` binary tests: exit codes, no "All done!", the `Default:` label.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {args:?} failed");
}

fn repo(parent: &Path, name: &str) -> std::path::PathBuf {
    let dir = parent.join(name);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"shake\"\nversion = \"0.1.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn gates(cwd: &Path, probe: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bump"))
        .arg("--gates")
        .args(args)
        .env("BUMP_GATES_PROBE", probe)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn unknown_exits_1_with_the_report_still_printed() {
    let tmp = TempDir::new().unwrap();
    let r = repo(tmp.path(), "r");
    let out = gates(&r, "unknown:offline", &[]);
    assert_eq!(out.status.code(), Some(1));
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(so.contains("Gates:  UNKNOWN"), "report printed; stdout: {so}");
    assert!(so.contains("Default:") && !so.contains("Branch:"), "stdout: {so}");
}

/// `/plan:release` matches this whole line byte for byte; the padding is a contract.
#[test]
fn ungated_line_is_byte_identical_to_v0_4_7() {
    let tmp = TempDir::new().unwrap();
    let r = repo(tmp.path(), "r");
    let out = gates(&r, "ungated", &[]);
    assert_eq!(out.status.code(), Some(0));
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(so.lines().any(|l| l == "Gates:  none (ungated)"), "stdout: {so}");
}

#[test]
fn known_verdict_exits_0() {
    let tmp = TempDir::new().unwrap();
    let r = repo(tmp.path(), "r");
    for probe in ["ungated", "gated:pull_request"] {
        let out = gates(&r, probe, &[]);
        assert_eq!(out.status.code(), Some(0), "{probe}");
    }
}

#[test]
fn multi_repo_gates_never_prints_all_done() {
    let tmp = TempDir::new().unwrap();
    let a = repo(tmp.path(), "a");
    let b = repo(tmp.path(), "b");
    let args = [a.to_str().unwrap(), b.to_str().unwrap()];
    let ok = gates(tmp.path(), "ungated", &args);
    assert_eq!(ok.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&ok.stdout).contains("All done!"));
    let bad = gates(tmp.path(), "unknown:offline", &args);
    assert_eq!(bad.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&bad.stdout).contains("All done!"));
}

#[test]
fn gates_with_no_verify_is_a_clap_error() {
    let tmp = TempDir::new().unwrap();
    let r = repo(tmp.path(), "r");
    let out = gates(&r, "ungated", &["--no-verify"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
}
