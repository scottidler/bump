//! Scott's standalone order (`--standalone "<words>"`) and the bump-only branch test it is
//! the one exception to (THE RULING 2026-07-03: the bump never rides alone; 2026-07-10: an
//! explicit order is the exception).

use super::ci::Ci;
use super::pr::{Pr, pr_title};
use super::{
    GATED_PAUSE_MESSAGE, Installer, PendingCheck, Ports, Pusher, ReleaseOpts, ReleaseReport, ReleaseState,
    classify_gated_feature, compute_target_tag, execute, pending_version,
};
use crate::config::Config;
use crate::git;
use crate::lang;
use crate::version;
use eyre::Result;
use log::debug;
use std::path::Path;

/// Root files a bump may touch (`git-release-guard.sh` `is_bump_only_ref`): exact paths, no
/// directory.
const BUMP_FILES: [&str; 9] = [
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "pyproject.toml",
    "uv.lock",
    "VERSION",
];

/// The manifests whose changed lines must all be version lines for a branch to be bump-only.
const BUMP_MANIFESTS: [&str; 4] = ["Cargo.toml", "package.json", "pyproject.toml", "VERSION"];

/// The standalone branch for a target tag: `bump-vX-Y-Z`. Dots become dashes because the PR
/// title derives from the branch and `.` never slugifies back.
pub(super) fn standalone_branch_name(tag: &str) -> String {
    format!("bump-{}", tag.replace('.', "-"))
}

/// Is HEAD's diff against `base` a version bump and nothing else? A port of the hook's
/// `is_bump_only_ref` with one intended difference: an EMPTY diff (zero commits ahead, or
/// commits that change nothing) is bump-only here, because the verb has no Gate A.
pub(super) fn is_bump_only_branch(dir: &Path, base: &str) -> Result<bool> {
    debug!("is_bump_only_branch: dir={} base={}", dir.display(), base);
    let files = git::changed_files(dir, base)?;
    let manifest_lines = git::changed_lines(dir, base, &BUMP_MANIFESTS)?;
    let bump_only = bump_only(&files, &manifest_lines);
    debug!("is_bump_only_branch: files={files:?} bump_only={bump_only}");
    Ok(bump_only)
}

/// The pure decision behind `is_bump_only_branch`: every changed path is a root bump file,
/// and at least one manifest line changed and every changed manifest line is a version line.
/// A lockfile-only refresh (no manifest line) and a dependency bump (a non-version manifest
/// line) are NOT bump-only; THE RULING allows both.
fn bump_only(files: &[String], manifest_lines: &[String]) -> bool {
    if files.is_empty() {
        return true;
    }
    if !files.iter().all(|f| BUMP_FILES.contains(&f.as_str())) {
        return false;
    }
    !manifest_lines.is_empty() && manifest_lines.iter().all(|l| git::is_version_diff_line(l))
}

/// Gated, on the default branch, clean, HEAD == origin, with Scott's order: resolve the
/// version the standalone PR releases (the same target the fresh-cut branch will classify
/// to) and name its branch.
pub(super) fn classify_gated_standalone(dir: &Path, opts: &ReleaseOpts, default: String) -> Result<ReleaseState> {
    debug!("classify_gated_standalone: dir={} default={}", dir.display(), default);
    if lang::detect(dir)?.is_empty() {
        return Ok(ReleaseState::GatedGeneric);
    }
    let level = opts.bump_type.unwrap_or_default();
    let target_tag = match pending_version(dir)? {
        PendingCheck::BelowLatest { manifest, latest } => {
            return Ok(ReleaseState::BelowLatest {
                manifest: version::format_file_version(&manifest),
                latest: version::format_tag(&latest),
            });
        }
        // An untagged version inherited from the default branch: the branch bumps again
        // from it, exactly as `classify_gated_feature` will decide once it is cut.
        PendingCheck::Pending(inherited) => version::format_tag(&version::bump_version(&inherited, level)),
        PendingCheck::NotPending => compute_target_tag(dir, level)?,
    };
    Ok(ReleaseState::GatedStandalone {
        branch: standalone_branch_name(&target_tag),
        default,
        target_tag,
    })
}

