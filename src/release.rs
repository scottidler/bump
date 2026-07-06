//! `bump release` -- the release-verb state machine (UNGATED flow Phase 5, GATED flow
//! Phase 6).
//!
//! This module absorbs the bash release driver's mechanical steps behind ONE verb that
//! inspects the repo's typed state and either executes the single correct sequence or
//! refuses with the exact next command. It operates entirely on bump's typed internals
//! (`github::Gate`, `VersionAction`, the `git::*` helpers, the `lang` adapter seam) --
//! ZERO stdout scraping of any `bump`/`git` output.
//!
//! Scope: BOTH the ungated rows (Phase 5) and the gated / feature-branch / PR rows
//! (Phase 6) of the `bump release` state table, plus `bump finish` (Phase 7, the
//! post-merge tag step). Phase 8 wires the `bump release` / `bump finish` clap
//! subcommands (`main.rs::dispatch_release`/`dispatch_finish`) and the
//! `--install`/`--no-install` flags to the callable `release(dir, opts, pusher,
//! installer, pr)` / `finish(dir, opts, pusher, installer)` functions; tests still drive
//! them via injected `Pusher`/`Installer`/`Pr` doubles.
//!
//! GATED invariant (Phase 6): NO tag is ever created or pushed in the gated `release`
//! flow -- the version commit rides the feature branch (internal `--no-tag`), the branch
//! is pushed with `--no-follow-tags` so a stray local tag can't ride, a PR is opened if
//! none is open, and the verb PAUSES (exit 0) for the human to merge. Tagging the merged
//! commit is `bump finish`'s job (Phase 7), never `release`'s.
//!
//! Strengthened ordering invariant (git.md, enforced in code below, NOT prose): a tag is
//! created ONLY after the commit it points to is confirmed on `origin/<default>`. Plain
//! `bump` tags local HEAD before pushing; `bump release` inverts that -- version commit
//! -> push branch -> confirm on origin -> THEN tag -> push tag by name -- so a rejected
//! branch push can never strand a local tag on an unpushed commit.

use crate::cli::Cli;
use crate::config::{self, Config};
use crate::git::{self, HeadRemote};
use crate::github::{self, Gate};
use crate::lang::{self, Manifest, ManifestVersion, ProjectType};
use crate::version::{self, BumpType};
use crate::{DEFAULT_UNTOUCHED_VERSION, TagState, determine_version_action, process_directory, tag_ladder};
use eyre::{Context, Result, bail};
use log::debug;
use semver::Version;
use std::path::Path;
use std::process::Command;

/// The default install command when none is configured and a Cargo manifest is present.
const DEFAULT_INSTALL_COMMAND: &str = "cargo install --path .";

/// The pause message printed at the end of the gated `release` flow: the verb has done
/// everything mechanical up to (and including) opening the PR, and now hands control back
/// to the human/agent to merge and then run `bump finish`.
const GATED_PAUSE_MESSAGE: &str = "merge the PR, then run: bump finish";

/// How the install step is resolved. Precedence (general.md): CLI override > config
/// `install` > default (`cargo install --path .` iff a `Cargo.toml` is present) > skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChoice {
    /// Explicit override (the future `--install "<cmd>"`): run this exact command.
    Command(String),
    /// Explicit opt-out (the future `--no-install`): skip the install step.
    Skip,
    /// Neither flag given: config `install` > default-if-Cargo > skip.
    Auto,
}

/// Inputs to a `bump release` invocation. A plain opts struct so tests (and the Phase 8
/// CLI) drive the verb without a clap dependency here.
#[derive(Debug, Clone)]
pub struct ReleaseOpts {
    /// The bump level (patch/minor/major) for a fresh release.
    pub bump_type: BumpType,
    /// `-n`: echo every command that would run and execute NOTHING.
    pub dry_run: bool,
    /// How to resolve the post-release install step.
    pub install: InstallChoice,
}

/// Inputs to a `bump finish` invocation. No bump level -- finish tags the version already
/// merged onto the default branch; it NEVER computes a bump. A plain opts struct so tests
/// (and the Phase 8 CLI) drive the verb without a clap dependency here.
#[derive(Debug, Clone)]
pub struct FinishOpts {
    /// `-n`: echo every command that would run and execute NOTHING.
    pub dry_run: bool,
    /// How to resolve the post-release install step.
    pub install: InstallChoice,
}

