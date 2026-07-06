use super::{Manifest, ManifestVersion};
use eyre::{Context, ContextCompat, Result, bail};
use log::{debug, error};
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, Item, Value};

/// Check if pyproject.toml exists at the given path
pub fn pyproject_toml_exists(dir: &Path) -> bool {
    dir.join("pyproject.toml").exists()
}

/// True iff pyproject.toml exists AND carries a version-bearing section (`[project]`
/// or `[tool.poetry]`). A ruff-config-only pyproject (no `[project]`/`[tool.poetry]`)
/// is NOT version-bearing and must not trigger the Python adapter. A dynamic-version
/// `[project]` IS version-bearing (so bump can refuse it, not ignore it).
pub fn is_version_bearing(dir: &Path) -> bool {
    if !pyproject_toml_exists(dir) {
        return false;
    }
    let path = pyproject_toml_path(dir);
    let Ok(content) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(doc) = content.parse::<DocumentMut>() else {
        return false;
    };
    let has_project = doc.get("project").is_some();
    let has_poetry = doc.get("tool").and_then(|t| t.get("poetry")).is_some();
    has_project || has_poetry
}

/// Get the path to pyproject.toml in the given directory
pub fn pyproject_toml_path(dir: &Path) -> std::path::PathBuf {
    dir.join("pyproject.toml")
}

/// Read the version from pyproject.toml
/// Checks [project].version (PEP 621) first, then [tool.poetry].version
/// Returns None if version field is missing or dynamic
pub fn read_version(pyproject_path: &Path) -> Result<Option<String>> {
    debug!("read_version: pyproject_path={}", pyproject_path.display());
    let content = fs::read_to_string(pyproject_path).context(format!("Failed to read {}", pyproject_path.display()))?;

    let doc = content
        .parse::<DocumentMut>()
        .context("Failed to parse pyproject.toml")?;

    // Check if version is declared dynamic (PEP 621). A dynamic version is owned
    // elsewhere (build backend / SCM plugin), so there is no static version to read.
    if has_dynamic_version(&doc) {
        return Ok(None);
    }

    // Try [project].version first (PEP 621 - modern standard)
    if let Some(project) = doc.get("project")
        && let Some(version) = project.get("version")
        && let Some(v) = version.as_str()
    {
        return Ok(Some(v.to_string()));
    }

    // Try [tool.poetry].version (Poetry)
    if let Some(tool) = doc.get("tool")
        && let Some(poetry) = tool.get("poetry")
        && let Some(version) = poetry.get("version")
        && let Some(v) = version.as_str()
    {
        return Ok(Some(v.to_string()));
    }

    Ok(None)
}

/// True if this pyproject declares its version dynamically (`dynamic = ["version"]`
/// under `[project]`). The version is then owned elsewhere (a build backend / SCM
/// plugin such as hatch-vcs or setuptools-scm), so a static `[project].version` MUST
/// NOT be written -- doing so produces PEP 621-invalid metadata (both a `version`
/// field and a `"version"` entry in `dynamic`).
fn has_dynamic_version(doc: &DocumentMut) -> bool {
    if let Some(project) = doc.get("project")
        && let Some(dynamic) = project.get("dynamic")
        && let Some(arr) = dynamic.as_array()
    {
        return arr.iter().any(|item| item.as_str() == Some("version"));
    }
    false
}

/// Determine which section holds the version: "project" or "tool.poetry"
fn version_section(doc: &DocumentMut) -> Option<&'static str> {
    // Prefer [project].version
    if let Some(project) = doc.get("project")
        && project.get("version").is_some()
    {
        return Some("project");
    }
    // Fall back to [tool.poetry].version
    if let Some(tool) = doc.get("tool")
        && let Some(poetry) = tool.get("poetry")
        && poetry.get("version").is_some()
    {
        return Some("tool.poetry");
    }
    // If neither has a version, default to [project] for new versions
    if doc.get("project").is_some() {
        return Some("project");
    }
    None
}

