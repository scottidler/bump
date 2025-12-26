use clap::Parser;
use eyre::{Context, Result, bail};
use log::info;
use semver::Version;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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

/// Prompt user for commit message
fn prompt_commit_message(staged_files: &[String]) -> Result<String> {
    println!("\nStaged changes:");
    for file in staged_files {
        println!("  {}", file);
    }
    println!();

    print!("Enter commit message: ");
    io::stdout().flush()?;

    let mut message = String::new();
    io::stdin().read_line(&mut message)?;

    let message = message.trim().to_string();
    if message.is_empty() {
        bail!("Commit message cannot be empty");
    }
    Ok(message)
}

/// Result of determining what version action to take
struct VersionAction {
    /// The version to tag
    target_version: Version,
    /// Whether we need to update Cargo.toml
    needs_cargo_update: bool,
    /// Whether this is an initial tag (no bump) vs a version bump
    is_initial_tag: bool,
}

/// Determine what version action to take
fn determine_version_action(dir: &Path, cargo_path: &Path, bump_type: BumpType) -> Result<VersionAction> {
    // First try to read from Cargo.toml
    if let Some(cargo_version) = cargo::read_version(cargo_path)? {
        let parsed = version::parse_version(&cargo_version)?;
        let tag = version::format_tag(&parsed);

        if !git::tag_exists(dir, &tag)? {
            // Tag doesn't exist for current Cargo.toml version
            // This is initial tagging - create tag for current version, no bump needed
            info!("Tag {} does not exist. Creating initial tag for current version.", tag);
            return Ok(VersionAction {
                target_version: parsed,
                needs_cargo_update: false,
                is_initial_tag: true,
            });
        }

        // Tag exists - need to bump from current version
        info!("Tag {} exists. Bumping version.", tag);
        let bumped = version::bump_version(&parsed, bump_type);
        return Ok(VersionAction {
            target_version: bumped,
            needs_cargo_update: true,
            is_initial_tag: false,
        });
    }

    // No version in Cargo.toml, check git tags
    if let Some(tag) = git::get_latest_tag(dir)? {
        info!("No version in Cargo.toml. Using latest git tag: {}", tag);
        let parsed = version::parse_version(&tag)?;
        let bumped = version::bump_version(&parsed, bump_type);
        return Ok(VersionAction {
            target_version: bumped,
            needs_cargo_update: true,
            is_initial_tag: false,
        });
    }

    // No version anywhere - start at 0.1.0 as initial tag
    info!("No version found anywhere. Starting at 0.1.0");
    Ok(VersionAction {
        target_version: Version::new(0, 1, 0),
        needs_cargo_update: true,
        is_initial_tag: true,
    })
}

/// Process a single directory
fn process_directory(dir: &Path, bump_type: BumpType, dry_run: bool) -> Result<()> {
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

    // 6. Handle dry-run
    if dry_run {
        if action.needs_cargo_update {
            println!("[dry-run] Would update: Cargo.toml");
        }
        println!("[dry-run] Would commit and tag: {}", new_tag);
        return Ok(());
    }

    // 7. Update Cargo.toml if needed
    if action.needs_cargo_update {
        cargo::write_version(&cargo_path, &new_cargo_version)?;
        info!("Updated Cargo.toml to version {}", new_cargo_version);

        // 7b. Sync Cargo.lock if it exists
        cargo::sync_lockfile(dir)?;
    }

    // 8. Stage all changes
    git::stage_all(dir)?;

    // 9. Determine commit message
    let staged_files = git::get_staged_files(dir)?;

    let commit_message = if staged_files.is_empty() {
        // No changes staged (initial tag with clean working tree)
        // We still need to create a tag, but no commit needed
        // Actually, git tag can be created without a new commit
        // But for consistency, let's create an empty commit or just tag HEAD
        format!("Release {}", new_tag)
    } else {
        let only_cargo_toml = staged_files.len() == 1 && staged_files[0] == "Cargo.toml";
        if only_cargo_toml || (action.is_initial_tag && staged_files.is_empty()) {
            if action.is_initial_tag {
                format!("Release {}", new_tag)
            } else {
                format!("Bump version to {}", new_tag)
            }
        } else {
            prompt_commit_message(&staged_files)?
        }
    };

    // 10. Commit (only if there are staged changes)
    if !staged_files.is_empty() {
        git::commit(dir, &commit_message)?;
        info!("Committed with message: {}", commit_message);
    }

    // 11. Create annotated tag
    git::create_tag(dir, &new_tag, &commit_message)?;
    info!("Created tag: {}", new_tag);

    println!("Committed and tagged {}", new_tag);
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
        cli.directories
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

        match process_directory(&dir, bump_type, cli.dry_run) {
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