/// The outcome of a successful `release()` (refusals are `Err`). Lets callers/tests
/// assert what happened without scraping stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReport {
    /// The tag involved. In the ungated flow this is the tag CREATED and/or pushed; in
    /// the gated flow it is the target/riding version's tag for reporting ONLY -- NO tag
    /// is ever created in the gated flow (that is `bump finish`'s job).
    pub tag: String,
    /// True when this completed a partial-release RESUME rather than a fresh release.
    pub resumed: bool,
    /// True when this was a GATED run that pushed the branch, ensured a PR, and PAUSED
    /// (exit 0) for the human to merge -- no tag, no install.
    pub paused: bool,
    /// The resolved install command that ran (`None` = install skipped, always `None`
    /// on a paused gated run).
    pub install_command: Option<String>,
    /// True when this was a `-n` dry run (nothing was mutated).
    pub dry_run: bool,
}

/// The typed state the repo is in, as classified from git + gate facts. Each refusal
/// row carries the data its exact-next-command message needs; execution turns each into
/// either the correct mutation sequence or a loud, actionable refusal.
#[derive(Debug)]
enum ReleaseState {
    /// Ungated, on default, ahead of origin, clean: fresh release.
    Release { target_tag: String, default: String },
    /// Ungated RESUME: origin carries the version, the remote tag is missing (a prior
    /// run died between branch push and tag push). `local_tag_present` distinguishes the
    /// two sub-states (`git::tag_exists`): present -> push only; absent -> create + push.
    Resume {
        tag: String,
        default: String,
        local_tag_present: bool,
    },
    /// Ungated, not on the default branch.
    NotOnDefault { default: String, current: String },
    /// Ungated, behind (or diverged from) origin.
    Behind { default: String },
    /// Ungated, nothing ahead and the version is already tagged.
    Nothing { default: String },
    /// Dirty working tree.
    DirtyTree,
    /// Detached HEAD.
    DetachedHead,
    /// Gated, on a feature branch, version == last tag: fresh gated release. Bump rides
    /// the branch (`--no-tag`), push branch, ensure PR, PAUSE.
    GatedFresh { branch: String, target_tag: String },
    /// Gated, on a feature branch, version ALREADY bumped (idempotent re-run). Skip the
    /// re-bump, ensure the branch is pushed + a PR is open, PAUSE.
    GatedAlreadyBumped { branch: String, tag: String },
    /// Gated re-run whose requested level (`-m`/`-M`) implies a DIFFERENT version than the
    /// one already riding the branch. REFUSE naming BOTH (never silently keep either).
    GatedLevelMismatch { riding: String, implied: String },
    /// Gated, on the local default branch, with commits NOT on origin (stranded). REFUSE
    /// with the LITERAL rescue commands; the verb never invents a branch or resets.
    GatedStranded { default: String, suggested_branch: String },
    /// Gated, on the default branch, clean, HEAD == origin. REFUSE: bump rides a PR.
    GatedDefaultClean { default: String },
    /// Gated + generic (no manifest): unsupported -- `bump finish` cannot derive a target
    /// version without a manifest, so both verbs refuse (Resolved Decisions).
    GatedGeneric,
    /// Gate probe inconclusive: `release` pushes, so it FAILS CLOSED.
    Unknown { reason: String },
}

/// Pushes a branch / tag to origin. A port so tests can record ordering and inject a
/// rejected push without touching a real remote for the failure case.
pub trait Pusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()>;
    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()>;
    /// Push a FEATURE branch with `--no-follow-tags -u` (the gated flow). Separate from
    /// `push_branch` so the gated `--no-follow-tags` invariant can't leak into the ungated
    /// default-branch push, and vice versa.
    fn push_feature_branch(&self, dir: &Path, branch: &str) -> Result<()>;
}

/// Runs the post-release install command. A port so tests assert the RESOLVED command
/// without executing a real (slow, outward) `cargo install`.
pub trait Installer {
    fn install(&self, dir: &Path, command: &str) -> Result<()>;
}

/// The PR seam for the gated flow. A port (preferred over the doc's optional
/// `BUMP_PR_PROBE` env seam for consistency with `Pusher`/`Installer`) so tests inject a
/// fake `gh` without a real GitHub round-trip.
///
/// `open_pr_exists` is the Phase-0 open-PR probe (`gh pr list --head <branch> --state
/// open --json number`, NOT `gh pr view`); `create_pr` is `gh pr create --fill`, only
/// ever called when `open_pr_exists` returns false.
pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    fn create_pr(&self, dir: &Path, branch: &str) -> Result<()>;
}

/// Production `Pr`: the real `gh` PR operations (list-probe + `--fill` create).
pub struct GhPr;

