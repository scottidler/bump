//! `bump release` and `bump finish`: the two release verbs, one per half of a release.
//!
//! There are exactly two release flows and `github::detect` picks between them; nothing
//! is left to the caller except running the verb:
//!
//! - UNGATED (the default branch takes direct pushes): version commit -> push the default
//!   branch -> confirm it landed on origin -> WAIT FOR CI TO GO GREEN on that sha -> tag
//!   -> push the tag by name -> install. One verb, `bump release`, does all of it.
//! - GATED (the default branch requires a PR): on the feature branch, version commit ->
//!   push the branch -> open the PR (title derived from the branch, body carrying the
//!   `Release: rides this PR (vX.Y.Z)` line) -> PAUSE. After the merge, `bump finish`
//!   fetches the merged tip, waits for its CI, tags it, pushes the tag by name, installs.
//!
//! Two invariants are enforced in code, never prose. A tag is created ONLY after the
//! commit it points to is confirmed on `origin/<default>` AND its CI has finished green:
//! a tag is the one artifact a release cannot take back, so a red run found after the
//! tag can only be repaired by a second tag (the double-tap that burned otto three
//! version pairs), whereas a red run found before it costs a re-run and reuses the same
//! number. And a bump never rides alone: a branch whose whole diff against the default
//! branch is version lines needs `--standalone "<Scott's words>"`, which is Scott's
//! explicit order quoted into the PR body (THE RULING, ~/HALL-OF-SHAME.md).
//!
//! Everything here operates on bump's typed internals (`github::Gate`, `VersionAction`,
//! the `git::*` helpers, the `lang` adapter seam) with zero stdout scraping. External
//! effects go through the `Pusher`, `Installer`, `Pr` and `Ci` ports so tests drive the
//! whole machine against bare local remotes with no network.

use crate::cli::Cli;
use crate::config::{self, Config};
use crate::git::{self, HeadRemote};
use crate::github::{self, CheckRuns, Gate};
use crate::lang::{self, Manifest, ManifestVersion, ProjectType};
use crate::version::{self, BumpType};
use crate::{DEFAULT_UNTOUCHED_VERSION, TagState, determine_version_action, process_directory, tag_ladder};
use eyre::{Context, Result, bail};
use log::debug;
use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The default install command when none is configured and a Cargo manifest is present.
const DEFAULT_INSTALL_COMMAND: &str = "cargo install --path .";

/// The pause message printed at the end of the gated `release` flow: everything
/// mechanical up to and including the PR is done, and the merge is the human's.
const GATED_PAUSE_MESSAGE: &str = "merge the PR, then run: bump finish";

/// How long the CI gate waits for ANY check to register before concluding the repo has
/// no CI on that commit.
const CI_APPEAR: Duration = Duration::from_secs(120);

/// Poll interval for the CI gate.
const CI_POLL: Duration = Duration::from_secs(15);

/// Default `--ci-timeout`.
pub const DEFAULT_CI_TIMEOUT_SECS: u64 = 1800;

/// Files whose change alone makes a branch a bump-only branch (version manifests and
/// their lockfiles). Mirrors the git-release-guard hook's Gate B list.
const BUMP_ONLY_FILES: [&str; 11] = [
    "Cargo.toml",
    "Cargo.lock",
    "pyproject.toml",
    "uv.lock",
    "poetry.lock",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "VERSION",
    "bump.yml",
];

/// How the install step is resolved. Precedence (general.md): CLI override > config
/// `install` > default (`cargo install --path .` iff a `Cargo.toml` is present) > skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChoice {
    /// Explicit override (`--install "<cmd>"`): run this exact command.
    Command(String),
    /// Explicit opt-out (`--no-install`): skip the install step.
    Skip,
    /// Neither flag given: config `install` > default-if-Cargo > skip.
    Auto,
}

/// Inputs to a `bump release` invocation.
#[derive(Debug, Clone)]
pub struct ReleaseOpts {
    /// The bump level (patch/minor/major) for a fresh release.
    pub bump_type: BumpType,
    /// `-n`: echo every command that would run and execute NOTHING.
    pub dry_run: bool,
    /// How to resolve the post-release install step.
    pub install: InstallChoice,
    /// Scott's explicit order for a standalone (bump-only) release, quoted verbatim.
    /// `None` means a bump-only release refuses.
    pub standalone: Option<String>,
    /// Wait for green CI on the commit before tagging it (default true).
    pub ci_gate: bool,
    /// How long the CI gate waits before giving up (no tag is created on a timeout).
    pub ci_timeout: Duration,
}