/// Update the version in pyproject.toml
/// Creates the version field if it doesn't exist
pub fn write_version(pyproject_path: &Path, new_version: &str) -> Result<()> {
    debug!(
        "write_version: pyproject_path={} new_version={}",
        pyproject_path.display(),
        new_version
    );
    let content = fs::read_to_string(pyproject_path).context(format!("Failed to read {}", pyproject_path.display()))?;

    let mut doc = content
        .parse::<DocumentMut>()
        .context("Failed to parse pyproject.toml")?;

    // Refuse (never corrupt) when the version is dynamic. `read_version` maps BOTH a
    // genuinely-missing-but-writable version AND a dynamic version to `None`; they must
    // diverge here at the write path -- dynamic REFUSES, plain-absent gets written.
    if has_dynamic_version(&doc) {
        error!(
            "write_version: refusing to write static version to dynamic pyproject at {}",
            pyproject_path.display()
        );
        bail!(
            "Cannot write a version to {}: it declares `dynamic = [\"version\"]`.\n\
             The version is owned elsewhere (a build backend / SCM plugin such as \
             hatch-vcs or setuptools-scm), so writing a static `[project].version` would \
             produce PEP 621-invalid metadata (a `version` field alongside a \"version\" \
             entry in `dynamic`).\n\
             For a dynamic-version project the git tag is the source of truth -- remove \
             \"version\" from `dynamic` and add a static `version` field if you want bump \
             to manage it.",
            pyproject_path.display()
        );
    }

    match version_section(&doc) {
        Some("project") => {
            let project = doc.get_mut("project").context("[project] section not found")?;

            if let Item::Table(table) = project {
                table["version"] = Item::Value(Value::from(new_version));
            } else {
                bail!("[project] is not a table");
            }
        }
        Some("tool.poetry") => {
            let tool = doc.get_mut("tool").context("[tool] section not found")?;
            if let Item::Table(tool_table) = tool {
                let poetry = tool_table
                    .get_mut("poetry")
                    .context("[tool.poetry] section not found")?;
                if let Item::Table(poetry_table) = poetry {
                    poetry_table["version"] = Item::Value(Value::from(new_version));
                } else {
                    bail!("[tool.poetry] is not a table");
                }
            } else {
                bail!("[tool] is not a table");
            }
        }
        _ => {
            // Create [project] section with version
            let project = doc.entry("project").or_insert(Item::Table(toml_edit::Table::new()));
            if let Item::Table(table) = project {
                table["version"] = Item::Value(Value::from(new_version));
            } else {
                bail!("[project] is not a table");
            }
        }
    }

    fs::write(pyproject_path, doc.to_string()).context(format!("Failed to write {}", pyproject_path.display()))?;

    Ok(())
}

/// Sync Python lockfiles after a version change.
///
/// - `uv.lock` records the root project's OWN version at exactly one site (Phase 0
///   spike, 2026-07-06), so a bump leaves it stale and `uv lock --check` /
///   `uv sync --locked` fail in CI. Run `uv lock` to true it up. The `uv` binary
///   absent while `uv.lock` is present is a LOUD error -- bump never leaves a
///   silently stale lock.
/// - `poetry.lock` does NOT record the root package version (verified), so it needs
///   no sync and is left untouched.
pub fn sync_lockfile(dir: &Path) -> Result<()> {
    sync_lockfile_with(dir, "uv")
}

