use clap::Parser;
use eyre::{Context, Result, bail};
use log::{info, warn};
use semver::Version;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

mod cargo;
mod cli;
mod git;
mod github;
mod python;
mod version;

use cli::Cli;
use version::BumpType;

/// Detected project type for a directory
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectType {
    Rust,
    Python,
    Generic,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectType::Rust => write!(f, "Rust"),
            ProjectType::Python => write!(f, "Python"),
            ProjectType::Generic => write!(f, "Generic"),
        }
    }
}

/// Detect the project type for a directory
fn detect_project_type(dir: &Path) -> ProjectType {
    if cargo::cargo_toml_exists(dir) {
        ProjectType::Rust
    } else if python::pyproject_toml_exists(dir) {
        ProjectType::Python
    } else {
        ProjectType::Generic
    }
}

/// Read the file version for a project type
fn read_file_version(dir: &Path, project_type: ProjectType) -> Result<Option<String>> {
    match project_type {
        ProjectType::Rust => {
            let cargo_path = cargo::cargo_toml_path(dir);
            cargo::read_version(&cargo_path)
        }
        ProjectType::Python => {
            let pyproject_path = python::pyproject_toml_path(dir);
            python::read_version(&pyproject_path)
        }
        ProjectType::Generic => Ok(None),
    }
}

/// Write the file version for a project type
fn write_file_version(dir: &Path, project_type: ProjectType, new_version: &str) -> Result<()> {
    match project_type {
        ProjectType::Rust => {
            let cargo_path = cargo::cargo_toml_path(dir);
            cargo::write_version(&cargo_path, new_version)
        }
        ProjectType::Python => {
            let pyproject_path = python::pyproject_toml_path(dir);
            python::write_version(&pyproject_path, new_version)
        }
        ProjectType::Generic => Ok(()),
    }
}

/// Sync lockfile for a project type
fn sync_lockfile(dir: &Path, project_type: ProjectType) -> Result<()> {
    match project_type {
        ProjectType::Rust => cargo::sync_lockfile(dir),
        ProjectType::Python => python::sync_lockfile(dir),
        ProjectType::Generic => Ok(()),
    }
}

/// Get the version file name for display purposes
fn version_file_name(project_type: ProjectType) -> &'static str {
    match project_type {
        ProjectType::Rust => "Cargo.toml",
        ProjectType::Python => "pyproject.toml",
        ProjectType::Generic => "",
    }
}

/// Check if staged files are only version-related files
fn is_version_files_only(staged_files: &[String], project_type: ProjectType) -> bool {
    match project_type {
        ProjectType::Rust => staged_files.iter().all(|f| f == "Cargo.toml" || f == "Cargo.lock"),
        ProjectType::Python => staged_files.iter().all(|f| f == "pyproject.toml"),
        ProjectType::Generic => staged_files.is_empty(),
    }
}

/// Validate project-specific constraints
fn validate_project(dir: &Path, project_type: ProjectType) -> Result<()> {
    if project_type == ProjectType::Rust {
        let independent_members = cargo::check_workspace_independent_versions(dir)?;
        if !independent_members.is_empty() {
            let member_list: Vec<String> = independent_members
                .iter()
                .map(|m| format!("  - {} ({}): {}", m.name, m.path, m.version))
                .collect();
            bail!(
                "Workspace members have independent versions (not using version.workspace = true):\n{}\n\n\
                 bump only supports workspaces with a unified version in [workspace.package].",
                member_list.join("\n")
            );
        }
    }
    Ok(())
}

/// XDG data dir, honoring `$XDG_DATA_HOME` and falling back to `$HOME/.local/share`.
///
/// We deliberately do NOT use the `dirs` config/data helpers: those honor
/// `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` only on Linux. On macOS they resolve via system
/// APIs and return `~/Library/...`, ignoring the env vars. These helpers resolve to the
/// same XDG layout on every platform.
fn xdg_data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|h| h.join(".local").join("share"))
}