/// Inputs to a `bump finish` invocation. No bump level: finish tags the version already
/// merged onto the default branch and never computes a bump.
#[derive(Debug, Clone)]
pub struct FinishOpts {
    /// `-n`: echo every command that would run and execute NOTHING.
    pub dry_run: bool,
    /// How to resolve the post-release install step.
    pub install: InstallChoice,
    /// Wait for green CI on the merged commit before tagging it (default true).
    pub ci_gate: bool,
    /// How long the CI gate waits before giving up (no tag is created on a timeout).
    pub ci_timeout: Duration,
}

/// The outcome of a successful `release()` or `finish()` (refusals are `Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReport {
    /// The tag involved. Ungated: the tag created and/or pushed. Gated: the version
    /// riding the PR, for reporting only, because no tag exists until `bump finish`.
    pub tag: String,
    /// True when this completed a partial-release RESUME rather than a fresh release.
    pub resumed: bool,
    /// True when this was a GATED run that pushed the branch, ensured a PR, and PAUSED.
    pub paused: bool,
    /// The resolved install command that ran (`None` = skipped; always `None` on pause).
    pub install_command: Option<String>,
    /// True when this was a `-n` dry run (nothing was mutated).
    pub dry_run: bool,
    /// The PR url when this run opened one.
    pub pr_url: Option<String>,
}

/// The typed state the repo is in, classified from git + gate facts.
#[derive(Debug)]
enum ReleaseState {
    /// Ungated, on default, ahead of origin, clean: fresh release.
    Release { target_tag: String, default: String },
    /// Ungated RESUME: origin carries the version, the remote tag is missing.
    Resume {
        tag: String,
        default: String,
        local_tag_present: bool,
    },
    /// Ungated, on default, HEAD == origin, everything tagged, and Scott ordered a
    /// standalone release: a version-only commit is the release.
    UngatedStandalone { target_tag: String, default: String },
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
    /// Gated, on a feature branch carrying work, version == last tag: fresh gated release.
    GatedFresh {
        branch: String,
        default: String,
        target_tag: String,
    },
    /// Gated, on a feature branch, version ALREADY bumped (idempotent re-run).
    GatedAlreadyBumped {
        branch: String,
        default: String,
        tag: String,
    },
    /// Gated re-run whose requested level implies a DIFFERENT version than the riding one.
    GatedLevelMismatch { riding: String, implied: String },
    /// Gated, on a feature branch whose diff against the default branch is nothing (a
    /// fresh cut) or version lines only, with no `--standalone` order: the bump-only
    /// branch THE RULING forbids.
    GatedBumpOnlyBranch { branch: String, default: String },
    /// Gated, on a feature branch whose name can never match a conventional PR title
    /// (it carries `/` or `.`): refuse before any mutation.
    GatedBadBranchName { branch: String },
    /// Gated, on the local default branch with commits NOT on origin (stranded).
    GatedStranded { default: String, suggested_branch: String },
    /// Gated, on the default branch, clean, HEAD == origin, no standalone order.
    GatedDefaultClean { default: String },
    /// Gated, on the default branch, clean, HEAD == origin, and Scott ordered a
    /// standalone release: cut `bump-vX-Y-Z` off origin, bump on it, PR it, pause.
    GatedStandalone { default: String, target_tag: String },
    /// Gated + generic (no manifest): unsupported.
    GatedGeneric,
    /// Gate probe inconclusive: `release` pushes, so it FAILS CLOSED.
    Unknown { reason: String },
}

/// Pushes a branch / tag to origin.
pub trait Pusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()>;
    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()>;
    /// Push a FEATURE branch with `--no-follow-tags -u` (the gated flow).
    fn push_feature_branch(&self, dir: &Path, branch: &str) -> Result<()>;
}

/// Runs the post-release install command.
pub trait Installer {
    fn install(&self, dir: &Path, command: &str) -> Result<()>;
}

/// The PR seam for the gated flow: the open-PR probe and the create.
pub trait Pr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool>;
    /// Create the PR and return its url.
    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String>;
}

/// The CI seam: what GitHub currently reports for one commit. `Ok(None)` means there is
/// no GitHub remote and nothing to wait on.
pub trait Ci {
    fn check_runs(&self, dir: &Path, sha: &str) -> Result<Option<CheckRuns>>;
}

/// Production `Pr`: the real `gh` PR operations.
pub struct GhPr;

impl Pr for GhPr {
    fn open_pr_exists(&self, dir: &Path, branch: &str) -> Result<bool> {
        github::open_pr_exists(dir, branch)
    }

