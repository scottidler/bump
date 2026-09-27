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
//! installer, pr, ci)` / `finish(dir, opts, pusher, installer, ci)` functions; tests
//! still drive them via injected `Pusher`/`Installer`/`Pr`/`Ci` doubles.
//!
//! GATED invariant (Phase 6): NO tag is ever created or pushed in the gated `release`
//! flow -- the version commit rides the feature branch (internal `--no-tag`), the branch
//! is pushed with `--no-follow-tags` so a stray local tag can't ride, a PR is opened if
//! none is open, and the verb PAUSES (exit 0) for the human to merge. Tagging the merged
//! commit is `bump finish`'s job (Phase 7), never `release`'s.
//!
//! Tag invariant (2026-09-26 one-release-command doc, enforced in `gate_tag_and_push`):
//! a tag is created ONLY on a sha that (1) passed the CI gate (`wait_for_green`), (2) a
//! fresh fetch shows EQUALS `origin/<default>`, and (3) whose committed manifest carries
//! the tag's version; the push re-checks (2) after a second fresh fetch. A tip that moved
//! during the wait with the same version restarts the gate on the new tip.
//!
//! Pending version (`pending_version`): the manifest version at HEAD is the release when
//! it has no remote tag, is not below the latest tag, and is not the Rust untouched
//! default while tags exist. It is classified before the ahead/equal split (ungated) and
//! before the fresh/inherited split (gated); `compute_target_tag` never runs while one
//! exists.

use crate::cli::Cli;
use crate::config::{self, CiDeclaration, Config};
use crate::git::{self, HeadRemote};
use crate::github::{self, CheckRuns, Gate, StatusState};
use crate::lang::{self, Manifest, ManifestVersion, ProjectType};
use crate::version::{self, BumpType};
use crate::{DEFAULT_UNTOUCHED_VERSION, TagState, determine_version_action, process_directory, tag_ladder};
use eyre::{Context, Result, bail};
use log::debug;
use semver::Version;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The default install command when none is configured and a Cargo manifest is present.
const DEFAULT_INSTALL_COMMAND: &str = "cargo install --path .";

/// The pause message printed at the end of the gated `release` flow: the verb has done
/// everything mechanical up to (and including) opening the PR, and now hands control back
/// to the human/agent to merge and then run `bump finish`.
const GATED_PAUSE_MESSAGE: &str = "merge the PR, then run: bump finish";

/// How often the CI gate re-reads check runs and legacy statuses.
pub const CI_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long the CI gate waits for ANY check run or status to register on a sha before
/// deciding from the sha's `.github/workflows` tree.
pub const CI_APPEAR_WINDOW: Duration = Duration::from_secs(120);

/// The default `--ci-timeout`: how long the gate waits on incomplete runs before refusing.
pub const DEFAULT_CI_TIMEOUT: Duration = Duration::from_secs(1800);

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
    /// The explicit `-m`/`-M` level. `None` (no level flag) takes a pending version when
    /// one exists and means patch on a fresh release.
    pub bump_type: Option<BumpType>,
    /// `-n`: echo every command that would run and execute NOTHING.
    pub dry_run: bool,
    /// How to resolve the post-release install step.
    pub install: InstallChoice,
    /// `false` skips the CI gate with a printed warning (`--no-ci-gate`).
    pub ci_gate: bool,
    /// How long the CI gate waits on incomplete runs before refusing (`--ci-timeout`).
    pub ci_timeout: Duration,
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
    /// `false` skips the CI gate with a printed warning (`--no-ci-gate`).
    pub ci_gate: bool,
    /// How long the CI gate waits on incomplete runs before refusing (`--ci-timeout`).
    pub ci_timeout: Duration,
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
    /// The URL of the PR this run CREATED (`None` when one was already open, or on any
    /// ungated / finish run).
    pub pr_url: Option<String>,
    /// A warning printed alongside the pause (the inherited-pending-version notice).
    pub notice: Option<String>,
}

impl ReleaseReport {
    fn new(tag: &str) -> Self {
        Self {
            tag: tag.to_string(),
            resumed: false,
            paused: false,
            install_command: None,
            dry_run: false,
            pr_url: None,
            notice: None,
        }
    }
}

