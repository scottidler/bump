//! Install resolution (pure): precedence override > config > default-if-Cargo > skip.

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