    fn create_pr(&self, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<String> {
        github::create_pr(dir, branch, base, title, body)
    }
}

/// Production `Ci`: `gh api .../check-runs`.
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

/// The external-effect ports bundled together.
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
        "release: dir={} dry_run={} bump_type={:?} install={:?} standalone={:?} ci_gate={}",
        dir.display(),
        opts.dry_run,
        opts.bump_type,
        opts.install,
        opts.standalone,
        opts.ci_gate
    );
    if let Some(words) = &opts.standalone
        && words.trim().is_empty()
    {
        bail!("--standalone needs Scott's ordering words, quoted verbatim; an empty order is not an order.");
    }
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
/// preconditional `git fetch origin <default>`.
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
        HeadRemote::Equal => classify_equal(dir, opts, default),
    }
}

/// HEAD == origin/<default> and the tree is clean: a partial-release RESUME (the version
/// is committed and pushed but the remote tag is missing), a Scott-ordered standalone
/// release of what is already on the default branch, or truly nothing to do.
fn classify_equal(dir: &Path, opts: &ReleaseOpts, default: String) -> Result<ReleaseState> {
    debug!("classify_equal: dir={} default={}", dir.display(), default);
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        // Generic repo (version lives in tags): with nothing ahead, there is nothing to
        // release and no manifest version to resume-tag or bump alone.
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

    // The remote tag is the source of truth for "released": present on origin => done,
    // unless Scott ordered a standalone release of the commits that landed since.
    if git::remote_tag_sha(dir, &tag)?.is_some() {
        if opts.standalone.is_some() {
            let target_tag = compute_target_tag(dir, opts.bump_type)?;
            return Ok(ReleaseState::UngatedStandalone { target_tag, default });
        }
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

/// Classify a GATED repo: on the default branch it is a stranded-commits refusal, a
/// behind refusal, the "bump rides a PR" refusal, or (with an order) a standalone
/// release; on a feature branch it is fresh, already bumped, level mismatch, bump-only,
/// bad branch name, or generic-unsupported.
fn classify_gated(dir: &Path, opts: &ReleaseOpts, current: &str) -> Result<ReleaseState> {
    debug!("classify_gated: dir={} current={}", dir.display(), current);
    let default = git::remote_default_branch(dir)?;
    git::fetch_branch(dir, &default)?;

    if current == default {
        return match git::compare_head_to_remote(dir, &default)? {
            HeadRemote::Ahead | HeadRemote::Diverged => {
                let suggested_branch = suggest_rescue_branch(dir)?;
                Ok(ReleaseState::GatedStranded {
                    default,
                    suggested_branch,
                })
            }
            HeadRemote::Behind => Ok(ReleaseState::Behind { default }),
            HeadRemote::Equal => {
                if opts.standalone.is_some() {
                    if lang::detect(dir)?.is_empty() {
                        return Ok(ReleaseState::GatedGeneric);
                    }
                    let target_tag = compute_target_tag(dir, opts.bump_type)?;
                    Ok(ReleaseState::GatedStandalone { default, target_tag })
                } else {
                    Ok(ReleaseState::GatedDefaultClean { default })
                }
            }
        };
    }

    classify_gated_feature(dir, opts, current.to_string(), default)
}

/// Classify a gated repo when HEAD is on a FEATURE branch.
fn classify_gated_feature(
    dir: &Path,
    opts: &ReleaseOpts,
    branch: String,
    default: String,
) -> Result<ReleaseState> {
    debug!("classify_gated_feature: dir={} branch={}", dir.display(), branch);
    if branch.contains('/') || branch.contains('.') {
        return Ok(ReleaseState::GatedBadBranchName { branch });
    }

    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        return Ok(ReleaseState::GatedGeneric);
    }

    // The bump never rides alone. A branch with nothing on it, or with only version
    // lines on it, is the bump-only branch THE RULING forbids, unless Scott ordered it.
    let base = format!("origin/{default}");
    if opts.standalone.is_none() && is_bump_only_branch(dir, &base)? {
        return Ok(ReleaseState::GatedBumpOnlyBranch { branch, default });
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
    // prior gated run's version commit already rode this branch), and it is not the Rust
    // untouched-default 0.1.0 (which defers to the tag, so still a fresh release).
    if let (Some(v), Some(t)) = (&current_version, &latest_tag) {
        let is_untouched_default = project_type == ProjectType::Rust && *v == DEFAULT_UNTOUCHED_VERSION;
        if v != t && !is_untouched_default {
            let implied = version::bump_version(t, opts.bump_type);
            if implied != *v {
                return Ok(ReleaseState::GatedLevelMismatch {
                    riding: version::format_tag(v),
                    implied: version::format_tag(&implied),
                });
            }
            return Ok(ReleaseState::GatedAlreadyBumped {
                branch,
                default,
                tag: version::format_tag(v),
            });
        }
    }

    let target_tag = compute_target_tag(dir, opts.bump_type)?;
    Ok(ReleaseState::GatedFresh {
        branch,
        default,
        target_tag,
    })
}

/// Is HEAD's whole diff against `base` nothing at all, or version manifests and lockfiles
/// only? Either way the branch carries no work for a bump to ride.
fn is_bump_only_branch(dir: &Path, base: &str) -> Result<bool> {
    let files = git::changed_files(dir, base)?;
    let bump_only = files
        .iter()
        .all(|f| BUMP_ONLY_FILES.contains(&f.rsplit('/').next().unwrap_or(f.as_str())));
    debug!(
        "is_bump_only_branch: base={} changed={} bump_only={}",
        base,
        files.len(),
        bump_only
    );
    Ok(bump_only)
}

/// A deterministic suggested branch name for the stranded-commits rescue, derived from the
/// stranded HEAD's short SHA. Only ever printed in the refusal message.
fn suggest_rescue_branch(dir: &Path) -> Result<String> {
    let head = git::head_sha(dir)?;
    let short: String = head.chars().take(8).collect();
    Ok(format!("stranded-{short}"))
}

/// Compute the tag a fresh release would create, via bump's own `determine_version_action`.
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

/// The branch name a standalone release rides on: `bump-v1-21-0` for `v1.21.0`. No dots,
/// because the branch name is the PR title's source and slugifying collapses `.`.
pub fn standalone_branch_name(tag: &str) -> String {
    format!("bump-{}", tag.replace('.', "-"))
}

/// Derive the PR title from the branch name and the first commit subject: the branch's
/// words, prefixed with the conventional-commit type (and scope) of the first commit on
/// the branch, `chore` when that subject has none. By construction the title slugifies
/// back to the branch name, which is the rule `branch-pr-title-guard.sh` enforces.
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
        && (s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
    {
        return None;
    }
    Some((kind.to_string(), scope.map(str::to_string)))
}

/// The PR body: the branch's commit subjects, Scott's standalone order when there is
/// one, and the release-intent line the git-release-guard hook's Gate D looks for.
pub fn pr_body(subjects: &[String], tag: &str, standalone: Option<&str>) -> String {
    let mut body = String::new();
    for s in subjects {
        body.push_str("- ");
        body.push_str(s);
        body.push('\n');
    }
    if subjects.is_empty() {
        body.push_str("- version bump\n");
    }
    if let Some(words) = standalone {
        body.push_str(&format!("\nStandalone release ordered by Scott: \"{}\"\n", words.trim()));
    }
    body.push_str(&format!("\nRelease: rides this PR ({tag})\n"));
    body
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
        ReleaseState::UngatedStandalone { target_tag, default } => {
            println!(
                "STANDALONE ungated release of {target_tag}, ordered by Scott: \"{}\"",
                opts.standalone.as_deref().unwrap_or("").trim()
            );
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
             Commit a change first, then bump release. If Scott ordered a release of what is already on \
             {default}, run: bump release --standalone \"<his words>\""
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
        } => execute_gated(dir, opts, &branch, &default, Some(&target_tag), &target_tag, ports),
        ReleaseState::GatedAlreadyBumped { branch, default, tag } => {
            execute_gated(dir, opts, &branch, &default, None, &tag, ports)
        }
        ReleaseState::GatedStandalone { default, target_tag } => {
            execute_gated_standalone(dir, opts, &default, &target_tag, ports)
        }
        ReleaseState::GatedLevelMismatch { riding, implied } => bail!(
            "this branch already carries a version bump to {riding}, but the requested level implies {implied}.\n\
             bump refuses to name two versions: either drop the -m/-M flag to keep {riding}, or reset the branch's \
             bump commit and re-run for {implied}."
        ),
        ReleaseState::GatedBumpOnlyBranch { branch, default } => bail!(
            "branch '{branch}' carries no work for a bump to ride: its whole diff against origin/{default} is \
             empty or version lines only, which makes it a bump-only release branch, forbidden (THE RULING \
             2026-07-03, ~/HALL-OF-SHAME.md).\n\
             The version bump rides the feature PR: commit the real change on this branch first, then bump release.\n\
             If Scott explicitly ordered a standalone release, that order is the exception; run it quoted:\n  \
             bump release --standalone \"<his words>\"\n\
             Otherwise STOP and ask Scott; do not rename the branch, hand-edit the version, or pad the diff."
        ),
        ReleaseState::GatedBadBranchName { branch } => {
            let fixed: String = branch.chars().map(|c| if c == '/' || c == '.' { '-' } else { c }).collect();
            bail!(
                "branch '{branch}' carries '/' or '.', so no conventional PR title can slugify back to it \
                 (general.md: the branch name is the PR title's source of truth).\n\
                 Rename it before anything is pushed: git branch -m {fixed}, then bump release"
            )
        }
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
             Run: git checkout -b <feature>, commit your change, then bump release.\n\
             If Scott explicitly ordered a standalone release of what is already on {default}, run it quoted:\n  \
             bump release --standalone \"<his words>\""
        ),
        ReleaseState::GatedGeneric => bail!(
            "this repo is GATED and has no version-bearing manifest (generic).\n\
             Gated generic repos are unsupported: bump finish cannot derive a version without a manifest."
        ),
    }
}

