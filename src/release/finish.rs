//! `bump finish`: the gated post-merge tag step.

use super::ci::{Ci, ci_gate};
use super::tag::{TagTarget, echo_tag_steps, gate_tag_and_push};
use super::{
    FinishOpts, Installer, Pusher, ReleaseReport, agreed_file_version, echo_install, resolve_install, run_install,
};
use crate::config::{self, Config};
use crate::git::{self, HeadRemote};
use crate::lang::{self, Manifest};
use crate::version;
use crate::{TagState, tag_ladder};
use eyre::{Result, bail};
use log::debug;
use std::path::{Path, PathBuf};

/// Where `bump finish` runs. The default branch can only be checked out in one worktree,
/// so from a feature-branch worktree the release happens in the sibling that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FinishDir {
    /// The current checkout is on the default branch.
    Own,
    /// A sibling worktree (`git worktree list`) has the default branch checked out.
    Sibling(PathBuf),
    /// No worktree holds the default branch: check it out here.
    CheckoutHere,
}

impl FinishDir {
    fn path<'a>(&'a self, dir: &'a Path) -> &'a Path {
        match self {
            FinishDir::Sibling(wt) => wt,
            FinishDir::Own | FinishDir::CheckoutHere => dir,
        }
    }
}

/// Resolve the worktree finish runs in: own checkout -> sibling worktree -> checkout here.
/// Read-only; nothing is checked out until the default branch has been classified.
fn finish_dir(dir: &Path, default: &str) -> Result<FinishDir> {
    debug!("finish_dir: dir={} default={}", dir.display(), default);
    if git::current_branch(dir)? == default {
        return Ok(FinishDir::Own);
    }
    Ok(match git::worktree_for_branch(dir, default)? {
        Some(wt) => FinishDir::Sibling(wt),
        None => FinishDir::CheckoutHere,
    })
}

/// `bump finish`: the gated post-merge tag step the paused `bump release` points to. After
/// the PR merges, finish resolves the worktree holding the default branch (`finish_dir`),
/// refuses tracked changes in the current or that worktree, classifies its default branch
/// against origin BEFORE anything moves (behind fast-forwards; ahead or diverged refuse),
/// then -- reusing `crate::tag_ladder` (the SAME `--tag-only` verification ladder, never a
/// duplicate) -- either runs the CI gate and tags the merged commit (pushing it BY NAME),
/// resumes a local-only tag through the same gate, reports an already-released tag, or
/// refuses (missed bump / gated generic / dirty). Every step after resolution, `bump.yml`
/// and install included, runs in the resolved worktree.
///
/// The DIFFERENCE from `bump --tag-only`: `--tag-only` only PRINTS the push command; finish
/// EXECUTES the tag push via the `Pusher` port (by explicit name) and then runs install,
/// and it reaches the merged tip itself. NO tag is ever created on an unconfirmed commit --
/// `gate_tag_and_push` requires green CI and sha == origin/<default>.
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
    if !git::is_git_repo(dir) {
        bail!("not a git repository: {}", dir.display());
    }

    let default = git::remote_default_branch(dir)?;
    let place = finish_dir(dir, &default)?;
    let work = place.path(dir);
    debug!("finish: place={:?} work={}", place, work.display());

    // Tracked changes in EITHER worktree: the checkout / fast-forward would clobber them.
    // Untracked files aren't a reason to refuse: finish never stages or commits anything
    // (it only tags), so a stray file can't ride onto the release; if it collides with a
    // path the checkout would create, `git checkout` itself will say so. Refuse before ANY
    // mutation, with the one exact fix.
    if git::has_tracked_changes(dir)? {
        bail!(
            "the working tree has uncommitted tracked changes; bump finish checks out or \
             fast-forwards the default branch, which would clobber them.\n\
             Commit or stash your changes first, then bump finish"
        );
    }
    if work != dir && git::has_tracked_changes(work)? {
        bail!(
            "{} (the worktree holding {default}) has uncommitted tracked changes; bump finish \
             fast-forwards {default} there, which would clobber them.\n\
             Commit or stash them in {} first, then bump finish",
            work.display(),
            work.display()
        );
    }

    // Generic repo (no version-bearing manifest): finish cannot derive a version to tag.
    // Gated generic is unsupported per the design's Resolved Decisions -- fail closed.
    let manifests = lang::detect(work)?;
    if manifests.is_empty() {
        bail!(
            "this repo has no version-bearing manifest (generic).\n\
             Gated generic repos are unsupported: bump finish cannot derive a version without a manifest."
        );
    }

    if opts.dry_run {
        let config = config::load(work)?;
        return finish_dry_run(work, &place, opts, &config, &manifests, &default);
    }

    if let FinishDir::Sibling(wt) = &place {
        println!("{default} is checked out in {}; finishing there", wt.display());
    }
    reach_merged_tip(work, &place, &default)?;
    let config = config::load(work)?;

    // Reuse the --tag-only verification ladder (clean-tree, on-default, HEAD==origin,
    // manifest-version -> tag, remote-then-local existence). The consumer decides the
    // action; the ladder only classifies.
    let check = tag_ladder(work)?;
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
            gate_tag_and_push(work, gate, &target, check.head.clone(), pusher, ci)?;
            if resumed {
                println!("resumed release: pushed {tag} on {default}");
            } else {
                println!("released {tag} on {default}");
            }
            let install_command = run_install(work, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                resumed,
                install_command,
                ..ReleaseReport::new(&tag)
            })
        }
        // The tag exists on the REMOTE. `remote_tag_sha` (behind the ladder) returns the
        // annotated TAG-OBJECT sha for an exact refspec, so the ladder can't tell an
        // at-HEAD remote tag from an at-other one -- resolve the tag's actual COMMIT here.
        // At the merged tip -> already released: the install still runs, so a re-run after
        // "tag pushed, install failed" installs (`--no-install` skips it). Elsewhere -> the
        // version wasn't bumped for this merge (missed bump).
        TagState::RemoteAtHead | TagState::RemoteAtOther(_) => match git::remote_tag_commit(work, &tag)? {
            Some(commit) if commit == check.head => {
                println!("already released {tag}");
                let install_command = run_install(work, &opts.install, &config, installer)?;
                Ok(ReleaseReport {
                    install_command,
                    ..ReleaseReport::new(&tag)
                })
            }
            _ => missed_bump(&default),
        },
        // The tag exists LOCALLY at a commit OTHER than the merged tip: the version equals
        // the last released tag, so nothing new merged with a bump.
        TagState::LocalAtOther(_) => missed_bump(&default),
    }
}

