//! Which root-manifest lines are the PACKAGE version, and whether a branch changed them.
//! Both the branch's-own-bump test and the bump-only test read this: a `version =` under
//! `[dependencies.<name>]` is a dependency bump, never the package's version (design doc,
//! "Bump-only branch": a dependency bump is NOT bump-only).

use crate::git;
use eyre::{Context, Result, bail};
use log::debug;
use std::path::Path;
use std::process::Command;

/// The root manifests whose package version decides whether a branch carries its own bump
/// (Gate D's pathspec, `git-release-guard.sh:565`).
const OWN_BUMP_MANIFESTS: [&str; 3] = ["Cargo.toml", "pyproject.toml", "package.json"];

/// The TOML tables whose `version` key is the package version, per manifest.
const CARGO_VERSION_TABLES: [&str; 2] = ["package", "workspace.package"];
const PYPROJECT_VERSION_TABLES: [&str; 2] = ["project", "tool.poetry"];

/// How one root manifest changed between the merge base and HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ManifestChange {
    /// A package version line was added, removed, or changed.
    pub(super) version: bool,
    /// Any other line was added, removed, or changed.
    pub(super) other: bool,
}

/// A manifest's lines, split into the package version lines and everything else.
#[derive(Debug, Default, PartialEq, Eq)]
struct Split<'a> {
    version: Vec<&'a str>,
    other: Vec<&'a str>,
}

/// Split `content` of root manifest `file` into its package version lines and the rest.
/// `VERSION` is all version; an unknown file name is all "other".
fn split_package_version<'a>(file: &str, content: &'a str) -> Split<'a> {
    match file {
        "Cargo.toml" => split_toml(content, &CARGO_VERSION_TABLES),
        "pyproject.toml" => split_toml(content, &PYPROJECT_VERSION_TABLES),
        "package.json" => split_json(content),
        "VERSION" => Split {
            version: content.lines().filter(|l| !l.trim().is_empty()).collect(),
            other: Vec::new(),
        },
        _ => Split {
            version: Vec::new(),
            other: content.lines().collect(),
        },
    }
}

/// TOML: a `version` (or dotted `version.<x>`) key inside one of `tables` is a version line.
/// The current table is tracked from `[table]` / `[[array]]` headers.
fn split_toml<'a>(content: &'a str, tables: &[&str]) -> Split<'a> {
    let mut split = Split::default();
    let mut table = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            let header = trimmed.split('#').next().unwrap_or("").trim();
            table = header.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            split.other.push(line);
            continue;
        }
        let key = trimmed.split('=').next().unwrap_or("").trim().trim_matches('"');
        let is_version_key = trimmed.contains('=') && (key == "version" || key.starts_with("version."));
        if is_version_key && tables.contains(&table.as_str()) {
            split.version.push(line);
        } else {
            split.other.push(line);
        }
    }
    split
}

/// JSON: a `"version":` key at object depth 1 (the top-level object) is the version line.
/// Depth is counted over `{`/`[` outside strings, as of the start of each line.
fn split_json(content: &str) -> Split<'_> {
    let mut split = Split::default();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for line in content.lines() {
        let at_top = depth == 1;
        let rest = line.trim_start();
        let is_version_key = rest
            .strip_prefix("\"version\"")
            .is_some_and(|r| r.trim_start().starts_with(':'));
        if at_top && is_version_key {
            split.version.push(line);
        } else {
            split.other.push(line);
        }
        for c in line.chars() {
            match (in_string, escaped, c) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (true, false, '"') => in_string = false,
                (false, _, '"') => in_string = true,
                (false, _, '{' | '[') => depth += 1,
                (false, _, '}' | ']') => depth -= 1,
                _ => {}
            }
        }
    }
    split
}

/// Compare one manifest's content at the merge base and at HEAD (`None` = absent there).
fn compare(file: &str, base: Option<&str>, head: Option<&str>) -> ManifestChange {
    let base = split_package_version(file, base.unwrap_or(""));
    let head = split_package_version(file, head.unwrap_or(""));
    ManifestChange {
        version: base.version != head.version,
        other: base.other != head.other,
    }
}

