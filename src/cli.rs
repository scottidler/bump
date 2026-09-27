use crate::release::DEFAULT_CI_TIMEOUT;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

/// `--ci-timeout`'s default, in seconds -- the single source of truth is
/// `release::DEFAULT_CI_TIMEOUT`; clap's `default_value_t` needs the plain `u64` it
/// renders into `--help`, not a `Duration`.
fn default_ci_timeout_secs() -> u64 {
    DEFAULT_CI_TIMEOUT.as_secs()
}

static HELP_TEXT: LazyLock<String> = LazyLock::new(get_tool_validation_help);
static RELEASE_HELP_TEXT: LazyLock<String> = LazyLock::new(get_release_help);
static FINISH_HELP_TEXT: LazyLock<String> = LazyLock::new(get_finish_help);

#[derive(Parser)]
#[command(
    name = "bump",
    about = "bump semantic versions, commit, and tag (Rust, Python, or any git repo)",
    version = env!("GIT_DESCRIBE"),
    after_help = HELP_TEXT.as_str()
)]
pub struct Cli {
    /// The two release verbs (`release`, `finish`). Absent -> the legacy flag behavior
    /// below, UNCHANGED. A directory literally named `release`/`finish` with no path
    /// prefix (e.g. `bump release` meaning "process the dir named release") is the one
    /// documented ambiguity this optional-subcommand pattern carries; prefix it
    /// (`./release`) to disambiguate, exactly like any other clap subcommand CLI.
    #[command(subcommand)]
    pub command: Option<Commands>,

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

    /// Never fall back to amending the previous commit, even when HEAD looks unpushed --
    /// always create a NEW commit. Not a CLI flag: `#[arg(skip)]` because the release
    /// verbs set this programmatically before delegating into the shared bump logic; no
    /// operator flag should let an ordinary `bump` invocation flip it.
    #[arg(skip)]
    pub never_amend: bool,
}

/// The two release verbs. `None` (no subcommand) is today's legacy `bump` behavior,
/// untouched. See the design doc's API Design section for the full state tables these
/// verbs implement; `--help` on each subcommand summarizes them.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Release the current repo: on ungated main this lands a version commit, pushes it,
    /// then tags and pushes the tag; on a gated repo it rides the bump on a feature branch,
    /// pushes it, opens a PR if none is open, and pauses for `bump finish` after merge.
    #[command(after_help = RELEASE_HELP_TEXT.as_str())]
    Release(ReleaseArgs),

    /// Post-merge step for gated repos: fast-forward to the merged default branch, tag the
    /// merged version, and push the tag by name. No-op if it is already released; resumes
    /// a local-only tag from a prior interrupted run.
    #[command(after_help = FINISH_HELP_TEXT.as_str())]
    Finish(FinishArgs),
}

/// `bump release [-m|-M] [-n] [--install "<cmd>"|--no-install] [--standalone "<words>"]
/// [--no-ci-gate] [--ci-timeout SECS]`
#[derive(clap::Args, Debug)]
pub struct ReleaseArgs {
    /// Bump major version (X.0.0)
    #[arg(short = 'M', long, conflicts_with = "minor")]
    pub major: bool,

    /// Bump minor version (x.Y.0)
    #[arg(short = 'm', long, conflicts_with = "major")]
    pub minor: bool,

    /// Preview every command that would run; execute nothing
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Run this exact command after a successful release, instead of the config/default
    #[arg(long, value_name = "CMD", conflicts_with = "no_install")]
    pub install: Option<String>,

    /// Skip the post-release install step entirely
    #[arg(long, conflicts_with = "install")]
    pub no_install: bool,

    /// Scott's standalone order, verbatim: the one legitimate way to ship a bump-only
    /// release (a branch/default whose diff is empty or version-only otherwise refuses).
    /// Quoted into the PR body (gated) or printed (ungated). Re-asking for this order is
    /// a violation -- an agent runs this flag only when Scott already gave the words in
    /// this session.
    #[arg(long, value_name = "WORDS")]
    pub standalone: Option<String>,

