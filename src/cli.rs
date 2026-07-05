use clap::Parser;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);

#[derive(Parser)]
#[command(
    name = "bump",
    about = "bump semantic versions, commit, and tag (Rust, Python, or any git repo)",
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

    /// Force bump even if HEAD already has a tag
    #[arg(short = 'f', long)]
    pub force: bool,

    /// Bump version and commit, but do NOT create a tag (for PR-gated repos)
    #[arg(long, conflicts_with = "tag_only")]
    pub no_tag: bool,

    /// Create the tag for the current manifest version on HEAD (post-merge step
    /// for PR-gated repos). No version change, no commit.
    #[arg(long, conflicts_with_all = ["no_tag", "major", "minor", "force", "message", "automatic"])]
    pub tag_only: bool,

    /// Report gate status (classic protection + rulesets) and the recommended flow
    #[arg(long)]
    pub gates: bool,

    /// Skip the remote gate probe (treat the repo as ungated)
    #[arg(long)]
    pub no_verify: bool,

    /// Leave a workspace member's independent (literal) version untouched, matched by
    /// package name (e.g. claude-pricing, not the member path). Repeatable and
    /// space-separated. For a member whose version is a deliberate contract, not
    /// workspace-inherited; a name matching no independent member is an error.
    #[arg(long = "skip-member", value_name = "NAME", num_args = 1.., action = clap::ArgAction::Append)]
    pub skip_member: Vec<String>,

    /// Paths to git repository roots
    #[arg(value_name = "DIRECTORIES")]
    pub directories: Vec<PathBuf>,
}

/// Generate tool validation help text (called once via LazyLock)
fn get_tool_validation_help() -> String {
    let git_status = check_tool_version("git", "--version", "2.20.0");
    let gh_status = check_tool_version("gh", "--version", "2.0.0");
    format!(
        "RELEASE FLOWS (run `bump --gates` to see which applies to this repo):\n\
         \x20 ungated:  bump [-m|-M]  &&  git push origin <branch>  &&  git push origin vX.Y.Z\n\
         \x20 gated:    bump --no-tag [-m|-M]   (version bump rides your PR branch)\n\
         \x20           <push branch, open PR, merge>\n\
         \x20           git checkout <default> && git pull --ff-only origin <default>\n\
         \x20           bump --tag-only  &&  git push origin vX.Y.Z\n\n\
         REQUIRED TOOLS:\n  {} {:<10} {}\n  {} {:<10} {}\n\n\
         gh is used to probe branch-protection gates; without it gated repos cannot be\n\
         detected and bump warns and proceeds as if ungated.\n\n\
         Logs are written to: ~/.local/share/bump/logs/bump.log",
        git_status.status_icon, "git", git_status.version, gh_status.status_icon, "gh", gh_status.version
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
    // Both `git --version` ("git version 2.34.1") and `gh --version`
    // ("gh version 2.40.1 (2023-12-13)") put the version in the third field.
    if (tool == "git" || tool == "gh")
        && let Some(line) = output.lines().next()
        && let Some(version_part) = line.split_whitespace().nth(2)
    {
        return version_part.to_string();
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
    fn test_extract_gh_version() {
        let output = "gh version 2.40.1 (2023-12-13)\nhttps://github.com/cli/cli/releases/latest";
        assert_eq!(extract_version_from_output("gh", output), "2.40.1");
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
    fn test_cli_force_flag() {
        let cli = Cli::try_parse_from(["bump", "--force"]).unwrap();
        assert!(cli.force);
    }

    #[test]
    fn test_cli_force_short_flag() {
        let cli = Cli::try_parse_from(["bump", "-f"]).unwrap();
        assert!(cli.force);
    }

    #[test]
    fn test_cli_no_verify_flag() {
        let cli = Cli::try_parse_from(["bump", "--no-verify"]).unwrap();
        assert!(cli.no_verify);
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(!cli.no_verify);
    }

    #[test]
    fn test_cli_no_tag_flag() {
        let cli = Cli::try_parse_from(["bump", "--no-tag"]).unwrap();
        assert!(cli.no_tag);
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(!cli.no_tag);
    }

    #[test]
    fn test_cli_tag_only_flag() {
        let cli = Cli::try_parse_from(["bump", "--tag-only"]).unwrap();
        assert!(cli.tag_only);
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(!cli.tag_only);
    }

    #[test]
    fn test_cli_gates_flag() {
        let cli = Cli::try_parse_from(["bump", "--gates"]).unwrap();
        assert!(cli.gates);
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(!cli.gates);
    }

    #[test]
    fn test_cli_tag_only_conflicts_with_no_tag() {
        assert!(Cli::try_parse_from(["bump", "--tag-only", "--no-tag"]).is_err());
    }

    #[test]
    fn test_cli_tag_only_conflicts_with_bump_flags() {
        assert!(Cli::try_parse_from(["bump", "--tag-only", "-M"]).is_err());
        assert!(Cli::try_parse_from(["bump", "--tag-only", "-m"]).is_err());
        assert!(Cli::try_parse_from(["bump", "--tag-only", "--force"]).is_err());
        assert!(Cli::try_parse_from(["bump", "--tag-only", "-a"]).is_err());
        assert!(Cli::try_parse_from(["bump", "--tag-only", "--message", "x"]).is_err());
    }

    #[test]
    fn test_cli_message_automatic_conflict() {
        let result = Cli::try_parse_from(["bump", "--message", "test", "--automatic"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_skip_member_default_empty() {
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(cli.skip_member.is_empty());
    }

    #[test]
    fn test_cli_skip_member_repeated() {
        let cli = Cli::try_parse_from(["bump", "--skip-member", "a", "--skip-member", "b"]).unwrap();
        assert_eq!(cli.skip_member, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_cli_skip_member_space_separated() {
        let cli = Cli::try_parse_from(["bump", "--skip-member", "a", "b"]).unwrap();
        assert_eq!(cli.skip_member, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_cli_skip_member_single() {
        let cli = Cli::try_parse_from(["bump", "--skip-member", "claude-pricing"]).unwrap();
        assert_eq!(cli.skip_member, vec!["claude-pricing".to_string()]);
    }

    /// Documented footgun: `--skip-member` uses num_args=1.. (space-separated, per the
    /// house CLI rule), so a trailing DIRECTORIES positional is greedily swallowed into
    /// the skip list. This is a KNOWN contract, not a surprise -- and it fails closed
    /// downstream (the swallowed path is a stale skip name that aborts validation). The
    /// realistic invocation runs bump in the repo (no positional) or puts the directory
    /// before the flag.
    #[test]
    fn test_cli_skip_member_swallows_trailing_positional() {
        let cli = Cli::try_parse_from(["bump", "--skip-member", "claude-pricing", "./some-dir"]).unwrap();
        assert_eq!(
            cli.skip_member,
            vec!["claude-pricing".to_string(), "./some-dir".to_string()]
        );
        assert!(
            cli.directories.is_empty(),
            "the positional was swallowed by --skip-member"
        );

        // Putting the directory before the flag keeps them separate.
        let cli = Cli::try_parse_from(["bump", "./some-dir", "--skip-member", "claude-pricing"]).unwrap();
        assert_eq!(cli.skip_member, vec!["claude-pricing".to_string()]);
        assert_eq!(cli.directories.len(), 1);
    }
}