fn setup_logging() -> Result<()> {
    let log_dir = xdg_data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bump")
        .join("logs");

    fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

    let log_file = log_dir.join("bump.log");

    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

/// Prompt user for commit message using an editor (like git does)
fn prompt_commit_message_with_editor(staged_files: &[String]) -> Result<String> {
    // Create temp file with template
    let temp_file = NamedTempFile::new().context("Failed to create temp file for commit message")?;

    let staged_list = staged_files
        .iter()
        .map(|f| format!("#   {}", f))
        .collect::<Vec<_>>()
        .join("\n");

    let template = format!(
        "\n\
# Enter commit message above.\n\
# Lines starting with '#' will be ignored.\n\
#\n\
# Staged changes:\n\
{}\n\
#\n\
# An empty message aborts the commit.\n",
        staged_list
    );

    fs::write(temp_file.path(), &template).context("Failed to write commit message template")?;

    // Determine editor: $VISUAL -> $EDITOR -> vim
    let editor = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_else(|_| "vim".to_string());

    // Open editor
    let status = Command::new(&editor)
        .arg(temp_file.path())
        .status()
        .with_context(|| format!("Failed to open editor: {}", editor))?;

    if !status.success() {
        bail!("Editor exited with error");
    }

    // Read and process result
    let content = fs::read_to_string(temp_file.path()).context("Failed to read commit message")?;

    let message: String = content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if message.is_empty() {
        bail!("Aborting commit due to empty commit message");
    }

    Ok(message)
}

/// Result of determining what version action to take
#[derive(Debug)]
struct VersionAction {
    /// The version to tag
    target_version: Version,
    /// Whether we need to update the version file
    needs_file_update: bool,
    /// Whether this is an initial tag (no bump) vs a version bump
    is_initial_tag: bool,
}

/// The default "untouched" version in Cargo.toml
const DEFAULT_UNTOUCHED_VERSION: Version = Version::new(0, 1, 0);

/// Determine what version action to take
fn determine_version_action(
    dir: &Path,
    file_version: Option<Version>,
    project_type: ProjectType,
    bump_type: BumpType,
) -> Result<VersionAction> {
    // Get latest git tag (if any exist)
    let latest_tag_version = git::get_latest_tag(dir)?.and_then(|t| version::parse_version(&t).ok());

    // Only Rust has the "untouched default" concept (Cargo.toml starts at 0.1.0)
    let is_untouched_default =
        project_type == ProjectType::Rust && file_version.as_ref() == Some(&DEFAULT_UNTOUCHED_VERSION);

    // Determine the base version to bump from
    match (&file_version, &latest_tag_version) {
        // Case: Both file version and git tags exist
        (Some(file_ver), Some(tag)) => {
            if is_untouched_default {
                // File is at default untouched version - defer to git tag
                info!(
                    "{} is at default 0.1.0, using git tag {} as base.",
                    version_file_name(project_type),
                    version::format_tag(tag)
                );
                let bumped = version::bump_version(tag, bump_type);
                Ok(VersionAction {
                    target_version: bumped,
                    needs_file_update: true,
                    is_initial_tag: false,
                })
            } else if file_ver == tag {
                // File version matches latest tag - bump from it
                info!(
                    "{} matches latest tag {}. Bumping.",
                    version_file_name(project_type),
                    version::format_tag(file_ver)
                );
                let bumped = version::bump_version(file_ver, bump_type);
                Ok(VersionAction {
                    target_version: bumped,
                    needs_file_update: true,
                    is_initial_tag: false,
                })
            } else {
                // File version doesn't match latest tag - ERROR
                bail!(
                    "Version mismatch: {} has {} but latest git tag is {}. \
                    Please sync them manually before running bump.",
                    version_file_name(project_type),
                    version::format_file_version(file_ver),
                    version::format_tag(tag)
                );
            }
        }

        // Case: File version exists, no git tags
        (Some(file_ver), None) => {
            let file_tag = version::format_tag(file_ver);
            // No tags exist - create initial tag for file version
            info!(
                "No git tags found. Creating initial tag {} from {}.",
                file_tag,
                version_file_name(project_type)
            );
            Ok(VersionAction {
                target_version: file_ver.clone(),
                needs_file_update: false,
                is_initial_tag: true,
            })
        }

        // Case: No file version, but git tags exist
        (None, Some(tag)) => {
            info!(
                "No version in {}. Using git tag {} as base.",
                version_file_name(project_type),
                version::format_tag(tag)
            );
            let bumped = version::bump_version(tag, bump_type);
            let needs_update = project_type != ProjectType::Generic;
            Ok(VersionAction {
                target_version: bumped,
                needs_file_update: needs_update,
                is_initial_tag: false,
            })
        }

        // Case: No version anywhere
        (None, None) => {
            info!("No version found anywhere. Starting at 0.1.0");
            let needs_update = project_type != ProjectType::Generic;
            Ok(VersionAction {
                target_version: Version::new(0, 1, 0),
                needs_file_update: needs_update,
                is_initial_tag: true,
            })
        }
    }
}

/// Determine the commit message based on CLI flags and context
fn determine_commit_message(
    cli: &Cli,
    new_tag: &str,
    staged_files: &[String],
    is_initial_tag: bool,
    project_type: ProjectType,
) -> Result<String> {
    // Priority 1: User provided --message
    if let Some(ref msg) = cli.message {
        return Ok(msg.clone());
    }

    // Priority 2: User requested --automatic
    if cli.automatic {
        return Ok(format!("Bump version to {}", new_tag));
    }

    // Priority 3: Auto-generate for version-only changes
    if staged_files.is_empty() {
        return Ok(format!("Release {}", new_tag));
    }

    if is_version_files_only(staged_files, project_type) {
        if is_initial_tag {
            return Ok(format!("Release {}", new_tag));
        } else {
            return Ok(format!("Bump version to {}", new_tag));
        }
    }

    // Priority 4: Open editor for complex changes
    prompt_commit_message_with_editor(staged_files)
}

/// Build the gated-repo refusal message: names the gated branch/repo and the
/// blocking rules, explains the orphan risk, and prints the gated release recipe.
fn gated_refusal_message(label: &str, rules: &[String]) -> String {
    format!(
        "{label} is gated ({}).\n\
         Tagging here would orphan the tag (squash-merge rewrites the SHA).\n\n\
         Gated flow:\n  \
         bump --no-tag [-m|-M]      # version bump rides your branch/PR\n  \
         <push branch, open PR, merge>\n  \
         git checkout main && git pull --ff-only origin main\n  \
         bump --tag-only            # tag the merged commit\n  \
         git push origin vX.Y.Z",
        rules.join(", ")
    )
}

/// --gates: probe and report the branch-protection verdict and the recommended
/// release flow for this repo, then exit 0. Purely informational.
fn report_gates(dir: &Path) -> Result<()> {
    info!("report_gates: dir={}", dir.display());

    let slug = github::remote_slug(dir).unwrap_or_else(|| "(no github remote)".to_string());
    let branch = github::local_default_branch(dir).unwrap_or_else(|| "(unknown)".to_string());

    println!("Repo:   {slug}");
    println!("Branch: {branch}");

    match github::detect(dir) {
        github::Gate::Ungated => {
            println!("Gates:  none (ungated)");
            println!();
            println!("Ungated flow:");
            println!("  bump [-m|-M]");
            println!("  git push origin {branch}");
            println!("  git push origin vX.Y.Z");
        }
        github::Gate::Gated(rules) => {
            println!("Gates:  {} (gated)", rules.join(", "));
            println!();
            println!("Gated flow:");
            println!("  bump --no-tag [-m|-M]      # version bump rides your branch/PR");
            println!("  <push branch, open PR, merge>");
            println!("  git checkout {branch} && git pull --ff-only origin {branch}");
            println!("  bump --tag-only            # tag the merged commit");
            println!("  git push origin vX.Y.Z");
        }
        github::Gate::Unknown(reason) => {
            println!("Gates:  UNKNOWN (could not verify: {reason})");
            println!();
            println!("Re-run `bump --gates` once online / authenticated for a verdict.");
        }
    }

    Ok(())
}

/// --tag-only: create the annotated tag for the current manifest version on HEAD,
/// after verifying HEAD is exactly the merged default-branch commit. No version
/// change, no commit. Every check must hold or it exits non-zero with no mutation.
fn tag_only(dir: &Path) -> Result<()> {
    info!("tag_only: dir={}", dir.display());

    // 1. Working tree must be clean.
    if git::has_uncommitted_changes(dir)? {
        bail!("--tag-only requires a clean working tree; commit or stash changes first.");
    }

    // 2. Must be on the remote default branch.
    let default = git::remote_default_branch(dir)?;
    let current = git::current_branch(dir)?;
    if current != default {
        bail!(
            "--tag-only must run on the default branch '{default}', but HEAD is on '{current}'.\n\
             Run: git checkout {default} && git pull --ff-only origin {default}"
        );
    }

    // 3. HEAD must equal origin/<default> EXACTLY (not merely an ancestor).
    git::fetch_branch(dir, &default)?;
    match git::compare_head_to_remote(dir, &default)? {
        git::HeadRemote::Equal => {}
        git::HeadRemote::Behind => bail!(
            "HEAD is behind origin/{default}; the merged bump commit isn't checked out.\n\
             Run: git pull --ff-only origin {default}"
        ),
        git::HeadRemote::Ahead => bail!(
            "HEAD is ahead of origin/{default}; the commit to tag is not merged/pushed yet.\n\
             Merge the PR first, then run --tag-only on the merged commit."
        ),
        git::HeadRemote::Diverged => bail!(
            "HEAD has diverged from origin/{default}; reconcile before tagging.\n\
             Run: git pull --ff-only origin {default}"
        ),
    }

    // 4. Manifest version -> tag name.
    let project_type = detect_project_type(dir);
    let file_version = read_file_version(dir, project_type)?
        .and_then(|v| version::parse_version(&v).ok())
        .ok_or_else(|| {
            eyre::eyre!(
                "--tag-only needs a version in {}; none found. \
                 (Generic/tag-only projects have no manifest version to tag.)",
                version_file_name(project_type)
            )
        })?;
    let new_tag = version::format_tag(&file_version);
    let head = git::head_sha(dir)?;

    // 5. Tag-existence check, remote then local.
    if let Some(remote_sha) = git::remote_tag_sha(dir, &new_tag)? {
        if remote_sha == head {
            println!("{new_tag} is already tagged at HEAD on origin. Nothing to do.");
            return Ok(());
        }
        bail!(
            "Tag {new_tag} already exists on origin at {remote_sha}, not HEAD ({head}).\n\
             Resolving that is manual tag surgery, not bump's job."
        );
    }

    if git::tag_exists(dir, &new_tag)? {
        let local_sha = git::tag_sha(dir, &new_tag)?;
        if local_sha == head {
            println!("{new_tag} is already tagged at HEAD locally. Run: git push origin {new_tag}");
            return Ok(());
        }
        bail!(
            "Tag {new_tag} exists locally at {local_sha}, not HEAD ({head}).\n\
             Resolving that is manual tag surgery, not bump's job."
        );
    }

    // 6. Create the annotated tag on the merged commit.
    let message = format!("Release {new_tag}");
    git::create_tag(dir, &new_tag, &message)?;
    info!("tag_only: created tag {new_tag} at {head}");
    let short: String = head.chars().take(12).collect();
    println!("Tagged {new_tag} on merged {default} ({short})");

    // 7. Push hint (by explicit name; never --tags).
    println!("Run: git push origin {new_tag}");
    Ok(())
}

/// Process a single directory
fn process_directory(dir: &Path, cli: &Cli, bump_type: BumpType) -> Result<()> {
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.display().to_string());

    // 1. Validate - is this a git repo?
    if !git::is_git_repo(dir) {
        bail!("Not a git repository: {}", dir.display());
    }

    // --gates is informational: report the verdict and recommended flow, then exit 0.
    if cli.gates {
        return report_gates(dir);
    }

    // --tag-only is its own flow: tag the already-merged commit after a hard
    // verification ladder. No version change, no commit, no gate refusal.
    if cli.tag_only {
        return tag_only(dir);
    }

    // --no-tag bumps the version and commits but creates no tag, so the orphan risk
    // (and thus the gate probe) does not apply.
    let create_tag = !cli.no_tag;

    // 1b. Probe the remote default-branch gate and apply policy BEFORE any mutation:
    //     refuse on a gated branch (tagging would orphan the tag), warn-and-proceed
    //     when the probe is inconclusive, skip under --no-tag (nothing to refuse) or
    //     --no-verify.
    if !create_tag {
        info!("Skipping gate probe for {} (--no-tag: no tag created)", dir.display());
    } else if cli.no_verify {
        info!("Skipping gate probe for {} (--no-verify)", dir.display());
    } else {
        match github::detect(dir) {
            github::Gate::Ungated => {
                info!("Gate status for {}: ungated", dir.display());
            }
            github::Gate::Gated(rules) => {
                info!("Gate status for {}: gated by {:?}", dir.display(), rules);
                bail!("{}", gated_refusal_message(&github::repo_label(dir), &rules));
            }
            github::Gate::Unknown(reason) => {
                warn!("Gate probe inconclusive for {}: {}", dir.display(), reason);
                eprintln!(
                    "bump: WARNING: could not verify branch-protection gates ({reason}); \
                     proceeding as ungated.\n\
                     If this repo is PR-gated, run `bump --gates` once online before tagging."
                );
            }
        }
    }

    // 2. Detect project type
    let project_type = detect_project_type(dir);
    info!("Detected project type: {}", project_type);

    // 3. Project-specific validation
    validate_project(dir, project_type)?;

    // 4. Read file version and determine version action
    let file_version = read_file_version(dir, project_type)?.and_then(|v| version::parse_version(&v).ok());
    let action = determine_version_action(dir, file_version, project_type, bump_type)?;
    let new_tag = version::format_tag(&action.target_version);
    let new_file_version = version::format_file_version(&action.target_version);

    // 5. Display what we're doing
    if action.is_initial_tag {
        if create_tag {
            println!("tag: {}", new_tag);
        } else {
            println!("version: {}", new_file_version);
        }
    } else {
        // For bumps, show the transition
        let current_version = read_file_version(dir, project_type)?
            .and_then(|v| version::parse_version(&v).ok())
            .map(|v| version::format_file_version(&v))
            .unwrap_or_else(|| "unknown".to_string());
        println!("bump: {} -> {}", current_version, new_file_version);
    }

    // 6. Verify new tag doesn't exist
    if git::tag_exists(dir, &new_tag)? {
        bail!("Tag {} already exists", new_tag);
    }

    // 7. Check for uncommitted changes to determine workflow
    let has_changes = git::has_uncommitted_changes(dir)?;

    // 8. Handle dry-run
    if cli.dry_run {
        if action.needs_file_update {
            println!("[dry-run] Would update: {}", version_file_name(project_type));
        }
        if !create_tag {
            println!("[dry-run] Would commit version bump to {} (no tag)", new_file_version);
        } else if !has_changes && !git::head_has_tag(dir)? {
            let is_pushed = git::is_head_pushed(dir)?;
            if is_pushed {
                println!("[dry-run] Would create new commit and tag: {}", new_tag);
            } else {
                println!("[dry-run] Would amend previous commit and tag: {}", new_tag);
            }
        } else {
            println!("[dry-run] Would commit and tag: {}", new_tag);
        }
        return Ok(());
    }

    // Workflow branches based on whether there are uncommitted changes
    if has_changes {
        // ===== STANDARD WORKFLOW: Uncommitted changes exist =====

        // Update version file if needed
        if action.needs_file_update {
            write_file_version(dir, project_type, &new_file_version)?;
            info!(
                "Updated {} to version {}",
                version_file_name(project_type),
                new_file_version
            );
            sync_lockfile(dir, project_type)?;
        }

        // Stage all changes
        git::stage_all(dir)?;

        // Determine commit message
        let staged_files = git::get_staged_files(dir)?;
        let commit_message =
            determine_commit_message(cli, &new_tag, &staged_files, action.is_initial_tag, project_type)?;

        // Commit
        if !staged_files.is_empty() {
            git::commit(dir, &commit_message)?;
            info!("Committed with message: {}", commit_message);
        }

        // Create annotated tag (unless --no-tag)
        if create_tag {
            git::create_tag(dir, &new_tag, &commit_message)?;
            info!("Created tag: {}", new_tag);
            println!("Committed and tagged {}", new_tag);
        } else {
            println!("Committed version bump to {} (no tag)", new_file_version);
        }
    } else {
        // ===== CLEAN TREE WORKFLOW: No uncommitted changes =====

        // Check if HEAD already has a tag
        if git::head_has_tag(dir)? && !cli.force {
            bail!("HEAD already has a tag. Make changes first, then run bump. Use --force to override.");
        }

        // Check if HEAD has been pushed
        let is_pushed = git::is_head_pushed(dir)?;

        // Update version file
        if action.needs_file_update {
            write_file_version(dir, project_type, &new_file_version)?;
            info!(
                "Updated {} to version {}",
                version_file_name(project_type),
                new_file_version
            );
            sync_lockfile(dir, project_type)?;
        }

        // Stage changes
        git::stage_all(dir)?;
        let staged_files = git::get_staged_files(dir)?;

        if is_pushed {
            // HEAD is pushed - create a new commit
            let commit_message =
                determine_commit_message(cli, &new_tag, &staged_files, action.is_initial_tag, project_type)?;

            if !staged_files.is_empty() {
                git::commit(dir, &commit_message)?;
                info!("Committed with message: {}", commit_message);
            }

            if create_tag {
                git::create_tag(dir, &new_tag, &commit_message)?;
                info!("Created tag: {}", new_tag);
                println!("Committed and tagged {}", new_tag);
            } else {
                println!("Committed version bump to {} (no tag)", new_file_version);
            }
        } else {
            // HEAD is not pushed - amend the previous commit
            if !staged_files.is_empty() {
                git::amend_commit_no_edit(dir)?;
                info!("Amended previous commit with version file changes");
            }

            if create_tag {
                // Use automatic message for the tag since we're amending
                let tag_message = format!("Bump version to {}", new_tag);
                git::create_tag(dir, &new_tag, &tag_message)?;
                info!("Created tag: {}", new_tag);
                println!("Amended commit and tagged {}", new_tag);
            } else {
                println!("Amended commit with version bump to {} (no tag)", new_file_version);
            }
        }
    }

    if create_tag {
        // Push the branch first, then the tag BY EXPLICIT NAME (never `git push --tags`,
        // which can land the tag even when the branch push is rejected).
        let branch = git::current_branch(dir).unwrap_or_else(|_| "<branch>".to_string());
        println!("Run: git push origin {branch} && git push origin {new_tag}");
    } else {
        println!("Run: git push <branch>  (open a PR; after merge, tag with: bump --tag-only)");
    }

    if !dir_name.is_empty() && dir != env::current_dir().unwrap_or_default() {
        println!("[{}] Done", dir_name);
    }

    Ok(())
}

