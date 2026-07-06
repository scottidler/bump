//! Node adapter: read/write package.json and sync package-lock.json.
//!
//! Mechanic (decided in the design doc's Phase 0 spike, do NOT re-derive):
//! - READ the authoritative top-level version with serde_json (`value["version"]`).
//!   package.json carries many fields we do not model and MULTIPLE nested `"version"`
//!   keys in the dependency tree, so we read ONLY the top-level object's `version`.
//! - WRITE via a targeted string edit of the SHALLOWEST-indent `"version": "..."`
//!   line, cross-checked against the parsed top-level value. This is byte-exact (like
//!   toml_edit for Cargo.toml): zero indent/newline/unicode churn. First-match is
//!   UNSAFE because real files have nested version keys, so we anchor to the shallowest
//!   indent (the sole top-level `version`) and bail loudly if it disagrees with the
//!   parse.
//! - LOCK sync via `npm install --package-lock-only` when package-lock.json exists; it
//!   updates BOTH root version sites (top-level `version` + `packages[""].version`).
//!   pnpm-lock.yaml / yarn.lock record no root version -> no sync. npm absent while
//!   package-lock.json is present is a LOUD error (never a silently stale lock).
//!   Workspaces / pnpm / yarn / bun are parked (design Non-Goals): Node v1 handles a
//!   single root package.json + package-lock.json.

use super::{Manifest, ManifestVersion};
use eyre::{Context, ContextCompat, Result, bail};
use log::{debug, error};
use semver::Version;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Check if package.json exists at the given directory (ROOT-LEVEL only).
pub fn package_json_exists(dir: &Path) -> bool {
    dir.join("package.json").exists()
}

/// Path to package.json in the given directory.
pub fn package_json_path(dir: &Path) -> PathBuf {
    dir.join("package.json")
}

/// Read the authoritative TOP-LEVEL version from package.json.
///
/// Forward-compat carve-out (rules/rust.md "schema is law"): package.json has many
/// fields we do not model and MULTIPLE nested `"version"` keys in the dependency tree.
/// We parse the whole document with `serde_json::Value` and read ONLY the top-level
/// object's `version` -- deliberately NO `deny_unknown_fields` and no modeled struct;
/// unmodeled keys are expected and ignored. Returns `None` when there is no top-level
/// `version` field (writable-missing at the trait layer).
pub fn read_version(path: &Path) -> Result<Option<String>> {
    debug!("node::read_version: path={}", path.display());
    let content = fs::read_to_string(path).context(format!("Failed to read {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).context(format!("Failed to parse {} as JSON", path.display()))?;
    match value.get("version") {
        None => {
            debug!("node::read_version: no top-level version field");
            Ok(None)
        }
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => bail!(
            "package.json \"version\" is not a string (found {other}) in {}",
            path.display()
        ),
    }
}

/// Byte-exact targeted write of the top-level version. See the module docs for the
/// mechanic. Cross-checks the shallowest-indent `"version"` line against the parsed
/// top-level value and bails loudly on disagreement (never corrupt the file on an
/// ambiguous match). Node v1 does NOT synthesize a missing `version` field.
pub fn write_version(path: &Path, new_version: &str) -> Result<()> {
    debug!(
        "node::write_version: path={} new_version={}",
        path.display(),
        new_version
    );
    let content = fs::read_to_string(path).context(format!("Failed to read {}", path.display()))?;

    let parsed = read_version(path)?.ok_or_else(|| {
        eyre::eyre!(
            "package.json at {} has no top-level \"version\" field to update; \
             bump does not synthesize one for Node projects (v1).",
            path.display()
        )
    })?;

    let (val_start, val_end, on_line) = locate_shallowest_version(&content).ok_or_else(|| {
        eyre::eyre!(
            "could not locate a `\"version\": \"...\"` line in {} to edit",
            path.display()
        )
    })?;

    // Cross-check: the targeted line's value MUST equal the authoritative parse.
    if on_line != parsed {
        error!(
            "node::write_version: shallowest version line (\"{on_line}\") disagrees with \
             parsed top-level version (\"{parsed}\")"
        );
        bail!(
            "package.json version ambiguity in {}: the parser reads \"{parsed}\" but the \
             shallowest-indent \"version\" line reads \"{on_line}\". Refusing to edit to \
             avoid corrupting the manifest.",
            path.display()
        );
    }

    let mut new_content = String::with_capacity(content.len() + new_version.len());
    new_content.push_str(
        content
            .get(..val_start)
            .context("version value start is not on a char boundary")?,
    );
    new_content.push_str(new_version);
    new_content.push_str(
        content
            .get(val_end..)
            .context("version value end is not on a char boundary")?,
    );

    fs::write(path, new_content).context(format!("Failed to write {}", path.display()))?;
    debug!("node::write_version: wrote version {new_version}");
    Ok(())
}

/// Locate the value span of the SHALLOWEST-indent `"version"` line (the sole top-level
/// `version` in a pretty-printed package.json). Returns
/// `(value_start_byte, value_end_byte, value)` as byte offsets into `content`. Uses
/// `split_inclusive('\n')` so byte offsets (and any trailing newline) are exact.
fn locate_shallowest_version(content: &str) -> Option<(usize, usize, String)> {
    let mut best: Option<(usize, usize, usize, String)> = None; // (indent, start, end, value)
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        if let Some((indent, rel_start, len, value)) = parse_version_line(line) {
            let start = offset + rel_start;
            let end = start + len;
            let better = match &best {
                Some((best_indent, _, _, _)) => indent < *best_indent,
                None => true,
            };
            if better {
                best = Some((indent, start, end, value));
            }
        }
        offset += line.len();
    }
    best.map(|(_, s, e, v)| (s, e, v))
}

/// Parse a single line as `<indent>"version"<ws>:<ws>"<value>"...`. Returns
/// `(indent_bytes, value_start_byte_within_line, value_len_bytes, value)`. No string
/// slicing at computed offsets -- `strip_prefix`/`find`/`get` are all boundary-safe.
fn parse_version_line(line: &str) -> Option<(usize, usize, usize, String)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len(); // leading whitespace is ASCII
    let after_key = trimmed.strip_prefix("\"version\"")?;
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_quote = after_colon.trim_start().strip_prefix('"')?;
    let end_rel = after_quote.find('"')?;
    let value = after_quote.get(..end_rel)?.to_string();
    let value_start = line.len() - after_quote.len();
    Some((indent, value_start, end_rel, value))
}

