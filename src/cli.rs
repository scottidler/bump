use clap::Parser;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);

#[derive(Parser)]
#[command(
    name = "bump",
    about = "bump semantic versions in Cargo.toml, commit, and tag",
    version = env!("GIT_DESCRIBE"),
    after_help = HELP_TEXT.as_str()
)]
pub struct Cli {
    /// Bump major version (X.0.0)
    #[arg(short = 'M', long, conflicts_with = "minor")]
    pub major: bool,

    /// Bump minor version (x.Y.0)
    #[arg(short = 'm', long, conflicts_with = "major")]
    pub minor: bool,

    /// Preview changes without applying
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Commit message to use
    #[arg(long, conflicts_with = "automatic")]
    pub message: Option<String>,

    /// Generate automatic commit message
    #[arg(short = 'a', long, conflicts_with = "message")]
    pub automatic: bool,

    /// Paths to git repository roots
    #[arg(value_name = "DIRECTORIES")]
    pub directories: Vec<PathBuf>,
}

/// Generate tool validation help text (called once via LazyLock)
fn get_tool_validation_help() -> String {
    let git_status = check_tool_version("git", "--version", "2.20.0");
    format!(
        "REQUIRED TOOLS:\n  {} {:<10} {}\n\nLogs are written to: ~/.local/share/bump/logs/bump.log",
        git_status.status_icon, "git", git_status.version
    )
}

struct ToolStatus {
    version: String,
    status_icon: String,
}

/// Check if a tool is installed and meets minimum version requirements
fn check_tool_version(tool: &str, version_arg: &str, min_version: &str) -> ToolStatus {
    match Command::new(tool).arg(version_arg).output() {
        Ok(output) if output.status.success() => {
            let version_output = String::from_utf8_lossy(&output.stdout);
            let version = extract_version_from_output(tool, &version_output);

            let meets_requirement = if let Some(stripped) = version.strip_prefix('v') {
                version_compare(stripped, min_version)
            } else {
                version_compare(&version, min_version)
            };

            ToolStatus {
                version: if version.is_empty() { "unknown".to_string() } else { version },
                status_icon: if meets_requirement { "✅" } else { "⚠️" }.to_string(),
            }
        }
        _ => ToolStatus {
            version: "not found".to_string(),
            status_icon: "❌".to_string(),
        },
    }
}

/// Extract version number from tool output
fn extract_version_from_output(tool: &str, output: &str) -> String {
    if tool == "git" {
        // git version 2.34.1
        if let Some(line) = output.lines().next()
            && let Some(version_part) = line.split_whitespace().nth(2)
        {
            return version_part.to_string();
        }
    }
    "unknown".to_string()
}

/// Simple version comparison (assumes semantic versioning)
fn version_compare(version: &str, min_version: &str) -> bool {
    let parse_version = |v: &str| -> Vec<u32> { v.split('.').map(|part| part.parse().unwrap_or(0)).collect() };

    let v1 = parse_version(version);
    let v2 = parse_version(min_version);

    for (a, b) in v1.iter().zip(v2.iter()) {
        if a > b {
            return true;
        }
        if a < b {
            return false;
        }
    }

    v1.len() >= v2.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_compare() {
        assert!(version_compare("2.34.1", "2.20.0"));
        assert!(version_compare("2.20.0", "2.20.0"));
        assert!(!version_compare("2.19.0", "2.20.0"));
        assert!(version_compare("3.0.0", "2.20.0"));
        assert!(!version_compare("1.0.0", "2.20.0"));
    }

    #[test]
    fn test_extract_git_version() {
        let output = "git version 2.43.0";
        assert_eq!(extract_version_from_output("git", output), "2.43.0");
    }

    #[test]
    fn test_cli_parsing() {
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(!cli.major);
        assert!(!cli.minor);
        assert!(!cli.dry_run);
        assert!(cli.directories.is_empty());
    }

    #[test]
    fn test_cli_major_flag() {
        let cli = Cli::try_parse_from(["bump", "--major"]).unwrap();
        assert!(cli.major);
        assert!(!cli.minor);
    }

    #[test]
    fn test_cli_minor_flag() {
        let cli = Cli::try_parse_from(["bump", "-m"]).unwrap();
        assert!(!cli.major);
        assert!(cli.minor);
    }

    #[test]
    fn test_cli_dry_run() {
        let cli = Cli::try_parse_from(["bump", "-n"]).unwrap();
        assert!(cli.dry_run);
    }

    #[test]
    fn test_cli_directories() {
        let cli = Cli::try_parse_from(["bump", "./proj1", "./proj2"]).unwrap();
        assert_eq!(cli.directories.len(), 2);
    }

    #[test]
    fn test_cli_major_minor_conflict() {
        let result = Cli::try_parse_from(["bump", "--major", "--minor"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_message_flag() {
        let cli = Cli::try_parse_from(["bump", "--message", "my commit message"]).unwrap();
        assert_eq!(cli.message, Some("my commit message".to_string()));
        assert!(!cli.automatic);
    }

    #[test]
    fn test_cli_automatic_flag() {
        let cli = Cli::try_parse_from(["bump", "-a"]).unwrap();
        assert!(cli.automatic);
        assert!(cli.message.is_none());
    }

    #[test]
    fn test_cli_automatic_long_flag() {
        let cli = Cli::try_parse_from(["bump", "--automatic"]).unwrap();
        assert!(cli.automatic);
    }

    #[test]
    fn test_cli_message_automatic_conflict() {
        let result = Cli::try_parse_from(["bump", "--message", "test", "--automatic"]);
        assert!(result.is_err());
    }
}