/// Bring the resolved worktree's default branch to origin's merged tip. The local default
/// is classified against a fresh fetch BEFORE any checkout or pull: behind fast-forwards,
/// equal does nothing, ahead or diverged refuse with the worktree untouched (a pull would
/// otherwise die in git's own error, or merge local-only commits into the release). A
/// missing local default is created from origin by the checkout.
fn reach_merged_tip(work: &Path, place: &FinishDir, default: &str) -> Result<()> {
    debug!(
        "reach_merged_tip: work={} place={:?} default={}",
        work.display(),
        place,
        default
    );
    git::fetch_branch(work, default)?;
    let relation = git::compare_branch_to_remote(work, default)?;
    debug!("reach_merged_tip: relation={:?}", relation);
    match relation {
        None | Some(HeadRemote::Equal) | Some(HeadRemote::Behind) => {}
        Some(HeadRemote::Ahead) => {
            let rescue = format!(
                "stranded-{}",
                short_sha(&git::rev_parse(work, &format!("refs/heads/{default}"))?)
            );
            let commands = match place {
                FinishDir::CheckoutHere => format!(
                    "  git branch {rescue} {default}\n  git branch -f {default} origin/{default}\n  bump finish"
                ),
                FinishDir::Own | FinishDir::Sibling(_) => format!(
                    "  cd {}\n  git branch {rescue}\n  git reset --hard origin/{default}\n  bump finish",
                    work.display()
                ),
            };
            bail!(
                "local {default} has commits that are NOT on origin/{default}; they never landed, and on a \
                 gated repo they can only land through a PR.\n\
                 bump finish refuses to reset history; move them to a branch yourself, then re-run:\n{commands}"
            );
        }
        Some(HeadRemote::Diverged) => {
            let command = match place {
                FinishDir::CheckoutHere => format!("git checkout {default} && git pull --rebase origin {default}"),
                FinishDir::Own | FinishDir::Sibling(_) => {
                    format!("cd {} && git pull --rebase origin {default}", work.display())
                }
            };
            bail!(
                "local {default} has diverged from origin/{default}; a fast-forward cannot reconcile it.\n\
                 Run: {command}, then re-run bump finish"
            );
        }
    }
    if *place == FinishDir::CheckoutHere {
        git::checkout(work, default)?;
    }
    if relation == Some(HeadRemote::Behind) {
        git::pull_ff_only(work, default)?;
    }
    Ok(())
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// The missed-bump refusal (finish table row 2): a commit merged to the default branch
/// without a version bump, so origin/<default>'s version still equals the last tag. The
/// bump rides the NEXT feature PR.
fn missed_bump(default: &str) -> Result<ReleaseReport> {
    bail!(
        "no untagged version on {default}: bump never rode this merge, and bump rides a feature PR.\n\
         If Scott already ordered a standalone release in this session, run bump release on {default} with \
         `--standalone \"<his exact words>\"`. Otherwise STOP and report; do not invent an order."
    )
}

/// `-n` dry run for `bump finish`: echo every command it would run and mutate NOTHING (no
/// checkout, no pull, no fetch). `dir` is the resolved worktree. The reported tag is read
/// from its CURRENT manifest version (a best-effort preview; the real run tags the merged
/// version after the fast-forward).
fn finish_dry_run(
    dir: &Path,
    place: &FinishDir,
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
    match place {
        FinishDir::Own => println!("[dry-run] finishing in this checkout ({default})"),
        FinishDir::Sibling(wt) => println!(
            "[dry-run] {default} is checked out in {}; finishing there",
            wt.display()
        ),
        FinishDir::CheckoutHere => println!("[dry-run] git checkout {default}"),
    }
    println!("[dry-run] git fetch origin {default}  (refuse if local {default} is ahead of or diverged from origin)");
    println!("[dry-run] git pull --ff-only origin {default}  (only if local {default} is behind)");
    println!("[dry-run] (tag-only ladder: require HEAD == origin/{default} before tagging)");
    println!("[dry-run] (only if the merged version is untagged; already released runs only the install:)");
    echo_tag_steps(&tag, default, &ci_gate(opts.ci_gate, opts.ci_timeout), false);
    echo_install(dir, &opts.install, &install_command);
    Ok(ReleaseReport {
        install_command,
        dry_run: true,
        ..ReleaseReport::new(&tag)
    })
}