impl Pr for GhPr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool> {
        github::open_pr_exists(dir, branch)
    }

    fn create_pr(&self, dir: &Path, branch: &str) -> Result<()> {
        github::create_pr(dir, branch)
    }
}

/// Production `Pusher`: real `git push origin <name>` by explicit name (never `--tags`,
/// never `--follow-tags`, never `--force`).
pub struct GitPusher;

impl Pusher for GitPusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        git::push_branch(dir, branch)
    }

    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()> {
        git::push_tag(dir, tag)
    }

    fn push_feature_branch(&self, dir: &Path, branch: &str) -> Result<()> {
        git::push_feature_branch(dir, branch)
    }
}

/// The external-effect ports bundled together, so the execution functions stay under the
/// argument-count limit and the seams travel as one unit (rules/rust.md `Deps`).
struct Ports<'a, P: Pusher, I: Installer, R: Pr> {
    pusher: &'a P,
    installer: &'a I,
    pr: &'a R,
}

/// Production `Installer`: run the repo-committed install command through the shell (same
/// trust model as `.otto.yml`).
pub struct ShellInstaller;

impl Installer for ShellInstaller {
    fn install(&self, dir: &Path, command: &str) -> Result<()> {
        debug!("ShellInstaller::install: dir={} command={}", dir.display(), command);
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(dir)
            .status()
            .with_context(|| format!("failed to run install command: {command}"))?;
        if !status.success() {
            bail!("install command failed: {command}");
        }
        Ok(())
    }
}

/// `bump release`: classify the repo's state, then execute the one correct ungated
/// sequence or refuse with the exact next command (non-zero exit at the caller).
pub fn release<P: Pusher, I: Installer, R: Pr>(
    dir: &Path,
    opts: &ReleaseOpts,
    pusher: &P,
    installer: &I,
    pr: &R,
) -> Result<ReleaseReport> {
    debug!(
        "release: dir={} dry_run={} bump_type={:?} install={:?}",
        dir.display(),
        opts.dry_run,
        opts.bump_type,
        opts.install
    );
    let config = config::load(dir)?;
    let state = classify(dir, opts)?;
    debug!("release: classified state={:?}", state);
    let ports = Ports { pusher, installer, pr };
    execute(dir, opts, &config, state, &ports)
}

/// Inspect git + gate facts and return the typed state. Read-only except for the
/// preconditional `git fetch origin <default>` (updates the remote-tracking ref).
fn classify(dir: &Path, opts: &ReleaseOpts) -> Result<ReleaseState> {
    debug!("classify: dir={}", dir.display());

    if !git::is_git_repo(dir) {
        bail!("not a git repository: {}", dir.display());
    }
    if git::has_uncommitted_changes(dir)? {
        return Ok(ReleaseState::DirtyTree);
    }

    let current = git::current_branch(dir)?;
    if current == "HEAD" {
        return Ok(ReleaseState::DetachedHead);
    }

    // Gate FIRST: `release` pushes, so an Unknown verdict fails closed (unlike plain
    // `bump`, which warn-and-proceeds because it never pushes).
    match github::detect(dir) {
        Gate::Unknown(reason) => return Ok(ReleaseState::Unknown { reason }),
        Gate::Gated(_) => return classify_gated(dir, opts, &current),
        Gate::Ungated => {}
    }

    let default = git::remote_default_branch(dir)?;
    git::fetch_branch(dir, &default)?;

    if current != default {
        return Ok(ReleaseState::NotOnDefault { default, current });
    }

    match git::compare_head_to_remote(dir, &default)? {
        HeadRemote::Behind | HeadRemote::Diverged => Ok(ReleaseState::Behind { default }),
        HeadRemote::Ahead => {
            let target_tag = compute_target_tag(dir, opts.bump_type)?;
            Ok(ReleaseState::Release { target_tag, default })
        }
        HeadRemote::Equal => classify_equal(dir, default),
    }
}

