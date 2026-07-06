use super::*;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// A real-world-style package.json: 2-space indent, a top-level `version`, AND a
/// nested dependency `version` at a deeper indent (proving first-match is unsafe).
const REAL_WORLD: &str = r#"{
  "name": "example",
  "version": "1.2.3",
  "description": "a package",
  "dependencies": {
    "left-pad": {
      "version": "1.3.0"
    }
  }
}
"#;

#[test]
fn read_version_reads_top_level_only() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("package.json");
    fs::write(&p, REAL_WORLD).unwrap();
    // The authoritative version is the top-level one, never the nested dependency's.
    assert_eq!(read_version(&p).unwrap().as_deref(), Some("1.2.3"));
}

#[test]
fn read_version_missing_returns_none() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("package.json");
    fs::write(&p, "{\n  \"name\": \"x\"\n}\n").unwrap();
    assert_eq!(
        read_version(&p).unwrap(),
        None,
        "no top-level version -> None (writable-missing)"
    );
}

#[test]
fn write_version_targeted_one_line_diff_with_nested_version() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("package.json");
    fs::write(&p, REAL_WORLD).unwrap();

    write_version(&p, "1.2.4").unwrap();

    let after = fs::read_to_string(&p).unwrap();
    // Byte-exact everywhere except the single top-level version line.
    let expected = REAL_WORLD.replacen("\"version\": \"1.2.3\"", "\"version\": \"1.2.4\"", 1);
    assert_eq!(after, expected, "only the top-level version line may change");
    assert!(
        after.contains("\"version\": \"1.3.0\""),
        "nested dependency version must be untouched"
    );

    // Exactly one line differs from the original.
    let diff_lines = REAL_WORLD.lines().zip(after.lines()).filter(|(a, b)| a != b).count();
    assert_eq!(diff_lines, 1, "exactly one line changes");
}

#[test]
fn write_version_anchors_to_shallowest_not_first() {
    // A nested "version" appears FIRST in file order but at a deeper indent; the
    // top-level "version" is later but shallowest. The edit MUST target the top-level.
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("package.json");
    let json = "{\n  \"packages\": {\n    \"dep\": {\n      \"version\": \"9.9.9\"\n    }\n  },\n  \"version\": \"1.0.0\"\n}\n";
    fs::write(&p, json).unwrap();

    write_version(&p, "1.0.1").unwrap();

    let after = fs::read_to_string(&p).unwrap();
    assert!(
        after.contains("\"version\": \"9.9.9\""),
        "nested (deeper-indent) version untouched"
    );
    assert!(
        after.contains("\"version\": \"1.0.1\""),
        "top-level (shallowest) version bumped"
    );
    assert!(
        !after.contains("\"version\": \"1.0.0\""),
        "old top-level version must be gone"
    );
}

#[test]
fn write_version_missing_field_is_loud_error() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("package.json");
    fs::write(&p, "{\n  \"name\": \"x\"\n}\n").unwrap();
    let err = write_version(&p, "1.0.0").unwrap_err().to_string();
    assert!(err.contains("no top-level \"version\""), "got: {err}");
}

#[test]
fn sync_lockfile_no_lock_is_noop() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("package.json"), REAL_WORLD).unwrap();
    // No package-lock.json (pnpm/yarn need no sync) -> empty, never shells out even to
    // a nonexistent binary.
    let touched = sync_lockfile_with(dir.path(), "npm-does-not-exist-bump-test").unwrap();
    assert!(touched.is_empty());
}

#[test]
fn sync_lockfile_npm_missing_with_lock_is_loud_error() {
    // package-lock.json present but npm absent -> loud error, never a silently stale
    // lock. A guaranteed-nonexistent binary name simulates NotFound deterministically
    // without touching PATH.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("package.json"), REAL_WORLD).unwrap();
    fs::write(dir.path().join("package-lock.json"), "{\n  \"version\": \"1.2.3\"\n}\n").unwrap();

    let err = sync_lockfile_with(dir.path(), "npm-does-not-exist-bump-test")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("package-lock.json is present") && err.contains("was not found"),
        "expected a loud npm-missing error; got: {err}"
    );
}

/// GATED real round-trip: requires a working `npm` with registry access AND a writable
/// `~/.npm` cache. Skips (returns, never fails) when npm is absent OR when
/// `npm install --package-lock-only` errors (sandbox EROFS / offline), so the default
/// sandboxed `otto ci` stays green. Only truly runs where npm works.
#[test]
fn npm_round_trip_updates_both_lock_sites() {
    if Command::new("npm")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping npm_round_trip_updates_both_lock_sites: `npm` not available");
        return;
    }

    let dir = TempDir::new().unwrap();
    let pkg = dir.path().join("package.json");
    fs::write(
        &pkg,
        "{\n  \"name\": \"bump-node-fixture\",\n  \"version\": \"0.1.0\"\n}\n",
    )
    .unwrap();

    // Establish the initial lock. Skip if npm can't write it (EROFS / offline).
    let init = Command::new("npm")
        .args(["install", "--package-lock-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    if !init.status.success() {
        eprintln!(
            "skipping npm_round_trip_updates_both_lock_sites: initial npm install failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        return;
    }
    if !dir.path().join("package-lock.json").exists() {
        eprintln!("skipping npm_round_trip_updates_both_lock_sites: no package-lock.json produced");
        return;
    }

    // Bump package.json, then sync the lock via the code under test.
    write_version(&pkg, "0.1.1").unwrap();
    let touched = sync_lockfile(dir.path()).unwrap();
    assert!(touched.iter().any(|p| p.ends_with("package-lock.json")));

    let lock = fs::read_to_string(dir.path().join("package-lock.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&lock).unwrap();
    // BOTH root version sites updated: top-level `version` + `packages[""].version`.
    assert_eq!(
        v.get("version").and_then(|x| x.as_str()),
        Some("0.1.1"),
        "top-level package-lock.json version must update"
    );
    assert_eq!(
        v.get("packages")
            .and_then(|p| p.get(""))
            .and_then(|r| r.get("version"))
            .and_then(|x| x.as_str()),
        Some("0.1.1"),
        "packages[\"\"] version must update"
    );
}