/// The typed state the repo is in, as classified from git + gate facts. Each refusal
/// row carries the data its exact-next-command message needs; execution turns each into
/// either the correct mutation sequence or a loud, actionable refusal.
#[derive(Debug)]
enum ReleaseState {
    /// Ungated, on default, ahead of origin, clean, no pending version: fresh release.
    Release { target_tag: String, default: String },
    /// Ungated, on default, clean, the manifest carries a PENDING version (see
    /// `pending_version`). `ahead` pushes the commits first; `!ahead` is the RESUME row
    /// (a prior run died, or CI was red and nothing new was committed). Never re-bumps.
    UngatedPending {
        tag: String,
        version: Version,
        default: String,
        ahead: bool,
    },
    /// Ungated pending version, but the requested level implies a DIFFERENT version.
    UngatedLevelMismatch { pending: String, implied: String },
    /// Any gate: the manifest version is below the latest tag.
    BelowLatest { manifest: String, latest: String },
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
    /// Gated, on a feature branch, no pending version, no version line in the diff:
    /// fresh gated release. Version commit by level, push branch, ensure PR, PAUSE.
    GatedFresh {
        branch: String,
        default: String,
        target_tag: String,
        bump_type: BumpType,
    },
    /// Gated, on a feature branch whose diff vs origin/<default> changes a version line
    /// (the branch's own bump, e.g. an idempotent re-run). Skip the re-bump, ensure the
    /// branch is pushed + a PR is open, PAUSE.
    GatedAlreadyBumped {
        branch: String,
        default: String,
        tag: String,
    },
    /// Gated, on a feature branch carrying work, the manifest carries a pending version
    /// with NO version line in the branch diff: inherited from origin/<default>. Bumps
    /// again from the manifest version (Scott, 2026-09-26: "bump again").
    GatedInheritedPending {
        branch: String,
        default: String,
        inherited: String,
        target: Version,
    },
    /// Gated re-run whose requested level (`-m`/`-M`) implies a DIFFERENT version than the
    /// one already riding the branch. REFUSE naming BOTH (never silently keep either).
    GatedLevelMismatch { riding: String, implied: String },
    /// Gated feature branch whose name is not its own slug: the PR title is built from the
    /// branch, so it could not slugify back to it (`branch-pr-title-guard.sh`). REFUSE
    /// before any mutation.
    GatedBadBranchName { branch: String, slug: String },
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

/// The pending-version classification (design doc, Data Model): the one definition every
/// release row uses.
#[derive(Debug)]
enum PendingCheck {
    /// The manifest version is the release: untagged on the remote, not below the latest
    /// tag, not the Rust untouched default while tags exist.
    Pending(Version),
    /// The manifest version is BELOW the latest tag: no release row matches, refuse.
    BelowLatest { manifest: Version, latest: Version },
    /// No pending version: the manifest equals a released tag, is the untouched default,
    /// or there is no manifest version at all.
    NotPending,
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
/// open --json number`, NOT `gh pr view`); `create_pr` is `gh pr create` with an explicit
/// head, base, title and body, only ever called when `open_pr_exists` returns false, and
/// returns the new PR's URL.
pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String>;
}

/// The CI seam for the gate. `Ok(None)` = the repo has no GitHub remote (nothing to wait
/// on); `Err` = an API/auth failure or a truncated read, which the gate fails closed on.
///
/// The poll interval and appear window live on the port because they describe the remote
/// CI system (how often it is sane to poll GitHub, how long GitHub takes to register a
/// run); test doubles return zero for both so no test ever sleeps.
pub trait Ci {
    fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>>;
    fn poll_interval(&self) -> Duration {
        CI_POLL_INTERVAL
    }
    fn appear_window(&self) -> Duration {
        CI_APPEAR_WINDOW
    }
}

/// Production `Pr`: the real `gh` PR operations (list-probe + explicit create).
pub struct GhPr;

impl Pr for GhPr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool> {
        github::open_pr_exists(dir, branch)
    }

    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        github::create_pr(dir, branch, base, title, body)
    }
}

/// Production `Ci`: `gh api` check-runs + legacy status for the sha.
pub struct GhCi;

