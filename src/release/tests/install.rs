//! Install resolution (pure): precedence override > config > default-if-installable-Cargo > skip,
//! plus the post-install `--version` match that proves the installed binary is the release.

use super::*;

#[test]
fn resolve_install_explicit_override_wins() {
    let tmp = TempDir::new().unwrap();
    let config = Config {
        install: Some("from-config".to_string()),
        ..Config::default()
    };
    let choice = InstallChoice::Command("explicit".to_string());
    assert_eq!(
        resolve_install(tmp.path(), &choice, &config).as_deref(),
        Some("explicit")
    );
}

#[test]
fn resolve_install_skip_is_none() {
    let tmp = TempDir::new().unwrap();
    let config = Config {
        install: Some("from-config".to_string()),
        ..Config::default()
    };
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Skip, &config), None);
}

#[test]
fn resolve_install_auto_prefers_config() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0"); // Cargo present, but config wins
    let config = Config {
        install: Some("make install".to_string()),
        ..Config::default()
    };
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("make install")
    );
}

#[test]
fn resolve_install_auto_defaults_to_cargo_when_cargo_present() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    let config = Config::default();
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("cargo install --path .")
    );
}

#[test]
fn resolve_install_auto_skips_when_no_manifest_and_no_config() {
    let tmp = TempDir::new().unwrap();
    let config = Config::default();
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Auto, &config), None);
}

/// A virtual workspace root (`[workspace]`, no `[package]`, e.g. `tatari-tv/marquee`) has
/// no default install: `cargo install --path .` fails on it, which would fail a release
/// after the tag is already pushed.
#[test]
fn resolve_install_auto_skips_on_virtual_workspace_root() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"cli\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    let config = Config::default();
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Auto, &config), None);
}

/// Explicit override and config `install:` still work on a virtual workspace root --
/// only the bare default is withheld.
#[test]
fn resolve_install_explicit_override_works_on_virtual_workspace_root() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = [\"cli\"]\n").unwrap();
    let choice = InstallChoice::Command("cargo install --path cli".to_string());
    let config = Config::default();
    assert_eq!(
        resolve_install(tmp.path(), &choice, &config).as_deref(),
        Some("cargo install --path cli")
    );
}

#[test]
fn install_skip_reason_names_the_virtual_workspace_root() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = [\"cli\"]\n").unwrap();
    assert_eq!(
        install_skip_reason(tmp.path(), &InstallChoice::Auto),
        "skipped (virtual workspace root; pass --install or set install: in bump.yml)"
    );
}

#[test]
fn install_skip_reason_is_plain_with_no_cargo_manifest() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(install_skip_reason(tmp.path(), &InstallChoice::Auto), "skipped");
}

#[test]
fn install_skip_reason_is_plain_on_no_install_flag_even_on_virtual_workspace_root() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = [\"cli\"]\n").unwrap();
    assert_eq!(install_skip_reason(tmp.path(), &InstallChoice::Skip), "skipped");
}

/// A library crate (`[package]`, `src/lib.rs`, no binary target, e.g. `tatari-tv/renew`)
/// has no default install: `cargo install --path .` fails on it with `no packages found
/// with binaries`, after the tag is already pushed.
#[test]
fn resolve_install_auto_skips_on_library_crate() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
    let config = Config::default();
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Auto, &config), None);
    assert_eq!(
        install_skip_reason(tmp.path(), &InstallChoice::Auto),
        "skipped (library crate, no binary target; pass --install or set install: in bump.yml)"
    );
}

/// A service (`[package]`, `src/main.rs`, no clap, e.g. `tatari-tv/valet`) has no default
/// install and says why; `--install` and config `install:` still fire on it.
#[test]
fn resolve_install_auto_skips_on_service_without_clap() {
    let tmp = TempDir::new().unwrap();
    write_service_cargo(tmp.path(), "1.0.0");
    let config = Config::default();
    assert_eq!(resolve_install(tmp.path(), &InstallChoice::Auto, &config), None);
    assert_eq!(
        install_skip_reason(tmp.path(), &InstallChoice::Auto),
        "skipped (no clap dependency: not a CLI; pass --install or set install: in bump.yml)"
    );
}

#[test]
fn resolve_install_auto_still_defaults_on_clap_cli() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &Config::default()).as_deref(),
        Some("cargo install --path .")
    );
}

#[test]
fn resolve_install_explicit_and_config_still_fire_on_service() {
    let tmp = TempDir::new().unwrap();
    write_service_cargo(tmp.path(), "1.0.0");
    let choice = InstallChoice::Command("cargo install --path .".to_string());
    assert_eq!(
        resolve_install(tmp.path(), &choice, &Config::default()).as_deref(),
        Some("cargo install --path .")
    );
    let config = Config {
        install: Some("make install".to_string()),
        ..Config::default()
    };
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("make install")
    );
}

