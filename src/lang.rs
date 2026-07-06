//! Language-adapter seam: one place that knows which ecosystem (Rust, Python, ...)
//! a directory belongs to, and dispatches manifest/lockfile operations accordingly.
//!
//! This module is the ONLY place that matches on `ProjectType` for language-specific
//! behavior. `main.rs` and the rest of the crate call the plain functions below;
//! adding a new language means adding a `ProjectType` variant, an adapter submodule,
//! and one arm per function here -- zero new match sites outside this module.
//!
//! Phase 3 note: the `Manifest` trait, `ManifestVersion` enum, and Vec-returning
//! `detect()` (deferred in Phase 1 as premature with only one caller) land here now
//! that Node is a second adapter that needs the multi-manifest shape. `detect()`
//! returns ALL root-level version-bearing manifests, `agreed_version()` enforces "one
//! repo = one version = one tag" (a loud refusal on disagreement), and `write_all()`
//! writes every manifest in lockstep. `ProjectType` stays as the POLICY marker for
//! `determine_version_action` (the doc keeps language-specific version policy out of
//! the trait); the plain dispatch functions that remain (`read_file_version`,
//! `version_file_name`, `is_version_files_only`) serve that policy layer.

use eyre::{Result, bail};
use log::{debug, error};
use semver::Version;
use std::path::{Path, PathBuf};

pub mod cargo;
pub mod node;
pub mod python;

/// The version state read from a single manifest.
///
/// `read_version` returns this enum, NOT `Option`: the four current behaviors must
/// stay distinct -- `Static` (version present, writable) | `Missing` (version-bearing
/// file with no version field yet, writable) | `Dynamic` (version owned elsewhere,
/// e.g. `dynamic = ["version"]`: REFUSE) | no-manifest-at-all (an empty Vec from
/// `detect`, handled by the caller). `Missing` and no-manifest MUST stay distinct in
/// the policy layer -- collapsing them resurrects the bug the enum prevents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestVersion {
    /// Normal case: a version is present and writable.
    Static(Version),
    /// A version-bearing file with no version field yet: writable.
    Missing,
    /// Version owned elsewhere (carries the reason); the write path REFUSES.
    Dynamic(String),
}

/// A detected root-level version-bearing manifest, handled uniformly across the
/// heterogeneous cargo / python / node ecosystems.
///
/// `Box<dyn Manifest>` is EXPLICITLY sanctioned by the design doc's Data Model here --
/// a deliberate, doc-specified exception to `rules/rust.md`'s generics-over-`dyn`
/// preference, because `detect()` returns a heterogeneous collection that the release
/// mechanic handles uniformly. `path()` (not in the doc's trait sketch) is added so
/// the disagreement error can name each offending file.
pub trait Manifest {
    /// Path to the manifest file itself (for messages / disagreement reports).
    fn path(&self) -> PathBuf;
    /// Read the manifest's version state.
    fn read_version(&self) -> Result<ManifestVersion>;
    /// Write a new version. Errors on `Dynamic`; never writes corrupt metadata.
    fn write_version(&self, new_version: &Version) -> Result<()>;
    /// Sync lockfiles after a version change; returns the files touched, for the commit.
    fn sync_lockfiles(&self) -> Result<Vec<PathBuf>>;
    /// Manifest + locks (for `is_version_files_only`).
    fn version_files(&self) -> Vec<PathBuf>;
    /// Project-specific validation (e.g. the Cargo workspace independent-version guard).
    fn validate(&self, skip_members: &[String]) -> Result<()>;
}

