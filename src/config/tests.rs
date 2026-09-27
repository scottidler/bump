use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn load_missing_file_returns_defaults() {
    let tmp = TempDir::new().unwrap();

    let config = load(tmp.path()).unwrap();

    assert!(config.skip_members.is_empty());
    assert!(config.install.is_none());
}

#[test]
fn load_parses_skip_members_and_install() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join(CONFIG_FILE_NAME),
        "skip-members:\n  - claude-pricing\ninstall: cargo install --path .\n",
    )
    .unwrap();

    let config = load(tmp.path()).unwrap();

    assert_eq!(config.skip_members, vec!["claude-pricing".to_string()]);
    assert_eq!(config.install, Some("cargo install --path .".to_string()));
}

#[test]
fn load_empty_file_returns_defaults() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "").unwrap();

    // An empty bump.yml deserializes as null, not a map; must still fall back to
    // all-defaults rather than erroring.
    let config = load(tmp.path()).unwrap();

    assert!(config.skip_members.is_empty());
    assert!(config.install.is_none());
}

/// An unknown key is a LOUD error that NAMES the offending key -- deny_unknown_fields
/// is the feature (rules/rust.md: "the error naming the unknown field IS the feature").
#[test]
fn load_unknown_key_errors_naming_the_key() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "flows:\n  - gated\n").unwrap();

    let err = load(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("flows"), "error must name the offending key: {err}");
}

/// A different unknown key still gets named -- proves this is deny_unknown_fields
/// naming the ACTUAL key, not a hardcoded string.
#[test]
fn load_different_unknown_key_names_that_key() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "foo: bar\n").unwrap();

    let err = load(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("foo"), "error must name the offending key: {err}");
}

#[test]
fn load_malformed_yaml_is_a_loud_error() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "skip-members: [unterminated\n").unwrap();

    assert!(load(tmp.path()).is_err());
}

#[test]
fn effective_skip_members_cli_flag_overrides_config() {
    let config = Config {
        skip_members: vec!["from-config".to_string()],
        install: None,
        ci: None,
    };

    let effective = effective_skip_members(&["from-cli".to_string()], &config);

    assert_eq!(effective, vec!["from-cli".to_string()]);
}

#[test]
fn effective_skip_members_falls_back_to_config_when_flag_absent() {
    let config = Config {
        skip_members: vec!["from-config".to_string()],
        install: None,
        ci: None,
    };

    let effective = effective_skip_members(&[], &config);

    assert_eq!(effective, vec!["from-config".to_string()]);
}

#[test]
fn load_parses_ci_none() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "ci: none\n").unwrap();

    let config = load(tmp.path()).unwrap();

    assert_eq!(config.ci, Some(CiDeclaration::None));
}

#[test]
fn load_ci_absent_is_none() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "install: cargo install --path .\n").unwrap();

    let config = load(tmp.path()).unwrap();

    assert_eq!(config.ci, None);
}

/// `ci` accepts exactly one value; anything else is a loud, serde-driven error --
/// same trust model as `deny_unknown_fields` on the rest of the file.
#[test]
fn load_ci_unknown_value_is_a_loud_error() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join(CONFIG_FILE_NAME), "ci: sometimes\n").unwrap();

    let err = load(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("sometimes"), "error must name the offending value: {err}");
}

#[test]
fn effective_skip_members_empty_when_both_absent() {
    let config = Config::default();

    let effective = effective_skip_members(&[], &config);

    assert!(effective.is_empty());
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_with_commit() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    git(d, &["init", "-b", "main"]);
    git(d, &["config", "user.email", "test@test.com"]);
    git(d, &["config", "user.name", "Test"]);
    fs::write(d.join("README.md"), "x").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "init"]);
    tmp
}

/// `load_at` reads the COMMITTED `bump.yml`; an untracked one on disk does not count.
#[test]
fn load_at_reads_committed_config_never_untracked() {
    let tmp = repo_with_commit();
    let d = tmp.path();
    fs::write(d.join(CONFIG_FILE_NAME), "ci: none\n").unwrap();
    assert_eq!(load_at(d, "HEAD").unwrap().ci, None, "untracked bump.yml is ignored");

    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "declare ci none"]);
    assert_eq!(load_at(d, "HEAD").unwrap().ci, Some(CiDeclaration::None));
}

#[test]
fn load_at_bad_committed_value_is_a_loud_error() {
    let tmp = repo_with_commit();
    let d = tmp.path();
    fs::write(d.join(CONFIG_FILE_NAME), "ci: sometimes\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-m", "bad ci"]);
    let err = load_at(d, "HEAD").unwrap_err().to_string();
    assert!(err.contains("sometimes"), "got: {err}");
}
