//! The verify verdict `bump finish -n --install '<cmd>'` prints is true: an env-prefixed
//! `cargo install` verifies, a compound skips on shell syntax, and a `--path` outside the
//! repo skips instead of verifying another crate. Real compiled binary on the design
//! doc's `t4` fixture (released, HEAD tagged) with a sibling `other` crate.

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

fn cli_manifest(name: &str, version: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n\n[dependencies]\nclap = \"4\"\n"
    )
}

/// The doc's `mk t4; v2; tag v0.1.2`: a clap CLI `shake` at 0.1.2, HEAD tagged v0.1.2,
/// pushed to a bare `origin` with `origin/HEAD` set; plus an untracked sibling crate
/// `other` (`otherbin` 9.9.9). Returns the `t4` work tree.
fn released_t4_with_sibling(tmp: &Path) -> PathBuf {
    let origin = tmp.join("t4.git");
    let work = tmp.join("t4");
    fs::create_dir_all(work.join("src")).unwrap();
    git(tmp, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    fs::write(work.join("Cargo.toml"), cli_manifest("shake", "0.1.2")).unwrap();
    fs::write(work.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "v0.1.2"]);
    git(&work, &["tag", "-a", "v0.1.2", "-m", "v0.1.2"]);
    git(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&work, &["push", "-q", "origin", "main", "v0.1.2"]);
    git(&work, &["remote", "set-head", "origin", "main"]);

    let other = tmp.join("other");
    fs::create_dir_all(other.join("src")).unwrap();
    fs::write(other.join("Cargo.toml"), cli_manifest("otherbin", "9.9.9")).unwrap();
    fs::write(other.join("src/main.rs"), "fn main() {}\n").unwrap();
    work
}

fn finish_dry_run(work: &Path, install: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bump"))
        .args(["finish", "-n", "--install", install])
        .current_dir(work)
        .env("BUMP_GATES_PROBE", "ungated")
        .output()
        .unwrap()
}

/// The `[dry-run] verify:` line of a successful `finish -n`.
fn verify_line(work: &Path, install: &str) -> String {
    let out = finish_dry_run(work, install);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "finish -n --install {install:?} failed; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        .find(|line| line.starts_with("[dry-run] verify:"))
        .unwrap_or_else(|| panic!("no verify line; stdout: {stdout}"))
        .to_string()
}

#[test]
fn env_prefixed_cargo_install_verifies() {
    let tmp = TempDir::new().unwrap();
    let work = released_t4_with_sibling(tmp.path());
    for install in ["RUSTFLAGS=-g cargo install --path .", "env A=1 cargo install --path ."] {
        assert_eq!(
            verify_line(&work, install),
            "[dry-run] verify: shake --version == v0.1.2",
            "{install}"
        );
    }
}

#[test]
fn compound_install_skips_on_shell_syntax() {
    let tmp = TempDir::new().unwrap();
    let work = released_t4_with_sibling(tmp.path());
    for install in ["cargo install --path . && echo done", "cargo install --path .;"] {
        assert_eq!(
            verify_line(&work, install),
            "[dry-run] verify: skipped (install command uses shell syntax; verify reads one plain cargo install)",
            "{install}"
        );
    }
}

#[test]
fn out_of_repo_path_skips_instead_of_verifying_another_crate() {
    let tmp = TempDir::new().unwrap();
    let work = released_t4_with_sibling(tmp.path());
    let other = tmp.path().join("other").canonicalize().unwrap();
    assert_eq!(
        verify_line(&work, "cargo install --path ../other"),
        format!(
            "[dry-run] verify: skipped (install target {} is outside this repo)",
            other.display()
        )
    );
}

#[test]
fn custom_install_root_skips() {
    let tmp = TempDir::new().unwrap();
    let work = released_t4_with_sibling(tmp.path());
    for install in [
        "CARGO_INSTALL_ROOT=/x cargo install --path .",
        "cargo install --path . --root /x",
    ] {
        assert_eq!(
            verify_line(&work, install),
            "[dry-run] verify: skipped (install command sets its own install root)",
            "{install}"
        );
    }
}
