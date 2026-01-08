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
  -M, --major       Bump major version (X.0.0)
  -m, --minor       Bump minor version (x.Y.0)
                    (default: bump patch x.y.Z)
  -n, --dry-run     Preview changes without applying
      --message     Commit message to use
  -a, --automatic   Generate automatic commit message
  -h, --help        Print help
  -V, --version     Print version

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
│ 2. DETERMINE VERSION ACTION                                 │
├─────────────────────────────────────────────────────────────┤
│ • Read version from Cargo.toml                              │
│ • Get latest git tag (if any)                               │
│ • 0.1.0 in Cargo.toml is treated as "untouched default"     │
│   and defers to git tags if they exist                      │
│ • Calculate target version based on bump type               │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. CHECK FOR UNCOMMITTED CHANGES                            │
├─────────────────────────────────────────────────────────────┤
│ • git status --porcelain                                    │
│ • Branch based on result:                                   │
│   - Has changes → Standard workflow                         │
│   - Clean tree → Clean tree workflow                        │
└─────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
┌─────────────────────────┐     ┌─────────────────────────────┐
│ STANDARD WORKFLOW       │     │ CLEAN TREE WORKFLOW         │
│ (uncommitted changes)   │     │ (already committed)         │
├─────────────────────────┤     ├─────────────────────────────┤
│ 4. Update Cargo.toml    │     │ 4. Check if HEAD has tag    │
│ 5. Sync Cargo.lock      │     │    → Error if already tagged│
│ 6. git add -A           │     │ 5. Check if HEAD is pushed  │
│ 7. Determine message:   │     │ 6. Update Cargo.toml        │
│    --message > -a >     │     │ 7. Sync Cargo.lock          │
│    auto > editor        │     │ 8. git add -A               │
│ 8. git commit           │     │                             │
│ 9. git tag              │     │ If pushed:                  │
│                         │     │   9. git commit (new)       │
│                         │     │   10. git tag               │
│                         │     │                             │
│                         │     │ If not pushed:              │
│                         │     │   9. git commit --amend     │
│                         │     │   10. git tag               │
└─────────────────────────┘     └─────────────────────────────┘
              │                               │
              └───────────────┬───────────────┘
                              ▼
                    ┌─────────────────┐
                    │ REPORT SUCCESS  │
                    ├─────────────────┤
                    │ Print version   │
                    │ Remind to push  │
                    └─────────────────┘
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

### Standard workflow (uncommitted changes)
```
$ bump
bump: 0.4.2 → 0.4.3
Committed and tagged v0.4.3
Run: git push && git push --tags
```

### Clean tree workflow (already committed, unpushed)
```
$ bump -a
bump: 0.4.2 → 0.4.3
Amended commit and tagged v0.4.3
Run: git push && git push --tags
```

### Clean tree workflow (already committed and pushed)
```
$ bump -a
bump: 0.4.2 → 0.4.3
Committed and tagged v0.4.3
Run: git push && git push --tags
```

### Multiple directories
```
$ bump --major ./proj1 ./proj2
[proj1] bump: 1.2.3 → 2.0.0
Committed and tagged v2.0.0

[proj2] bump: 0.9.1 → 1.0.0
Committed and tagged v1.0.0

All done! Don't forget to push your changes.
```

### Dry run
```
$ bump --dry-run
bump: 0.4.2 → 0.4.3
[dry-run] Would update: Cargo.toml
[dry-run] Would commit and tag: v0.4.3

# Or for clean tree (unpushed):
[dry-run] Would amend previous commit and tag: v0.4.3
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
| Amend commit | `git commit --amend --no-edit` |
| Create annotated tag | `git tag -a <tag> -m "<message>"` |
| Check for uncommitted changes | `git status --porcelain` |
| Check if HEAD has tag | `git describe --exact-match HEAD` |
| Check if HEAD is pushed | `git merge-base --is-ancestor HEAD @{u}` |

### User Input for Commit Message

Commit message is determined by priority:

1. `--message <MSG>` - Use provided message directly
2. `-a` / `--automatic` - Generate "Bump version to vX.Y.Z"
3. Version-only changes - Auto-generate appropriate message
4. Other changes - Open editor for user input

When opening editor:
- Checks `$VISUAL`, then `$EDITOR`, then falls back to `vim`
- Creates temp file with template showing staged files
- Lines starting with `#` are stripped
- Empty message aborts the operation

## Future Considerations

Potential future additions:

- Support for other version files (package.json, pyproject.toml)
- `--push` flag to automatically push after tagging
- Config file for per-project defaults
- Pre/post bump hooks