impl Ci for GhCi {
    fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>> {
        github::check_runs(dir, sha)
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
struct Ports<'a, P: Pusher, I: Installer, R: Pr, C: Ci> {
    pusher: &'a P,
    installer: &'a I,
    pr: &'a R,
    ci: &'a C,
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

/// The CI gate's settings for one run, taken from the verb's opts.
#[derive(Debug, Clone, Copy)]
struct CiGate {
    enabled: bool,
    timeout: Duration,
}

/// What `gate_tag_and_push` tags and where: the tag, the version the manifest at the
/// tagged sha must carry, the default branch it must equal, and the verb a refusal tells
/// the operator to re-run.
struct TagTarget<'a> {
    tag: &'a str,
    version: &'a Version,
    default: &'a str,
    rerun: &'a str,
}

/// `bump release`: classify the repo's state, then execute the one correct sequence or
/// refuse with the exact next command (non-zero exit at the caller).
pub fn release<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    pusher: &P,
    installer: &I,
    pr: &R,
    ci: &C,
) -> Result<ReleaseReport> {
    debug!(
        "release: dir={} dry_run={} bump_type={:?} install={:?} ci_gate={} ci_timeout={:?}",
        dir.display(),
        opts.dry_run,
        opts.bump_type,
        opts.install,
        opts.ci_gate,
        opts.ci_timeout
    );
    let config = config::load(dir)?;
    let state = classify(dir, opts)?;
    debug!("release: classified state={:?}", state);
    let ports = Ports {
        pusher,
        installer,
        pr,
        ci,
    };
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

    let ahead = match git::compare_head_to_remote(dir, &default)? {
        HeadRemote::Behind | HeadRemote::Diverged => return Ok(ReleaseState::Behind { default }),
        HeadRemote::Ahead => true,
        HeadRemote::Equal => false,
    };

    // The pending version is classified BEFORE the ahead/equal split: a pending version
    // IS the release, whether or not commits sit on top of it.
    match pending_version(dir)? {
        PendingCheck::BelowLatest { manifest, latest } => Ok(ReleaseState::BelowLatest {
            manifest: version::format_file_version(&manifest),
            latest: version::format_tag(&latest),
        }),
        PendingCheck::Pending(version) => classify_ungated_pending(dir, opts, version, default, ahead),
        PendingCheck::NotPending if ahead => {
            let target_tag = compute_target_tag(dir, opts.bump_type.unwrap_or_default())?;
            Ok(ReleaseState::Release { target_tag, default })
        }
        PendingCheck::NotPending => Ok(ReleaseState::Nothing { default }),
    }
}

/// Ungated, on default, the manifest carries a pending version: refuse a level flag that
/// implies a different version, refuse a local tag for it at another commit (manual
/// surgery), otherwise ship the pending version.
fn classify_ungated_pending(
    dir: &Path,
    opts: &ReleaseOpts,
    version: Version,
    default: String,
    ahead: bool,
) -> Result<ReleaseState> {
    debug!(
        "classify_ungated_pending: dir={} version={} ahead={}",
        dir.display(),
        version,
        ahead
    );
    let tag = version::format_tag(&version);
    if let Some(level) = opts.bump_type {
        let implied = implied_version(latest_tag_version(dir)?.as_ref(), &version, level);
        if implied != version {
            return Ok(ReleaseState::UngatedLevelMismatch {
                pending: tag,
                implied: version::format_tag(&implied),
            });
        }
    }

    // A local tag at HEAD is the local-tag resume row (`gate_tag_and_push` pushes it
    // without re-creating); a local tag at a DIFFERENT commit is manual surgery.
    if git::tag_exists(dir, &tag)? {
        let head = git::head_sha(dir)?;
        let sha = git::tag_sha(dir, &tag)?;
        if sha != head {
            bail!(
                "tag {tag} exists locally at {sha}, not HEAD ({head}); \
                 resolving that is manual tag surgery, not bump's job."
            );
        }
    }

    Ok(ReleaseState::UngatedPending {
        tag,
        version,
        default,
        ahead,
    })
}

/// Classify a GATED repo. Resolves the remote default and fetches it (like the ungated
/// path), then splits on branch: on the default branch it is either a stranded-commits
/// refusal, a behind refusal, or the "bump rides a PR" refusal; on a feature branch see
/// `classify_gated_feature`.
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

    classify_gated_feature(dir, opts, current.to_string(), default)
}