/// Config `install:` still fires on a library crate -- only the bare default is withheld.
#[test]
fn resolve_install_config_works_on_library_crate() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    let config = Config {
        install: Some("cargo install --path cli".to_string()),
        ..Config::default()
    };
    assert_eq!(
        resolve_install(tmp.path(), &InstallChoice::Auto, &config).as_deref(),
        Some("cargo install --path cli")
    );
}

/// The verification is token equality against the tag: the build-version contract prints
/// `<name> vX.Y.Z` on the tagged commit and nothing else passes.
#[test]
fn version_matches_requires_the_exact_tag_token() {
    assert!(version_matches("bump v0.4.6", "v0.4.6"));
    assert!(version_matches("v0.4.6", "v0.4.6"));
    assert!(
        !version_matches("bump v0.4.6-3-gabc1234", "v0.4.6"),
        "built past the tag"
    );
    assert!(!version_matches("bump 0.4.6", "v0.4.6"), "CARGO_PKG_VERSION, no v");
    assert!(!version_matches("bump v0.4.5", "v0.4.6"), "stale binary");
    assert!(!version_matches("", "v0.4.6"));
}

/// A hint only where the output's shape proves the cause. The old "an older binary may
/// sit earlier on PATH" guess is gone: a plain mismatch gets no hint, since the fact line
/// already says which path ran.
#[test]
fn version_mismatch_hint_names_only_a_proven_shape() {
    assert!(
        version_mismatch_hint("bump v0.4.6-3-gabc1234", "v0.4.6")
            .unwrap()
            .contains("past the tag")
    );
    assert!(
        version_mismatch_hint("bump 0.4.6", "v0.4.6")
            .unwrap()
            .contains("CARGO_PKG_VERSION")
    );
    assert_eq!(version_mismatch_hint("bump v0.4.5", "v0.4.6"), None);
    assert_eq!(
        version_mismatch_hint("bump v0.4.60", "v0.4.6"),
        None,
        "a longer version is not a build past the tag"
    );
}

/// The mismatch message: what printed, the fact line naming the path and its source, the
/// hint when there is one, and the shared re-run line last.
#[test]
fn version_mismatch_message_states_the_fact_line() {
    let message = version_mismatch_message(Path::new("tool"), BinSource::Path, "tool v1.2.2", "v1.2.3");
    assert!(
        message.contains("printed \"tool v1.2.2\", expected v1.2.3."),
        "{message}"
    );
    assert!(message.contains("verify ran tool (from PATH)"), "{message}");
    assert!(!message.contains("older binary"), "no guessing: {message}");
    assert!(message.ends_with(VERIFY_RERUN), "{message}");
}

/// `installed_bin_path` prefers the install root's `bin/` when the file is there and falls
/// back to the bare name (PATH) otherwise, including when there is no root at all, and
/// says which.
#[test]
fn installed_bin_path_prefers_install_root_then_path() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("bin")).unwrap();
    fs::write(tmp.path().join("bin/tool"), "").unwrap();
    assert_eq!(
        installed_bin_path(Some(tmp.path()), "tool"),
        (tmp.path().join("bin/tool"), BinSource::InstallRoot)
    );
    assert_eq!(
        installed_bin_path(Some(tmp.path()), "other"),
        (std::path::PathBuf::from("other"), BinSource::Path)
    );
    assert_eq!(
        installed_bin_path(None, "tool"),
        (std::path::PathBuf::from("tool"), BinSource::Path)
    );
}

/// A private install root holding a fake `tool` whose body is `script`.
///
/// The binary is written by a short-lived child (`install`), never by this process: a
/// parallel test that forks while this process holds a write handle inherits it until its
/// exec, and the later exec of the script fails `Text file busy`.
fn fake_install_root(script: &str) -> TempDir {
    let root = TempDir::new().unwrap();
    let bin_dir = root.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let staged = root.path().join("tool.src");
    fs::write(&staged, format!("#!/bin/sh\n{script}\n")).unwrap();
    let status = std::process::Command::new("install")
        .args(["-m", "755"])
        .arg(&staged)
        .arg(bin_dir.join("tool"))
        .status()
        .unwrap();
    assert!(status.success(), "install -m 755 failed: {status}");
    root
}

fn shell_installer(root: &TempDir, timeout: std::time::Duration) -> ShellInstaller {
    ShellInstaller {
        install_root: Some(root.path().to_path_buf()),
        verify_timeout: timeout,
    }
}

const TEN_SECONDS: std::time::Duration = std::time::Duration::from_secs(10);

