//! `bump finish`: the gated post-merge tag step.

use super::ci::{Ci, ci_gate};
use super::tag::{TagTarget, echo_tag_steps, gate_tag_and_push};
use super::{
    FinishOpts, Installer, Pusher, ReleaseReport, agreed_file_version, echo_install, resolve_install, run_install,
};
use crate::config::{self, Config};
use crate::git;
use crate::lang::{self, Manifest};
use crate::version;
use crate::{TagState, tag_ladder};
use eyre::{Result, bail};
use log::debug;
use std::path::Path;

/// `bump finish`: the gated post-merge tag step the paused `bump release` points to. After
/// the PR merges, finish checks out the default branch, fast-forwards to the merged tip,
/// then -- reusing `crate::tag_ladder` (the SAME `--tag-only` verification ladder, never a
/// duplicate) -- either runs the CI gate and tags the merged commit (pushing it BY NAME),
/// resumes a local-only tag through the same gate, no-ops an already-released tag, or
/// refuses (missed bump / gated generic / dirty).
///
/// The DIFFERENCE from `bump --tag-only`: `--tag-only` only PRINTS the push command; finish
/// EXECUTES the tag push via the `Pusher` port (by explicit name) and then runs install,
/// and it does the checkout + `pull --ff-only` up front. NO tag is ever created on an
/// unconfirmed commit -- `gate_tag_and_push` requires green CI and sha == origin/<default>.
pub fn finish<P: Pusher, I: Installer, C: Ci>(
    dir: &Path,
    opts: &FinishOpts,
    pusher: &P,
    installer: &I,
    ci: &C,
) -> Result<ReleaseReport> {
    debug!(
        "finish: dir={} dry_run={} install={:?} ci_gate={} ci_timeout={:?}",
        dir.display(),
        opts.dry_run,
        opts.install,
        opts.ci_gate,
        opts.ci_timeout
    );
    let config = config::load(dir)?;

    if !git::is_git_repo(dir) {
        bail!("not a git repository: {}", dir.display());
    }

    // Dirty tree: checking out the default branch would clobber TRACKED changes. Untracked
    // files aren't a reason to refuse: finish never stages or commits anything (it only
    // tags), so a stray file can't ride onto the release; if it collides with a path the
    // checkout would create, `git checkout` itself will say so. Refuse before ANY mutation,
    // with the one exact fix.
    if git::has_tracked_changes(dir)? {
        bail!(
            "the working tree has uncommitted tracked changes; bump finish checks out the \
             default branch, which would clobber them.\n\
             Commit or stash your changes first, then bump finish"
        );
    }

    // Generic repo (no version-bearing manifest): finish cannot derive a version to tag.
    // Gated generic is unsupported per the design's Resolved Decisions -- fail closed.
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        bail!(
            "this repo has no version-bearing manifest (generic).\n\
             Gated generic repos are unsupported: bump finish cannot derive a version without a manifest."
        );
    }

    let default = git::remote_default_branch(dir)?;

    if opts.dry_run {
        return finish_dry_run(dir, opts, &config, &manifests, &default);
    }

    // Reach the merged tip: checkout the default branch, then fast-forward to origin.
    // `pull --ff-only` does its own fetch; the shared ladder re-fetches before comparing.
    git::checkout(dir, &default)?;
    git::pull_ff_only(dir, &default)?;

    // Reuse the --tag-only verification ladder (clean-tree, on-default, HEAD==origin,
    // manifest-version -> tag, remote-then-local existence). The consumer decides the
    // action; the ladder only classifies.
    let check = tag_ladder(dir)?;
    let tag = check.tag.clone();
    debug!("finish: tag={} state={:?}", tag, check.state);

    match check.state {
        // Local-only tag at the merged commit (a prior run died before/during the tag
        // push: RESUME, never "already released"), or no tag yet for the merged version.
        // Both run the CI gate on the merged sha, then tag it (a local tag already there
        // is kept) and push by name.
        TagState::LocalAtHead | TagState::Absent => {
            let resumed = matches!(check.state, TagState::LocalAtHead);
            let version = version::parse_version(&tag)?;
            let target = TagTarget {
                tag: &tag,
                version: &version,
                default: &default,
                rerun: "bump finish",
            };
            let gate = ci_gate(opts.ci_gate, opts.ci_timeout);
            gate_tag_and_push(dir, gate, &target, check.head.clone(), pusher, ci)?;
            if resumed {
                println!("resumed release: pushed {tag} on {default}");
            } else {
                println!("released {tag} on {default}");
            }
            let install_command = run_install(dir, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                resumed,
                install_command,
                ..ReleaseReport::new(&tag)
            })
        }
        // The tag exists on the REMOTE. `remote_tag_sha` (behind the ladder) returns the
        // annotated TAG-OBJECT sha for an exact refspec, so the ladder can't tell an
        // at-HEAD remote tag from an at-other one -- resolve the tag's actual COMMIT here.
        // At the merged tip -> already released (NO-OP). Elsewhere -> the version wasn't
        // bumped for this merge (missed bump).
        TagState::RemoteAtHead | TagState::RemoteAtOther(_) => match git::remote_tag_commit(dir, &tag)? {
            Some(commit) if commit == check.head => {
                println!("already released {tag}");
                Ok(ReleaseReport::new(&tag))
            }
            _ => missed_bump(&default),
        },
        // The tag exists LOCALLY at a commit OTHER than the merged tip: the version equals
        // the last released tag, so nothing new merged with a bump.
        TagState::LocalAtOther(_) => missed_bump(&default),
    }
}

/// The missed-bump refusal (finish table row 2): a commit merged to the default branch
/// without a version bump, so origin/<default>'s version still equals the last tag. The
/// bump rides the NEXT feature PR.
fn missed_bump(default: &str) -> Result<ReleaseReport> {
    bail!("no untagged version on {default}; bump rides a feature PR -- run bump release on a branch")
}

/// `-n` dry run for `bump finish`: echo every command it would run and mutate NOTHING (no
/// checkout, no pull, no fetch). The reported tag is read from the CURRENT manifest version
/// (a best-effort preview; the real run tags the merged version after the fast-forward).
fn finish_dry_run(
    dir: &Path,
    opts: &FinishOpts,
    config: &Config,
    manifests: &[Box<dyn Manifest>],
    default: &str,
) -> Result<ReleaseReport> {
    debug!("finish_dry_run: dir={} default={}", dir.display(), default);
    let install_command = resolve_install(dir, &opts.install, config);
    let tag = match agreed_file_version(manifests)? {
        Some(v) => version::format_tag(&v),
        None => "vX.Y.Z".to_string(),
    };
    println!("[dry-run] git checkout {default}");
    println!("[dry-run] git pull --ff-only origin {default}");
    println!("[dry-run] (tag-only ladder: require HEAD == origin/{default} before tagging)");
    println!("[dry-run] (only if the merged version is untagged:)");
    echo_tag_steps(&tag, default, &ci_gate(opts.ci_gate, opts.ci_timeout), false);
    echo_install(&install_command);
    Ok(ReleaseReport {
        install_command,
        dry_run: true,
        ..ReleaseReport::new(&tag)
    })
}
