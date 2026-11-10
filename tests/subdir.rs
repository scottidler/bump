//! A subdirectory runs on its repo root, from every entry point: plain bump, `--gates`,
//! `--tag-only`, `release`, `finish`, and a verb's `[DIR]`. Real compiled binary, real
//! stdout/stderr, fixture repos with a local bare `origin` (no network;
//! `BUMP_GATES_PROBE` stands in for the gh probe).

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

/// The design doc's `mk` fixture: Cargo 0.1.1 tagged v0.1.1, one commit past the tag,
/// both pushed to a bare `origin`, `origin/HEAD` set. Returns the work tree path.
fn released_repo(tmp: &Path) -> PathBuf {
    let origin = tmp.join("origin.git");
    let work = tmp.join("work");
    fs::create_dir_all(work.join("src")).unwrap();
    git(tmp, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    fs::write(
        work.join("Cargo.toml"),
        "[package]\nname = \"shake\"\nversion = \"0.1.1\"\nedition = \"2021\"\n",
    )
    .unwrap();
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

/// The mpe-qa-tool shape: a generic root (README only), `plugin/Cargo.toml` at 0.2.0,
/// tagged v0.2.0, one commit past the tag.
fn generic_root_with_plugin(tmp: &Path) -> PathBuf {
    let work = tmp.join("mq");
    fs::create_dir_all(work.join("plugin/src")).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "user.name", "t"]);
    fs::write(work.join("README.md"), "readme\n").unwrap();
    fs::write(work.join("plugin/src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        work.join("plugin/Cargo.toml"),
        "[package]\nname = \"mq\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-qm", "init"]);
    git(&work, &["tag", "-a", "v0.2.0", "-m", "v0.2.0"]);
    fs::write(work.join("README.md"), "readme\nmore\n").unwrap();
    git(&work, &["commit", "-qam", "more"]);
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

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn notice(dir: &Path, prefix: &str, root: &Path) -> String {
    format!(
        "bump: {} is {prefix} inside {}; running on {}",
        dir.display(),
        root.display(),
        root.display()
    )
}

/// Stderr with the subdirectory notice line removed: what is left must match the root run.
fn stderr_after_notice(o: &Output, notice: &str) -> String {
    let err = stderr(o);
    let first = err.lines().next().unwrap_or_default();
    assert_eq!(first, notice, "the notice is the first stderr line; stderr: {err}");
    err.lines().skip(1).map(|l| format!("{l}\n")).collect()
}

/// Run `args` at the root and at `root/src`: the subdir run prints the notice first, then
/// exactly what the root run prints, with the same exit code.
fn assert_subdir_matches_root(root: &Path, args: &[&str]) -> Output {
    let src = root.join("src");
    let at_root = bump(root, args);
    let at_src = bump(&src, args);
    assert!(
        !stderr(&at_root).contains(" inside "),
        "no notice at the root; stderr: {}",
        stderr(&at_root)
    );
    assert_eq!(
        stderr_after_notice(&at_src, &notice(&src, "src", root)),
        stderr(&at_root),
        "{args:?}: stderr after the notice"
    );
    assert_eq!(stdout(&at_src), stdout(&at_root), "{args:?}: stdout");
    assert_eq!(at_src.status.code(), at_root.status.code(), "{args:?}: exit code");
    at_src
}

#[test]
fn plain_bump_from_a_subdir_runs_on_the_root() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let out = assert_subdir_matches_root(&root, &["-n", "--no-verify"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("bump: 0.1.1 -> 0.1.2"), "stdout: {so}");
    assert!(so.contains("Would update: Cargo.toml"), "stdout: {so}");
}

#[test]
fn gates_from_a_subdir_runs_on_the_root() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let out = assert_subdir_matches_root(&root, &["--gates"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("Gates:  none (ungated)"),
        "stdout: {}",
        stdout(&out)
    );
}

/// `--tag-only` reads the root manifest: from `src` it tags v0.1.2 where the old code
/// read `src` as generic and refused for want of a version.
#[test]
fn tag_only_from_a_subdir_tags_the_root_version() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"shake\"\nversion = \"0.1.2\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&root, &["commit", "-qam", "v0.1.2"]);
    git(&root, &["push", "-q", "origin", "main"]);

    let src = root.join("src");
    let out = bump(&src, &["--tag-only"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out).lines().next().unwrap_or_default(),
        notice(&src, "src", &root)
    );
    assert!(
        stdout(&out).contains("Tagged v0.1.2 on merged main"),
        "stdout: {}",
        stdout(&out)
    );
    let tagged = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "refs/tags/v0.1.2"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(tagged.status.success(), "v0.1.2 was created");
}