/// The production verify against a binary in the install root: the tag token passes, a
/// build past the tag and a bare `X.Y.Z` fail with the shape and the fact line named.
#[test]
fn shell_installer_verify_runs_the_installed_binary() {
    let dir = Path::new("/repo");

    let root = fake_install_root("echo 'tool v1.2.3'");
    shell_installer(&root, TEN_SECONDS)
        .verify(dir, "tool", "v1.2.3")
        .expect("exact tag passes");

    let root = fake_install_root("echo 'tool v1.2.3-2-gdeadbee'");
    let err = shell_installer(&root, TEN_SECONDS)
        .verify(dir, "tool", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(err.contains("past the tag"), "{err}");
    assert!(err.contains("(from the install root)"), "fact line: {err}");
    assert!(err.ends_with(VERIFY_RERUN), "{err}");

    let root = fake_install_root("echo 'tool 1.2.3'");
    let err = shell_installer(&root, TEN_SECONDS)
        .verify(dir, "tool", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(err.contains("CARGO_PKG_VERSION"), "{err}");
    assert!(err.contains("bump finish"), "names the reinstall: {err}");
    assert!(err.contains("bump release"), "names the new release: {err}");

    let root = fake_install_root("echo 'tool v1.2.2'");
    let err = shell_installer(&root, TEN_SECONDS)
        .verify(dir, "tool", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(err.contains("verify ran "), "{err}");
    assert!(
        !err.contains("past the tag") && !err.contains("CARGO_PKG_VERSION"),
        "{err}"
    );
    assert!(err.ends_with(VERIFY_RERUN), "{err}");
}

/// A binary that ignores `--version` (a server) is killed at the timeout and verify fails;
/// the forked `sleep` keeps the output files open past the kill, and the deadline still
/// holds because nothing reads a pipe.
#[test]
fn kills_a_binary_that_does_not_exit() {
    let root = fake_install_root("echo starting server\nsleep 5");
    let started = std::time::Instant::now();
    let err = shell_installer(&root, std::time::Duration::from_millis(200))
        .verify(Path::new("/repo"), "tool", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the timeout is hard: {:?}",
        started.elapsed()
    );
    assert!(err.contains("did not exit within 200ms"), "{err}");
    assert!(err.contains("starting server"), "carries the output: {err}");
    assert!(err.ends_with(VERIFY_RERUN), "{err}");
}

/// A non-zero exit fails verify with the status and the stderr text.
#[test]
fn shell_installer_verify_reports_a_non_zero_exit() {
    let root = fake_install_root("echo 'missing VALET_STATE_SIGNING_KEY' >&2\nexit 3");
    let err = shell_installer(&root, TEN_SECONDS)
        .verify(Path::new("/repo"), "tool", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(err.contains("exited"), "{err}");
    assert!(err.contains("3"), "{err}");
    assert!(err.contains("missing VALET_STATE_SIGNING_KEY"), "{err}");
    assert!(err.ends_with(VERIFY_RERUN), "{err}");
}

/// A binary that cannot be spawned fails verify with the same closing line.
#[test]
fn shell_installer_verify_reports_a_spawn_failure() {
    let root = TempDir::new().unwrap();
    let err = shell_installer(&root, TEN_SECONDS)
        .verify(Path::new("/repo"), "bump-no-such-binary-anywhere", "v1.2.3")
        .unwrap_err()
        .to_string();
    assert!(err.contains("could not run"), "{err}");
    assert!(err.ends_with(VERIFY_RERUN), "{err}");
}

// ---- the verify target ---------------------------------------------------------------

/// clyde's shape: a virtual root with `[workspace.package] version`, and a member `clyde/`
/// inheriting it, with clap and `src/main.rs`.
fn write_clyde_shaped(dir: &Path, version: &str) {
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = [\"clyde\"]\nresolver = \"2\"\n\n[workspace.package]\nversion = \"{version}\"\n"
        ),
    )
    .unwrap();
    let member = dir.join("clyde");
    fs::create_dir_all(member.join("src")).unwrap();
    fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"clyde\"\nversion.workspace = true\n\n[dependencies]\nclap = \"4\"\n",
    )
    .unwrap();
    fs::write(member.join("src/main.rs"), "fn main() {}\n").unwrap();
}

fn config_install(command: &str) -> Config {
    Config {
        install: Some(command.to_string()),
        ..Config::default()
    }
}

/// `install: cargo install --locked --path clyde` verifies the member at `<work>/clyde`.
#[test]
fn verify_follows_the_install_path_into_a_member() {
    let tmp = TempDir::new().unwrap();
    write_clyde_shaped(tmp.path(), "1.2.3");
    let installer = RecordingInstaller::new();
    let config = config_install("cargo install --locked --path clyde");
    run_install(tmp.path(), &InstallChoice::Auto, &config, "v1.2.3", &installer).unwrap();
    assert_eq!(
        installer.verified(),
        vec![verified_entry(&tmp.path().join("clyde"), "clyde", "v1.2.3")]
    );
    let config = config_install("cargo install --path=clyde");
    assert_eq!(
        verify_target(tmp.path(), config.install.as_deref().unwrap()),
        VerifyTarget::Verify {
            dir: tmp.path().join("clyde"),
            bin: "clyde".to_string()
        }
    );
}

/// The same virtual root installed with no `--path` skips on the binary name, not on clap:
/// a virtual root has neither, and "no binary name" is what is true of it.
#[test]
fn verify_skips_a_virtual_root_on_the_binary_name() {
    let tmp = TempDir::new().unwrap();
    write_clyde_shaped(tmp.path(), "1.2.3");
    let installer = RecordingInstaller::new();
    run_install(
        tmp.path(),
        &InstallChoice::Auto,
        &config_install("cargo install"),
        "v1.2.3",
        &installer,
    )
    .unwrap();
    assert!(installer.verified().is_empty());
    assert_eq!(
        verify_target(tmp.path(), "cargo install"),
        VerifyTarget::Skip(format!("no binary name in {}", tmp.path().join("Cargo.toml").display()))
    );
}

#[test]
fn verify_skips_a_command_that_is_not_cargo_install() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    let installer = RecordingInstaller::new();
    run_install(
        tmp.path(),
        &InstallChoice::Auto,
        &config_install("make install"),
        "v1.0.0",
        &installer,
    )
    .unwrap();
    assert_eq!(
        installer.calls(),
        vec!["make install".to_string()],
        "the install still runs"
    );
    assert!(installer.verified().is_empty());
    assert_eq!(
        verify_target(tmp.path(), "make install"),
        VerifyTarget::Skip("install command is not cargo install".to_string())
    );
}

#[test]
fn verify_skips_a_cargo_install_of_another_crate() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    let installer = RecordingInstaller::new();
    let choice = InstallChoice::Command("cargo install ripgrep".to_string());
    run_install(tmp.path(), &choice, &Config::default(), "v1.0.0", &installer).unwrap();
    assert!(installer.verified().is_empty());
    assert_eq!(
        verify_target(tmp.path(), "cargo install ripgrep"),
        VerifyTarget::Skip("install command does not install this repo".to_string())
    );
}