/// `git merge-base <base> HEAD`: the commit the `base...HEAD` diffs compare against.
fn merge_base(dir: &Path, base: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["merge-base", base, "HEAD"])
        .current_dir(dir)
        .output()
        .context("Failed to run git merge-base")?;
    if !output.status.success() {
        bail!(
            "git merge-base {} HEAD failed: {}",
            base,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// How each of `files` changed on `base...HEAD`, by package version vs everything else.
pub(super) fn manifest_changes(dir: &Path, base: &str, files: &[&str]) -> Result<Vec<ManifestChange>> {
    debug!(
        "manifest_changes: dir={} base={} files={:?}",
        dir.display(),
        base,
        files
    );
    let mb = merge_base(dir, base)?;
    files
        .iter()
        .map(|file| {
            let old = git::file_at(dir, &mb, file)?;
            let new = git::file_at(dir, "HEAD", file)?;
            Ok(compare(file, old.as_deref(), new.as_deref()))
        })
        .collect()
}

/// Does the branch change the PACKAGE version in a root manifest relative to `base`? True
/// means the branch bumped the version itself; a pending version with no such change was
/// inherited from `base`.
pub(super) fn version_line_changed(dir: &Path, base: &str) -> Result<bool> {
    let changed = manifest_changes(dir, base, &OWN_BUMP_MANIFESTS)?
        .iter()
        .any(|c| c.version);
    debug!(
        "version_line_changed: dir={} base={} changed={changed}",
        dir.display(),
        base
    );
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "[package]\nname = \"x\"\nversion = \"0.1.5\"\n\n[dependencies.itoa]\nversion = \"1.0.14\"\n\n[dependencies]\nserde = { version = \"1\" }\n";

    #[test]
    fn cargo_package_version_is_the_only_version_line() {
        let split = split_package_version("Cargo.toml", CARGO);
        assert_eq!(split.version, vec!["version = \"0.1.5\""]);
        assert!(
            split.other.contains(&"version = \"1.0.14\""),
            "dependency table stays other"
        );
    }

    #[test]
    fn cargo_workspace_package_and_pyproject_tables() {
        let ws = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"2.0.0\"\n";
        assert_eq!(
            split_package_version("Cargo.toml", ws).version,
            vec!["version = \"2.0.0\""]
        );
        let py = "[project]\nname = \"x\"\nversion = \"1.0.0\"\n\n[tool.other]\nversion = \"9\"\n";
        assert_eq!(
            split_package_version("pyproject.toml", py).version,
            vec!["version = \"1.0.0\""]
        );
        let poetry = "[tool.poetry]\nversion = \"0.3.0\"  # pinned\n";
        assert_eq!(split_package_version("pyproject.toml", poetry).version.len(), 1);
    }

    #[test]
    fn json_top_level_version_only() {
        let pkg =
            "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": {\n    \"version\": \"3\"\n  }\n}\n";
        let split = split_package_version("package.json", pkg);
        assert_eq!(split.version, vec!["  \"version\": \"1.0.0\","]);
        let quoted = "{\n  \"description\": \"a { brace\",\n  \"version\": \"1.0.1\"\n}\n";
        assert_eq!(
            split_package_version("package.json", quoted).version.len(),
            1,
            "braces in strings"
        );
    }

    #[test]
    fn compare_tells_package_bump_from_dependency_bump() {
        let bumped = CARGO.replacen("0.1.5", "0.1.6", 1);
        let dep = CARGO.replace("1.0.14", "1.0.15");
        assert_eq!(
            compare("Cargo.toml", Some(CARGO), Some(&bumped)),
            ManifestChange {
                version: true,
                other: false
            }
        );
        assert_eq!(
            compare("Cargo.toml", Some(CARGO), Some(&dep)),
            ManifestChange {
                version: false,
                other: true
            }
        );
        assert_eq!(
            compare("VERSION", Some("1.0.0\n"), Some("1.0.1\n")),
            ManifestChange {
                version: true,
                other: false
            }
        );
        assert_eq!(
            compare("Cargo.toml", None, None),
            ManifestChange {
                version: false,
                other: false
            }
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit_manifest(dir: &Path, content: &str, message: &str) {
        std::fs::write(dir.join("Cargo.toml"), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", message]);
    }

    /// Moved from `git::tests::version_line_changed_tells_own_bump_from_work_only` when the
    /// test moved here, plus the dependency-table case that test missed.
    #[test]
    fn version_line_changed_tells_own_bump_from_work_and_dependency_bumps() {
        let repo = tempfile::TempDir::new().unwrap();
        let w = repo.path();
        git(w, &["init", "-q", "-b", "main"]);
        git(w, &["config", "user.email", "t@t"]);
        git(w, &["config", "user.name", "t"]);
        commit_manifest(w, CARGO, "init");
        git(w, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(w.join("work.txt"), "x").unwrap();
        git(w, &["add", "-A"]);
        git(w, &["commit", "-m", "work"]);
        assert!(!version_line_changed(w, "main").unwrap(), "work only");

        commit_manifest(w, &CARGO.replace("1.0.14", "1.0.15"), "dependency bump");
        assert!(!version_line_changed(w, "main").unwrap(), "a dependency-table bump");

        commit_manifest(
            w,
            &CARGO.replace("1.0.14", "1.0.15").replacen("0.1.5", "0.1.6", 1),
            "bump",
        );
        assert!(version_line_changed(w, "main").unwrap(), "the branch's own bump");
    }

    #[test]
    fn merge_base_errors_on_unknown_ref() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(merge_base(dir.path(), "no-such-ref").is_err());
    }
}
