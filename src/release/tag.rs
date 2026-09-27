//! Tag placement: `gate_tag_and_push`, the only place a release tag is created and pushed,
//! and the re-verify helpers it uses (design doc, "Tag placement and the two re-verifies").

use super::Pusher;
use super::ci::{CI_POLL_INTERVAL, Ci, CiGate, wait_for_green};
use crate::git;
use crate::lang::{self, ProjectType};
use crate::version;
use eyre::{Result, bail};
use log::debug;
use semver::Version;
use std::path::Path;

/// What `gate_tag_and_push` tags and where: the tag, the version the manifest at the
/// tagged sha must carry, the default branch it must equal, and the verb a refusal tells
/// the operator to re-run.
pub(super) struct TagTarget<'a> {
    pub(super) tag: &'a str,
    pub(super) version: &'a Version,
    pub(super) default: &'a str,
    pub(super) rerun: &'a str,
}

/// The manifest version committed at `sha`, parsed.
fn version_at(dir: &Path, sha: &str) -> Result<Option<Version>> {
    Ok(git::manifest_version_at(dir, sha)?.and_then(|v| version::parse_version(&v).ok()))
}

/// Human form of a manifest version read for a refusal message.
fn describe_version(version: &Option<Version>) -> String {
    version
        .as_ref()
        .map(version::format_file_version)
        .unwrap_or_else(|| "no version".to_string())
}

/// The ONLY place a release tag is created and pushed. Starting from `start_sha`:
/// 1. `wait_for_green` on the sha;
/// 2. fetch origin/<default> fresh: the sha must EQUAL its tip. A tip that moved and still
///    carries the tag's version restarts the gate on the new tip; a different version
///    refuses with no tag;
/// 3. the manifest at the sha must carry the tag's version (a generic repo has no manifest:
///    its version lives in tags alone, so this sub-check is skipped and a moved tip, which
///    no manifest can prove is the same release, refuses);
/// 4. create the annotated tag ON THAT SHA (a local tag already there is kept);
/// 5. fetch fresh again: the sha must still equal the tip, else refuse and leave the local
///    tag for the local-tag resume row;
/// 6. push the tag by name.
///
/// Returns the sha that was tagged.
pub(super) fn gate_tag_and_push<P: Pusher, C: Ci>(
    dir: &Path,
    gate: CiGate,
    target: &TagTarget,
    start_sha: String,
    pusher: &P,
    ci: &C,
) -> Result<String> {
    let TagTarget {
        tag,
        version,
        default,
        rerun,
    } = *target;
    debug!(
        "gate_tag_and_push: dir={} tag={} default={} start_sha={}",
        dir.display(),
        tag,
        default,
        start_sha
    );

    let generic = lang::detect_project_type(dir) == ProjectType::Generic;
    let mut sha = start_sha;
    loop {
        wait_for_green(dir, &sha, gate, ci, target)?;
        let tip = git::remote_tip(dir, default)?;
        if tip == sha {
            break;
        }
        if generic {
            bail!(
                "origin/{default} moved from {sha} to {tip} during the CI wait, and a generic repo has no manifest \
                 to prove the new tip is still {tag}. NO tag was created.\n\
                 Run: git pull --ff-only origin {default}, then {rerun}"
            );
        }
        let at_tip = version_at(dir, &tip)?;
        if at_tip.as_ref() != Some(version) {
            bail!(
                "origin/{default} moved from {sha} to {tip} during the CI wait, and the manifest there carries {}, \
                 not {tag}. NO tag was created.\n\
                 Run: git pull --ff-only origin {default}, then {rerun}",
                describe_version(&at_tip)
            );
        }
        println!(
            "origin/{default} moved to {tip} during the CI wait and still carries {tag}; re-running the CI gate on the new tip"
        );
        sha = tip;
    }

    if !generic {
        let at_sha = version_at(dir, &sha)?;
        if at_sha.as_ref() != Some(version) {
            bail!(
                "the manifest at {sha} carries {}, not {tag}; NO tag was created.",
                describe_version(&at_sha)
            );
        }
    }

    if git::tag_exists(dir, tag)? {
        let at = git::tag_sha(dir, tag)?;
        if at != sha {
            bail!(
                "tag {tag} exists locally at {at}, but the verified tip of origin/{default} is {sha}; NO tag was pushed.\n\
                 Run: git tag -d {tag}, then {rerun}"
            );
        }
        debug!("gate_tag_and_push: local {tag} already at {sha}; pushing it");
    } else {
        git::create_tag(dir, tag, &format!("Release {tag}"), &sha)?;
    }

    let tip = git::remote_tip(dir, default)?;
    if tip != sha {
        bail!(
            "origin/{default} moved from {sha} to {tip} between creating {tag} and pushing it; {tag} was NOT pushed \
             and stays local at {sha}.\n\
             Run: {rerun} (it re-runs the CI gate on the tip; if origin/{default} no longer carries {sha}, \
             run git tag -d {tag} first)"
        );
    }
    pusher.push_tag(dir, tag)?;
    if sha != git::head_sha(dir)? {
        println!(
            "tagged {sha}, the tip of origin/{default}; local HEAD is behind it: git pull --ff-only origin {default}"
        );
    }
    Ok(sha)
}

/// Echo the CI gate + tag steps for `-n` dry-run.
pub(super) fn echo_tag_steps(tag: &str, default: &str, gate: &CiGate, local_tag_present: bool) {
    if gate.enabled {
        println!(
            "[dry-run] wait for green CI on the sha (check-runs + commit status every {}s, up to {}s)",
            CI_POLL_INTERVAL.as_secs(),
            gate.timeout.as_secs()
        );
    } else {
        println!("[dry-run] CI gate: SKIPPED (--no-ci-gate)");
    }
    println!("[dry-run] git fetch origin {default}  (the sha must equal origin/{default} and carry {tag})");
    if local_tag_present {
        println!("[dry-run] (local tag {tag} already present at the sha)");
    } else {
        println!("[dry-run] git tag -a {tag} <sha> -m \"Release {tag}\"");
    }
    println!("[dry-run] git fetch origin {default}  (re-verify before the push)");
    println!("[dry-run] git push --no-follow-tags origin {tag}");
}
