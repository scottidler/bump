# bump - Design Document

A Rust CLI tool for bumping semantic versions in Cargo.toml, creating commits, and tagging releases.

## Overview

`bump` automates the version release workflow for Rust projects:
1. Increment the version in `Cargo.toml`
2. Stage and commit changes
3. Create an annotated git tag

## Usage

```
bump [OPTIONS] [DIRECTORIES...]

Arguments:
  [DIRECTORIES...]  Paths to git repository roots containing Cargo.toml
                    (default: current working directory)

Options:
  -M, --major     Bump major version (X.0.0)
  -m, --minor     Bump minor version (x.Y.0)
                  (default: bump patch x.y.Z)
  -n, --dry-run   Preview changes without applying
  -h, --help      Print help
  -V, --version   Print version

REQUIRED TOOLS:
  ✅ git       2.43.0

Logs are written to: ~/.local/share/bump/logs/bump.log
```

## Help Output Style

Following the `gx` pattern, the `--help` output includes:
1. Description at top
2. Usage line
3. Arguments section
4. Options section
5. `REQUIRED TOOLS:` section showing git version with status icon
6. Log file location at bottom

This is implemented using clap's `after_help` attribute with a `LazyLock<String>` that dynamically checks tool versions at runtime.

## Semantic Versioning

