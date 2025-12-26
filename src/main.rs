use clap::Parser;
use eyre::{Context, Result, bail};
use log::info;
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

    // 3. Determine current version
    let current_version = determine_current_version(dir, &cargo_path)?;
    info!("Current version: {}", current_version);

    // 4. Parse and bump version
    let parsed_version = version::parse_version(&current_version)?;
    let new_version = version::bump_version(&parsed_version, bump_type);
    let new_tag = version::format_tag(&new_version);
    let new_cargo_version = version::format_cargo_version(&new_version);

    println!(
        "bump: {} → {}",
        version::format_cargo_version(&parsed_version),
        new_cargo_version
    );

    // 5. Verify new tag doesn't exist
    if git::tag_exists(dir, &new_tag)? {
        bail!("Tag {} already exists", new_tag);
    }

    // 6. Update Cargo.toml
    if dry_run {
        println!("[dry-run] Would update: Cargo.toml");
        println!("[dry-run] Would commit and tag: {}", new_tag);
        return Ok(());
    }

    cargo::write_version(&cargo_path, &new_cargo_version)?;
    info!("Updated Cargo.toml to version {}", new_cargo_version);

    // 7. Stage all changes
    git::stage_all(dir)?;

    // 8. Determine commit message
    let staged_files = git::get_staged_files(dir)?;
    let only_cargo_toml = staged_files.len() == 1 && staged_files[0] == "Cargo.toml";

    let commit_message = if only_cargo_toml {
        format!("Bump version to {}", new_tag)
    } else {
        prompt_commit_message(&staged_files)?
    };

    // 9. Commit
    git::commit(dir, &commit_message)?;
    info!("Committed with message: {}", commit_message);

    // 10. Create annotated tag
    git::create_tag(dir, &new_tag, &commit_message)?;
    info!("Created tag: {}", new_tag);

    println!("Committed and tagged {}", new_tag);
    println!("Run: git push && git push --tags");

    if !dir_name.is_empty() && dir != env::current_dir().unwrap_or_default() {
        println!("[{}] Done", dir_name);
    }

    Ok(())
}

/// Determine the current version to bump from
fn determine_current_version(dir: &Path, cargo_path: &Path) -> Result<String> {
    // First try to read from Cargo.toml
    if let Some(cargo_version) = cargo::read_version(cargo_path)? {
        // Check if this version's tag already exists (indicates Cargo.toml is stale)
        let parsed = version::parse_version(&cargo_version)?;
        let tag = version::format_tag(&parsed);

        if git::tag_exists(dir, &tag)? {
            info!("Tag {} exists, Cargo.toml may be stale. Using latest git tag.", tag);
            // Fall through to git tag
        } else {
            return Ok(cargo_version);
        }
    }

    // Fall back to latest git tag
    if let Some(tag) = git::get_latest_tag(dir)? {
        info!("Using latest git tag: {}", tag);
        return Ok(tag);
    }

    // No version found anywhere, start at 0.0.0
    info!("No version found, starting at 0.0.0");
    Ok("0.0.0".to_string())
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