/// Block until every check on `sha` has completed, and refuse on any failure. No tag
/// exists when this runs, so a red result costs a re-run and nothing else.
fn wait_for_green<C: Ci>(ci: &C, dir: &Path, sha: &str, gate: bool, timeout: Duration, dry_run: bool) -> Result<()> {
    debug!(
        "wait_for_green: dir={} sha={} gate={} timeout={:?} dry_run={}",
        dir.display(),
        sha,
        gate,
        timeout,
        dry_run
    );
    if !gate {
        println!("CI gate: SKIPPED (--no-ci-gate); a red run will cost a second tag");
        return Ok(());
    }
    if dry_run {
        println!("[dry-run] wait for CI to go green on {sha}, then tag");
        return Ok(());
    }
    let start = Instant::now();
    let mut announced = false;
    loop {
        let Some(summary) = ci.check_runs(dir, sha)? else {
            println!("CI gate: no GitHub remote, nothing to wait on");
            return Ok(());
        };
        if !summary.failed.is_empty() {
            let mut lines = String::new();
            for (name, url) in &summary.failed {
                lines.push_str(&format!("  FAILED  {name}  {url}\n"));
            }
            bail!(
                "CI is RED on {sha}:\n{lines}NO TAG WAS CREATED, which is the point: fix it, commit, push, and re-run. \
                 The re-run tags the SAME version instead of bumping past it, so this costs zero version numbers."
            );
        }
        if summary.total > 0 && summary.incomplete == 0 {
            println!("CI gate: green ({} checks) on {sha}", summary.total);
            return Ok(());
        }
        if summary.total == 0 && start.elapsed() >= CI_APPEAR {
            println!(
                "CI gate: no checks registered on {sha} after {}s, nothing to wait on",
                CI_APPEAR.as_secs()
            );
            return Ok(());
        }
        if start.elapsed() >= timeout {
            bail!(
                "CI gate: timed out after {}s with {} check(s) still running on {sha}. NO TAG WAS CREATED. \
                 Re-run when they finish (the same version is reused), or pass --ci-timeout <secs>.",
                timeout.as_secs(),
                summary.incomplete
            );
        }
        if !announced {
            println!("CI gate: waiting on checks for {sha} (no tag exists yet, so red is free)");
            announced = true;
        }
        println!(
            "  ... {}/{} checks complete",
            summary.total - summary.incomplete,
            summary.total
        );
        thread::sleep(CI_POLL);
    }
}