/// HEAD == origin/<default> and the tree is clean: either a partial-release RESUME (the
/// version is committed+pushed but the remote tag is missing) or truly nothing to do.
fn classify_equal(dir: &Path, default: String) -> Result<ReleaseState> {
    debug!("classify_equal: dir={} default={}", dir.display(), default);
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        // Generic repo (version lives in tags): with nothing ahead, there is nothing to
        // release and no manifest version to resume-tag.
        return Ok(ReleaseState::Nothing { default });
    }
    let version = match lang::agreed_version(&manifests)? {
        ManifestVersion::Static(v) => v,
        ManifestVersion::Missing => return Ok(ReleaseState::Nothing { default }),
        ManifestVersion::Dynamic(reason) => bail!(
            "cannot release: {reason}. The version is owned elsewhere; remove the \
             dynamic declaration to let bump manage it."
        ),
    };
    let tag = version::format_tag(&version);

    // The remote tag is the source of truth for "released": present on origin => done.
    if git::remote_tag_sha(dir, &tag)?.is_some() {
        return Ok(ReleaseState::Nothing { default });
    }

    // Origin carries the version but the remote tag is absent -> RESUME. A local tag may
    // or may not exist; a local tag at a DIFFERENT commit is manual surgery, not resume.
    let head = git::head_sha(dir)?;
    let local_tag_present = if git::tag_exists(dir, &tag)? {
        let sha = git::tag_sha(dir, &tag)?;
        if sha != head {
            bail!(
                "tag {tag} exists locally at {sha}, not HEAD ({head}); \
                 resolving that is manual tag surgery, not bump's job."
            );
        }
        true
    } else {
        false
    };
    Ok(ReleaseState::Resume {
        tag,
        default,
        local_tag_present,
    })
}

/// Classify a GATED repo. Resolves the remote default and fetches it (like the ungated
/// path), then splits on branch: on the default branch it is either a stranded-commits
/// refusal, a behind refusal, or the "bump rides a PR" refusal; on a feature branch it is
/// a fresh release, an idempotent re-run, a level-mismatch refusal, or generic-unsupported.
fn classify_gated(dir: &Path, opts: &ReleaseOpts, current: &str) -> Result<ReleaseState> {
    debug!("classify_gated: dir={} current={}", dir.display(), current);
    let default = git::remote_default_branch(dir)?;
    git::fetch_branch(dir, &default)?;

    if current == default {
        // On the gated DEFAULT branch: `release` never runs the flow here.
        return match git::compare_head_to_remote(dir, &default)? {
            // Local commits not on origin -> stranded: refuse with the literal rescue.
            HeadRemote::Ahead | HeadRemote::Diverged => {
                let suggested_branch = suggest_rescue_branch(dir)?;
                Ok(ReleaseState::GatedStranded {
                    default,
                    suggested_branch,
                })
            }
            // Stale local default: same fix as the ungated behind row.
            HeadRemote::Behind => Ok(ReleaseState::Behind { default }),
            // Clean and in sync: bump must ride a feature PR, not the default branch.
            HeadRemote::Equal => Ok(ReleaseState::GatedDefaultClean { default }),
        };
    }

    classify_gated_feature(dir, opts, current.to_string())
}

/// Classify a gated repo when HEAD is on a FEATURE branch: fresh vs already-bumped vs
/// level-mismatch vs generic-unsupported.
fn classify_gated_feature(dir: &Path, opts: &ReleaseOpts, branch: String) -> Result<ReleaseState> {
    debug!("classify_gated_feature: dir={} branch={}", dir.display(), branch);
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        // Gated + generic: `bump finish` cannot derive a version without a manifest, so
        // both verbs refuse (Resolved Decisions). Fail closed rather than bump nothing.
        return Ok(ReleaseState::GatedGeneric);
    }

    let current_version = match lang::agreed_version(&manifests)? {
        ManifestVersion::Static(v) => Some(v),
        ManifestVersion::Missing => None,
        ManifestVersion::Dynamic(reason) => bail!(
            "cannot release: {reason}. The version is owned elsewhere; remove the \
             dynamic declaration to let bump manage it."
        ),
    };
    let latest_tag = git::get_latest_tag(dir)?.and_then(|t| version::parse_version(&t).ok());
    let project_type = lang::detect_project_type(dir);

    // Already-bumped detection: the manifest version is AHEAD of the last released tag (a
    // prior gated run's `--no-tag` bump already rode this branch), and it is not the Rust
    // untouched-default 0.1.0 (which defers to the tag -> still a fresh release).
    if let (Some(v), Some(t)) = (&current_version, &latest_tag) {
        let is_untouched_default = project_type == ProjectType::Rust && *v == DEFAULT_UNTOUCHED_VERSION;
        if v != t && !is_untouched_default {
            let implied = version::bump_version(t, opts.bump_type);
            if implied != *v {
                // The requested level implies a DIFFERENT version than the one riding.
                return Ok(ReleaseState::GatedLevelMismatch {
                    riding: version::format_tag(v),
                    implied: version::format_tag(&implied),
                });
            }
            return Ok(ReleaseState::GatedAlreadyBumped {
                branch,
                tag: version::format_tag(v),
            });
        }
    }

    // Fresh: version == last tag (or an initial release). Compute the target the requested
    // level yields via bump's own version rules (no re-derivation, no stdout parsing).
    let target_tag = compute_target_tag(dir, opts.bump_type)?;
    Ok(ReleaseState::GatedFresh { branch, target_tag })
}