/// Classify a gated repo when HEAD is on a FEATURE branch. Order: generic, branch-name
/// precondition, below-latest, then the branch's own bump (a version line in the diff vs
/// origin/<default>, Gate D's test) vs an inherited pending version vs a fresh release.
fn classify_gated_feature(dir: &Path, opts: &ReleaseOpts, branch: String, default: String) -> Result<ReleaseState> {
    debug!("classify_gated_feature: dir={} branch={}", dir.display(), branch);
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        // Gated + generic: `bump finish` cannot derive a version without a manifest, so
        // both verbs refuse (Resolved Decisions). Fail closed rather than bump nothing.
        return Ok(ReleaseState::GatedGeneric);
    }

    let slug = branch_slug(&branch);
    if slug != branch {
        return Ok(ReleaseState::GatedBadBranchName { branch, slug });
    }

    let pending = pending_version(dir)?;
    if let PendingCheck::BelowLatest { manifest, latest } = &pending {
        return Ok(ReleaseState::BelowLatest {
            manifest: version::format_file_version(manifest),
            latest: version::format_tag(latest),
        });
    }

    let base_ref = format!("origin/{default}");
    if git::version_line_changed(dir, &base_ref)? {
        let riding = agreed_file_version(&manifests)?.ok_or_else(|| {
            eyre::eyre!("this branch changes a version line but the manifest carries no version; fix the manifest")
        })?;
        if let Some(level) = opts.bump_type {
            let implied = implied_version(branch_base_version(dir, &base_ref)?.as_ref(), &riding, level);
            if implied != riding {
                return Ok(ReleaseState::GatedLevelMismatch {
                    riding: version::format_tag(&riding),
                    implied: version::format_tag(&implied),
                });
            }
        }
        return Ok(ReleaseState::GatedAlreadyBumped {
            branch,
            default,
            tag: version::format_tag(&riding),
        });
    }

    let bump_type = opts.bump_type.unwrap_or_default();
    if let PendingCheck::Pending(inherited) = pending {
        let target = version::bump_version(&inherited, bump_type);
        return Ok(ReleaseState::GatedInheritedPending {
            branch,
            default,
            inherited: version::format_tag(&inherited),
            target,
        });
    }

    // Fresh: version == last tag (or an initial release). Compute the target the requested
    // level yields via bump's own version rules (no re-derivation, no stdout parsing).
    let target_tag = compute_target_tag(dir, bump_type)?;
    Ok(ReleaseState::GatedFresh {
        branch,
        default,
        target_tag,
        bump_type,
    })
}

/// The pending-version definition (design doc, Data Model): the manifest version at HEAD
/// (a) has no tag on the REMOTE, (b) is not below the latest local `v*` tag, and (c) is
/// not the Rust untouched default `0.1.0` while tags exist. A manifest below the latest
/// tag is its own verdict, refused by name.
fn pending_version(dir: &Path) -> Result<PendingCheck> {
    debug!("pending_version: dir={}", dir.display());
    let manifests = lang::detect(dir)?;
    let Some(manifest) = agreed_file_version(&manifests)? else {
        return Ok(PendingCheck::NotPending);
    };
    let latest = latest_tag_version(dir)?;
    let untouched_default =
        lang::detect_project_type(dir) == ProjectType::Rust && manifest == DEFAULT_UNTOUCHED_VERSION;
    if untouched_default && latest.is_some() {
        return Ok(PendingCheck::NotPending);
    }
    if let Some(latest) = latest
        && manifest < latest
    {
        return Ok(PendingCheck::BelowLatest { manifest, latest });
    }
    if git::remote_tag_sha(dir, &version::format_tag(&manifest))?.is_some() {
        return Ok(PendingCheck::NotPending);
    }
    debug!("pending_version: {manifest} is pending");
    Ok(PendingCheck::Pending(manifest))
}

/// The latest local `v*` tag as a `Version` (`None` when there are no tags).
fn latest_tag_version(dir: &Path) -> Result<Option<Version>> {
    Ok(git::get_latest_tag(dir)?.and_then(|t| version::parse_version(&t).ok()))
}

/// The version an explicit level implies, bumped from `base` (the latest tag, or the
/// version the branch bumped from). With no base (no tags), the level cannot move an
/// initial release, so it implies the version already there.
fn implied_version(base: Option<&Version>, current: &Version, level: BumpType) -> Version {
    match base {
        Some(base) => version::bump_version(base, level),
        None => current.clone(),
    }
}

/// The version a gated branch bumped FROM: the higher of the manifest at `base_ref`
/// (skipped when it is the Rust untouched default while tags exist) and the latest tag.
fn branch_base_version(dir: &Path, base_ref: &str) -> Result<Option<Version>> {
    let latest = latest_tag_version(dir)?;
    let at_base = git::manifest_version_at(dir, base_ref)?.and_then(|v| version::parse_version(&v).ok());
    let untouched_default = lang::detect_project_type(dir) == ProjectType::Rust
        && at_base.as_ref() == Some(&DEFAULT_UNTOUCHED_VERSION)
        && latest.is_some();
    let at_base = if untouched_default { None } else { at_base };
    Ok(at_base.into_iter().chain(latest).max())
}