/// Testable seam for `sync_lockfile`: `uv_bin` is the uv executable to invoke, so a
/// test can point it at a nonexistent binary to exercise the "uv missing but uv.lock
/// present" loud-error path deterministically (no PATH mutation).
fn sync_lockfile_with(dir: &Path, uv_bin: &str) -> Result<()> {
    debug!("sync_lockfile_with: dir={} uv_bin={}", dir.display(), uv_bin);

    let uv_lock = dir.join("uv.lock");
    if !uv_lock.exists() {
        debug!("sync_lockfile_with: no uv.lock, nothing to sync");
        return Ok(());
    }

    debug!("sync_lockfile_with: uv.lock present, running `{} lock`", uv_bin);
    let output = Command::new(uv_bin).arg("lock").current_dir(dir).output();

    match output {
        Ok(out) if out.status.success() => {
            debug!("sync_lockfile_with: `{} lock` succeeded", uv_bin);
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            error!("sync_lockfile_with: `{} lock` failed: {}", uv_bin, stderr);
            bail!(
                "`uv lock` failed while syncing uv.lock after the version bump:\n{}",
                stderr
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            error!("sync_lockfile_with: `{}` binary not found but uv.lock present", uv_bin);
            bail!(
                "uv.lock is present but the `uv` binary was not found on PATH.\n\
                 bump will not leave a stale lockfile (CI `uv lock --check` would fail).\n\
                 Install uv, or remove uv.lock if this project no longer uses uv."
            )
        }
        Err(e) => Err(e).context("Failed to run `uv lock`"),
    }
}

/// The Python manifest adapter (pyproject.toml, PEP 621 or Poetry, uv.lock sync).
pub struct PythonManifest {
    root: PathBuf,
}

impl PythonManifest {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Manifest for PythonManifest {
    fn path(&self) -> PathBuf {
        pyproject_toml_path(&self.root)
    }

    fn read_version(&self) -> Result<ManifestVersion> {
        // Distinguish dynamic (REFUSE) from missing (writable) -- the free
        // `read_version` maps BOTH to None, so re-parse to tell them apart here.
        let path = pyproject_toml_path(&self.root);
        let content = fs::read_to_string(&path).context(format!("Failed to read {}", path.display()))?;
        let doc = content
            .parse::<DocumentMut>()
            .context("Failed to parse pyproject.toml")?;
        if has_dynamic_version(&doc) {
            return Ok(ManifestVersion::Dynamic("dynamic = [\"version\"]".to_string()));
        }
        match read_version(&path)? {
            Some(s) => Ok(ManifestVersion::Static(crate::version::parse_version(&s)?)),
            None => Ok(ManifestVersion::Missing),
        }
    }

    fn write_version(&self, new_version: &Version) -> Result<()> {
        write_version(
            &pyproject_toml_path(&self.root),
            &crate::version::format_file_version(new_version),
        )
    }

    fn sync_lockfiles(&self) -> Result<Vec<PathBuf>> {
        sync_lockfile(&self.root)?;
        let lock = self.root.join("uv.lock");
        Ok(if lock.exists() { vec![lock] } else { vec![] })
    }

    fn version_files(&self) -> Vec<PathBuf> {
        let mut files = vec![pyproject_toml_path(&self.root)];
        let lock = self.root.join("uv.lock");
        if lock.exists() {
            files.push(lock);
        }
        files
    }

    fn validate(&self, skip_members: &[String]) -> Result<()> {
        super::validate_project(&self.root, super::ProjectType::Python, skip_members)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_pyproject_toml(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("pyproject.toml");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_read_version_pep621() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
version = "1.2.3"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.2.3".to_string()));
    }

    #[test]
    fn test_read_version_poetry() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[tool.poetry]
name = "my-package"
version = "2.0.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("2.0.0".to_string()));
    }

    #[test]
    fn test_read_version_pep621_preferred_over_poetry() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
version = "1.0.0"

[tool.poetry]
name = "my-package"
version = "2.0.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(
            version,
            Some("1.0.0".to_string()),
            "PEP 621 should take precedence over Poetry"
        );
    }

    #[test]
    fn test_read_version_dynamic() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
dynamic = ["version"]
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None, "Dynamic version should return None");
    }

    #[test]
    fn test_read_version_missing() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn test_write_version_pep621() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
version = "1.0.0"
"#,
        );

        write_version(&path, "1.0.1").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.0.1".to_string()));
    }

    #[test]
    fn test_write_version_poetry() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[tool.poetry]