/// Fresh ungated release: version commit -> push branch -> confirm on origin -> wait for
/// green CI -> tag -> push tag by name -> install.
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
        println!("[dry-run] git push origin {default}");
        println!("[dry-run] (confirm HEAD is on origin/{default} before tagging)");
        wait_for_green(ports.ci, dir, "<HEAD>", opts.ci_gate, opts.ci_timeout, true)?;
        println!("[dry-run] git tag -a {target_tag} -m \"Release {target_tag}\"");
        println!("[dry-run] git push origin {target_tag}");
        echo_install(&install_command);
        return Ok(ReleaseReport {
            tag: target_tag.to_string(),
            resumed: false,
            paused: false,
            install_command,
            dry_run: true,
            pr_url: None,
        });
    }

    // 1. The version commit is the existing `--no-tag` code path, reused verbatim.
    version_commit(dir, opts.bump_type)?;
    // 2. Push the branch FIRST.
    ports.pusher.push_branch(dir, default)?;
    // 3. Confirm it landed on origin BEFORE any tag exists.
    confirm_on_origin(dir, default)?;
    // 4. Wait for CI on that exact sha. Red here costs nothing: no tag exists.
    let head = git::head_sha(dir)?;
    wait_for_green(ports.ci, dir, &head, opts.ci_gate, opts.ci_timeout, false)?;
    // 5. Only now create the annotated tag on the confirmed, green commit.
    let message = format!("Release {target_tag}");
    git::create_tag(dir, target_tag, &message)?;
    // 6. Push the tag BY EXPLICIT NAME.
    ports.pusher.push_tag(dir, target_tag)?;
    println!("Released {target_tag} on {default}");
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        tag: target_tag.to_string(),
        resumed: false,
        paused: false,
        install_command,
        dry_run: false,
        pr_url: None,
    })
}

