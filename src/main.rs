use clap::Parser;
use eyre::{Context, Result, bail};
use log::info;
use semver::Version;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

mod cargo;
mod cli;
mod git;
mod version;

use cli::Cli;
use version::BumpType;

fn setup_logging() -> Result<()> {
    let log_dir = dirs::data_local_dir()
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
    /// Whether we need to update Cargo.toml
    needs_cargo_update: bool,
    /// Whether this is an initial tag (no bump) vs a version bump
    is_initial_tag: bool,
}

/// The default "untouched" version in Cargo.toml
const DEFAULT_UNTOUCHED_VERSION: Version = Version::new(0, 1, 0);

/// Determine what version action to take
fn determine_version_action(dir: &Path, cargo_path: &Path, bump_type: BumpType) -> Result<VersionAction> {
    // Get version from Cargo.toml (if it exists)
    let cargo_version = cargo::read_version(cargo_path)?.and_then(|v| version::parse_version(&v).ok());

    // Get latest git tag (if any exist)
    let latest_tag_version = git::get_latest_tag(dir)?.and_then(|t| version::parse_version(&t).ok());

    // Determine the base version to bump from
    match (&cargo_version, &latest_tag_version) {
        // Case: Both Cargo.toml and git tags exist
        (Some(cargo), Some(tag)) => {
            if *cargo == DEFAULT_UNTOUCHED_VERSION {
                // Cargo.toml is at default 0.1.0 (untouched) - defer to git tag
                info!(
                    "Cargo.toml is at default 0.1.0, using git tag {} as base.",
                    version::format_tag(tag)
                );
                let bumped = version::bump_version(tag, bump_type);
                Ok(VersionAction {
                    target_version: bumped,
                    needs_cargo_update: true,
                    is_initial_tag: false,
                })
            } else if cargo == tag {
                // Cargo.toml matches latest tag - bump from it
                info!("Cargo.toml matches latest tag {}. Bumping.", version::format_tag(cargo));
                let bumped = version::bump_version(cargo, bump_type);
                Ok(VersionAction {
                    target_version: bumped,
                    needs_cargo_update: true,
                    is_initial_tag: false,
                })
            } else {
                // Cargo.toml is NOT 0.1.0 and doesn't match latest tag - ERROR
                bail!(
                    "Version mismatch: Cargo.toml has {} but latest git tag is {}. \
                    Please sync them manually before running bump.",
                    version::format_cargo_version(cargo),
                    version::format_tag(tag)
                );
            }
        }

        // Case: Cargo.toml exists, no git tags
        (Some(cargo), None) => {
            let cargo_tag = version::format_tag(cargo);
            // No tags exist - create initial tag for Cargo.toml version
            info!("No git tags found. Creating initial tag {} from Cargo.toml.", cargo_tag);
            Ok(VersionAction {
                target_version: cargo.clone(),
                needs_cargo_update: false,
                is_initial_tag: true,
            })
        }

        // Case: No Cargo.toml version, but git tags exist
        (None, Some(tag)) => {
            info!(
                "No version in Cargo.toml. Using git tag {} as base.",
                version::format_tag(tag)
            );
            let bumped = version::bump_version(tag, bump_type);
            Ok(VersionAction {
                target_version: bumped,
                needs_cargo_update: true,
                is_initial_tag: false,
            })
        }

        // Case: No version anywhere
        (None, None) => {
            info!("No version found anywhere. Starting at 0.1.0");
            Ok(VersionAction {
                target_version: Version::new(0, 1, 0),
                needs_cargo_update: true,
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

    let only_cargo_files = staged_files.iter().all(|f| f == "Cargo.toml" || f == "Cargo.lock");
    if only_cargo_files {
        if is_initial_tag {
            return Ok(format!("Release {}", new_tag));
        } else {
            return Ok(format!("Bump version to {}", new_tag));
        }
    }

    // Priority 4: Open editor for complex changes
    prompt_commit_message_with_editor(staged_files)
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

    // 2. Validate - does Cargo.toml exist?
    if !cargo::cargo_toml_exists(dir) {
        bail!("No Cargo.toml found in: {}", dir.display());
    }

    // 3. Validate - check for workspace members with independent versions
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

    let cargo_path = cargo::cargo_toml_path(dir);

    // 3. Determine version action
    let action = determine_version_action(dir, &cargo_path, bump_type)?;
    let new_tag = version::format_tag(&action.target_version);
    let new_cargo_version = version::format_cargo_version(&action.target_version);

    // 4. Display what we're doing
    if action.is_initial_tag {
        println!("tag: {}", new_tag);
    } else {
        // For bumps, show the transition
        let current_version = cargo::read_version(&cargo_path)?
            .and_then(|v| version::parse_version(&v).ok())
            .map(|v| version::format_cargo_version(&v))
            .unwrap_or_else(|| "unknown".to_string());
        println!("bump: {} → {}", current_version, new_cargo_version);
    }

    // 5. Verify new tag doesn't exist
    if git::tag_exists(dir, &new_tag)? {
        bail!("Tag {} already exists", new_tag);
    }

    // 6. Check for uncommitted changes to determine workflow
    let has_changes = git::has_uncommitted_changes(dir)?;

    // 7. Handle dry-run
    if cli.dry_run {
        if action.needs_cargo_update {
            println!("[dry-run] Would update: Cargo.toml");
        }
        if !has_changes && !git::head_has_tag(dir)? {
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

        // 8. Update Cargo.toml if needed
        if action.needs_cargo_update {
            cargo::write_version(&cargo_path, &new_cargo_version)?;
            info!("Updated Cargo.toml to version {}", new_cargo_version);
            cargo::sync_lockfile(dir)?;
        }

        // 9. Stage all changes
        git::stage_all(dir)?;

        // 10. Determine commit message
        let staged_files = git::get_staged_files(dir)?;
        let commit_message = determine_commit_message(cli, &new_tag, &staged_files, action.is_initial_tag)?;

        // 11. Commit
        if !staged_files.is_empty() {
            git::commit(dir, &commit_message)?;
            info!("Committed with message: {}", commit_message);
        }

        // 12. Create annotated tag
        git::create_tag(dir, &new_tag, &commit_message)?;
        info!("Created tag: {}", new_tag);

        println!("Committed and tagged {}", new_tag);
    } else {
        // ===== CLEAN TREE WORKFLOW: No uncommitted changes =====

        // Check if HEAD already has a tag
        if git::head_has_tag(dir)? {
            bail!("HEAD already has a tag. Make changes first, then run bump.");
        }

        // Check if HEAD has been pushed
        let is_pushed = git::is_head_pushed(dir)?;

        // Update Cargo.toml
        if action.needs_cargo_update {
            cargo::write_version(&cargo_path, &new_cargo_version)?;
            info!("Updated Cargo.toml to version {}", new_cargo_version);
            cargo::sync_lockfile(dir)?;
        }

        // Stage the Cargo.toml changes
        git::stage_all(dir)?;
        let staged_files = git::get_staged_files(dir)?;

        if is_pushed {
            // HEAD is pushed - create a new commit
            let commit_message = determine_commit_message(cli, &new_tag, &staged_files, action.is_initial_tag)?;

            if !staged_files.is_empty() {
                git::commit(dir, &commit_message)?;
                info!("Committed with message: {}", commit_message);
            }

            git::create_tag(dir, &new_tag, &commit_message)?;
            info!("Created tag: {}", new_tag);

            println!("Committed and tagged {}", new_tag);
        } else {
            // HEAD is not pushed - amend the previous commit
            if !staged_files.is_empty() {
                git::amend_commit_no_edit(dir)?;
                info!("Amended previous commit with Cargo.toml changes");
            }

            // Use automatic message for the tag since we're amending
            let tag_message = format!("Bump version to {}", new_tag);
            git::create_tag(dir, &new_tag, &tag_message)?;
            info!("Created tag: {}", new_tag);

            println!("Amended commit and tagged {}", new_tag);
        }
    }

    println!("Run: git push && git push --tags");

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

    if failures > 0 && successes == 0 {
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
/// 1. `0.1.0` is the SPECIAL "UNTOUCHED DEFAULT" version.
///    - If Cargo.toml = 0.1.0 and git tags exist → DEFER TO GIT TAG
///    - If Cargo.toml = 0.1.0 and no git tags → Create initial tag v0.1.0
///
/// 2. ANY OTHER VERSION in Cargo.toml means "ACTIVELY MANAGED"
///    - If Cargo.toml != 0.1.0 and latest tag MATCHES → Bump from that version
///    - If Cargo.toml != 0.1.0 and latest tag DOES NOT MATCH → **ERROR**
///    - If Cargo.toml != 0.1.0 and no tags exist → Create initial tag
///
/// 3. If Cargo.toml has NO version field:
///    - If git tags exist → Bump from latest tag
///    - If no git tags → Start at 0.1.0
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

    // =========================================================================
    // RULE 1: Cargo.toml = 0.1.0 (UNTOUCHED DEFAULT)
    // =========================================================================

    /// RULE 1a: Cargo.toml=0.1.0, NO git tags
    /// → Create initial tag v0.1.0, do NOT update Cargo.toml
    #[test]
    fn rule_1a_cargo_at_default_no_tags_creates_initial_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        // NO TAGS

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST create tag v0.1.0
        assert_eq!(action.target_version, Version::new(0, 1, 0), "MUST create tag v0.1.0");
        // MUST NOT update Cargo.toml (it's already at 0.1.0)
        assert!(
            !action.needs_cargo_update,
            "MUST NOT update Cargo.toml - already at 0.1.0"
        );
        // MUST be initial tag
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    /// RULE 1b: Cargo.toml=0.1.0, tag v0.1.0 exists
    /// → Bump to v0.1.1, update Cargo.toml
    #[test]
    fn rule_1b_cargo_at_default_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.0"); // TAG MATCHES DEFAULT

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST bump to v0.1.1
        assert_eq!(
            action.target_version,
            Version::new(0, 1, 1),
            "MUST bump from v0.1.0 to v0.1.1"
        );
        // MUST update Cargo.toml
        assert!(action.needs_cargo_update, "MUST update Cargo.toml to 0.1.1");
        // MUST NOT be initial tag
        assert!(!action.is_initial_tag, "MUST NOT be initial tag - this is a bump");
    }

    /// RULE 1c: Cargo.toml=0.1.0 (untouched), tag v0.1.28 exists (higher)
    /// → DEFER TO GIT TAG: Bump from v0.1.28 to v0.1.29, update Cargo.toml
    #[test]
    fn rule_1c_cargo_at_default_tag_higher_defers_to_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // TAG IS HIGHER

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST bump from tag v0.1.28 to v0.1.29
        assert_eq!(
            action.target_version,
            Version::new(0, 1, 29),
            "MUST bump from git tag v0.1.28 to v0.1.29 (Cargo.toml=0.1.0 is untouched default)"
        );
        // MUST update Cargo.toml to 0.1.29
        assert!(action.needs_cargo_update, "MUST update Cargo.toml from 0.1.0 to 0.1.29");
        // MUST NOT be initial tag
        assert!(!action.is_initial_tag, "MUST NOT be initial tag - this is a bump");
    }

    /// RULE 1d: Same as 1c but with minor bump
    /// → Bump from v0.1.28 to v0.2.0
    #[test]
    fn rule_1d_cargo_at_default_tag_higher_minor_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.0")); // DEFAULT UNTOUCHED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // TAG IS HIGHER

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Minor).unwrap();

        // MUST minor bump from tag v0.1.28 to v0.2.0
        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST minor bump from git tag v0.1.28 to v0.2.0"
        );
        assert!(action.needs_cargo_update);
        assert!(!action.is_initial_tag);
    }

    // =========================================================================
    // RULE 2: Cargo.toml != 0.1.0 (ACTIVELY MANAGED)
    // =========================================================================

    /// RULE 2a: Cargo.toml=0.2.0 (managed), tag v0.1.28 (MISMATCH)
    /// → **ERROR**: Version mismatch
    #[test]
    fn rule_2a_cargo_managed_tag_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED (not 0.1.0)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // DOES NOT MATCH

        let cargo_path = dir.join("Cargo.toml");
        let result = determine_version_action(dir, &cargo_path, BumpType::Patch);

        // MUST ERROR
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
    /// → **ERROR**: Version mismatch
    #[test]
    fn rule_2b_cargo_managed_lower_than_tag_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED (not 0.1.0)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // DOES NOT MATCH (higher)

        let cargo_path = dir.join("Cargo.toml");
        let result = determine_version_action(dir, &cargo_path, BumpType::Patch);

        // MUST ERROR
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
    /// → Bump to v0.2.1, update Cargo.toml
    #[test]
    fn rule_2c_cargo_managed_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.2.0"); // MATCHES

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST bump to v0.2.1
        assert_eq!(
            action.target_version,
            Version::new(0, 2, 1),
            "MUST bump from v0.2.0 to v0.2.1"
        );
        assert!(action.needs_cargo_update, "MUST update Cargo.toml");
        assert!(!action.is_initial_tag);
    }

    /// RULE 2d: Cargo.toml=0.1.5 (managed), tag v0.1.5 (MATCHES)
    /// → Bump to v0.1.6, update Cargo.toml
    #[test]
    fn rule_2d_cargo_managed_tag_matches_bumps() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED (not 0.1.0!)
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST bump to v0.1.6
        assert_eq!(
            action.target_version,
            Version::new(0, 1, 6),
            "MUST bump from v0.1.5 to v0.1.6"
        );
        assert!(action.needs_cargo_update, "MUST update Cargo.toml");
        assert!(!action.is_initial_tag);
    }

    /// RULE 2e: Cargo.toml=0.2.0 (managed), NO tags
    /// → Create initial tag v0.2.0, do NOT update Cargo.toml
    #[test]
    fn rule_2e_cargo_managed_no_tags_creates_initial_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.2.0")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        // NO TAGS

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST create tag v0.2.0
        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST create initial tag v0.2.0"
        );
        // MUST NOT update Cargo.toml (it's already at 0.2.0)
        assert!(
            !action.needs_cargo_update,
            "MUST NOT update Cargo.toml - already at 0.2.0"
        );
        // MUST be initial tag
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    /// RULE 2f: Cargo.toml=0.1.5 (managed), tag v0.1.5, minor bump
    /// → Bump to v0.2.0
    #[test]
    fn rule_2f_cargo_managed_minor_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Minor).unwrap();

        // MUST bump to v0.2.0
        assert_eq!(
            action.target_version,
            Version::new(0, 2, 0),
            "MUST minor bump from v0.1.5 to v0.2.0"
        );
        assert!(action.needs_cargo_update);
        assert!(!action.is_initial_tag);
    }

    /// RULE 2g: Cargo.toml=0.1.5 (managed), tag v0.1.5, major bump
    /// → Bump to v1.0.0
    #[test]
    fn rule_2g_cargo_managed_major_bump() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.1.5")); // ACTIVELY MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5"); // MATCHES

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Major).unwrap();

        // MUST bump to v1.0.0
        assert_eq!(
            action.target_version,
            Version::new(1, 0, 0),
            "MUST major bump from v0.1.5 to v1.0.0"
        );
        assert!(action.needs_cargo_update);
        assert!(!action.is_initial_tag);
    }

    // =========================================================================
    // RULE 3: Cargo.toml has NO version field
    // =========================================================================

    /// RULE 3a: NO version in Cargo.toml, tag v0.1.5 exists
    /// → Bump from tag to v0.1.6, update Cargo.toml
    #[test]
    fn rule_3a_no_cargo_version_tag_exists_bumps_from_tag() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, None); // NO VERSION FIELD
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.5");

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST bump from tag v0.1.5 to v0.1.6
        assert_eq!(
            action.target_version,
            Version::new(0, 1, 6),
            "MUST bump from git tag v0.1.5 to v0.1.6"
        );
        // MUST update Cargo.toml (it has no version)
        assert!(action.needs_cargo_update, "MUST update Cargo.toml to 0.1.6");
        assert!(!action.is_initial_tag);
    }

    /// RULE 3b: NO version in Cargo.toml, NO tags
    /// → Start at v0.1.0, update Cargo.toml, create initial tag
    #[test]
    fn rule_3b_no_cargo_version_no_tags_starts_at_0_1_0() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, None); // NO VERSION FIELD
        create_initial_commit(dir);
        // NO TAGS

        let cargo_path = dir.join("Cargo.toml");
        let action = determine_version_action(dir, &cargo_path, BumpType::Patch).unwrap();

        // MUST start at v0.1.0
        assert_eq!(action.target_version, Version::new(0, 1, 0), "MUST start at v0.1.0");
        // MUST update Cargo.toml (it has no version)
        assert!(action.needs_cargo_update, "MUST update Cargo.toml to 0.1.0");
        // MUST be initial tag
        assert!(action.is_initial_tag, "MUST be initial tag");
    }

    // =========================================================================
    // EDGE CASES: Cargo.toml higher than tag but tag doesn't match
    // =========================================================================

    /// EDGE CASE: Cargo.toml=0.3.0, tag v0.1.28 exists
    /// → **ERROR**: Mismatch (Cargo.toml is managed, doesn't match tag)
    #[test]
    fn edge_cargo_higher_than_tag_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("0.3.0")); // MANAGED, HIGHER THAN TAG
        create_initial_commit(dir);
        create_git_tag(dir, "v0.1.28"); // LOWER, DOES NOT MATCH

        let cargo_path = dir.join("Cargo.toml");
        let result = determine_version_action(dir, &cargo_path, BumpType::Patch);

        // MUST ERROR - this is a mismatch situation
        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=0.3.0 does not match latest tag v0.1.28"
        );
    }

    /// EDGE CASE: Cargo.toml=1.0.0, tag v0.9.0 exists
    /// → **ERROR**: Mismatch
    #[test]
    fn edge_cargo_1_0_0_tag_0_9_0_mismatch_is_error() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        setup_git_repo(dir);
        create_cargo_toml(dir, Some("1.0.0")); // MANAGED
        create_initial_commit(dir);
        create_git_tag(dir, "v0.9.0"); // DOES NOT MATCH

        let cargo_path = dir.join("Cargo.toml");
        let result = determine_version_action(dir, &cargo_path, BumpType::Patch);

        // MUST ERROR
        assert!(
            result.is_err(),
            "MUST ERROR: Cargo.toml=1.0.0 does not match latest tag v0.9.0"
        );
    }
}
