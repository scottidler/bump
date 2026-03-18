# bump - Design Document

A Rust CLI tool for bumping semantic versions, creating commits, and tagging releases. Supports Rust (Cargo.toml), Python (pyproject.toml), and generic git repos (tag-only).

## Overview

`bump` automates the version release workflow for any project:
1. Detect project type (Rust, Python, or Generic)
2. Increment the version in the appropriate file (if applicable)
3. Stage and commit changes
4. Create an annotated git tag

## Usage

```
bump [OPTIONS] [DIRECTORIES...]

Arguments:
  [DIRECTORIES...]  Paths to git repository roots
                    (default: current working directory)

Options:
  -M, --major       Bump major version (X.0.0)
  -m, --minor       Bump minor version (x.Y.0)
                    (default: bump patch x.y.Z)
  -n, --dry-run     Preview changes without applying
      --message     Commit message to use
  -a, --automatic   Generate automatic commit message
  -f, --force       Force bump even if HEAD already has a tag
  -h, --help        Print help
  -V, --version     Print version

REQUIRED TOOLS:
  git       2.20.0+

Logs are written to: ~/.local/share/bump/logs/bump.log
```

## Project Type Detection

bump automatically detects the project type based on files present:

| Priority | File | Project Type |
|----------|------|-------------|
| 1 | `Cargo.toml` | Rust |
| 2 | `pyproject.toml` | Python |
| 3 | (neither) | Generic |

If both `Cargo.toml` and `pyproject.toml` exist, Rust takes precedence.

### Rust Projects