/// Detect ALL root-level version-bearing manifests. ROOT-LEVEL only, NEVER recursive:
/// a nested `web/package.json` or a test fixture must not trigger. Cargo workspace
/// members are the cargo adapter's internal concern, not detection's. An empty Vec
/// means today's `Generic` (version lives in tags alone).
pub fn detect(root: &Path) -> Result<Vec<Box<dyn Manifest>>> {
    debug!("detect: root={}", root.display());
    let mut manifests: Vec<Box<dyn Manifest>> = Vec::new();
    if cargo::cargo_toml_exists(root) {
        manifests.push(Box::new(cargo::CargoManifest::new(root)));
    }
    // pyproject.toml counts only when version-bearing ([project] or [tool.poetry]);
    // a ruff-config-only pyproject next to Cargo.toml must not trigger Python.
    if python::is_version_bearing(root) {
        manifests.push(Box::new(python::PythonManifest::new(root)));
    }
    if node::package_json_exists(root) {
        manifests.push(Box::new(node::NodeManifest::new(root)));
    }
    debug!("detect: {} root manifest(s)", manifests.len());
    Ok(manifests)
}

/// Read the single version agreed upon by every detected manifest -- one repo = one
/// version = one tag.
///
/// - A `Dynamic` manifest anywhere is surfaced as `Dynamic` (the caller refuses):
///   writing a static version elsewhere while one manifest owns it dynamically is
///   corruption.
/// - Two disagreeing `Static` versions are a LOUD error naming BOTH files AND values.
///   ("Rust silently wins" dies.)
/// - All-`Missing` (version-bearing files with no version yet) returns `Missing`.
///
/// Precondition: `manifests` is non-empty (an empty Vec is the caller's generic case,
/// distinct from `Missing`).
pub fn agreed_version(manifests: &[Box<dyn Manifest>]) -> Result<ManifestVersion> {
    debug!("agreed_version: {} manifest(s)", manifests.len());
    let mut statics: Vec<(PathBuf, Version)> = Vec::new();
    let mut dynamic: Option<(PathBuf, String)> = None;
    let mut any_missing = false;

    for m in manifests {
        match m.read_version()? {
            ManifestVersion::Static(v) => statics.push((m.path(), v)),
            ManifestVersion::Dynamic(reason) => {
                if dynamic.is_none() {
                    dynamic = Some((m.path(), reason));
                }
            }
            ManifestVersion::Missing => any_missing = true,
        }
    }

    // A dynamic manifest contests the version: refuse rather than corrupt metadata.
    if let Some((path, reason)) = dynamic {
        return Ok(ManifestVersion::Dynamic(format!(
            "{} declares {}",
            path.display(),
            reason
        )));
    }

    if let Some((_, first)) = statics.first() {
        if statics.iter().any(|(_, v)| v != first) {
            let listing = statics
                .iter()
                .map(|(p, v)| format!("  {} = {}", p.display(), v))
                .collect::<Vec<_>>()
                .join("\n");
            error!("agreed_version: manifest versions disagree");
            bail!(
                "Manifest versions disagree (one repo = one version = one tag):\n{listing}\n\n\
                 Sync every manifest to the same version before bumping."
            );
        }
        return Ok(ManifestVersion::Static(first.clone()));
    }

    if any_missing {
        return Ok(ManifestVersion::Missing);
    }
    // Non-empty precondition means we never fall through; keep it total anyway.
    Ok(ManifestVersion::Missing)
}

/// Write `new_version` into EVERY detected manifest in lockstep and sync their locks.
/// Returns every file touched (manifests + synced locks) for the commit.
pub fn write_all(manifests: &[Box<dyn Manifest>], new_version: &Version) -> Result<Vec<PathBuf>> {
    debug!("write_all: {} manifest(s) -> {}", manifests.len(), new_version);
    let mut touched: Vec<PathBuf> = Vec::new();
    for m in manifests {
        m.write_version(new_version)?;
        touched.extend(m.version_files());
        touched.extend(m.sync_lockfiles()?);
    }
    Ok(touched)
}

/// Detected project type for a directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Rust,
    Python,
    Node,
    Generic,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Rust => write!(f, "Rust"),
            ProjectType::Python => write!(f, "Python"),
            ProjectType::Node => write!(f, "Node"),
            ProjectType::Generic => write!(f, "Generic"),
        }
    }
}

