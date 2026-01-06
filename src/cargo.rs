use eyre::{Context, ContextCompat, Result, bail};
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Value};

/// Read the version from Cargo.toml
/// Returns None if version field is missing
pub fn read_version(cargo_toml_path: &Path) -> Result<Option<String>> {
    let content =
        fs::read_to_string(cargo_toml_path).context(format!("Failed to read {}", cargo_toml_path.display()))?;

    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Try [package] version first
    if let Some(package) = doc.get("package")
        && let Some(version) = package.get("version")
    {
        if let Some(v) = version.as_str() {
            return Ok(Some(v.to_string()));
        }
        // Check for version.workspace = true
        if let Some(table) = version.as_inline_table()
            && table.get("workspace").is_some_and(|w| w.as_bool() == Some(true))
        {
            // Version is inherited from workspace, check workspace.package.version
            return read_workspace_version(&doc);
        }
    }

    // Try [workspace.package] version
    if let Some(version) = read_workspace_version(&doc)? {
        return Ok(Some(version));
    }

    Ok(None)
}

/// Read workspace version from [workspace.package]
fn read_workspace_version(doc: &DocumentMut) -> Result<Option<String>> {
    if let Some(workspace) = doc.get("workspace")
        && let Some(package) = workspace.get("package")
        && let Some(version) = package.get("version")
        && let Some(v) = version.as_str()
    {
        return Ok(Some(v.to_string()));
    }
    Ok(None)
}

/// Check if this is a workspace-only manifest (has [workspace] but no [package])
fn is_workspace_only(doc: &DocumentMut) -> bool {
    doc.get("workspace").is_some() && doc.get("package").is_none()
}

/// Update the version in Cargo.toml
/// Creates the version field if it doesn't exist
pub fn write_version(cargo_toml_path: &Path, new_version: &str) -> Result<()> {
    let content =
        fs::read_to_string(cargo_toml_path).context(format!("Failed to read {}", cargo_toml_path.display()))?;

    let mut doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Check if this is a workspace-only manifest (no [package] section)
    if is_workspace_only(&doc) {
        // Update or create [workspace.package].version
        let workspace = doc.get_mut("workspace").context("[workspace] section not found")?;

        if let Item::Table(ws_table) = workspace {
            let package = ws_table
                .entry("package")
                .or_insert(Item::Table(toml_edit::Table::new()));

            if let Item::Table(pkg_table) = package {
                pkg_table["version"] = Item::Value(Value::from(new_version));
            } else {
                bail!("[workspace.package] is not a table");
            }
        } else {
            bail!("[workspace] is not a table");
        }

        fs::write(cargo_toml_path, doc.to_string())
            .context(format!("Failed to write {}", cargo_toml_path.display()))?;
        return Ok(());
    }

    // Check if this is a workspace member with version.workspace = true
    let uses_workspace_version = doc
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_inline_table())
        .is_some_and(|t| t.get("workspace").is_some_and(|w| w.as_bool() == Some(true)));

    if uses_workspace_version {
        // Update workspace.package.version instead
        if let Some(workspace) = doc.get_mut("workspace") {
            if let Some(package) = workspace.get_mut("package") {
                if let Some(version) = package.get_mut("version") {
                    *version = Item::Value(Value::from(new_version));
                } else {
                    bail!("Workspace package section exists but has no version field");
                }
            } else {
                bail!("version.workspace = true but no [workspace.package] section found");
            }
        } else {
            bail!("version.workspace = true but no [workspace] section found");
        }
    } else {
        // Update or create [package] version
        let package = doc.entry("package").or_insert(Item::Table(toml_edit::Table::new()));

        if let Item::Table(table) = package {
            table["version"] = Item::Value(Value::from(new_version));
        } else {
            bail!("[package] is not a table");
        }
    }

    fs::write(cargo_toml_path, doc.to_string()).context(format!("Failed to write {}", cargo_toml_path.display()))?;

    Ok(())
}

/// Sync Cargo.lock with Cargo.toml by running cargo update
/// Only runs if Cargo.lock exists (to avoid creating one in library-only projects)
pub fn sync_lockfile(dir: &Path) -> Result<()> {
    let lockfile = dir.join("Cargo.lock");
    if !lockfile.exists() {
        return Ok(());
    }

    // Read Cargo.toml to determine if this is a workspace or a package
    let cargo_toml = dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml).context(format!("Failed to read {}", cargo_toml.display()))?;
    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Check if this is a workspace-only manifest
    if is_workspace_only(&doc) {
        // For workspaces, just run cargo update to sync all workspace members
        let output = std::process::Command::new("cargo")
            .args(["update", "--workspace"])
            .current_dir(dir)
            .output()
            .context("Failed to run cargo update")?;

        if !output.status.success() {
            bail!(
                "cargo update --workspace failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        return Ok(());
    }

    // For regular packages, get the package name
    let package_name = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .context("Failed to get package name from Cargo.toml")?;

    // Run cargo update -p <package> to sync just this package in the lock file
    let output = std::process::Command::new("cargo")
        .args(["update", "-p", package_name])
        .current_dir(dir)
        .output()
        .context("Failed to run cargo update")?;

    if !output.status.success() {
        bail!("cargo update failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

/// Check if Cargo.toml exists at the given path
pub fn cargo_toml_exists(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

/// Get the path to Cargo.toml in the given directory
pub fn cargo_toml_path(dir: &Path) -> std::path::PathBuf {
    dir.join("Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_cargo_toml(dir: &Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("Cargo.toml");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_read_version_package() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
version = "1.2.3"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.2.3".to_string()));
    }

    #[test]
    fn test_read_version_missing() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn test_read_version_workspace() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace.package]
version = "2.0.0"

[package]
name = "test"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("2.0.0".to_string()));
    }

    #[test]
    fn test_write_version() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
version = "1.0.0"
"#,
        );

        write_version(&path, "1.0.1").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.0.1".to_string()));
    }

    #[test]
    fn test_write_version_creates_field() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "test"
"#,
        );

        write_version(&path, "0.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("0.1.0".to_string()));
    }

    #[test]
    fn test_cargo_toml_exists() {
        let dir = TempDir::new().unwrap();
        assert!(!cargo_toml_exists(dir.path()));

        create_cargo_toml(dir.path(), "[package]\nname = \"test\"");
        assert!(cargo_toml_exists(dir.path()));
    }

    #[test]
    fn test_read_version_workspace_only() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "3.0.0"

[workspace.dependencies]
serde = "1.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("3.0.0".to_string()));
    }

    #[test]
    fn test_read_version_workspace_only_no_version() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.dependencies]
serde = "1.0"
"#,
        );

        let version = read_version(&path).unwrap();
        assert_eq!(version, None);
    }

    #[test]
    fn test_write_version_workspace_only_creates_workspace_package() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.dependencies]
serde = "1.0"
"#,
        );

        write_version(&path, "0.2.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("0.2.0".to_string()));

        // Verify that no [package] section was created
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[package]"), "Should not create [package] section");
        assert!(
            content.contains("[workspace.package]"),
            "Should create [workspace.package] section"
        );
    }

    #[test]
    fn test_write_version_workspace_only_updates_existing() {
        let dir = TempDir::new().unwrap();
        let path = create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "1.0.0"
edition = "2021"

[workspace.dependencies]
serde = "1.0"
"#,
        );

        write_version(&path, "1.1.0").unwrap();

        let version = read_version(&path).unwrap();
        assert_eq!(version, Some("1.1.0".to_string()));

        // Verify the content still has no [package] section
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("\n[package]"), "Should not create [package] section");
    }
}