/// `title_slug` from `branch-pr-title-guard.sh`: lowercase, collapse every run of
/// non-`[a-z0-9]` to `-`, trim dashes. A branch is a legal release branch only when it is
/// its own slug.
fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut in_run = false;
    for c in branch.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            in_run = false;
        } else if !in_run {
            slug.push('-');
            in_run = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// The PR title: `<type>(<scope>): <branch words>`, type/scope from the first commit
/// subject on the branch (`chore` when it has no conventional prefix). By construction it
/// slugifies back to the branch, which `branch-pr-title-guard.sh` enforces.
pub fn pr_title(branch: &str, subjects: &[String]) -> String {
    let words = branch.replace('-', " ");
    let (kind, scope) = subjects
        .first()
        .and_then(|s| parse_conventional_prefix(s))
        .unwrap_or_else(|| ("chore".to_string(), None));
    match scope {
        Some(scope) => format!("{kind}({scope}): {words}"),
        None => format!("{kind}: {words}"),
    }
}

/// `feat(scope)!: subject` -> `("feat", Some("scope"))`. `None` when the subject has no
/// conventional prefix.
fn parse_conventional_prefix(subject: &str) -> Option<(String, Option<String>)> {
    let (head, _) = subject.split_once(':')?;
    let head = head.trim_end_matches('!');
    let (kind, scope) = match head.split_once('(') {
        Some((k, rest)) => (k, Some(rest.strip_suffix(')')?)),
        None => (head, None),
    };
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    if let Some(s) = scope
        && (s.is_empty()
            || !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
    {
        return None;
    }
    Some((kind.to_string(), scope.map(str::to_string)))
}

/// The PR body: one `- <subject>` per commit on the branch, then the release-intent line
/// Gate D looks for as the LAST line.
pub fn pr_body(subjects: &[String], tag: &str) -> String {
    let mut body = String::new();
    for s in subjects {
        body.push_str(&format!("- {s}\n"));
    }
    if subjects.is_empty() {
        body.push_str("- version bump\n");
    }
    body.push_str(&format!("\nRelease: rides this PR ({tag})"));
    body
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
/// (no stdout parsing, no re-derivation of the version rules). Only called when no pending
/// version exists.
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
fn execute<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    state: ReleaseState,
    ports: &Ports<P, I, R, C>,
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
        ReleaseState::UngatedPending {
            tag,
            version,
            default,
            ahead,
        } => execute_pending(dir, opts, config, &tag, &version, &default, ahead, ports),
        ReleaseState::UngatedLevelMismatch { pending, implied } => bail!(
            "{pending} is committed and untagged, so it IS the pending release, but the requested level implies {implied}.\n\
             bump never bumps past a pending version: drop the -m/-M flag and re-run bump release to ship {pending}."
        ),
        ReleaseState::BelowLatest { manifest, latest } => bail!(
            "manifest {manifest} is below the latest tag {latest}; bump never lowers a version, fix the manifest by hand"
        ),
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
        ReleaseState::GatedFresh {
            branch,
            default,
            target_tag,
            bump_type,
        } => execute_gated(
            dir,
            opts,
            GatedPlan {
                branch,
                default,
                commit: VersionCommit::Level(bump_type),
                tag: target_tag,
                notice: None,
            },
            ports,
        ),
        ReleaseState::GatedAlreadyBumped { branch, default, tag } => execute_gated(
            dir,
            opts,
            GatedPlan {
                branch,
                default,
                commit: VersionCommit::AlreadyRiding,
                tag,
                notice: None,
            },
            ports,
        ),
        ReleaseState::GatedInheritedPending {
            branch,
            default,
            inherited,
            target,
        } => {
            let tag = version::format_tag(&target);
            let notice = format!(
                "origin/{default} carries untagged {inherited}; this PR releases as {tag}. \
                 To ship {inherited} on its own first, run bump finish before merging this PR."
            );
            execute_gated(
                dir,
                opts,
                GatedPlan {
                    branch,
                    default,
                    commit: VersionCommit::To(target),
                    tag,
                    notice: Some(notice),
                },
                ports,
            )
        }
        ReleaseState::GatedLevelMismatch { riding, implied } => bail!(
            "this branch already carries a version bump to {riding}, but the requested level implies {implied}.\n\
             bump refuses to name two versions: either drop the -m/-M flag to keep {riding}, or reset the branch's \
             bump commit and re-run for {implied}."
        ),
        ReleaseState::GatedBadBranchName { branch, slug } => bail!(
            "branch '{branch}' is not its own slug ('{slug}'): the PR title is built from the branch name and must \
             slugify back to it.\n\
             Run: git branch -m {slug}, then bump release"
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

/// Fresh ungated release: version commit -> push branch -> confirm on origin -> CI gate
/// -> re-verify -> tag the verified sha -> re-verify -> push tag by name -> install.
fn execute_release<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    target_tag: &str,
    default: &str,
    ports: &Ports<P, I, R, C>,
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
        println!("[dry-run] git push --no-follow-tags origin {default}");
        println!("[dry-run] (confirm HEAD is on origin/{default} before tagging)");
        echo_tag_steps(target_tag, default, &ci_gate(opts.ci_gate, opts.ci_timeout), false);
        echo_install(&install_command);
        return Ok(ReleaseReport {
            install_command,
            dry_run: true,
            ..ReleaseReport::new(target_tag)
        });
    }

    // 1. The version commit is the existing `--no-tag` code path (version bump + commit,
    //    no tag). Reused verbatim -- release never re-implements commit/version logic.
    version_commit(dir, opts.bump_type.unwrap_or_default())?;
    // 2. Push the branch FIRST.
    ports.pusher.push_branch(dir, default)?;
    // 3. Confirm it landed on origin BEFORE any tag exists (a rejected push errored above
    //    and we never reach here; a push that reported success but didn't land is caught).
    confirm_on_origin(dir, default)?;
    // 4. CI gate, re-verify, tag the verified sha, re-verify, push the tag by name.
    let version = version::parse_version(target_tag)?;
    let target = TagTarget {
        tag: target_tag,
        version: &version,
        default,
        rerun: "bump release",
    };
    let gate = ci_gate(opts.ci_gate, opts.ci_timeout);
    gate_tag_and_push(dir, gate, &target, git::head_sha(dir)?, ports.pusher, ports.ci)?;
    println!("Released {target_tag} on {default}");
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        install_command,
        ..ReleaseReport::new(target_tag)
    })
}