Follows [semver](https://semver.org/) format: `MAJOR.MINOR.PATCH`

### Bump Behavior

| Current | Flag | Result |
|---------|------|--------|
| 1.2.3 | (none) | 1.2.4 |
| 1.2.9 | (none) | 1.2.10 |
| 1.2.99 | (none) | 1.2.100 |
| 1.2.3 | `--minor` | 1.3.0 |
| 1.2.3 | `--major` | 2.0.0 |

- Patch bump: increment patch, preserve major/minor
- Minor bump: increment minor, reset patch to 0
- Major bump: increment major, reset minor and patch to 0

### Version Formats

- **Cargo.toml**: `0.4.2` (no prefix)
- **Git tags**: `v0.4.2` (with `v` prefix)

## Architecture

```
src/
├── main.rs      # Entry point, orchestration
├── cli.rs       # Clap CLI definition
├── version.rs   # SemVer parsing and bumping logic
├── cargo.rs     # Cargo.toml reading/writing
└── git.rs       # Git operations (shell commands)

build.rs         # Git describe for version string
```

### Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing (derive) |
| `toml_edit` | Edit Cargo.toml preserving formatting |
| `semver` | Parse and manipulate semantic versions |
| `eyre` | Error handling with context |
| `dirs` | Log directory path (~/.local/share) |
| `env_logger` | Logging to file |
| `log` | Logging macros |

### Git Operations

Shell out to `git` commands rather than using `git2` library:
- Simpler implementation
- No native library dependency
- User can see exactly what commands are run
- Matches user's mental model

### Version String (build.rs)

Following the `gx` pattern, `build.rs` runs `git describe --tags --always` to generate
a version string that includes:
- The latest tag
- Commit count since tag
- Short commit hash (if not exactly on a tag)

This is exposed via `env!("GIT_DESCRIBE")` in the CLI's `#[command(version = ...)]`.

## Workflow

### Per-Directory Flow

```
┌─────────────────────────────────────────────────────────────┐
│ 1. VALIDATE                                                 │
├─────────────────────────────────────────────────────────────┤
│ • Verify directory is a git repository (error if not)       │
│ • Verify Cargo.toml exists (error if not)                   │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. DETERMINE CURRENT VERSION                                │
├─────────────────────────────────────────────────────────────┤
│ • Read version from Cargo.toml                              │
│ • If version field missing → use latest semver git tag      │
│ • If Cargo.toml version's tag exists → use latest git tag   │
│   (indicates Cargo.toml is stale/incorrect)                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. CALCULATE NEW VERSION                                    │
├─────────────────────────────────────────────────────────────┤
│ • Apply bump type (major/minor/patch)                       │
│ • Error if pre-release or build metadata present            │
│ • Verify new tag doesn't already exist                      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. UPDATE CARGO.TOML                                        │
├─────────────────────────────────────────────────────────────┤
│ • Update [package] version field                            │
│ • Or update [workspace.package] version if workspace        │
│ • Add version field if missing                              │
│ • Preserve file formatting                                  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │   --dry-run?    │
                    └────────┬────────┘
                      yes    │    no
                 ┌───────────┴───────────┐
                 ▼                       ▼
        ┌────────────────┐    ┌─────────────────────────────────┐
        │ Print preview  │    │ 5. STAGE CHANGES                │
        │ and exit       │    ├─────────────────────────────────┤
        └────────────────┘    │ • git add -A                    │
                              └─────────────────────────────────┘
                                          │
                                          ▼
                              ┌─────────────────────────────────┐
                              │ 6. DETERMINE COMMIT MESSAGE     │
                              ├─────────────────────────────────┤
                              │ • Check if only Cargo.toml      │
                              │   changed (git diff --cached)   │
                              │                                 │
                              │ Only Cargo.toml:                │
                              │   → "Bump version to vX.Y.Z"    │
                              │                                 │
                              │ Other changes present:          │
                              │   → Prompt user for message     │
                              └─────────────────────────────────┘
                                          │
                                          ▼
                              ┌─────────────────────────────────┐
                              │ 7. COMMIT                       │
                              ├─────────────────────────────────┤
                              │ • git commit -m "<message>"     │
                              └─────────────────────────────────┘
                                          │
                                          ▼
                              ┌─────────────────────────────────┐
                              │ 8. TAG                          │
                              ├─────────────────────────────────┤
                              │ • git tag -a vX.Y.Z             │
                              │       -m "<same message>"       │
                              └─────────────────────────────────┘
                                          │
                                          ▼
                              ┌─────────────────────────────────┐
                              │ 9. REPORT SUCCESS               │
                              ├─────────────────────────────────┤
                              │ • Print new version             │
                              │ • Remind user to push           │
                              └─────────────────────────────────┘
```

### Multiple Directories

When multiple directory paths are provided:
- Each directory is treated as an independent git repository root
- Each is processed in sequence
- Errors on one do not prevent processing others
- Summary of successes/failures printed at end

**Commit Message Behavior:**
- If all directories have only Cargo.toml changes → use default messages
- If any directory has other changes → prompt for that directory's message

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Not a git repository | Error and skip |
| No Cargo.toml | Error and skip |
| No version field | Add version field |
| Pre-release version (e.g., `1.0.0-alpha`) | Error and skip |
| Build metadata (e.g., `1.0.0+build`) | Error and skip |
| Calculated tag already exists | Error and skip |
| Git command fails | Error with command output |

## Output

Default output is concise but informative:

```
$ bump
bump: 0.4.2 → 0.4.3
Committed and tagged v0.4.3
Run: git push && git push --tags

$ bump --major ./proj1 ./proj2
[proj1] bump: 1.2.3 → 2.0.0
Committed and tagged v2.0.0

[proj2] bump: 0.9.1 → 1.0.0
Committed and tagged v1.0.0

All done! Don't forget to push your changes.
```

With other changes present:
```
$ bump
bump: 0.4.2 → 0.4.3

Staged changes:
  M src/main.rs
  A src/new_feature.rs

Enter commit message: Add new feature

Committed and tagged v0.4.3
Run: git push && git push --tags
```

Dry run:
```
$ bump --dry-run
[dry-run] Would bump: 0.4.2 → 0.4.3
[dry-run] Would update: Cargo.toml
[dry-run] Would commit and tag: v0.4.3
```

## Implementation Details

### CLI Structure (cli.rs)

```rust
use clap::Parser;
use std::path::PathBuf;
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

    /// Paths to git repository roots
    #[arg(value_name = "DIRECTORIES")]
    pub directories: Vec<PathBuf>,
}

/// Generate tool validation help text (called once via LazyLock)
fn get_tool_validation_help() -> String {
    let git_status = check_tool_version("git", "--version", "2.20.0");
    format!(
        "REQUIRED TOOLS:\n  {} git       {}\n\nLogs are written to: ~/.local/share/bump/logs/bump.log",
        git_status.icon,
        git_status.version
    )
}
```

### Bump Type Enum

```rust
#[derive(Debug, Clone, Copy, Default)]
pub enum BumpType {
    Major,
    Minor,
    #[default]
    Patch,
}

impl BumpType {
    pub fn from_cli(major: bool, minor: bool) -> Self {
        match (major, minor) {
            (true, _) => BumpType::Major,
            (_, true) => BumpType::Minor,
            _ => BumpType::Patch,
        }
    }
}
```

### Git Commands Used

| Operation | Command |
|-----------|---------|
| Check if git repo | `git rev-parse --git-dir` |
| Get latest semver tag | `git tag -l 'v*' --sort=-v:refname` |
| Check if tag exists | `git tag -l <tag>` |
| Stage all changes | `git add -A` |
| Get staged files | `git diff --cached --name-only` |
| Commit | `git commit -m "<message>"` |
| Create annotated tag | `git tag -a <tag> -m "<message>"` |
| Check for uncommitted changes | `git status --porcelain` |

### User Input for Commit Message

When prompting for a commit message (other changes exist beyond Cargo.toml):

```rust
use std::io::{self, Write};

fn prompt_commit_message() -> Result<String> {
    print!("Enter commit message: ");
    io::stdout().flush()?;

    let mut message = String::new();
    io::stdin().read_line(&mut message)?;

    let message = message.trim().to_string();
    if message.is_empty() {
        eyre::bail!("Commit message cannot be empty");
    }
    Ok(message)
}
```

## Future Considerations

Not in scope for initial implementation, but potential future additions:

- `--message` / `-m` flag to provide commit message non-interactively
- Support for other version files (package.json, pyproject.toml)
- `--push` flag to automatically push after tagging
- Config file for per-project defaults
- Pre/post bump hooks

