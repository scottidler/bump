//! `bump release` -- the release-verb state machine (UNGATED flow, Phase 5).
//!
//! This module absorbs the bash release driver's mechanical steps behind ONE verb that
//! inspects the repo's typed state and either executes the single correct sequence or
//! refuses with the exact next command. It operates entirely on bump's typed internals
//! (`github::Gate`, `VersionAction`, the `git::*` helpers, the `lang` adapter seam) --
//! ZERO stdout scraping of any `bump`/`git` output.
//!
//! Scope of THIS phase: the UNGATED rows of the `bump release` state table only. The
//! gated / feature-branch / PR rows are Phase 6 and `bump finish` is Phase 7; a gated
//! verdict here refuses with a clear "not this phase" message. The clap `bump release`
//! subcommand and the `--install`/`--no-install` flags are Phase 8 -- this phase's
//! surface is the callable `release(dir, opts, pusher, installer)` function, driven in
//! tests via injected `Pusher`/`Installer` doubles.
//!
//! Module gating: the whole module is `#[cfg(test)]` this phase. bump is a BIN crate,
//! so any item not reached from `main` is `dead_code`, which `cargo clippy -- -D
//! warnings` (this repo's CI, per Phase 3's notes) rejects. With no CLI wiring yet (a
//! deliberate phase boundary), the only reachable callers are the tests. Phase 8 removes
//! the `#[cfg(test)]` gate when it wires the subcommand and calls `release()` from
//! `process`/`main`. The state machine is fully compiled (clippy `--all-targets` builds
//! the test target) and fully tested; it is simply not yet in the shipped binary.
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
use crate::lang::{self, Manifest, ManifestVersion};
use crate::version::{self, BumpType};
use crate::{determine_version_action, process_directory};
use eyre::{Context, Result, bail};
use log::debug;
use semver::Version;
use std::path::Path;
use std::process::Command;

/// The default install command when none is configured and a Cargo manifest is present.
const DEFAULT_INSTALL_COMMAND: &str = "cargo install --path .";

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

/// The outcome of a successful `release()` (refusals are `Err`). Lets callers/tests
/// assert what happened without scraping stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseReport {
    /// The tag created and/or pushed (`vX.Y.Z`).
    pub tag: String,
    /// True when this completed a partial-release RESUME rather than a fresh release.
    pub resumed: bool,
    /// The resolved install command that ran (`None` = install skipped).
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
    /// Gated repo (Phase 6).
    Gated,
    /// Gate probe inconclusive: `release` pushes, so it FAILS CLOSED.
    Unknown { reason: String },
}

/// Pushes a branch / tag to origin. A port so tests can record ordering and inject a
/// rejected push without touching a real remote for the failure case.
pub trait Pusher {
    fn push_branch(&self, dir: &Path, branch: &str) -> Result<()>;
    fn push_tag(&self, dir: &Path, tag: &str) -> Result<()>;
}

/// Runs the post-release install command. A port so tests assert the RESOLVED command
/// without executing a real (slow, outward) `cargo install`.
pub trait Installer {
    fn install(&self, dir: &Path, command: &str) -> Result<()>;
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
}

/// The external-effect ports bundled together, so the execution functions stay under the
/// argument-count limit and the two seams travel as one unit (rules/rust.md `Deps`).
struct Ports<'a, P: Pusher, I: Installer> {
    pusher: &'a P,
    installer: &'a I,
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
pub fn release<P: Pusher, I: Installer>(
    dir: &Path,
    opts: &ReleaseOpts,
    pusher: &P,
    installer: &I,
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
    let ports = Ports { pusher, installer };
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
        Gate::Gated(_) => return Ok(ReleaseState::Gated),
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
fn execute<P: Pusher, I: Installer>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    state: ReleaseState,
    ports: &Ports<P, I>,
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
        ReleaseState::Gated => bail!(
            "this repo is GATED; the gated bump release flow lands in a later phase and is not implemented here.\n\
             Until then use the gated primitives: bump --no-tag on a feature branch, PR, merge, then bump --tag-only"
        ),
    }
}

/// Fresh ungated release: version commit -> push branch -> confirm on origin -> tag ->
/// push tag by name -> install. The confirm step is the strengthened-ordering guard.
fn execute_release<P: Pusher, I: Installer>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    target_tag: &str,
    default: &str,
    ports: &Ports<P, I>,
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
        install_command,
        dry_run: false,
    })
}

/// Partial-release RESUME: never re-bump, never claim "already released". Create the
/// annotated tag only if it is absent locally, then push it by name and install.
fn execute_resume<P: Pusher, I: Installer>(
    dir: &Path,
    opts: &ReleaseOpts,
    config: &Config,
    tag: &str,
    default: &str,
    local_tag_present: bool,
    ports: &Ports<P, I>,
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
        install_command,
        dry_run: false,
    })
}

/// The internal version commit: bump the version file(s) and commit, NO tag. This is
/// exactly `bump --no-tag`'s `process_directory` code path, reused.
fn version_commit(dir: &Path, bump_type: BumpType) -> Result<()> {
    debug!("version_commit: dir={} bump_type={:?}", dir.display(), bump_type);
    let cli = Cli {
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