/// Partial-release RESUME: never re-bump, never claim "already released". Wait for CI,
/// create the annotated tag only if it is absent locally, then push it by name and install.
fn execute_resume<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    tag: &str,
    default: &str,
    local_tag_present: bool,
    ports: &Ports<P, I, R, C>,
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
        wait_for_green(ports.ci, dir, "<HEAD>", opts.ci_gate, opts.ci_timeout, true)?;
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
            pr_url: None,
        });
    }

    println!("RESUMING: {tag} is already committed on {default} and carries no remote tag; tagging it, not bumping past it");
    // The version is already on origin (that is what makes this a resume), but confirm
    // before anything irreversible so the invariant holds on this path too.
    confirm_on_origin(dir, default)?;
    let head = git::head_sha(dir)?;
    wait_for_green(ports.ci, dir, &head, opts.ci_gate, opts.ci_timeout, false)?;
    if !local_tag_present {
        let message = format!("Release {tag}");
        git::create_tag(dir, tag, &message)?;
    }
    ports.pusher.push_tag(dir, tag)?;
    println!("Resumed release: pushed {tag} on {default}");
    let install_command = run_install(dir, &opts.install, config, ports.installer)?;
    Ok(ReleaseReport {
        tag: tag.to_string(),
        resumed: true,
        paused: false,
        install_command,
        dry_run: false,
        pr_url: None,
    })
}

/// The GATED release flow: on a feature branch, ride the version bump on the branch (fresh
/// only), push the branch with `--no-follow-tags -u`, ensure an OPEN PR exists (probe, then
/// create with a derived title and a body carrying the release-intent line), and PAUSE.
/// NO tag is created or pushed here; tagging the merged commit is `bump finish`'s job.
fn execute_gated<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    branch: &str,
    default: &str,
    fresh_target: Option<&str>,
    report_tag: &str,
    ports: &Ports<P, I, R, C>,
) -> Result<ReleaseReport> {
    debug!(
        "execute_gated: dir={} branch={} default={} fresh_target={:?} report_tag={} dry_run={}",
        dir.display(),
        branch,
        default,
        fresh_target,
        report_tag,
        opts.dry_run
    );
    let base = format!("origin/{default}");

    if opts.dry_run {
        match fresh_target {
            Some(target) => {
                println!("[dry-run] bump --no-tag  (commit the version bump for {target} on {branch})");
            }
            None => {
                println!("[dry-run] (version already bumped on {branch}; no re-bump)");
            }
        }
        let subjects = git::commit_subjects(dir, &base)?;
        println!("[dry-run] git push --no-follow-tags -u origin {branch}");
        println!("[dry-run] gh pr list --head {branch} --state open --json number  (open-PR probe)");
        println!(
            "[dry-run] gh pr create --head {branch} --base {default} --title \"{}\" --body <subjects + release line>  (only if no open PR)",
            pr_title(branch, &subjects)
        );
        println!("[dry-run] {GATED_PAUSE_MESSAGE}");
        return Ok(ReleaseReport {
            tag: report_tag.to_string(),
            resumed: false,
            paused: true,
            install_command: None,
            dry_run: true,
            pr_url: None,
        });
    }

    // 1. Fresh: ride the version bump on the branch via the `--no-tag` path (no tag).
    //    Idempotent re-run: skip, the bump already rode the branch.
    if let Some(target) = fresh_target {
        debug!("execute_gated: fresh bump for {target}");
        version_commit(dir, opts.bump_type)?;
    } else {
        debug!("execute_gated: version already bumped on {branch}; skipping re-bump");
    }

    // 2. Push the feature branch with `--no-follow-tags -u`.
    ports.pusher.push_feature_branch(dir, branch)?;

    // 3. Ensure an OPEN PR exists: probe first, create only if none is open. The title
    //    and body are built here, by construction, so the PR always carries a title that
    //    slugifies to the branch and the release-intent line the hook's Gate D wants.
    let mut pr_url = None;
    if ports.pr.open_pr_exists(dir, branch)? {
        println!("open PR already exists for {branch}; not creating another");
    } else {
        let subjects = git::commit_subjects(dir, &base)?;
        let title = pr_title(branch, &subjects);
        let body = pr_body(&subjects, report_tag, opts.standalone.as_deref());
        let url = ports.pr.create_pr(dir, branch, default, &title, &body)?;
        println!("opened PR for {branch}: {url}");
        pr_url = Some(url);
    }

    // 4. PAUSE. No tag, no install: both are `bump finish`'s after the merge.
    println!("{GATED_PAUSE_MESSAGE}");
    Ok(ReleaseReport {
        tag: report_tag.to_string(),
        resumed: false,
        paused: true,
        install_command: None,
        dry_run: false,
        pr_url,
    })
}