/// A deterministic suggested branch name for the stranded-commits rescue, derived from the
/// stranded HEAD's short SHA. Only ever printed in the refusal message -- the verb never
/// creates it.
fn suggest_rescue_branch(dir: &Path) -> Result<String> {
    let head = git::head_sha(dir)?;
    let short: String = head.chars().take(8).collect();
    Ok(format!("stranded-{short}"))
}

/// Compute the tag a fresh release would create, via bump's own `determine_version_action`
/// (no stdout parsing, no re-derivation of the version rules).
fn compute_target_tag(dir: &Path, bump_type: BumpType) -> Result<String> {
    debug!("compute_target_tag: dir={} bump_type={:?}", dir.display(), bump_type);
    let project_type = lang::detect_project_type(dir);
    let manifests = lang::detect(dir)?;
    let file_version = agreed_file_version(&manifests)?;
    let action = determine_version_action(dir, file_version, project_type, bump_type)?;
    Ok(version::format_tag(&action.target_version))
}

/// The single version agreed across all detected manifests, as an `Option<Version>` for
/// `determine_version_action` (empty Vec / `Missing` -> `None`; `Dynamic` -> refuse).
fn agreed_file_version(manifests: &[Box<dyn Manifest>]) -> Result<Option<Version>> {
    if manifests.is_empty() {
        return Ok(None);
    }
    match lang::agreed_version(manifests)? {
        ManifestVersion::Static(v) => Ok(Some(v)),
        ManifestVersion::Missing => Ok(None),
        ManifestVersion::Dynamic(reason) => bail!(
            "cannot release: {reason}. The version is owned elsewhere; remove the \
             dynamic declaration to let bump manage it."
        ),
    }
}

/// Turn a classified state into either the correct mutation sequence or a refusal whose
/// message is the exact next command.
fn execute<P: Pusher, I: Installer, R: Pr>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    state: ReleaseState,
    ports: &Ports<P, I, R>,
) -> Result<ReleaseReport> {
    debug!(
        "execute: dir={} state={:?} dry_run={}",
        dir.display(),
        state,
        opts.dry_run
    );
    match state {
        ReleaseState::Release { target_tag, default } => {
            execute_release(dir, opts, config, &target_tag, &default, ports)
        }
        ReleaseState::Resume {
            tag,
            default,
            local_tag_present,
        } => execute_resume(dir, opts, config, &tag, &default, local_tag_present, ports),
        ReleaseState::NotOnDefault { default, current } => bail!(
            "bump release runs on the default branch '{default}', but you are on '{current}'.\n\
             Run: git checkout {default}, then bump release"
        ),
        ReleaseState::Behind { default } => bail!(
            "{default} is behind origin/{default}; releasing a stale branch would orphan the tag.\n\
             Run: git pull --ff-only origin {default}, then bump release"
        ),
        ReleaseState::Nothing { default } => bail!(
            "nothing to release: nothing ahead of origin/{default} and the version is already tagged.\n\
             Commit a change first, then bump release"
        ),
        ReleaseState::DirtyTree => bail!(
            "the working tree is dirty; bump release only performs the mechanical release on a CLEAN tree.\n\
             Commit or stash your changes first, then bump release"
        ),
        ReleaseState::DetachedHead => bail!(
            "HEAD is detached; bump release runs on the default branch.\n\
             Run: git checkout <default-branch>, then bump release"
        ),
        ReleaseState::Unknown { reason } => bail!(
            "gate status is UNKNOWN ({reason}); bump release pushes, so it refuses to guess (fail closed).\n\
             Run `gh auth status` (or `bump --gates`) once online, then bump release"
        ),
        ReleaseState::GatedFresh { branch, target_tag } => {
            execute_gated(dir, opts, &branch, Some(&target_tag), &target_tag, ports)
        }
        ReleaseState::GatedAlreadyBumped { branch, tag } => execute_gated(dir, opts, &branch, None, &tag, ports),
        ReleaseState::GatedLevelMismatch { riding, implied } => bail!(
            "this branch already carries a version bump to {riding}, but the requested level implies {implied}.\n\
             bump refuses to name two versions: either drop the -m/-M flag to keep {riding}, or reset the branch's \
             bump commit and re-run for {implied}."
        ),
        ReleaseState::GatedStranded {
            default,
            suggested_branch,
        } => bail!(
            "you are on the gated default branch '{default}' with local commits that are NOT on origin/{default}.\n\
             bump release refuses to invent a branch or reset history; move the work to a branch yourself, then re-run:\n  \
             git branch {suggested_branch}\n  \
             git reset --hard origin/{default}\n  \
             git checkout {suggested_branch}\n  \
             bump release"
        ),
        ReleaseState::GatedDefaultClean { default } => bail!(
            "this repo is GATED and you are on the default branch '{default}'; bump rides a feature PR, not the default branch.\n\
             Run: git checkout -b <feature>, commit your change, then bump release"
        ),
        ReleaseState::GatedGeneric => bail!(
            "this repo is GATED and has no version-bearing manifest (generic).\n\
             Gated generic repos are unsupported: bump finish cannot derive a version without a manifest."
        ),
    }
}