- Version read from / written to `Cargo.toml`
- Supports workspace version (`[workspace.package].version`)
- Supports workspace inheritance (`version.workspace = true` and `version = { workspace = true }`)
- Syncs `Cargo.lock` via `cargo update` after version change
- `0.1.0` is treated as the "untouched default" (Cargo's initial value) and defers to git tags

### Python Projects

- Version read from / written to `pyproject.toml`
- Supports PEP 621 (`[project].version`) - preferred
- Supports Poetry (`[tool.poetry].version`) - fallback
- Handles dynamic versions (`dynamic = ["version"]`) by treating as no version
- No lockfile sync needed (Python lockfiles don't track the project's own version)
- No "untouched default" concept - all versions are treated as actively managed

### Generic Projects (git-tag-only)

- No version file to read or write
- Version is determined entirely from git tags
- If no tags exist, starts at `v0.1.0`
- Useful for non-code repos, documentation repos, or projects without a standard version file

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

- **Version files**: `0.4.2` (no prefix)
- **Git tags**: `v0.4.2` (with `v` prefix)

## Architecture

```
src/
+-- main.rs      # Entry point, project type detection, orchestration
+-- cli.rs       # Clap CLI definition
+-- version.rs   # SemVer parsing and bumping logic
+-- cargo.rs     # Cargo.toml reading/writing (Rust)
+-- python.rs    # pyproject.toml reading/writing (Python)
+-- git.rs       # Git operations (shell commands)

build.rs         # Git describe for version string
```

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `main.rs` | Project type detection, version action logic, workflow orchestration |
| `cli.rs` | CLI argument parsing with clap derive |
| `version.rs` | Pure version logic - parsing, bumping, formatting (no I/O) |
| `cargo.rs` | Cargo.toml read/write, workspace detection, lockfile sync |
| `python.rs` | pyproject.toml read/write (PEP 621 + Poetry) |
| `git.rs` | All git operations via shell commands |

### Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing (derive) |
| `toml_edit` | Edit TOML files preserving formatting |
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

`build.rs` runs `git describe --tags --always` to generate a version string that includes:
- The latest tag
- Commit count since tag
- Short commit hash (if not exactly on a tag)

This is exposed via `env!("GIT_DESCRIBE")` in the CLI's `#[command(version = ...)]`.

## Version Action Rules

### Rule 1: Untouched Default (Rust only)

`0.1.0` is the special "untouched default" version that Cargo generates. This does NOT apply to Python or Generic projects.

- If Cargo.toml = 0.1.0 and git tags exist - DEFER TO GIT TAG
- If Cargo.toml = 0.1.0 and no git tags - Create initial tag v0.1.0

### Rule 2: Actively Managed Version

Any version file with a non-default version:

- If version matches latest tag - Bump from that version
- If version does not match latest tag - **ERROR** (version mismatch)
- If no tags exist - Create initial tag at current version

### Rule 3: No Version in File (or Generic project)

- If git tags exist - Bump from latest tag
- If no git tags - Start at 0.1.0

## Workflow

### Per-Directory Flow

```
+-------------------------------------------------------------+
| 1. VALIDATE                                                   |
+---------------------------------------------------------------+
| - Verify directory is a git repository (error if not)         |
| - Detect project type (Rust / Python / Generic)               |
| - Run project-specific validation (workspace checks for Rust) |
+---------------------------------------------------------------+
                              |
                              v
+---------------------------------------------------------------+
| 2. DETERMINE VERSION ACTION                                   |
+---------------------------------------------------------------+
| - Read version from project file (if applicable)              |
| - Get latest git tag (if any)                                 |
| - Apply version action rules (see above)                      |
| - Calculate target version based on bump type                 |
+---------------------------------------------------------------+
                              |
                              v
+---------------------------------------------------------------+
| 3. CHECK FOR UNCOMMITTED CHANGES                              |
+---------------------------------------------------------------+
| - git status --porcelain                                      |
| - Branch based on result:                                     |
|   - Has changes -> Standard workflow                          |
|   - Clean tree -> Clean tree workflow                         |
+---------------------------------------------------------------+
                              |
              +---------------+---------------+
              v                               v
+---------------------------+   +-------------------------------+
| STANDARD WORKFLOW         |   | CLEAN TREE WORKFLOW           |
| (uncommitted changes)     |   | (already committed)           |
+---------------------------+   +-------------------------------+
| 4. Update version file    |   | 4. Check if HEAD has tag      |
| 5. Sync lockfile          |   |    -> Error if already tagged  |
| 6. git add -A             |   | 5. Check if HEAD is pushed    |
| 7. Determine message:     |   | 6. Update version file        |
|    --message > -a >       |   | 7. Sync lockfile              |
|    auto > editor          |   | 8. git add -A                 |
| 8. git commit             |   |                               |
| 9. git tag                |   | If pushed:                    |
|                           |   |   9. git commit (new)         |
|                           |   |   10. git tag                 |
|                           |   |                               |
|                           |   | If not pushed:                |
|                           |   |   9. git commit --amend       |
|                           |   |   10. git tag                 |
+---------------------------+   +-------------------------------+
              |                               |
              +---------------+---------------+
                              v
                    +-------------------+
                    | REPORT SUCCESS    |
                    +-------------------+
                    | Print version     |
                    | Remind to push    |
                    +-------------------+
```

### Multiple Directories

When multiple directory paths are provided:
- Each directory is treated as an independent git repository root
- Each is processed in sequence
- Errors on one do not prevent processing others
- Summary of successes/failures printed at end

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Not a git repository | Error and skip |
| No version file (Rust/Python) | Use git tags only |
| No version field in file | Add version field |
| Version mismatch (file vs tag) | Error and skip |
| Pre-release version (e.g., `1.0.0-alpha`) | Error and skip |
| Build metadata (e.g., `1.0.0+build`) | Error and skip |
| Calculated tag already exists | Error and skip |
| HEAD already has a tag (clean tree) | Error unless --force |
| Workspace members with independent versions | Error and skip |
| Git command fails | Error with command output |

## Output

### Rust project (standard workflow)
```
$ bump
bump: 0.4.2 -> 0.4.3
Committed and tagged v0.4.3
Run: git push && git push --tags
```

### Python project
```
$ bump
bump: 1.0.0 -> 1.0.1
Committed and tagged v1.0.1
Run: git push && git push --tags
```

### Generic project (tag-only)
```
$ bump
bump: 0.5.0 -> 0.5.1
Committed and tagged v0.5.1
Run: git push && git push --tags
```

### Initial tag
```
$ bump
tag: v0.1.0
Committed and tagged v0.1.0
Run: git push && git push --tags
```

### Clean tree workflow (unpushed)
```
$ bump -a
bump: 0.4.2 -> 0.4.3
Amended commit and tagged v0.4.3
Run: git push && git push --tags
```

### Dry run
```
$ bump --dry-run
bump: 0.4.2 -> 0.4.3
[dry-run] Would update: Cargo.toml
[dry-run] Would commit and tag: v0.4.3
```

### Multiple directories
```
$ bump --major ./proj1 ./proj2
[proj1] bump: 1.2.3 -> 2.0.0
Committed and tagged v2.0.0

[proj2] bump: 0.9.1 -> 1.0.0
Committed and tagged v1.0.0

All done! Don't forget to push your changes.
```

## Commit Message Logic

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

Version-only files per project type:
- **Rust**: `Cargo.toml`, `Cargo.lock`
- **Python**: `pyproject.toml`
- **Generic**: (no version files - empty staged list triggers auto-message)

## Git Commands Used

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

## Future Considerations

Potential future additions:

- Support for other version files (package.json, setup.cfg)
- `--push` flag to automatically push after tagging
- Config file for per-project defaults
- Pre/post bump hooks