/// Detect the project type (POLICY marker) for a directory. Uses the same root-level,
/// version-bearing predicates as `detect()` so the policy type and the manifest set
/// never diverge (a ruff-config-only pyproject is NOT Python; package.json is Node).
pub fn detect_project_type(dir: &Path) -> ProjectType {
    debug!("detect_project_type: dir={}", dir.display());
    if cargo::cargo_toml_exists(dir) {
        ProjectType::Rust
    } else if python::is_version_bearing(dir) {
        ProjectType::Python
    } else if node::package_json_exists(dir) {
        ProjectType::Node
    } else {
        ProjectType::Generic
    }
}

/// Read the file version for a project type (policy layer: display + `--tag-only`).
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
        ProjectType::Node => {
            let package_json = node::package_json_path(dir);
            node::read_version(&package_json)
        }
        ProjectType::Generic => Ok(None),
    }
}

/// Get the version file name for display purposes
pub fn version_file_name(project_type: ProjectType) -> &'static str {
    match project_type {
        ProjectType::Rust => "Cargo.toml",
        ProjectType::Python => "pyproject.toml",
        ProjectType::Node => "package.json",
        ProjectType::Generic => "",
    }
}

/// Manifest files that carry the version for a project type. These are ALWAYS
/// version-files (bump writes the version into them).
fn manifest_files(project_type: ProjectType) -> &'static [&'static str] {
    match project_type {
        ProjectType::Rust => &["Cargo.toml"],
        ProjectType::Python => &["pyproject.toml"],
        ProjectType::Node => &["package.json"],
        ProjectType::Generic => &[],
    }
}