fn main() -> Result<()> {
    setup_logging().context("Failed to setup logging")?;

    let cli = Cli::parse();
    let bump_type = BumpType::from_cli(cli.major, cli.minor);

    info!("Starting bump with type: {:?}", bump_type);

    // Determine directories to process
    let directories: Vec<PathBuf> = if cli.directories.is_empty() {
        vec![env::current_dir().context("Failed to get current directory")?]
    } else {
        cli.directories.clone()
    };

    let mut successes = 0;
    let mut failures = 0;

    for dir in &directories {
        let dir = if dir.is_absolute() { dir.clone() } else { env::current_dir()?.join(dir) };

        if directories.len() > 1 {
            let dir_name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            println!("\n[{}]", dir_name);
        }

        match process_directory(&dir, &cli, bump_type) {
            Ok(()) => successes += 1,
            Err(e) => {
                eprintln!("Error: {:#}", e);
                failures += 1;
            }
        }
    }

    if directories.len() > 1 {
        println!();
        if failures == 0 {
            println!("All done! Don't forget to push your changes.");
        } else {
            println!("Completed: {} succeeded, {} failed", successes, failures);
        }
    }

    // Exit non-zero if ANY directory failed, so a gated refusal (or any error) in a
    // batch fails loudly instead of being masked by a sibling success.
    if failures > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// =============================================================================
/// TEST MODULE FOR BUMP VERSION LOGIC
/// =============================================================================
///
/// THE RULES (EXACTLY AS SPECIFIED):
///
/// 1. `0.1.0` is the SPECIAL "UNTOUCHED DEFAULT" version (Rust only).
///    - If Cargo.toml = 0.1.0 and git tags exist -> DEFER TO GIT TAG
///    - If Cargo.toml = 0.1.0 and no git tags -> Create initial tag v0.1.0
///
/// 2. ANY OTHER VERSION in a version file means "ACTIVELY MANAGED"
///    - If version file != 0.1.0 and latest tag MATCHES -> Bump from that version
///    - If version file != 0.1.0 and latest tag DOES NOT MATCH -> **ERROR**
///    - If version file != 0.1.0 and no tags exist -> Create initial tag
///
/// 3. If the version file has NO version field (or Generic project):
///    - If git tags exist -> Bump from latest tag
///    - If no git tags -> Start at 0.1.0
///
/// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    // =========================================================================
    // TEST HELPERS
    // =========================================================================

    fn setup_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("Failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git email");

        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .expect("Failed to set git name");
    }

    fn create_initial_commit(dir: &Path) {
        fs::write(dir.join("README.md"), "# Test").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .expect("Failed to add files");

        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(dir)
            .output()
            .expect("Failed to commit");
    }

    fn create_git_tag(dir: &Path, tag: &str) {
        Command::new("git")
            .args(["tag", "-a", tag, "-m", tag])
            .current_dir(dir)
            .output()
            .expect("Failed to create tag");
    }

    fn create_cargo_toml(dir: &Path, version: Option<&str>) {
        let content = match version {
            Some(v) => format!(
                r#"[package]
name = "test-pkg"
version = "{}"
"#,
                v
            ),
            None => r#"[package]
name = "test-pkg"
"#
            .to_string(),
        };
        fs::write(dir.join("Cargo.toml"), content).unwrap();
    }

    fn create_pyproject_toml(dir: &Path, version: Option<&str>) {
        let content = match version {
            Some(v) => format!(
                r#"[project]
name = "test-pkg"
version = "{}"
"#,
                v
            ),
            None => r#"[project]
name = "test-pkg"
"#
            .to_string(),
        };
        fs::write(dir.join("pyproject.toml"), content).unwrap();
    }

    // =========================================================================
    // PROJECT TYPE DETECTION
    // =========================================================================

    #[test]
    fn detect_rust_project() {
        let tmp = TempDir::new().unwrap();
        create_cargo_toml(tmp.path(), Some("1.0.0"));
        assert_eq!(detect_project_type(tmp.path()), ProjectType::Rust);
    }

    #[test]
    fn detect_python_project() {
        let tmp = TempDir::new().unwrap();
        create_pyproject_toml(tmp.path(), Some("1.0.0"));
        assert_eq!(detect_project_type(tmp.path()), ProjectType::Python);
    }

    #[test]
    fn detect_generic_project() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(detect_project_type(tmp.path()), ProjectType::Generic);
    }

    #[test]
    fn detect_rust_over_python_when_both_present() {
        let tmp = TempDir::new().unwrap();
        create_cargo_toml(tmp.path(), Some("1.0.0"));
        create_pyproject_toml(tmp.path(), Some("1.0.0"));
        assert_eq!(detect_project_type(tmp.path()), ProjectType::Rust);
    }

    // =========================================================================
    // RULE 1: Cargo.toml = 0.1.0 (UNTOUCHED DEFAULT) - Rust only
    // =========================================================================

    /// RULE 1a: Cargo.toml=0.1.0, NO git tags
    /// -> Create initial tag v0.1.0, do NOT update Cargo.toml
    #[test]
    fn rule_1a_cargo_at_default_no_tags_creates_initial_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        // NO TAGS

        let file_version = Some(Version::new(0, 1, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(0, 1, 0), "MUST create tag v0.1.0");
        assert!(
            !action.needs_file_update,
            "MUST NOT update Cargo.toml - already at 0.1.0"
        );
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    /// RULE 1b: Cargo.toml=0.1.0, tag v0.1.0 exists
    /// -> Bump to v0.1.1, update Cargo.toml
    #[test]
    fn rule_1b_cargo_at_default_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.0"); // TAG MATCHES DEFAULT

        let file_version = Some(Version::new(0, 1, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 1, 1),
            "MUST bump from v0.1.0 to v0.1.1"
        );
        assert!(action.needs_file_update, "MUST update Cargo.toml to 0.1.1");
        assert!(!action.is_initial_tag, "MUST NOT be initial tag - this is a bump");
    }

    /// RULE 1c: Cargo.toml=0.1.0 (untouched), tag v0.1.28 exists (higher)
    /// -> DEFER TO GIT TAG: Bump from v0.1.28 to v0.1.29, update Cargo.toml
    #[test]
    fn rule_1c_cargo_at_default_tag_higher_defers_to_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // TAG IS HIGHER

        let file_version = Some(Version::new(0, 1, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 1, 29),
            "MUST bump from git tag v0.1.28 to v0.1.29 (Cargo.toml=0.1.0 is untouched default)"
        );
        assert!(action.needs_file_update, "MUST update Cargo.toml from 0.1.0 to 0.1.29");
        assert!(!action.is_initial_tag, "MUST NOT be initial tag - this is a bump");
    }

    /// RULE 1d: Same as 1c but with minor bump
    /// -> Bump from v0.1.28 to v0.2.0
    #[test]
    fn rule_1d_cargo_at_default_tag_higher_minor_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // TAG IS HIGHER

        let file_version = Some(Version::new(0, 1, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Minor).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST minor bump from git tag v0.1.28 to v0.2.0"
        );
        assert!(action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    // =========================================================================
    // RULE 2: Version file != 0.1.0 (ACTIVELY MANAGED)
    // =========================================================================

    /// RULE 2a: Cargo.toml=0.2.0 (managed), tag v0.1.28 (MISMATCH)
    /// -> **ERROR**: Version mismatch
    #[test]
    fn rule_2a_cargo_managed_tag_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED (not 0.1.0)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // DOES NOT MATCH

        let file_version = Some(Version::new(0, 2, 0));
        let result = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch);

        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=0.2.0 does not match latest tag v0.1.28"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("mismatch") || err.contains("Mismatch"),
            "Error MUST mention version mismatch. Got: {}",
            err
        );
    }

    /// RULE 2b: Cargo.toml=0.1.5 (managed), tag v0.1.28 (MISMATCH - tag higher)
    /// -> **ERROR**: Version mismatch
    #[test]
    fn rule_2b_cargo_managed_lower_than_tag_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED (not 0.1.0)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // DOES NOT MATCH (higher)

        let file_version = Some(Version::new(0, 1, 5));
        let result = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch);

        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=0.1.5 does not match latest tag v0.1.28"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("mismatch") || err.contains("Mismatch"),
            "Error MUST mention version mismatch. Got: {}",
            err
        );
    }

    /// RULE 2c: Cargo.toml=0.2.0 (managed), tag v0.2.0 (MATCHES)
    /// -> Bump to v0.2.1, update Cargo.toml
    #[test]
    fn rule_2c_cargo_managed_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.2.0"); // MATCHES

        let file_version = Some(Version::new(0, 2, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 2, 1),
            "MUST bump from v0.2.0 to v0.2.1"
        );
        assert!(action.needs_file_update, "MUST update Cargo.toml");
        assert!(!action.is_initial_tag);
    }

    /// RULE 2d: Cargo.toml=0.1.5 (managed), tag v0.1.5 (MATCHES)
    /// -> Bump to v0.1.6, update Cargo.toml
    #[test]
    fn rule_2d_cargo_managed_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED (not 0.1.0!)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let file_version = Some(Version::new(0, 1, 5));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 1, 6),
            "MUST bump from v0.1.5 to v0.1.6"
        );
        assert!(action.needs_file_update, "MUST update Cargo.toml");
        assert!(!action.is_initial_tag);
    }

    /// RULE 2e: Cargo.toml=0.2.0 (managed), NO tags
    /// -> Create initial tag v0.2.0, do NOT update Cargo.toml
    #[test]
    fn rule_2e_cargo_managed_no_tags_creates_initial_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        // NO TAGS

        let file_version = Some(Version::new(0, 2, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST create initial tag v0.2.0"
        );
        assert!(
            !action.needs_file_update,
            "MUST NOT update Cargo.toml - already at 0.2.0"
        );
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    /// RULE 2f: Cargo.toml=0.1.5 (managed), tag v0.1.5, minor bump
    /// -> Bump to v0.2.0
    #[test]
    fn rule_2f_cargo_managed_minor_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let file_version = Some(Version::new(0, 1, 5));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Minor).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST minor bump from v0.1.5 to v0.2.0"
        );
        assert!(action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    /// RULE 2g: Cargo.toml=0.1.5 (managed), tag v0.1.5, major bump
    /// -> Bump to v1.0.0
    #[test]
    fn rule_2g_cargo_managed_major_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let file_version = Some(Version::new(0, 1, 5));
        let action = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Major).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(1, 0, 0),
            "MUST major bump from v0.1.5 to v1.0.0"
        );
        assert!(action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    // =========================================================================
    // RULE 3: No version in file (or Generic project)
    // =========================================================================

    /// RULE 3a: NO version in Cargo.toml, tag v0.1.5 exists
    /// -> Bump from tag to v0.1.6, update Cargo.toml
    #[test]
    fn rule_3a_no_cargo_version_tag_exists_bumps_from_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, None); // NO VERSION FIELD
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5");

        let action = determine_version_action(dir, None, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(
            action.target_version,
            Version::new(0, 1, 6),
            "MUST bump from git tag v0.1.5 to v0.1.6"
        );
        assert!(action.needs_file_update, "MUST update Cargo.toml to 0.1.6");
        assert!(!action.is_initial_tag);
    }

    /// RULE 3b: NO version in Cargo.toml, NO tags
    /// -> Start at v0.1.0, update Cargo.toml, create initial tag
    #[test]
    fn rule_3b_no_cargo_version_no_tags_starts_at_0_1_0() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, None); // NO VERSION FIELD
        create_initial_commit(dir);
        // NO TAGS

        let action = determine_version_action(dir, None, ProjectType::Rust, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(0, 1, 0), "MUST start at v0.1.0");
        assert!(action.needs_file_update, "MUST update Cargo.toml to 0.1.0");
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    // =========================================================================
    // EDGE CASES: Version mismatch
    // =========================================================================

    /// EDGE CASE: Cargo.toml=0.3.0, tag v0.1.28 exists
    /// -> **ERROR**: Mismatch (Cargo.toml is managed, doesn't match tag)
    #[test]
    fn edge_cargo_higher_than_tag_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.3.0")); // MANAGED, HIGHER THAN TAG
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // LOWER, DOES NOT MATCH

        let file_version = Some(Version::new(0, 3, 0));
        let result = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch);

        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=0.3.0 does not match latest tag v0.1.28"
        );
    }

    /// EDGE CASE: Cargo.toml=1.0.0, tag v0.9.0 exists
    /// -> **ERROR**: Mismatch
    #[test]
    fn edge_cargo_1_0_0_tag_0_9_0_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("1.0.0")); // MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.9.0"); // DOES NOT MATCH

        let file_version = Some(Version::new(1, 0, 0));
        let result = determine_version_action(dir, file_version, ProjectType::Rust, BumpType::Patch);

        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=1.0.0 does not match latest tag v0.9.0"
        );
    }

    // =========================================================================
    // PYTHON PROJECT TESTS
    // =========================================================================

    /// Python: pyproject.toml=1.0.0, tag v1.0.0 -> bump to v1.0.1
    #[test]
    fn python_version_matches_tag_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_pyproject_toml(dir, Some("1.0.0"));
        create_initial_commit(dir);
        create_git_tag(dir, "v1.0.0");

        let file_version = Some(Version::new(1, 0, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Python, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(1, 0, 1));
        assert!(action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    /// Python: pyproject.toml=1.0.0, no tags -> initial tag v1.0.0
    #[test]
    fn python_version_no_tags_creates_initial_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_pyproject_toml(dir, Some("1.0.0"));
        create_initial_commit(dir);

        let file_version = Some(Version::new(1, 0, 0));
        let action = determine_version_action(dir, file_version, ProjectType::Python, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(1, 0, 0));
        assert!(!action.needs_file_update);
        assert!(action.is_initial_tag);
    }

    /// Python: 0.1.0 is NOT special for Python (no untouched default concept)
    #[test]
    fn python_0_1_0_is_not_special() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_pyproject_toml(dir, Some("0.1.0"));
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // Higher tag

        let file_version = Some(Version::new(0, 1, 0));
        let result = determine_version_action(dir, file_version, ProjectType::Python, BumpType::Patch);

        // For Python, 0.1.0 vs v0.1.28 is a mismatch error (not deferred to tag)
        assert!(result.is_err(), "Python should error on version mismatch, not defer");
    }

    /// Python: version mismatch is an error
    #[test]
    fn python_version_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_pyproject_toml(dir, Some("2.0.0"));
        create_initial_commit(dir);
        create_git_tag(dir, "v1.0.0");

        let file_version = Some(Version::new(2, 0, 0));
        let result = determine_version_action(dir, file_version, ProjectType::Python, BumpType::Patch);

        assert!(result.is_err());
    }

    // =========================================================================
    // GENERIC PROJECT TESTS (git-tag-only)
    // =========================================================================

    /// Generic: tag v0.5.0 exists -> bump to v0.5.1
    #[test]
    fn generic_tag_exists_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_initial_commit(dir);
        create_git_tag(dir, "v0.5.0");

        let action = determine_version_action(dir, None, ProjectType::Generic, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(0, 5, 1));
        assert!(!action.needs_file_update, "Generic projects have no file to update");
        assert!(!action.is_initial_tag);
    }

    /// Generic: no tags -> start at v0.1.0
    #[test]
    fn generic_no_tags_starts_at_0_1_0() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_initial_commit(dir);

        let action = determine_version_action(dir, None, ProjectType::Generic, BumpType::Patch).unwrap();

        assert_eq!(action.target_version, Version::new(0, 1, 0));
        assert!(!action.needs_file_update, "Generic projects have no file to update");
        assert!(action.is_initial_tag);
    }

    /// Generic: tag v1.0.0, minor bump -> v1.1.0
    #[test]
    fn generic_minor_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_initial_commit(dir);
        create_git_tag(dir, "v1.0.0");

        let action = determine_version_action(dir, None, ProjectType::Generic, BumpType::Minor).unwrap();

        assert_eq!(action.target_version, Version::new(1, 1, 0));
        assert!(!action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    /// Generic: tag v1.0.0, major bump -> v2.0.0
    #[test]
    fn generic_major_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_initial_commit(dir);
        create_git_tag(dir, "v1.0.0");

        let action = determine_version_action(dir, None, ProjectType::Generic, BumpType::Major).unwrap();

        assert_eq!(action.target_version, Version::new(2, 0, 0));
        assert!(!action.needs_file_update);
        assert!(!action.is_initial_tag);
    }

    // =========================================================================
    // GATE POLICY (Phase 2): gated refusal / unknown-proceed / --no-verify
    //
    // These exercise process_directory with the BUMP_GATES_PROBE env seam, so no
    // network is touched. Env mutation is serialized behind ENV_LOCK.
    // =========================================================================

    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Set BUMP_GATES_PROBE, returning the prior value for restoration.
    fn set_probe(val: &str) -> Option<String> {
        let prev = env::var("BUMP_GATES_PROBE").ok();
        unsafe { env::set_var("BUMP_GATES_PROBE", val) };
        prev
    }

    fn restore_probe(prev: Option<String>) {
        match prev {
            Some(v) => unsafe { env::set_var("BUMP_GATES_PROBE", v) },
            None => unsafe { env::remove_var("BUMP_GATES_PROBE") },
        }
    }

    /// Minimal Rust repo at 0.1.0 with one commit and no tags. Without gating,
    /// process_directory would create the initial tag v0.1.0.
    fn setup_taggable_repo(dir: &Path) {
        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0"));
        create_initial_commit(dir);
    }

    /// A gated repo must refuse and make NO mutation (no tag created).
    #[test]
    fn gated_refusal_aborts_before_mutation() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_taggable_repo(dir);

        let cli = Cli::try_parse_from(["bump"]).unwrap();
        let prev = set_probe("gated:pull_request,workflows");
        let result = process_directory(dir, &cli, BumpType::Patch);
        restore_probe(prev);

        assert!(result.is_err(), "gated repo must refuse to tag");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("gated"), "error must name the gate, got: {err}");
        assert!(
            err.contains("--tag-only"),
            "error must show the gated recipe, got: {err}"
        );
        assert!(
            !git::tag_exists(dir, "v0.1.0").unwrap(),
            "no tag may be created when refusing"
        );
    }

    /// An inconclusive probe (Unknown) proceeds and tags.
    #[test]
    fn unknown_gate_proceeds_and_tags() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_taggable_repo(dir);

        let cli = Cli::try_parse_from(["bump"]).unwrap();
        let prev = set_probe("unknown:offline");
        let result = process_directory(dir, &cli, BumpType::Patch);
        restore_probe(prev);

        assert!(result.is_ok(), "unknown gate must proceed: {result:?}");
        assert!(
            git::tag_exists(dir, "v0.1.0").unwrap(),
            "tag must be created when proceeding"
        );
    }

    /// --no-verify bypasses the probe even when the repo would be gated.
    #[test]
    fn no_verify_skips_gate_probe() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_taggable_repo(dir);

        let cli = Cli::try_parse_from(["bump", "--no-verify"]).unwrap();
        let prev = set_probe("gated:pull_request");
        let result = process_directory(dir, &cli, BumpType::Patch);
        restore_probe(prev);

        assert!(result.is_ok(), "--no-verify must bypass gating: {result:?}");
        assert!(
            git::tag_exists(dir, "v0.1.0").unwrap(),
            "tag must be created under --no-verify"
        );
    }

    // =========================================================================
    // --no-tag (Phase 3): bump + commit, but never tag
    // =========================================================================

    /// --no-tag bumps the version file and commits, but creates NO tag.
    #[test]
    fn no_tag_bumps_file_without_tagging() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5"));
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5");
        // Pending work on the branch (the realistic --no-tag case), so this takes the
        // standard (uncommitted-changes) workflow rather than the clean-tree path. Use
        // -a so the non-version-only staged set doesn't open an editor for the message.
        fs::write(dir.join("feature.txt"), "work").unwrap();

        let cli = Cli::try_parse_from(["bump", "--no-tag", "-a"]).unwrap();
        process_directory(dir, &cli, BumpType::Patch).unwrap();

        assert!(!git::tag_exists(dir, "v0.1.6").unwrap(), "no new tag under --no-tag");
        assert!(git::tag_exists(dir, "v0.1.5").unwrap(), "prior tag untouched");
        let version = read_file_version(dir, ProjectType::Rust).unwrap().unwrap();
        assert_eq!(version, "0.1.6", "version file must still be bumped");
    }

    /// --no-tag on an initial-tag scenario commits the version but creates no tag.
    #[test]
    fn no_tag_initial_creates_no_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0"));
        create_initial_commit(dir);

        let cli = Cli::try_parse_from(["bump", "--no-tag"]).unwrap();
        process_directory(dir, &cli, BumpType::Patch).unwrap();

        assert!(
            !git::tag_exists(dir, "v0.2.0").unwrap(),
            "no tag created under --no-tag"
        );
    }

    /// --no-tag does not probe gates, so it never refuses even on a gated repo.
    #[test]
    fn no_tag_skips_gate_probe_even_when_gated() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_taggable_repo(dir);

        let cli = Cli::try_parse_from(["bump", "--no-tag"]).unwrap();
        let prev = set_probe("gated:pull_request");
        let result = process_directory(dir, &cli, BumpType::Patch);
        restore_probe(prev);

        assert!(result.is_ok(), "--no-tag must not refuse on a gated repo: {result:?}");
        assert!(!git::tag_exists(dir, "v0.1.0").unwrap(), "still no tag under --no-tag");
    }

    // =========================================================================
    // --tag-only (Phase 4): tag the merged commit after a verification ladder.
    //
    // These use a real bare "origin" so fetch / ls-remote / origin/HEAD behave.
    // =========================================================================

    /// Run a git command in `dir`, returning trimmed stdout (panics on failure).
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A bare `origin` plus a working clone on `main` at 0.1.0, HEAD == origin/main.
    /// Returns both TempDirs (keep them alive for the test's duration).
    fn setup_remote_repo() -> (TempDir, TempDir) {
        let origin = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin.path())
            .output()
            .unwrap();

        let work = TempDir::new().unwrap();
        setup_git_repo(work.path());
        create_cargo_toml(work.path(), Some("0.1.0"));
        create_initial_commit(work.path());
        git_in(work.path(), &["branch", "-M", "main"]);
        git_in(
            work.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git_in(work.path(), &["push", "-u", "origin", "main"]);
        git_in(work.path(), &["remote", "set-head", "origin", "main"]);
        (origin, work)
    }

    /// Happy path: HEAD == origin/main, no existing tag -> create vX.Y.Z.
    #[test]
    fn tag_only_creates_tag_on_merged_head() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();

        tag_only(dir).unwrap();

        assert!(
            git::tag_exists(dir, "v0.1.0").unwrap(),
            "tag must be created on merged HEAD"
        );
        assert_eq!(git::tag_sha(dir, "v0.1.0").unwrap(), git::head_sha(dir).unwrap());
    }

    /// Step 1: dirty working tree refuses, no tag.
    #[test]
    fn tag_only_refuses_dirty_tree() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        fs::write(dir.join("dirty.txt"), "x").unwrap();

        let err = tag_only(dir).unwrap_err().to_string();
        assert!(err.contains("clean working tree"), "got: {err}");
        assert!(!git::tag_exists(dir, "v0.1.0").unwrap());
    }

    /// Step 2: not on the default branch refuses, no tag.
    #[test]
    fn tag_only_refuses_wrong_branch() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        git_in(dir, &["checkout", "-b", "feature"]);

        let err = tag_only(dir).unwrap_err().to_string();
        assert!(err.contains("default branch"), "got: {err}");
        assert!(!git::tag_exists(dir, "v0.1.0").unwrap());
    }

    /// Step 3: HEAD ahead of origin/main refuses with a distinct message.
    #[test]
    fn tag_only_refuses_when_ahead() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        git_in(dir, &["commit", "--allow-empty", "-m", "unmerged work"]);

        let err = tag_only(dir).unwrap_err().to_string();
        assert!(err.contains("ahead"), "got: {err}");
        assert!(!git::tag_exists(dir, "v0.1.0").unwrap());
    }

    /// Step 3: HEAD behind origin/main refuses with a distinct message.
    #[test]
    fn tag_only_refuses_when_behind() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        let c1 = git::head_sha(dir).unwrap();
        git_in(dir, &["commit", "--allow-empty", "-m", "c2"]);
        git_in(dir, &["push", "origin", "main"]);
        git_in(dir, &["reset", "--hard", &c1]);

        let err = tag_only(dir).unwrap_err().to_string();
        assert!(err.contains("behind"), "got: {err}");
        assert!(!git::tag_exists(dir, "v0.1.0").unwrap());
    }

    /// Step 5: a local tag already at HEAD is idempotent success.
    #[test]
    fn tag_only_idempotent_local_tag_at_head() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        git_in(dir, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);

        let result = tag_only(dir);
        assert!(result.is_ok(), "local tag at HEAD must be idempotent: {result:?}");
    }

    /// Step 5: a remote tag at a different commit is a refusal, no local tag.
    #[test]
    fn tag_only_refuses_remote_tag_conflict() {
        let (_origin, work) = setup_remote_repo();
        let dir = work.path();
        let c1 = git::head_sha(dir).unwrap();
        // Tag v0.1.0 at a different commit and push only the tag, then return to c1.
        git_in(dir, &["commit", "--allow-empty", "-m", "other"]);
        git_in(dir, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
        git_in(dir, &["push", "origin", "v0.1.0"]);
        git_in(dir, &["reset", "--hard", &c1]);
        // Drop the local tag so only the remote one remains in conflict.
        git_in(dir, &["tag", "-d", "v0.1.0"]);

        let err = tag_only(dir).unwrap_err().to_string();
        assert!(err.contains("already exists on origin"), "got: {err}");
        assert!(
            !git::tag_exists(dir, "v0.1.0").unwrap(),
            "no local tag created on conflict"
        );
    }

    /// Step 4: a generic (no-manifest-version) repo cannot --tag-only.
    #[test]
    fn tag_only_generic_no_version_errors() {
        let origin = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(origin.path())
            .output()
            .unwrap();
        let work = TempDir::new().unwrap();
        setup_git_repo(work.path());
        create_initial_commit(work.path()); // no Cargo.toml -> Generic
        git_in(work.path(), &["branch", "-M", "main"]);
        git_in(
            work.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git_in(work.path(), &["push", "-u", "origin", "main"]);
        git_in(work.path(), &["remote", "set-head", "origin", "main"]);

        let err = tag_only(work.path()).unwrap_err().to_string();
        assert!(err.contains("needs a version"), "got: {err}");
    }

    // =========================================================================
    // --gates report (Phase 5)
    // =========================================================================

    /// report_gates renders (and succeeds) for every verdict.
    #[test]
    fn report_gates_runs_for_each_verdict() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0"));
        create_initial_commit(dir);

        for probe in ["ungated", "gated:pull_request,workflows", "unknown:offline"] {
            let prev = set_probe(probe);
            let result = report_gates(dir);
            restore_probe(prev);
            assert!(result.is_ok(), "report_gates must succeed for {probe}: {result:?}");
        }
    }
}