/// Scott-ordered standalone release on a GATED repo, from a clean default branch equal to
/// origin: cut `bump-vX-Y-Z` off origin/<default> (tracking it, so the version commit is
/// a new commit and never an amend of something already on origin), then run the normal
/// gated flow on it, with Scott's words quoted in the PR body.
fn execute_gated_standalone<P: Pusher, I: Installer, R: Pr, C: Ci>(
    dir: &Path,
    opts: &ReleaseOpts,
    default: &str,
    target_tag: &str,
    ports: &Ports<P, I, R, C>,
) -> Result<ReleaseReport> {
    let branch = standalone_branch_name(target_tag);
    let upstream = format!("origin/{default}");
    debug!(
        "execute_gated_standalone: dir={} default={} target_tag={} branch={}",
        dir.display(),
        default,
        target_tag,
        branch
    );
    println!(
        "STANDALONE gated release of {target_tag}, ordered by Scott: \"{}\"",
        opts.standalone.as_deref().unwrap_or("").trim()
    );
    if opts.dry_run {
        println!("[dry-run] git checkout -b {branch} --track {upstream}");
    } else {
        git::checkout_new_tracking(dir, &branch, &upstream)?;
        println!("cut {branch} off {upstream}");
    }
    execute_gated(dir, opts, &branch, default, Some(target_tag), target_tag, ports)
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
        never_amend: true,
        tag_only: false,
        gates: false,
        no_verify: false,
        skip_member: Vec::new(),
        directories: Vec::new(),
    };
    process_directory(dir, &cli, bump_type)
}

/// Re-fetch and require HEAD == origin/<default> before any tag is created.
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

/// The worktree `bump finish` operates in. From the default branch's own checkout that is
/// `dir`. From a feature-branch worktree it is the sibling worktree that has the default
/// branch checked out, which under a worktree layout is where the default branch lives and
/// where `git checkout <default>` would otherwise fail with "already checked out". With
/// neither, the default branch is checked out here.
fn finish_dir(dir: &Path, default: &str) -> Result<PathBuf> {
    let current = git::current_branch(dir)?;
    if current == default {
        return Ok(dir.to_path_buf());
    }
    if let Some(wt) = git::worktree_for_branch(dir, default)? {
        println!("{default} is checked out in {}; finishing there", wt.display());
        return Ok(wt);
    }
    git::checkout(dir, default)?;
    Ok(dir.to_path_buf())
}

