//! Language-adapter seam: one place that knows which ecosystem (Rust, Python, ...)
//! a directory belongs to, and dispatches manifest/lockfile operations accordingly.
//!
//! This module is the ONLY place that matches on `ProjectType` for language-specific
//! behavior. `main.rs` and the rest of the crate call the plain functions below;
//! adding a new language means adding a `ProjectType` variant, an adapter submodule,
//! and one arm per function here -- zero new match sites outside this module.
//!
//! Phase 1 note: this extracts the 6 existing match sites behind this boundary with
//! byte-identical behavior (detection stays first-match / "Rust wins"). It does not
//! introduce the `Manifest` trait / `Vec<Box<dyn Manifest>>` multi-manifest detection
//! from the design doc's target Data Model -- that lands in Phase 3 once a second
//! adapter (Node) actually needs the Vec-returning shape. Building the trait now,
//! with only one call site exercising it, would be premature abstraction.

use eyre::Result;
use log::debug;
use std::path::Path;

pub mod cargo;
pub mod python;

/// Detected project type for a directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Rust,
    Python,
    Generic,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Rust => write!(f, "Rust"),
            ProjectType::Python => write!(f, "Python"),
            ProjectType::Generic => write!(f, "Generic"),
        }
    }
}

/// Detect the project type for a directory
pub fn detect_project_type(dir: &Path) -> ProjectType {
    debug!("detect_project_type: dir={}", dir.display());
    if cargo::cargo_toml_exists(dir) {
        ProjectType::Rust
    } else if python::pyproject_toml_exists(dir) {
        ProjectType::Python
    } else {
        ProjectType::Generic
    }
}

/// Read the file version for a project type
pub fn read_file_version(dir: &Path, project_type: ProjectType) -> Result<Option<String>> {
    match project_type {
        ProjectType::Rust => {
            let cargo_path = cargo::cargo_toml_path(dir);
            cargo::read_version(&cargo_path)
        }
        ProjectType::Python => {
            let pyproject_path = python::pyproject_toml_path(dir);
            python::read_version(&pyproject_path)
        }
        ProjectType::Generic => Ok(None),
    }
}

/// Write the file version for a project type
pub fn write_file_version(dir: &Path, project_type: ProjectType, new_version: &str) -> Result<()> {
    match project_type {
        ProjectType::Rust => {
            let cargo_path = cargo::cargo_toml_path(dir);
            cargo::write_version(&cargo_path, new_version)
        }
        ProjectType::Python => {
            let pyproject_path = python::pyproject_toml_path(dir);
            python::write_version(&pyproject_path, new_version)
        }
        ProjectType::Generic => Ok(()),
    }
}

/// Sync lockfile for a project type
pub fn sync_lockfile(dir: &Path, project_type: ProjectType) -> Result<()> {
    match project_type {
        ProjectType::Rust => cargo::sync_lockfile(dir),
        ProjectType::Python => python::sync_lockfile(dir),
        ProjectType::Generic => Ok(()),
    }
}

/// Get the version file name for display purposes
pub fn version_file_name(project_type: ProjectType) -> &'static str {
    match project_type {
        ProjectType::Rust => "Cargo.toml",
        ProjectType::Python => "pyproject.toml",
        ProjectType::Generic => "",
    }
}

/// Manifest files that carry the version for a project type. These are ALWAYS
/// version-files (bump writes the version into them).
fn manifest_files(project_type: ProjectType) -> &'static [&'static str] {
    match project_type {
        ProjectType::Rust => &["Cargo.toml"],
        ProjectType::Python => &["pyproject.toml"],
        ProjectType::Generic => &[],
    }
}

/// Lockfiles that bump SYNCS as part of a version bump (Cargo.lock, uv.lock). These
/// count toward "version-files-only" only under the lockfile guard below. poetry.lock,
/// pnpm-lock.yaml, yarn.lock are deliberately absent: bump never syncs them (they do
/// not record the root package version), so a change to one is always the user's.
fn synced_lockfiles(project_type: ProjectType) -> &'static [&'static str] {
    match project_type {
        ProjectType::Rust => &["Cargo.lock"],
        ProjectType::Python => &["uv.lock"],
        ProjectType::Generic => &[],
    }
}

/// Check if the staged file set is only version-related files, which lets bump
/// auto-generate the commit message instead of prompting the operator.
///
/// Lockfile guard (panel finding): a synced lockfile counts as version-only ONLY when
/// bump's own sync produced the change on a previously-clean tree. `predirty_files` is
/// the working-tree dirty set captured BEFORE bump mutated anything; a lockfile present
/// there carries the user's dependency changes and must never be misclassified as a
/// version-only bump. The manifest itself is always a version-file.
pub fn is_version_files_only(staged_files: &[String], project_type: ProjectType, predirty_files: &[String]) -> bool {
    debug!(
        "is_version_files_only: project_type={} staged={:?} predirty={:?}",
        project_type, staged_files, predirty_files
    );
    if project_type == ProjectType::Generic {
        return staged_files.is_empty();
    }
    staged_files.iter().all(|f| {
        let name = f.as_str();
        if manifest_files(project_type).contains(&name) {
            true
        } else if synced_lockfiles(project_type).contains(&name) {
            // Only a bump-synced lockfile (clean before bump ran) counts as version-only.
            !predirty_files.iter().any(|d| d == f)
        } else {
            false
        }
    })
}

/// The terminal line announcing a member left untouched. Printed with `println!`
/// (NOT `info!`): `bump` routes logging to a file, so an `info!` line would be
/// invisible to the operator and break the "never silent" guarantee.
pub(crate) fn skip_message(member: &cargo::IndependentVersionMember) -> String {
    format!("skipping {} (independent version {})", member.name, member.version)
}