/// Fresh ungated release: version commit -> push branch -> confirm on origin -> tag ->
/// push tag by name -> install. The confirm step is the strengthened-ordering guard.
fn execute_release<P: Pusher, I: Installer, R: Pr>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    target_tag: &str,
    default: &str,
    ports: &Ports<P, I, R>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_release: dir={} target_tag={} default={} dry_run={}",
        dir.display(),
        target_tag,
        default,
        opts.dry_run
    );

    if opts.dry_run {
        let install_command = resolve_install(dir, &opts.install, config);
        println!("[dry-run] bump --no-tag  (commit the version bump for {target_tag})");
        println!("[dry-run] git push origin {default}");
        println!("[dry-run] (confirm HEAD is on origin/{default} before tagging)");
        println!("[dry-run] git tag -a {target_tag} -m \"Release {target_tag}\"");
        println!("[dry-run] git push origin {target_tag}");
        echo_install(&install_command);
        return Ok(ReleaseReport {
            tag: target_tag.to_string(),
            resumed: false,
            paused: false,
            install_command,
            dry_run: true,
        });
    }

    // 1. The version commit is the existing `--no-tag` code path (version bump + commit,
    //    no tag). Reused verbatim -- release never re-implements commit/version logic.
    version_commit(dir, opts.bump_type)?;
    // 2. Push the branch FIRST.
    ports.pusher.push_branch(dir, default)?;
    // 3. Confirm it landed on origin BEFORE any tag exists (a rejected push errored above
    //    and we never reach here; a push that reported success but didn't land is caught).
    confirm_on_origin(dir, default)?;
    // 4. Only now create the annotated tag on the confirmed commit.
    let message = format!("Release {target_tag}");
    git::create_tag(dir, target_tag, &message)?;
    // 5. Push the tag BY EXPLICIT NAME.
    ports.pusher.push_tag(dir, target_tag)?;
    println!("Released {target_tag} on {default}");
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        tag: target_tag.to_string(),
        resumed: false,
        paused: false,
        install_command,
        dry_run: false,
    })
}

/// Partial-release RESUME: never re-bump, never claim "already released". Create the
/// annotated tag only if it is absent locally, then push it by name and install.
fn execute_resume<P: Pusher, I: Installer, R: Pr>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    tag: &str,
    default: &str,
    local_tag_present: bool,
    ports: &Ports<P, I, R>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_resume: dir={} tag={} default={} local_tag_present={} dry_run={}",
        dir.display(),
        tag,
        default,
        local_tag_present,
        opts.dry_run
    );

    if opts.dry_run {
        let install_command = resolve_install(dir, &opts.install, config);
        if local_tag_present {
            println!("[dry-run] git push origin {tag}  (local tag already present)");
        } else {
            println!("[dry-run] git tag -a {tag} -m \"Release {tag}\"");
            println!("[dry-run] git push origin {tag}");
        }
        echo_install(&install_command);
        return Ok(ReleaseReport {
            tag: tag.to_string(),
            resumed: true,
            paused: false,
            install_command,
            dry_run: true,
        });
    }

    if !local_tag_present {
        let message = format!("Release {tag}");
        git::create_tag(dir, tag, &message)?;
    }
    // The version is already on origin (that is what makes this a resume), but confirm
    // before pushing the tag so the invariant holds on this path too.
    confirm_on_origin(dir, default)?;
    ports.pusher.push_tag(dir, tag)?;
    println!("Resumed release: pushed {tag} on {default}");
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        tag: tag.to_string(),
        resumed: true,
        paused: false,
        install_command,
        dry_run: false,
    })
}