/// `bump finish`: the gated post-merge step. Reach the merged tip of the default branch
/// (in whichever worktree holds it), wait for its CI, then, reusing `tag_ladder`, either
/// tag the merged commit and push it BY NAME, resume a local-only tag, no-op an
/// already-released tag, or refuse (missed bump / gated generic / dirty).
pub fn finish<P: Pusher, I: Installer, C: Ci>(
    dir: &Path,
    opts: &FinishOpts,
    pusher: &P,
    installer: &I,
    ci: &C,
) -> Result<ReleaseReport> {
    debug!(
        "finish: dir={} dry_run={} install={:?} ci_gate={}",
        dir.display(),
        opts.dry_run,
        opts.install,
        opts.ci_gate
    );

    if !git::is_git_repo(dir) {
        bail!("not a git repository: {}", dir.display());
    }

    // Generic repo (no version-bearing manifest): finish cannot derive a version to tag.
    let manifests = lang::detect(dir)?;
    if manifests.is_empty() {
        bail!(
            "this repo has no version-bearing manifest (generic).\n\
             Gated generic repos are unsupported: bump finish cannot derive a version without a manifest."
        );
    }

    let default = git::remote_default_branch(dir)?;

    if opts.dry_run {
        let config = config::load(dir)?;
        return finish_dry_run(dir, opts, &config, &manifests, &default);
    }

    // Dirty tree: the checkout / fast-forward would clobber TRACKED changes. Untracked
    // files are fine: finish never stages or commits anything. Checked on the current
    // worktree before any mutation, and again on the resolved one below.
    if git::has_tracked_changes(dir)? {
        bail!(
            "the working tree has uncommitted tracked changes; bump finish fast-forwards the \
             default branch, which would clobber them.\n\
             Commit or stash your changes first, then bump finish"
        );
    }

    let work = finish_dir(dir, &default)?;
    let work = work.as_path();
    if work != dir && git::has_tracked_changes(work)? {
        bail!(
            "{} has uncommitted tracked changes; bump finish fast-forwards {default} there, which would clobber them.\n\
             Commit or stash them first, then bump finish",
            work.display()
        );
    }
    let config = config::load(work)?;
    git::pull_ff_only(work, &default)?;

    // Reuse the --tag-only verification ladder (clean-tree, HEAD==origin, manifest-version
    // -> tag, remote-then-local existence). The consumer decides the action.
    let check = tag_ladder(work)?;
    let tag = check.tag.clone();
    debug!("finish: tag={} state={:?}", tag, check.state);

    match check.state {
        // Local-only tag at the merged commit: a prior run died before/during the tag push.
        // RESUME: wait for CI (the tag is still private), push by name, install.
        TagState::LocalAtHead => {
            wait_for_green(ci, work, &check.head, opts.ci_gate, opts.ci_timeout, false)?;
            pusher.push_tag(work, &tag)?;
            println!("resumed release: pushed {tag} on {default}");
            let install_command = run_install(work, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                tag,
                resumed: true,
                paused: false,
                install_command,
                dry_run: false,
                pr_url: None,
            })
        }
        // origin/<default> carries an untagged version (the merged bump): wait for CI,
        // tag the merged commit, push by name, install.
        TagState::Absent => {
            wait_for_green(ci, work, &check.head, opts.ci_gate, opts.ci_timeout, false)?;
            let message = format!("Release {tag}");
            git::create_tag(work, &tag, &message)?;
            pusher.push_tag(work, &tag)?;
            println!("released {tag} on {default}");
            let install_command = run_install(work, &opts.install, &config, installer)?;
            Ok(ReleaseReport {
                tag,
                resumed: false,
                paused: false,
                install_command,
                dry_run: false,
                pr_url: None,
            })
        }
        // The tag exists on the REMOTE. Resolve its actual COMMIT: at the merged tip it is
        // already released (no-op); elsewhere the version was not bumped for this merge.
        TagState::RemoteAtHead | TagState::RemoteAtOther(_) => match git::remote_tag_commit(work, &tag)? {
            Some(commit) if commit == check.head => {
                println!("already released {tag}");
                Ok(ReleaseReport {
                    tag,
                    resumed: false,
                    paused: false,
                    install_command: None,
                    dry_run: false,
                    pr_url: None,
                })
            }
            _ => missed_bump(&default),
        },
        // The tag exists LOCALLY at a commit OTHER than the merged tip: nothing new merged
        // with a bump.
        TagState::LocalAtOther(_) => missed_bump(&default),
    }
}

/// The missed-bump refusal: a commit merged to the default branch without a version bump,
/// so origin/<default>'s version still equals the last tag.
fn missed_bump(default: &str) -> Result<ReleaseReport> {
    bail!(
        "no untagged version on {default}; bump rides a feature PR -- run bump release on a branch. \
         If Scott ordered a release of what already merged: bump release --standalone \"<his words>\" from {default}"
    )
}

/// `-n` dry run for `bump finish`: echo every command it would run and mutate NOTHING.
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
    println!("[dry-run] (in the worktree holding {default}, or `git checkout {default}` here)");
    println!("[dry-run] git pull --ff-only origin {default}");
    println!("[dry-run] (tag-only ladder: require HEAD == origin/{default} before tagging)");
    if opts.ci_gate {
        println!("[dry-run] wait for CI to go green on the merged commit, then tag");
    } else {
        println!("[dry-run] CI gate: SKIPPED (--no-ci-gate)");
    }
    println!("[dry-run] git tag -a {tag} -m \"Release {tag}\"  (only if the merged version is untagged)");
    println!("[dry-run] git push origin {tag}");
    echo_install(&install_command);
    Ok(ReleaseReport {
        tag,
        resumed: false,
        paused: false,
        install_command,
        dry_run: true,
        pr_url: None,
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