    /// Skip the CI gate: tag immediately after the push instead of waiting for green CI
    /// on the sha. For a human at a terminal who already knows the repo's CI story; an
    /// agent should not reach for this to get unstuck.
    #[arg(long)]
    pub no_ci_gate: bool,

    /// How long the CI gate waits on incomplete check runs before refusing (seconds).
    /// Never tags on a timeout -- the re-run reuses the same version.
    #[arg(long, value_name = "SECS", default_value_t = default_ci_timeout_secs())]
    pub ci_timeout: u64,
}

/// `bump finish [-n] [--install "<cmd>"|--no-install] [--no-ci-gate] [--ci-timeout SECS]`
#[derive(clap::Args, Debug)]
pub struct FinishArgs {
    /// Preview every command that would run; execute nothing
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Run this exact command after a successful finish, instead of the config/default
    #[arg(long, value_name = "CMD", conflicts_with = "no_install")]
    pub install: Option<String>,

    /// Skip the post-release install step entirely
    #[arg(long, conflicts_with = "install")]
    pub no_install: bool,

    /// Skip the CI gate: tag the merged commit immediately instead of waiting for green
    /// CI on it.
    #[arg(long)]
    pub no_ci_gate: bool,

    /// How long the CI gate waits on incomplete check runs before refusing (seconds).
    #[arg(long, value_name = "SECS", default_value_t = default_ci_timeout_secs())]
    pub ci_timeout: u64,
}

/// Generate the `bump release --help` after-help text: the state table (condensed), the
/// two flows and their refusals, RESUME, `--standalone`, the CI wait, the persona token
/// rule, required tools and the runtime log path, matching `get_tool_validation_help`'s
/// sources.
fn get_release_help() -> String {
    let git_status = check_tool_version("git", "--version", "2.20.0");
    let gh_status = check_tool_version("gh", "--version", "2.0.0");
    format!(
        "TWO FLOWS, decided by `bump --gates` (never both):\n\
         \x20 UNGATED: version commit -> push origin <default> -> confirm on origin -> wait \
         for green CI -> tag the verified sha -> push tag -> install\n\
         \x20 GATED:   version commit rides the feature branch -> push branch \
         (no tags follow it) -> open a PR if none is open (title/body built from the branch \
         and its commits) -> pause: \"merge the PR, then run: bump finish\"\n\n\
         STATE TABLE (condensed; see the design doc's API Design section for the full table):\n\
         \x20 ungated, on default, ahead of origin, no pending version: the UNGATED flow above\n\
         \x20 ungated, PENDING VERSION (manifest untagged, not below the latest tag): that \
         version IS the release -- pushed if ahead, never re-bumped past it, an explicit \
         -m/-M implying a different version refuses naming both\n\
         \x20 ungated RESUME (pending version, HEAD == origin): wait for green CI, tag, push \
         tag, install -- never reported as \"already released\"\n\
         \x20 ungated, manifest below the latest tag: refuse by name; bump never lowers a \
         version\n\
         \x20 ungated, not on default / behind / diverged / nothing to release: refuse with \
         the one exact fix (checkout, `git pull --ff-only`, `git pull --rebase`, or \
         `--standalone`)\n\
         \x20 gated, feature branch, fresh: rides the bump, pushes, opens/ensures the PR, \
         pauses\n\
         \x20 gated, feature branch, already bumped (version line already in the diff): skip \
         the re-bump, ensure branch/PR, same pause; a mismatched level refuses naming both\n\
         \x20 gated, feature branch, PENDING VERSION inherited from the default branch (no \
         version line of its own): bumps AGAIN from the manifest version, names the untagged \
         one it is burning unless `bump finish` ships it first\n\
         \x20 gated, feature branch whose diff vs the default is empty or version-only: \
         refuses, names `--standalone`\n\
         \x20 gated, feature branch where the title-guard slug != the branch name: refuse \
         before any mutation, prints `git branch -m <slug>`\n\
         \x20 gated, on default with commits not on origin (stranded): refuse with the \
         literal rescue commands, never auto-rescued\n\
         \x20 gated, on default, clean, tagged: refuse \"bump rides a feature PR\", names \
         `--standalone`\n\
         \x20 gate unknown, dirty tree, detached HEAD: refuse with the one exact fix\n\n\
         --standalone \"<words>\": Scott's words, verbatim -- the one legitimate way to ship \
         a release whose diff is empty or version-only (an empty diff, or a tagged/clean \
         default with nothing new). On a tagged default this cuts (or reuses) a \
         bump-vX-Y-Z branch and runs the GATED flow with the words quoted in the PR body; \
         on an ungated tagged default the version commit itself is the release. Re-asking \
         for this order is a violation: only pass it when Scott already gave the words in \
         this session; otherwise STOP and report.\n\n\
         CI WAIT: no tag exists until the pushed sha's check runs + legacy commit status are \
         all green (polled every 15s, up to --ci-timeout, default {ci_timeout}s). Red, \
         truncated, or errored CI refuses with NO tag created; a re-run reuses the same \
         version, it never bumps past it. --no-ci-gate bypasses the wait entirely -- for a \
         human at a terminal who already knows the repo's CI story, not for an agent to get \
         unstuck.\n\n\
         PERSONA TOKENS: gh calls are authed per-org -- a token file, then \
         GITHUB_PAT_<ORG>, then GITHUB_PAT_WORK for tatari-tv / GITHUB_PAT_HOME otherwise, \
         else ambient `gh auth` -- so a work-org PR/CI read never goes out under the wrong \
         account.\n\n\
         REQUIRED TOOLS:\n  {} {:<10} {}\n  {} {:<10} {}\n\n\
         gh probes branch-protection gates, opens/lists PRs, and reads CI; `bump release` \
         FAILS CLOSED on an unknown gate verdict or an unreadable CI read (it pushes and \
         tags, unlike plain bump).\n\n\
         Logs are written to: {}",
        git_status.status_icon,
        "git",
        git_status.version,
        gh_status.status_icon,
        "gh",
        gh_status.version,
        log_path_for_help(),
        ci_timeout = default_ci_timeout_secs(),
    )
}