/// The GATED release flow: on a feature branch, ride the version bump on the branch (fresh
/// only), push the branch with `--no-follow-tags -u`, ensure an OPEN PR exists (list-probe
/// then create), and PAUSE (exit 0) for the human to merge. NO tag is created or pushed
/// here -- tagging the merged commit is `bump finish`'s job (Phase 7).
///
/// `fresh_target` is `Some(target_tag)` for a fresh release (do the `--no-tag` bump) and
/// `None` for an idempotent re-run (the bump already rode the branch -- never re-bump).
/// `report_tag` is the tag reported (fresh target, or the riding version's tag); it is
/// informational and NEVER created.
fn execute_gated<P: Pusher, I: Installer, R: Pr>(
    dir: &Path,
    opts: &ReleaseOpts,
    branch: &str,
    fresh_target: Option<&str>,
    report_tag: &str,
    ports: &Ports<P, I, R>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_gated: dir={} branch={} fresh_target={:?} report_tag={} dry_run={}",
        dir.display(),
        branch,
        fresh_target,
        report_tag,
        opts.dry_run
    );

    if opts.dry_run {
        match fresh_target {
            Some(target) => {
                println!("[dry-run] bump --no-tag  (commit the version bump for {target} on {branch})");
            }
            None => {
                println!("[dry-run] (version already bumped on {branch}; no re-bump)");
            }
        }
        println!("[dry-run] git push --no-follow-tags -u origin {branch}");
        println!("[dry-run] gh pr list --head {branch} --state open --json number  (open-PR probe)");
        println!("[dry-run] gh pr create --fill  (only if no open PR)");
        println!("[dry-run] {GATED_PAUSE_MESSAGE}");
        return Ok(ReleaseReport {
            tag: report_tag.to_string(),
            resumed: false,
            paused: true,
            install_command: None,
            dry_run: true,
        });
    }

    // 1. Fresh: ride the version bump on the branch via the existing `--no-tag` path (no
    //    tag). Idempotent re-run: skip -- the bump already rode the branch.
    if let Some(target) = fresh_target {
        debug!("execute_gated: fresh bump for {target}");
        version_commit(dir, opts.bump_type)?;
    } else {
        debug!("execute_gated: version already bumped on {branch}; skipping re-bump");
    }

    // 2. Push the feature branch with `--no-follow-tags -u` (a stray local tag must not
    //    ride; tagging is `bump finish`'s job on the merged commit).
    ports.pusher.push_feature_branch(dir, branch)?;

    // 3. Ensure an OPEN PR exists: the list-probe FIRST (exit-0 in all cases, reused
    //    branch names read correctly), create ONLY if none is open.
    if ports.pr.open_pr_exists(dir, branch)? {
        println!("open PR already exists for {branch}; not creating another");
    } else {
        ports.pr.create_pr(dir, branch)?;
        println!("opened a PR for {branch}");
    }

    // 4. PAUSE. No tag, no install -- both are `bump finish`'s after the merge.
    println!("{GATED_PAUSE_MESSAGE}");
    Ok(ReleaseReport {
        tag: report_tag.to_string(),
        resumed: false,
        paused: true,
        install_command: None,
        dry_run: false,
    })
}

/// The internal version commit: bump the version file(s) and commit, NO tag. This is
/// exactly `bump --no-tag`'s `process_directory` code path, reused.
fn version_commit(dir: &Path, bump_type: BumpType) -> Result<()> {
    debug!("version_commit: dir={} bump_type={:?}", dir.display(), bump_type);
    let cli = Cli {
        command: None,
        major: bump_type == BumpType::Major,
        minor: bump_type == BumpType::Minor,
        dry_run: false,
        message: None,
        automatic: false,
        force: false,
        no_tag: true,
        tag_only: false,
        gates: false,
        no_verify: false,
        skip_member: Vec::new(),
        directories: Vec::new(),
    };
    process_directory(dir, &cli, bump_type)
}

/// The strengthened-ordering guard: re-fetch and require HEAD == origin/<default> before
/// any tag is created. If the branch push did not land, refuse loudly with no tag.
fn confirm_on_origin(dir: &Path, default: &str) -> Result<()> {
    debug!("confirm_on_origin: dir={} default={}", dir.display(), default);
    git::fetch_branch(dir, default)?;
    match git::compare_head_to_remote(dir, default)? {
        HeadRemote::Equal => Ok(()),
        other => bail!(
            "release aborted: HEAD is not confirmed on origin/{default} ({other:?}); \
             the branch push did not land, so NO tag was created."
        ),
    }
}