/// A server installed by an explicit command installs, but verify skips on clap: running
/// `<server> --version` would start it.
#[test]
fn verify_skips_a_service_installed_explicitly() {
    let tmp = TempDir::new().unwrap();
    write_service_cargo(tmp.path(), "1.0.0");
    let installer = RecordingInstaller::new();
    let choice = InstallChoice::Command("cargo install --path .".to_string());
    run_install(tmp.path(), &choice, &Config::default(), "v1.0.0", &installer).unwrap();
    assert_eq!(installer.calls(), vec!["cargo install --path .".to_string()]);
    assert!(installer.verified().is_empty());
    assert_eq!(
        verify_target(tmp.path(), "cargo install --path ."),
        VerifyTarget::Skip(format!(
            "no clap dependency in {}",
            tmp.path().join("Cargo.toml").display()
        ))
    );
}

/// The default install on a clap CLI verifies the repo root itself, not `<root>/.`.
#[test]
fn verify_default_install_targets_the_repo_root() {
    let tmp = TempDir::new().unwrap();
    write_cargo(tmp.path(), "1.0.0");
    let installer = RecordingInstaller::new();
    run_install(
        tmp.path(),
        &InstallChoice::Auto,
        &Config::default(),
        "v1.0.0",
        &installer,
    )
    .unwrap();
    assert_eq!(
        installer.verified(),
        vec![verified_entry(tmp.path(), "test-pkg", "v1.0.0")]
    );
}

/// The three skip reasons the shakedown-fixes doc added, each printed verbatim, and an
/// env-prefixed `cargo install` of this repo verifies.
#[test]
fn verify_target_new_skip_reasons_and_env_prefix() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let other_dir = tmp.path().join("other");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&other_dir).unwrap();
    write_cargo(&repo, "1.0.0");
    write_cargo(&other_dir, "9.9.9");
    let skip = |reason: &str| VerifyTarget::Skip(reason.to_string());
    assert_eq!(
        verify_target(&repo, "cargo install --path . && echo done"),
        skip("install command uses shell syntax; verify reads one plain cargo install")
    );
    assert_eq!(
        verify_target(&repo, "CARGO_INSTALL_ROOT=/x cargo install --path ."),
        skip("install command sets its own install root")
    );
    let other = other_dir.canonicalize().unwrap();
    assert_eq!(
        verify_target(&repo, "cargo install --path ../other"),
        skip(&format!("install target {} is outside this repo", other.display()))
    );
    assert_eq!(
        verify_target(&repo, "RUSTFLAGS=-g cargo install --path ."),
        VerifyTarget::Verify {
            dir: repo.clone(),
            bin: "test-pkg".to_string()
        }
    );
}