/// Generate the `bump finish --help` after-help text: the finish table, RESUME, the CI
/// gate, the persona token rule, tools and log path.
fn get_finish_help() -> String {
    let git_status = check_tool_version("git", "--version", "2.20.0");
    let gh_status = check_tool_version("gh", "--version", "2.0.0");
    format!(
        "Runs from ANY worktree of the repo: the default branch's OWN checkout, else a \
         sibling worktree found via `git worktree list`, else it is checked out here.\n\n\
         STATE TABLE (condensed; see the design doc's API Design section for the full table):\n\
         \x20 origin/<default> carries an untagged version (the merged bump): fast-forward \
         (if behind) -> wait for green CI on the merged sha -> tag it -> push tag by name -> \
         install\n\
         \x20 origin/<default> version == last tag (nothing merged / bump never rode): \
         refuse \"no untagged version on <default>; bump rides a feature PR\", names \
         `bump release --standalone` as the one door\n\
         \x20 tag vX exists on the remote at the merged commit: no-op \"already released\" -- \
         install still runs, so a re-run after \"tag pushed, install failed\" installs \
         (--no-install to skip)\n\
         \x20 tag vX exists LOCALLY only (RESUME: a prior run died before/during the tag \
         push): wait for green CI, push the tag, install -- never reported as already \
         released\n\
         \x20 local default ahead of origin (commits that never landed): refuse before any \
         pull, with the literal rescue commands\n\
         \x20 local default diverged from origin: refuse before any pull, names \
         `git pull --rebase origin <default>`\n\
         \x20 generic repo (no manifest), gated: refuse -- finish cannot derive a version\n\
         \x20 tracked changes in the current OR the resolved worktree: refuse before \
         anything moves (untracked files are fine, nothing is ever staged)\n\n\
         CI WAIT: same as `bump release --help` -- no tag until the merged sha's CI is green \
         (default {ci_timeout}s, see --ci-timeout above); --no-ci-gate bypasses the wait \
         entirely.\n\n\
         PERSONA TOKENS: same per-org resolution as `bump release --help` (a token file, then \
         GITHUB_PAT_<ORG>, then GITHUB_PAT_WORK/GITHUB_PAT_HOME, else ambient `gh auth`).\n\n\
         REQUIRED TOOLS:\n  {} {:<10} {}\n  {} {:<10} {}\n\n\
         gh is used only for the CI gate's check-runs/status reads; finish itself moves no \
         PRs.\n\n\
         Logs are written to: {}",
        git_status.status_icon,
        "git",
        git_status.version,
        gh_status.status_icon,
        "gh",
        gh_status.version,
        log_path_for_help(),
        ci_timeout = default_ci_timeout_secs(),
    )
}

