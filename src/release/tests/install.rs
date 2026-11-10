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

#[test]
fn version_mismatch_hint_names_the_shape() {
    assert!(version_mismatch_hint("bump v0.4.6-3-gabc1234", "v0.4.6").contains("past the tag"));
    assert!(version_mismatch_hint("bump 0.4.6", "v0.4.6").contains("CARGO_PKG_VERSION"));
    assert!(version_mismatch_hint("bump v0.4.5", "v0.4.6").contains("older binary"));
}

/// `installed_bin_path` prefers the install root's `bin/` when the file is there and falls
/// back to the bare name (PATH) otherwise, including when there is no root at all.
#[test]
fn installed_bin_path_prefers_install_root_then_path() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("bin")).unwrap();
    fs::write(tmp.path().join("bin/tool"), "").unwrap();
    assert_eq!(
        installed_bin_path(Some(tmp.path()), "tool"),
        tmp.path().join("bin/tool")
    );
    assert_eq!(
        installed_bin_path(Some(tmp.path()), "other"),
        std::path::PathBuf::from("other")
    );
    assert_eq!(installed_bin_path(None, "tool"), std::path::PathBuf::from("tool"));
}

/// The production verify against a binary in the install root: the tag token passes, a
/// build past the tag and a bare `X.Y.Z` fail with the shape named in the error.
#[test]
fn shell_installer_verify_runs_the_installed_binary() {
    let tmp = TempDir::new().unwrap();
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let write_fake = |prints: &str| {
        let path = bin_dir.join("tool");
        fs::write(&path, format!("#!/bin/sh\necho 'tool {prints}'\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&path, perms).unwrap();
    };
    let installer = ShellInstaller {
        install_root: Some(tmp.path().to_path_buf()),
    };

    write_fake("v1.2.3");
    installer.verify("tool", "v1.2.3").expect("exact tag passes");

    write_fake("v1.2.3-2-gdeadbee");
    let err = installer.verify("tool", "v1.2.3").unwrap_err().to_string();
    assert!(err.contains("past the tag"), "{err}");

    write_fake("1.2.3");
    let err = installer.verify("tool", "v1.2.3").unwrap_err().to_string();
    assert!(err.contains("CARGO_PKG_VERSION"), "{err}");
    assert!(err.contains("bump finish"), "names the re-run: {err}");
}
