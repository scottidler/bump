//! The install and verify messages (C3, S2, S3) on the real compiled binary: a failing
//! install prints its exit status and a re-run line naming the repo root; a repo with no
//! Cargo manifest says why its install was skipped.

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

/// A released repo (HEAD tagged v0.1.2, pushed to a bare origin) holding one manifest file.
fn released(tmp: &Path, name: &str, manifest: &str, contents: &str) -> PathBuf {
    let origin = tmp.join(format!("{name}.git"));
    let work = tmp.join(name);
    fs::create_dir_all(work.join("src")).unwrap();
    git(tmp, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    fs::write(work.join(manifest), contents).unwrap();
    fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "v0.1.2"]);
    git(&work, &["tag", "-a", "v0.1.2", "-m", "v0.1.2"]);
    git(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&work, &["push", "-q", "origin", "main", "v0.1.2"]);
    git(&work, &["remote", "set-head", "origin", "main"]);
    work
}

fn finish(work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bump"))
        .arg("finish")
        .args(args)
        .current_dir(work)
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap()
}

#[test]
fn failed_install_prints_the_status_and_a_dir_qualified_rerun() {
    let tmp = TempDir::new().unwrap();
    let work = released(
        tmp.path(),
        "t4",
        "Cargo.toml",
        "[package]\nname = \"shake\"\nversion = \"0.1.2\"\nedition = \"2021\"\n\n[dependencies]\nclap = \"4\"\n",
    );
    let out = finish(&work, &["--install", "exit 7"]);
    let both = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{both}");
    assert!(both.contains("exit status: 7"), "{both}");
    let root = work.canonicalize().unwrap();
    assert!(both.contains(&format!("`bump finish {}`", root.display())), "{both}");
}

#[test]
fn no_cargo_manifest_names_the_way_out() {
    let tmp = TempDir::new().unwrap();
    let work = released(
        tmp.path(),
        "py",
        "pyproject.toml",
        "[project]\nname = \"p\"\nversion = \"0.1.2\"\n",
    );
    let out = finish(&work, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout.contains("install: skipped (no Cargo manifest; pass --install or set install: in bump.yml)"),
        "{stdout}"
    );
}