/// A pending version on the ungated default branch: never re-bump. `ahead` pushes the
/// commits on top of it first (a fix after red CI, or an unpushed version commit); `!ahead`
/// is RESUME. Both then run the CI gate and tag the verified sha (a local tag already at
/// that sha is pushed, not re-created).
#[allow(clippy::too_many_arguments)]
fn execute_pending<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    tag: &str,
    version: &Version,
    default: &str,
    ahead: bool,
    ports: &Ports<P, I, R, C>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_pending: dir={} tag={} default={} ahead={} dry_run={}",
        dir.display(),
        tag,
        default,
        ahead,
        opts.dry_run
    );
    let local_tag_present = git::tag_exists(dir, tag)?;

    if opts.dry_run {
        let install_command = resolve_install(dir, &opts.install, config);
        println!("[dry-run] (pending version {tag}: no version commit, no re-bump)");
        if ahead {
            println!("[dry-run] git push --no-follow-tags origin {default}");
        }
        println!("[dry-run] (confirm HEAD is on origin/{default} before tagging)");
        echo_tag_steps(tag, default, &ci_gate(opts.ci_gate, opts.ci_timeout), local_tag_present);
        echo_install(&install_command);
        return Ok(ReleaseReport {
            resumed: !ahead,
            install_command,
            dry_run: true,
            ..ReleaseReport::new(tag)
        });
    }

    if ahead {
        ports.pusher.push_branch(dir, default)?;
    }
    confirm_on_origin(dir, default)?;
    let target = TagTarget {
        tag,
        version,
        default,
        rerun: "bump release",
    };
    let gate = ci_gate(opts.ci_gate, opts.ci_timeout);
    gate_tag_and_push(dir, gate, &target, git::head_sha(dir)?, ports.pusher, ports.ci)?;
    if ahead {
        println!("Released pending {tag} on {default}");
    } else {
        println!("Resumed release: pushed {tag} on {default}");
    }
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        resumed: !ahead,
        install_command,
        ..ReleaseReport::new(tag)
    })
}

/// How the gated flow's version commit is made.
#[derive(Debug)]
enum VersionCommit {
    /// Bump by level through the existing `--no-tag` path (fresh release).
    Level(BumpType),
    /// Set this exact version (the inherited-pending row bumps from the manifest version,
    /// which `determine_version_action` would refuse as a tag mismatch).
    To(Version),
    /// The branch already carries its own bump: no version commit.
    AlreadyRiding,
}

/// Everything `execute_gated` needs for one run.
struct GatedPlan {
    branch: String,
    default: String,
    commit: VersionCommit,
    tag: String,
    notice: Option<String>,
}