/// The gated standalone row: cut `bump-vX-Y-Z` tracking origin/<default> (or check it out if
/// a prior run already cut it), then classify it like any feature branch from its actual
/// diff and run that row. Existence alone proves nothing: an empty branch is the fresh
/// standalone (`force` on the version commit), a version line is the branch's own bump, and
/// work commits make it a feature branch the bump rides with. Scott's words are quoted in
/// the PR body in every case.
pub(super) fn execute_gated_standalone<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    branch: &str,
    default: &str,
    target_tag: &str,
    ports: &Ports<P, I, R, C>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_gated_standalone: dir={} branch={} default={} target_tag={} dry_run={}",
        dir.display(),
        branch,
        default,
        target_tag,
        opts.dry_run
    );
    let exists = git::local_branch_exists(dir, branch)?;
    let upstream = format!("origin/{default}");

    if opts.dry_run {
        if exists {
            println!("[dry-run] git checkout {branch}  (exists: classified from its own diff)");
        } else {
            println!("[dry-run] git checkout -b {branch} --track {upstream}");
            println!("[dry-run] commit the version bump for {target_tag} on {branch} (--force, a new commit)");
        }
        let title = pr_title(branch, &[format!("Bump version to {target_tag}")]);
        println!("[dry-run] git push --no-follow-tags -u origin {branch}");
        println!(
            "[dry-run] gh pr create --head {branch} --base {default} --title \"{title}\" --body \"<subjects>\\n\\nStandalone release ordered by Scott: \\\"<words>\\\"\\n\\nRelease: rides this PR ({target_tag})\"  (only if no open PR)"
        );
        println!("[dry-run] {GATED_PAUSE_MESSAGE}");
        return Ok(ReleaseReport {
            paused: true,
            dry_run: true,
            ..ReleaseReport::new(target_tag)
        });
    }

    if exists {
        println!("{branch} already exists; checking it out and classifying it from its diff");
        git::checkout(dir, branch)?;
    } else {
        git::checkout_new_tracking(dir, branch, &upstream)?;
    }
    let state = classify_gated_feature(dir, opts, branch.to_string(), default.to_string())?;
    debug!("execute_gated_standalone: {branch} classified state={state:?}");
    execute(dir, opts, config, state, ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn standalone_branch_name_dashes_the_dots() {
        assert_eq!(standalone_branch_name("v0.1.6"), "bump-v0-1-6");
        assert_eq!(standalone_branch_name("v1.20.1"), "bump-v1-20-1");
    }

    #[test]
    fn empty_diff_is_bump_only() {
        assert!(bump_only(&[], &[]));
    }

    #[test]
    fn version_lines_in_root_manifests_are_bump_only() {
        let files = strings(&["Cargo.toml", "Cargo.lock"]);
        let lines = strings(&["-version = \"0.1.5\"", "+version = \"0.1.6\""]);
        assert!(bump_only(&files, &lines));
        let files = strings(&["package.json"]);
        let lines = strings(&["-  \"version\": \"1.0.0\",", "+  \"version\": \"1.0.1\","]);
        assert!(bump_only(&files, &lines));
    }

    #[test]
    fn work_file_dependency_line_or_lockfile_only_is_not_bump_only() {
        let version = strings(&["+version = \"0.1.6\""]);
        assert!(!bump_only(&strings(&["Cargo.toml", "src/main.rs"]), &version));
        assert!(!bump_only(&strings(&["crates/x/Cargo.toml"]), &version));
        let dep = strings(&["+version = \"0.1.6\"", "+serde = \"1\""]);
        assert!(!bump_only(&strings(&["Cargo.toml"]), &dep));
        assert!(!bump_only(&strings(&["Cargo.lock"]), &[]));
    }
}