/// Lockfiles that bump SYNCS as part of a version bump (Cargo.lock, uv.lock,
/// package-lock.json). These count toward "version-files-only" only under the lockfile
/// guard below. poetry.lock, pnpm-lock.yaml, yarn.lock are deliberately absent: bump
/// never syncs them (they do not record the root package version), so a change to one
/// is always the user's.
fn synced_lockfiles(project_type: ProjectType) -> &'static [&'static str] {
    match project_type {
        ProjectType::Rust => &["Cargo.lock"],
        ProjectType::Python => &["uv.lock"],
        ProjectType::Node => &["package-lock.json"],
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
    use std::fs;
    use tempfile::TempDir;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ---- Multi-manifest detection + agreement (Phase 3) ----

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn detect_ignores_nested_package_json() {
        // ROOT-LEVEL only: a nested web/package.json must NOT trigger Node.
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n",
        );
        fs::create_dir_all(dir.path().join("web")).unwrap();
        fs::write(dir.path().join("web/package.json"), "{\n  \"version\": \"9.9.9\"\n}\n").unwrap();

        let manifests = detect(dir.path()).unwrap();
        assert_eq!(
            manifests.len(),
            1,
            "only the root Cargo.toml; nested package.json is invisible"
        );
        assert!(manifests[0].path().ends_with("Cargo.toml"));
    }

    #[test]
    fn detect_ignores_ruff_only_pyproject() {
        // A ruff-config-only pyproject (no [project]/[tool.poetry]) next to Cargo.toml
        // must NOT trigger Python.
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n",
        );
        write(dir.path(), "pyproject.toml", "[tool.ruff]\nline-length = 100\n");

        let manifests = detect(dir.path()).unwrap();
        assert_eq!(manifests.len(), 1, "ruff-only pyproject must not trigger Python");
        assert!(manifests[0].path().ends_with("Cargo.toml"));
        assert_eq!(detect_project_type(dir.path()), ProjectType::Rust);
    }

    #[test]
    fn detect_triggers_python_on_version_bearing_pyproject() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"1.0.0\"\n",
        );
        let manifests = detect(dir.path()).unwrap();
        assert_eq!(manifests.len(), 1);
        assert!(manifests[0].path().ends_with("pyproject.toml"));
        assert_eq!(detect_project_type(dir.path()), ProjectType::Python);
    }

    #[test]
    fn detect_triggers_node_on_root_package_json() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            "{\n  \"name\": \"x\",\n  \"version\": \"1.0.0\"\n}\n",
        );
        let manifests = detect(dir.path()).unwrap();
        assert_eq!(manifests.len(), 1);
        assert!(manifests[0].path().ends_with("package.json"));
        assert_eq!(detect_project_type(dir.path()), ProjectType::Node);
    }

    #[test]
    fn agreed_version_refuses_mismatch_naming_both_files_and_values() {
        // A Cargo+pyproject repo with MISMATCHED versions refuses, naming BOTH files
        // AND both values. Deterministic, no network -- must always run and bite.
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"1.2.3\"\n",
        );
        write(
            dir.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"4.5.6\"\n",
        );

        let manifests = detect(dir.path()).unwrap();
        assert_eq!(manifests.len(), 2);
        let err = agreed_version(&manifests).unwrap_err().to_string();
        assert!(err.contains("Cargo.toml"), "must name Cargo.toml: {err}");
        assert!(err.contains("pyproject.toml"), "must name pyproject.toml: {err}");
        assert!(err.contains("1.2.3"), "must name the Cargo value: {err}");
        assert!(err.contains("4.5.6"), "must name the pyproject value: {err}");
    }

    #[test]
    fn agreed_version_accepts_matching_static_versions() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"2.0.0\"\n",
        );
        write(
            dir.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"2.0.0\"\n",
        );
        let manifests = detect(dir.path()).unwrap();
        assert_eq!(
            agreed_version(&manifests).unwrap(),
            ManifestVersion::Static(Version::new(2, 0, 0))
        );
    }

    #[test]
    fn missing_version_is_distinct_from_no_manifest() {
        // A version-bearing pyproject with NO version field -> Missing (writable).
        let dir = TempDir::new().unwrap();
        write(dir.path(), "pyproject.toml", "[project]\nname = \"x\"\n");
        let manifests = detect(dir.path()).unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(agreed_version(&manifests).unwrap(), ManifestVersion::Missing);

        // No manifest at all -> empty Vec (generic), NOT Missing -- the distinction the
        // enum exists to preserve.
        let empty = TempDir::new().unwrap();
        assert!(
            detect(empty.path()).unwrap().is_empty(),
            "no manifest => empty Vec, not Missing"
        );
    }

    #[test]
    fn agreed_version_dynamic_is_surfaced() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\ndynamic = [\"version\"]\n",
        );
        let manifests = detect(dir.path()).unwrap();
        match agreed_version(&manifests).unwrap() {
            ManifestVersion::Dynamic(reason) => assert!(reason.contains("dynamic"), "got: {reason}"),
            other => panic!("expected Dynamic, got {other:?}"),
        }
    }

    #[test]
    fn write_all_writes_every_manifest_in_lockstep() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"1.0.0\"\n",
        );
        write(
            dir.path(),
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"1.0.0\"\n",
        );
        let manifests = detect(dir.path()).unwrap();

        let touched = write_all(&manifests, &Version::new(2, 0, 0)).unwrap();

        assert_eq!(
            cargo::read_version(&dir.path().join("Cargo.toml")).unwrap().as_deref(),
            Some("2.0.0")
        );
        assert_eq!(
            python::read_version(&dir.path().join("pyproject.toml"))
                .unwrap()
                .as_deref(),
            Some("2.0.0")
        );
        assert!(
            touched.iter().any(|p| p.ends_with("Cargo.toml")),
            "touched must include Cargo.toml"
        );
        assert!(
            touched.iter().any(|p| p.ends_with("pyproject.toml")),
            "touched must include pyproject.toml"
        );
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