/// Sync package-lock.json after a version change (production entry point).
pub fn sync_lockfile(dir: &Path) -> Result<Vec<PathBuf>> {
    sync_lockfile_with(dir, "npm")
}

/// Testable seam for `sync_lockfile`: `npm_bin` is the npm executable to invoke, so a
/// test can point it at a nonexistent binary to exercise the "npm missing but
/// package-lock.json present" loud-error path deterministically (no PATH mutation).
fn sync_lockfile_with(dir: &Path, npm_bin: &str) -> Result<Vec<PathBuf>> {
    debug!("node::sync_lockfile_with: dir={} npm_bin={}", dir.display(), npm_bin);
    let lock = dir.join("package-lock.json");
    if !lock.exists() {
        // pnpm-lock.yaml / yarn.lock record no root version -> nothing to sync.
        debug!("node::sync_lockfile_with: no package-lock.json, nothing to sync");
        return Ok(vec![]);
    }

    debug!("node::sync_lockfile_with: package-lock.json present, running `{npm_bin} install --package-lock-only`");
    let output = Command::new(npm_bin)
        .args(["install", "--package-lock-only"])
        .current_dir(dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            debug!("node::sync_lockfile_with: npm succeeded");
            Ok(vec![lock])
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            error!("node::sync_lockfile_with: npm failed: {stderr}");
            bail!("`npm install --package-lock-only` failed while syncing package-lock.json:\n{stderr}")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            error!("node::sync_lockfile_with: `{npm_bin}` not found but package-lock.json present");
            bail!(
                "package-lock.json is present but the `npm` binary was not found on PATH.\n\
                 bump will not leave a stale lockfile (a stale lock breaks reproducible installs).\n\
                 Install npm, or remove package-lock.json if this project no longer uses npm."
            )
        }
        Err(e) => Err(e).context("Failed to run `npm install --package-lock-only`"),
    }
}

/// The Node manifest adapter (a single root package.json + optional package-lock.json).
pub struct NodeManifest {
    root: PathBuf,
}

impl NodeManifest {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl Manifest for NodeManifest {
    fn path(&self) -> PathBuf {
        package_json_path(&self.root)
    }

    fn read_version(&self) -> Result<ManifestVersion> {
        match read_version(&package_json_path(&self.root))? {
            Some(s) => Ok(ManifestVersion::Static(crate::version::parse_version(&s)?)),
            None => Ok(ManifestVersion::Missing),
        }
    }

    fn write_version(&self, new_version: &Version) -> Result<()> {
        write_version(
            &package_json_path(&self.root),
            &crate::version::format_file_version(new_version),
        )
    }

    fn sync_lockfiles(&self) -> Result<Vec<PathBuf>> {
        sync_lockfile(&self.root)
    }

    fn version_files(&self) -> Vec<PathBuf> {
        let mut files = vec![package_json_path(&self.root)];
        let lock = self.root.join("package-lock.json");
        if lock.exists() {
            files.push(lock);
        }
        files
    }

    fn validate(&self, skip_members: &[String]) -> Result<()> {
        // Node has no workspace-member concept in v1; --skip-member fails closed here
        // (validate_project's non-Rust branch bails on a non-empty skip list).
        super::validate_project(&self.root, super::ProjectType::Node, skip_members)
    }
}

#[cfg(test)]
mod tests;