/// The design doc's release criterion: `nothing to release`, exit 1, same as the root
/// (the old code took the pushed untagged HEAD as a generic resume and exited 0).
#[test]
fn release_from_a_subdir_runs_on_the_root() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let out = assert_subdir_matches_root(&root, &["release", "-n", "--no-ci-gate"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("nothing to release"), "stderr: {}", stderr(&out));
}

/// `released_repo` is the missed-bump shape (origin/main one commit past v0.1.1, version
/// unchanged): the state-aware `finish -n` refuses exactly as the real run does, from the
/// subdir and the root alike.
#[test]
fn finish_from_a_subdir_runs_on_the_root() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let out = assert_subdir_matches_root(&root, &["finish", "-n", "--no-ci-gate", "--no-install"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("no untagged version on main"),
        "stderr: {}",
        stderr(&out)
    );
}

/// `release <subdir>` from outside the repo resolves the `[DIR]` the same way.
#[test]
fn release_dir_positional_naming_a_subdir_runs_on_the_root() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let src = root.join("src");
    let elsewhere = TempDir::new().unwrap();
    let args = ["release", "-n", "--no-ci-gate"];
    let at_root = bump(&root, &args);
    let via_dir = bump(
        elsewhere.path(),
        &["release", "-n", "--no-ci-gate", src.to_str().unwrap()],
    );
    assert_eq!(
        stderr_after_notice(&via_dir, &notice(&src, "src", &root)),
        stderr(&at_root)
    );
    assert_eq!(stdout(&via_dir), stdout(&at_root));
    assert_eq!(via_dir.status.code(), Some(1));
}

#[test]
fn a_subdir_with_its_own_manifest_names_it_in_the_notice() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let tools = root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(
        tools.join("Cargo.toml"),
        "[package]\nname = \"tools\"\nversion = \"7.0.0\"\n",
    )
    .unwrap();

    let out = bump(&tools, &["-n", "--no-verify"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        stderr(&out).lines().next().unwrap_or_default(),
        format!("{} (ignoring Cargo.toml in tools)", notice(&tools, "tools", &root))
    );
    assert!(
        stdout(&out).contains("bump: 0.1.1 -> 0.1.2"),
        "the root version, not 7.0.0"
    );
}

#[test]
fn a_generic_root_refuses_a_subdir_holding_a_manifest_from_every_entry_point() {
    let tmp = TempDir::new().unwrap();
    let root = generic_root_with_plugin(tmp.path());
    let plugin = root.join("plugin");
    let refusal = format!(
        "bump: {} is plugin inside {}, which has no version manifest, but plugin/Cargo.toml does;",
        plugin.display(),
        root.display()
    );
    for args in [
        vec!["-n", "--no-verify"],
        vec!["--gates"],
        vec!["--tag-only"],
        vec!["release", "-n", "--no-ci-gate"],
        vec!["finish", "-n", "--no-ci-gate"],
    ] {
        let out = bump(&plugin, &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: stderr: {}", stderr(&out));
        assert!(stderr(&out).contains(&refusal), "{args:?}: stderr: {}", stderr(&out));
        assert!(
            stderr(&out).contains("will not tag a version no root file holds"),
            "{args:?}: stderr: {}",
            stderr(&out)
        );
        assert!(
            stdout(&out).is_empty(),
            "{args:?}: nothing runs; stdout: {}",
            stdout(&out)
        );
    }
    let tags = Command::new("git")
        .args(["tag", "-l"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&tags.stdout), "v0.2.0\n", "no tag was created");
}

#[test]
fn a_root_and_its_subdir_as_arguments_run_once() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let src = root.join("src");
    let out = bump(
        tmp.path(),
        &["-n", "--no-verify", root.to_str().unwrap(), src.to_str().unwrap()],
    );
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert_eq!(so.matches("bump: 0.1.1 -> 0.1.2").count(), 1, "one run; stdout: {so}");
    assert!(
        !so.contains("All done!"),
        "a single run has no batch summary; stdout: {so}"
    );
}

#[test]
fn a_symlinked_path_to_a_root_gets_no_notice() {
    let tmp = TempDir::new().unwrap();
    let root = released_repo(tmp.path());
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let out = bump(tmp.path(), &["-n", "--no-verify", link.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(!stderr(&out).contains(" inside "), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("bump: 0.1.1 -> 0.1.2"),
        "stdout: {}",
        stdout(&out)
    );
}

#[test]
fn a_non_repo_dir_keeps_the_usual_error() {
    let plain = TempDir::new().unwrap();
    let out = bump(plain.path(), &["-n", "--no-verify"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        format!("Error: Not a git repository: {}\n", plain.path().display())
    );

    let out = bump(plain.path(), &["release", "-n"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("not a git repository: {}", plain.path().display())),
        "stderr: {}",
        stderr(&out)
    );
}