/// Validate project-specific constraints.
///
/// For a Rust workspace, every member carrying its own literal `version =` (not
/// `version.workspace = true`) must be accounted for by a `--skip-member <name>`,
/// matched on the **package name**. The guard fails **closed**, always before any
/// mutation:
///   - an independent member not named by `--skip-member` aborts (its raison d'être);
///   - a `--skip-member` matching no independent member aborts, so a stale flag can't
///     rot silently in CI.
///
/// With every independent member skipped, each skip is printed to the terminal and the
/// run proceeds (only `[workspace.package].version` is bumped; the pinned members are
/// left untouched).
pub fn validate_project(dir: &Path, project_type: ProjectType, skip_members: &[String]) -> Result<()> {
    debug!(
        "validate_project: dir={} project_type={} skip_members={:?}",
        dir.display(),
        project_type,
        skip_members
    );

    if project_type != ProjectType::Rust {
        // --skip-member is a Cargo-workspace concept. Fail closed rather than let a
        // stale flag no-op silently on a Python/generic repo (the doc's whole point is
        // that a stale skip can't rot unnoticed in CI).
        if !skip_members.is_empty() {
            log::warn!(
                "validate_project: --skip-member on a non-Rust ({}) project: {:?}",
                project_type,
                skip_members
            );
            eyre::bail!(
                "--skip-member is only meaningful for a Cargo workspace, but this is a {} project.\n\
                 There are no independently-versioned members to skip here; remove the flag.",
                project_type
            );
        }
        return Ok(());
    }

    let independent_members = cargo::check_workspace_independent_versions(dir)?;

    // Fail closed on a stale/misspelled skip: a --skip-member that names no
    // independently-versioned member (wrong name, or a member that already inherits
    // version.workspace = true) aborts before anything is touched.
    let unmatched: Vec<&String> = skip_members
        .iter()
        .filter(|name| !independent_members.iter().any(|m| &m.name == *name))
        .collect();
    if !unmatched.is_empty() {
        let names: Vec<&str> = unmatched.iter().map(|s| s.as_str()).collect();
        log::warn!(
            "validate_project: --skip-member names no independent member: {:?}",
            names
        );
        eyre::bail!(
            "--skip-member named member(s) that are not independently versioned: {}\n\
             (wrong package name, or that member already inherits version.workspace = true). \
             --skip-member matches the package name, not the member path.",
            names.join(", ")
        );
    }

    // Any independent member NOT covered by a --skip-member still aborts.
    let unhandled: Vec<&cargo::IndependentVersionMember> = independent_members
        .iter()
        .filter(|m| !skip_members.iter().any(|s| s == &m.name))
        .collect();
    if !unhandled.is_empty() {
        let member_list: Vec<String> = unhandled
            .iter()
            .map(|m| format!("  - {} ({}): {}", m.name, m.path, m.version))
            .collect();
        log::warn!("validate_project: {} unhandled independent member(s)", unhandled.len());
        eyre::bail!(
            "Workspace members have independent versions (not using version.workspace = true):\n{}\n\n\
             bump only supports workspaces with a unified version in [workspace.package].\n\
             Use --skip-member <name> for a contractually-pinned member.",
            member_list.join("\n")
        );
    }

    // Every independent member is accounted for: announce each skip to the terminal.
    for member in &independent_members {
        println!("{}", skip_message(member));
    }
    debug!(
        "validate_project: proceeding, {} member(s) skipped",
        independent_members.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn test_version_only_rust_manifest_and_synced_lock() {
        // Cargo.toml + a bump-synced Cargo.lock (clean before bump) => version-only.
        assert!(is_version_files_only(
            &s(&["Cargo.toml", "Cargo.lock"]),
            ProjectType::Rust,
            &[]
        ));
    }

    #[test]
    fn test_version_only_python_manifest_and_synced_uv_lock() {
        // pyproject.toml + a bump-synced uv.lock (clean before bump) => version-only.
        assert!(is_version_files_only(
            &s(&["pyproject.toml", "uv.lock"]),
            ProjectType::Python,
            &[]
        ));
    }

    #[test]
    fn test_lockfile_guard_predirtied_uv_lock_not_version_only() {
        // uv.lock was ALREADY dirty before bump (user dep changes) -> not version-only,
        // so bump must not fold it into an auto "Bump version to X" commit.
        assert!(!is_version_files_only(
            &s(&["pyproject.toml", "uv.lock"]),
            ProjectType::Python,
            &s(&["uv.lock"])
        ));
    }

    #[test]
    fn test_lockfile_guard_predirtied_cargo_lock_not_version_only() {
        assert!(!is_version_files_only(
            &s(&["Cargo.toml", "Cargo.lock"]),
            ProjectType::Rust,
            &s(&["Cargo.lock"])
        ));
    }

    #[test]
    fn test_poetry_lock_never_version_only() {
        // poetry.lock is never bump-synced, so it is always a non-version change.
        assert!(!is_version_files_only(
            &s(&["pyproject.toml", "poetry.lock"]),
            ProjectType::Python,
            &[]
        ));
    }

    #[test]
    fn test_unrelated_file_not_version_only() {
        assert!(!is_version_files_only(
            &s(&["Cargo.toml", "src/main.rs"]),
            ProjectType::Rust,
            &[]
        ));
    }

    #[test]
    fn test_generic_version_only_iff_empty() {
        assert!(is_version_files_only(&[], ProjectType::Generic, &[]));
        assert!(!is_version_files_only(&s(&["anything"]), ProjectType::Generic, &[]));
    }
}