/// The GATED release flow: on a feature branch, make the version commit (unless the
/// branch already carries its own bump), push the branch with `--no-follow-tags -u`,
/// ensure an OPEN PR exists (list-probe, then create with the derived title and body), and
/// PAUSE (exit 0) for the human to merge. NO tag is created or pushed here -- tagging the
/// merged commit is `bump finish`'s job.
fn execute_gated<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    plan: GatedPlan,
    ports: &Ports<P, I, R, C>,
) -> Result<ReleaseReport> {
    let GatedPlan {
        branch,
        default,
        commit,
        tag,
        notice,
    } = plan;
    debug!(
        "execute_gated: dir={} branch={} default={} commit={:?} tag={} dry_run={}",
        dir.display(),
        branch,
        default,
        commit,
        tag,
        opts.dry_run
    );
    let base_ref = format!("origin/{default}");

    if opts.dry_run {
        match &commit {
            VersionCommit::Level(_) | VersionCommit::To(_) => {
                println!("[dry-run] commit the version bump for {tag} on {branch} (a new commit, never an amend)");
            }
            VersionCommit::AlreadyRiding => {
                println!("[dry-run] (version already bumped on {branch}; no re-bump)");
            }
        }
        let title = pr_title(&branch, &git::commit_subjects(dir, &base_ref)?);
        println!("[dry-run] git push --no-follow-tags -u origin {branch}");
        println!("[dry-run] gh pr list --head {branch} --state open --json number  (open-PR probe)");
        println!(
            "[dry-run] gh pr create --head {branch} --base {default} --title \"{title}\" --body \"<subjects>\\n\\nRelease: rides this PR ({tag})\"  (only if no open PR)"
        );
        if let Some(notice) = &notice {
            println!("[dry-run] {notice}");
        }
        println!("[dry-run] {GATED_PAUSE_MESSAGE}");
        return Ok(ReleaseReport {
            paused: true,
            dry_run: true,
            notice,
            ..ReleaseReport::new(&tag)
        });
    }

    // 1. The version commit: always a NEW commit, never an amend (`never_amend`).
    match &commit {
        VersionCommit::Level(level) => version_commit(dir, *level)?,
        VersionCommit::To(target) => version_commit_to(dir, target)?,
        VersionCommit::AlreadyRiding => debug!("execute_gated: version already bumped on {branch}; skipping re-bump"),
    }

    // 2. Push the feature branch with `--no-follow-tags -u` (a stray local tag must not
    //    ride; tagging is `bump finish`'s job on the merged commit).
    ports.pusher.push_feature_branch(dir, &branch)?;

    // 3. Ensure an OPEN PR exists: the list-probe FIRST (exit-0 in all cases, reused
    //    branch names read correctly), create ONLY if none is open, with the title and
    //    body built from the branch and its commits.
    let pr_url = if ports.pr.open_pr_exists(dir, &branch)? {
        println!("open PR already exists for {branch}; not creating another");
        None
    } else {
        let subjects = git::commit_subjects(dir, &base_ref)?;
        let title = pr_title(&branch, &subjects);
        let body = pr_body(&subjects, &tag);
        let url = ports.pr.create_pr(dir, &branch, &default, &title, &body)?;
        println!("opened PR: {url}");
        Some(url)
    };

    // 4. PAUSE. No tag, no install -- both are `bump finish`'s after the merge.
    if let Some(notice) = &notice {
        println!("{notice}");
    }
    println!("{GATED_PAUSE_MESSAGE}");
    Ok(ReleaseReport {
        paused: true,
        pr_url,
        notice,
        ..ReleaseReport::new(&tag)
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
        // The release verb's own bump commit must never silently rewrite a commit that
        // might already be public (a re-run after a partial push, a branch someone else
        // fetched): always a new commit, never `process_directory`'s amend fallback.
        never_amend: true,
    };
    process_directory(dir, &cli, bump_type)
}

/// The version commit to an EXACT version, for the inherited-pending row: the manifest
/// is already above the latest tag, which `process_directory`'s `determine_version_action`
/// refuses as a mismatch. Same manifest validation and lockstep write as `process_directory`,
/// then a new commit (the tree is clean at classification, so only version files stage).
fn version_commit_to(dir: &Path, target: &Version) -> Result<()> {
    debug!("version_commit_to: dir={} target={}", dir.display(), target);
    let config = config::load(dir)?;
    let skip_members = config::effective_skip_members(&[], &config);
    let manifests = lang::detect(dir)?;
    for m in &manifests {
        m.validate(&skip_members)?;
    }
    let tag = version::format_tag(target);
    lang::write_all(&manifests, target)?;
    git::stage_all(dir)?;
    git::commit(dir, &format!("Bump version to {tag}"))?;
    println!(
        "Committed version bump to {} (no tag)",
        version::format_file_version(target)
    );
    Ok(())
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

fn ci_gate(enabled: bool, timeout: Duration) -> CiGate {
    CiGate { enabled, timeout }
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
/// 3. the manifest at the sha must carry the tag's version;
/// 4. create the annotated tag ON THAT SHA (a local tag already there is kept);
/// 5. fetch fresh again: the sha must still equal the tip, else refuse and leave the local
///    tag for the local-tag resume row;
/// 6. push the tag by name.
///
/// Returns the sha that was tagged.
fn gate_tag_and_push<P: Pusher, C: Ci>(
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

    let mut sha = start_sha;
    loop {
        wait_for_green(dir, &sha, gate, ci, target)?;
        let tip = git::remote_tip(dir, default)?;
        if tip == sha {
            break;
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

    let at_sha = version_at(dir, &sha)?;
    if at_sha.as_ref() != Some(version) {
        bail!(
            "the manifest at {sha} carries {}, not {tag}; NO tag was created.",
            describe_version(&at_sha)
        );
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

/// The CI gate (design doc, API Design "CI gate"): poll check runs + legacy statuses for
/// `sha` until every run completed green (proceed) or anything is red, truncated, errored,
/// or timed out (refuse, no tag). Zero runs and zero statuses after the appear window are
/// decided by the sha's `.github/workflows` tree. A committed `ci: none` in `bump.yml` at
/// the sha, a repo with no GitHub remote, or `--no-ci-gate` skip the gate with a notice.
fn wait_for_green<C: Ci>(dir: &Path, sha: &str, gate: CiGate, ci: &C, target: &TagTarget) -> Result<()> {
    debug!(
        "wait_for_green: dir={} sha={} enabled={} timeout={:?}",
        dir.display(),
        sha,
        gate.enabled,
        gate.timeout
    );
    let TagTarget { tag, rerun, .. } = *target;
    if !gate.enabled {
        println!("CI gate: SKIPPED (--no-ci-gate); {tag} is tagged without waiting for CI on {sha}");
        return Ok(());
    }
    if config::load_at(dir, sha)?.ci == Some(CiDeclaration::None) {
        println!("CI gate: skipped, bump.yml at {sha} declares `ci: none` (this repo's workflows never run on push)");
        return Ok(());
    }

    let start = Instant::now();
    let mut announced = false;
    loop {
        let runs = ci
            .check_runs(dir, sha)
            .wrap_err_with(|| format!("CI gate: could not read CI for {sha}; NO tag was created (fails closed)"))?;
        let Some(runs) = runs else {
            println!("CI gate: no GitHub remote, nothing to wait on");
            return Ok(());
        };
        debug!("wait_for_green: sha={sha} runs={runs:?}");

        if !runs.failed.is_empty() || runs.statuses == StatusState::Failure {
            let mut lines = String::new();
            for (name, url) in &runs.failed {
                lines.push_str(&format!("  FAILED  {name}  {url}\n"));
            }
            if runs.statuses == StatusState::Failure {
                lines.push_str("  FAILED  combined commit status (failure/error)\n");
            }
            bail!(
                "CI is RED on {sha}:\n{lines}NO tag was created. Fix it, commit, and re-run {rerun}: \
                 the re-run reuses {tag}, it never bumps past it."
            );
        }

        let no_ci_reported = runs.total == 0 && runs.statuses == StatusState::None;
        if no_ci_reported {
            if start.elapsed() >= ci.appear_window() {
                let window = ci.appear_window().as_secs();
                if git::has_workflows_at(dir, sha)? {
                    bail!(
                        "CI never registered on {sha} after {window}s; re-run when it has. If this repo's workflows \
                         never run on push, declare `ci: none` in bump.yml (committed, reviewed) and re-run."
                    );
                }
                println!(
                    "CI gate: no check runs or statuses on {sha} after {window}s, and the repo has no workflows at that sha; proceeding"
                );
                return Ok(());
            }
        } else if runs.incomplete == 0 && runs.statuses != StatusState::Pending {
            println!("CI gate: green ({} check run(s)) on {sha}", runs.total);
            return Ok(());
        } else if start.elapsed() >= gate.timeout {
            bail!(
                "CI gate: timed out after {}s with {} check run(s) incomplete{} on {sha}. NO tag was created.\n\
                 Re-run {rerun} when they finish (it reuses {tag}).",
                gate.timeout.as_secs(),
                runs.incomplete,
                if runs.statuses == StatusState::Pending {
                    " and the commit status pending"
                } else {
                    ""
                }
            );
        }

        if !announced {
            println!("CI gate: waiting on CI for {sha} (no tag exists yet)");
            announced = true;
        }
        thread::sleep(ci.poll_interval());
    }
}

/// Echo the CI gate + tag steps for `-n` dry-run.
fn echo_tag_steps(tag: &str, default: &str, gate: &CiGate, local_tag_present: bool) {
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
    println!("[dry-run] git push origin {tag}");
}

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