/// The XDG log path, rendered from the SAME resolution `main.rs::xdg_data_dir` uses, so
/// `--help` can never drift from where the logger actually writes (rules/rust.md: render
/// at runtime, don't hardcode).
fn log_path_for_help() -> String {
    crate::log_file_path().display().to_string()
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

    // =========================================================================
    // SUBCOMMAND WIRING: bump release / bump finish (Phase 8). Bare `bump` (no
    // subcommand) must stay `command: None` on every legacy invocation -- these tests
    // pin that ALONGSIDE the new subcommand parsing, so a future clap change that starts
    // stealing a bare invocation or a positional directory is caught here.
    // =========================================================================

    #[test]
    fn test_cli_no_subcommand_is_none_on_bare_bump() {
        let cli = Cli::try_parse_from(["bump"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_no_subcommand_is_none_with_legacy_flags() {
        // The exact legacy matrix this phase's hard constraint pins byte-identical.
        for args in [
            vec!["bump"],
            vec!["bump", "-m"],
            vec!["bump", "-M"],
            vec!["bump", "-n"],
            vec!["bump", "--no-tag"],
            vec!["bump", "--tag-only"],
            vec!["bump", "--gates"],
            vec!["bump", "--skip-member", "name"],
            vec!["bump", "./some-dir"],
            vec!["bump", "-a"],
            vec!["bump", "--message", "msg"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap();
            assert!(cli.command.is_none(), "expected no subcommand for {:?}", args);
        }
    }

    #[test]
    fn test_cli_release_subcommand_parses() {
        let cli = Cli::try_parse_from(["bump", "release"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => {
                assert!(!args.major);
                assert!(!args.minor);
                assert!(!args.dry_run);
                assert!(args.install.is_none());
                assert!(!args.no_install);
            }
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_subcommand_flags() {
        let cli = Cli::try_parse_from(["bump", "release", "-m", "-n"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => {
                assert!(args.minor);
                assert!(args.dry_run);
            }
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_major_minor_conflict() {
        assert!(Cli::try_parse_from(["bump", "release", "-m", "-M"]).is_err());
    }

    #[test]
    fn test_cli_release_install_flag() {
        let cli = Cli::try_parse_from(["bump", "release", "--install", "cargo install --path ."]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => {
                assert_eq!(args.install, Some("cargo install --path .".to_string()));
                assert!(!args.no_install);
            }
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_no_install_flag() {
        let cli = Cli::try_parse_from(["bump", "release", "--no-install"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => {
                assert!(args.no_install);
                assert!(args.install.is_none());
            }
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_standalone_flag() {
        let cli = Cli::try_parse_from(["bump", "release", "--standalone", "his exact words"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => {
                assert_eq!(args.standalone, Some("his exact words".to_string()));
            }
            other => panic!("expected Release, got {:?}", other),
        }
        let cli = Cli::try_parse_from(["bump", "release"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => assert!(args.standalone.is_none()),
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_no_ci_gate_flag() {
        let cli = Cli::try_parse_from(["bump", "release", "--no-ci-gate"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => assert!(args.no_ci_gate),
            other => panic!("expected Release, got {:?}", other),
        }
        let cli = Cli::try_parse_from(["bump", "release"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => assert!(!args.no_ci_gate),
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_release_ci_timeout_flag_and_default() {
        let cli = Cli::try_parse_from(["bump", "release", "--ci-timeout", "60"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => assert_eq!(args.ci_timeout, 60),
            other => panic!("expected Release, got {:?}", other),
        }
        let cli = Cli::try_parse_from(["bump", "release"]).unwrap();
        match cli.command {
            Some(Commands::Release(args)) => assert_eq!(args.ci_timeout, default_ci_timeout_secs()),
            other => panic!("expected Release, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_finish_no_ci_gate_and_ci_timeout_flags() {
        let cli = Cli::try_parse_from(["bump", "finish", "--no-ci-gate", "--ci-timeout", "30"]).unwrap();
        match cli.command {
            Some(Commands::Finish(args)) => {
                assert!(args.no_ci_gate);
                assert_eq!(args.ci_timeout, 30);
            }
            other => panic!("expected Finish, got {:?}", other),
        }
        let cli = Cli::try_parse_from(["bump", "finish"]).unwrap();
        match cli.command {
            Some(Commands::Finish(args)) => {
                assert!(!args.no_ci_gate);
                assert_eq!(args.ci_timeout, default_ci_timeout_secs());
            }
            other => panic!("expected Finish, got {:?}", other),
        }
    }

    /// `bump release --help | grep -cE 'standalone|no-ci-gate|ci-timeout'` (the design
    /// doc's acceptance criterion) must print exactly 3 -- one line per flag. The
    /// after-help prose (this is the after-help TEXT ITSELF, `RELEASE_HELP_TEXT`'s source)
    /// must not repeat those literal substrings; the flag list is clap's own Options
    /// block, exercised end-to-end by `tests/release_cli.rs`.
    #[test]
    fn test_release_after_help_names_all_three_flags_literally() {
        // Mirrors the design doc's amended acceptance criterion (presence, not line
        // count): an agent reading --help should see `--standalone "<words>"`,
        // `--no-ci-gate`, and `--ci-timeout` spelled out, not paraphrased.
        let help = get_release_help();
        for flag in ["--standalone", "--no-ci-gate", "--ci-timeout"] {
            assert!(help.contains(flag), "after-help must name {flag} literally:\n{help}");
        }
    }

    #[test]
    fn test_cli_release_install_and_no_install_conflict() {
        assert!(Cli::try_parse_from(["bump", "release", "--install", "x", "--no-install"]).is_err());
    }

    #[test]
    fn test_cli_finish_subcommand_parses() {
        let cli = Cli::try_parse_from(["bump", "finish"]).unwrap();
        match cli.command {
            Some(Commands::Finish(args)) => {
                assert!(!args.dry_run);
                assert!(args.install.is_none());
                assert!(!args.no_install);
            }
            other => panic!("expected Finish, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_finish_dry_run_flag() {
        let cli = Cli::try_parse_from(["bump", "finish", "-n"]).unwrap();
        match cli.command {
            Some(Commands::Finish(args)) => assert!(args.dry_run),
            other => panic!("expected Finish, got {:?}", other),
        }
    }

    #[test]
    fn test_cli_finish_install_and_no_install_conflict() {
        assert!(Cli::try_parse_from(["bump", "finish", "--install", "x", "--no-install"]).is_err());
    }

    #[test]
    fn test_cli_finish_has_no_bump_level_flags() {
        // `finish` never computes a bump; -m/-M/--major/--minor are not its flags.
        assert!(Cli::try_parse_from(["bump", "finish", "-m"]).is_err());
        assert!(Cli::try_parse_from(["bump", "finish", "-M"]).is_err());
    }

    /// The one documented ambiguity of the optional-subcommand pattern: a directory
    /// literally named `release`/`finish` with NO path prefix is parsed as the
    /// subcommand, not the directory positional. A `./`-prefixed (or any other) path is
    /// unaffected -- this is the realistic invocation and it is unambiguous.
    #[test]
    fn test_cli_dir_named_release_needs_path_prefix_to_disambiguate() {
        let cli = Cli::try_parse_from(["bump", "./release"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.directories, vec![PathBuf::from("./release")]);
    }
}
