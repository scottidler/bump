use eyre::{Context, ContextCompat, Result, bail};
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Value};

/// Check if pyproject.toml exists at the given path
pub fn pyproject_toml_exists(dir: &Path) -> bool {
    dir.join("pyproject.toml").exists()
}

/// Get the path to pyproject.toml in the given directory
pub fn pyproject_toml_path(dir: &Path) -> std::path::PathBuf {
    dir.join("pyproject.toml")
}

/// Read the version from pyproject.toml
/// Checks [project].version (PEP 621) first, then [tool.poetry].version
/// Returns None if version field is missing or dynamic
pub fn read_version(pyproject_path: &Path) -> Result<Option<String>> {
    let content =
        fs::read_to_string(pyproject_path).context(format!("Failed to read {}", pyproject_path.display()))?;

    let doc = content.parse::<DocumentMut>().context("Failed to parse pyproject.toml")?;

    // Check if version is declared dynamic (PEP 621)
    if let Some(project) = doc.get("project")
        && let Some(dynamic) = project.get("dynamic")
        && let Some(arr) = dynamic.as_array()
    {
        for item in arr.iter() {
            if item.as_str() == Some("version") {
                return Ok(None);
            }
        }
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
    let content =
        fs::read_to_string(pyproject_path).context(format!("Failed to read {}", pyproject_path.display()))?;

    let mut doc = content.parse::<DocumentMut>().context("Failed to parse pyproject.toml")?;

    match version_section(&doc) {
        Some("project") => {
            let project = doc
                .get_mut("project")
                .context("[project] section not found")?;

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
            let project = doc
                .entry("project")
                .or_insert(Item::Table(toml_edit::Table::new()));
            if let Item::Table(table) = project {
                table["version"] = Item::Value(Value::from(new_version));
            } else {
                bail!("[project] is not a table");
            }
        }
    }

    fs::write(pyproject_path, doc.to_string())
        .context(format!("Failed to write {}", pyproject_path.display()))?;

    Ok(())
}

/// Sync lockfile after version change (no-op for Python - lockfiles don't track project version)
pub fn sync_lockfile(_dir: &Path) -> Result<()> {
    Ok(())
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
        assert_eq!(version, Some("1.0.0".to_string()), "PEP 621 should take precedence over Poetry");
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
