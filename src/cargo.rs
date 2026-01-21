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

/// Represents a workspace member with an independent version
#[derive(Debug)]
pub struct IndependentVersionMember {
    pub name: String,
    pub path: String,
    pub version: String,
}

/// Check if workspace members have independent versions (not using version.workspace = true)
/// Returns a list of members with independent versions, or empty vec if all use workspace version
pub fn check_workspace_independent_versions(dir: &Path) -> Result<Vec<IndependentVersionMember>> {
    let cargo_toml = dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml)
        .context(format!("Failed to read {}", cargo_toml.display()))?;
    let doc = content.parse::<DocumentMut>().context("Failed to parse Cargo.toml")?;

    // Only check if this is a workspace
    let workspace = match doc.get("workspace") {
        Some(ws) => ws,
        None => return Ok(vec![]), // Not a workspace, nothing to check
    };

    // Get workspace members
    let members = match workspace.get("members").and_then(|m| m.as_array()) {
        Some(arr) => arr,
        None => return Ok(vec![]), // No members defined
    };

    let mut independent_versions = Vec::new();

    for member in members.iter() {
        let member_path = match member.as_str() {
            Some(p) => p,
            None => continue,
        };

        let member_cargo_toml = dir.join(member_path).join("Cargo.toml");
        if !member_cargo_toml.exists() {
            continue; // Member might use glob pattern or doesn't exist yet
        }

        let member_content = fs::read_to_string(&member_cargo_toml)
            .context(format!("Failed to read {}", member_cargo_toml.display()))?;
        let member_doc = member_content
            .parse::<DocumentMut>()
            .context(format!("Failed to parse {}", member_cargo_toml.display()))?;

        // Check if this member has an independent version
        if let Some(package) = member_doc.get("package") {
            if let Some(version) = package.get("version") {
                // Check if it's NOT using workspace = true
                let uses_workspace = version
                    .as_inline_table()
                    .is_some_and(|t| t.get("workspace").is_some_and(|w| w.as_bool() == Some(true)));

                if !uses_workspace {
                    // This member has an independent version
                    if let Some(v) = version.as_str() {
                        let name = package
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or(member_path)
                            .to_string();

                        independent_versions.push(IndependentVersionMember {
                            name,
                            path: member_path.to_string(),
                            version: v.to_string(),
                        });
                    }
                }
            }
        }
    }

    Ok(independent_versions)
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

    // Tests for check_workspace_independent_versions

    fn create_member_cargo_toml(dir: &Path, member_path: &str, content: &str) {
        let member_dir = dir.join(member_path);
        fs::create_dir_all(&member_dir).unwrap();
        let path = member_dir.join("Cargo.toml");
        fs::write(&path, content).unwrap();
    }

    #[test]
    fn test_check_independent_versions_not_a_workspace() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[package]
name = "my-app"
version = "1.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Non-workspace should return empty vec");
    }

    #[test]
    fn test_check_independent_versions_workspace_no_members() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]

[workspace.package]
version = "1.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Workspace with no members should return empty vec");
    }

    #[test]
    fn test_check_independent_versions_all_use_workspace_version() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version.workspace = true
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version.workspace = true
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(
            result.is_empty(),
            "All members using workspace version should return empty vec"
        );
    }

    #[test]
    fn test_check_independent_versions_some_independent() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b", "crate-c"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // crate-a uses workspace version
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version.workspace = true
"#,
        );

        // crate-b has independent version
        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version = "2.0.0"
"#,
        );

        // crate-c has independent version
        create_member_cargo_toml(
            dir.path(),
            "crate-c",
            r#"
[package]
name = "crate-c"
version = "3.5.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 2, "Should detect 2 members with independent versions");

        let names: Vec<&str> = result.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"crate-b"), "Should detect crate-b");
        assert!(names.contains(&"crate-c"), "Should detect crate-c");

        let crate_b = result.iter().find(|m| m.name == "crate-b").unwrap();
        assert_eq!(crate_b.version, "2.0.0");
        assert_eq!(crate_b.path, "crate-b");

        let crate_c = result.iter().find(|m| m.name == "crate-c").unwrap();
        assert_eq!(crate_c.version, "3.5.0");
        assert_eq!(crate_c.path, "crate-c");
    }

    #[test]
    fn test_check_independent_versions_all_independent() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-b"]
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crate-b",
            r#"
[package]
name = "crate-b"
version = "2.0.0"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 2, "Should detect both members with independent versions");
    }

    #[test]
    fn test_check_independent_versions_member_missing_cargo_toml() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a", "crate-missing"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // Only create crate-a, crate-missing doesn't exist
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
version = "1.0.0"
"#,
        );

        // Should not error, just skip missing member
        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 1, "Should only detect existing member");
        assert_eq!(result[0].name, "crate-a");
    }

    #[test]
    fn test_check_independent_versions_member_no_version_field() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crate-a"]

[workspace.package]
version = "1.0.0"
"#,
        );

        // Member has no version field at all
        create_member_cargo_toml(
            dir.path(),
            "crate-a",
            r#"
[package]
name = "crate-a"
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert!(result.is_empty(), "Member with no version field should not be flagged");
    }

    #[test]
    fn test_check_independent_versions_nested_path() {
        let dir = TempDir::new().unwrap();
        create_cargo_toml(
            dir.path(),
            r#"
[workspace]
members = ["crates/core", "crates/cli"]

[workspace.package]
version = "1.0.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crates/core",
            r#"
[package]
name = "my-core"
version = "0.5.0"
"#,
        );

        create_member_cargo_toml(
            dir.path(),
            "crates/cli",
            r#"
[package]
name = "my-cli"
version.workspace = true
"#,
        );

        let result = check_workspace_independent_versions(dir.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "my-core");
        assert_eq!(result[0].path, "crates/core");
        assert_eq!(result[0].version, "0.5.0");
    }
}
