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

/// Check if staged files are only version-related files
pub fn is_version_files_only(staged_files: &[String], project_type: ProjectType) -> bool {
    match project_type {
        ProjectType::Rust => staged_files.iter().all(|f| f == "Cargo.toml" || f == "Cargo.lock"),
        ProjectType::Python => staged_files.iter().all(|f| f == "pyproject.toml"),
        ProjectType::Generic => staged_files.is_empty(),
    }
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