name = "my-package"
version = "2.0.0"
"#,
        );

        write_version(&path, "2.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("2.1.0".to_string()));
    }

    #[test]
    fn test_write_version_dynamic_refuses_without_corrupting() {
        // Regression (bite) test for the dynamic-version corruption bug: a
        // `dynamic = ["version"]` pyproject with existing tags used to hit the
        // `(None, Some(tag))` arm and get a STATIC `version` written next to the
        // `dynamic` entry -- PEP 621-invalid metadata. The write path must REFUSE.
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
dynamic = ["version"]
"#,
        );

        let before = fs::read_to_string(&path).unwrap();
        let result = write_version(&path, "1.2.3");

        assert!(
            result.is_err(),
            "write_version must refuse on dynamic = [\"version\"], not write corrupt metadata"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("dynamic = [\"version\"]"),
            "refusal must name `dynamic = [\"version\"]`; got: {err}"
        );

        // The file must be untouched: no static `version` field injected.
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, before, "pyproject.toml must be left byte-identical on refusal");
        assert!(
            !after.contains("version = \"1.2.3\""),
            "no static version may be written alongside dynamic"
        );
    }

    #[test]
    fn test_write_version_creates_in_project() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
"#,
        );

        write_version(&path, "0.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("0.1.0".to_string()));
    }

    #[test]
    fn test_pyproject_toml_exists() {
        let dir = TempDir::new().unwrap();
        assert!(!pyproject_toml_exists(dir.path()));

        create_pyproject_toml(dir.path(), "[project]\nname = \"test\"");
        assert!(pyproject_toml_exists(dir.path()));
    }

    #[test]
    fn test_sync_lockfile_no_uv_lock_is_noop() {
        // No uv.lock present -> sync is a no-op and never shells out, even to a
        // nonexistent uv binary.
        let dir = TempDir::new().unwrap();
        create_pyproject_toml(dir.path(), "[project]\nname = \"x\"\nversion = \"0.1.0\"\n");
        sync_lockfile_with(dir.path(), "uv-does-not-exist-bump-test").unwrap();
    }

    #[test]
    fn test_sync_lockfile_uv_missing_with_lock_present_is_loud_error() {
        // uv.lock present but the uv binary is absent -> loud error, never a silently
        // stale lock. A guaranteed-nonexistent binary name simulates NotFound
        // deterministically without touching PATH.
        let dir = TempDir::new().unwrap();
        create_pyproject_toml(dir.path(), "[project]\nname = \"x\"\nversion = \"0.1.0\"\n");
        fs::write(dir.path().join("uv.lock"), "version = 1\n").unwrap();

        let err = sync_lockfile_with(dir.path(), "uv-does-not-exist-bump-test").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("uv.lock is present") && msg.contains("was not found"),
            "expected a loud uv-missing error; got: {msg}"
        );
    }

    #[test]
    fn test_sync_lockfile_uv_lock_check_green() {
        // Requires the real `uv` binary. Skip (do NOT fail) when uv is unavailable or
        // the initial lock cannot be created (no python/network in CI), so the default
        // suite stays green everywhere. The fixture is dep-free, so `uv lock` needs no
        // network to resolve.
        if Command::new("uv")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping test_sync_lockfile_uv_lock_check_green: `uv` not available");
            return;
        }

        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            "[project]\nname = \"uv-fixture\"\nversion = \"0.1.0\"\nrequires-python = \">=3.8\"\n",
        );

        // Establish the initial lock. If this fails (no interpreter / offline), skip.
        let init = Command::new("uv").arg("lock").current_dir(dir.path()).output().unwrap();
        if !init.status.success() {
            eprintln!(
                "skipping test_sync_lockfile_uv_lock_check_green: initial `uv lock` failed: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            return;
        }

        // Bump the version, then sync the lock via the code under test.
        write_version(&path, "0.1.1").unwrap();
        sync_lockfile(dir.path()).unwrap();

        // The lock must now be in sync: `uv lock --check` exits 0.
        let check = Command::new("uv")
            .args(["lock", "--check"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "uv lock --check must be green after sync_lockfile: {}",
            String::from_utf8_lossy(&check.stderr)
        );
    }

    #[test]
    fn test_write_version_preserves_other_fields() {
        let dir = TempDir::new().unwrap();
        let path = create_pyproject_toml(
            dir.path(),
            r#"
[project]
name = "my-package"
version = "1.0.0"
description = "A test package"
requires-python = ">=3.8"

[build-system]
requires = ["setuptools"]
"#,
        );

        write_version(&path, "1.1.0").unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("description = \"A test package\""));
        assert!(content.contains("requires-python = \">=3.8\""));
        assert!(content.contains("[build-system]"));
    }
}