/// `bump finish`: the gated post-merge tag step the paused `bump release` points to. After
/// the PR merges, finish checks out the default branch, fast-forwards to the merged tip,
/// then -- reusing `crate::tag_ladder` (the SAME `--tag-only` verification ladder, never a
/// duplicate) -- either tags the merged commit and pushes it BY NAME, resumes a local-only
/// tag, no-ops an already-released tag, or refuses (missed bump / gated generic / dirty).
///
/// The DIFFERENCE from `bump --tag-only`: `--tag-only` only PRINTS the push command; finish
/// EXECUTES the tag push via the `Pusher` port (by explicit name) and then runs install,
/// and it does the checkout + `pull --ff-only` up front. NO tag is ever created on an
/// unconfirmed commit -- the shared ladder requires HEAD == origin/<default>.
pub fn finish<P: Pusher, I: Installer>(
    dir: &Path,
    opts: &FinishOpts,
    pusher: &P,
    installer: &I,
) -> Result<ReleaseReport> {
    debug!(
        "finish: dir={} dry_run={} install={:?}",
        dir.display(),
        opts.dry_run,
        opts.install
    );
    let config = config::load(dir)?;

    if !git::is_git_repo(dir) {
        bail!("not a git repository: {}", dir.display());
    }

    // Dirty tree: checking out the default branch would clobber tracked changes or carry
    // strays onto the release. Refuse before ANY mutation, with the one exact fix.
    if git::has_uncommitted_changes(dir)? {
        bail!(
            "the working tree is dirty; bump finish checks out the default branch, which would \
             clobber or carry strays.\n\
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
        // Local-only tag at the merged commit: a prior run died before/during the tag push.
        // RESUME -- push by name + install. A local-only tag is NOT released; NEVER report
        // it as already released.
        TagState::LocalAtHead => {
            pusher.push_tag(dir, &tag)?;
            println!("resumed release: pushed {tag} on {default}");
            let install_command = run_install(dir, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                tag,
                resumed: true,
                paused: false,
                install_command,
                dry_run: false,
            })
        }
        // origin/<default> carries an untagged version (the merged bump): tag the merged
        // commit, push by name, install.
        TagState::Absent => {
            let message = format!("Release {tag}");
            git::create_tag(dir, &tag, &message)?;
            pusher.push_tag(dir, &tag)?;
            println!("released {tag} on {default}");
            let install_command = run_install(dir, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                tag,
                resumed: false,
                paused: false,
                install_command,
                dry_run: false,
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
                Ok(ReleaseReport {
                    tag,
                    resumed: false,
                    paused: false,
                    install_command: None,
                    dry_run: false,
                })
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
    println!("[dry-run] git tag -a {tag} -m \"Release {tag}\"  (only if the merged version is untagged)");
    println!("[dry-run] git push origin {tag}");
    echo_install(&install_command);
    Ok(ReleaseReport {
        tag,
        resumed: false,
        paused: false,
        install_command,
        dry_run: true,
    })
}

/// Resolve the install command (precedence: explicit override > config > default-if-Cargo
/// > skip) WITHOUT running it. `None` = the install step is skipped.
pub(crate) fn resolve_install(dir: &Path, choice: &InstallChoice, config: &Config) -> Option<String> {
    debug!(
        "resolve_install: dir={} choice={:?} config.install={:?}",
        dir.display(),
        choice,
        config.install
    );
    match choice {
        InstallChoice::Command(cmd) => Some(cmd.clone()),
        InstallChoice::Skip => None,
        InstallChoice::Auto => config.install.clone().or_else(|| {
            if lang::cargo::cargo_toml_exists(dir) {
                Some(DEFAULT_INSTALL_COMMAND.to_string())
            } else {
                None
            }
        }),
    }
}

/// Resolve and run the install step; return the command that ran (`None` = skipped).
fn run_install<I: Installer>(
    dir: &Path,
    choice: &InstallChoice,
    config: &Config,
    installer: &I,
) -> Result<Option<String>> {
    match resolve_install(dir, choice, config) {
        Some(cmd) => {
            println!("install: {cmd}");
            installer.install(dir, &cmd)?;
            Ok(Some(cmd))
        }
        None => {
            println!("install: skipped");
            Ok(None)
        }
    }
}

/// Echo the install step for `-n` dry-run.
fn echo_install(install_command: &Option<String>) {
    match install_command {
        Some(cmd) => println!("[dry-run] install: {cmd}"),
        None => println!("[dry-run] install: skipped"),
    }
}

#[cfg(test)]
mod tests;
